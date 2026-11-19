//! Checks `SCH021` component metadata and role-specific connection contracts.

use kicad_clippy_core::BuiltinRuleId;
use kicad_clippy_core::Diagnostic;
use kicad_clippy_core::Severity;
use kicad_clippy_erc::Netlist;
use std::collections::BTreeSet;
use super::ComponentIntent;
use super::ComponentKind;
use super::LintProfile;

/// Reports profile entries whose reference designators do not identify placed symbols in the schematic.
///
/// # Arguments
///
/// * `profile` — Project profile whose component declarations are compared with the placed schematic inventory.
/// * `references` — Set of placed schematic reference designators used to validate profile entries.
///
/// # Returns
///
/// Diagnostics for profile entries that refer to components absent from the schematic.
pub(super) fn validate_profile_references(
    profile: &LintProfile,
    references: &BTreeSet<String>,
) -> Vec<Diagnostic> {
    profile.components
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


/// Validates the declared kind, role, purpose, pin contract, and any decoupling intent for one component.
///
/// # Arguments
///
/// * `reference` — Reference designator of the component whose declared function, role, and connections are validated.
/// * `intent` — Component-intent record containing the documented purpose, electrical role, and optional decoupling contract.
/// * `netlist` — Optional `KiCad`-exported flattened netlist used to validate logical pin-to-net assignments.
///
/// # Returns
///
/// Diagnostics for missing purpose documentation, mismatched pin nets, invalid role connections, or incomplete decoupling intent.
pub(super) fn validate_component_intent(
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
        diagnostics.extend(super::sch022::validate_decoupling_intent(
            reference,
            intent.decoupling.as_ref(),
            netlist,
        ));
    }
    diagnostics.extend(super::sch014::validate_pin_net_contract(reference, intent, netlist));
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
