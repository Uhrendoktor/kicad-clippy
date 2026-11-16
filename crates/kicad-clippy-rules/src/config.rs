use std::{collections::BTreeMap, fs, path::Path};

use kicad_clippy_core::{RuleLevel, RuleRegistryError};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Versioned configuration for built-in rules and an optional design profile.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RuleConfig {
    /// Configuration schema version. The initial schema is version 1.
    pub schema_version: u32,
    /// Named starting point: `recommended`, `strict`, or `generic`.
    pub preset: String,
    /// Per-rule level overrides.
    pub rules: BTreeMap<String, RuleLevel>,
    /// Optional declarative project-specific contracts.
    pub profile: Option<DesignProfile>,
}

impl Default for RuleConfig {
    fn default() -> Self {
        Self {
            schema_version: 1,
            preset: "recommended".to_owned(),
            rules: BTreeMap::new(),
            profile: None,
        }
    }
}

impl RuleConfig {
    /// Reads a JSON configuration file.
    ///
    /// # Errors
    ///
    /// Returns an error for I/O failures, invalid JSON, unsupported schema versions,
    /// or unrecognized preset names.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, RuleConfigError> {
        let path = path.as_ref();
        let contents = fs::read_to_string(path).map_err(|source| RuleConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let config: Self = serde_json::from_str(&contents)?;
        config.validate()?;
        Ok(config)
    }

    /// Validates the schema version and preset name.
    ///
    /// # Errors
    ///
    /// Returns an error when the schema version or preset is unsupported.
    pub fn validate(&self) -> Result<(), RuleConfigError> {
        if self.schema_version != 1 {
            return Err(RuleConfigError::UnsupportedSchema(self.schema_version));
        }
        if !matches!(self.preset.as_str(), "recommended" | "strict" | "generic") {
            return Err(RuleConfigError::UnknownPreset(self.preset.clone()));
        }
        Ok(())
    }

    /// Returns the level override for a rule, or its descriptor default.
    #[must_use]
    pub fn level_for(&self, id: &str, default: RuleLevel) -> RuleLevel {
        self.rules.get(id).copied().unwrap_or(default)
    }
}

/// Optional design-specific contracts; no KiCad example is imported by the rule engine.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DesignProfile {
    /// Human-readable profile name.
    pub name: String,
    /// Grid quantum in millimetres for symbol-origin alignment.
    pub grid_mm: Option<f64>,
    /// Global maximum distance between a visible field and its symbol geometry.
    pub field_symbol_max_gap_mm: Option<f64>,
    /// Per-reference overrides for the global field/symbol gap.
    pub field_symbol_gap_overrides_mm: BTreeMap<String, f64>,
    /// Minimum inset between visible text and a functional frame.
    pub frame_content_padding_mm: Option<f64>,
    /// Expected horizontal gap between primary functional frames.
    pub group_gap_mm: Option<f64>,
    /// Declared functional groups.
    pub groups: Vec<DesignGroup>,
    /// Component-pin pairs that must share one physical wire network.
    pub direct_wire_pairs: Vec<PinPair>,
    /// Configured common logical-net contracts.
    pub common_nets: Vec<CommonNetContract>,
    /// Pins that must carry an explicit no-connect marker.
    pub required_no_connect_pins: Vec<String>,
    /// Component pins that must face down (world angle 90 degrees).
    pub ground_pins_down: Vec<String>,
    /// Local labels forbidden on physically wired networks.
    pub forbidden_local_labels: Vec<String>,
    /// Component adjacency/alignment contracts.
    pub adjacent_components: Vec<AdjacencyContract>,
    /// Maximum distance from symbol origin to selected visible property anchors.
    pub property_distance_limits_mm: BTreeMap<String, f64>,
    /// Approved DNP instances and their required values.
    pub dnp_components: BTreeMap<String, String>,
    /// Per-group maximum internal wire lengths in millimetres.
    pub wire_length_budgets_mm: BTreeMap<String, f64>,
    /// Required title-block fields; empty means title-block rule uses its default checks.
    pub required_title_fields: Vec<String>,
    /// Shared evidence used by group documentation checks.
    pub evidence: DocumentEvidence,
    /// Type-aware component roles and electrical intent declarations.
    pub component_metadata: Vec<ComponentMetadata>,
    /// Component types for which metadata is mandatory. Strict mode always requires capacitors.
    pub require_component_metadata_for: Vec<ComponentKind>,
    /// Maximum schematic distance from a capacitor decoupling terminal to its target IC pin.
    pub decoupling_max_pin_distance_mm: Option<f64>,
    /// Maximum symbol-origin spacing between decouplers serving the same target pin.
    pub decoupling_max_cluster_spacing_mm: Option<f64>,
}

/// A functional schematic region and the components assigned to it.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DesignGroup {
    /// Stable group identifier.
    pub id: String,
    /// Exact title text used to locate the group tab.
    pub title: String,
    /// Component references owned by the group.
    pub members: Vec<String>,
    /// Component references whose purpose must be explained.
    pub documentation_members: Vec<String>,
    /// Function/rationale records for SCH020.
    pub component_purposes: Vec<ComponentPurpose>,
    /// Expected description phrases; each must appear in the associated text box.
    pub description_phrases: Vec<String>,
    /// Whether the group is part of the primary left-to-right lane.
    pub primary: bool,
}

/// A direct physical connection contract between two pins.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PinPair {
    /// First pin as `REFERENCE.NUMBER`.
    pub first: String,
    /// Second pin as `REFERENCE.NUMBER`.
    pub second: String,
}

