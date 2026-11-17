#![forbid(unsafe_code)]

use std::{
    fmt::Write as _,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use clap::{Parser, Subcommand, ValueEnum};
use kicad_clippy_core::{Diagnostic, Severity, SourceFile};
use kicad_clippy_erc::{NetlistError, export_netlist, run_native_erc};
use kicad_clippy_parser::SchematicDocument;
use kicad_clippy_rules::{LintProfile, ProfileMode, RuleEngine};
use miette::{LabeledSpan, MietteDiagnostic, NamedSource, Report, Severity as RenderSeverity};
use serde::Serialize;
use thiserror::Error;

/// Clippy-inspired checks for `KiCad` schematic source files.
#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Parse a schematic and run `KiCad`'s native Electrical Rules Checker.
    Lint {
        /// Schematic source file to inspect.
        schematic: PathBuf,
        /// Output format.
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
        /// Skip ERC (profile checks may still invoke `KiCad` to export a netlist).
        #[arg(long)]
        skip_erc: bool,
        /// Explicit path or command name for kicad-cli.
        #[arg(long, value_name = "PATH")]
        kicad_cli: Option<PathBuf>,
        /// KiCad project file with netclass settings for SCH013 wire widths.
        #[arg(long, value_name = "FILE")]
        project: Option<PathBuf>,
        /// JSON project profile containing component-intent metadata.
        #[arg(long, value_name = "FILE")]
        profile: Option<PathBuf>,
        /// Require complete component-intent metadata (requires --profile).
        #[arg(long)]
        strict: bool,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum OutputFormat {
    /// Human-readable diagnostics.
    Text,
    /// Structured JSON diagnostics.
    Json,
}

#[derive(Debug, Error)]
enum CliError {
    #[error(transparent)]
    Schematic(#[from] kicad_clippy_parser::SchematicParseError),
    #[error(transparent)]
    Erc(#[from] kicad_clippy_erc::ErcError),
    #[error("KiCad CLI was not found; install KiCad or pass --kicad-cli PATH")]
    KiCadCliNotFound,
    #[error(transparent)]
    Metadata(#[from] kicad_clippy_rules::MetadataError),
    #[error(transparent)]
    Netlist(#[from] NetlistError),
    #[error("--strict requires --profile FILE")]
    StrictNeedsProfile,
    #[error("failed to serialize JSON output: {0}")]
    Json(#[from] serde_json::Error),
    #[error("failed to write CLI output: {0}")]
    Io(#[from] io::Error),
}

#[derive(Debug, Serialize)]
struct LintOutput {
    schema_version: &'static str,
    tool_version: &'static str,
    schematic: String,
    syntax_valid: bool,
    erc_skipped: bool,
    kicad_version: Option<String>,
    diagnostics: Vec<Diagnostic>,
    summary: Summary,
}

#[derive(Debug, Serialize)]
struct Summary {
    errors: usize,
    warnings: usize,
    notes: usize,
    excluded: usize,
}

impl Summary {
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

fn main() -> ExitCode {
    match execute(Cli::parse()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("kicad-clippy: {error}");
            ExitCode::FAILURE
        }
    }
}

fn execute(cli: Cli) -> Result<ExitCode, CliError> {
    match cli.command {
        Commands::Lint {
            schematic,
            format,
            skip_erc,
            kicad_cli,
            project,
            profile,
            strict,
        } => lint(
            &schematic, format, skip_erc, kicad_cli, project, profile, strict,
        ),
    }
}

fn lint(
    schematic_path: &Path,
    format: OutputFormat,
    skip_erc: bool,
    explicit_cli: Option<PathBuf>,
    project_path: Option<PathBuf>,
    profile_path: Option<PathBuf>,
    strict: bool,
) -> Result<ExitCode, CliError> {
    if strict && profile_path.is_none() {
        return Err(CliError::StrictNeedsProfile);
    }
    let schematic = SchematicDocument::from_path(schematic_path)?;
    let default_wire_stroke_width_mm = project_path
        .as_deref()
        .map(default_wire_stroke_width_mm_from_project)
        .transpose()?
        .flatten();
    let needs_netlist = profile_path.is_some();
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
    diagnostics.extend(
        RuleEngine::with_builtins()
            .run_with_default_wire_stroke_width(&schematic, default_wire_stroke_width_mm)
            .diagnostics,
    );
    if let Some(path) = profile_path {
        let mut profile = LintProfile::from_path(path)?;
        if strict {
            profile.mode = ProfileMode::Strict;
        }
        diagnostics.extend(profile.validate_with_netlist(&schematic, netlist.as_ref()));
    }
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

    match format {
        OutputFormat::Text => render_text(&output, schematic.source())?,
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&output)?;
            writeln!(io::stdout().lock(), "{json}")?;
        }
    }

    if output.summary.errors > 0 {
        Ok(ExitCode::FAILURE)
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

fn default_wire_stroke_width_mm_from_project(project_path: &Path) -> Result<Option<f64>, CliError> {
    let contents = fs::read_to_string(project_path)?;
    let project: serde_json::Value = serde_json::from_str(&contents)?;
    Ok(project_default_wire_stroke_width_mm(&project))
}

fn project_default_wire_stroke_width_mm(project: &serde_json::Value) -> Option<f64> {
    let settings = project.get("net_settings")?.as_object()?;

    // A global Default width is safe only when no project-level net assignment
    // mechanism can select a higher-priority class for an individual wire.
    for key in [
        "netclass_assignments",
        "net_chain_classes",
        "net_chain_netclasses",
    ] {
        if settings
            .get(key)
            .is_some_and(|value| value.as_object().is_none_or(|entries| !entries.is_empty()))
        {
            return None;
        }
    }
    if settings
        .get("netclass_patterns")
        .is_some_and(|value| value.as_array().is_none_or(|patterns| !patterns.is_empty()))
    {
        return None;
    }

    let classes = settings.get("classes")?.as_array()?;
    let default_class = classes
        .iter()
        .find(|class| class.get("name").and_then(serde_json::Value::as_str) == Some("Default"))?;
    let width_mils = default_class.get("wire_width")?.as_f64()?;
    let width_mm = width_mils * 0.0254;
    (width_mm.is_finite() && width_mm > 0.0).then_some(width_mm)
}

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
mod tests {
    use kicad_clippy_core::{Diagnostic, Severity};

    use super::{Summary, project_default_wire_stroke_width_mm};

    #[test]
    fn project_default_wire_width_reads_mils_only_without_assignments() {
        let project = serde_json::json!({
            "net_settings": {
                "classes": [{"name": "Default", "wire_width": 10.0}],
                "netclass_assignments": {},
                "netclass_patterns": [],
                "net_chain_classes": {},
                "net_chain_netclasses": {}
            }
        });

        assert_eq!(project_default_wire_stroke_width_mm(&project), Some(0.254));
    }

    #[test]
    fn project_default_wire_width_stays_unknown_when_assignments_exist() {
        let mut project = serde_json::json!({
            "net_settings": {
                "classes": [{"name": "Default", "wire_width": 10.0}],
                "netclass_assignments": {},
                "netclass_patterns": [],
                "net_chain_classes": {},
                "net_chain_netclasses": {}
            }
        });
        project["net_settings"]["netclass_patterns"] = serde_json::json!([
            {"pattern": "/USB*", "netclass": "HighSpeed"}
        ]);

        assert_eq!(project_default_wire_stroke_width_mm(&project), None);
    }

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
