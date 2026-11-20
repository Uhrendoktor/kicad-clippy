use crate::core::Diagnostic;
use crate::erc::Netlist;
use crate::parser::BoardDocument;
use crate::parser::SchematicDocument;
use crate::rules::Rule;
use crate::rules::generic::GEOMETRY_TOLERANCE_MM;
use crate::rules::generic::SchematicFacts;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use thiserror::Error;

/// Checks `SCH004` profile-declared physical pin-connectivity and wire-separation contracts.
#[path = "profile_rules/sch004.rs"]
mod sch004;

/// Shared transformed body geometry and clearance calculations for profile layout rules.
#[path = "profile_rules/geometry.rs"]
mod geometry;

/// Checks `SCH006` minimum spacing between functional-group member symbols.
#[path = "profile_rules/sch006.rs"]
mod sch006;

/// Checks `SCH007` consistency between profile group membership and native `KiCad` groups.
#[path = "profile_rules/sch007.rs"]
mod sch007;

/// Checks profile-declared maximum gaps between visible component fields and their own body outlines.
#[path = "profile_rules/sch010.rs"]
mod sch010;

/// Checks `SCH009` containment of group members and their associated schematic geometry.
#[path = "profile_rules/sch009.rs"]
mod sch009;

/// Checks `SCH012` minimum frame-inset clearance for declared group contents.
#[path = "profile_rules/sch012.rs"]
mod sch012;

/// Checks `SCH014` component pin-to-net expectations against the exported netlist.
#[path = "profile_rules/sch014.rs"]
mod sch014;

/// Checks `SCH016` internal wire-length budgets for functional schematic groups.
#[path = "profile_rules/sch016.rs"]
mod sch016;

/// Checks `SCH017` group membership, description references, and evidence requirements.
#[path = "profile_rules/sch017.rs"]
mod sch017;

/// Checks `SCH020` component function and rationale documentation requirements.
#[path = "profile_rules/sch020.rs"]
mod sch020;

/// Checks `SCH021` strict component metadata and role-specific connection requirements.
#[path = "profile_rules/sch021.rs"]
mod sch021;

/// Checks `SCH022` decoupling target-pin, terminal, return-net, and cluster verification.
#[path = "profile_rules/sch022.rs"]
mod sch022;

/// Checks `SCH025` required test-point coverage for project-critical nets.
#[path = "profile_rules/sch025.rs"]
mod sch025;

/// Checks `SCH026` configurable alignment of selected schematic anchors to a project grid.
#[path = "profile_rules/sch026.rs"]
mod sch026;

/// Checks UUID-linked functional-frame title tabs, title containment, and minimum group-frame spacing.
#[path = "profile_rules/sch028.rs"]
mod sch028;

/// Currently supported project-profile schema version.
const SUPPORTED_PROFILE_SCHEMA_VERSION: u32 = 1;
/// Minimum number of distinct pins required in a group contract.
const MINIMUM_PIN_GROUP_SIZE: usize = 2;
/// Decimal precision used in stable coordinate keys.
const COORDINATE_KEY_DECIMAL_PLACES: usize = 3;
/// Maximum normalized wire-compactness score.
const MAX_COMPACTNESS_SCORE: f64 = 1.0;
/// Dimensionless tolerance for comparing normalized scores.
const SCORE_COMPARISON_EPSILON: f64 = 0.001;

/// Profile enforcement mode.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileMode {
    /// Validate only metadata that is present.
    #[default]
    Standard,
    /// Require explicit metadata for every placed component.
    Strict,
}

/// Supported component families.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentKind {
    /// Two-terminal passive component that stores energy in an electric field and is used for filtering, coupling, timing, or decoupling.
    Capacitor,
    /// Passive component that provides resistance for current limiting, sensing, biasing, pull-up/pull-down, or voltage division.
    Resistor,
    /// Semiconductor device with directional conduction, including rectifiers, signal diodes, LEDs, and protection diodes.
    Diode,
    /// Passive magnetic component used for energy storage, filtering, impedance, or conversion-stage inductors.
    Inductor,
    /// Semiconductor device used for switching, amplification, or controlled power delivery.
    Transistor,
    /// Electrical interconnect that exposes circuit signals or power to another board, cable, or assembly.
    Connector,
    /// Probe point intended for manufacturing test, debug, or in-circuit measurement.
    TestPoint,
    /// Multi-function semiconductor device containing an integrated circuit, such as a controller, regulator, interface, or logic device.
    IntegratedCircuit,
    /// Overcurrent protection element designed to interrupt the circuit when current exceeds its rated limit.
    Fuse,
    /// Passive EMI-suppression component that presents frequency-dependent impedance to attenuate high-frequency noise.
    FerriteBead,
    /// Piezoelectric frequency reference used to establish or stabilize an oscillator clock.
    Crystal,
    /// Mechanical or electronic device that opens, closes, or selects one or more circuit paths.
    Switch,
    /// Component family that does not fit any of the specific built-in classifications.
    Other,
}

/// Extensible component role, such as `decoupling`, `bulk`, `pull_up`, `clamp`, or `load_switch`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ComponentRole(
    /// Extensible component role identifier.
    pub String,
);

/// Pin-specific intent for a decoupling capacitor.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DecouplingIntent {
    /// Reference and pin number, e.g. U1.21.
    pub target_pin: String,
    /// Capacitor terminal connected to the target pin.
    pub target_terminal: String,
    /// Capacitor terminal connected to the return network.
    pub return_terminal: String,
    /// Expected return net, commonly GND.
    pub return_net: String,
    /// Stable cluster identifier for capacitors serving the same target.
    pub cluster: String,
    /// Optional maximum schematic distance between the target IC pin and capacitor terminal anchor, in millimetres.
    /// This is a symbol-pin placement contract, not a PCB copper-path measurement.
    #[serde(default)]
    pub max_target_distance_mm: Option<f64>,
    /// Optional maximum Euclidean distance between target IC and capacitor pad centers on the PCB, in millimetres.
    /// This pad-center check does not evaluate routed copper or the return loop.
    #[serde(default)]
    pub max_target_pad_distance_mm: Option<f64>,
}

/// A pair of placed pins that must share one physical wire-only network.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DirectWirePair {
    /// First pin reference in REF.PIN form.
    pub first: String,
    /// Second pin reference in REF.PIN form.
    pub second: String,
}

/// A profile-defined set of pins that must carry one named physical net.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CommonPinNetGroup {
    /// Expected label/net name shared by all listed pin networks.
    pub name: String,
    /// Placed pins in REF.PIN form.
    pub pins: Vec<String>,
    /// Require all listed pins to share one continuous wire-only component.
    #[serde(default)]
    pub wire_connected: bool,
    /// Require all listed pins to resolve to the same exported logical net; SCH014 owns this check.
    #[serde(default)]
    pub netlist_connected: bool,
    /// Require exactly one matching label across the resolved wire components.
    #[serde(default)]
    pub single_label: bool,
}

/// A named set of pins that must all share one physical wire-only component.
///
/// Unlike `CommonPinNetGroup`, this contract does not require labels or an
/// exported netlist; labels never create physical connectivity edges.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PhysicalWireGroup {
    /// Stable identifier used in diagnostics.
    pub name: String,
    /// Placed pins in REF.PIN form.
    pub pins: Vec<String>,
}

/// A pair of pins that must not share a physical wire-only component.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SeparateWirePair {
    /// First pin reference in REF.PIN form.
    pub first: String,
    /// Second pin reference in REF.PIN form.
    pub second: String,
    /// Also require the pins to resolve to different exported logical nets; SCH014 owns this check.
    #[serde(default)]
    pub netlist_separate: bool,
}

/// Default coordinate tolerance for comparing schematic anchors with the configured grid, in millimetres.
const DEFAULT_GRID_ALIGNMENT_TOLERANCE_MM: f64 = 0.001;

/// A schematic object family selected for project grid-alignment validation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GridAlignmentTarget {
    /// Placed component symbol origins.
    SymbolOrigins,
    /// Transformed embedded-symbol pin anchors.
    PinAnchors,
    /// Local, global and hierarchical label anchors.
    LabelAnchors,
    /// Explicit schematic junction anchors.
    Junctions,
    /// Both endpoints of each physical wire segment.
    WireEndpoints,
}

/// Returns the default grid targets: symbol origins and label anchors.
///
/// # Returns
///
/// A set containing symbol-origin and label-anchor targets, leaving electrical anchors opt-in.
fn default_grid_alignment_targets() -> BTreeSet<GridAlignmentTarget> {
    BTreeSet::from([
        GridAlignmentTarget::SymbolOrigins,
        GridAlignmentTarget::LabelAnchors,
    ])
}

/// Returns the default grid origin at the sheet coordinate origin.
///
/// # Returns
///
/// The two-dimensional sheet coordinate origin, [0.0, 0.0], in millimetres.
const fn default_grid_origin_mm() -> [f64; 2] {
    [0.0, 0.0]
}

/// Returns the default coordinate tolerance for grid comparisons, in millimetres.
///
/// # Returns
///
/// The shared 0.001 mm coordinate tolerance.
const fn default_grid_alignment_tolerance_mm() -> f64 {
    DEFAULT_GRID_ALIGNMENT_TOLERANCE_MM
}

/// Opt-in project contract for aligning selected schematic anchors to a regular grid.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct GridAlignmentIntent {
    /// Positive grid spacing in millimetres.
    pub spacing_mm: f64,
    /// Sheet-space grid origin as [x, y], in millimetres.
    #[serde(default = "default_grid_origin_mm")]
    pub origin_mm: [f64; 2],
    /// Maximum allowed per-axis distance from the nearest grid coordinate, in millimetres.
    #[serde(default = "default_grid_alignment_tolerance_mm")]
    pub tolerance_mm: f64,
    /// Object families to check. Defaults to symbol origins and label anchors; pin/junction/wire checks are explicit opt-ins.
    #[serde(default = "default_grid_alignment_targets")]
    pub targets: BTreeSet<GridAlignmentTarget>,
    /// Component references exempt from symbol-origin grid checks.
    #[serde(default)]
    pub exempt_components: BTreeSet<String>,
    /// Pin references in REF.PIN form exempt from pin-anchor grid checks.
    #[serde(default)]
    pub exempt_pins: BTreeSet<String>,
    /// Label selectors in kind:name form, such as local:VDD, exempt from label-anchor grid checks.
    #[serde(default)]
    pub exempt_labels: BTreeSet<String>,
    /// Exact sheet-space anchors exempt from coordinate checks, within the geometry equality tolerance.
    #[serde(default)]
    pub exempt_coordinates_mm: Vec<[f64; 2]>,
}

/// Label scope enforced for a named schematic label.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LabelScopeKind {
    /// Ordinary sheet-local label.
    Local,
    /// Project-wide global label.
    Global,
    /// Hierarchical label exposed at the current sheet boundary.
    Hierarchical,
}

/// Project-specific policy for global-label use and required label scopes on the loaded sheet.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct LabelScopePolicy {
    /// When present, the only global label names permitted on the loaded sheet. An empty set forbids all global labels.
    #[serde(default)]
    pub allowed_global_labels: Option<BTreeSet<String>>,
    /// Global label names forbidden on the loaded sheet, even when an allow-list is configured.
    #[serde(default)]
    pub forbidden_global_labels: BTreeSet<String>,
    /// Label names and their required scopes on the loaded sheet.
    #[serde(default)]
    pub required_label_scopes: BTreeMap<String, LabelScopeKind>,
}

/// Declared purpose and engineering constraints for one component.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ComponentIntent {
    /// Component family.
    pub kind: ComponentKind,
    /// Circuit function category.
    pub role: ComponentRole,
    /// Concise description of the component's electrical function.
    pub function: Option<String>,
    /// Reason for selecting this implementation/value/topology.
    pub rationale: Option<String>,
    /// Datasheet, application note, design requirement, or review reference.
    pub evidence: Option<String>,
    /// Decoupling-specific pin and terminal contract.
    pub decoupling: Option<DecouplingIntent>,
    /// Named electrical connections, expressed as nets or reference/pin pairs.
    #[serde(default)]
    pub connections: BTreeMap<String, String>,
    /// Component pin number mapped to its expected `KiCad` net name.
    #[serde(default)]
    pub pin_nets: BTreeMap<String, String>,
    /// Extensible role-specific attributes such as interface standard or orientation.
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
    /// Optional per-field maximum distance from the component's transformed body outline, in millimetres.
    ///
    /// Supported keys are `Reference` and `Value`; values are non-negative finite distances.
    #[serde(default)]
    pub max_field_body_gap_mm: BTreeMap<String, f64>,
    /// Optional minimum voltage rating in volts.
    pub voltage_rating_min_v: Option<f64>,
    /// Optional minimum power rating in watts.
    pub power_rating_min_w: Option<f64>,
}

/// Component-specific text that must appear in the owning group description.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ComponentPurpose {
    /// Electrical function.
    pub function: Option<String>,
    /// Design rationale.
    pub rationale: Option<String>,
}

