//! Checks SCH021 component metadata and role-specific connection contracts.

use super::ComponentIntent;
use super::ComponentKind;
use super::LintProfile;
use super::ProfileMode;
use kicad_clippy_core::BuiltinRuleId;
use kicad_clippy_core::Diagnostic;
use kicad_clippy_core::Severity;
use std::collections::BTreeSet;

/// Reports profile component entries that do not identify placed schematic symbols.
///
/// # Arguments
///
/// * `profile` — Loaded component-intent profile whose component declarations are compared with the placed schematic inventory.
/// * `references` — Set of placed schematic reference designators used to resolve profile component declarations.
///
/// # Returns
///
/// Diagnostics for profile component references that are absent from the schematic.
pub(super) fn validate_profile_references(
    profile: &LintProfile,
    references: &BTreeSet<String>,
) -> Vec<Diagnostic> {
    profile
        .components
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
pub(super) fn validate_role_contract(reference: &str, intent: &ComponentIntent) -> Vec<Diagnostic> {
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

    if intent.kind == ComponentKind::TestPoint {
        if intent.role.0 != "test_point" {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch021,
                Severity::Error,
                format!("{reference} test-point component must use role \"test_point\""),
            ));
        }
        if intent.pin_nets.is_empty() {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch021,
                Severity::Error,
                format!("{reference} test-point metadata must declare at least one pin_nets mapping"),
            ));
        }
        for (pin, net) in &intent.pin_nets {
            if pin.trim().is_empty() || net.trim().is_empty() || pin.trim() != pin {
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch021,
                    Severity::Error,
                    format!("{reference} test-point pin_nets entries must have trimmed, non-empty pin numbers and net names"),
                ));
            }
        }
    }

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
        if !intent.connections.keys().any(|key| key.starts_with("pin."))
            && intent.pin_nets.is_empty()
        {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch021,
                Severity::Error,
                format!("{reference} connector metadata must declare at least one pin.N or pin_nets mapping"),
            ));
        }
        diagnostics.extend(validate_required_connector_pins(reference, intent));
    }
    diagnostics
}

/// Validates the optional required connector pin list against the declared pin-to-net map.
///
/// The list is an explicit interface contract; this check does not infer which connector
/// pins should be populated from the component family or its library symbol.
///
/// # Arguments
///
/// * `reference` — Reference designator of the connector instance whose required interface pins are being validated.
/// * `intent` — Component metadata containing the optional `required_pins` attribute and pin mappings.
///
/// # Returns
///
/// SCH021 diagnostics for malformed pin lists or required pins without an expected net mapping.
fn validate_required_connector_pins(reference: &str, intent: &ComponentIntent) -> Vec<Diagnostic> {
    let Some(raw_required_pins) = intent.attributes.get("required_pins") else {
        return Vec::new();
    };
    let mut diagnostics = Vec::new();
    let mut required_pins = BTreeSet::new();
    for token in raw_required_pins.split(',').map(str::trim) {
        if token.is_empty()
            || !token
                .chars()
                .all(|character| character.is_ascii_alphanumeric())
        {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch021,
                Severity::Error,
                format!(
                    "{reference} connector required_pins contains invalid pin number {token:?}"
                ),
            ));
            continue;
        }
        if !required_pins.insert(token.to_owned()) {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch021,
                Severity::Warning,
                format!("{reference} connector required_pins repeats pin {token}"),
            ));
        }
    }
    if required_pins.is_empty() {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch021,
            Severity::Error,
            format!("{reference} connector required_pins must list at least one pin number"),
        ));
        return diagnostics;
    }

    for pin in required_pins {
        let connection_key = format!("pin.{pin}");
        let has_mapping = intent
            .connections
            .get(&connection_key)
            .is_some_and(|net| !net.trim().is_empty())
            || intent
                .pin_nets
                .get(&pin)
                .is_some_and(|net| !net.trim().is_empty());
        if !has_mapping {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch021,
                Severity::Error,
                format!(
                    "{reference} connector required pin {pin} has no expected net mapping; add connections.{connection_key} or pin_nets.{pin}"
                ),
            ));
        }
    }
    diagnostics
}

