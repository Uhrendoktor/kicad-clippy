//! Checks `SCH006` minimum spacing between functional-group member symbols.

use crate::generic::GEOMETRY_TOLERANCE_MM;
use kicad_clippy_core::BuiltinRuleId;
use kicad_clippy_core::Diagnostic;
use kicad_clippy_core::Severity;
use super::GroupIntent;
use super::SchematicFacts;

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
pub(super) fn validate_group_spacing(
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
