#![forbid(unsafe_code)]

//! Command-line entry point for the `KiCad` schematic linter.
//!
//! Loads schematic, project, and component-intent profile inputs; runs native
//! Electrical Rules Checker and built-in schematic rules; and renders findings
//! as human-readable text or structured JSON.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use crate::core::{Diagnostic, RulePolicy, Severity, SourceFile, builtin_rule_registry};
use crate::erc::{Netlist, NetlistError, export_netlist, run_native_erc};
use crate::parser::{BoardDocument, SchematicDocument};
use crate::rules::{
    LintProfile, MILLIMETRES_PER_MIL, ProfileMode, RuleEngine, SchematicFacts,
    physical_wire_component_ids, physical_wire_component_key,
};
use clap::{Parser, Subcommand, ValueEnum};
use miette::{LabeledSpan, MietteDiagnostic, NamedSource, Report, Severity as RenderSeverity};
use regex::Regex;
use serde::Serialize;
use thiserror::Error;

/// KiCad's default netclass wire width, in mils.
const DEFAULT_NETCLASS_WIRE_WIDTH_MILS: f64 = 6.0;
/// Maximum number of member patterns expanded from one project bus assignment.
const MAX_NETCLASS_BUS_MEMBERS: usize = 4096;
/// Maximum recursive nesting depth accepted when expanding project bus groups.
const MAX_NETCLASS_BUS_EXPANSION_DEPTH: usize = 16;
/// Minimum member count required for a string to be treated as a bus group.
const MIN_NETCLASS_BUS_GROUP_MEMBERS: usize = 2;

/// Clippy-inspired checks for `KiCad` schematic source files.
#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Subcommand selected by the user, including the schematic and optional project/profile paths.
    #[command(subcommand)]
    command: Commands,
}
/// Commands accepted by the executable.
#[derive(Debug, Subcommand)]
enum Commands {
    /// Parse a schematic and run `KiCad`'s native Electrical Rules Checker.
    Lint {
        /// Schematic source file to inspect.
        schematic: PathBuf,
        /// Output format.
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
        /// Skip ERC (profile and project checks may still export a netlist).
        #[arg(long)]
        skip_erc: bool,
        /// Explicit path or command name for kicad-cli.
        #[arg(long, value_name = "PATH")]
        kicad_cli: Option<PathBuf>,
        /// `KiCad` project file with per-wire SCH013 netclass settings (requires the `KiCad` CLI).
        #[arg(long, value_name = "FILE")]
        project: Option<PathBuf>,
        /// JSON project profile containing component-intent metadata.
        #[arg(long, value_name = "FILE")]
        profile: Option<PathBuf>,
        /// Optional TOML policy for built-in rule levels and named presets.
        #[arg(long, value_name = "FILE")]
        policy: Option<PathBuf>,
        /// Optional `KiCad` PCB file for board-level checks such as PCB001 (requires --profile).
        #[arg(long, value_name = "FILE")]
        board: Option<PathBuf>,
        /// Require complete component-intent metadata (requires --profile).
        #[arg(long)]
        strict: bool,
    },
}
/// Output encodings supported by the CLI renderer.
#[derive(Clone, Copy, Debug, ValueEnum)]
enum OutputFormat {
    /// Human-readable diagnostics.
    Text,
    /// Structured JSON diagnostics.
    Json,
}
/// Inputs required to run one schematic lint invocation.
struct LintOptions<'a> {
    /// Requested diagnostic output encoding.
    format: OutputFormat,
    /// Whether native `KiCad` ERC should be skipped.
    skip_erc: bool,
    /// Optional explicit path to the `KiCad` CLI executable.
    explicit_cli: Option<PathBuf>,
    /// Optional `KiCad` project file providing schematic and netclass settings.
    project_path: Option<&'a Path>,
    /// Optional JSON component-intent profile, consumed during profile validation.
    profile_path: Option<PathBuf>,
    /// Optional TOML rule policy, borrowed because policy loading does not consume the path.
    policy_path: Option<&'a Path>,
    /// Optional PCB document used for board-level design-intent checks.
    board_path: Option<PathBuf>,
    /// Whether complete component-intent metadata is required.
    strict: bool,
}

