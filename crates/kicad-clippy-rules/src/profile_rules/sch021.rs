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
