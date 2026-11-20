//! Checks `SCH017` group membership, description references, and evidence requirements.

use super::GroupIntent;
use crate::core::BuiltinRuleId;
use crate::core::Diagnostic;
use crate::core::Severity;
use std::collections::BTreeSet;

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
pub(super) fn validate_group_evidence(group_id: &str, group: &GroupIntent) -> Vec<Diagnostic> {
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
pub(super) fn validate_group_members(
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
    diagnostics
}

/// Concrete implementation of built-in rule SCH017.
pub(super) struct Sch017;
impl<'a> crate::rules::Rule<super::ProfileRuleContext<'a>> for Sch017 {
    /// Returns the stable SCH017 identifier.
    ///
    /// # Returns
    ///
    /// The stable SCH017 identifier used to associate diagnostics with this rule.
    fn id(&self) -> BuiltinRuleId {
        BuiltinRuleId::Sch017
    }
    /// Evaluates group evidence, component membership, and description-box references.
    ///
    /// # Arguments
    ///
    /// * `context` — Project group declarations and normalized schematic facts.
    ///
    /// # Returns
    ///
    /// Diagnostics for missing members, incomplete evidence, or missing group description boxes.
    fn check(&self, context: &super::ProfileRuleContext<'a>) -> Vec<Diagnostic> {
        let references: BTreeSet<_> = context
            .facts
            .components
            .iter()
            .map(|item| item.reference.as_str())
            .collect();
        let mut diagnostics = Vec::new();
        for (group_id, group) in &context.profile.groups {
            diagnostics.extend(validate_group_evidence(group_id, group));
            diagnostics.extend(validate_group_members(group_id, group, &references));
            if !context
                .facts
                .text_boxes
                .iter()
                .any(|item| item.uuid == group.description_uuid)
            {
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch017,
                    Severity::Error,
                    format!(
                        "group {group_id} description text box UUID {} is missing",
                        group.description_uuid
                    ),
                ));
            }
        }
        diagnostics
    }
}
