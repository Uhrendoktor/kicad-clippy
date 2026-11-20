//! Checks `SCH020` component function and rationale documentation requirements.

use super::GroupIntent;
use crate::core::BuiltinRuleId;
use crate::core::Diagnostic;
use crate::core::Severity;

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

/// Concrete implementation of built-in rule SCH020.
pub(super) struct Sch020;
impl<'a> crate::rules::Rule<super::ProfileRuleContext<'a>> for Sch020 {
    /// Returns the stable SCH020 identifier.
    ///
    /// # Returns
    ///
    /// The stable SCH020 identifier used to associate diagnostics with this rule.
    fn id(&self) -> BuiltinRuleId {
        BuiltinRuleId::Sch020
    }
    /// Evaluates component function/rationale and functional-group purpose documentation.
    ///
    /// # Arguments
    ///
    /// * `context` — Component intent, group descriptions, and normalized schematic text-box facts.
    ///
    /// # Returns
    ///
    /// Diagnostics for incomplete component descriptions or group-purpose statements that do not match the documented text.
    fn check(&self, context: &super::ProfileRuleContext<'a>) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for component in &context.facts.components {
            if let Some(intent) = context.profile.components.get(&component.reference) {
                if intent.function.as_deref().is_none_or(str::is_empty)
                    || intent.rationale.as_deref().is_none_or(str::is_empty)
                {
                    diagnostics.push(Diagnostic::for_builtin_rule(
                        BuiltinRuleId::Sch020,
                        Severity::Error,
                        format!(
                            "{} must document both its circuit function and design rationale",
                            component.reference
                        ),
                    ));
                }
            }
        }
        for (group_id, group) in &context.profile.groups {
            for reference in &group.documentation_members {
                if !group.members.contains(reference) {
                    diagnostics.push(Diagnostic::for_builtin_rule(
                        BuiltinRuleId::Sch020,
                        Severity::Error,
                        format!("group {group_id} lists {reference} as a documentation member but not as a group member"),
                    ));
                }
            }
            if let Some(description) = context
                .facts
                .text_boxes
                .iter()
                .find(|item| item.uuid == group.description_uuid)
                .map(|item| item.text.as_str())
            {
                diagnostics.extend(validate_group_component_purposes(
                    group_id,
                    group,
                    description,
                ));
            }
        }
        diagnostics
    }
}
