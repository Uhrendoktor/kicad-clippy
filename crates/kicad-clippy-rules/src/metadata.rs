use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use kicad_clippy_core::{BuiltinRuleId, Diagnostic, Severity};
use kicad_clippy_erc::Netlist;
use kicad_clippy_parser::SchematicDocument;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::generic::{
    GEOMETRY_TOLERANCE_MM, HALF_FACTOR, STROKE_CLEARANCE_MARGIN_MM, SchematicFacts,
    TWO_SIDED_FACTOR, physical_wire_component_ids,
};

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
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
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
    /// Require all listed pins to resolve to the same net in the exported `KiCad` netlist.
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
    /// Also require the pins to resolve to different nets in the exported `KiCad` netlist.
    #[serde(default)]
    pub netlist_separate: bool,
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

/// Profile-level description and membership contract for one functional group.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
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
    /// Optional required clearance from group member origins and its description box to the frame edge.
    #[serde(default)]
    pub minimum_frame_clearance_mm: Option<f64>,
    /// Optional minimum Euclidean spacing between member symbol origins, in millimetres.
    #[serde(default)]
    pub minimum_member_origin_spacing_mm: Option<f64>,
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

    /// Validates all declared component, group, and connectivity intent against the schematic and, when available, KiCad's exported netlist.
    ///
    /// # Arguments
    ///
    /// * `document` — Parsed schematic whose symbols, geometry, labels, and group declarations are being checked.
    /// * `netlist` — Optional KiCad-exported flattened netlist; supplies logical pin-to-net assignments when present.
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
        let facts = SchematicFacts::from_document(document);
        let references: BTreeSet<_> = facts
            .components
            .iter()
            .map(|item| item.reference.clone())
            .collect();
        let mut diagnostics = self.validate_profile_references(&references);
        for component in &facts.components {
            if let Some(intent) = self.components.get(&component.reference) {
                diagnostics.extend(validate_component_intent(
                    &component.reference,
                    intent,
                    netlist,
                ));
            } else if self.mode == ProfileMode::Strict
                && !self.ignore_components.contains(&component.reference)
            {
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch021,
                    Severity::Error,
                    format!("{} has no component-intent metadata", component.reference),
                ));
            }
        }
        diagnostics.extend(validate_decoupler_groups(&self.components));
        diagnostics.extend(validate_groups(&self.groups, &facts));
        diagnostics.extend(validate_direct_wire_connectivity(
            &self.direct_wire_pairs,
            &self.forbidden_direct_wire_labels,
            &facts,
        ));
        diagnostics.extend(validate_common_pin_groups(
            &self.common_pin_groups,
            &facts,
            netlist,
        ));
        diagnostics.extend(validate_physical_wire_groups(
            &self.physical_wire_groups,
            &facts,
        ));
        diagnostics.extend(validate_separate_wire_pairs(
            &self.separate_wire_pairs,
            &facts,
            netlist,
        ));
        diagnostics
    }
    /// Reports profile entries whose reference designators do not identify placed symbols in the schematic.
    ///
    /// # Arguments
    ///
    /// * `references` — Set of schematic reference designators used to verify profile membership.
    ///
    /// # Returns
    ///
    /// Diagnostics for profile component references that do not resolve to placed symbols in the schematic.
    fn validate_profile_references(&self, references: &BTreeSet<String>) -> Vec<Diagnostic> {
        self.components
            .keys()
            .filter(|reference| !references.contains(*reference))
            .map(|reference| {
                Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch021,
                    Severity::Error,
                    format!("profile metadata references missing component {reference}"),
                )
            })
            .collect()
    }
}
/// Checks declared pin pairs and forbidden labels against physical wire connectivity, not merely matching net names.
///
/// # Arguments
///
/// * `pairs` — Declared physical-wire pin pairs, each expressed as two REF.PIN references.
/// * `forbidden_labels` — Names that are not permitted on the selected physical wire components.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
///
/// # Returns
///
/// Diagnostics for declared direct-wire pairs that are not physically connected, or for forbidden labels attached to those wire components.
fn validate_direct_wire_connectivity(
    pairs: &[DirectWirePair],
    forbidden_labels: &BTreeSet<String>,
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    if pairs.is_empty() && forbidden_labels.is_empty() {
        return Vec::new();
    }
    let component_ids = physical_wire_component_ids(facts);
    let mut protected_components = BTreeSet::new();
    let mut diagnostics = Vec::new();

    for pair in pairs {
        let (mut pair_diagnostics, pair_components) =
            validate_direct_wire_pair(pair, facts, &component_ids);
        diagnostics.append(&mut pair_diagnostics);
        protected_components.extend(pair_components);
    }
    diagnostics.extend(validate_forbidden_direct_wire_labels(
        forbidden_labels,
        &protected_components,
        &component_ids,
        facts,
    ));
    diagnostics
}
/// Checks that both REF.PIN endpoints share one physical wire-only component and do not have disallowed label anchors.
///
/// # Arguments
///
/// * `pair` — The direct-wire or separate-wire pair contract being validated.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `component_ids` — Map from tolerance-normalized schematic coordinates to physical wire-component identifiers.
///
/// # Returns
///
/// The diagnostics produced for this pair and the physical wire-component identifiers protected by its declaration.
fn validate_direct_wire_pair(
    pair: &DirectWirePair,
    facts: &SchematicFacts,
    component_ids: &BTreeMap<String, usize>,
) -> (Vec<Diagnostic>, BTreeSet<usize>) {
    let mut diagnostics = Vec::new();
    let mut protected_components = BTreeSet::new();
    let pair_name = format!("{} / {}", pair.first, pair.second);
    let (Some((first_reference, first_number)), Some((second_reference, second_number))) =
        (pair.first.rsplit_once('.'), pair.second.rsplit_once('.'))
    else {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch004,
            Severity::Error,
            format!("direct-wire pair {pair_name} must use REF.PIN notation"),
        ));
        return (diagnostics, protected_components);
    };
    if !valid_pin_ref(&pair.first) || !valid_pin_ref(&pair.second) || pair.first == pair.second {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch004,
            Severity::Error,
            format!("direct-wire pair {pair_name} must identify two distinct valid pins"),
        ));
        return (diagnostics, protected_components);
    }

    let first_pin = facts
        .pins
        .iter()
        .find(|pin| pin.reference == first_reference && pin.number == first_number);
    let second_pin = facts
        .pins
        .iter()
        .find(|pin| pin.reference == second_reference && pin.number == second_number);
    let (Some(first_pin), Some(second_pin)) = (first_pin, second_pin) else {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch004,
            Severity::Error,
            format!(
                "direct-wire pair {pair_name} references a missing or unsupported embedded pin"
            ),
        ));
        return (diagnostics, protected_components);
    };

    let first_point = first_pin.position_mm;
    let second_point = second_pin.position_mm;
    let first_attached = point_is_on_wire(first_point, facts);
    let second_attached = point_is_on_wire(second_point, facts);
    if !first_attached {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch004,
            Severity::Error,
            format!(
                "pin {first_reference}.{first_number} is not attached to a physical wire segment"
            ),
        ));
    }
    if !second_attached {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch004,
            Severity::Error,
            format!(
                "pin {second_reference}.{second_number} is not attached to a physical wire segment"
            ),
        ));
    }
    if first_attached && second_attached {
        let first_key = format!(
            "{:.*}:{:.*}",
            COORDINATE_KEY_DECIMAL_PLACES,
            first_point.0,
            COORDINATE_KEY_DECIMAL_PLACES,
            first_point.1
        );
        let second_key = format!(
            "{:.*}:{:.*}",
            COORDINATE_KEY_DECIMAL_PLACES,
            second_point.0,
            COORDINATE_KEY_DECIMAL_PLACES,
            second_point.1
        );
        let first_component = component_ids.get(&first_key).copied();
        let second_component = component_ids.get(&second_key).copied();
        if first_component != second_component {
            diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch004,
                Severity::Error,
                format!(
                    "{} and {} must share one physical wire-only network; labels do not establish this connection",
                    pair.first, pair.second
                ),
            ));
        }
        protected_components.extend(first_component);
        protected_components.extend(second_component);
    }

    append_direct_wire_pair_label_diagnostics(pair, facts, &mut diagnostics);
    (diagnostics, protected_components)
}

