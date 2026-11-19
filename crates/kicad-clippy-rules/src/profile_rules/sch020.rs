//! Checks `SCH020` component function and rationale documentation requirements.

use kicad_clippy_core::BuiltinRuleId;
use kicad_clippy_core::Diagnostic;
use kicad_clippy_core::Severity;
use super::GroupIntent;
use super::validate_group_phrase;

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
pub(super) fn validate_group_component_purposes(
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
