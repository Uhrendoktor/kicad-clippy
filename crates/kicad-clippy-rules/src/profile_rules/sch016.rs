//! Checks `SCH016` internal wire-length budgets for functional schematic groups.

use crate::generic::GEOMETRY_TOLERANCE_MM;
use kicad_clippy_core::BuiltinRuleId;
use kicad_clippy_core::Diagnostic;
use kicad_clippy_core::Severity;
use super::GroupIntent;
use super::MAX_COMPACTNESS_SCORE;
use super::SchematicFacts;
use super::SCORE_COMPARISON_EPSILON;

/// Checks whether the total physical wire length of a group stays within its declared length budget.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
///
/// # Returns
///
/// Diagnostics when the wire-length budget is invalid, the frame cannot be resolved, or the group's physical wire length exceeds its budget.
pub(super) fn validate_wire_compactness(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    let Some(budget) = group.wire_length_budget_mm else {
        return Vec::new();
    };
    if !budget.is_finite() || budget <= 0.0 {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch016,
            Severity::Error,
            format!("group {group_id} wire_length_budget_mm must be a finite positive distance"),
        )];
    }
    let Some(frame_uuid) = group.frame_uuid.as_deref() else {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch016,
            Severity::Error,
            format!("group {group_id} must set frame_uuid to calculate its internal wire length"),
        )];
    };
    let Some(frame) = facts.frames.iter().find(|frame| frame.uuid == frame_uuid) else {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch016,
            Severity::Error,
            format!(
                "group {group_id} references missing schematic frame UUID {frame_uuid} for wire compactness"
            ),
        )];
    };
    let (left, top, right, bottom) = frame.bounds_mm;
    if left >= right || top >= bottom {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch016,
            Severity::Error,
            format!("group {group_id} frame {frame_uuid} has invalid or zero-area bounds"),
        )];
    }

    let inside = |point: (f64, f64)| {
        point.0 >= left - GEOMETRY_TOLERANCE_MM
            && point.0 <= right + GEOMETRY_TOLERANCE_MM
            && point.1 >= top - GEOMETRY_TOLERANCE_MM
            && point.1 <= bottom + GEOMETRY_TOLERANCE_MM
    };
    let total_wire_length_mm: f64 = facts
        .wires
        .iter()
        .filter(|wire| inside(wire.start_mm) && inside(wire.end_mm))
        .map(|wire| (wire.end_mm.0 - wire.start_mm.0).hypot(wire.end_mm.1 - wire.start_mm.1))
        .sum();
    let score = total_wire_length_mm / budget;
    if score > MAX_COMPACTNESS_SCORE + SCORE_COMPARISON_EPSILON {
        vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch016,
            Severity::Warning,
            format!(
                "group {group_id} wire compactness score is {score:.3} ({total_wire_length_mm:.2}/{budget:.2} mm); maximum score is 1.000"
            ),
        )]
    } else {
        Vec::new()
    }
}