/// Adds diagnostics for non-global labels attached to either direct-wire pin anchor.
///
/// # Arguments
///
/// * `pair` — Direct-wire pair whose endpoint anchors are being checked.
/// * `facts` — Schematic facts containing the resolved label positions.
/// * `diagnostics` — Diagnostic collection to extend with any invalid label overlaps.
fn append_direct_wire_pair_label_diagnostics(
    pair: &DirectWirePair,
    facts: &SchematicFacts,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let endpoints = [
        (pair.first.as_str(), pair.first.rsplit_once('.')),
        (pair.second.as_str(), pair.second.rsplit_once('.')),
    ];
    for (pin_reference, parsed_reference) in endpoints {
        let Some((reference, number)) = parsed_reference else {
            continue;
        };
        let Some(pin) = facts
            .pins
            .iter()
            .find(|pin| pin.reference == reference && pin.number == number)
        else {
            continue;
        };
        for label in &facts.labels {
            if label.kind != "global_label" && same_wire_point(label.position_mm, pin.position_mm) {
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch004,
                    Severity::Error,
                    format!(
                        "direct-wire pin {pin_reference} shares its anchor with {} {:?}; use a continuous wire-only connection",
                        label.kind, label.name
                    ),
                ));
            }
        }
    }
}
/// Reports forbidden labels that appear on wire components protected by a direct-wire-pair declaration.
///
/// # Arguments
///
/// * `forbidden_labels` — Names that are not permitted on the selected physical wire components.
/// * `protected_components` — Wire-component identifiers that are covered by a direct-wire-pair declaration.
/// * `component_ids` — Map from tolerance-normalized schematic coordinates to physical wire-component identifiers.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
///
/// # Returns
///
/// Diagnostics for forbidden label kinds or names found on physical wire components protected by a direct-wire contract.
fn validate_forbidden_direct_wire_labels(
    forbidden_labels: &BTreeSet<String>,
    protected_components: &BTreeSet<usize>,
    component_ids: &BTreeMap<String, usize>,
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for label in facts.labels.iter().filter(|label| {
        if label.kind != "label" || !forbidden_labels.contains(&label.name) {
            return false;
        }
        let key = format!(
            "{:.*}:{:.*}",
            COORDINATE_KEY_DECIMAL_PLACES,
            label.position_mm.0,
            COORDINATE_KEY_DECIMAL_PLACES,
            label.position_mm.1
        );
        component_ids
            .get(&key)
            .is_some_and(|component| protected_components.contains(component))
    }) {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch004,
            Severity::Error,
            format!(
                "local label {:?} is forbidden on a direct-wire-only network",
                label.name
            ),
        ));
    }
    diagnostics
}
/// Validates common-net group declarations against physical wire topology and the optional exported netlist.
///
/// # Arguments
///
/// * `groups` — Map of profile group identifiers to their associated group-intent records.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `netlist` — Optional KiCad-exported flattened netlist; supplies logical pin-to-net assignments when present.
///
/// # Returns
///
/// Diagnostics for common-pin groups whose members fail physical-wire, label, or exported-netlist connectivity requirements.
fn validate_common_pin_groups(
    groups: &[CommonPinNetGroup],
    facts: &SchematicFacts,
    netlist: Option<&Netlist>,
) -> Vec<Diagnostic> {
    if groups.is_empty() {
        return Vec::new();
    }
    let component_ids = physical_wire_component_ids(facts);
    let mut diagnostics = Vec::new();
    for group in groups {
        diagnostics.extend(validate_common_pin_group(group, facts, &component_ids));
        diagnostics.extend(validate_common_pin_group_netlist(group, netlist));
    }
    diagnostics
}
/// Checks that every pin in one common-net group resolves to the same physical wire component and has the required label.
///
/// # Arguments
///
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `component_ids` — Map from tolerance-normalized schematic coordinates to physical wire-component identifiers.
///
/// # Returns
///
/// Diagnostics for missing group pins, disconnected wire components, or an absent/mismatched label on the common physical net.
fn validate_common_pin_group(
    group: &CommonPinNetGroup,
    facts: &SchematicFacts,
    component_ids: &BTreeMap<String, usize>,
) -> Vec<Diagnostic> {
    if let Some(invalid) = common_pin_group_configuration_error(group) {
        return vec![invalid];
    }
    let (roots, mut diagnostics) = resolve_common_pin_group_components(group, facts, component_ids);
    if group.wire_connected && roots.len() > 1 {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch004,
            Severity::Error,
            format!(
                "configured pins {:?} must share one physical {} wire network",
                group.pins, group.name
            ),
        ));
    }
    if group.single_label {
        let matching_labels = common_pin_group_label_count(group, &roots, facts, component_ids);
        if matching_labels != 1 {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch004,
                Severity::Error,
                format!(
                    "configured pins {:?} must share exactly one {} label; found {matching_labels}",
                    group.pins, group.name
                ),
            ));
        }
    }
    diagnostics
}
/// Checks the exported net assignments for a common-pin group when a netlist is available.
///
/// # Arguments
///
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `netlist` — Optional KiCad-exported flattened netlist; supplies logical pin-to-net assignments when present.
///
/// # Returns
///
/// Diagnostics when the exported netlist assigns group pins to different logical nets or to a net name other than the declared name.
fn validate_common_pin_group_netlist(
    group: &CommonPinNetGroup,
    netlist: Option<&Netlist>,
) -> Vec<Diagnostic> {
    if !group.netlist_connected {
        return Vec::new();
    }
    let Some(netlist) = netlist else {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch004,
            Severity::Warning,
            format!(
                "common pin group {:?} requests netlist_connected but no exported KiCad netlist was supplied",
                group.name
            ),
        )];
    };

    let mut nets = BTreeSet::new();
    let mut diagnostics = Vec::new();
    for pin_ref in &group.pins {
        let Some((reference, pin_number)) = pin_ref.rsplit_once('.') else {
            continue;
        };
        if !valid_pin_ref(pin_ref) {
            continue;
        }
        match netlist.net_for_pin(reference, pin_number) {
            Some(net_name) => {
                nets.insert(net_name.to_owned());
            }
            None => diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch004,
                Severity::Error,
                format!(
                    "{pin_ref} in common pin group {:?} is absent from the exported KiCad netlist",
                    group.name
                ),
            )),
        }
    }
    if nets.len() > 1 {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch004,
            Severity::Error,
            format!(
                "pins in common pin group {:?} resolve to different exported KiCad nets: {}",
                group.name,
                nets.into_iter().collect::<Vec<_>>().join(", ")
            ),
        ));
    }
    diagnostics
}
/// Validates the physical membership and label constraints of each declared wire group.
///
/// # Arguments
///
/// * `groups` — Map of profile group identifiers to their associated group-intent records.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
///
/// # Returns
///
/// Diagnostics for physical-wire groups whose declared pin members do not share the required wire-only component.
fn validate_physical_wire_groups(
    groups: &[PhysicalWireGroup],
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    if groups.is_empty() {
        return Vec::new();
    }
    let component_ids = physical_wire_component_ids(facts);
    groups
        .iter()
        .flat_map(|group| validate_physical_wire_group(group, facts, &component_ids))
        .collect()
}
/// Checks that the declared pins of one physical-wire group resolve to the required connected wire component.
///
/// # Arguments
///
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `component_ids` — Map from tolerance-normalized schematic coordinates to physical wire-component identifiers.
///
/// # Returns
///
/// Diagnostics for pins that are missing, unattached, or split across more than one physical wire component.
fn validate_physical_wire_group(
    group: &PhysicalWireGroup,
    facts: &SchematicFacts,
    component_ids: &BTreeMap<String, usize>,
) -> Vec<Diagnostic> {
    let unique_pins: BTreeSet<_> = group.pins.iter().collect();
    if group.name.trim().is_empty() {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch004,
            Severity::Error,
            "physical wire group must have a non-empty name",
        )];
    }
    if group.pins.len() < MINIMUM_PIN_GROUP_SIZE || unique_pins.len() != group.pins.len() {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch004,
            Severity::Error,
            format!(
                "physical wire group {:?} must list at least two distinct REF.PIN entries",
                group.name
            ),
        )];
    }

    let mut roots = BTreeSet::new();
    let mut diagnostics = Vec::new();
    for pin_ref in &group.pins {
        let Some((reference, pin_number)) = pin_ref.rsplit_once('.') else {
            diagnostics.push(invalid_physical_wire_reference(&group.name, pin_ref));
            continue;
        };
        if !valid_pin_ref(pin_ref) {
            diagnostics.push(invalid_physical_wire_reference(&group.name, pin_ref));
            continue;
        }
        let Some(pin) = facts
            .pins
            .iter()
            .find(|pin| pin.reference == reference && pin.number == pin_number)
        else {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch004,
                Severity::Error,
                format!(
                    "physical wire group {:?} references missing or unsupported pin {pin_ref}",
                    group.name
                ),
            ));
            continue;
        };
        if !point_is_on_wire(pin.position_mm, facts) {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch004,
                Severity::Error,
                format!(
                    "{pin_ref} in physical wire group {:?} is not attached to a physical wire",
                    group.name
                ),
            ));
            continue;
        }
        let key = format!(
            "{:.*}:{:.*}",
            COORDINATE_KEY_DECIMAL_PLACES,
            pin.position_mm.0,
            COORDINATE_KEY_DECIMAL_PLACES,
            pin.position_mm.1
        );
        if let Some(component) = component_ids.get(&key).copied() {
            roots.insert(component);
        } else {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch004,
                Severity::Error,
                format!(
                    "{pin_ref} in physical wire group {:?} has no resolved physical wire component",
                    group.name
                ),
            ));
        }
    }

    if roots.len() > 1 {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch004,
            Severity::Error,
            format!(
                "pins {:?} in physical wire group {:?} must share one physical wire-only network; matching labels do not establish this connection",
                group.pins, group.name
            ),
        ));
    }
    diagnostics
}
/// Builds an SCH004 error for a physical-wire group member that is not a valid REF.PIN identifier.
///
/// # Arguments
///
/// * `group_name` — Human-readable group name used in the diagnostic message.
/// * `pin_ref` — A single pin identifier in REF.PIN form.
///
/// # Returns
///
/// An SCH004 error diagnostic identifying the malformed pin reference.
fn invalid_physical_wire_reference(group_name: &str, pin_ref: &str) -> Diagnostic {
    Diagnostic::for_builtin_rule(
        BuiltinRuleId::Sch004,
        Severity::Error,
        format!("physical wire group {group_name:?} contains invalid pin reference {pin_ref:?}"),
    )
}
/// Checks that each declared pair of pins belongs to separate physical and logical nets.
///
/// # Arguments
///
/// * `pairs` — Declared physical-wire pin pairs, each expressed as two REF.PIN references.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `netlist` — Optional KiCad-exported flattened netlist; supplies logical pin-to-net assignments when present.
///
/// # Returns
///
/// Diagnostics for separate-wire pairs that share a physical wire component or an exported logical net.
fn validate_separate_wire_pairs(
    pairs: &[SeparateWirePair],
    facts: &SchematicFacts,
    netlist: Option<&Netlist>,
) -> Vec<Diagnostic> {
    if pairs.is_empty() {
        return Vec::new();
    }
    let component_ids = physical_wire_component_ids(facts);
    let mut diagnostics = Vec::new();
    for pair in pairs {
        diagnostics.extend(validate_separate_wire_pair(
            pair,
            facts,
            &component_ids,
            netlist,
        ));
    }
    diagnostics
}
/// Checks that the two pins in a separate-wire contract do not share a physical wire component or an exported logical net.
///
/// # Arguments
///
/// * `pair` — The direct-wire or separate-wire pair contract being validated.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `component_ids` — Map from tolerance-normalized schematic coordinates to physical wire-component identifiers.
/// * `netlist` — Optional KiCad-exported flattened netlist; supplies logical pin-to-net assignments when present.
///
/// # Returns
///
/// Diagnostics when either pin is missing or the two pins share a physical wire component or logical net.
fn validate_separate_wire_pair(
    pair: &SeparateWirePair,
    facts: &SchematicFacts,
    component_ids: &BTreeMap<String, usize>,
    netlist: Option<&Netlist>,
) -> Vec<Diagnostic> {
    let pair_name = format!("{} / {}", pair.first, pair.second);
    let mut diagnostics = Vec::new();
    if !valid_pin_ref(&pair.first) || !valid_pin_ref(&pair.second) || pair.first == pair.second {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch004,
            Severity::Error,
            format!(
                "separate-wire pair {pair_name} must identify two distinct valid REF.PIN entries"
            ),
        )];
    }

    let (Some((first_reference, first_number)), Some((second_reference, second_number))) =
        (pair.first.rsplit_once('.'), pair.second.rsplit_once('.'))
    else {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch004,
            Severity::Error,
            format!("separate-wire pair {pair_name} must use REF.PIN notation"),
        )];
    };
    let first_pin = facts
        .pins
        .iter()
        .find(|pin| pin.reference == first_reference && pin.number == first_number);
    let second_pin = facts
        .pins
        .iter()
        .find(|pin| pin.reference == second_reference && pin.number == second_number);
    let (Some(first_pin), Some(second_pin)) = (first_pin, second_pin) else {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch004,
            Severity::Error,
            format!(
                "separate-wire pair {pair_name} references a missing or unsupported embedded pin"
            ),
        )];
    };

    let physical_component = |point: (f64, f64)| {
        if !point_is_on_wire(point, facts) {
            return None;
        }
        let key = format!(
            "{:.*}:{:.*}",
            COORDINATE_KEY_DECIMAL_PLACES, point.0, COORDINATE_KEY_DECIMAL_PLACES, point.1
        );
        component_ids.get(&key).copied()
    };
    let first_component = physical_component(first_pin.position_mm);
    let second_component = physical_component(second_pin.position_mm);
    if matches!((first_component, second_component), (Some(first), Some(second)) if first == second)
    {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch004,
            Severity::Error,
            format!(
                "pins {} and {} must remain on separate physical wire-only networks",
                pair.first, pair.second
            ),
        ));
    }

    if pair.netlist_separate {
        let Some(netlist) = netlist else {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch004,
                Severity::Warning,
                format!("separate-wire pair {pair_name} requests netlist_separate but no exported KiCad netlist was supplied"),
            ));
            return diagnostics;
        };
        let first_net = netlist.net_for_pin(first_reference, first_number);
        let second_net = netlist.net_for_pin(second_reference, second_number);
        match (first_net, second_net) {
            (Some(first), Some(second)) if first == second => {
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch004,
                    Severity::Error,
                    format!(
                        "pins {} and {} must resolve to different exported KiCad nets, but both resolve to {first:?}",
                        pair.first, pair.second
                    ),
                ));
            }
            (None, _) => diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch004,
                Severity::Error,
                format!("{} in separate-wire pair {pair_name} is absent from the exported KiCad netlist", pair.first),
            )),
            (_, None) => diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch004,
                Severity::Error,
                format!("{} in separate-wire pair {pair_name} is absent from the exported KiCad netlist", pair.second),
            )),
            (Some(_), Some(_)) => {}
        }
    }
    diagnostics
}
/// Returns a configuration error when a common-pin group has an empty name, too few pins, duplicate pins, or malformed pin references.
///
/// # Arguments
///
/// * `group` — The group-intent record containing its members and declared constraints.
///
/// # Returns
///
/// A configuration diagnostic when the group declaration is invalid, or `None` when the declaration is well formed.
fn common_pin_group_configuration_error(group: &CommonPinNetGroup) -> Option<Diagnostic> {
    let group_name = group.name.trim();
    let unique_pins: BTreeSet<_> = group.pins.iter().collect();
    let message = if group_name.is_empty() {
        Some("common pin group must define a non-empty expected label/net name".to_owned())
    } else if group.pins.len() < MINIMUM_PIN_GROUP_SIZE || unique_pins.len() != group.pins.len() {
        Some(format!(
            "common pin group {group_name:?} must list at least two distinct REF.PIN entries"
        ))
    } else {
        None
    };
    message.map(|message| {
        Diagnostic::for_builtin_rule(BuiltinRuleId::Sch004, Severity::Error, message)
    })
}
/// Resolves the physical wire components touched by a common-pin group and reports pins that cannot be mapped to the schematic.
///
/// # Arguments
///
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `component_ids` — Map from tolerance-normalized schematic coordinates to physical wire-component identifiers.
///
/// # Returns
///
/// The physical wire components resolved from the declared pins, together with diagnostics for pins that could not be resolved.
fn resolve_common_pin_group_components(
    group: &CommonPinNetGroup,
    facts: &SchematicFacts,
    component_ids: &BTreeMap<String, usize>,
) -> (BTreeSet<usize>, Vec<Diagnostic>) {
    let mut roots = BTreeSet::new();
    let mut diagnostics = Vec::new();
    for pin_ref in &group.pins {
        let Some((reference, pin_number)) = pin_ref.rsplit_once('.') else {
            diagnostics.push(invalid_common_pin_reference(&group.name, pin_ref));
            continue;
        };
        if !valid_pin_ref(pin_ref) {
            diagnostics.push(invalid_common_pin_reference(&group.name, pin_ref));
            continue;
        }
        let pin = facts
            .pins
            .iter()
            .find(|item| item.reference == reference && item.number == pin_number);
        let Some(pin) = pin else {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch004,
                Severity::Error,
                format!(
                    "common pin group {:?} references missing or unsupported pin {pin_ref}",
                    group.name
                ),
            ));
            continue;
        };
        if !point_is_on_wire(pin.position_mm, facts) {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch004,
                Severity::Error,
                format!(
                    "{pin_ref} in common pin group {:?} is not attached to a physical wire",
                    group.name
                ),
            ));
            continue;
        }
        let key = format!(
            "{:.*}:{:.*}",
            COORDINATE_KEY_DECIMAL_PLACES,
            pin.position_mm.0,
            COORDINATE_KEY_DECIMAL_PLACES,
            pin.position_mm.1
        );
        let Some(component) = component_ids.get(&key).copied() else {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch004,
                Severity::Error,
                format!(
                    "{pin_ref} in common pin group {:?} has no resolved physical wire component",
                    group.name
                ),
            ));
            continue;
        };
        roots.insert(component);
        if !component_has_label(component, group.name.trim(), facts, component_ids) {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch004,
                Severity::Error,
                format!("{pin_ref} must belong to common net {}", group.name),
            ));
        }
    }
    (roots, diagnostics)
}
/// Builds an SCH004 error for a common-pin group member that is not a valid REF.PIN identifier.
///
/// # Arguments
///
/// * `group_name` — Human-readable group name used in the diagnostic message.
/// * `pin_ref` — A single pin identifier in REF.PIN form.
///
/// # Returns
///
/// An SCH004 error diagnostic identifying the malformed pin reference.
fn invalid_common_pin_reference(group_name: &str, pin_ref: &str) -> Diagnostic {
    Diagnostic::for_builtin_rule(
        BuiltinRuleId::Sch004,
        Severity::Error,
        format!("common pin group {group_name:?} contains invalid pin reference {pin_ref:?}"),
    )
}
/// Counts how many declared pins belong to a physical component carrying the requested common-net label.
///
/// # Arguments
///
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `roots` — Physical wire-component identifiers already resolved for the group's declared pins.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `component_ids` — Map from tolerance-normalized schematic coordinates to physical wire-component identifiers.
///
/// # Returns
///
/// The number of declared pins that resolve to a component carrying the expected label.
fn common_pin_group_label_count(
    group: &CommonPinNetGroup,
    roots: &BTreeSet<usize>,
    facts: &SchematicFacts,
    component_ids: &BTreeMap<String, usize>,
) -> usize {
    facts
        .labels
        .iter()
        .filter(|label| {
            if label.name != group.name.trim() {
                return false;
            }
            let key = format!(
                "{:.*}:{:.*}",
                COORDINATE_KEY_DECIMAL_PLACES,
                label.position_mm.0,
                COORDINATE_KEY_DECIMAL_PLACES,
                label.position_mm.1
            );
            component_ids
                .get(&key)
                .is_some_and(|component| roots.contains(component))
        })
        .count()
}
/// Checks whether a physical wire component carries a label with the requested exact name.
///
/// # Arguments
///
/// * `component` — Physical wire-component identifier to inspect.
/// * `expected_name` — Exact label text expected on the selected wire component.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `component_ids` — Map from tolerance-normalized schematic coordinates to physical wire-component identifiers.
///
/// # Returns
///
/// `true` when the physical component carries a label whose text exactly matches `expected_name`.
fn component_has_label(
    component: usize,
    expected_name: &str,
    facts: &SchematicFacts,
    component_ids: &BTreeMap<String, usize>,
) -> bool {
    facts.labels.iter().any(|label| {
        if label.name != expected_name {
            return false;
        }
        let key = format!(
            "{:.*}:{:.*}",
            COORDINATE_KEY_DECIMAL_PLACES,
            label.position_mm.0,
            COORDINATE_KEY_DECIMAL_PLACES,
            label.position_mm.1
        );
        component_ids.get(&key) == Some(&component)
    })
}
/// Checks whether a schematic coordinate lies on any parsed wire segment within the geometry tolerance.
///
/// # Arguments
///
/// * `point` — Schematic coordinate to test, in millimetres.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
///
/// # Returns
///
/// `true` when the coordinate lies on a wire segment within the schematic geometry tolerance.
fn point_is_on_wire(point: (f64, f64), facts: &SchematicFacts) -> bool {
    facts.wires.iter().any(|wire| {
        let horizontal = (wire.start_mm.1 - wire.end_mm.1).abs() <= GEOMETRY_TOLERANCE_MM;
        if horizontal {
            (point.1 - wire.start_mm.1).abs() <= GEOMETRY_TOLERANCE_MM
                && point.0 >= wire.start_mm.0.min(wire.end_mm.0) - GEOMETRY_TOLERANCE_MM
                && point.0 <= wire.start_mm.0.max(wire.end_mm.0) + GEOMETRY_TOLERANCE_MM
        } else if (wire.start_mm.0 - wire.end_mm.0).abs() <= GEOMETRY_TOLERANCE_MM {
            (point.0 - wire.start_mm.0).abs() <= GEOMETRY_TOLERANCE_MM
                && point.1 >= wire.start_mm.1.min(wire.end_mm.1) - GEOMETRY_TOLERANCE_MM
                && point.1 <= wire.start_mm.1.max(wire.end_mm.1) + GEOMETRY_TOLERANCE_MM
        } else {
            false
        }
    })
}
/// Compares two schematic coordinates using the shared geometry tolerance.
///
/// # Arguments
///
/// * `first` — First schematic point or pin endpoint in the comparison.
/// * `second` — Second schematic point or pin endpoint in the comparison.
///
/// # Returns
///
/// `true` when both coordinates match within the schematic geometry tolerance.
fn same_wire_point(first: (f64, f64), second: (f64, f64)) -> bool {
    (first.0 - second.0).abs() <= GEOMETRY_TOLERANCE_MM
        && (first.1 - second.1).abs() <= GEOMETRY_TOLERANCE_MM
}
/// Validates every group-intent declaration in the profile.
///
/// # Arguments
///
/// * `groups` — Map of profile group identifiers to their associated group-intent records.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
///
/// # Returns
///
/// Diagnostics for every invalid group declaration, including membership, evidence, containment, clearance, spacing, and wire-length constraints.
fn validate_groups(
    groups: &BTreeMap<String, GroupIntent>,
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    let references: BTreeSet<_> = facts
        .components
        .iter()
        .map(|component| component.reference.as_str())
        .collect();
    let mut diagnostics = Vec::new();
    for (group_id, group) in groups {
        diagnostics.extend(validate_group(group_id, group, facts, &references));
    }
    diagnostics
}
/// Validates one group declaration, including membership, evidence, containment, frame clearance, spacing, and wiring contracts.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `references` — Set of schematic reference designators used to verify profile membership.
///
/// # Returns
///
/// Diagnostics for the group's invalid evidence, members, KiCad membership, containment, frame clearance, spacing, or wiring contract.
fn validate_group(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    references: &BTreeSet<&str>,
) -> Vec<Diagnostic> {
    let mut diagnostics = validate_group_evidence(group_id, group);
    diagnostics.extend(validate_group_members(group_id, group, references));
    diagnostics.extend(validate_kicad_group_membership(group_id, group, facts));
    diagnostics.extend(validate_group_containment(group_id, group, facts));
    diagnostics.extend(validate_frame_clearance(group_id, group, facts));
    diagnostics.extend(validate_group_spacing(group_id, group, facts));
    diagnostics.extend(validate_wire_compactness(group_id, group, facts));
    let description = facts
        .text_boxes
        .iter()
        .find(|text_box| text_box.uuid == group.description_uuid)
        .map(|text_box| text_box.text.as_str());
    let Some(description) = description else {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch017,
            Severity::Error,
            format!(
                "group {group_id} description text box UUID {} is missing",
                group.description_uuid
            ),
        ));
        return diagnostics;
    };
    diagnostics.extend(validate_group_component_purposes(
        group_id,
        group,
        description,
    ));
    diagnostics
}
/// Checks that every declared group member and its attached geometry lies inside the group's referenced frame.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
///
/// # Returns
///
/// Diagnostics for group members or attached schematic objects that extend outside the frame assigned to the group.
fn validate_group_containment(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    let Some(frame_uuid) = group.frame_uuid.as_deref() else {
        return Vec::new();
    };
    let Some(frame) = facts.frames.iter().find(|frame| frame.uuid == frame_uuid) else {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch009,
            Severity::Error,
            format!("group {group_id} references missing schematic frame UUID {frame_uuid}"),
        )];
    };
    let bounds = frame.bounds_mm;
    if !valid_frame_bounds(bounds) {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch009,
            Severity::Error,
            format!("group {group_id} frame {frame_uuid} has invalid or zero-area bounds"),
        )];
    }

    let mut diagnostics = Vec::new();
    for reference in &group.members {
        let Some(component) = facts
            .components
            .iter()
            .find(|component| component.reference == *reference)
        else {
            continue;
        };
        let Some(position) = component.position_mm else {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch009,
                Severity::Error,
                format!(
                    "group {group_id} member {reference} has no placed-symbol origin for frame containment validation"
                ),
            ));
            continue;
        };
        if !point_inside_frame(bounds, position) {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch009,
                Severity::Error,
                format!(
                    "group {group_id} member {reference} origin ({:.2}, {:.2}) mm is outside frame {frame_uuid}",
                    position.0, position.1
                ),
            ));
        }
    }
    diagnostics.extend(validate_group_pin_containment(
        group_id, group, facts, frame_uuid, bounds,
    ));
    diagnostics.extend(validate_group_field_containment(
        group_id, group, facts, frame_uuid, bounds,
    ));
    diagnostics.extend(validate_group_description_containment(
        group_id, group, facts, frame_uuid, bounds,
    ));
    diagnostics.extend(validate_group_symbol_primitive_containment(
        group_id, group, facts, frame_uuid, bounds,
    ));
    diagnostics.extend(validate_group_wire_containment(
        group_id, group, facts, frame_uuid, bounds,
    ));
    diagnostics.extend(validate_group_label_containment(
        group_id, group, facts, frame_uuid, bounds,
    ));
    diagnostics.extend(validate_group_annotation_box_containment(
        group_id, group, facts, frame_uuid, bounds,
    ));
    diagnostics.extend(validate_group_plain_text_containment(
        group_id, facts, frame_uuid, bounds,
    ));
    diagnostics.extend(validate_group_graphic_containment(
        group_id, facts, frame_uuid, bounds,
    ));
    diagnostics.extend(validate_group_image_containment(
        group_id, facts, frame_uuid, bounds,
    ));
    diagnostics
}
/// Checks that every pin belonging to a group member lies inside the frame bounds.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
///
/// # Returns
///
/// Diagnostics for pins belonging to group members whose anchors lie outside the referenced frame.
fn validate_group_pin_containment(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
) -> Vec<Diagnostic> {
    facts
        .pins
        .iter()
        .filter(|pin| group.members.contains(&pin.reference))
        .filter(|pin| !point_inside_frame(frame_bounds, pin.position_mm))
        .map(|pin| {
            Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch009,
                Severity::Error,
                format!(
                    "group {group_id} member pin {}.{} at ({:.2}, {:.2}) mm is outside frame {frame_uuid}",
                    pin.reference, pin.number, pin.position_mm.0, pin.position_mm.1
                ),
            )
        })
        .collect()
}
/// Checks that visible reference/value fields for group members lie inside the frame bounds.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
///
/// # Returns
///
/// Diagnostics for visible reference or value fields belonging to group members that extend outside the frame.
fn validate_group_field_containment(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
) -> Vec<Diagnostic> {
    facts
        .visible_fields
        .iter()
        .filter(|field| group.members.contains(&field.reference))
        .filter(|field| !rectangle_inside_frame(field.bounds_mm, frame_bounds))
        .map(|field| {
            Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch009,
                Severity::Error,
                format!(
                    "group {group_id} member field {} {} extends outside frame {frame_uuid}",
                    field.reference, field.name
                ),
            )
        })
        .collect()
}
/// Checks that the description text box referenced by the group lies inside the frame bounds.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
///
/// # Returns
///
/// Diagnostics when the group's referenced description text box extends beyond the frame bounds.
fn validate_group_description_containment(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
) -> Vec<Diagnostic> {
    let Some(description) = facts
        .text_boxes
        .iter()
        .find(|text_box| text_box.uuid == group.description_uuid)
    else {
        return Vec::new();
    };
    let Some(description_bounds) = description.bounds_mm else {
        return Vec::new();
    };
    if rectangle_inside_frame(description_bounds, frame_bounds) {
        return Vec::new();
    }
    vec![Diagnostic::for_builtin_rule(
        BuiltinRuleId::Sch009,
        Severity::Error,
        format!(
            "group {group_id} description text box {} extends outside frame {frame_uuid}",
            group.description_uuid
        ),
    )]
}
/// Checks that symbol-library primitives for group members lie inside the frame bounds.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
///
/// # Returns
///
/// Diagnostics for embedded symbol primitives whose transformed geometry extends outside the group frame.
fn validate_group_symbol_primitive_containment(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
) -> Vec<Diagnostic> {
    facts
        .symbol_primitives
        .iter()
        .filter(|primitive| group.members.contains(&primitive.reference))
        .filter(|primitive| !symbol_primitive_inside_frame(primitive, frame_bounds, 0.0))
        .map(|primitive| {
            Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch009,
                Severity::Error,
                format!(
                    "group {group_id} member {} symbol primitive extends outside frame {frame_uuid}",
                    primitive.reference
                ),
            )
        })
        .collect()
}
/// Tests whether a symbol primitive's stroked geometry is contained within the supplied frame rectangle.
///
/// # Arguments
///
/// * `primitive` — The transformed symbol primitive whose stroked geometry must fit inside the frame.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
/// * `clearance` — Minimum required distance from each frame edge, in millimetres.
///
/// # Returns
///
/// `true` when the primitive's stroked geometry is fully contained in the frame's clearance inset.
fn symbol_primitive_inside_frame(
    primitive: &crate::generic::SchematicPrimitiveFact,
    frame_bounds: (f64, f64, f64, f64),
    clearance: f64,
) -> bool {
    let stroke_margin = if primitive.stroke_width_mm.is_finite() {
        primitive.stroke_width_mm.abs() * HALF_FACTOR
    } else {
        0.0
    };
    let margin = clearance + stroke_margin + STROKE_CLEARANCE_MARGIN_MM;
    if let Some((center_x, center_y, radius)) = primitive.circle_mm {
        let radius = radius.abs();
        return rectangle_inside_frame_with_clearance(
            (
                center_x - radius,
                center_y - radius,
                center_x + radius,
                center_y + radius,
            ),
            frame_bounds,
            margin,
        );
    }
    !primitive.points_mm.is_empty()
        && primitive
            .points_mm
            .iter()
            .all(|point| point_inside_frame_with_clearance(frame_bounds, *point, margin))
}
/// Resolves the connected wire components that contain the group's member pins and returns the corresponding pin-to-component map.
///
/// # Arguments
///
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
///
/// # Returns
///
/// Mappings from normalized pin coordinates to their physical wire-component identifiers, together with the related component lookup needed by the caller.
fn group_wire_components(
    group: &GroupIntent,
    facts: &SchematicFacts,
) -> (BTreeMap<String, usize>, BTreeSet<usize>) {
    let component_ids = physical_wire_component_ids(facts);
    let member_components = facts
        .pins
        .iter()
        .filter(|pin| group.members.contains(&pin.reference))
        .filter(|pin| point_is_on_wire(pin.position_mm, facts))
        .filter_map(|pin| {
            let key = format!(
                "{:.*}:{:.*}",
                COORDINATE_KEY_DECIMAL_PLACES,
                pin.position_mm.0,
                COORDINATE_KEY_DECIMAL_PLACES,
                pin.position_mm.1
            );
            component_ids.get(&key).copied()
        })
        .collect();
    (component_ids, member_components)
}
/// Checks whether a wire segment belongs to one of the physical components reached from the group's member pins.
///
/// # Arguments
///
/// * `wire` — Parsed wire segment being tested for membership in the group's connected components.
/// * `component_ids` — Map from tolerance-normalized schematic coordinates to physical wire-component identifiers.
/// * `member_components` — Physical wire-component identifiers reached from the group's member pins.
///
/// # Returns
///
/// `true` when the wire's connected component is among the components reached from the group pins.
fn wire_belongs_to_group(
    wire: &crate::generic::SchematicWireFact,
    component_ids: &BTreeMap<String, usize>,
    member_components: &BTreeSet<usize>,
) -> bool {
    [
        (wire.start_mm.0, wire.start_mm.1),
        (wire.end_mm.0, wire.end_mm.1),
    ]
    .iter()
    .filter_map(|(x, y)| component_ids.get(&format!("{x:.3}:{y:.3}")))
    .any(|component| member_components.contains(component))
}
/// Checks that every wire segment in the group's physical components remains inside the referenced frame.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
///
/// # Returns
///
/// Diagnostics for wire segments in the group's physical components that extend outside the referenced frame.
fn validate_group_wire_containment(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
) -> Vec<Diagnostic> {
    let (component_ids, member_components) = group_wire_components(group, facts);
    facts
        .wires
        .iter()
        .filter(|wire| {
            wire_belongs_to_group(wire, &component_ids, &member_components)
                || wire_overlaps_frame(wire, frame_bounds)
        })
        .filter(|wire| {
            !point_inside_frame(frame_bounds, wire.start_mm)
                || !point_inside_frame(frame_bounds, wire.end_mm)
        })
        .map(|wire| {
            Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch009,
                Severity::Error,
                format!(
                    "group {group_id} member wire segment from ({:.2}, {:.2}) to ({:.2}, {:.2}) extends outside frame {frame_uuid}",
                    wire.start_mm.0, wire.start_mm.1, wire.end_mm.0, wire.end_mm.1
                ),
            )
        })
        .collect()
}
/// Checks that labels attached to the group's physical wire components lie inside the referenced frame.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
///
/// # Returns
///
/// Diagnostics for labels attached to the group's physical wire components when their anchors lie outside the frame.
fn validate_group_label_containment(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
) -> Vec<Diagnostic> {
    let (component_ids, member_components) = group_wire_components(group, facts);
    facts
        .labels
        .iter()
        .filter(|label| {
            let key = format!(
                "{:.*}:{:.*}",
                COORDINATE_KEY_DECIMAL_PLACES,
                label.position_mm.0,
                COORDINATE_KEY_DECIMAL_PLACES,
                label.position_mm.1
            );
            let member_wire_label = component_ids
                .get(&key)
                .is_some_and(|component| member_components.contains(component));
            member_wire_label
                || point_inside_frame(frame_bounds, label.position_mm)
                || label
                    .bounds_mm
                    .is_some_and(|bounds| rectangles_overlap(bounds, frame_bounds))
        })
        .filter(|label| {
            label.bounds_mm.map_or_else(
                || !point_inside_frame(frame_bounds, label.position_mm),
                |bounds| !rectangle_inside_frame(bounds, frame_bounds),
            )
        })
        .map(|label| {
            Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch009,
                Severity::Error,
                format!(
                    "group {group_id} member wire label {} {:?} extends outside frame {frame_uuid}",
                    label.kind, label.name
                ),
            )
        })
        .collect()
}
/// Checks that annotation text boxes associated with group members lie inside the referenced frame.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
///
/// # Returns
///
/// Diagnostics for associated annotation text boxes that overlap the frame but are not fully contained by it.
fn validate_group_annotation_box_containment(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
) -> Vec<Diagnostic> {
    facts
        .text_boxes
        .iter()
        .filter(|text_box| text_box.uuid != group.description_uuid)
        .filter_map(|text_box| {
            let bounds = text_box.bounds_mm?;
            (rectangles_overlap(bounds, frame_bounds)
                && !rectangle_inside_frame(bounds, frame_bounds))
            .then(|| {
                Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch009,
                    Severity::Error,
                    format!(
                        "group {group_id} annotation text box {} extends outside frame {frame_uuid}",
                        text_box.uuid
                    ),
                )
            })
        })
        .collect()
}
/// Checks that standalone schematic text lies inside the referenced frame.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
///
/// # Returns
///
/// Diagnostics for standalone schematic text that overlaps the frame but extends beyond its edges.
fn validate_group_plain_text_containment(
    group_id: &str,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
) -> Vec<Diagnostic> {
    facts
        .texts
        .iter()
        .filter_map(|text| {
            let bounds = text.bounds_mm?;
            (rectangles_overlap(bounds, frame_bounds)
                && !rectangle_inside_frame(bounds, frame_bounds))
            .then(|| {
                Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch009,
                    Severity::Error,
                    format!(
                        "group {group_id} plain text annotation {:?} extends outside frame {frame_uuid}",
                        text.text
                    ),
                )
            })
        })
        .collect()
}
/// Checks that sheet-level graphical primitives lie inside the referenced frame.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
///
/// # Returns
///
/// Diagnostics for sheet-level graphic bounds that overlap the frame but are not fully contained.
fn validate_group_graphic_containment(
    group_id: &str,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
) -> Vec<Diagnostic> {
    facts
        .graphics
        .iter()
        .filter(|graphic| graphic.uuid.as_deref() != Some(frame_uuid))
        .filter_map(|graphic| {
            let bounds = graphic.bounds_mm?;
            (rectangles_overlap(bounds, frame_bounds)
                && !rectangle_inside_frame(bounds, frame_bounds))
            .then(|| {
                Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch009,
                    Severity::Error,
                    format!(
                        "group {group_id} vector graphic {} {:?} extends outside frame {frame_uuid}",
                        graphic.kind, graphic.uuid
                    ),
                )
            })
        })
        .collect()
}
/// Checks that bitmap images associated with the group lie inside the referenced frame.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
///
/// # Returns
///
/// Diagnostics for bitmap image bounds that overlap the frame but extend beyond its edges.
fn validate_group_image_containment(
    group_id: &str,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
) -> Vec<Diagnostic> {
    facts
        .images
        .iter()
        .filter_map(|image| {
            let bounds = image.bounds_mm?;
            (rectangles_overlap(bounds, frame_bounds)
                && !rectangle_inside_frame(bounds, frame_bounds))
            .then(|| {
                Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch009,
                    Severity::Error,
                    format!(
                        "group {group_id} bitmap image {:?} extends outside frame {frame_uuid}",
                        image.uuid
                    ),
                )
            })
        })
        .collect()
}
/// Tests whether a wire segment intersects or crosses the supplied frame boundary.
///
/// # Arguments
///
/// * `wire` — Parsed wire segment being tested for membership in the group's connected components.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
///
/// # Returns
///
/// `true` when the wire segment intersects the frame rectangle or its boundary.
fn wire_overlaps_frame(
    wire: &crate::generic::SchematicWireFact,
    frame_bounds: (f64, f64, f64, f64),
) -> bool {
    let (x1, y1) = wire.start_mm;
    let (x2, y2) = wire.end_mm;
    let (left, top, right, bottom) = frame_bounds;
    if (y1 - y2).abs() < GEOMETRY_TOLERANCE_MM {
        y1 >= top - GEOMETRY_TOLERANCE_MM
            && y1 <= bottom + GEOMETRY_TOLERANCE_MM
            && x1.min(x2) <= right + GEOMETRY_TOLERANCE_MM
            && x1.max(x2) >= left - GEOMETRY_TOLERANCE_MM
    } else if (x1 - x2).abs() < GEOMETRY_TOLERANCE_MM {
        x1 >= left - GEOMETRY_TOLERANCE_MM
            && x1 <= right + GEOMETRY_TOLERANCE_MM
            && y1.min(y2) <= bottom + GEOMETRY_TOLERANCE_MM
            && y1.max(y2) >= top - GEOMETRY_TOLERANCE_MM
    } else {
        rectangles_overlap(
            (x1.min(x2), y1.min(y2), x1.max(x2), y1.max(y2)),
            frame_bounds,
        )
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
/// Checks that group members and their associated schematic content are inset from the referenced frame by the requested clearance.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
///
/// # Returns
///
/// Diagnostics for group members or associated schematic objects that do not maintain the configured inset from the frame edges.
fn validate_frame_clearance(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    let Some(clearance) = group.minimum_frame_clearance_mm else {
        return Vec::new();
    };
    if !clearance.is_finite() || clearance < 0.0 {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch012,
            Severity::Error,
            format!("group {group_id} minimum_frame_clearance_mm must be finite and non-negative"),
        )];
    }
    let Some(frame_uuid) = group.frame_uuid.as_deref() else {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch012,
            Severity::Error,
            format!("group {group_id} requests frame clearance but has no frame_uuid"),
        )];
    };
    let Some(frame) = facts.frames.iter().find(|frame| frame.uuid == frame_uuid) else {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch012,
            Severity::Error,
            format!("group {group_id} references missing schematic frame UUID {frame_uuid}"),
        )];
    };
    let bounds = frame.bounds_mm;
    if !valid_frame_bounds(bounds) {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch012,
            Severity::Error,
            format!("group {group_id} frame {frame_uuid} has invalid or zero-area bounds"),
        )];
    }
    let (left, top, right, bottom) = bounds;
    if right - left < TWO_SIDED_FACTOR * clearance || bottom - top < TWO_SIDED_FACTOR * clearance
    {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch012,
            Severity::Error,
            format!(
                "group {group_id} frame {frame_uuid} is too small to provide {clearance:.2} mm clearance on every side"
            ),
        )];
    }
    let mut diagnostics =
        validate_member_frame_clearance(group_id, group, facts, frame_uuid, bounds, clearance);
    diagnostics.extend(validate_member_pin_frame_clearance(
        group_id, group, facts, frame_uuid, bounds, clearance,
    ));
    diagnostics.extend(validate_member_field_frame_clearance(
        group_id, group, facts, frame_uuid, bounds, clearance,
    ));
    diagnostics.extend(validate_description_frame_clearance(
        group_id, group, facts, frame_uuid, bounds, clearance,
    ));
    diagnostics.extend(validate_group_symbol_primitive_clearance(
        group_id, group, facts, frame_uuid, bounds, clearance,
    ));
    diagnostics.extend(validate_group_wire_clearance(
        group_id, group, facts, frame_uuid, bounds, clearance,
    ));
    diagnostics.extend(validate_group_label_clearance(
        group_id, group, facts, frame_uuid, bounds, clearance,
    ));
    diagnostics.extend(validate_group_annotation_box_clearance(
        group_id, group, facts, frame_uuid, bounds, clearance,
    ));
    diagnostics.extend(validate_group_plain_text_clearance(
        group_id, facts, frame_uuid, bounds, clearance,
    ));
    diagnostics.extend(validate_group_graphic_clearance(
        group_id, facts, frame_uuid, bounds, clearance,
    ));
    diagnostics.extend(validate_group_image_clearance(
        group_id, facts, frame_uuid, bounds, clearance,
    ));
    diagnostics
}
/// Checks that all four frame coordinates are finite and that the rectangle has positive width and height.
///
/// # Returns
///
/// `true` when all coordinates are finite and the frame has positive width and height.
fn valid_frame_bounds((left, top, right, bottom): (f64, f64, f64, f64)) -> bool {
    left < right
        && top < bottom
        && [left, top, right, bottom]
            .iter()
            .all(|value| value.is_finite())
}
/// Checks each group member's symbol origin against the minimum clearance inset from the frame edges.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
/// * `clearance` — Minimum required distance from each frame edge, in millimetres.
///
/// # Returns
///
/// Diagnostics when a group member's symbol origin is closer to a frame edge than the declared minimum clearance.
fn validate_member_frame_clearance(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    frame_uuid: &str,
    bounds: (f64, f64, f64, f64),
    clearance: f64,
) -> Vec<Diagnostic> {
    let (left, top, right, bottom) = bounds;
    let mut diagnostics = Vec::new();
    for reference in &group.members {
        let Some(component) = facts
            .components
            .iter()
            .find(|component| component.reference == *reference)
        else {
            continue;
        };
        let Some((x_mm, y_mm)) = component.position_mm else {
            diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch012,
                Severity::Error,
                format!(
                    "group {group_id} member {reference} has no symbol origin for frame-clearance validation"
                ),
            ));
            continue;
        };
        let inside_clearance = x_mm >= left + clearance - GEOMETRY_TOLERANCE_MM
            && x_mm <= right - clearance + GEOMETRY_TOLERANCE_MM
            && y_mm >= top + clearance - GEOMETRY_TOLERANCE_MM
            && y_mm <= bottom - clearance + GEOMETRY_TOLERANCE_MM;
        if !inside_clearance {
            diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch012,
                Severity::Error,
                format!(
                    "group {group_id} member {reference} origin is within {clearance:.2} mm of frame {frame_uuid} edge"
                ),
            ));
        }
    }
    diagnostics
}
/// Checks every placed pin on a group member against the minimum frame-edge clearance.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
/// * `clearance` — Minimum required distance from each frame edge, in millimetres.
///
/// # Returns
///
/// Diagnostics when a placed pin belonging to a group member lies inside the required frame-edge clearance zone.
fn validate_member_pin_frame_clearance(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    frame_uuid: &str,
    bounds: (f64, f64, f64, f64),
    clearance: f64,
) -> Vec<Diagnostic> {
    let (left, top, right, bottom) = bounds;
    let mut diagnostics = Vec::new();
    for pin in facts
        .pins
        .iter()
        .filter(|pin| group.members.contains(&pin.reference))
    {
        let (x_mm, y_mm) = pin.position_mm;
        let inside_clearance = x_mm >= left + clearance - GEOMETRY_TOLERANCE_MM
            && x_mm <= right - clearance + GEOMETRY_TOLERANCE_MM
            && y_mm >= top + clearance - GEOMETRY_TOLERANCE_MM
            && y_mm <= bottom - clearance + GEOMETRY_TOLERANCE_MM;
        if !inside_clearance {
            diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch012,
                Severity::Error,
                format!(
                    "group {group_id} member pin {}.{} does not maintain {clearance:.2} mm clearance from frame {frame_uuid}",
                    pin.reference, pin.number
                ),
            ));
        }
    }
    diagnostics
}
/// Checks the visible reference and value fields of group members against the minimum frame-edge clearance.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
/// * `clearance` — Minimum required distance from each frame edge, in millimetres.
///
/// # Returns
///
/// Diagnostics when a visible reference or value field lies inside the required frame-edge clearance zone.
fn validate_member_field_frame_clearance(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    frame_uuid: &str,
    bounds: (f64, f64, f64, f64),
    clearance: f64,
) -> Vec<Diagnostic> {
    let (left, top, right, bottom) = bounds;
    let mut diagnostics = Vec::new();
    for field in facts
        .visible_fields
        .iter()
        .filter(|field| group.members.contains(&field.reference))
    {
        let (field_left, field_top, field_right, field_bottom) = field.bounds_mm;
        let inside_clearance = field_left >= left + clearance - GEOMETRY_TOLERANCE_MM
            && field_top >= top + clearance - GEOMETRY_TOLERANCE_MM
            && field_right <= right - clearance + GEOMETRY_TOLERANCE_MM
            && field_bottom <= bottom - clearance + GEOMETRY_TOLERANCE_MM;
        if !inside_clearance {
            diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch012,
                Severity::Error,
                format!(
                    "group {group_id} member field {} {} does not maintain {clearance:.2} mm clearance from frame {frame_uuid}",
                    field.reference, field.name
                ),
            ));
        }
    }
    diagnostics
}
/// Checks that stroked symbol-library geometry stays the requested distance from the frame edges.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
/// * `clearance` — Minimum required distance from each frame edge, in millimetres.
///
/// # Returns
///
/// Diagnostics for stroked symbol primitives that approach the frame edges more closely than the declared clearance.
fn validate_group_symbol_primitive_clearance(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
    clearance: f64,
) -> Vec<Diagnostic> {
    facts
        .symbol_primitives
        .iter()
        .filter(|primitive| group.members.contains(&primitive.reference))
        .filter(|primitive| !symbol_primitive_inside_frame(primitive, frame_bounds, clearance))
        .map(|primitive| {
            Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch012,
                Severity::Error,
                format!(
                    "group {group_id} member {} symbol primitive does not maintain {clearance:.2} mm clearance from frame {frame_uuid}",
                    primitive.reference
                ),
            )
        })
        .collect()
}
/// Checks that wire segments belonging to the group stay the requested distance from the frame edges.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
/// * `clearance` — Minimum required distance from each frame edge, in millimetres.
///
/// # Returns
///
/// Diagnostics for group wire segments that enter the required frame-edge clearance zone.
fn validate_group_wire_clearance(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
    clearance: f64,
) -> Vec<Diagnostic> {
    let (component_ids, member_components) = group_wire_components(group, facts);
    facts
        .wires
        .iter()
        .filter(|wire| {
            wire_belongs_to_group(wire, &component_ids, &member_components)
                || wire_overlaps_frame(wire, frame_bounds)
        })
        .filter(|wire| {
            !point_inside_frame_with_clearance(frame_bounds, wire.start_mm, clearance)
                || !point_inside_frame_with_clearance(frame_bounds, wire.end_mm, clearance)
        })
        .map(|wire| {
            Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch012,
                Severity::Error,
                format!(
                    "group {group_id} member wire segment from ({:.2}, {:.2}) to ({:.2}, {:.2}) does not maintain {clearance:.2} mm clearance from frame {frame_uuid}",
                    wire.start_mm.0, wire.start_mm.1, wire.end_mm.0, wire.end_mm.1
                ),
            )
        })
        .collect()
}
/// Checks that labels attached to group wires stay the requested distance from the frame edges.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
/// * `clearance` — Minimum required distance from each frame edge, in millimetres.
///
/// # Returns
///
/// Diagnostics for labels attached to the group's wire components that violate the frame-edge clearance.
fn validate_group_label_clearance(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
    clearance: f64,
) -> Vec<Diagnostic> {
    let (component_ids, member_components) = group_wire_components(group, facts);
    facts
        .labels
        .iter()
        .filter(|label| {
            let key = format!("{:.*}:{:.*}", COORDINATE_KEY_DECIMAL_PLACES, label.position_mm.0, COORDINATE_KEY_DECIMAL_PLACES, label.position_mm.1);
            let member_wire_label = component_ids
                .get(&key)
                .is_some_and(|component| member_components.contains(component));
            member_wire_label
                || point_inside_frame(frame_bounds, label.position_mm)
                || label
                    .bounds_mm
                    .is_some_and(|bounds| rectangles_overlap(bounds, frame_bounds))
        })
        .filter(|label| {
            label.bounds_mm.map_or_else(
                || !point_inside_frame_with_clearance(frame_bounds, label.position_mm, clearance),
                |bounds| !rectangle_inside_frame_with_clearance(bounds, frame_bounds, clearance),
            )
        })
        .map(|label| {
            Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch012,
                Severity::Error,
                format!(
                    "group {group_id} member wire label {} {:?} does not maintain {clearance:.2} mm clearance from frame {frame_uuid}",
                    label.kind, label.name
                ),
            )
        })
        .collect()
}
/// Checks that group annotation boxes stay the requested distance from the frame edges.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
/// * `clearance` — Minimum required distance from each frame edge, in millimetres.
///
/// # Returns
///
/// Diagnostics for annotation text boxes that do not preserve the configured distance from frame edges.
fn validate_group_annotation_box_clearance(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
    clearance: f64,
) -> Vec<Diagnostic> {
    facts
        .text_boxes
        .iter()
        .filter(|text_box| text_box.uuid != group.description_uuid)
        .filter_map(|text_box| {
            let bounds = text_box.bounds_mm?;
            (rectangles_overlap(bounds, frame_bounds)
                && !rectangle_inside_frame_with_clearance(bounds, frame_bounds, clearance))
            .then(|| {
                Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch012,
                    Severity::Error,
                    format!(
                        "group {group_id} annotation text box {} does not maintain {clearance:.2} mm clearance from frame {frame_uuid}",
                        text_box.uuid
                    ),
                )
            })
        })
        .collect()
}
/// Checks that standalone text stays the requested distance from the frame edges.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
/// * `clearance` — Minimum required distance from each frame edge, in millimetres.
///
/// # Returns
///
/// Diagnostics for standalone text whose bounds do not preserve the configured distance from frame edges.
fn validate_group_plain_text_clearance(
    group_id: &str,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
    clearance: f64,
) -> Vec<Diagnostic> {
    facts
        .texts
        .iter()
        .filter_map(|text| {
            let bounds = text.bounds_mm?;
            (rectangles_overlap(bounds, frame_bounds)
                && !rectangle_inside_frame_with_clearance(bounds, frame_bounds, clearance))
            .then(|| {
                Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch012,
                    Severity::Error,
                    format!(
                        "group {group_id} plain text annotation {:?} does not maintain {clearance:.2} mm clearance from frame {frame_uuid}",
                        text.text
                    ),
                )
            })
        })
        .collect()
}
/// Checks that sheet graphics stay the requested distance from the frame edges.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
/// * `clearance` — Minimum required distance from each frame edge, in millimetres.
///
/// # Returns
///
/// Diagnostics for sheet-level graphics whose stroked bounds enter the configured frame-edge clearance zone.
fn validate_group_graphic_clearance(
    group_id: &str,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
    clearance: f64,
) -> Vec<Diagnostic> {
    facts
        .graphics
        .iter()
        .filter(|graphic| graphic.uuid.as_deref() != Some(frame_uuid))
        .filter_map(|graphic| {
            let bounds = graphic.bounds_mm?;
            (rectangles_overlap(bounds, frame_bounds)
                && !rectangle_inside_frame_with_clearance(bounds, frame_bounds, clearance))
            .then(|| {
                Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch012,
                    Severity::Error,
                    format!(
                        "group {group_id} vector graphic {} {:?} does not maintain {clearance:.2} mm clearance from frame {frame_uuid}",
                        graphic.kind, graphic.uuid
                    ),
                )
            })
        })
        .collect()
}
/// Checks that bitmap image bounds stay the requested distance from the frame edges.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
/// * `clearance` — Minimum required distance from each frame edge, in millimetres.
///
/// # Returns
///
/// Diagnostics for bitmap image bounds that do not preserve the configured distance from frame edges.
fn validate_group_image_clearance(
    group_id: &str,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
    clearance: f64,
) -> Vec<Diagnostic> {
    facts
        .images
        .iter()
        .filter_map(|image| {
            let bounds = image.bounds_mm?;
            (rectangles_overlap(bounds, frame_bounds)
                && !rectangle_inside_frame_with_clearance(bounds, frame_bounds, clearance))
            .then(|| {
                Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch012,
                    Severity::Error,
                    format!(
                        "group {group_id} bitmap image {:?} does not maintain {clearance:.2} mm clearance from frame {frame_uuid}",
                        image.uuid
                    ),
                )
            })
        })
        .collect()
}
/// Checks that the group's referenced description text box remains inside the sheet frame with the required clearance on all four sides. A missing description box is treated as absent metadata; a present box without usable geometry produces an SCH012 diagnostic.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
/// * `clearance` — Minimum required distance from each frame edge, in millimetres.
///
/// # Returns
///
/// SCH012 diagnostics when the referenced description box lacks usable bounds or fails the required inset on any frame edge.
fn validate_description_frame_clearance(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    frame_uuid: &str,
    bounds: (f64, f64, f64, f64),
    clearance: f64,
) -> Vec<Diagnostic> {
    let (left, top, right, bottom) = bounds;
    let mut diagnostics = Vec::new();
    if let Some(text_box) = facts
        .text_boxes
        .iter()
        .find(|text_box| text_box.uuid == group.description_uuid)
    {
        if let Some((box_left, box_top, box_right, box_bottom)) = text_box.bounds_mm {
            let text_inside_clearance = box_left >= left + clearance - GEOMETRY_TOLERANCE_MM
                && box_top >= top + clearance - GEOMETRY_TOLERANCE_MM
                && box_right <= right - clearance + GEOMETRY_TOLERANCE_MM
                && box_bottom <= bottom - clearance + GEOMETRY_TOLERANCE_MM;
            if !text_inside_clearance {
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch012,
                    Severity::Error,
                    format!(
                        "group {group_id} description text box {} does not maintain {clearance:.2} mm clearance from frame {frame_uuid}",
                        text_box.uuid
                    ),
                ));
            }
        } else {
            diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch012,
                Severity::Error,
                format!(
                    "group {group_id} description text box {} has no usable geometry for frame-clearance validation",
                    text_box.uuid
                ),
            ));
        }
    }
    diagnostics
}
/// Checks whether the total physical wire length of a group stays within its declared length budget.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
///
/// # Returns
///
/// Diagnostics when the wire-length budget is invalid, the frame cannot be resolved, or the group's physical wire length exceeds its budget.
fn validate_wire_compactness(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    let Some(budget) = group.wire_length_budget_mm else {
        return Vec::new();
    };
    if !budget.is_finite() || budget <= 0.0 {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch016,
            Severity::Error,
            format!("group {group_id} wire_length_budget_mm must be a finite positive distance"),
        )];
    }
    let Some(frame_uuid) = group.frame_uuid.as_deref() else {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch016,
            Severity::Error,
            format!("group {group_id} must set frame_uuid to calculate its internal wire length"),
        )];
    };
    let Some(frame) = facts.frames.iter().find(|frame| frame.uuid == frame_uuid) else {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch016,
            Severity::Error,
            format!(
                "group {group_id} references missing schematic frame UUID {frame_uuid} for wire compactness"
            ),
        )];
    };
    let (left, top, right, bottom) = frame.bounds_mm;
    if left >= right || top >= bottom {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch016,
            Severity::Error,
            format!("group {group_id} frame {frame_uuid} has invalid or zero-area bounds"),
        )];
    }

    let inside = |point: (f64, f64)| {
        point.0 >= left - GEOMETRY_TOLERANCE_MM
            && point.0 <= right + GEOMETRY_TOLERANCE_MM
            && point.1 >= top - GEOMETRY_TOLERANCE_MM
            && point.1 <= bottom + GEOMETRY_TOLERANCE_MM
    };
    let total_wire_length_mm: f64 = facts
        .wires
        .iter()
        .filter(|wire| inside(wire.start_mm) && inside(wire.end_mm))
        .map(|wire| (wire.end_mm.0 - wire.start_mm.0).hypot(wire.end_mm.1 - wire.start_mm.1))
        .sum();
    let score = total_wire_length_mm / budget;
    if score > MAX_COMPACTNESS_SCORE + SCORE_COMPARISON_EPSILON {
        vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch016,
            Severity::Warning,
            format!(
                "group {group_id} wire compactness score is {score:.3} ({total_wire_length_mm:.2}/{budget:.2} mm); maximum score is 1.000"
            ),
        )]
    } else {
        Vec::new()
    }
}
/// Checks that a group's member symbols satisfy the spacing constraints declared by the profile.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
///
/// # Returns
///
/// Diagnostics when the requested minimum spacing is invalid or placed group-member origins are closer than that spacing.
fn validate_group_spacing(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    let Some(minimum_spacing) = group.minimum_member_origin_spacing_mm else {
        return Vec::new();
    };
    if !minimum_spacing.is_finite() || minimum_spacing <= 0.0 {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch006,
            Severity::Error,
            format!(
                "group {group_id} minimum_member_origin_spacing_mm must be a finite positive distance"
            ),
        )];
    }

    let mut positions = Vec::new();
    let mut diagnostics = Vec::new();
    for reference in &group.members {
        let Some(component) = facts
            .components
            .iter()
            .find(|component| component.reference == *reference)
        else {
            continue;
        };
        let Some((x_mm, y_mm)) = component.position_mm else {
            diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch006,
                Severity::Error,
                format!(
                    "group {group_id} member {reference} has no symbol-origin position for spacing validation"
                ),
            ));
            continue;
        };
        positions.push((reference.as_str(), x_mm, y_mm));
    }

    for (index, first) in positions.iter().enumerate() {
        for second in positions.iter().skip(index + 1) {
            let actual_spacing = (first.1 - second.1).hypot(first.2 - second.2);
            if actual_spacing + GEOMETRY_TOLERANCE_MM < minimum_spacing {
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch006,
                    Severity::Error,
                    format!(
                        "group {group_id} members {} and {} have symbol-origin spacing {:.2} mm; minimum is {:.2} mm",
                        first.0, second.0, actual_spacing, minimum_spacing
                    ),
                ));
            }
        }
    }
    diagnostics
}
/// Checks that the profile's member references match the schematic's explicit KiCad group membership.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
///
/// # Returns
///
/// Diagnostics when the schematic's native KiCad group is missing or its member UUIDs do not match the profile references.
fn validate_kicad_group_membership(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    let Some(expected_uuid) = group.kicad_group_uuid.as_deref() else {
        return Vec::new();
    };
    let Some(native_group) = facts.groups.iter().find(|item| item.uuid == expected_uuid) else {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch007,
            Severity::Error,
            format!("profile group {group_id} references missing KiCad group UUID {expected_uuid}"),
        )];
    };

    let mut diagnostics = Vec::new();
    for reference in &group.members {
        let Some(component) = facts
            .components
            .iter()
            .find(|component| component.reference == *reference)
        else {
            continue;
        };
        let Some(uuid) = component.uuid.as_deref() else {
            diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch007,
                Severity::Error,
                format!(
                    "group {group_id} member {reference} has no placed-symbol UUID for native group membership validation"
                ),
            ));
            continue;
        };
        if !native_group.member_uuids.contains(uuid) {
            diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch007,
                Severity::Error,
                format!(
                    "group {group_id} includes {reference} in its profile but native KiCad group {} does not contain symbol UUID {uuid}",
                    native_group.name
                ),
            ));
        }
    }

    for component in &facts.components {
        let Some(uuid) = component.uuid.as_deref() else {
            continue;
        };
        if native_group.member_uuids.contains(uuid) && !group.members.contains(&component.reference)
        {
            diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch007,
                Severity::Error,
                format!(
                    "native KiCad group {} contains component {} which is not listed in profile group {group_id}",
                    native_group.name, component.reference
                ),
            ));
        }
    }
    diagnostics
}
/// Checks that the group contains the evidence required by its declared evidence level.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
///
/// # Returns
///
/// Diagnostics when the group's evidence does not satisfy the required document, page, or reference fields for its evidence level.
fn validate_group_evidence(group_id: &str, group: &GroupIntent) -> Vec<Diagnostic> {
    let evidence = group.evidence.as_ref();
    let has_document_evidence = evidence.is_some_and(|item| {
        item.document
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
            && item.page.is_some()
            && item
                .figure
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
    });
    let has_reviewed_asset = evidence
        .and_then(|item| item.screenshot_asset.as_deref())
        .is_some_and(|value| !value.trim().is_empty());
    if has_document_evidence || has_reviewed_asset {
        Vec::new()
    } else {
        vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch017,
            Severity::Error,
            format!(
                "group {group_id} must cite a document/page/figure or a reviewed screenshot asset"
            ),
        )]
    }
}
/// Checks that every reference listed as a group member exists in the schematic.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `references` — Set of schematic reference designators used to verify profile membership.
///
/// # Returns
///
/// Diagnostics for group member references that are not present among the placed schematic components.
fn validate_group_members(
    group_id: &str,
    group: &GroupIntent,
    references: &BTreeSet<&str>,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for reference in &group.members {
        if !references.contains(reference.as_str()) {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch017,
                Severity::Error,
                format!("group {group_id} contains missing component {reference}"),
            ));
        }
    }
    for reference in &group.documentation_members {
        if !group.members.contains(reference) {
            diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch020,
                Severity::Error,
                format!(
                    "group {group_id} lists {reference} as a documentation member but not as a group member"
                ),
            ));
        }
    }
    diagnostics
}
/// Checks that member component roles and purposes are consistent with the group's description.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `description` — Description text used to verify declared group/component purpose phrases.
///
/// # Returns
///
/// Diagnostics when a purpose refers to a non-member component or its function text is missing from the group description.
fn validate_group_component_purposes(
    group_id: &str,
    group: &GroupIntent,
    description: &str,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for (reference, purpose) in &group.component_purposes {
        if !group.members.contains(reference) {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch020,
                Severity::Error,
                format!("group {group_id} documents {reference}, which is not a group member"),
            ));
        }
        if !group.documentation_members.contains(reference) {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch020,
                Severity::Error,
                format!("group {group_id} must list {reference} as a documentation member"),
            ));
        }
        validate_group_phrase(
            &mut diagnostics,
            group_id,
            reference,
            "function",
            purpose.function.as_deref(),
            description,
        );
        validate_group_phrase(
            &mut diagnostics,
            group_id,
            reference,
            "rationale",
            purpose.rationale.as_deref(),
            description,
        );
    }
    diagnostics
}
/// Adds a diagnostic when a component's descriptive text does not contain a required role or purpose phrase.
///
/// # Arguments
///
/// * `diagnostics` — Mutable diagnostic vector that receives any failed phrase checks.
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `reference` — Reference designator of the component whose intent is being checked.
/// * `field` — Name of the component field being checked, such as its reference or value field.
/// * `phrase` — Required phrase expected to appear in the component's descriptive text.
/// * `description` — Description text used to verify declared group/component purpose phrases.
fn validate_group_phrase(
    diagnostics: &mut Vec<Diagnostic>,
    group_id: &str,
    reference: &str,
    field: &str,
    phrase: Option<&str>,
    description: &str,
) {
    let Some(phrase) = phrase.filter(|value| !value.trim().is_empty()) else {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch020,
            Severity::Error,
            format!("group {group_id} must provide {reference} {field}"),
        ));
        return;
    };
    if !description.contains(phrase) {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch020,
            Severity::Error,
            format!("group {group_id} description must include {reference} {field}"),
        ));
    }
}
/// Validates the declared kind, role, purpose, pin contract, and any decoupling intent for one component.
///
/// # Arguments
///
/// * `reference` — Reference designator of the component whose intent is being checked.
/// * `intent` — Component-intent record declaring the electrical role and connection contract.
/// * `netlist` — Optional KiCad-exported flattened netlist; supplies logical pin-to-net assignments when present.
///
/// # Returns
///
/// Diagnostics for missing component function/rationale, mismatched pin nets, invalid role connections, or incomplete decoupling intent.
fn validate_component_intent(
    reference: &str,
    intent: &ComponentIntent,
    netlist: Option<&Netlist>,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    if intent.function.as_deref().is_none_or(str::is_empty)
        || intent.rationale.as_deref().is_none_or(str::is_empty)
    {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch020,
            Severity::Error,
            format!("{reference} must document both its circuit function and design rationale"),
        ));
    }
    if intent.kind == ComponentKind::Capacitor && intent.role.0 == "decoupling" {
        diagnostics.extend(validate_decoupling_intent(
            reference,
            intent.decoupling.as_ref(),
            netlist,
        ));
    }
    diagnostics.extend(validate_pin_net_contract(reference, intent, netlist));
    diagnostics.extend(validate_role_contract(reference, intent));
    if intent.kind == ComponentKind::Capacitor
        && !matches!(
            intent.role.0.as_str(),
            "decoupling"
                | "bulk"
                | "filter"
                | "timing"
                | "coupling"
                | "snubber"
                | "reservoir"
                | "safety"
        )
    {
        diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch021,
            Severity::Warning,
            format!(
                "{reference} uses unknown capacitor role {:?}; document its meaning or use a standard role",
                intent.role.0
            ),
        ));
    }
    diagnostics
}
/// Checks the declared pin-to-net expectations against the exported logical netlist.
///
/// # Arguments
///
/// * `reference` — Reference designator of the component whose intent is being checked.
/// * `intent` — Component-intent record declaring the electrical role and connection contract.
/// * `netlist` — Optional KiCad-exported flattened netlist; supplies logical pin-to-net assignments when present.
///
/// # Returns
///
/// Diagnostics when a declared pin-to-net mapping is missing from or disagrees with the exported netlist.
fn validate_pin_net_contract(
    reference: &str,
    intent: &ComponentIntent,
    netlist: Option<&Netlist>,
) -> Vec<Diagnostic> {
    let mut mappings = intent.pin_nets.clone();
    let mut diagnostics = Vec::new();
    for (key, expected_net) in &intent.connections {
        let Some(pin) = key.strip_prefix("pin.") else {
            continue;
        };
        if let Some(previous) = mappings.insert(pin.to_owned(), expected_net.clone()) {
            if previous != *expected_net {
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch014,
                    Severity::Error,
                    format!(
                        "{reference} pin {pin} has conflicting expected nets {previous:?} and {expected_net:?}"
                    ),
                ));
            }
        }
    }
    if mappings.is_empty() {
        return diagnostics;
    }
    let Some(netlist) = netlist else {
        diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch014,
            Severity::Warning,
            format!(
                "{reference} declares pin-to-net contracts but no KiCad netlist was supplied; the contracts were not checked"
            ),
        ));
        return diagnostics;
    };

    for (pin, expected_net) in mappings {
        if pin.trim().is_empty() || expected_net.trim().is_empty() {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch014,
                Severity::Error,
                format!("{reference} pin-to-net mapping must use non-empty pin and net names"),
            ));
            continue;
        }
        match netlist.net_for_pin(reference, &pin) {
            Some(actual_net) if actual_net == expected_net => {}
            Some(actual_net) => diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch014,
                Severity::Error,
                format!(
                    "{reference}.{pin} is connected to {actual_net}, but the profile expects {expected_net}"
                ),
            )),
            None => diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch014,
                Severity::Error,
                format!(
                    "{reference}.{pin} is absent from the exported KiCad netlist but is assigned to {expected_net} in the profile"
                ),
            )),
        }
    }
    diagnostics
}
/// Checks that the declared component role has the required electrical connections.
///
/// # Arguments
///
/// * `reference` — Reference designator of the component whose intent is being checked.
/// * `intent` — Component-intent record declaring the electrical role and connection contract.
///
/// # Returns
///
/// Diagnostics when the declared component role does not include the electrical connections required for that role.
fn validate_role_contract(reference: &str, intent: &ComponentIntent) -> Vec<Diagnostic> {
    let required_connections: &[&str] = match (intent.kind, intent.role.0.as_str()) {
        (ComponentKind::Capacitor, "bulk") => &["rail", "return"],
        (ComponentKind::Resistor, "pull_up" | "pull_down") => &["signal", "rail"],
        (ComponentKind::Resistor, "divider") => &["high", "tap", "low"],
        (ComponentKind::Resistor, "series" | "current_limit" | "current_sense") => {
            &["input", "output"]
        }
        (ComponentKind::Resistor, "gate_bias") => &["gate", "bias", "return"],
        (ComponentKind::Resistor, "discharge" | "bleeder") => &["storage", "return"],
        (ComponentKind::Diode, "esd" | "clamp") => &["protected", "return"],
        (ComponentKind::Diode, "flyback") => &["anode", "cathode", "switched_node", "return"],
        (ComponentKind::Diode, "reverse_polarity") => &["input", "protected_output"],
        (ComponentKind::Inductor | ComponentKind::FerriteBead, _) => &["input", "output"],
        (ComponentKind::Transistor, _) => &["control", "switched_high", "switched_low"],
        _ => &[],
    };
    let mut diagnostics: Vec<_> = required_connections
        .iter()
        .filter(|key| {
            intent
                .connections
                .get(**key)
                .is_none_or(|value| value.trim().is_empty())
        })
        .map(|key| {
            Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch021,
                Severity::Error,
                format!(
                    "{reference} role {:?} is missing required connection {key:?}",
                    intent.role.0
                ),
            )
        })
        .collect();

    if intent.kind == ComponentKind::Connector {
        if intent
            .attributes
            .get("interface_standard")
            .is_none_or(|value| value.trim().is_empty())
        {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch021,
                Severity::Error,
                format!("{reference} connector metadata must declare interface_standard"),
            ));
        }
        if !intent.connections.keys().any(|key| key.starts_with("pin.")) {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch021,
                Severity::Error,
                format!("{reference} connector metadata must declare at least one pin.N mapping"),
            ));
        }
    }
    diagnostics
}
/// Validates a capacitor's decoupling target and supply/return net declarations.
///
/// # Arguments
///
/// * `reference` — Reference designator of the component whose intent is being checked.
/// * `contract` — Optional decoupling contract declared for this component.
/// * `netlist` — Optional KiCad-exported flattened netlist; supplies logical pin-to-net assignments when present.
///
/// # Returns
///
/// Diagnostics when a decoupling capacitor lacks a target pin, target terminal, return terminal, or declared return net.
fn validate_decoupling_intent(
    reference: &str,
    contract: Option<&DecouplingIntent>,
    netlist: Option<&Netlist>,
) -> Vec<Diagnostic> {
    let Some(contract) = contract else {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch022,
            Severity::Error,
            format!(
                "{reference} is a decoupling capacitor but does not identify its target pin and terminal/net contract"
            ),
        )];
    };
    let mut diagnostics = Vec::new();
    if !valid_pin_ref(&contract.target_pin) {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch022,
            Severity::Error,
            format!(
                "{reference} has invalid decoupling target pin {:?}",
                contract.target_pin
            ),
        ));
    }
    if contract.target_terminal.is_empty()
        || contract.return_terminal.is_empty()
        || contract.return_net.is_empty()
        || contract.cluster.is_empty()
    {
        diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch022,
            Severity::Error,
            format!(
                "{reference} decoupling metadata must define target/return terminals, return net, and cluster"
            ),
        ));
    }
    if contract.target_terminal == contract.return_terminal {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch022,
            Severity::Error,
            format!("{reference} maps both decoupling terminals to the same capacitor terminal"),
        ));
    }
    if let Some(netlist) = netlist {
        diagnostics.extend(validate_decoupling_nets(reference, contract, netlist));
    }
    diagnostics
}
/// Checks that the decoupling capacitor connects to its declared target net and return net.
///
/// # Arguments
///
/// * `reference` — Reference designator of the component whose intent is being checked.
/// * `contract` — Optional decoupling contract declared for this component.
/// * `netlist` — Optional KiCad-exported flattened netlist; supplies logical pin-to-net assignments when present.
///
/// # Returns
///
/// Diagnostics when the capacitor's exported target or return pin resolves to a different net than its decoupling contract declares.
fn validate_decoupling_nets(
    reference: &str,
    contract: &DecouplingIntent,
    netlist: &Netlist,
) -> Vec<Diagnostic> {
    let Some((target_reference, target_pin)) = contract.target_pin.rsplit_once('.') else {
        return Vec::new();
    };
    let target_net = netlist.net_for_pin(target_reference, target_pin);
    let capacitor_target_net = netlist.net_for_pin(reference, &contract.target_terminal);
    let capacitor_return_net = netlist.net_for_pin(reference, &contract.return_terminal);
    let mut diagnostics = Vec::new();

    match (target_net, capacitor_target_net) {
        (Some(target), Some(capacitor)) if target == capacitor => {}
        (Some(target), Some(capacitor)) => diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch022,
            Severity::Error,
            format!(
                "{reference} terminal {} is on {capacitor}, but {}.{} is on {target}",
                contract.target_terminal, target_reference, target_pin
            ),
        )),
        (None, _) => diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch022,
            Severity::Error,
            format!("target pin {target_reference}.{target_pin} is absent from the exported KiCad netlist"),
        )),
        (_, None) => diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch022,
            Severity::Error,
            format!(
                "{}.{} is absent from the exported KiCad netlist",
                reference, contract.target_terminal
            ),
        )),
    }

    match capacitor_return_net {
        Some(actual) if actual == contract.return_net => {}
        Some(actual) => diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch022,
            Severity::Error,
            format!(
                "{reference} return terminal {} is on {actual}, expected {}",
                contract.return_terminal, contract.return_net
            ),
        )),
        None => diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch022,
            Severity::Error,
            format!(
                "{}.{} is absent from the exported KiCad netlist",
                reference, contract.return_terminal
            ),
        )),
    }
    diagnostics
}
/// Checks that capacitors assigned to the same decoupling target are grouped consistently.
///
/// # Arguments
///
/// * `components` — Map of component reference designators to their intent records.
///
/// # Returns
///
/// Diagnostics when capacitors assigned to one decoupling target are not grouped consistently or declare conflicting target/return nets.
fn validate_decoupler_groups(components: &BTreeMap<String, ComponentIntent>) -> Vec<Diagnostic> {
    let mut by_target: BTreeMap<&str, Vec<&DecouplingIntent>> = BTreeMap::new();
    for intent in components.values() {
        if intent.kind == ComponentKind::Capacitor && intent.role.0 == "decoupling" {
            if let Some(contract) = &intent.decoupling {
                by_target
                    .entry(&contract.target_pin)
                    .or_default()
                    .push(contract);
            }
        }
    }
    let mut diagnostics = Vec::new();
    for (target, capacitors) in by_target {
        let returns: BTreeSet<_> = capacitors
            .iter()
            .map(|item| item.return_net.as_str())
            .collect();
        let clusters: BTreeSet<_> = capacitors
            .iter()
            .map(|item| item.cluster.as_str())
            .collect();
        if returns.len() > 1 {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch022,
                Severity::Error,
                format!(
                    "decouplers for {target} disagree on their return net: {}",
                    returns.into_iter().collect::<Vec<_>>().join(", ")
                ),
            ));
        }
        if clusters.len() > 1 {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch022,
                Severity::Warning,
                format!(
                    "decouplers for {target} are split across cluster identifiers: {}",
                    clusters.into_iter().collect::<Vec<_>>().join(", ")
                ),
            ));
        }
    }
    diagnostics
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
    use std::collections::BTreeMap;

    use kicad_clippy_core::Severity;
    use kicad_clippy_erc::{Netlist, PinRef};

    use super::{ComponentKind, ComponentRole, LintProfile, ProfileMode, SchematicFacts};
    /// Builds a minimal schematic fixture with two placed one-pin symbols and appends the supplied serialized wire and label expressions.
    ///
    /// # Arguments
    ///
    /// * `wire_and_label_items` — KiCad S-expression fragments inserted into the fixture to define wires, junctions, and labels.
    ///
    /// # Returns
    ///
    /// A parsed schematic document containing U1.1, R1.1, and the supplied connectivity geometry.
    fn direct_wire_schematic(wire_and_label_items: &str) -> kicad_clippy_parser::SchematicDocument {
        kicad_clippy_parser::SchematicDocument::from_text(
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
    ) -> kicad_clippy_parser::SchematicDocument {
        let junction = if explicit_junction {
            r"(junction (at 14 10) (diameter 0) (color 0 0 0 0))"
        } else {
            ""
        };
        kicad_clippy_parser::SchematicDocument::from_text(
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
    /// Regression test: verifies that common pin group can require one exported KiCad net.
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
            item.code == "SCH004" && item.message.contains("different exported KiCad nets")
        }));
    }
    /// Regression test: verifies that common pin group rejects different exported KiCad nets.
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
            item.code == "SCH004" && item.message.contains("different exported KiCad nets")
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
            item.code == "SCH004"
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
    /// Regression test: verifies that profile group can be bound to native KiCad group membership.
    #[test]
    fn profile_group_can_be_bound_to_native_kicad_group_membership() {
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
    /// Regression test: verifies that native KiCad group must include profile component members.
    #[test]
    fn native_kicad_group_must_include_profile_component_members() {
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
    /// Regression test: verifies that missing native KiCad group uuid is reported.
    #[test]
    fn missing_native_kicad_group_uuid_is_reported() {
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
                    item.code == "SCH004" && item.message.contains("different exported KiCad nets")
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
            item.code == "SCH004"
                && item.severity == Severity::Warning
                && item.message.contains("no exported KiCad netlist")
        }));
    }
    /// Regression test: verifies that group symbol body primitive must be inside frame.
    #[test]
    fn group_symbol_body_primitive_must_be_inside_frame() {
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
    /// A `Netlist` containing U1.21 on VREG_1V2, C1.1 on the requested supply net, and C1.2 on GND.
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
        let schematic = kicad_clippy_parser::SchematicDocument::from_text(
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
