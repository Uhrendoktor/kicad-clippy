//! Checks `SCH007` consistency between profile group membership and native `KiCad` groups.

use super::GroupIntent;
use super::SchematicFacts;
use kicad_clippy_core::BuiltinRuleId;
use kicad_clippy_core::Diagnostic;
use kicad_clippy_core::Severity;

/// Checks that the profile's member references match the schematic's explicit `KiCad` group membership.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
///
/// # Returns
///
/// Diagnostics when the schematic's native `KiCad` group is missing or its member UUIDs do not match the profile references.
pub(super) fn validate_kicad_group_membership(
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

/// Concrete implementation of built-in rule SCH007.
pub(super) struct Sch007;
impl<'a> crate::Rule<super::ProfileRuleContext<'a>> for Sch007 {
    /// Returns the stable SCH007 identifier.
    ///
    /// # Returns
    ///
    /// The stable SCH007 identifier.
    fn id(&self) -> BuiltinRuleId {
        BuiltinRuleId::Sch007
    }
    /// Compares profile group membership with native KiCad group membership.
    ///
    /// # Arguments
    ///
    /// * context — Project group declarations and normalized native group facts.
    ///
    /// # Returns
    ///
    /// Diagnostics for missing native groups or mismatched native member UUIDs.
    fn check(&self, context: &super::ProfileRuleContext<'a>) -> Vec<Diagnostic> {
        context
            .profile
            .groups
            .iter()
            .flat_map(|(id, group)| validate_kicad_group_membership(id, group, context.facts))
            .collect()
    }
}