/// Errors that can prevent a lint command from completing.
#[derive(Debug, Error)]
enum CliError {
    /// Schematic file reading or parsing failed.
    #[error(transparent)]
    Schematic(#[from] crate::parser::SchematicParseError),
    /// Board file reading or parsing failed.
    #[error(transparent)]
    Board(#[from] crate::parser::BoardParseError),
    /// Board-level checks need a component-intent profile.
    #[error("--board requires --profile FILE")]
    BoardNeedsProfile,
    /// Native `KiCad` ERC invocation or report decoding failed.
    #[error(transparent)]
    Erc(#[from] crate::erc::ErcError),
    /// No `KiCad` CLI executable could be resolved.
    #[error("KiCad CLI was not found; install KiCad or pass --kicad-cli PATH")]
    KiCadCliNotFound,
    /// The component-intent profile could not be loaded or validated.
    #[error(transparent)]
    Metadata(#[from] crate::rules::MetadataError),
    /// The TOML rule policy could not be decoded.
    #[error("invalid TOML rule policy: {0}")]
    RulePolicyToml(#[from] toml::de::Error),
    /// The policy references an unknown built-in rule.
    #[error(transparent)]
    RulePolicy(#[from] crate::core::RulePolicyError),
    /// The built-in rule descriptor catalogue could not be constructed.
    #[error(transparent)]
    RuleRegistry(#[from] crate::core::RuleRegistryError),
    /// Native netlist export or parsing failed.
    #[error(transparent)]
    Netlist(#[from] NetlistError),
    /// Strict mode was requested without a profile file.
    #[error("--strict requires --profile FILE")]
    StrictNeedsProfile,
    /// JSON output serialization failed.
    #[error("failed to serialize JSON output: {0}")]
    Json(#[from] serde_json::Error),
    /// Writing CLI output failed.
    #[error("failed to write CLI output: {0}")]
    Io(#[from] io::Error),
}
/// JSON-serializable result returned by a lint invocation.
#[derive(Debug, Serialize)]
struct LintOutput {
    /// Version of the JSON output schema.
    schema_version: &'static str,
    /// Version of the `kicad-clippy` executable that produced this report.
    tool_version: &'static str,
    /// Display path of the schematic that was analyzed.
    schematic: String,
    /// Whether the input passed S-expression and schematic-root validation.
    syntax_valid: bool,
    /// Whether native ERC was intentionally skipped for this invocation.
    erc_skipped: bool,
    /// `KiCad` CLI version reported by native ERC, or `None` when ERC was skipped.
    kicad_version: Option<String>,
    /// Normalized ERC and Rust-rule findings for the schematic.
    diagnostics: Vec<Diagnostic>,
    /// Counts of errors, warnings, notes, and excluded findings.
    summary: Summary,
}
/// Counts of lint findings grouped by severity and exclusion state.
#[derive(Debug, Serialize)]
struct Summary {
    /// Number of non-excluded error diagnostics that make the command fail.
    errors: usize,
    /// Number of non-excluded warning diagnostics.
    warnings: usize,
    /// Number of non-excluded informational diagnostics.
    notes: usize,
    /// Number of diagnostics marked excluded by `KiCad` or project configuration.
    excluded: usize,
}

impl Summary {
    /// Counts active errors, non-excluded warnings, notes, and excluded findings for the CLI summary.
    ///
    /// # Arguments
    ///
    /// * `diagnostics` — All normalized findings produced by ERC and the schematic rule engine.
    ///
    /// # Returns
    ///
    /// A summary whose counts distinguish active errors, visible warnings/notes, and excluded findings.
    fn from_diagnostics(diagnostics: &[Diagnostic]) -> Self {
        Self {
            errors: diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.is_active_error())
                .count(),
            warnings: diagnostics
                .iter()
                .filter(|diagnostic| {
                    diagnostic.severity == Severity::Warning && !diagnostic.excluded
                })
                .count(),
            notes: diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.severity == Severity::Note && !diagnostic.excluded)
                .count(),
            excluded: diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.excluded)
                .count(),
        }
    }
}
/// Parses command-line arguments, executes the selected operation, and converts its result into the process exit code.
///
/// # Returns
///
/// The process exit code returned by the lint command.
/// Runs the command-line application and returns its process exit status.
///
/// # Returns
///
/// `ExitCode::SUCCESS` when linting completes without active errors, or a failure code otherwise.
pub fn run() -> ExitCode {
    match execute(Cli::parse()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("kicad-clippy: {error}");
            ExitCode::FAILURE
        }
    }
}
/// Dispatches the parsed command to the corresponding CLI operation.
///
/// # Arguments
///
/// * `cli` — Validated CLI options describing the schematic, selected output format, and optional project/profile inputs.
///
/// # Returns
///
/// A success exit code when linting has no active errors, or a failure exit code when errors are present or execution fails.
///
/// # Errors
///
/// Returns `CliError` when input loading, `KiCad` execution, profile validation, or report output fails.
fn execute(cli: Cli) -> Result<ExitCode, CliError> {
    match cli.command {
        Commands::Lint {
            schematic,
            format,
            skip_erc,
            kicad_cli,
            project,
            profile,
            policy,
            board,
            strict,
        } => lint(
            &schematic,
            LintOptions {
                format,
                skip_erc,
                explicit_cli: kicad_cli,
                project_path: project.as_deref(),
                profile_path: profile,
                policy_path: policy.as_deref(),
                board_path: board,
                strict,
            },
        ),
    }
}
/// Loads the schematic and any requested project/profile data, runs native ERC and Rust checks, then renders the normalized findings.
///
/// # Arguments
///
/// * `schematic_path` — Path to the `.kicad_sch` file being analyzed.
/// * `options` — Format, ERC, project, profile, policy, and strict-mode inputs for this invocation.
///
/// # Returns
///
/// A success exit code when there are no active errors, or a failure exit code when errors are present.
///
/// # Errors
///
/// Returns `CliError` for schematic parsing, project/profile decoding, native ERC/netlist failures, or output serialization/writing.
fn lint(schematic_path: &Path, options: LintOptions<'_>) -> Result<ExitCode, CliError> {
    let LintOptions {
        format,
        skip_erc,
        explicit_cli,
        project_path,
        profile_path,
        policy_path,
        board_path,
        strict,
    } = options;
    if strict && profile_path.is_none() {
        return Err(CliError::StrictNeedsProfile);
    }
    if board_path.is_some() && profile_path.is_none() {
        return Err(CliError::BoardNeedsProfile);
    }
    let schematic = SchematicDocument::from_path(schematic_path)?;
    let board = board_path
        .as_ref()
        .map(BoardDocument::from_path)
        .transpose()?;
    let project = project_path
        .map(project_settings_from_project)
        .transpose()?;
    let default_schematic_line_stroke_width_mm = project
        .as_ref()
        .and_then(project_default_schematic_line_stroke_width_mm);
    let needs_netlist = profile_path.is_some() || project_path.is_some();
    let cli = if skip_erc && !needs_netlist {
        None
    } else {
        Some(resolve_kicad_cli(explicit_cli)?)
    };
    let erc_report = if skip_erc {
        None
    } else {
        let Some(cli_path) = cli.as_deref() else {
            return Err(CliError::KiCadCliNotFound);
        };
        Some(run_native_erc(cli_path, schematic_path)?)
    };
    let netlist = if needs_netlist {
        let Some(cli_path) = cli.as_deref() else {
            return Err(CliError::KiCadCliNotFound);
        };
        Some(export_netlist(cli_path, schematic_path)?)
    } else {
        None
    };

    let (kicad_version, mut diagnostics) = match erc_report {
        Some(report) => (report.kicad_version, report.diagnostics),
        None => (None, Vec::new()),
    };
    let project_wire_stroke_widths = project
        .as_ref()
        .zip(netlist.as_ref())
        .map(|(project, netlist)| {
            let facts = SchematicFacts::from_document(&schematic);
            project_wire_stroke_widths_mm(project, &facts, netlist)
        })
        .unwrap_or_default();
    diagnostics.extend(
        RuleEngine::with_builtins()
            .run_with_wire_stroke_widths(
                &schematic,
                &project_wire_stroke_widths,
                default_schematic_line_stroke_width_mm,
            )
            .diagnostics,
    );
    diagnostics.extend(validate_profile_inputs(
        profile_path.as_deref(),
        &schematic,
        netlist.as_ref(),
        board.as_ref(),
        strict,
    )?);
    diagnostics = apply_optional_rule_policy(policy_path, diagnostics)?;
    let summary = Summary::from_diagnostics(&diagnostics);
    let output = LintOutput {
        schema_version: "1",
        tool_version: env!("CARGO_PKG_VERSION"),
        schematic: schematic.source().path().display().to_string(),
        syntax_valid: true,
        erc_skipped: skip_erc,
        kicad_version,
        diagnostics,
        summary,
    };

    render_output(format, &output, schematic.source())
}

/// Renders a completed lint result and chooses the process exit code from its active errors.
///
/// # Arguments
///
/// * `format` — Output format selected for this invocation.
/// * `output` — Completed lint result containing diagnostics and their summary.
/// * `source` — Original schematic source used for text-renderer line context.
///
/// # Returns
///
/// `ExitCode::FAILURE` when the summary contains active errors, otherwise `ExitCode::SUCCESS`.
///
/// # Errors
///
/// Returns a renderer, JSON serialization, or standard-output I/O error when the result cannot be written.
fn render_output(
    format: OutputFormat,
    output: &LintOutput,
    source: &SourceFile,
) -> Result<ExitCode, CliError> {
    match format {
        OutputFormat::Text => render_text(output, source)?,
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(output)?;
            writeln!(io::stdout().lock(), "{json}")?;
        }
    }
    Ok(if output.summary.errors > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// Loads the optional component profile and validates it against schematic and board evidence.
///
/// # Arguments
///
/// * `profile_path` — Optional JSON project-profile path; `None` disables profile checks.
/// * `schematic` — Parsed schematic containing the placed symbols and embedded pin geometry.
/// * `netlist` — Optional exported schematic netlist used by logical component-pin contracts.
/// * `board` — Optional parsed PCB used by board-level pad-distance contracts.
/// * `strict` — Whether the loaded profile must enforce strict component metadata completeness.
///
/// # Returns
///
/// Profile diagnostics, including PCB001 findings when board-distance metadata is declared.
///
/// # Errors
///
/// Returns a profile I/O or decoding error when the specified JSON file cannot be loaded or validated.
fn validate_profile_inputs(
    profile_path: Option<&Path>,
    schematic: &SchematicDocument,
    netlist: Option<&Netlist>,
    board: Option<&BoardDocument>,
    strict: bool,
) -> Result<Vec<Diagnostic>, CliError> {
    let Some(profile_path) = profile_path else {
        return Ok(Vec::new());
    };
    let mut profile = LintProfile::from_path(profile_path)?;
    if strict {
        profile.mode = ProfileMode::Strict;
    }
    Ok(board.map_or_else(
        || profile.validate_with_netlist(schematic, netlist),
        |board| profile.validate_with_board(schematic, netlist, board),
    ))
}

/// Reads and decodes an optional rule policy written in TOML.
///
/// # Arguments
///
/// * `policy_path` — File containing a `[rules]` table and optional named preset.
///
/// # Returns
///
/// The decoded `RulePolicy` ready for validation against the built-in descriptor catalogue.
///
/// # Errors
///
/// Returns an I/O error when the file cannot be read, or a TOML error when its contents are invalid.
fn load_rule_policy(policy_path: &Path) -> Result<RulePolicy, CliError> {
    let contents = fs::read_to_string(policy_path)?;
    Ok(parse_rule_policy(&contents)?)
}

/// Applies the optional TOML rule policy to built-in diagnostics.
///
/// # Arguments
///
/// * `policy_path` — Optional policy file path; `None` preserves existing rule-emitted severities.
/// * `diagnostics` — Native and custom diagnostics produced by this lint invocation.
///
/// # Returns
///
/// The diagnostics after applying the selected preset and explicit built-in rule overrides.
///
/// # Errors
///
/// Returns a policy decoding, registry construction, or unknown-rule validation error.
fn apply_optional_rule_policy(
    policy_path: Option<&Path>,
    diagnostics: Vec<Diagnostic>,
) -> Result<Vec<Diagnostic>, CliError> {
    let Some(policy_path) = policy_path else {
        return Ok(diagnostics);
    };
    let policy = load_rule_policy(policy_path)?;
    let registry = builtin_rule_registry()?;
    Ok(policy.apply(&registry, diagnostics)?)
}

/// Parses a TOML policy string.
///
/// # Arguments
///
/// * `contents` — TOML source text containing a rule preset and optional rule overrides.
///
/// # Returns
///
/// The policy configuration parsed from the supplied TOML source text.
///
/// # Errors
///
/// Returns a TOML deserialization error when the source does not match the policy schema.
fn parse_rule_policy(contents: &str) -> Result<RulePolicy, toml::de::Error> {
    toml::from_str(contents)
}

/// Reads and decodes a `KiCad` project JSON file.
///
/// # Arguments
///
/// * `project_path` — Optional `KiCad` project file providing netclass and schematic drawing settings.
///
/// # Returns
///
/// The decoded project JSON value.
///
/// # Errors
///
/// Returns an I/O error when the project file cannot be read, or a JSON error when its contents are malformed.
fn project_settings_from_project(project_path: &Path) -> Result<serde_json::Value, CliError> {
    let contents = fs::read_to_string(project_path)?;
    Ok(serde_json::from_str(&contents)?)
}
/// Reads the project's default schematic line thickness and converts its mil value to millimetres.
///
/// # Arguments
///
/// * `project` — Decoded `KiCad` project JSON object.
///
/// # Returns
///
/// The positive default line width in millimetres, or `None` when the project does not provide a valid value.
fn project_default_schematic_line_stroke_width_mm(project: &serde_json::Value) -> Option<f64> {
    let width_mils = project
        .get("schematic")?
        .get("drawing")?
        .get("default_line_thickness")?
        .as_f64()?;
    let width_mm = width_mils * MILLIMETRES_PER_MIL;
    (width_mm.is_finite() && width_mm > 0.0).then_some(width_mm)
}
/// Resolves effective netclass wire widths for wires whose physical components map unambiguously to exported logical nets.
///
/// # Arguments
///
/// * `project` — Decoded `KiCad` project JSON object.
/// * `facts` — Schematic facts containing placed pins and the topology of physical wire components.
/// * `netlist` — Flattened netlist exported by `KiCad`, including pin-to-net and effective netclass data.
///
/// # Returns
///
/// A map from each unambiguously resolved wire's source byte offset to its effective width in millimetres.
fn project_wire_stroke_widths_mm(
    project: &serde_json::Value,
    facts: &SchematicFacts,
    netlist: &Netlist,
) -> BTreeMap<usize, f64> {
    // Directive labels and rule areas contribute runtime netclass assignments
    // that are not represented by the project-only resolver below.
    if facts.has_netclass_assignments || !netlist_pin_coordinates_are_complete(facts, netlist) {
        return BTreeMap::new();
    }

    let component_ids = physical_wire_component_ids(facts);
    let mut component_nets: BTreeMap<usize, BTreeSet<String>> = BTreeMap::new();
    let mut components_with_unmapped_pins = BTreeSet::new();

    for pin in &facts.pins {
        // `KiCad` excludes virtual and power-symbol references from flattened netlist nodes.
        if pin.reference.starts_with('#') {
            continue;
        }
        let key = physical_wire_component_key(pin.position_mm.0, pin.position_mm.1);
        let Some(component) = component_ids.get(&key).copied() else {
            continue;
        };
        if let Some(net_name) = netlist.net_for_pin(&pin.reference, &pin.number) {
            component_nets
                .entry(component)
                .or_default()
                .insert(net_name.to_owned());
        } else {
            components_with_unmapped_pins.insert(component);
        }
    }

    let mut net_widths = BTreeMap::new();
    for net_name in netlist.nets.keys() {
        if let Some(width_mm) = project_netclass_wire_width_mm(project, netlist, net_name) {
            net_widths.insert(net_name.as_str(), width_mm);
        }
    }

    let mut widths_by_wire = BTreeMap::new();
    for wire in &facts.wires {
        let start_key = physical_wire_component_key(wire.start_mm.0, wire.start_mm.1);
        let end_key = physical_wire_component_key(wire.end_mm.0, wire.end_mm.1);
        let Some(start_component) = component_ids.get(&start_key).copied() else {
            continue;
        };
        let Some(end_component) = component_ids.get(&end_key).copied() else {
            continue;
        };
        if start_component != end_component
            || components_with_unmapped_pins.contains(&start_component)
        {
            continue;
        }
        let Some(names) = component_nets.get(&start_component) else {
            continue;
        };
        if names.len() != 1 {
            continue;
        }
        let Some(net_name) = names.first() else {
            continue;
        };
        if let Some(width_mm) = net_widths.get(net_name.as_str()) {
            widths_by_wire.insert(wire.span_start, *width_mm);
        }
    }
    widths_by_wire
}
/// Checks that every non-virtual pin in the exported netlist has a corresponding coordinate-bearing pin fact.
///
/// # Arguments
///
/// * `facts` — Schematic facts containing placed pins and the topology of physical wire components.
/// * `netlist` — Flattened netlist exported by `KiCad`, including pin-to-net and effective netclass data.
///
/// # Returns
///
/// `true` when every exported non-virtual pin is represented in the schematic facts; otherwise `false`.
fn netlist_pin_coordinates_are_complete(facts: &SchematicFacts, netlist: &Netlist) -> bool {
    let schematic_pin_refs = facts
        .pins
        .iter()
        .filter(|pin| !pin.reference.starts_with('#'))
        .map(|pin| (pin.reference.as_str(), pin.number.as_str()))
        .collect::<BTreeSet<_>>();

    netlist.pin_nets.keys().all(|pin_ref| {
        pin_ref.reference.starts_with('#')
            || schematic_pin_refs.contains(&(pin_ref.reference.as_str(), pin_ref.pin.as_str()))
    })
}
/// Project netclasses keyed by name, with each entry storing priority and optional wire width in millimetres.
type ProjectNetclassDefinitions = BTreeMap<String, (i64, Option<f64>)>;
/// Resolves the effective wire width for one net from the project's explicit assignments, patterns, chains, priorities, and Default class.
///
/// # Arguments
///
/// * `project` — Decoded `KiCad` project JSON object.
/// * `netlist` — Flattened netlist exported by `KiCad`, including pin-to-net and effective netclass data.
/// * `net_name` — Exact logical net name whose project netclass assignments are being resolved.
///
/// # Returns
///
/// The effective wire width in millimetres, or `None` when the assignment cannot be resolved safely.
fn project_netclass_wire_width_mm(
    project: &serde_json::Value,
    netlist: &Netlist,
    net_name: &str,
) -> Option<f64> {
    let settings = project.get("net_settings")?.as_object()?;
    let classes = project_netclass_definitions(settings)?;
    let default_width_mm = classes.get("Default").and_then(|(_, width)| *width)?;
    let candidates = project_netclass_candidates(settings, netlist, net_name)?;
    let exported_class = netlist.nets.get(net_name)?.class_name.as_deref()?;
    if !exported_netclass_matches(exported_class, net_name, &candidates) {
        return None;
    }

    let selected = candidates
        .iter()
        .filter_map(|name| {
            let (priority, width_mm) = classes.get(name)?;
            width_mm.map(|width| (*priority, name.as_str(), width))
        })
        .min_by(|first, second| first.0.cmp(&second.0).then_with(|| first.1.cmp(second.1)));
    Some(selected.map_or(default_width_mm, |(_, _, width_mm)| width_mm))
}
/// Reads project netclass names, priorities, and optional wire widths into a lookup table.
///
/// # Arguments
///
/// * `settings` — The `net_settings` JSON object from the `KiCad` project.
///
/// # Returns
///
/// A map from class name to its priority and optional positive wire width in millimetres, or `None` when the settings are malformed.
fn project_netclass_definitions(
    settings: &serde_json::Map<String, serde_json::Value>,
) -> Option<ProjectNetclassDefinitions> {
    let class_rows = settings.get("classes")?.as_array()?;
    let mut classes = BTreeMap::new();
    for class in class_rows {
        let name = class.get("name")?.as_str()?.to_owned();
        let priority = class
            .get("priority")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or_else(|| {
                if name == "Default" {
                    i64::from(i32::MAX)
                } else {
                    -1
                }
            });
        let width_mils = class
            .get("wire_width")
            .and_then(serde_json::Value::as_f64)
            .or_else(|| (name == "Default").then_some(DEFAULT_NETCLASS_WIRE_WIDTH_MILS));
        let width_mm = width_mils.map(|width| width * MILLIMETRES_PER_MIL);
        let width_mm = width_mm.filter(|width| width.is_finite() && *width > 0.0);
        classes.insert(name, (priority, width_mm));
    }
    Some(classes)
}
/// Collects all project netclasses that may apply to the named net through explicit, pattern, or net-chain assignments.
///
/// # Arguments
///
/// * `settings` — The `net_settings` JSON object from the `KiCad` project.
/// * `netlist` — Flattened netlist exported by `KiCad`, including pin-to-net and effective netclass data.
/// * `net_name` — Exact logical net name whose project netclass assignments are being resolved.
///
/// # Returns
///
/// All supported class names that could apply to the given net, or `None` when the assignment data has an unsupported shape.
fn project_netclass_candidates(
    settings: &serde_json::Map<String, serde_json::Value>,
    netlist: &Netlist,
    net_name: &str,
) -> Option<BTreeSet<String>> {
    let mut candidates = BTreeSet::new();
    add_explicit_netclass_assignments(settings, net_name, &mut candidates)?;
    add_pattern_netclass_assignments(settings, net_name, &mut candidates)?;
    add_chain_netclass_assignments(settings, netlist, net_name, &mut candidates)?;
    Some(candidates)
}
/// Adds netclass names assigned directly to the exact logical net.
///
/// # Arguments
///
/// * `settings` — The `net_settings` JSON object from the `KiCad` project.
/// * `net_name` — Exact logical net name whose project netclass assignments are being resolved.
/// * `candidates` — Set to extend with each supported class assignment that applies to the net.
///
/// # Returns
///
/// `Some(())` after successfully processing the explicit-assignment object; `None` when it has an unsupported JSON shape.
fn add_explicit_netclass_assignments(
    settings: &serde_json::Map<String, serde_json::Value>,
    net_name: &str,
    candidates: &mut BTreeSet<String>,
) -> Option<()> {
    match settings.get("netclass_assignments") {
        None | Some(serde_json::Value::Null) => {}
        Some(serde_json::Value::Object(assignments)) => {
            if let Some(values) = assignments.get(net_name) {
                for value in values.as_array()? {
                    candidates.insert(value.as_str()?.to_owned());
                }
            }
        }
        _ => return None,
    }
    Some(())
}
/// Adds netclass names whose supported wildcard patterns match the logical net name.
///
/// # Arguments
///
/// * `settings` — The `net_settings` JSON object from the `KiCad` project.
/// * `net_name` — Exact logical net name whose project netclass assignments are being resolved.
/// * `candidates` — Set to extend with each supported class assignment that applies to the net.
///
/// # Returns
///
/// `Some(())` after processing supported pattern entries; `None` when the pattern settings have an unsupported shape.
fn add_pattern_netclass_assignments(
    settings: &serde_json::Map<String, serde_json::Value>,
    net_name: &str,
    candidates: &mut BTreeSet<String>,
) -> Option<()> {
    match settings.get("netclass_patterns") {
        None | Some(serde_json::Value::Null) => {}
        Some(serde_json::Value::Array(patterns)) => {
            for assignment in patterns {
                let pattern = assignment.get("pattern")?.as_str()?;
                let class_name = assignment.get("netclass")?.as_str()?;
                for member_pattern in expand_netclass_bus_pattern(pattern)? {
                    if netclass_pattern_matches(&member_pattern, net_name)? {
                        candidates.insert(class_name.to_owned());
                    }
                }
            }
        }
        _ => return None,
    }
    Some(())
}
/// Expands the supported KiCad bus-vector and bus-group forms used in project netclass patterns.
///
/// # Arguments
///
/// * `pattern` — Original project netclass pattern, possibly containing a bus vector or group.
///
/// # Returns
///
/// The expanded member patterns, or the original pattern when it is not a recognized bus form.
/// Returns `None` when a recognized expansion exceeds the conservative depth or member limits.
fn expand_netclass_bus_pattern(pattern: &str) -> Option<Vec<String>> {
    expand_netclass_bus_pattern_at_depth(pattern, 0)
}
/// Expands one bus pattern while enforcing recursion and output-size limits.
///
/// # Arguments
///
/// * `pattern` — Pattern or bus member to expand.
/// * `depth` — Current recursive bus-group nesting depth.
///
/// # Returns
///
/// Expanded member patterns, or a singleton containing the original input when it is not a bus.
fn expand_netclass_bus_pattern_at_depth(pattern: &str, depth: usize) -> Option<Vec<String>> {
    if depth > MAX_NETCLASS_BUS_EXPANSION_DEPTH {
        return None;
    }

    if let Some((prefix, first, last, suffix)) = parse_netclass_bus_vector(pattern) {
        let member_count = first.abs_diff(last).checked_add(1)?;
        if member_count > MAX_NETCLASS_BUS_MEMBERS {
            return None;
        }

        let mut members = Vec::with_capacity(member_count);
        for index in first.min(last)..=first.max(last) {
            members.push(format!("{prefix}{index}{suffix}"));
        }
        return Some(members);
    }

    if let Some(group_members) = parse_netclass_bus_group(pattern) {
        let mut expanded = Vec::new();
        for member in group_members {
            let mut member_patterns =
                expand_netclass_bus_pattern_at_depth(&member, depth + 1)?;
            if expanded.len().saturating_add(member_patterns.len()) > MAX_NETCLASS_BUS_MEMBERS {
                return None;
            }
            expanded.append(&mut member_patterns);
        }
        return Some(expanded);
    }

    if pattern.contains('{') || pattern.contains('}') {
        // Do not treat unrecognized formatting/group braces as a literal netclass pattern.
        if !has_supported_netclass_regex_quantifier(pattern) {
            return None;
        }
    }

    Some(vec![pattern.to_owned()])
}
/// Parses a simple numeric bus vector such as `/DATA[0..7]` or `D_{[1..2]}`.
///
/// # Arguments
///
/// * `pattern` — Candidate bus-vector pattern.
///
/// # Returns
///
/// The literal prefix, inclusive numeric range, and literal suffix when the pattern is supported.
fn parse_netclass_bus_vector(pattern: &str) -> Option<(&str, usize, usize, &str)> {
    let opening_bracket = pattern.rfind('[')?;
    let closing_bracket = pattern[opening_bracket + 1..].find(']')? + opening_bracket + 1;
    let range = &pattern[opening_bracket + 1..closing_bracket];
    let suffix = &pattern[closing_bracket + 1..];
    let prefix = &pattern[..opening_bracket];

    if range.contains('[')
        || range.contains(']')
        || prefix.contains(']')
        || suffix.contains('[')
        || suffix.contains(']')
    {
        return None;
    }

    let (first, last) = range.split_once("..")?;
    if first.is_empty()
        || last.is_empty()
        || !first.bytes().all(|byte| byte.is_ascii_digit())
        || !last.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }

    let first = first.parse::<usize>().ok()?;
    let last = last.parse::<usize>().ok()?;
    (first != last).then_some((prefix, first, last, suffix))
}
/// Parses a simple bus group into its top-level member patterns.
///
/// # Arguments
///
/// * `pattern` — Candidate bus-group pattern such as `/PCI{North[0..2] South[3..5]}`.
///
/// # Returns
///
/// The top-level members when a balanced group with at least two members is recognized.
fn parse_netclass_bus_group(pattern: &str) -> Option<Vec<String>> {
    let mut in_quotes = false;
    let mut escaped = false;
    let mut opening_brace = None;
    for (index, character) in pattern.char_indices() {
        if in_quotes {
            if escaped {
                escaped = false;
            } else if character as u32 == 92 {
                escaped = true;
            } else if character == '"' {
                in_quotes = false;
            }
            continue;
        }

        if character == '"' {
            in_quotes = true;
            continue;
        }
        if character != '{' {
            continue;
        }

        let formatting_marker_precedes_brace = pattern[..index]
            .chars()
            .next_back()
            .is_some_and(|preceding| matches!(preceding, '_' | '^' | '~'));
        if !formatting_marker_precedes_brace {
            opening_brace = Some(index);
            break;
        }
    }
    let opening_brace = opening_brace?;

    let mut nesting = 0_usize;
    let mut in_quotes = false;
    let mut escaped = false;
    let mut closing_brace = None;
    for (relative_index, character) in pattern[opening_brace..].char_indices() {
        if in_quotes {
            if escaped {
                escaped = false;
            } else if character as u32 == 92 {
                escaped = true;
            } else if character == '"' {
                in_quotes = false;
            }
            continue;
        }

        match character {
            '"' => in_quotes = true,
            '{' => nesting += 1,
            '}' => {
                nesting = nesting.checked_sub(1)?;
                if nesting == 0 {
                    closing_brace = Some(opening_brace + relative_index);
                    break;
                }
            }
            _ => {}
        }
    }
    let closing_brace = closing_brace?;
    if closing_brace + 1 != pattern.len() {
        return None;
    }

    let members = split_netclass_bus_group_members(&pattern[opening_brace + 1..closing_brace])?;
    (members.len() >= MIN_NETCLASS_BUS_GROUP_MEMBERS).then_some(members)
}
/// Splits a bus-group body on whitespace or commas outside nested bus vectors and groups.
///
/// # Arguments
///
/// * `body` — Text between the outer bus-group braces.
///
/// # Returns
///
/// Top-level member tokens, or `None` when nested delimiters or quoted names are unbalanced.
fn split_netclass_bus_group_members(body: &str) -> Option<Vec<String>> {
    let mut members = Vec::new();
    let mut current = String::new();
    let mut brace_depth = 0_usize;
    let mut bracket_depth = 0_usize;
    let mut in_quotes = false;
    let mut characters = body.chars().peekable();

    while let Some(character) = characters.next() {
        if in_quotes {
            if character == '"' {
                in_quotes = false;
            } else if character as u32 == 92 {
                current.push(characters.next()?);
            } else {
                current.push(character);
            }
            continue;
        }

        if character == '"' {
            in_quotes = true;
            continue;
        }
        if character as u32 == 92 && characters.peek() == Some(&' ') {
            let _ = characters.next();
            current.push(' ');
            continue;
        }

        match character {
            '{' => {
                brace_depth += 1;
                current.push(character);
            }
            '}' => {
                brace_depth = brace_depth.checked_sub(1)?;
                current.push(character);
            }
            '[' => {
                bracket_depth += 1;
                current.push(character);
            }
            ']' => {
                bracket_depth = bracket_depth.checked_sub(1)?;
                current.push(character);
            }
            separator if (separator.is_whitespace() || separator == ',')
                && brace_depth == 0
                && bracket_depth == 0 =>
            {
                if !current.is_empty() {
                    members.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(character),
        }
    }

    if in_quotes || brace_depth != 0 || bracket_depth != 0 {
        return None;
    }
    if !current.is_empty() {
        members.push(current);
    }
    Some(members)
}
/// Checks whether all brace pairs in a pattern form supported regular-expression quantifiers.
///
/// # Arguments
///
/// * `pattern` — Pattern containing braces that were not parsed as a bus group.
///
/// # Returns
///
/// `true` when each brace pair is a numeric quantifier such as `{2}`, `{2,4}`, or `{2,}`.
fn has_supported_netclass_regex_quantifier(pattern: &str) -> bool {
    let mut remaining = pattern;
    while let Some(opening) = remaining.find('{') {
        let Some(closing) = remaining[opening + 1..].find('}') else {
            return false;
        };
        let closing = opening + 1 + closing;
        let quantifier = &remaining[opening + 1..closing];
        let previous = remaining[..opening].chars().next_back();
        let previous_can_be_repeated = previous.is_some_and(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, ')' | ']' | '}' | '.' | '*' | '+' | '?')
        });
        let (minimum, maximum) = quantifier
            .split_once(',')
            .map_or((quantifier, None), |(minimum, maximum)| {
                (minimum, Some(maximum))
            });
        let valid_minimum = !minimum.is_empty()
            && minimum.bytes().all(|byte| byte.is_ascii_digit());
        let valid_maximum = maximum.is_none_or(|value| {
            value.is_empty() || value.bytes().all(|byte| byte.is_ascii_digit())
        });
        if !previous_can_be_repeated || !valid_minimum || !valid_maximum {
            return false;
        }
        remaining = &remaining[closing + 1..];
    }
    !remaining.contains('}')
}
/// Determines whether one expanded netclass pattern matches a complete exported net name.
///
/// # Arguments
///
/// * `pattern` — Expanded KiCad wildcard or regular-expression pattern used for the match.
/// * `candidate` — Complete hierarchical net name exported by KiCad and checked against the pattern.
///
/// # Returns
///
/// `Some(true/false)` for supported patterns; `None` when extensions outside the supported
/// expression subset would make a negative match unsafe to infer.
fn netclass_pattern_matches(pattern: &str, candidate: &str) -> Option<bool> {
    if pattern.contains("(?") || !netclass_regex_escapes_are_supported(pattern) {
        return None;
    }

    let regex_source = format!("^(?:{pattern})$");
    let regex_match = match Regex::new(&regex_source) {
        Ok(regex) => regex.is_match(candidate),
        Err(_) if pattern_has_regex_metacharacters(pattern) => return None,
        Err(_) => false,
    };
    Some(regex_match || wildcard_pattern_matches(pattern, candidate))
}
/// Checks whether every backslash escape belongs to the supported regular-expression subset.
///
/// # Arguments
///
/// * `pattern` — Expanded KiCad netclass pattern whose escape sequences are being checked.
///
/// # Returns
///
/// `true` when every escape quotes a supported regular-expression metacharacter; otherwise `false`.
fn netclass_regex_escapes_are_supported(pattern: &str) -> bool {
    let mut escaped = false;
    for byte in pattern.bytes() {
        if escaped {
            if !b"\\.^$*+?()[]{}|".contains(&byte) {
                return false;
            }
            escaped = false;
        } else if byte == 92 {
            escaped = true;
        }
    }
    !escaped
}
/// Checks whether a pattern uses syntax that cannot safely be treated as a wildcard-only pattern.
///
/// # Arguments
///
/// * `pattern` — Project netclass pattern whose metacharacters decide whether a failed regex compile stays unresolved.
///
/// # Returns
///
/// `true` when regular-expression syntax is present and a failed regex compile must remain unresolved.
fn pattern_has_regex_metacharacters(pattern: &str) -> bool {
    pattern.chars().any(|character| {
        matches!(character as u32, 94 | 36 | 91 | 93 | 40 | 41 | 43 | 124 | 123 | 125 | 92)
    })
}
/// Adds netclass names inherited from committed net chains containing the logical net.
///
/// # Arguments
///
/// * `settings` — The `net_settings` JSON object from the `KiCad` project.
/// * `netlist` — Flattened netlist exported by `KiCad`, including pin-to-net and effective netclass data.
/// * `net_name` — Exact logical net name whose project netclass assignments are being resolved.
/// * `candidates` — Set to extend with each supported class assignment that applies to the net.
///
/// # Returns
///
/// `Some(())` after processing chain assignments; `None` when the assignment data has an unsupported shape.
fn add_chain_netclass_assignments(
    settings: &serde_json::Map<String, serde_json::Value>,
    netlist: &Netlist,
    net_name: &str,
    candidates: &mut BTreeSet<String>,
) -> Option<()> {
    let chain_classes = match settings.get("net_chain_netclasses") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::Object(assignments)) => Some(assignments),
        _ => return None,
    };
    for chain in &netlist.chains {
        if !chain.members.contains(net_name) {
            continue;
        }
        let configured_class =
            match chain_classes.and_then(|assignments| assignments.get(&chain.name)) {
                Some(value) => Some(value.as_str()?),
                None => None,
            };
        let exported_class = chain.netclass.as_deref();
        let class_name = match (configured_class, exported_class) {
            (Some(configured), Some(exported)) if configured != exported => return None,
            (Some(configured), _) => Some(configured),
            (None, Some(exported)) => Some(exported),
            (None, None) => None,
        };
        if let Some(class_name) = class_name {
            candidates.insert(class_name.to_owned());
        }
    }
    Some(())
}
/// Checks that the project-derived candidate set is consistent with the effective class name exported by `KiCad`.
///
/// # Arguments
///
/// * `exported_class` — Effective class name reported by `KiCad`'s netlist exporter.
/// * `net_name` — Exact logical net name whose project netclass assignments are being resolved.
/// * `candidates` — Set to extend with each supported class assignment that applies to the net.
///
/// # Returns
///
/// `true` when `KiCad`'s exported effective class is compatible with the project-derived candidate set.
fn exported_netclass_matches(
    exported_class: &str,
    net_name: &str,
    candidates: &BTreeSet<String>,
) -> bool {
    if exported_class == "Default" {
        candidates.is_empty() || (candidates.len() == 1 && candidates.contains("Default"))
    } else if exported_class.strip_prefix("Effective for net: ") == Some(net_name) {
        !candidates.is_empty()
    } else {
        candidates.len() == 1 && candidates.contains(exported_class)
    }
}
/// Matches a project netclass pattern containing `*` and `?` against a complete net name.
///
/// # Arguments
///
/// * `pattern` — Supported netclass wildcard expression; `*` matches any sequence and `?` matches one character.
/// * `candidate` — Complete net name being tested against the wildcard expression.
///
/// # Returns
///
/// `true` when the entire candidate string matches the pattern.
fn wildcard_pattern_matches(pattern: &str, candidate: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let candidate: Vec<char> = candidate.chars().collect();
    let mut previous = vec![false; candidate.len() + 1];
    previous[0] = true;

    for pattern_char in pattern {
        let mut current = vec![false; candidate.len() + 1];
        match pattern_char {
            '*' => {
                current[0] = previous[0];
                for index in 1..=candidate.len() {
                    current[index] = previous[index] || current[index - 1];
                }
            }
            '?' => {
                current[1..].copy_from_slice(&previous[..candidate.len()]);
            }
            literal => {
                for index in 1..=candidate.len() {
                    current[index] = previous[index - 1] && candidate[index - 1] == literal;
                }
            }
        }
        previous = current;
    }
    previous[candidate.len()]
}
/// Writes the lint summary and diagnostics as human-readable text.
///
/// # Arguments
///
/// * `output` — Normalized lint output containing the diagnostics and summary counts.
/// * `source` — Original source file used to resolve diagnostic spans and render highlighted text.
///
/// # Returns
///
/// `Ok(())` when all text was written successfully.
///
/// # Errors
///
/// Returns an I/O error when writing the summary or any formatted diagnostic to standard output fails.
fn render_text(output: &LintOutput, source: &SourceFile) -> Result<(), io::Error> {
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "kicad-clippy {}", output.tool_version)?;
    writeln!(stdout, "S-expression syntax: valid ({})", output.schematic)?;

    if output.erc_skipped {
        writeln!(stdout, "Native KiCad ERC: skipped")?;
    } else if let Some(version) = &output.kicad_version {
        writeln!(stdout, "Native KiCad ERC: KiCad {version}")?;
    } else {
        writeln!(stdout, "Native KiCad ERC: completed")?;
    }

    if output.diagnostics.is_empty() && !output.erc_skipped {
        writeln!(stdout, "No native ERC violations reported.")?;
    }

    for diagnostic in &output.diagnostics {
        render_diagnostic(&mut stdout, diagnostic, source)?;
    }

    writeln!(
        stdout,
        "Summary: {} errors, {} warnings, {} notes, {} excluded",
        output.summary.errors,
        output.summary.warnings,
        output.summary.notes,
        output.summary.excluded
    )?;
    writeln!(stdout, "Built-in schematic rules: active")?;
    Ok(())
}
/// Formats one diagnostic, including its source span and schematic location when those are available.
///
/// # Arguments
///
/// * `stdout` — Output writer receiving the formatted diagnostic.
/// * `diagnostic` — Finding to render, including its message, severity, source span, and related context.
/// * `source` — Original source file used to resolve diagnostic spans and render highlighted text.
///
/// # Returns
///
/// `Ok(())` when the diagnostic was written successfully.
///
/// # Errors
///
/// Returns an I/O error when the formatted report for this diagnostic cannot be written to the supplied output stream.
fn render_diagnostic(
    stdout: &mut impl Write,
    diagnostic: &Diagnostic,
    source: &SourceFile,
) -> Result<(), io::Error> {
    let mut message = diagnostic.message.clone();
    if let Some(location) = diagnostic.schematic_location {
        let _ = write!(
            message,
            " at ({:.3} mm, {:.3} mm)",
            location.x_mm, location.y_mm
        );
    }
    if let Some(sheet_path) = &diagnostic.sheet_path {
        let _ = write!(message, " on sheet {sheet_path}");
    }
    if diagnostic.excluded {
        message.push_str(" [excluded]");
    }

    let mut rendered = MietteDiagnostic::new(message)
        .with_code(diagnostic.code.clone())
        .with_severity(match diagnostic.severity {
            Severity::Error => RenderSeverity::Error,
            Severity::Warning => RenderSeverity::Warning,
            Severity::Note => RenderSeverity::Advice,
        });

    if let Some(span) = diagnostic.span.filter(|span| source.slice(*span).is_some()) {
        rendered = rendered.with_label(LabeledSpan::new_primary_with_span(
            Some("here".to_owned()),
            span.start..span.end,
        ));
    }

    let mut guidance = Vec::new();
    if let Some(explanation) = &diagnostic.explanation {
        guidance.push(explanation.clone());
    }
    if let Some(help) = &diagnostic.help {
        guidance.push(format!("Help: {help}"));
    }
    if !diagnostic.related.is_empty() {
        let related = diagnostic
            .related
            .iter()
            .map(|item| {
                let mut description = item.description.clone();
                if let Some(uuid) = &item.uuid {
                    let _ = write!(description, " [{uuid}]");
                }
                if let Some(location) = item.location {
                    let _ = write!(
                        description,
                        " at ({:.3} mm, {:.3} mm)",
                        location.x_mm, location.y_mm
                    );
                }
                description
            })
            .collect::<Vec<_>>()
            .join("; ");
        guidance.push(format!("Related items: {related}"));
    }
    if !guidance.is_empty() {
        rendered = rendered.with_help(guidance.join("\n"));
    }

    let report = Report::new(rendered).with_source_code(NamedSource::new(
        source.path().display().to_string(),
        source.shared_text(),
    ));
    writeln!(stdout, "{report:?}")?;
    Ok(())
}
/// Resolves the `KiCad` CLI executable, preferring an explicit path before searching the environment.
///
/// # Arguments
///
/// * `explicit` — Optional executable path supplied by the caller.
///
/// # Returns
///
/// The explicit executable path or the first `KiCad` CLI executable found on PATH.
///
/// # Errors
///
/// Returns `CliError::KiCadCliNotFound` when neither an explicit path, `KICAD_CLI`, nor a callable `kicad-cli` executable can be resolved.
fn resolve_kicad_cli(explicit: Option<PathBuf>) -> Result<PathBuf, CliError> {
    if let Some(path) = explicit {
        return Ok(path);
    }

    if let Some(path) = std::env::var_os("KICAD_CLI") {
        return Ok(PathBuf::from(path));
    }

    for candidate in ["kicad-cli-nightly", "kicad-cli"] {
        if Command::new(candidate).arg("version").output().is_ok() {
            return Ok(PathBuf::from(candidate));
        }
    }

    Err(CliError::KiCadCliNotFound)
}
#[cfg(test)]
/// Unit tests for project setting conversion, conservative netclass resolution, and summary counts.
mod tests {
    use crate::core::{Diagnostic, Severity};

    use super::{
        Summary, parse_rule_policy, project_default_schematic_line_stroke_width_mm,
        project_netclass_wire_width_mm, project_wire_stroke_widths_mm, wildcard_pattern_matches,
    };
    use crate::erc::Netlist;
    use crate::rules::SchematicFacts;
    use std::collections::{BTreeMap, BTreeSet};
    /// Checks that TOML policy presets and explicit per-rule levels decode into the typed policy model.
    ///
    /// # Returns
    ///
    /// Ok when the preset and overrides round-trip to their expected typed values.
    ///
    /// # Errors
    ///
    /// Returns a TOML error if a valid policy fixture cannot be decoded.
    #[test]
    fn rule_policy_toml_decodes_preset_and_overrides() -> Result<(), toml::de::Error> {
        use crate::core::{RuleLevel, RulePreset};

        let policy = parse_rule_policy(
            r#"
preset = "strict"

[rules]
SCH013 = "warn"
SCH019 = "allow"
"#,
        )?;

        assert_eq!(policy.preset, RulePreset::Strict);
        assert_eq!(policy.rules.get("SCH013"), Some(&RuleLevel::Warn));
        assert_eq!(policy.rules.get("SCH019"), Some(&RuleLevel::Allow));
        Ok(())
    }

    /// Checks that unknown top-level TOML keys are rejected instead of silently ignored.
    ///
    /// # Returns
    ///
    /// Ok when a misspelled policy key causes TOML decoding to fail.
    #[test]
    fn rule_policy_toml_rejects_unknown_keys() {
        assert!(parse_rule_policy("presett = \"strict\"\n").is_err());
    }

    /// Checks that the project's default schematic line width is interpreted in mils and converted to millimetres.
    #[test]
    fn project_default_schematic_line_width_reads_mils() {
        let project = serde_json::json!({
            "schematic": {
                "drawing": {
                    "default_line_thickness": 6.0
                }
            }
        });

        assert!(
            project_default_schematic_line_stroke_width_mm(&project)
                .is_some_and(|width| (width - 0.1524).abs() < 1.0e-12)
        );
    }
    /// Checks that an absent, nonnumeric, zero, or negative default line width remains unresolved rather than being guessed.
    #[test]
    fn project_default_schematic_line_width_stays_unknown_when_missing_or_invalid() {
        let missing = serde_json::json!({"schematic": {"drawing": {}}});
        let zero = serde_json::json!({
            "schematic": {"drawing": {"default_line_thickness": 0.0}}
        });
        let negative = serde_json::json!({
            "schematic": {"drawing": {"default_line_thickness": -6.0}}
        });

        assert_eq!(
            project_default_schematic_line_stroke_width_mm(&missing),
            None
        );
        assert_eq!(project_default_schematic_line_stroke_width_mm(&zero), None);
        assert_eq!(
            project_default_schematic_line_stroke_width_mm(&negative),
            None
        );
    }
    /// Builds a valid project-settings fixture with `Default`, `HighSpeed`, `Critical`, and `Inheriting` netclasses and empty assignment lists.
    ///
    /// # Returns
    ///
    /// A mutable JSON value that tests customize to exercise class priority, wildcard matching, chain assignments, and default-width inheritance.
    fn project_fixture() -> serde_json::Value {
        serde_json::json!({
            "net_settings": {
                "classes": [
                    {"name": "Default", "priority": 2_147_483_647, "wire_width": 8.0},
                    {"name": "HighSpeed", "priority": 5, "wire_width": 18.0},
                    {"name": "Critical", "priority": 1, "wire_width": 30.0},
                    {"name": "Inheriting", "priority": 0}
                ],
                "netclass_assignments": {},
                "netclass_patterns": [],
                "net_chain_netclasses": {}
            }
        })
    }
    /// Checks that overlapping netclass patterns and exact assignments are resolved using `KiCad` class priority.
    #[test]
    fn overlapping_patterns_and_explicit_assignments_use_class_priority() {
        let mut project = project_fixture();
        project["net_settings"]["netclass_assignments"] =
            serde_json::json!({"/USB-C": ["HighSpeed"]});
        project["net_settings"]["netclass_patterns"] = serde_json::json!([
            {"pattern": "/USB*", "netclass": "HighSpeed"},
            {"pattern": "/USB-C", "netclass": "Critical"}
        ]);
        let netlist = Netlist {
            nets: BTreeMap::from([(
                "/USB-C".to_owned(),
                crate::erc::NetlistNet {
                    name: "/USB-C".to_owned(),
                    class_name: Some("Effective for net: /USB-C".to_owned()),
                    nodes: Vec::new(),
                },
            )]),
            ..Netlist::default()
        };

        assert_eq!(
            project_netclass_wire_width_mm(&project, &netlist, "/USB-C"),
            Some(0.762)
        );
        assert!(wildcard_pattern_matches("/USB?", "/USB1"));
        assert!(!wildcard_pattern_matches("/USB?", "/USB12"));
    }
    /// Checks that a selected class without its own wire width inherits the Default netclass width.
    #[test]
    fn class_without_wire_width_inherits_the_project_default() {
        let mut project = project_fixture();
        project["net_settings"]["netclass_assignments"] =
            serde_json::json!({"/PWR": ["Inheriting"]});
        let netlist = Netlist {
            nets: BTreeMap::from([(
                "/PWR".to_owned(),
                crate::erc::NetlistNet {
                    name: "/PWR".to_owned(),
                    class_name: Some("Effective for net: /PWR".to_owned()),
                    nodes: Vec::new(),
                },
            )]),
            ..Netlist::default()
        };

        assert_eq!(
            project_netclass_wire_width_mm(&project, &netlist, "/PWR"),
            Some(0.2032)
        );
    }
    /// Checks that a netclass assigned to a committed net chain is applied to each exported member net.
    #[test]
    fn committed_chain_netclass_is_applied_to_member_nets() {
        let mut project = project_fixture();
        project["net_settings"]["net_chain_netclasses"] = serde_json::json!({"DDR": "HighSpeed"});
        let netlist = Netlist {
            nets: BTreeMap::from([(
                "/DDR_DQ0".to_owned(),
                crate::erc::NetlistNet {
                    name: "/DDR_DQ0".to_owned(),
                    class_name: Some("Effective for net: /DDR_DQ0".to_owned()),
                    nodes: Vec::new(),
                },
            )]),
            chains: vec![crate::erc::NetlistChain {
                name: "DDR".to_owned(),
                netclass: Some("HighSpeed".to_owned()),
                members: BTreeSet::from(["/DDR_DQ0".to_owned()]),
            }],
            ..Netlist::default()
        };

        assert_eq!(
            project_netclass_wire_width_mm(&project, &netlist, "/DDR_DQ0"),
            Some(0.4572)
        );
    }
    /// Checks that conflicting or unsupported class-assignment syntax does not produce a guessed wire width.
    #[test]
    fn inconsistent_or_unmodeled_assignments_remain_unknown() {
        let mut project = project_fixture();
        for pattern in ["/^USB.*$/", "/USB.*/", "/DATA[7..0]", "/USB{DP DM}"] {
            project["net_settings"]["netclass_patterns"] =
                serde_json::json!([{"pattern": pattern, "netclass": "HighSpeed"}]);
            assert_eq!(
                project_netclass_wire_width_mm(&project, &Netlist::default(), "/USB1"),
                None
            );
        }
        project["net_settings"]["netclass_patterns"] =
            serde_json::json!([{"pattern": "^/USB.*$", "netclass": "HighSpeed"}]);
        let netlist = Netlist {
            nets: BTreeMap::from([(
                "/USB1".to_owned(),
                crate::erc::NetlistNet {
                    name: "/USB1".to_owned(),
                    class_name: Some("HighSpeed".to_owned()),
                    nodes: Vec::new(),
                },
            )]),
            ..Netlist::default()
        };
        assert_eq!(
            project_netclass_wire_width_mm(&project, &netlist, "/USB1"),
            Some(0.4572)
        );

        project["net_settings"]["netclass_patterns"] = serde_json::json!([]);
        assert_eq!(
            project_netclass_wire_width_mm(&project, &netlist, "/USB1"),
            None
        );
    }
    /// Resolves a regular-expression netclass pattern against the complete hierarchical net name.
    #[test]
    fn regex_netclass_patterns_match_complete_net_names() {
        let mut project = project_fixture();
        project["net_settings"]["netclass_patterns"] =
            serde_json::json!([{"pattern": "^/USB[0-9]+$", "netclass": "HighSpeed"}]);
        let netlist = Netlist {
            nets: BTreeMap::from([
                (
                    "/USB42".to_owned(),
                    crate::erc::NetlistNet {
                        name: "/USB42".to_owned(),
                        class_name: Some("HighSpeed".to_owned()),
                        nodes: Vec::new(),
                    },
                ),
                (
                    "/USB42-extra".to_owned(),
                    crate::erc::NetlistNet {
                        name: "/USB42-extra".to_owned(),
                        class_name: Some("HighSpeed".to_owned()),
                        nodes: Vec::new(),
                    },
                ),
            ]),
            ..Netlist::default()
        };

        assert_eq!(
            project_netclass_wire_width_mm(&project, &netlist, "/USB42"),
            Some(0.4572)
        );
        assert_eq!(
            project_netclass_wire_width_mm(&project, &netlist, "/USB42-extra"),
            None
        );
    }
    /// Expands descending and ascending numeric bus-vector ranges before resolving their member nets.
    #[test]
    fn bus_vector_netclass_patterns_expand_member_ranges() {
        let mut project = project_fixture();
        project["net_settings"]["netclass_patterns"] =
            serde_json::json!([{"pattern": "/DATA[7..0]", "netclass": "HighSpeed"}]);

        for net_name in ["/DATA7", "/DATA3", "/DATA0"] {
            let netlist = Netlist {
                nets: BTreeMap::from([(
                    net_name.to_owned(),
                    crate::erc::NetlistNet {
                        name: net_name.to_owned(),
                        class_name: Some("HighSpeed".to_owned()),
                        nodes: Vec::new(),
                    },
                )]),
                ..Netlist::default()
            };
            assert_eq!(
                project_netclass_wire_width_mm(&project, &netlist, net_name),
                Some(0.4572)
            );
        }
    }
    /// Expands numeric bus vectors while retaining suffix text around formatted subscript notation.
    #[test]
    fn bus_vector_netclass_patterns_preserve_suffixes() {
        let mut project = project_fixture();
        project["net_settings"]["netclass_patterns"] =
            serde_json::json!([{"pattern": "D_{[1..2]}", "netclass": "HighSpeed"}]);
        let netlist = Netlist {
            nets: BTreeMap::from([(
                "D_{2}".to_owned(),
                crate::erc::NetlistNet {
                    name: "D_{2}".to_owned(),
                    class_name: Some("HighSpeed".to_owned()),
                    nodes: Vec::new(),
                },
            )]),
            ..Netlist::default()
        };

        assert_eq!(
            project_netclass_wire_width_mm(&project, &netlist, "D_{2}"),
            Some(0.4572)
        );
    }
    /// Expands KiCad-style bus-group member vectors without carrying the group prefix into member net names.
    #[test]
    fn bus_group_netclass_patterns_expand_nested_member_vectors() {
        let mut project = project_fixture();
        project["net_settings"]["netclass_patterns"] =
            serde_json::json!([{"pattern": "/PCI{North[0..2], South[3..5]}", "netclass": "HighSpeed"}]);

        for net_name in ["North0", "North2", "South3", "South5"] {
            let netlist = Netlist {
                nets: BTreeMap::from([(
                    net_name.to_owned(),
                    crate::erc::NetlistNet {
                        name: net_name.to_owned(),
                        class_name: Some("HighSpeed".to_owned()),
                        nodes: Vec::new(),
                    },
                )]),
                ..Netlist::default()
            };

            assert_eq!(
                project_netclass_wire_width_mm(&project, &netlist, net_name),
                Some(0.4572)
            );
        }
    }
    /// Splits bus-group members on commas and whitespace while preserving quoted and escaped spaces.
    #[test]
    fn bus_group_member_splitter_handles_comma_and_quoted_names() {
        assert_eq!(
            split_netclass_bus_group_members(&format!(
                r#"CLK, DATA[0..1] "DATA BUS" CTRL{} CLK"#,
                char::from(92_u8)
            )),
            Some(vec![
                "CLK".to_owned(),
                "DATA[0..1]".to_owned(),
                "DATA BUS".to_owned(),
                "CTRL CLK".to_owned(),
            ])
        );
    }
    /// Leaves unsupported regex extensions and unbounded bus ranges unresolved rather than guessing a class width.
    #[test]
    fn unsupported_regex_extensions_and_bus_expansion_limits_remain_unknown() {
        let mut project = project_fixture();
        let netlist = Netlist {
            nets: BTreeMap::from([(
                "/USB1".to_owned(),
                crate::erc::NetlistNet {
                    name: "/USB1".to_owned(),
                    class_name: Some("HighSpeed".to_owned()),
                    nodes: Vec::new(),
                },
            )]),
            ..Netlist::default()
        };

        project["net_settings"]["netclass_patterns"] =
            serde_json::json!([{"pattern": "^/USB(?=1)$", "netclass": "HighSpeed"}]);
        assert_eq!(
            project_netclass_wire_width_mm(&project, &netlist, "/USB1"),
            None
        );

        project["net_settings"]["netclass_patterns"] =
            serde_json::json!([{"pattern": "^/USB\\d+$", "netclass": "HighSpeed"}]);
        assert_eq!(
            project_netclass_wire_width_mm(&project, &netlist, "/USB1"),
            None
        );

        project["net_settings"]["netclass_patterns"] =
            serde_json::json!([{"pattern": "/DATA[0..5000]", "netclass": "HighSpeed"}]);
        assert_eq!(
            project_netclass_wire_width_mm(&project, &netlist, "/USB1"),
            None
        );
    }
    /// Checks that a wire receives a project width only when its physical component maps unambiguously to one exported logical net.
    #[test]
    fn per_wire_width_requires_one_unambiguous_physical_component_net() {
        use crate::erc::{NetlistNode, PinRef};
        use crate::rules::{SchematicPinFact, SchematicWireFact};

        let project = serde_json::json!({
            "net_settings": {
                "classes": [{"name": "Default", "wire_width": 10.0}],
                "netclass_assignments": {},
                "netclass_patterns": [],
                "net_chain_netclasses": {}
            }
        });
        let facts = SchematicFacts {
            wires: vec![SchematicWireFact {
                start_mm: (0.0, 0.0),
                end_mm: (10.0, 0.0),
                span_start: 42,
            }],
            pins: vec![
                SchematicPinFact {
                    reference: "J1".to_owned(),
                    number: "1".to_owned(),
                    position_mm: (0.0, 0.0),
                    electrical_type: None,
                },
                SchematicPinFact {
                    reference: "#PWR01".to_owned(),
                    number: "1".to_owned(),
                    position_mm: (0.0, 0.0),
                    electrical_type: None,
                },
            ],
            ..SchematicFacts::default()
        };
        let mut netlist = Netlist::default();
        netlist.pin_nets.insert(
            PinRef {
                reference: "J1".to_owned(),
                pin: "1".to_owned(),
            },
            "N1".to_owned(),
        );
        netlist.nets.insert(
            "N1".to_owned(),
            crate::erc::NetlistNet {
                name: "N1".to_owned(),
                class_name: Some("Default".to_owned()),
                nodes: vec![NetlistNode {
                    reference: "J1".to_owned(),
                    pin: "1".to_owned(),
                }],
            },
        );

        let widths = project_wire_stroke_widths_mm(&project, &facts, &netlist);
        assert_eq!(widths.get(&42), Some(&0.254));

        let mut incomplete_netlist = netlist.clone();
        incomplete_netlist.pin_nets.insert(
            PinRef {
                reference: "U2".to_owned(),
                pin: "1".to_owned(),
            },
            "N2".to_owned(),
        );
        assert!(project_wire_stroke_widths_mm(&project, &facts, &incomplete_netlist).is_empty());

        let conflicting_facts = SchematicFacts {
            pins: vec![
                SchematicPinFact {
                    reference: "J1".to_owned(),
                    number: "1".to_owned(),
                    position_mm: (0.0, 0.0),
                    electrical_type: None,
                },
                SchematicPinFact {
                    reference: "J2".to_owned(),
                    number: "1".to_owned(),
                    position_mm: (10.0, 0.0),
                    electrical_type: None,
                },
            ],
            ..facts
        };
        netlist.pin_nets.insert(
            PinRef {
                reference: "J2".to_owned(),
                pin: "1".to_owned(),
            },
            "N2".to_owned(),
        );
        assert!(project_wire_stroke_widths_mm(&project, &conflicting_facts, &netlist).is_empty());
    }
    /// Checks that schematic-local netclass directives prevent project-only settings from being treated as the effective wire width.
    #[test]
    fn schematic_netclass_directives_suppress_project_only_width_inference() {
        let project = project_fixture();
        let facts = SchematicFacts {
            has_netclass_assignments: true,
            ..SchematicFacts::default()
        };
        assert!(project_wire_stroke_widths_mm(&project, &facts, &Netlist::default()).is_empty());
    }
    /// Checks that excluded error diagnostics remain visible but do not increase the summary's active-error count.
    #[test]
    fn summary_counts_only_active_errors() {
        let mut excluded = Diagnostic::new("ERC/EXCLUDED", Severity::Error, "ignored");
        excluded.excluded = true;
        let diagnostics = vec![
            Diagnostic::new("ERC/ERROR", Severity::Error, "blocking"),
            Diagnostic::new("ERC/WARNING", Severity::Warning, "review"),
            Diagnostic::new("ERC/NOTE", Severity::Note, "info"),
            excluded,
        ];
        let summary = Summary::from_diagnostics(&diagnostics);

        assert_eq!(summary.errors, 1);
        assert_eq!(summary.warnings, 1);
        assert_eq!(summary.notes, 1);
        assert_eq!(summary.excluded, 1);
    }
}