/// Source evidence for a functional schematic group.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GroupEvidence {
    /// Document title or identifier.
    pub document: Option<String>,
    /// Page number in the cited document.
    pub page: Option<u32>,
    /// Figure, table or section identifier.
    pub figure: Option<String>,
    /// Reviewed screenshot asset identifier.
    pub screenshot_asset: Option<String>,
}

/// Axis used to align functional-group member symbol origins.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupAlignmentAxis {
    /// Member origins share a horizontal row within the configured vertical tolerance.
    Horizontal,
    /// Member origins share a vertical column within the configured horizontal tolerance.
    Vertical,
}

/// Profile-level description and membership contract for one functional group.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct GroupIntent {
    /// References belonging to the functional group.
    #[serde(default)]
    pub members: Vec<String>,
    /// Members whose purpose must be described.
    #[serde(default)]
    pub documentation_members: Vec<String>,
    /// UUID of the text box containing the rendered group description.
    pub description_uuid: String,
    /// Optional UUID of the native `KiCad` group whose component membership is enforced.
    #[serde(default)]
    pub kicad_group_uuid: Option<String>,
    /// Optional UUID of the schematic rectangle that encloses the functional group.
    #[serde(default)]
    pub frame_uuid: Option<String>,
    /// Optional UUID of the rectangular title tab attached to this group's frame.
    #[serde(default)]
    pub frame_title_tab_uuid: Option<String>,
    /// Optional UUID of the title text box contained by the declared frame title tab.
    #[serde(default)]
    pub frame_title_text_box_uuid: Option<String>,
    /// Optional minimum Euclidean gap between this frame and other distinct declared group frames, in millimetres.
    #[serde(default)]
    pub minimum_frame_gap_mm: Option<f64>,
    /// Optional required clearance from group member origins and its description box to the frame edge.
    #[serde(default)]
    pub minimum_frame_clearance_mm: Option<f64>,
    /// Optional minimum Euclidean spacing between member symbol origins, in millimetres.
    #[serde(default)]
    pub minimum_member_origin_spacing_mm: Option<f64>,
    /// Optional minimum edge-to-edge clearance between supported member body outlines, in millimetres.
    #[serde(default)]
    pub minimum_member_body_clearance_mm: Option<f64>,
    /// Optional axis along which all member symbol origins must align.
    #[serde(default)]
    pub alignment_axis: Option<GroupAlignmentAxis>,
    /// Maximum perpendicular deviation from the declared alignment axis, in millimetres.
    #[serde(default)]
    pub alignment_tolerance_mm: Option<f64>,
    /// Optional maximum total internal wire length for the group frame, in millimetres.
    #[serde(default)]
    pub wire_length_budget_mm: Option<f64>,
    /// Component-specific function and rationale phrases.
    #[serde(default)]
    pub component_purposes: BTreeMap<String, ComponentPurpose>,
    /// Datasheet/document citation or reviewed screenshot evidence.
    pub evidence: Option<GroupEvidence>,
}

/// Per-project rule configuration.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LintProfile {
    /// Profile schema version.
    pub schema_version: u32,
    /// Standard or strict metadata enforcement.
    #[serde(default)]
    pub mode: ProfileMode,
    /// Component reference to intent metadata.
    #[serde(default)]
    pub components: BTreeMap<String, ComponentIntent>,
    /// References intentionally exempt from strict metadata requirements.
    #[serde(default)]
    pub ignore_components: BTreeSet<String>,
    /// Functional groups and their documentation/evidence contracts.
    #[serde(default)]
    pub groups: BTreeMap<String, GroupIntent>,
    /// Pin pairs that must be joined by physical wires, not by labels.
    #[serde(default)]
    pub direct_wire_pairs: Vec<DirectWirePair>,
    /// Local label names forbidden on physical networks covered by direct-wire pairs.
    #[serde(default)]
    pub forbidden_direct_wire_labels: BTreeSet<String>,
    /// Pin groups requiring a named common physical/logical net contract.
    #[serde(default)]
    pub common_pin_groups: Vec<CommonPinNetGroup>,
    /// Named multi-pin groups that must share one physical wire-only component, independent of labels.
    #[serde(default)]
    pub physical_wire_groups: Vec<PhysicalWireGroup>,
    /// Pin pairs that must remain physically separate, with optional netlist separation.
    #[serde(default)]
    pub separate_wire_pairs: Vec<SeparateWirePair>,
    /// Explicit title-block fields that must be present and non-empty on this schematic sheet.
    #[serde(default)]
    pub required_title_block_fields: BTreeSet<String>,
    /// Named critical nets that must have at least one profile-declared test-point pin.
    #[serde(default)]
    pub required_test_point_nets: BTreeSet<String>,
    /// Optional grid-alignment policy for selected symbol, pin, label and wire anchors.
    #[serde(default)]
    pub grid_alignment: Option<GridAlignmentIntent>,
    /// Optional current-sheet label naming and scope policy.
    #[serde(default)]
    pub label_scope_policy: Option<LabelScopePolicy>,
}

/// Immutable data shared by all project-profile rule implementations.
pub struct ProfileRuleContext<'a> {
    /// Profile declarations evaluated by the rule set.
    profile: &'a LintProfile,
    /// Normalized facts extracted from the parsed schematic.
    facts: &'a SchematicFacts,
    /// Optional flattened netlist used by logical pin-to-net contracts.
    netlist: Option<&'a Netlist>,
}

impl LintProfile {
    /// Loads a component-intent profile, validates its schema version, and returns the decoded profile.
    ///
    /// # Arguments
    ///
    /// * `path` — Path to the profile JSON file to load.
    ///
    /// # Returns
    ///
    /// The decoded profile on success; an error describes file access, JSON decoding, or an unsupported profile schema.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read, the JSON is invalid, or the schema version is unsupported.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, MetadataError> {
        let content = fs::read_to_string(path).map_err(MetadataError::Io)?;
        let profile: Self = serde_json::from_str(&content).map_err(MetadataError::Json)?;
        if profile.schema_version != SUPPORTED_PROFILE_SCHEMA_VERSION {
            return Err(MetadataError::UnsupportedVersion(profile.schema_version));
        }
        Ok(profile)
    }

    /// Validates the profile against the schematic without relying on an exported electrical netlist.
    ///
    /// # Arguments
    ///
    /// * `document` — Parsed schematic whose symbols, geometry, labels, and group declarations are being checked.
    ///
    /// # Returns
    ///
    /// Diagnostics identifying profile declarations that do not match placed schematic components, physical connections, or group geometry.
    #[must_use]
    pub fn validate(&self, document: &SchematicDocument) -> Vec<Diagnostic> {
        self.validate_with_netlist(document, None)
    }

    /// Validates all declared component, group, and connectivity intent against the schematic and, when available, `KiCad`'s exported netlist.
    ///
    /// # Arguments
    ///
    /// * `document` — Parsed schematic whose symbols, geometry, labels, and group declarations are being checked.
    /// * `netlist` — Optional `KiCad`-exported flattened netlist; supplies logical pin-to-net assignments when present.
    ///
    /// # Returns
    ///
    /// Diagnostics identifying profile mismatches found in the schematic or in the exported logical pin-to-net assignments.
    #[must_use]
    pub fn validate_with_netlist(
        &self,
        document: &SchematicDocument,
        netlist: Option<&Netlist>,
    ) -> Vec<Diagnostic> {
        self.validate_internal(document, netlist, None)
    }

    /// Validates schematic intent and PCB001 pad-distance contracts against a parsed board.
    ///
    /// # Arguments
    ///
    /// * `document` — Parsed schematic whose component and connectivity intent is checked.
    /// * `netlist` — Optional exported schematic netlist used to verify target and capacitor pad nets.
    /// * `board` — Parsed PCB document containing placed footprints and pads.
    ///
    /// # Returns
    ///
    /// Schematic profile diagnostics plus PCB001 pad-distance findings.
    #[must_use]
    pub fn validate_with_board(
        &self,
        document: &SchematicDocument,
        netlist: Option<&Netlist>,
        board: &BoardDocument,
    ) -> Vec<Diagnostic> {
        self.validate_internal(document, netlist, Some(board))
    }

    /// Shared profile-validation path with optional PCB evidence.
    ///
    /// # Arguments
    ///
    /// * `document` — Parsed schematic document whose profile rules are evaluated.
    /// * `netlist` — Optional exported `KiCad` netlist used by logical pin-to-net contracts.
    /// * `board` — Optional parsed PCB document used by board-level pad-distance contracts.
    ///
    /// # Returns
    ///
    /// Profile diagnostics, including an unverified warning when a PCB contract has no board evidence.
    fn validate_internal(
        &self,
        document: &SchematicDocument,
        netlist: Option<&Netlist>,
        board: Option<&BoardDocument>,
    ) -> Vec<Diagnostic> {
        let facts = SchematicFacts::from_document(document);
        let context = ProfileRuleContext {
            profile: self,
            facts: &facts,
            netlist,
        };
        let mut diagnostics = Vec::new();
        diagnostics.extend(
            crate::rules::additional_profile_rules::validate_profile_extensions(self, &facts),
        );
        diagnostics.extend(sch021::Sch021.check(&context));
        diagnostics.extend(sch020::Sch020.check(&context));
        diagnostics.extend(sch022::Sch022.check(&context));
        diagnostics.extend(sch025::Sch025.check(&context));
        diagnostics.extend(sch026::Sch026.check(&context));
        diagnostics.extend(sch028::Sch028.check(&context));
        diagnostics.extend(sch014::Sch014.check(&context));
        diagnostics.extend(sch010::Sch010.check(&context));
        diagnostics.extend(sch017::Sch017.check(&context));
        diagnostics.extend(sch007::Sch007.check(&context));
        diagnostics.extend(sch009::Sch009.check(&context));
        diagnostics.extend(sch012::Sch012.check(&context));
        diagnostics.extend(sch006::Sch006.check(&context));
        diagnostics.extend(sch016::Sch016.check(&context));
        diagnostics.extend(sch004::Sch004.check(&context));
        if let Some(board) = board {
            diagnostics.extend(crate::rules::pcb_rules::validate_decoupling_pad_distances(
                self, board, netlist,
            ));
            diagnostics.extend(crate::rules::pcb_rules::validate_test_point_pad_access(
                self, board,
            ));
        } else {
            diagnostics.extend(crate::rules::pcb_rules::validate_missing_board(self));
            diagnostics.extend(crate::rules::pcb_rules::validate_missing_test_point_board(
                self,
            ));
        }
        diagnostics
    }
}

/// Tests whether two axis-aligned rectangles have a non-empty overlap.
///
/// # Arguments
///
/// * `first` — First schematic point or pin endpoint in the comparison.
/// * `second` — Second schematic point or pin endpoint in the comparison.
///
/// # Returns
///
/// `true` when the two rectangles share any area.
fn rectangles_overlap(first: (f64, f64, f64, f64), second: (f64, f64, f64, f64)) -> bool {
    first.0 <= second.2 + GEOMETRY_TOLERANCE_MM
        && first.2 >= second.0 - GEOMETRY_TOLERANCE_MM
        && first.1 <= second.3 + GEOMETRY_TOLERANCE_MM
        && first.3 >= second.1 - GEOMETRY_TOLERANCE_MM
}
/// Tests whether a point remains inside a frame after reserving the requested edge clearance.
///
/// # Arguments
///
/// * `clearance` — Minimum required distance from each frame edge, in millimetres.
///
/// # Returns
///
/// `true` when the point remains inside the frame after the requested inset is applied.
fn point_inside_frame_with_clearance(
    (left, top, right, bottom): (f64, f64, f64, f64),
    (x, y): (f64, f64),
    clearance: f64,
) -> bool {
    x >= left + clearance - GEOMETRY_TOLERANCE_MM
        && x <= right - clearance + GEOMETRY_TOLERANCE_MM
        && y >= top + clearance - GEOMETRY_TOLERANCE_MM
        && y <= bottom - clearance + GEOMETRY_TOLERANCE_MM
}
/// Tests whether an entire rectangle remains inside a frame after reserving the requested edge clearance.
///
/// # Arguments
///
/// * `clearance` — Minimum required distance from each frame edge, in millimetres.
///
/// # Returns
///
/// `true` when the full rectangle remains inside the frame after the requested inset is applied.
fn rectangle_inside_frame_with_clearance(
    (rect_left, rect_top, rect_right, rect_bottom): (f64, f64, f64, f64),
    (left, top, right, bottom): (f64, f64, f64, f64),
    clearance: f64,
) -> bool {
    rect_left >= left + clearance - GEOMETRY_TOLERANCE_MM
        && rect_top >= top + clearance - GEOMETRY_TOLERANCE_MM
        && rect_right <= right - clearance + GEOMETRY_TOLERANCE_MM
        && rect_bottom <= bottom - clearance + GEOMETRY_TOLERANCE_MM
}
/// Tests whether a point lies inside or on the edge of an axis-aligned frame rectangle.
///
/// # Returns
///
/// `true` when the point lies within the frame rectangle, including its boundary.
fn point_inside_frame(
    (left, top, right, bottom): (f64, f64, f64, f64),
    (x, y): (f64, f64),
) -> bool {
    x >= left - GEOMETRY_TOLERANCE_MM
        && x <= right + GEOMETRY_TOLERANCE_MM
        && y >= top - GEOMETRY_TOLERANCE_MM
        && y <= bottom + GEOMETRY_TOLERANCE_MM
}
/// Tests whether an axis-aligned rectangle is fully contained by another rectangle.
///
/// # Returns
///
/// `true` when the full rectangle is contained within the frame rectangle.
fn rectangle_inside_frame(
    (rect_left, rect_top, rect_right, rect_bottom): (f64, f64, f64, f64),
    (left, top, right, bottom): (f64, f64, f64, f64),
) -> bool {
    rect_left >= left - GEOMETRY_TOLERANCE_MM
        && rect_top >= top - GEOMETRY_TOLERANCE_MM
        && rect_right <= right + GEOMETRY_TOLERANCE_MM
        && rect_bottom <= bottom + GEOMETRY_TOLERANCE_MM
}
/// Checks whether a string uses the required REF.PIN syntax with non-empty reference and pin parts.
///
/// # Arguments
///
/// * `value` — value associated with this validation.
///
/// # Returns
///
/// `true` when the value contains non-empty reference and pin parts separated by a period.
fn valid_pin_ref(value: &str) -> bool {
    let Some((reference, pin)) = value.rsplit_once('.') else {
        return false;
    };
    !reference.is_empty()
        && !pin.is_empty()
        && pin
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
}