/// A named common-net and optional physical-wire contract.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommonNetContract {
    /// Expected logical net name.
    pub name: String,
    /// Pins expected on the net.
    pub pins: Vec<String>,
    /// Whether the pins must be physically wired together.
    #[serde(default)]
    pub wire_connected: bool,
    /// Whether exactly one matching label must exist on the physical components.
    #[serde(default)]
    pub single_label: bool,
}

/// A pair of components with alignment and spacing expectations.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdjacencyContract {
    /// First component reference.
    pub left: String,
    /// Second component reference.
    pub right: String,
    /// Distance between component origins occupied by the symbol bodies.
    pub body_width_mm: f64,
    /// Maximum remaining gap between symbol bodies.
    pub max_gap_mm: f64,
    /// Maximum vertical misalignment. Defaults to 0.001 mm.
    #[serde(default = "default_alignment_tolerance")]
    pub alignment_tolerance_mm: f64,
}

/// Function and reason a component is present.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentPurpose {
    /// Component reference.
    pub reference: String,
    /// What the component electrically does.
    pub function: String,
    /// Why this component/value is required in the circuit.
    pub rationale: String,
}

/// Component categories used by the design-intent metadata system.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentKind {
    /// Fixed or variable capacitor.
    Capacitor,
    /// Fixed or variable resistor.
    Resistor,
    /// Diode, TVS or ESD protection device.
    Diode,
    /// Inductor, bead or choke.
    Inductor,
    /// MOSFET, BJT or other transistor.
    Transistor,
    /// Integrated circuit.
    IntegratedCircuit,
    /// Connector, socket or terminal.
    Connector,
    /// Test point.
    TestPoint,
    /// Other or project-specific component.
    Other,
}

/// Typed design role independent of the component's library symbol name.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentRole {
    /// Bulk energy storage on a supply rail.
    Bulk,
    /// High-frequency supply bypass/decoupling.
    Decoupling,
    /// Local bypass capacitor.
    Bypass,
    /// Signal or supply filter.
    Filter,
    /// AC/DC coupling.
    Coupling,
    /// Snubber or damping network component.
    Snubber,
    /// Reservoir or hold-up energy storage.
    Reservoir,
    /// Pull-up bias.
    PullUp,
    /// Pull-down bias.
    PullDown,
    /// Series current limiting.
    SeriesCurrentLimit,
    /// Discharge path.
    Discharge,
    /// Gate bias or gate pull.
    GateBias,
    /// Bleeder resistor.
    Bleeder,
    /// Current-sense resistor.
    CurrentSense,
    /// Transient clamp or suppression.
    Clamp,
    /// ESD protection.
    EsdProtection,
    /// Flyback/freewheel diode.
    Flyback,
    /// Rectification.
    Rectifier,
    /// Reverse-polarity protection.
    ReversePolarity,
    /// High-side electronic switch.
    HighSideSwitch,
    /// Low-side electronic switch.
    LowSideSwitch,
    /// Back-to-back MOSFET switch.
    BackToBackSwitch,
    /// Signal or power interface.
    Interface,
    /// Test access.
    TestAccess,
    /// Other explicitly described design role.
    Other,
}

/// Typed component intent used by profile-driven checks.
///
/// Pin references use `REFERENCE.NUMBER`; component terminal numbers are the
/// pin numbers in the embedded KiCad library symbol, not physical package pads.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentMetadata {
    /// Placed component reference.
    pub reference: String,
    /// Electrical/mechanical component category.
    pub kind: ComponentKind,
    /// Intended circuit role.
    pub role: ComponentRole,
    /// What this part electrically does.
    pub function: String,
    /// Why this part/value is used in this circuit.
    pub rationale: String,
    /// Pin the part serves, for example `U1.23` for a decoupling capacitor.
    pub associated_pin: Option<String>,
    /// This component's terminal that connects to `associated_pin`.
    pub connection_pin: Option<String>,
    /// Return terminal on this component, commonly its GND terminal.
    pub return_pin: Option<String>,
    /// Expected name of the common return net, for example `GND`.
    pub return_net: Option<String>,
    /// Shared cluster key, usually the same as `associated_pin`.
    pub cluster: Option<String>,
    /// Maximum connection-terminal to target-pin distance in millimetres.
    pub max_distance_mm: Option<f64>,
}

/// Datasheet or reviewed-image provenance for a schematic profile.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DocumentEvidence {
    /// Document name/revision or URL.
    pub document: Option<String>,
    /// Page number or page label.
    pub page: Option<String>,
    /// Figure/table identifier.
    pub figure: Option<String>,
    /// Path/URL to a reviewed image asset.
    pub screenshot_asset: Option<String>,
}

const fn default_alignment_tolerance() -> f64 {
    0.001
}

/// Configuration parsing and validation failures.
#[derive(Debug, Error)]
pub enum RuleConfigError {
    /// The configuration file could not be read.
    #[error("could not read rule configuration {path}: {source}")]
    Io {
        /// Configuration path.
        path: std::path::PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The JSON document could not be decoded.
    #[error("invalid rule configuration JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// The configuration schema version is unsupported.
    #[error("unsupported rule configuration schema version {0}")]
    UnsupportedSchema(u32),
    /// The configured preset name is not supported.
    #[error("unknown rule preset {0:?}; expected recommended, strict, or generic")]
    UnknownPreset(String),
    /// A registry descriptor could not be registered.
    #[error(transparent)]
    Registry(#[from] RuleRegistryError),
}
