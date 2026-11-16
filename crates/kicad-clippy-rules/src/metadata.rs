use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use kicad_clippy_core::{Diagnostic, Severity};
use kicad_clippy_erc::Netlist;
use kicad_clippy_parser::SchematicDocument;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::generic::SchematicFacts;

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
    Capacitor,
    Resistor,
    Diode,
    Inductor,
    Transistor,
    Connector,
    IntegratedCircuit,
    Fuse,
    FerriteBead,
    Crystal,
    Switch,
    Other,
}

/// Extensible component role, such as `decoupling`, `bulk`, `pull_up`, `clamp`, or `load_switch`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ComponentRole(pub String);

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
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GroupIntent {
    /// References belonging to the functional group.
    #[serde(default)]
    pub members: Vec<String>,
    /// Members whose purpose must be described.
    #[serde(default)]
    pub documentation_members: Vec<String>,
    /// UUID of the text box containing the rendered group description.
    pub description_uuid: String,
    /// Optional UUID of the native KiCad group whose component membership is enforced.
    #[serde(default)]
    pub kicad_group_uuid: Option<String>,
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
}

impl LintProfile {
    /// Load a JSON profile from disk.
    ///
    /// # Errors
    /// Returns an I/O or JSON decoding error.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, MetadataError> {
        let content = fs::read_to_string(path).map_err(MetadataError::Io)?;
        let profile: Self = serde_json::from_str(&content).map_err(MetadataError::Json)?;
        if profile.schema_version != 1 {
            return Err(MetadataError::UnsupportedVersion(profile.schema_version));
        }
        Ok(profile)
    }

    /// Validate declared intent against placed components in the schematic.
    #[must_use]
    pub fn validate(&self, document: &SchematicDocument) -> Vec<Diagnostic> {
        self.validate_with_netlist(document, None)
    }

    /// Validate declared intent against the schematic and an exported `KiCad` netlist.
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
                diagnostics.push(Diagnostic::new(
                    "SCH021",
                    Severity::Error,
                    format!("{} has no component-intent metadata", component.reference),
                ));
            }
        }
        diagnostics.extend(validate_decoupler_groups(&self.components));
        diagnostics.extend(validate_groups(&self.groups, &facts));
        diagnostics
    }

    fn validate_profile_references(&self, references: &BTreeSet<String>) -> Vec<Diagnostic> {
        self.components
            .keys()
            .filter(|reference| !references.contains(*reference))
            .map(|reference| {
                Diagnostic::new(
                    "SCH021",
                    Severity::Error,
                    format!("profile metadata references missing component {reference}"),
                )
            })
            .collect()
    }
}

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