/// Profile loading and validation errors.
#[derive(Debug, Error)]
pub enum MetadataError {
    /// Profile could not be read.
    #[error("could not read lint profile: {0}")]
    Io(#[from] std::io::Error),
    /// Profile is not valid JSON.
    #[error("invalid lint profile JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// Unsupported profile schema.
    #[error("unsupported lint profile schema version {0}; expected 1")]
    UnsupportedVersion(u32),
}
#[cfg(test)]
/// Exercises profile deserialization and validation for physical connectivity, group geometry, component intent, and decoupling contracts.
mod tests {
    #![allow(clippy::expect_used)]

    use super::{ComponentKind, ComponentRole, LintProfile, ProfileMode, SchematicFacts};
    use crate::core::Severity;
    use crate::erc::{Netlist, PinRef};
    use std::collections::BTreeMap;

    /// Builds a minimal schematic fixture with two placed one-pin symbols and appends the supplied serialized wire and label expressions.
    ///
    /// # Arguments
    ///
    /// * `wire_and_label_items` — `KiCad` S-expression fragments inserted into the fixture to define wires, junctions, and labels.
    ///
    /// # Returns
    ///
    /// A parsed schematic document containing U1.1, R1.1, and the supplied connectivity geometry.
    fn direct_wire_schematic(wire_and_label_items: &str) -> crate::parser::SchematicDocument {
        crate::parser::SchematicDocument::from_text(
            "direct-wire-test.kicad_sch",
            format!(
                r#"(kicad_sch
                    (lib_symbols
                      (symbol "Test:OnePin"
                        (symbol "OnePin_1_1"
                          (pin passive line (at 0 0 0) (length 1.27)
                            (name "1") (number "1"))
                        )
                      )
                    )
                    (symbol (lib_id "Test:OnePin") (at 10 10 0) (unit 1)
                      (property "Reference" "U1" (at 10 10 0) (effects (font (size 1 1))))
                    )
                    (symbol (lib_id "Test:OnePin") (at 20 10 0) (unit 1)
                      (property "Reference" "R1" (at 20 10 0) (effects (font (size 1 1))))
                    )
                    {wire_and_label_items}
                )"#
            ),
        )
        .expect("fixture parses")
    }
    /// Regression test: verifies that physical wire group accepts connected pins without labels.
    #[test]
    fn physical_wire_group_accepts_connected_pins_without_labels() {
        let schematic = direct_wire_schematic("(wire (pts (xy 10 10) (xy 20 10)))");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"physical_wire_groups":[{"name":"SPI_MISO","pins":["U1.1","R1.1"]}]}"#,
        )
        .expect("profile parses");
        assert!(
            !profile
                .validate(&schematic)
                .iter()
                .any(|item| item.code == "SCH004")
        );
    }
    /// Regression test: verifies that physical wire group rejects disconnected islands even when labels match.
    #[test]
    fn physical_wire_group_rejects_disconnected_islands_even_when_labels_match() {
        let schematic = direct_wire_schematic(
            r#"
            (wire (pts (xy 10 10) (xy 12 10)))
            (wire (pts (xy 20 10) (xy 18 10)))
            (label "SPI_MISO" (at 10 10 0) (effects (font (size 1 1))))
            (label "SPI_MISO" (at 20 10 0) (effects (font (size 1 1))))
            "#,
        );
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"physical_wire_groups":[{"name":"SPI_MISO","pins":["U1.1","R1.1"]}]}"#,
        )
        .expect("profile parses");
        assert!(profile.validate(&schematic).iter().any(|item| {
            item.code == "SCH004" && item.message.contains("matching labels do not establish")
        }));
    }
    /// Regression test: verifies that physical wire group rejects invalid missing and unwired pins.
    #[test]
    fn physical_wire_group_rejects_invalid_missing_and_unwired_pins() {
        let schematic = direct_wire_schematic("(wire (pts (xy 10 10) (xy 15 10)))");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"physical_wire_groups":[{"name":"INVALID","pins":["U1.1","bad"]},{"name":"MISSING","pins":["U1.99","R1.1"]},{"name":"UNWIRED","pins":["U1.1","R1.1"]}]}"#,
        )
        .expect("profile parses");
        let messages: Vec<_> = profile
            .validate(&schematic)
            .into_iter()
            .filter(|item| item.code == "SCH004")
            .map(|item| item.message)
            .collect();
        assert!(
            messages
                .iter()
                .any(|message| message.contains("invalid pin reference"))
        );
        assert!(
            messages
                .iter()
                .any(|message| message.contains("missing or unsupported pin U1.99"))
        );
        assert!(
            messages
                .iter()
                .any(|message| message.contains("R1.1") && message.contains("not attached"))
        );
    }
    /// Regression test: verifies that physical wire group rejects duplicate pins and empty names.
    #[test]
    fn physical_wire_group_rejects_duplicate_pins_and_empty_names() {
        let schematic = direct_wire_schematic("(wire (pts (xy 10 10) (xy 20 10)))");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"physical_wire_groups":[{"name":"DUPLICATE","pins":["U1.1","U1.1"]},{"name":" ","pins":["U1.1","R1.1"]}]}"#,
        )
        .expect("profile parses");
        let messages: Vec<_> = profile
            .validate(&schematic)
            .into_iter()
            .filter(|item| item.code == "SCH004")
            .map(|item| item.message)
            .collect();
        assert!(
            messages
                .iter()
                .any(|message| message.contains("at least two distinct"))
        );
        assert!(
            messages
                .iter()
                .any(|message| message.contains("non-empty name"))
        );
    }
    /// Regression test: verifies that common pin group accepts one named label on a shared wire network.
    #[test]
    fn common_pin_group_accepts_one_named_label_on_a_shared_wire_network() {
        let schematic = direct_wire_schematic(
            r#"
            (wire (pts (xy 10 10) (xy 20 10)))
            (label "SENSE" (at 10 10 0) (effects (font (size 1 1))))
            "#,
        );
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"common_pin_groups":[{"name":"SENSE","pins":["U1.1","R1.1"],"wire_connected":true,"single_label":true}]}"#,
        )
        .expect("profile parses");
        assert!(
            !profile
                .validate(&schematic)
                .iter()
                .any(|item| item.code == "SCH004")
        );
    }
    /// Regression test: verifies that common pin group rejects disconnected label joined islands.
    #[test]
    fn common_pin_group_rejects_disconnected_label_joined_islands() {
        let schematic = direct_wire_schematic(
            r#"
            (wire (pts (xy 10 10) (xy 12 10)))
            (wire (pts (xy 20 10) (xy 18 10)))
            (label "SENSE" (at 10 10 0) (effects (font (size 1 1))))
            (label "SENSE" (at 20 10 0) (effects (font (size 1 1))))
            "#,
        );
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"common_pin_groups":[{"name":"SENSE","pins":["U1.1","R1.1"],"wire_connected":true}]}"#,
        )
        .expect("profile parses");
        assert!(
            profile
                .validate(&schematic)
                .iter()
                .any(|item| item.code == "SCH004"
                    && item.message.contains("must share one physical"))
        );
    }
    /// Regression test: verifies that common pin group allows label joined islands when wire connection is not required.
    #[test]
    fn common_pin_group_allows_label_joined_islands_when_wire_connection_is_not_required() {
        let schematic = direct_wire_schematic(
            r#"
            (wire (pts (xy 10 10) (xy 12 10)))
            (wire (pts (xy 20 10) (xy 18 10)))
            (label "SENSE" (at 10 10 0) (effects (font (size 1 1))))
            (label "SENSE" (at 20 10 0) (effects (font (size 1 1))))
            "#,
        );
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"common_pin_groups":[{"name":"SENSE","pins":["U1.1","R1.1"]}]}"#,
        )
        .expect("profile parses");
        assert!(
            !profile
                .validate(&schematic)
                .iter()
                .any(|item| item.code == "SCH004")
        );
    }
    /// Regression test: verifies that common pin group rejects duplicate and unresolvable pin references.
    #[test]
    fn common_pin_group_rejects_duplicate_and_unresolvable_pin_references() {
        let schematic = direct_wire_schematic(
            r#"(wire (pts (xy 10 10) (xy 20 10)))
               (label "VBUS" (at 20 10 0) (effects (font (size 1 1))))"#,
        );
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"common_pin_groups":[{"name":"SENSE","pins":["U1.1","U1.1"]},{"name":"VBUS","pins":["U1.99","R1.1"]}]}"#,
        )
        .expect("profile parses");
        let diagnostics: Vec<_> = profile
            .validate(&schematic)
            .into_iter()
            .filter(|item| item.code == "SCH004")
            .collect();
        assert_eq!(diagnostics.len(), 2);
        assert!(
            diagnostics
                .iter()
                .any(|item| item.message.contains("two distinct REF.PIN entries"))
        );
        assert!(
            diagnostics
                .iter()
                .any(|item| item.message.contains("missing or unsupported pin U1.99"))
        );
    }
    /// Regression test: verifies that direct wire pair does not connect at an unmarked crossing.
    #[test]
    fn direct_wire_pair_does_not_connect_at_an_unmarked_crossing() {
        let schematic = direct_wire_schematic(
            r"
            (wire (pts (xy 10 10) (xy 16 10)))
            (wire (pts (xy 20 10) (xy 20 16)))
            (wire (pts (xy 20 16) (xy 14 16)))
            (wire (pts (xy 14 16) (xy 14 4)))
            ",
        );
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"direct_wire_pairs":[{"first":"U1.1","second":"R1.1"}]}"#,
        )
        .expect("profile parses");
        assert!(profile.validate(&schematic).iter().any(|item| {
            item.code == "SCH004"
                && item
                    .message
                    .contains("must share one physical wire-only network")
        }));
    }
    /// Regression test: verifies that direct wire pair connects at a crossing with an explicit junction.
    #[test]
    fn direct_wire_pair_connects_at_a_crossing_with_an_explicit_junction() {
        let schematic = direct_wire_schematic(
            r"
            (wire (pts (xy 10 10) (xy 16 10)))
            (wire (pts (xy 20 10) (xy 20 16)))
            (wire (pts (xy 20 16) (xy 14 16)))
            (wire (pts (xy 14 16) (xy 14 4)))
            (junction (at 14 10) (diameter 0) (color 0 0 0 0))
            ",
        );
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"direct_wire_pairs":[{"first":"U1.1","second":"R1.1"}]}"#,
        )
        .expect("profile parses");
        assert!(
            !profile
                .validate(&schematic)
                .iter()
                .any(|item| item.code == "SCH004")
        );
    }
    /// Builds a schematic fixture in which a branch wire terminates on another wire, optionally adding an explicit junction marker at the shared coordinate.
    ///
    /// # Arguments
    ///
    /// * `explicit_junction` — Whether to serialize a junction marker at the T-connection; `false` models an unmarked endpoint-on-segment contact.
    ///
    /// # Returns
    ///
    /// A parsed schematic document with two one-pin symbols and the requested T-junction topology.
    fn direct_wire_t_junction_schematic(
        explicit_junction: bool,
    ) -> crate::parser::SchematicDocument {
        let junction = if explicit_junction {
            r"(junction (at 14 10) (diameter 0) (color 0 0 0 0))"
        } else {
            ""
        };
        crate::parser::SchematicDocument::from_text(
            "direct-wire-t-junction-test.kicad_sch",
            format!(
                r#"(kicad_sch
                    (lib_symbols
                      (symbol "Test:OnePin"
                        (symbol "OnePin_1_1"
                          (pin passive line (at 0 0 0) (length 1.27)
                            (name "1") (number "1"))
                        )
                      )
                    )
                    (symbol (lib_id "Test:OnePin") (at 10 10 0) (unit 1)
                      (property "Reference" "U1" (at 10 10 0) (effects (font (size 1 1))))
                    )
                    (symbol (lib_id "Test:OnePin") (at 14 5 0) (unit 1)
                      (property "Reference" "R1" (at 14 5 0) (effects (font (size 1 1))))
                    )
                    (wire (pts (xy 10 10) (xy 20 10)))
                    (wire (pts (xy 14 10) (xy 14 5)))
                    {junction}
                )"#
            ),
        )
        .expect("T-junction schematic parses")
    }
    /// Regression test: verifies that direct wire pair does not connect through an unmarked t junction.
    #[test]
    fn direct_wire_pair_does_not_connect_through_an_unmarked_t_junction() {
        let schematic = direct_wire_t_junction_schematic(false);
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"direct_wire_pairs":[{"first":"U1.1","second":"R1.1"}]}"#,
        )
        .expect("profile parses");
        assert!(profile.validate(&schematic).iter().any(|item| {
            item.code == "SCH004"
                && item
                    .message
                    .contains("must share one physical wire-only network")
        }));
    }
    /// Regression test: verifies that direct wire pair connects through a marked t junction.
    #[test]
    fn direct_wire_pair_connects_through_a_marked_t_junction() {
        let schematic = direct_wire_t_junction_schematic(true);
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"direct_wire_pairs":[{"first":"U1.1","second":"R1.1"}]}"#,
        )
        .expect("profile parses");
        assert!(
            !profile
                .validate(&schematic)
                .iter()
                .any(|item| item.code == "SCH004")
        );
    }
    /// Regression test: verifies that common pin group enforces exactly one matching label.
    #[test]
    fn common_pin_group_enforces_exactly_one_matching_label() {
        let schematic = direct_wire_schematic(
            r#"
            (wire (pts (xy 10 10) (xy 20 10)))
            (label "SENSE" (at 10 10 0) (effects (font (size 1 1))))
            (label "SENSE" (at 20 10 0) (effects (font (size 1 1))))
            "#,
        );
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"common_pin_groups":[{"name":"SENSE","pins":["U1.1","R1.1"],"wire_connected":true,"single_label":true}]}"#,
        )
        .expect("profile parses");
        assert!(
            profile
                .validate(&schematic)
                .iter()
                .any(|item| item.code == "SCH004" && item.message.contains("share exactly one"))
        );
    }
    /// Creates a flattened-netlist fixture assigning U1.1 and R1.1 to the supplied logical nets.
    ///
    /// # Arguments
    ///
    /// * `first_net` — Optional exported net name for U1.1; `None` leaves that pin absent from the reverse index.
    /// * `second_net` — Optional exported net name for R1.1; `None` leaves that pin absent from the reverse index.
    ///
    /// # Returns
    ///
    /// A `Netlist` whose `pin_nets` index contains only the assignments supplied by the caller.
    fn common_pin_netlist(first_net: Option<&str>, second_net: Option<&str>) -> Netlist {
        let mut pin_nets = BTreeMap::new();
        if let Some(net) = first_net {
            pin_nets.insert(
                PinRef {
                    reference: "U1".to_owned(),
                    pin: "1".to_owned(),
                },
                net.to_owned(),
            );
        }
        if let Some(net) = second_net {
            pin_nets.insert(
                PinRef {
                    reference: "R1".to_owned(),
                    pin: "1".to_owned(),
                },
                net.to_owned(),
            );
        }
        Netlist {
            pin_nets,
            ..Netlist::default()
        }
    }
    /// Regression test: verifies that common pin group can require one exported `KiCad` net.
    #[test]
    fn common_pin_group_can_require_one_exported_kicad_net() {
        let schematic = direct_wire_schematic(
            r#"
            (wire (pts (xy 10 10) (xy 20 10)))
            (label "SENSE" (at 10 10 0) (effects (font (size 1 1))))
            "#,
        );
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"common_pin_groups":[{"name":"SENSE","pins":["U1.1","R1.1"],"netlist_connected":true}]}"#,
        )
        .expect("profile parses");
        let diagnostics = profile.validate_with_netlist(
            &schematic,
            Some(&common_pin_netlist(Some("SENSE"), Some("SENSE"))),
        );
        assert!(!diagnostics.iter().any(|item| {
            item.code == "SCH014" && item.message.contains("different exported KiCad nets")
        }));
    }
    /// Regression test: verifies that common pin group rejects different exported `KiCad` nets.
    #[test]
    fn common_pin_group_rejects_different_exported_kicad_nets() {
        let schematic = direct_wire_schematic(
            r#"
            (wire (pts (xy 10 10) (xy 20 10)))
            (label "SENSE" (at 10 10 0) (effects (font (size 1 1))))
            "#,
        );
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"common_pin_groups":[{"name":"SENSE","pins":["U1.1","R1.1"],"netlist_connected":true}]}"#,
        )
        .expect("profile parses");
        let diagnostics = profile.validate_with_netlist(
            &schematic,
            Some(&common_pin_netlist(Some("SENSE"), Some("OTHER"))),
        );
        assert!(diagnostics.iter().any(|item| {
            item.code == "SCH014" && item.message.contains("different exported KiCad nets")
        }));
    }
    /// Regression test: verifies that common pin group reports pins missing from exported netlist.
    #[test]
    fn common_pin_group_reports_pins_missing_from_exported_netlist() {
        let schematic = direct_wire_schematic(
            r#"
            (wire (pts (xy 10 10) (xy 20 10)))
            (label "SENSE" (at 10 10 0) (effects (font (size 1 1))))
            "#,
        );
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"common_pin_groups":[{"name":"SENSE","pins":["U1.1","R1.1"],"netlist_connected":true}]}"#,
        )
        .expect("profile parses");
        let diagnostics = profile
            .validate_with_netlist(&schematic, Some(&common_pin_netlist(Some("SENSE"), None)));
        assert!(diagnostics.iter().any(|item| item.message.contains("R1.1")
            && item.message.contains("absent from the exported")));
    }
    /// Regression test: verifies that common pin group warns when requested netlist is not supplied.
    #[test]
    fn common_pin_group_warns_when_requested_netlist_is_not_supplied() {
        let schematic = direct_wire_schematic(
            r#"
            (wire (pts (xy 10 10) (xy 20 10)))
            (label "SENSE" (at 10 10 0) (effects (font (size 1 1))))
            "#,
        );
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"common_pin_groups":[{"name":"SENSE","pins":["U1.1","R1.1"],"netlist_connected":true}]}"#,
        )
        .expect("profile parses");
        let diagnostics = profile.validate(&schematic);
        assert!(diagnostics.iter().any(|item| {
            item.code == "SCH014"
                && item.severity == Severity::Warning
                && item
                    .message
                    .contains("no exported KiCad netlist was supplied")
        }));
    }
    /// Regression test: verifies that common pin group requires a label on each physical pin network.
    #[test]
    fn common_pin_group_requires_a_label_on_each_physical_pin_network() {
        let schematic = direct_wire_schematic(r"(wire (pts (xy 10 10) (xy 20 10)))");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"common_pin_groups":[{"name":"SENSE","pins":["U1.1","R1.1"]}]}"#,
        )
        .expect("profile parses");
        let diagnostics = profile.validate(&schematic);
        assert_eq!(
            diagnostics
                .iter()
                .filter(|item| item.code == "SCH004")
                .count(),
            2
        );
        assert!(diagnostics.iter().any(|item| {
            item.message
                .contains("U1.1 must belong to common net SENSE")
        }));
        assert!(diagnostics.iter().any(|item| {
            item.message
                .contains("R1.1 must belong to common net SENSE")
        }));
    }
    /// Regression test: verifies that direct wire pair passes when pins share a continuous wire.
    #[test]
    fn direct_wire_pair_passes_when_pins_share_a_continuous_wire() {
        let schematic = direct_wire_schematic(r"(wire (pts (xy 10 10) (xy 20 10)))");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"direct_wire_pairs":[{"first":"U1.1","second":"R1.1"}]}"#,
        )
        .expect("profile parses");
        assert!(
            !profile
                .validate(&schematic)
                .iter()
                .any(|item| item.code == "SCH004")
        );
    }
    /// Regression test: verifies that direct wire pair rejects label joined islands.
    #[test]
    fn direct_wire_pair_rejects_label_joined_islands() {
        let schematic = direct_wire_schematic(
            r#"
            (wire (pts (xy 10 10) (xy 12 10)))
            (wire (pts (xy 20 10) (xy 18 10)))
            (label "SENSE" (at 10 10 0) (effects (font (size 1 1))))
            (label "SENSE" (at 20 10 0) (effects (font (size 1 1))))
            "#,
        );
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"direct_wire_pairs":[{"first":"U1.1","second":"R1.1"}]}"#,
        )
        .expect("profile parses");
        assert!(profile.validate(&schematic).iter().any(|item| {
            item.code == "SCH004"
                && item
                    .message
                    .contains("must share one physical wire-only network")
        }));
    }
    /// Regression test: verifies that direct wire pair rejects label anchors and forbidden local labels.
    #[test]
    fn direct_wire_pair_rejects_label_anchors_and_forbidden_local_labels() {
        let schematic = direct_wire_schematic(
            r#"
            (wire (pts (xy 10 10) (xy 20 10)))
            (label "FET_MID" (at 10 10 0) (effects (font (size 1 1))))
            (label "FET_MID" (at 15 10 0) (effects (font (size 1 1))))
            "#,
        );
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"direct_wire_pairs":[{"first":"U1.1","second":"R1.1"}],"forbidden_direct_wire_labels":["FET_MID"]}"#,
        )
        .expect("profile parses");
        let diagnostics = profile.validate(&schematic);
        assert!(
            diagnostics.iter().any(
                |item| item.code == "SCH004" && item.message.contains("shares its anchor with")
            )
        );
        assert!(diagnostics.iter().any(|item| {
            item.code == "SCH004"
                && item
                    .message
                    .contains("forbidden on a direct-wire-only network")
        }));
    }
    /// Regression test: verifies that forbidden direct wire label is ignored on unrelated wire islands.
    #[test]
    fn forbidden_direct_wire_label_is_ignored_on_unrelated_wire_islands() {
        let schematic = direct_wire_schematic(
            r#"
            (wire (pts (xy 10 10) (xy 20 10)))
            (wire (pts (xy 1 1) (xy 2 1)))
            (label "FET_MID" (at 1 1 0) (effects (font (size 1 1))))
            "#,
        );
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"direct_wire_pairs":[{"first":"U1.1","second":"R1.1"}],"forbidden_direct_wire_labels":["FET_MID"]}"#,
        )
        .expect("profile parses");
        assert!(
            !profile
                .validate(&schematic)
                .iter()
                .any(|item| item.code == "SCH004")
        );
    }
    /// Regression test: verifies that direct wire pair requires both pin anchors to touch wire segments.
    #[test]
    fn direct_wire_pair_requires_both_pin_anchors_to_touch_wire_segments() {
        let schematic = direct_wire_schematic("");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"direct_wire_pairs":[{"first":"U1.1","second":"R1.1"}]}"#,
        )
        .expect("profile parses");
        let diagnostics = profile.validate(&schematic);
        assert_eq!(
            diagnostics
                .iter()
                .filter(|item| item.code == "SCH004")
                .count(),
            2
        );
        assert!(
            diagnostics
                .iter()
                .any(|item| item.message.contains("U1.1 is not attached"))
        );
        assert!(
            diagnostics
                .iter()
                .any(|item| item.message.contains("R1.1 is not attached"))
        );
    }
    /// Regression test: verifies that resistor roles require their named connection contract.
    #[test]
    fn resistor_roles_require_their_named_connection_contract() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "profile-test.kicad_sch",
            r#"(kicad_sch (symbol (lib_id "Device:R") (property "Reference" "R1" (at 1 1 0))))"#,
        )
        .expect("fixture parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"components":{"R1":{"kind":"resistor","role":"pull_up","function":"Bias SDA high","rationale":"Required for open-drain I2C","connections":{"signal":"SDA","rail":"3V3"}}}}"#,
        )
        .expect("profile parses");
        assert!(
            !profile
                .validate(&schematic)
                .iter()
                .any(|item| item.code == "SCH021")
        );
    }
    /// Regression test: verifies that incomplete resistor role contract is reported.
    #[test]
    fn incomplete_resistor_role_contract_is_reported() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "profile-test.kicad_sch",
            r#"(kicad_sch (symbol (lib_id "Device:R") (property "Reference" "R1" (at 1 1 0))))"#,
        )
        .expect("fixture parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"components":{"R1":{"kind":"resistor","role":"pull_up","function":"Bias SDA high","rationale":"Required for open-drain I2C"}}}"#,
        )
        .expect("profile parses");
        assert!(
            profile
                .validate(&schematic)
                .iter()
                .any(|item| item.code == "SCH021" && item.message.contains("signal"))
        );
    }
    /// Regression test: verifies that group member origins must be inside the declared schematic frame.
    #[test]
    fn group_member_origins_must_be_inside_the_declared_schematic_frame() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "group-frame-test.kicad_sch",
            r#"(kicad_sch
                (symbol (lib_id "Device:R") (uuid "r1-uuid") (at 4 5 0)
                    (property "Reference" "R1" (at 4 6 0)))
                (rectangle (start 1 1) (end 8 9) (uuid "frame-1"))
                (text_box "Power input group" (uuid "description-1"))
            )"#,
        )
        .expect("fixture parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"power-input":{"members":["R1"],"description_uuid":"description-1","frame_uuid":"frame-1","evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");
        assert!(
            !profile
                .validate(&schematic)
                .iter()
                .any(|item| item.code == "SCH009")
        );
    }
    /// Regression test: verifies that group member origin outside the declared schematic frame is reported.
    #[test]
    fn group_member_origin_outside_the_declared_schematic_frame_is_reported() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "group-frame-test.kicad_sch",
            r#"(kicad_sch
                (symbol (lib_id "Device:R") (uuid "r1-uuid") (at 9 5 0)
                    (property "Reference" "R1" (at 9 6 0)))
                (rectangle (start 1 1) (end 8 9) (uuid "frame-1"))
                (text_box "Power input group" (uuid "description-1"))
            )"#,
        )
        .expect("fixture parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"power-input":{"members":["R1"],"description_uuid":"description-1","frame_uuid":"frame-1","evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");
        assert!(
            profile
                .validate(&schematic)
                .iter()
                .any(|item| item.code == "SCH009" && item.message.contains("outside frame"))
        );
    }
    /// Regression test: verifies that group pin outside frame is reported even when symbol origin is inside.
    #[test]
    fn group_pin_outside_frame_is_reported_even_when_symbol_origin_is_inside() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "group-frame-pin-test.kicad_sch",
            r#"(kicad_sch
                (lib_symbols
                  (symbol "Test:OnePin"
                    (symbol "OnePin_1_1"
                      (pin passive line (at 2.5 0 180) (length 1)
                        (name "1") (number "1"))
                    )
                  )
                )
                (symbol (lib_id "Test:OnePin") (at 4 5 0) (unit 1)
                  (property "Reference" "R1" (at 4 4 0) (effects (font (size 1 1))))
                )
                (rectangle (start 1 1) (end 6 9) (uuid "frame-1"))
                (text_box "Power input group" (uuid "description-1"))
            )"#,
        )
        .expect("fixture parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"power-input":{"members":["R1"],"description_uuid":"description-1","frame_uuid":"frame-1","evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");
        assert!(
            profile
                .validate(&schematic)
                .iter()
                .any(|item| { item.code == "SCH009" && item.message.contains("member pin R1.1") })
        );
    }
    /// Regression test: verifies that group visible field bounds must be inside the frame.
    #[test]
    fn group_visible_field_bounds_must_be_inside_the_frame() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "group-frame-field-test.kicad_sch",
            r#"(kicad_sch
                (symbol (lib_id "Device:R") (at 4 5 0)
                  (property "Reference" "R1" (at 4 4 0)
                    (effects (font (size 1 1))))
                  (property "Value" "10k" (at 9 5 0)
                    (effects (font (size 1 1))))
                )
                (rectangle (start 1 1) (end 8 9) (uuid "frame-1"))
                (text_box "Power input group" (uuid "description-1"))
            )"#,
        )
        .expect("fixture parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"power-input":{"members":["R1"],"description_uuid":"description-1","frame_uuid":"frame-1","evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");
        assert!(profile.validate(&schematic).iter().any(|item| {
            item.code == "SCH009" && item.message.contains("member field R1 Value")
        }));
    }
    /// Regression test: verifies that group description text box must be inside the frame.
    #[test]
    fn group_description_text_box_must_be_inside_the_frame() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "group-frame-description-test.kicad_sch",
            r#"(kicad_sch
                (symbol (lib_id "Device:R") (at 4 5 0)
                  (property "Reference" "R1" (at 4 4 0)
                    (effects (font (size 1 1))))
                )
                (rectangle (start 1 1) (end 8 9) (uuid "frame-1"))
                (text_box "Power input group" (at 7.5 5) (size 2 1)
                  (margins 0.5 0.5 0.5 0.5)
                  (stroke (width 0.1)) (uuid "description-1")
                  (effects (font (size 1 1))))
            )"#,
        )
        .expect("fixture parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"power-input":{"members":["R1"],"description_uuid":"description-1","frame_uuid":"frame-1","evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");
        assert!(profile.validate(&schematic).iter().any(|item| {
            item.code == "SCH009"
                && item.message.contains("description text box")
                && item.message.contains("extends outside frame")
        }));
    }
    /// Regression test: verifies that missing group frame uuid is reported.
    #[test]
    fn missing_group_frame_uuid_is_reported() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "group-frame-test.kicad_sch",
            r#"(kicad_sch
                (symbol (lib_id "Device:R") (uuid "r1-uuid") (at 4 5 0)
                    (property "Reference" "R1" (at 4 6 0)))
                (text_box "Power input group" (uuid "description-1"))
            )"#,
        )
        .expect("fixture parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"power-input":{"members":["R1"],"description_uuid":"description-1","frame_uuid":"missing-frame","evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");
        assert!(
            profile
                .validate(&schematic)
                .iter()
                .any(|item| item.code == "SCH009"
                    && item.message.contains("missing schematic frame UUID"))
        );
    }
    /// Regression test: verifies that group frame clearance accepts member and description inside margin.
    #[test]
    fn group_frame_clearance_accepts_member_and_description_inside_margin() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "group-frame-clearance-test.kicad_sch",
            r#"(kicad_sch
                (symbol (lib_id "Device:R") (uuid "r1-uuid") (at 3 3 0)
                    (property "Reference" "R1" (at 3 4 0)))
                (rectangle (start 0 0) (end 10 10) (uuid "frame-1"))
                (text_box "Power input group" (at 3 5) (size 3 1)
                    (margins 0.5 0.5 0.5 0.5)
                    (stroke (width 0.1)) (uuid "description-1"))
            )"#,
        )
        .expect("fixture parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"power-input":{"members":["R1"],"description_uuid":"description-1","frame_uuid":"frame-1","minimum_frame_clearance_mm":1.0,"evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");
        assert!(
            !profile
                .validate(&schematic)
                .iter()
                .any(|item| item.code == "SCH012")
        );
    }
    /// Regression test: verifies that group frame clearance reports symbol origin near frame edge.
    #[test]
    fn group_frame_clearance_reports_symbol_origin_near_frame_edge() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "group-frame-clearance-test.kicad_sch",
            r#"(kicad_sch
                (symbol (lib_id "Device:R") (uuid "r1-uuid") (at 0.5 3 0)
                    (property "Reference" "R1" (at 0.5 4 0)))
                (rectangle (start 0 0) (end 10 10) (uuid "frame-1"))
                (text_box "Power input group" (at 3 5) (size 3 1)
                    (margins 0.5 0.5 0.5 0.5)
                    (stroke (width 0.1)) (uuid "description-1"))
            )"#,
        )
        .expect("fixture parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"power-input":{"members":["R1"],"description_uuid":"description-1","frame_uuid":"frame-1","minimum_frame_clearance_mm":1.0,"evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");
        assert!(
            profile
                .validate(&schematic)
                .iter()
                .any(|item| item.code == "SCH012" && item.message.contains("origin is within"))
        );
    }
    /// Regression test: verifies that group frame clearance checks transformed member pin coordinates.
    #[test]
    fn group_frame_clearance_checks_transformed_member_pin_coordinates() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "group-pin-frame-clearance-test.kicad_sch",
            r#"(kicad_sch
                (lib_symbols
                  (symbol "Test:OnePin"
                    (symbol "OnePin_1_1"
                      (pin passive line (at -2.5 0 180) (length 1)
                        (name "1") (number "1"))
                    )
                  )
                )
                (symbol (lib_id "Test:OnePin") (at 3 3 0) (unit 1)
                  (property "Reference" "R1" (at 3 4 0) (effects (font (size 1 1))))
                )
                (rectangle (start 0 0) (end 10 10) (uuid "frame-1"))
                (text_box "Power input group" (at 3 5) (size 3 1)
                    (margins 0.5 0.5 0.5 0.5)
                    (stroke (width 0.1)) (uuid "description-1"))
            )"#,
        )
        .expect("fixture parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"power-input":{"members":["R1"],"description_uuid":"description-1","frame_uuid":"frame-1","minimum_frame_clearance_mm":1.0,"evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");
        assert!(profile.validate(&schematic).iter().any(|item| {
            item.code == "SCH012"
                && item
                    .message
                    .contains("member pin R1.1 does not maintain 1.00 mm clearance")
        }));
    }
    /// Regression test: verifies that group frame clearance accepts member pins inside the configured inset.
    #[test]
    fn group_frame_clearance_accepts_member_pins_inside_the_configured_inset() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "group-pin-frame-clearance-test.kicad_sch",
            r#"(kicad_sch
                (lib_symbols
                  (symbol "Test:OnePin"
                    (symbol "OnePin_1_1"
                      (pin passive line (at 0 0 180) (length 1)
                        (name "1") (number "1"))
                    )
                  )
                )
                (symbol (lib_id "Test:OnePin") (at 3 3 0) (unit 1)
                  (property "Reference" "R1" (at 3 4 0) (effects (font (size 1 1))))
                )
                (rectangle (start 0 0) (end 10 10) (uuid "frame-1"))
                (text_box "Power input group" (at 3 5) (size 3 1)
                    (margins 0.5 0.5 0.5 0.5)
                    (stroke (width 0.1)) (uuid "description-1"))
            )"#,
        )
        .expect("fixture parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"power-input":{"members":["R1"],"description_uuid":"description-1","frame_uuid":"frame-1","minimum_frame_clearance_mm":1.0,"evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");
        assert!(
            !profile
                .validate(&schematic)
                .iter()
                .any(|item| item.code == "SCH012")
        );
    }
    /// Regression test: verifies that group frame clearance checks visible member field bounds.
    #[test]
    fn group_frame_clearance_checks_visible_member_field_bounds() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "group-field-frame-clearance-test.kicad_sch",
            r#"(kicad_sch
                (symbol (lib_id "Device:R") (at 3 3 0)
                  (property "Reference" "R1" (at 3 4 0) (effects (font (size 1 1))))
                  (property "Value" "10k" (at 0.5 3 0) (effects (font (size 1 1))))
                )
                (rectangle (start 0 0) (end 10 10) (uuid "frame-1"))
                (text_box "Power input group" (at 3 5) (size 3 1)
                    (margins 0.5 0.5 0.5 0.5)
                    (stroke (width 0.1)) (uuid "description-1"))
            )"#,
        )
        .expect("fixture parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"power-input":{"members":["R1"],"description_uuid":"description-1","frame_uuid":"frame-1","minimum_frame_clearance_mm":1.0,"evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");
        assert!(profile.validate(&schematic).iter().any(|item| {
            item.code == "SCH012"
                && item
                    .message
                    .contains("member field R1 Value does not maintain")
        }));
    }
    /// Regression test: verifies that group frame clearance accepts visible member fields inside the configured inset.
    #[test]
    fn group_frame_clearance_accepts_visible_member_fields_inside_the_configured_inset() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "group-field-frame-clearance-test.kicad_sch",
            r#"(kicad_sch
                (symbol (lib_id "Device:R") (at 3 3 0)
                  (property "Reference" "R1" (at 3 4 0) (effects (font (size 1 1))))
                  (property "Value" "10k" (at 4 5 0) (effects (font (size 1 1))))
                )
                (rectangle (start 0 0) (end 10 10) (uuid "frame-1"))
                (text_box "Power input group" (at 3 6) (size 3 1)
                    (margins 0.5 0.5 0.5 0.5)
                    (stroke (width 0.1)) (uuid "description-1"))
            )"#,
        )
        .expect("fixture parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"power-input":{"members":["R1"],"description_uuid":"description-1","frame_uuid":"frame-1","minimum_frame_clearance_mm":1.0,"evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");
        assert!(
            !profile
                .validate(&schematic)
                .iter()
                .any(|item| item.code == "SCH012")
        );
    }
    /// Regression test: verifies that group frame clearance reports description box near frame edge.
    #[test]
    fn group_frame_clearance_reports_description_box_near_frame_edge() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "group-frame-clearance-test.kicad_sch",
            r#"(kicad_sch
                (symbol (lib_id "Device:R") (uuid "r1-uuid") (at 3 3 0)
                    (property "Reference" "R1" (at 3 4 0)))
                (rectangle (start 0 0) (end 10 10) (uuid "frame-1"))
                (text_box "Power input group" (at 0.5 5) (size 3 1)
                    (margins 0.5 0.5 0.5 0.5)
                    (stroke (width 0.1)) (uuid "description-1"))
            )"#,
        )
        .expect("fixture parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"power-input":{"members":["R1"],"description_uuid":"description-1","frame_uuid":"frame-1","minimum_frame_clearance_mm":1.0,"evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");
        assert!(
            profile
                .validate(&schematic)
                .iter()
                .any(|item| item.code == "SCH012" && item.message.contains("description text box"))
        );
    }
    /// Regression test: verifies that group wire compactness uses only segments wholly inside its frame.
    #[test]
    fn group_wire_compactness_uses_only_segments_wholly_inside_its_frame() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "wire-compactness-test.kicad_sch",
            r#"(kicad_sch
                (symbol (lib_id "Device:R") (uuid "r1-uuid") (at 4 5 0)
                    (property "Reference" "R1" (at 4 6 0)))
                (rectangle (start 0 0) (end 10 10) (uuid "frame-1"))
                (wire (pts (xy 1 1) (xy 4 1)))
                (wire (pts (xy 4 1) (xy 4 5)))
                (wire (pts (xy -5 2) (xy 15 2)))
                (text_box "Power input group" (uuid "description-1"))
            )"#,
        )
        .expect("fixture parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"power-input":{"members":["R1"],"description_uuid":"description-1","frame_uuid":"frame-1","wire_length_budget_mm":10.0,"evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");
        let diagnostics = profile.validate(&schematic);
        assert!(
            !diagnostics.iter().any(|item| item.code == "SCH016"),
            "the wire with endpoints outside the frame must not contribute: {diagnostics:#?}"
        );
    }
    /// Regression test: verifies that group wire compactness warns when normalized budget is exceeded.
    #[test]
    fn group_wire_compactness_warns_when_normalized_budget_is_exceeded() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "wire-compactness-test.kicad_sch",
            r#"(kicad_sch
                (symbol (lib_id "Device:R") (uuid "r1-uuid") (at 4 5 0)
                    (property "Reference" "R1" (at 4 6 0)))
                (rectangle (start 0 0) (end 10 10) (uuid "frame-1"))
                (wire (pts (xy 1 1) (xy 4 1)))
                (wire (pts (xy 4 1) (xy 4 5)))
                (wire (pts (xy 1 6) (xy 6 6)))
                (text_box "Power input group" (uuid "description-1"))
            )"#,
        )
        .expect("fixture parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"power-input":{"members":["R1"],"description_uuid":"description-1","frame_uuid":"frame-1","wire_length_budget_mm":10.0,"evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");
        let diagnostics = profile.validate(&schematic);
        let compactness = diagnostics
            .iter()
            .find(|item| item.code == "SCH016")
            .expect("over-budget compactness warning");
        assert_eq!(compactness.severity, Severity::Warning);
        assert!(compactness.message.contains("score is 1.200"));
        assert!(compactness.message.contains("12.00/10.00 mm"));
    }
    /// Regression test: verifies that group wire compactness requires a frame and positive budget.
    #[test]
    fn group_wire_compactness_requires_a_frame_and_positive_budget() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "wire-compactness-test.kicad_sch",
            r#"(kicad_sch
                (symbol (lib_id "Device:R") (uuid "r1-uuid") (at 4 5 0)
                    (property "Reference" "R1" (at 4 6 0)))
                (text_box "Power input group" (uuid "description-1"))
            )"#,
        )
        .expect("fixture parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"missing-frame":{"members":["R1"],"description_uuid":"description-1","wire_length_budget_mm":10.0,"evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}},"invalid-budget":{"members":["R1"],"description_uuid":"description-1","frame_uuid":"not-present","wire_length_budget_mm":0.0,"evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");
        let diagnostics = profile.validate(&schematic);
        assert_eq!(
            diagnostics
                .iter()
                .filter(|item| item.code == "SCH016")
                .count(),
            2
        );
        assert!(
            diagnostics
                .iter()
                .any(|item| item.message.contains("finite positive"))
        );
        assert!(
            diagnostics
                .iter()
                .any(|item| item.message.contains("must set frame_uuid"))
        );
    }
    /// Regression test: verifies that group minimum member origin spacing is enforced.
    #[test]
    fn group_minimum_member_origin_spacing_is_enforced() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "group-spacing-test.kicad_sch",
            r#"(kicad_sch
                (symbol (lib_id "Device:R") (uuid "r1-uuid") (at 0 0 0)
                    (property "Reference" "R1" (at 0 1 0)))
                (symbol (lib_id "Device:C") (uuid "c1-uuid") (at 2 0 0)
                    (property "Reference" "C1" (at 2 1 0)))
                (text_box "Power input group" (uuid "description-1"))
            )"#,
        )
        .expect("fixture parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"power-input":{"members":["R1","C1"],"description_uuid":"description-1","minimum_member_origin_spacing_mm":4.0,"evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");
        let diagnostics = profile.validate(&schematic);
        assert!(diagnostics.iter().any(|item| {
            item.code == "SCH006"
                && item.message.contains("R1 and C1")
                && item.message.contains("2.00 mm")
        }));
    }
    /// Regression test: verifies that group member origin spacing at or above minimum is accepted.
    #[test]
    fn group_member_origin_spacing_at_or_above_minimum_is_accepted() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "group-spacing-test.kicad_sch",
            r#"(kicad_sch
                (symbol (lib_id "Device:R") (uuid "r1-uuid") (at 0 0 0)
                    (property "Reference" "R1" (at 0 1 0)))
                (symbol (lib_id "Device:C") (uuid "c1-uuid") (at 2 0 0)
                    (property "Reference" "C1" (at 2 1 0)))
                (text_box "Power input group" (uuid "description-1"))
            )"#,
        )
        .expect("fixture parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"power-input":{"members":["R1","C1"],"description_uuid":"description-1","minimum_member_origin_spacing_mm":2.0,"evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");
        assert!(
            !profile
                .validate(&schematic)
                .iter()
                .any(|item| item.code == "SCH006")
        );
    }
    /// Regression test: verifies that group minimum member origin spacing must be positive and finite.
    #[test]
    fn group_minimum_member_origin_spacing_must_be_positive_and_finite() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "group-spacing-test.kicad_sch",
            r#"(kicad_sch
                (symbol (lib_id "Device:R") (uuid "r1-uuid") (at 0 0 0)
                    (property "Reference" "R1" (at 0 1 0)))
                (text_box "Power input group" (uuid "description-1"))
            )"#,
        )
        .expect("fixture parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"power-input":{"members":["R1"],"description_uuid":"description-1","minimum_member_origin_spacing_mm":0.0,"evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");
        assert!(
            profile
                .validate(&schematic)
                .iter()
                .any(|item| item.code == "SCH006" && item.message.contains("finite positive"))
        );
    }
    /// Regression test: verifies that profile group can be bound to native `KiCad` group membership.
    #[test]
    fn profile_group_can_be_bound_to_native_kicad_group_membership() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "native-group-test.kicad_sch",
            r#"(kicad_sch
                (symbol (lib_id "Device:R") (uuid "r1-uuid")
                    (property "Reference" "R1" (at 1 1 0)))
                (group "power-input" (id "group-uuid") (members "r1-uuid"))
                (text_box "R1 input resistor" (uuid "description-1"))
            )"#,
        )
        .expect("fixture parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"power-input":{"members":["R1"],"documentation_members":["R1"],"description_uuid":"description-1","kicad_group_uuid":"group-uuid","component_purposes":{"R1":{"function":"Input resistor","rationale":"Limit connector surge current"}},"evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");
        assert!(
            !profile
                .validate(&schematic)
                .iter()
                .any(|item| item.code == "SCH007")
        );
    }
    /// Regression test: verifies that native `KiCad` group must include profile component members.
    #[test]
    fn native_kicad_group_must_include_profile_component_members() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "native-group-test.kicad_sch",
            r#"(kicad_sch
                (symbol (lib_id "Device:R") (uuid "r1-uuid")
                    (property "Reference" "R1" (at 1 1 0)))
                (group "power-input" (id "group-uuid") (members "other-uuid"))
                (text_box "R1 input resistor" (uuid "description-1"))
            )"#,
        )
        .expect("fixture parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"power-input":{"members":["R1"],"documentation_members":["R1"],"description_uuid":"description-1","kicad_group_uuid":"group-uuid","component_purposes":{"R1":{"function":"Input resistor","rationale":"Limit connector surge current"}},"evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");
        assert!(
            profile
                .validate(&schematic)
                .iter()
                .any(|item| item.code == "SCH007"
                    && item.message.contains("does not contain symbol UUID"))
        );
    }
    /// Regression test: verifies that missing native `KiCad` group uuid is reported.
    #[test]
    fn missing_native_kicad_group_uuid_is_reported() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "native-group-test.kicad_sch",
            r#"(kicad_sch
                (symbol (lib_id "Device:R") (uuid "r1-uuid")
                    (property "Reference" "R1" (at 1 1 0)))
                (text_box "R1 input resistor" (uuid "description-1"))
            )"#,
        )
        .expect("fixture parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"power-input":{"members":["R1"],"documentation_members":["R1"],"description_uuid":"description-1","kicad_group_uuid":"missing-group","component_purposes":{"R1":{"function":"Input resistor","rationale":"Limit connector surge current"}},"evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");
        assert!(
            profile
                .validate(&schematic)
                .iter()
                .any(|item| item.code == "SCH007"
                    && item.message.contains("missing KiCad group UUID"))
        );
    }
    /// Regression test: verifies that group evidence and component purpose must match rendered description.
    #[test]
    fn group_evidence_and_component_purpose_must_match_rendered_description() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "group-test.kicad_sch",
            r#"(kicad_sch (symbol (lib_id "Device:R") (property "Reference" "R1" (at 1 1 0))) (text_box "R1: Pull up resistor. Why: I2C uses open-drain signalling." (uuid "description-1")))"#,
        )
        .expect("fixture parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"i2c":{"members":["R1"],"documentation_members":["R1"],"description_uuid":"description-1","component_purposes":{"R1":{"function":"Pull up resistor","rationale":"I2C uses open-drain signalling"}},"evidence":{"document":"Controller datasheet","page":12,"figure":"Figure 3"}}}}"#,
        )
        .expect("profile parses");
        let diagnostics = profile.validate(&schematic);
        assert!(
            !diagnostics
                .iter()
                .any(|item| item.code == "SCH017" || item.code == "SCH020")
        );
    }
    /// Regression test: verifies that group without evidence or purpose phrases is rejected.
    #[test]
    fn group_without_evidence_or_purpose_phrases_is_rejected() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "group-test.kicad_sch",
            r#"(kicad_sch (symbol (lib_id "Device:R") (property "Reference" "R1" (at 1 1 0))) (text_box "A resistor." (uuid "description-1")))"#,
        )
        .expect("fixture parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"i2c":{"members":["R1"],"documentation_members":["R1"],"description_uuid":"description-1","component_purposes":{"R1":{"function":"Pull up resistor","rationale":"I2C uses open-drain signalling"}},"evidence":{}}}}"#,
        )
        .expect("profile parses");
        let diagnostics = profile.validate(&schematic);
        assert!(diagnostics.iter().any(|item| item.code == "SCH017"));
        assert!(diagnostics.iter().any(|item| item.code == "SCH020"));
    }
    /// Regression test: verifies that separate wire pair accepts disconnected islands even when labels match.
    #[test]
    fn separate_wire_pair_accepts_disconnected_islands_even_when_labels_match() {
        let schematic = direct_wire_schematic(
            r#"
            (wire (pts (xy 10 10) (xy 12 10)))
            (wire (pts (xy 20 10) (xy 18 10)))
            (label "SENSE" (at 10 10 0) (effects (font (size 1 1))))
            (label "SENSE" (at 20 10 0) (effects (font (size 1 1))))
            "#,
        );
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"separate_wire_pairs":[{"first":"U1.1","second":"R1.1"}]}"#,
        )
        .expect("profile parses");
        assert!(
            !profile
                .validate(&schematic)
                .iter()
                .any(|item| item.code == "SCH004")
        );
    }
    /// Regression test: verifies that separate wire pair rejects pins on the same physical wire network.
    #[test]
    fn separate_wire_pair_rejects_pins_on_the_same_physical_wire_network() {
        let schematic = direct_wire_schematic(r"(wire (pts (xy 10 10) (xy 20 10)))");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"separate_wire_pairs":[{"first":"U1.1","second":"R1.1"}]}"#,
        )
        .expect("profile parses");
        assert!(profile.validate(&schematic).iter().any(|item| {
            item.code == "SCH004"
                && item
                    .message
                    .contains("separate physical wire-only networks")
        }));
    }
    /// Regression test: verifies that separate wire pair can require distinct exported nets.
    #[test]
    fn separate_wire_pair_can_require_distinct_exported_nets() {
        let schematic = direct_wire_schematic(
            r"
            (wire (pts (xy 10 10) (xy 12 10)))
            (wire (pts (xy 20 10) (xy 18 10)))
            ",
        );
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"separate_wire_pairs":[{"first":"U1.1","second":"R1.1","netlist_separate":true}]}"#,
        )
        .expect("profile parses");
        let netlist = Netlist {
            pin_nets: BTreeMap::from([
                (
                    PinRef {
                        reference: "U1".to_owned(),
                        pin: "1".to_owned(),
                    },
                    "SHARED".to_owned(),
                ),
                (
                    PinRef {
                        reference: "R1".to_owned(),
                        pin: "1".to_owned(),
                    },
                    "SHARED".to_owned(),
                ),
            ]),
            ..Netlist::default()
        };
        assert!(
            profile
                .validate_with_netlist(&schematic, Some(&netlist))
                .iter()
                .any(|item| {
                    item.code == "SCH014" && item.message.contains("different exported KiCad nets")
                })
        );
    }
    /// Regression test: verifies that separate wire pair warns when requested netlist is unavailable.
    #[test]
    fn separate_wire_pair_warns_when_requested_netlist_is_unavailable() {
        let schematic = direct_wire_schematic(
            r"
            (wire (pts (xy 10 10) (xy 12 10)))
            (wire (pts (xy 20 10) (xy 18 10)))
            ",
        );
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"separate_wire_pairs":[{"first":"U1.1","second":"R1.1","netlist_separate":true}]}"#,
        )
        .expect("profile parses");
        assert!(profile.validate(&schematic).iter().any(|item| {
            item.code == "SCH014"
                && item.severity == Severity::Warning
                && item.message.contains("no exported KiCad netlist")
        }));
    }
    /// Regression test: verifies that group symbol body primitive must be inside frame.
    #[test]
    fn group_symbol_body_primitive_must_be_inside_frame() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "group-frame-body-test.kicad_sch",
            r#"(kicad_sch
                (lib_symbols
                  (symbol "Test:OnePin"
                    (symbol "OnePin_1_1"
                      (rectangle (start -3 -1) (end 3 1)
                        (stroke (width 0.254) (type default)) (fill (type none)))
                      (pin passive line (at 0 0 0) (length 1.27)
                        (name "1") (number "1"))
                    )
                  )
                )
                (symbol (lib_id "Test:OnePin") (at 4 5 0) (unit 1)
                  (property "Reference" "R1" (at 4 4 0) (effects (font (size 1 1))))
                )
                (rectangle (start 1 1) (end 6 9) (uuid "frame-1"))
                (text_box "Power input group" (uuid "description-1"))
            )"#,
        )
        .expect("schematic parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"power-input":{"members":["R1"],"description_uuid":"description-1","frame_uuid":"frame-1","evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");

        assert!(profile.validate(&schematic).iter().any(|item| {
            item.code == "SCH009"
                && item
                    .message
                    .contains("symbol primitive extends outside frame")
        }));
    }
    /// Regression test: verifies that group symbol body primitive must maintain frame clearance.
    #[test]
    fn group_symbol_body_primitive_must_maintain_frame_clearance() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "group-frame-body-clearance-test.kicad_sch",
            r#"(kicad_sch
                (lib_symbols
                  (symbol "Test:OnePin"
                    (symbol "OnePin_1_1"
                      (rectangle (start -2 -1) (end 2 1)
                        (stroke (width 0.254) (type default)) (fill (type none)))
                      (pin passive line (at 0 0 0) (length 1.27)
                        (name "1") (number "1"))
                    )
                  )
                )
                (symbol (lib_id "Test:OnePin") (at 4.5 5 0) (unit 1)
                  (property "Reference" "R1" (at 4.5 4 0) (effects (font (size 1 1))))
                )
                (rectangle (start 1 1) (end 8 9) (uuid "frame-1"))
                (text_box "Power input group" (at 4 4) (size 1 1) (uuid "description-1"))
            )"#,
        )
        .expect("schematic parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"power-input":{"members":["R1"],"description_uuid":"description-1","frame_uuid":"frame-1","minimum_frame_clearance_mm":2.0,"evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");

        assert!(profile.validate(&schematic).iter().any(|item| {
            item.code == "SCH012"
                && item
                    .message
                    .contains("symbol primitive does not maintain 2.00 mm clearance")
        }));
    }
    /// Regression test: verifies that group wire label and annotation geometry must be inside frame.
    #[test]
    fn group_wire_label_and_annotation_geometry_must_be_inside_frame() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "group-frame-wire-label-test.kicad_sch",
            r#"(kicad_sch
                (lib_symbols
                  (symbol "Test:OnePin"
                    (symbol "OnePin_1_1"
                      (pin passive line (at 0 0 0) (length 1.27)
                        (name "1") (number "1"))
                    )
                  )
                )
                (symbol (lib_id "Test:OnePin") (at 4 5 0) (unit 1)
                  (property "Reference" "R1" (at 4 4 0) (effects (font (size 1 1))))
                )
                (wire (pts (xy 4 5) (xy 10 5)))
                (label "LONG_SIGNAL_NAME" (at 7.5 5 0) (effects (font (size 1 1))))
                (rectangle (start 1 1) (end 8 9) (uuid "frame-1"))
                (text_box "Power input group" (at 4 6) (size 1 1) (uuid "description-1"))
                (text_box "Other note" (at 7.5 6) (size 2 1) (uuid "note-1"))
            )"#,
        )
        .expect("schematic parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"power-input":{"members":["R1"],"description_uuid":"description-1","frame_uuid":"frame-1","evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");
        let diagnostics = profile.validate(&schematic);

        assert!(
            diagnostics.iter().any(|item| {
                item.code == "SCH009" && item.message.contains("member wire segment")
            })
        );
        assert!(
            diagnostics.iter().any(|item| {
                item.code == "SCH009" && item.message.contains("member wire label")
            })
        );
        assert!(diagnostics.iter().any(|item| {
            item.code == "SCH009" && item.message.contains("annotation text box note-1")
        }));
    }
    /// Regression test: verifies that group wire labels must maintain configured frame clearance.
    #[test]
    fn group_wire_labels_must_maintain_configured_frame_clearance() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "group-frame-label-clearance-test.kicad_sch",
            r#"(kicad_sch
                (lib_symbols
                  (symbol "Test:OnePin"
                    (symbol "OnePin_1_1"
                      (pin passive line (at 0 0 0) (length 1.27)
                        (name "1") (number "1"))
                    )
                  )
                )
                (symbol (lib_id "Test:OnePin") (at 4 5 0) (unit 1)
                  (property "Reference" "R1" (at 4 4 0) (effects (font (size 1 1))))
                )
                (wire (pts (xy 4 5) (xy 10 5)))
                (label "LONG_SIGNAL_NAME" (at 10 5 0) (effects (font (size 1 1))))
                (rectangle (start 1 1) (end 14 9) (uuid "frame-1"))
                (text_box "Power input group" (at 5 5) (size 1 1) (uuid "description-1"))
            )"#,
        )
        .expect("schematic parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"power-input":{"members":["R1"],"description_uuid":"description-1","frame_uuid":"frame-1","minimum_frame_clearance_mm":2.0,"evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");
        let diagnostics = profile.validate(&schematic);

        assert!(diagnostics.iter().any(|item| {
            item.code == "SCH012"
                && item.message.contains("member wire label")
                && item.message.contains("2.00 mm clearance")
        }));
    }
    /// Regression test: verifies that group frame contains free standing wires labels and plain text.
    #[test]
    fn group_frame_contains_free_standing_wires_labels_and_plain_text() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "group-frame-free-standing-test.kicad_sch",
            r#"(kicad_sch
                (lib_symbols
                  (symbol "Test:OnePin"
                    (symbol "OnePin_1_1"
                      (pin passive line (at 0 0 0) (length 1.27)
                        (name "1") (number "1"))
                    )
                  )
                )
                (symbol (lib_id "Test:OnePin") (at 4 5 0) (unit 1)
                  (property "Reference" "R1" (at 4 4 0) (effects (font (size 1 1))))
                )
                (wire (pts (xy 2 7) (xy 10 7)))
                (label "UNATTACHED_LABEL_THAT_EXTENDS" (at 7.5 3 0) (effects (font (size 1 1))))
                (text "Unattached note beyond the right edge" (at 7.5 5 0)
                  (effects (font (size 1 1))))
                (rectangle (start 1 1) (end 8 9) (uuid "frame-1"))
                (text_box "Power input group" (at 4 6) (size 1 1) (uuid "description-1"))
            )"#,
        )
        .expect("schematic parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"power-input":{"members":["R1"],"description_uuid":"description-1","frame_uuid":"frame-1","evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");
        let diagnostics = profile.validate(&schematic);

        assert!(
            diagnostics.iter().any(|item| {
                item.code == "SCH009" && item.message.contains("member wire segment")
            })
        );
        assert!(
            diagnostics.iter().any(|item| {
                item.code == "SCH009" && item.message.contains("member wire label")
            })
        );
        assert!(diagnostics.iter().any(|item| {
            item.code == "SCH009" && item.message.contains("plain text annotation")
        }));
    }
    /// Regression test: verifies that group frame clearance covers free standing wires labels and plain text.
    #[test]
    fn group_frame_clearance_covers_free_standing_wires_labels_and_plain_text() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "group-frame-free-standing-clearance-test.kicad_sch",
            r#"(kicad_sch
                (lib_symbols
                  (symbol "Test:OnePin"
                    (symbol "OnePin_1_1"
                      (pin passive line (at 0 0 0) (length 1.27)
                        (name "1") (number "1"))
                    )
                  )
                )
                (symbol (lib_id "Test:OnePin") (at 4 5 0) (unit 1)
                  (property "Reference" "R1" (at 4 4 0) (effects (font (size 1 1))))
                )
                (wire (pts (xy 1.5 7) (xy 2.5 7)))
                (label "UNATTACHED_LONG_LABEL" (at 13.5 3 0) (effects (font (size 1 1))))
                (text "Unattached note near right edge" (at 13.5 5 0)
                  (effects (font (size 1 1))))
                (rectangle (start 1 1) (end 14 9) (uuid "frame-1"))
                (text_box "Power input group" (at 5 5) (size 1 1) (uuid "description-1"))
            )"#,
        )
        .expect("schematic parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"power-input":{"members":["R1"],"description_uuid":"description-1","frame_uuid":"frame-1","minimum_frame_clearance_mm":2.0,"evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");
        let diagnostics = profile.validate(&schematic);

        assert!(diagnostics.iter().any(|item| {
            item.code == "SCH012"
                && item.message.contains("member wire segment")
                && item.message.contains("2.00 mm clearance")
        }));
        assert!(diagnostics.iter().any(|item| {
            item.code == "SCH012"
                && item.message.contains("member wire label")
                && item.message.contains("2.00 mm clearance")
        }));
        assert!(diagnostics.iter().any(|item| {
            item.code == "SCH012"
                && item.message.contains("plain text annotation")
                && item.message.contains("2.00 mm clearance")
        }));
    }
    /// Regression test: verifies that group frame contains free standing vector graphics.
    #[test]
    fn group_frame_contains_free_standing_vector_graphics() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "group-frame-vector-graphics-test.kicad_sch",
            r#"(kicad_sch
                (lib_symbols
                  (symbol "Test:OnePin"
                    (symbol "OnePin_1_1"
                      (pin passive line (at 0 0 0) (length 1.27)
                        (name "1") (number "1"))
                    )
                  )
                )
                (symbol (lib_id "Test:OnePin") (at 4 5 0) (unit 1)
                  (property "Reference" "R1" (at 4 4 0) (effects (font (size 1 1))))
                )
                (polyline (pts (xy 2 2) (xy 10 2))
                  (stroke (width 0.254) (type default)) (fill (type none)))
                (circle (center 7.5 7) (radius 1)
                  (stroke (width 0.254) (type default)) (fill (type none)))
                (rectangle (start 1 1) (end 8 9) (uuid "frame-1"))
                (text_box "Power input group" (at 4 6) (size 1 1) (uuid "description-1"))
            )"#,
        )
        .expect("schematic parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"power-input":{"members":["R1"],"description_uuid":"description-1","frame_uuid":"frame-1","evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");
        let diagnostics = profile.validate(&schematic);

        assert!(diagnostics.iter().any(|item| {
            item.code == "SCH009" && item.message.contains("vector graphic polyline")
        }));
        assert!(diagnostics.iter().any(|item| {
            item.code == "SCH009" && item.message.contains("vector graphic circle")
        }));
    }
    /// Regression test: verifies that group frame clearance covers free standing vector graphics.
    #[test]
    fn group_frame_clearance_covers_free_standing_vector_graphics() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "group-frame-vector-clearance-test.kicad_sch",
            r#"(kicad_sch
                (lib_symbols
                  (symbol "Test:OnePin"
                    (symbol "OnePin_1_1"
                      (pin passive line (at 0 0 0) (length 1.27)
                        (name "1") (number "1"))
                    )
                  )
                )
                (symbol (lib_id "Test:OnePin") (at 4 5 0) (unit 1)
                  (property "Reference" "R1" (at 4 4 0) (effects (font (size 1 1))))
                )
                (polyline (pts (xy 1.5 7) (xy 2.5 7))
                  (stroke (width 0.254) (type default)) (fill (type none)))
                (circle (center 12.5 5) (radius 0.5)
                  (stroke (width 0.254) (type default)) (fill (type none)))
                (rectangle (start 1 1) (end 14 9) (uuid "frame-1"))
                (text_box "Power input group" (at 5 5) (size 1 1) (uuid "description-1"))
            )"#,
        )
        .expect("schematic parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"power-input":{"members":["R1"],"description_uuid":"description-1","frame_uuid":"frame-1","minimum_frame_clearance_mm":2.0,"evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");
        let diagnostics = profile.validate(&schematic);

        assert!(diagnostics.iter().any(|item| {
            item.code == "SCH012"
                && item.message.contains("vector graphic polyline")
                && item.message.contains("2.00 mm clearance")
        }));
        assert!(diagnostics.iter().any(|item| {
            item.code == "SCH012"
                && item.message.contains("vector graphic circle")
                && item.message.contains("2.00 mm clearance")
        }));
    }
    /// Regression test: verifies that group frame contains embedded bitmap image bounds.
    #[test]
    fn group_frame_contains_embedded_bitmap_image_bounds() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "group-frame-bitmap-test.kicad_sch",
            r#"(kicad_sch
                (version 20260623)
                (lib_symbols
                  (symbol "Test:OnePin"
                    (symbol "OnePin_1_1"
                      (pin passive line (at 0 0 0) (length 1.27)
                        (name "1") (number "1"))
                    )
                  )
                )
                (symbol (lib_id "Test:OnePin") (at 4 5 0) (unit 1)
                  (property "Reference" "R1" (at 4 4 0) (effects (font (size 1 1))))
                )
                (image (at 4 5) (scale 1) (uuid "image-1")
                  (data iVBORw0KGgoAAAANSUhEUgAAAHgAAAA8CAYAAACtrX6oAAAACXBIWXMAAC4jAAAuIwF4pT92AAAAoklEQVR4nO3RAQkAMAzAsPk3vak4h5IoKHSWtPkdwFsGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcd1OAFhGyMXxbAAAAAElFTkSuQmCC))
                (rectangle (start 1 1) (end 8 9) (uuid "frame-1"))
                (text_box "Power input group" (at 4 6) (size 1 1) (uuid "description-1"))
            )"#,
        )
        .expect("schematic parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"power-input":{"members":["R1"],"description_uuid":"description-1","frame_uuid":"frame-1","evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");

        assert!(profile.validate(&schematic).iter().any(|item| {
            item.code == "SCH009" && item.message.contains("bitmap image Some(\"image-1\")")
        }));
    }
    /// Regression test: verifies that group frame clearance covers embedded bitmap image bounds.
    #[test]
    fn group_frame_clearance_covers_embedded_bitmap_image_bounds() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "group-frame-bitmap-clearance-test.kicad_sch",
            r#"(kicad_sch
                (version 20260623)
                (lib_symbols
                  (symbol "Test:OnePin"
                    (symbol "OnePin_1_1"
                      (pin passive line (at 0 0 0) (length 1.27)
                        (name "1") (number "1"))
                    )
                  )
                )
                (symbol (lib_id "Test:OnePin") (at 7.5 5 0) (unit 1)
                  (property "Reference" "R1" (at 7.5 4 0) (effects (font (size 1 1))))
                )
                (image (at 7.5 5) (scale 1) (uuid "image-2")
                  (data iVBORw0KGgoAAAANSUhEUgAAAHgAAAA8CAYAAACtrX6oAAAACXBIWXMAAC4jAAAuIwF4pT92AAAAoklEQVR4nO3RAQkAMAzAsPk3vak4h5IoKHSWtPkdwFsGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcd1OAFhGyMXxbAAAAAElFTkSuQmCC))
                (rectangle (start 1 1) (end 14 9) (uuid "frame-1"))
                (text_box "Power input group" (at 5 5) (size 1 1) (uuid "description-1"))
            )"#,
        )
        .expect("schematic parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"groups":{"power-input":{"members":["R1"],"description_uuid":"description-1","frame_uuid":"frame-1","minimum_frame_clearance_mm":2.0,"evidence":{"document":"Power input design note","page":2,"figure":"Figure 1"}}}}"#,
        )
        .expect("profile parses");

        assert!(profile.validate(&schematic).iter().any(|item| {
            item.code == "SCH012"
                && item.message.contains("bitmap image Some(\"image-2\")")
                && item.message.contains("2.00 mm clearance")
        }));
    }
    /// Regression test: verifies that bitmap bounds use legacy resolution migration for old schematics.
    #[test]
    fn bitmap_bounds_use_legacy_resolution_migration_for_old_schematics() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "legacy-bitmap-resolution-test.kicad_sch",
            r"(kicad_sch
                (version 20250114)
                (image (at 0 0) (scale 1)
                  (data iVBORw0KGgoAAAANSUhEUgAAAHgAAAA8CAYAAACtrX6oAAAACXBIWXMAAA7EAAAOxAGVKw4bAAAAoklEQVR4nO3RAQkAMAzAsPk3vak4h5IoKHSWtPkdwFsGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcd1OAFhGyMXxbAAAAAElFTkSuQmCC))
            )",
        )
        .expect("schematic parses");
        let facts = SchematicFacts::from_document(&schematic);
        let bounds = facts.images[0].bounds_mm.expect("PNG bounds");
        let width = bounds.2 - bounds.0;
        let expected = 120.0 * 25.4 / 94.0;
        assert!((width - expected).abs() < 0.001);
    }
    /// Creates a flattened-netlist fixture for a decoupling capacitor connected between a target supply and ground.
    ///
    /// # Arguments
    ///
    /// * `capacitor_target_net` — Exported net name assigned to capacitor C1 pin 1; C1 pin 2 is assigned to GND.
    ///
    /// # Returns
    ///
    /// A `Netlist` containing U1.21 on `VREG_1V2`, C1.1 on the requested supply net, and C1.2 on GND.
    fn decoupling_netlist(capacitor_target_net: &str) -> Netlist {
        Netlist {
            pin_nets: BTreeMap::from([
                (
                    PinRef {
                        reference: "U1".to_owned(),
                        pin: "21".to_owned(),
                    },
                    "VREG_1V2".to_owned(),
                ),
                (
                    PinRef {
                        reference: "C1".to_owned(),
                        pin: "1".to_owned(),
                    },
                    capacitor_target_net.to_owned(),
                ),
                (
                    PinRef {
                        reference: "C1".to_owned(),
                        pin: "2".to_owned(),
                    },
                    "GND".to_owned(),
                ),
            ]),
            ..Netlist::default()
        }
    }
    /// Creates a flattened-netlist fixture for resistor R1 with configurable nets on pins 1 and 2.
    ///
    /// # Arguments
    ///
    /// * `pin_one_net` — Exported net name assigned to R1 pin 1.
    /// * `pin_two_net` — Optional exported net name assigned to R1 pin 2; `None` models a pin missing from the exported netlist.
    ///
    /// # Returns
    ///
    /// A `Netlist` containing the supplied R1 pin assignments.
    fn pin_contract_netlist(pin_one_net: &str, pin_two_net: Option<&str>) -> Netlist {
        let mut pin_nets = BTreeMap::from([(
            PinRef {
                reference: "R1".to_owned(),
                pin: "1".to_owned(),
            },
            pin_one_net.to_owned(),
        )]);
        if let Some(pin_two_net) = pin_two_net {
            pin_nets.insert(
                PinRef {
                    reference: "R1".to_owned(),
                    pin: "2".to_owned(),
                },
                pin_two_net.to_owned(),
            );
        }
        Netlist {
            pin_nets,
            ..Netlist::default()
        }
    }
    /// Regression test: verifies that declared component pin nets are verified against the exported netlist.
    #[test]
    fn declared_component_pin_nets_are_verified_against_the_exported_netlist() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "pin-contract-test.kicad_sch",
            r#"(kicad_sch (symbol (lib_id "Device:R") (property "Reference" "R1" (at 1 1 0))))"#,
        )
        .expect("fixture parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"components":{"R1":{"kind":"other","role":"test","function":"Signal path","rationale":"Preserve input-to-output continuity","pin_nets":{"1":"VIN","2":"VOUT"}}}}"#,
        )
        .expect("profile parses");
        assert!(
            !profile
                .validate_with_netlist(&schematic, Some(&pin_contract_netlist("VIN", Some("VOUT"))))
                .iter()
                .any(|item| item.code == "SCH014")
        );
    }
    /// Regression test: verifies that incorrect or missing component pin nets are reported.
    #[test]
    fn incorrect_or_missing_component_pin_nets_are_reported() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "pin-contract-test.kicad_sch",
            r#"(kicad_sch (symbol (lib_id "Device:R") (property "Reference" "R1" (at 1 1 0))))"#,
        )
        .expect("fixture parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"components":{"R1":{"kind":"other","role":"test","function":"Signal path","rationale":"Preserve input-to-output continuity","pin_nets":{"1":"VIN","2":"VOUT"}}}}"#,
        )
        .expect("profile parses");
        let diagnostics =
            profile.validate_with_netlist(&schematic, Some(&pin_contract_netlist("GND", None)));
        assert_eq!(
            diagnostics
                .iter()
                .filter(|item| item.code == "SCH014")
                .count(),
            2
        );
        assert!(
            diagnostics
                .iter()
                .any(|item| item.message.contains("connected to GND"))
        );
        assert!(diagnostics.iter().any(|item| {
            item.message
                .contains("absent from the exported KiCad netlist")
        }));
    }
    /// Regression test: verifies that connector pin nets are verified from pin numbered connections.
    #[test]
    fn connector_pin_nets_are_verified_from_pin_numbered_connections() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "connector-contract-test.kicad_sch",
            r#"(kicad_sch (symbol (lib_id "Connector_Generic:Conn_01x02") (property "Reference" "J1" (at 1 1 0))))"#,
        )
        .expect("fixture parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"components":{"J1":{"kind":"connector","role":"power_input","function":"Input power","rationale":"External supply interface","connections":{"pin.1":"VBUS"},"attributes":{"interface_standard":"barrel-jack"}}}}"#,
        )
        .expect("profile parses");
        let netlist = Netlist {
            pin_nets: BTreeMap::from([(
                PinRef {
                    reference: "J1".to_owned(),
                    pin: "1".to_owned(),
                },
                "GND".to_owned(),
            )]),
            ..Netlist::default()
        };
        assert!(
            profile
                .validate_with_netlist(&schematic, Some(&netlist))
                .iter()
                .any(|item| item.code == "SCH014" && item.message.contains("expects VBUS"))
        );
    }
    /// Regression test: verifies that decoupling terminals are checked against exported pin nets.
    #[test]
    fn decoupling_terminals_are_checked_against_exported_pin_nets() {
        let schematic = crate::parser::SchematicDocument::from_text(
            "decoupling-test.kicad_sch",
            r#"(kicad_sch (symbol (lib_id "Device:C") (property "Reference" "C1" (at 1 1 0))) (symbol (lib_id "MyLib:U") (property "Reference" "U1" (at 2 2 0))))"#,
        )
        .expect("fixture parses");
        let profile: LintProfile = serde_json::from_str(
            r#"{"schema_version":1,"components":{"C1":{"kind":"capacitor","role":"decoupling","function":"Local bypass","rationale":"Minimize loop area","decoupling":{"target_pin":"U1.21","target_terminal":"1","return_terminal":"2","return_net":"GND","cluster":"U1.VREG_1V2"}}}}"#,
        )
        .expect("profile parses");
        assert!(
            !profile
                .validate_with_netlist(&schematic, Some(&decoupling_netlist("VREG_1V2")))
                .iter()
                .any(|item| item.code == "SCH022")
        );
        assert!(
            profile
                .validate_with_netlist(&schematic, Some(&decoupling_netlist("VBUS")))
                .iter()
                .any(|item| item.code == "SCH022")
        );
    }
    /// Regression test: verifies that decoupling intent preserves pin and terminal mapping.
    #[test]
    fn decoupling_intent_preserves_pin_and_terminal_mapping() {
        let json = r#"{"schema_version":1,"mode":"strict","components":{"C1":{"kind":"capacitor","role":"decoupling","function":"Local bypass","rationale":"Minimize loop area","decoupling":{"target_pin":"U1.21","target_terminal":"1","return_terminal":"2","return_net":"GND","cluster":"U1.VREG_1V2"}}}}"#;
        let profile: LintProfile = serde_json::from_str(json).expect("valid fixture");
        let intent = profile.components.get("C1").expect("C1 metadata");
        assert_eq!(intent.kind, ComponentKind::Capacitor);
        assert_eq!(intent.role, ComponentRole("decoupling".to_owned()));
        assert_eq!(
            intent
                .decoupling
                .as_ref()
                .map(|item| item.target_pin.as_str()),
            Some("U1.21")
        );
        assert_eq!(profile.mode, ProfileMode::Strict);
    }
}
