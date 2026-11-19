//! Checks `SCH021` strict component metadata and role-specific connection requirements.

use kicad_clippy_core::BuiltinRuleId;
use kicad_clippy_core::Diagnostic;
use kicad_clippy_core::Severity;
use std::collections::BTreeSet;
use super::ComponentIntent;
use super::ComponentKind;
use super::LintProfile;
use super::validate_role_contract;

    /// Reports profile entries whose reference designators do not identify placed symbols in the schematic.
    ///
    /// # Arguments
    ///
    /// * `profile` — Loaded profile whose component declarations and strict-mode exemptions are checked.
/// * `references` — Set of schematic reference designators used to verify profile membership.
    ///
    /// # Returns
    ///
    /// Diagnostics for profile component references that do not resolve to placed symbols in the schematic.
    pub(super) fn validate_profile_references(profile: &LintProfile, references: &BTreeSet<String>) -> Vec<Diagnostic> {
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