/// Concrete implementation of built-in rule SCH021.
pub(super) struct Sch021;
impl<'a> crate::Rule<super::ProfileRuleContext<'a>> for Sch021 {
    /// Returns the stable SCH021 identifier.
    ///
    /// # Returns
    ///
    /// The stable SCH021 identifier used to associate diagnostics with this rule.
    fn id(&self) -> BuiltinRuleId {
        BuiltinRuleId::Sch021
    }
    /// Evaluates component references, strict metadata coverage, and role-specific connection declarations.
    ///
    /// # Arguments
    ///
    /// * `context` — Project profile and normalized schematic component facts.
    ///
    /// # Returns
    ///
    /// Diagnostics for unresolved references, missing strict-mode metadata, or incomplete role contracts.
    fn check(&self, context: &super::ProfileRuleContext<'a>) -> Vec<Diagnostic> {
        let profile = context.profile;
        let references: BTreeSet<_> = context
            .facts
            .components
            .iter()
            .map(|item| item.reference.clone())
            .collect();
        let mut diagnostics = validate_profile_references(profile, &references);
        for component in &context.facts.components {
            if let Some(intent) = profile.components.get(&component.reference) {
                diagnostics.extend(validate_role_contract(&component.reference, intent));
            } else if profile.mode == ProfileMode::Strict
                && !profile.ignore_components.contains(&component.reference)
            {
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch021,
                    Severity::Error,
                    format!("{} has no component-intent metadata", component.reference),
                ));
            }
        }
        diagnostics
    }
}

#[cfg(test)]
/// Tests connector interface completeness contracts.
mod tests {
    #![allow(clippy::expect_used)]

    use super::validate_role_contract;
    use crate::metadata::{ComponentIntent, ComponentKind, ComponentRole};
    use std::collections::BTreeMap;

    /// Builds connector metadata with the supplied required-pin list and expected net mappings.
    ///
    /// # Arguments
    ///
    /// * `required_pins` — Comma-separated required connector pin numbers.
    /// * `mappings` — Expected net mappings keyed by `pin.N`.
    ///
    /// # Returns
    ///
    /// A connector intent record suitable for SCH021 validation.
    fn connector_intent(required_pins: &str, mappings: &[(&str, &str)]) -> ComponentIntent {
        ComponentIntent {
            kind: ComponentKind::Connector,
            role: ComponentRole("interface".to_owned()),
            function: Some("External interface".to_owned()),
            rationale: Some("Declared connector interface".to_owned()),
            evidence: None,
            decoupling: None,
            connections: mappings
                .iter()
                .map(|(pin, net)| ((*pin).to_owned(), (*net).to_owned()))
                .collect(),
            pin_nets: BTreeMap::new(),
            attributes: BTreeMap::from([
                (
                    "interface_standard".to_owned(),
                    "declared-interface".to_owned(),
                ),
                ("required_pins".to_owned(), required_pins.to_owned()),
            ]),
            max_field_body_gap_mm: BTreeMap::new(),
            voltage_rating_min_v: None,
            power_rating_min_w: None,
        }
    }

    /// Rejects a connector when one declared required pin has no expected net mapping.
    #[test]
    fn required_connector_pins_must_all_have_net_mappings() {
        let intent = connector_intent("1, 2, 3", &[("pin.1", "VBUS"), ("pin.3", "GND")]);
        let diagnostics = validate_role_contract("J1", &intent);
        assert!(
            diagnostics
                .iter()
                .any(|item| { item.code == "SCH021" && item.message.contains("required pin 2") })
        );
        assert!(
            !diagnostics
                .iter()
                .any(|item| { item.code == "SCH021" && item.message.contains("required pin 1") })
        );
    }

    /// Accepts a connector whose full declared required-pin set is mapped.
    #[test]
    fn fully_mapped_required_connector_pins_are_accepted() {
        let intent = connector_intent("1, 2", &[("pin.1", "VBUS"), ("pin.2", "GND")]);
        assert_eq!(validate_role_contract("J1", &intent), Vec::new());
    }

    /// Reports an empty token in a connector required-pin declaration.
    #[test]
    fn malformed_required_connector_pin_list_is_rejected() {
        let intent = connector_intent("1, ,2", &[("pin.1", "VBUS"), ("pin.2", "GND")]);
        assert!(
            validate_role_contract("J1", &intent).iter().any(|item| {
                item.code == "SCH021" && item.message.contains("invalid pin number")
            })
        );
    }
}
