//! Pin-intersection obstacle checks for SCH013.

use super::*;

/// Finds wires that pass through a placed pin that is not a permitted endpoint contact.
///
/// # Arguments
///
/// * `wire_index` — Stable index of the wire within the current segment collection.
/// * `segment` — Wire segment being checked for obstacle intersection or clearance.
/// * `pins` — Placed symbol-pin facts used to allow only local wire endpoint contact.
///
/// # Returns
///
/// SCH013 diagnostics for wires passing through placed pins that are not permitted wire endpoints.
pub(super) fn intermediate_pin_obstacle_diagnostics(
    wire_index: usize,
    segment: Segment,
    pins: &[SchematicPinFact],
) -> Vec<Diagnostic> {
    pins.iter()
        .filter(|pin| {
            let point = Point {
                x: pin.position_mm.0,
                y: pin.position_mm.1,
            };
            point_on_segment_any(point, segment.start, segment.end) && !is_endpoint(point, segment)
        })
        .map(|pin| {
            at_segment(
                BuiltinRuleId::Sch013,
                &format!(
                    "wire {wire_index} passes through intermediate pin {}.{}",
                    pin.reference, pin.number
                ),
                segment,
            )
        })
        .collect()
}