fn validate_group(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    references: &BTreeSet<&str>,
) -> Vec<Diagnostic> {
    let mut diagnostics = validate_group_evidence(group_id, group);
    diagnostics.extend(validate_group_members(group_id, group, references));
    diagnostics.extend(validate_kicad_group_membership(group_id, group, facts));
    let description = facts
        .text_boxes
        .iter()
        .find(|text_box| text_box.uuid == group.description_uuid)
        .map(|text_box| text_box.text.as_str());
    let Some(description) = description else {
        diagnostics.push(Diagnostic::new(
            "SCH017",
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

fn validate_kicad_group_membership(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    let Some(expected_uuid) = group.kicad_group_uuid.as_deref() else {
        return Vec::new();
    };
    let Some(native_group) = facts.groups.iter().find(|item| item.uuid == expected_uuid) else {
        return vec![Diagnostic::new(
            "SCH007",
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
            diagnostics.push(Diagnostic::new(
                "SCH007",
                Severity::Error,
                format!(
                    "group {group_id} member {reference} has no placed-symbol UUID for native group membership validation"
                ),
            ));
            continue;
        };
        if !native_group.member_uuids.contains(uuid) {
            diagnostics.push(Diagnostic::new(
                "SCH007",
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
            diagnostics.push(Diagnostic::new(
                "SCH007",
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
        vec![Diagnostic::new(
            "SCH017",
            Severity::Error,
            format!(
                "group {group_id} must cite a document/page/figure or a reviewed screenshot asset"
            ),
        )]
    }
}

fn validate_group_members(
    group_id: &str,
    group: &GroupIntent,
    references: &BTreeSet<&str>,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for reference in &group.members {
        if !references.contains(reference.as_str()) {
            diagnostics.push(Diagnostic::new(
                "SCH017",
                Severity::Error,
                format!("group {group_id} contains missing component {reference}"),
            ));
        }
    }
    for reference in &group.documentation_members {
        if !group.members.contains(reference) {
            diagnostics.push(Diagnostic::new(
                "SCH020",
                Severity::Error,
                format!(
                    "group {group_id} lists {reference} as a documentation member but not as a group member"
                ),
            ));
        }
    }
    diagnostics
}

fn validate_group_component_purposes(
    group_id: &str,
    group: &GroupIntent,
    description: &str,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for (reference, purpose) in &group.component_purposes {
        if !group.members.contains(reference) {
            diagnostics.push(Diagnostic::new(
                "SCH020",
                Severity::Error,
                format!("group {group_id} documents {reference}, which is not a group member"),
            ));
        }
        if !group.documentation_members.contains(reference) {
            diagnostics.push(Diagnostic::new(
                "SCH020",
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

fn validate_group_phrase(
    diagnostics: &mut Vec<Diagnostic>,
    group_id: &str,
    reference: &str,
    field: &str,
    phrase: Option<&str>,
    description: &str,
) {
    let Some(phrase) = phrase.filter(|value| !value.trim().is_empty()) else {
        diagnostics.push(Diagnostic::new(
            "SCH020",
            Severity::Error,
            format!("group {group_id} must provide {reference} {field}"),
        ));
        return;
    };
    if !description.contains(phrase) {
        diagnostics.push(Diagnostic::new(
            "SCH020",
            Severity::Error,
            format!("group {group_id} description must include {reference} {field}"),
        ));
    }
}

fn validate_component_intent(
    reference: &str,
    intent: &ComponentIntent,
    netlist: Option<&Netlist>,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    if intent.function.as_deref().is_none_or(str::is_empty)
        || intent.rationale.as_deref().is_none_or(str::is_empty)
    {
        diagnostics.push(Diagnostic::new(
            "SCH020",
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
        diagnostics.push(Diagnostic::new(
            "SCH021",
            Severity::Warning,
            format!(
                "{reference} uses unknown capacitor role {:?}; document its meaning or use a standard role",
                intent.role.0
            ),
        ));
    }
    diagnostics
}

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
                diagnostics.push(Diagnostic::new(
                    "SCH014",
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
        diagnostics.push(Diagnostic::new(
            "SCH014",
            Severity::Warning,
            format!(
                "{reference} declares pin-to-net contracts but no KiCad netlist was supplied; the contracts were not checked"
            ),
        ));
        return diagnostics;
    };

    for (pin, expected_net) in mappings {
        if pin.trim().is_empty() || expected_net.trim().is_empty() {
            diagnostics.push(Diagnostic::new(
                "SCH014",
                Severity::Error,
                format!("{reference} pin-to-net mapping must use non-empty pin and net names"),
            ));
            continue;
        }
        match netlist.net_for_pin(reference, &pin) {
            Some(actual_net) if actual_net == expected_net => {}
            Some(actual_net) => diagnostics.push(Diagnostic::new(
                "SCH014",
                Severity::Error,
                format!(
                    "{reference}.{pin} is connected to {actual_net}, but the profile expects {expected_net}"
                ),
            )),
            None => diagnostics.push(Diagnostic::new(
                "SCH014",
                Severity::Error,
                format!(
                    "{reference}.{pin} is absent from the exported KiCad netlist but is assigned to {expected_net} in the profile"
                ),
            )),
        }
    }
    diagnostics
}

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
            Diagnostic::new(
                "SCH021",
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
            diagnostics.push(Diagnostic::new(
                "SCH021",
                Severity::Error,
                format!("{reference} connector metadata must declare interface_standard"),
            ));
        }
        if !intent.connections.keys().any(|key| key.starts_with("pin.")) {
            diagnostics.push(Diagnostic::new(
                "SCH021",
                Severity::Error,
                format!("{reference} connector metadata must declare at least one pin.N mapping"),
            ));
        }
    }
    diagnostics
}

fn validate_decoupling_intent(
    reference: &str,
    contract: Option<&DecouplingIntent>,
    netlist: Option<&Netlist>,
) -> Vec<Diagnostic> {
    let Some(contract) = contract else {
        return vec![Diagnostic::new(
            "SCH022",
            Severity::Error,
            format!(
                "{reference} is a decoupling capacitor but does not identify its target pin and terminal/net contract"
            ),
        )];
    };
    let mut diagnostics = Vec::new();
    if !valid_pin_ref(&contract.target_pin) {
        diagnostics.push(Diagnostic::new(
            "SCH022",
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
        diagnostics.push(Diagnostic::new(
            "SCH022",
            Severity::Error,
            format!(
                "{reference} decoupling metadata must define target/return terminals, return net, and cluster"
            ),
        ));
    }
    if contract.target_terminal == contract.return_terminal {
        diagnostics.push(Diagnostic::new(
            "SCH022",
            Severity::Error,
            format!("{reference} maps both decoupling terminals to the same capacitor terminal"),
        ));
    }
    if let Some(netlist) = netlist {
        diagnostics.extend(validate_decoupling_nets(reference, contract, netlist));
    }
    diagnostics
}

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
        (Some(target), Some(capacitor)) => diagnostics.push(Diagnostic::new(
            "SCH022",
            Severity::Error,
            format!(
                "{reference} terminal {} is on {capacitor}, but {}.{} is on {target}",
                contract.target_terminal, target_reference, target_pin
            ),
        )),
        (None, _) => diagnostics.push(Diagnostic::new(
            "SCH022",
            Severity::Error,
            format!("target pin {target_reference}.{target_pin} is absent from the exported KiCad netlist"),
        )),
        (_, None) => diagnostics.push(Diagnostic::new(
            "SCH022",
            Severity::Error,
            format!(
                "{}.{} is absent from the exported KiCad netlist",
                reference, contract.target_terminal
            ),
        )),
    }

    match capacitor_return_net {
        Some(actual) if actual == contract.return_net => {}
        Some(actual) => diagnostics.push(Diagnostic::new(
            "SCH022",
            Severity::Error,
            format!(
                "{reference} return terminal {} is on {actual}, expected {}",
                contract.return_terminal, contract.return_net
            ),
        )),
        None => diagnostics.push(Diagnostic::new(
            "SCH022",
            Severity::Error,
            format!(
                "{}.{} is absent from the exported KiCad netlist",
                reference, contract.return_terminal
            ),
        )),
    }
    diagnostics
}

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
            diagnostics.push(Diagnostic::new(
                "SCH022",
                Severity::Error,
                format!(
                    "decouplers for {target} disagree on their return net: {}",
                    returns.into_iter().collect::<Vec<_>>().join(", ")
                ),
            ));
        }
        if clusters.len() > 1 {
            diagnostics.push(Diagnostic::new(
                "SCH022",
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
mod tests {
    #![allow(clippy::expect_used)]
    use std::collections::BTreeMap;

    use kicad_clippy_erc::{Netlist, PinRef};

    use super::{ComponentKind, ComponentRole, LintProfile, ProfileMode};

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
