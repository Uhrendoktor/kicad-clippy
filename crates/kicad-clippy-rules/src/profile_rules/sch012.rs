//! Checks `SCH012` minimum frame-inset clearance for declared group contents.

use crate::generic::GEOMETRY_TOLERANCE_MM;
use crate::generic::TWO_SIDED_FACTOR;
use kicad_clippy_core::BuiltinRuleId;
use kicad_clippy_core::Diagnostic;
use kicad_clippy_core::Severity;
use super::COORDINATE_KEY_DECIMAL_PLACES;
use super::group_wire_components;
use super::GroupIntent;
use super::point_inside_frame_with_clearance;
use super::point_inside_frame;
use super::rectangle_inside_frame_with_clearance;
use super::rectangles_overlap;
use super::sch009::{group_wire_components, symbol_primitive_inside_frame, wire_belongs_to_group, wire_overlaps_frame};
use super::SchematicFacts;
use super::symbol_primitive_inside_frame;
use super::valid_frame_bounds;
use super::validate_description_frame_clearance;
use super::validate_frame_clearance;
use super::validate_group_annotation_box_clearance;
use super::validate_group_graphic_clearance;
use super::validate_group_image_clearance;
use super::validate_group_label_clearance;
use super::validate_group_plain_text_clearance;
use super::validate_group_symbol_primitive_clearance;
use super::validate_group_wire_clearance;
use super::validate_member_field_frame_clearance;
use super::validate_member_frame_clearance;
use super::validate_member_pin_frame_clearance;
use super::wire_belongs_to_group;
use super::wire_overlaps_frame;
use thiserror::Error;

/// Checks that group members and their associated schematic content are inset from the referenced frame by the requested clearance.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
///
/// # Returns
///
/// Diagnostics for group members or associated schematic objects that do not maintain the configured inset from the frame edges.
pub(super) fn validate_frame_clearance(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    let Some(clearance) = group.minimum_frame_clearance_mm else {
        return Vec::new();
    };
    if !clearance.is_finite() || clearance < 0.0 {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch012,
            Severity::Error,
            format!("group {group_id} minimum_frame_clearance_mm must be finite and non-negative"),
        )];
    }
    let Some(frame_uuid) = group.frame_uuid.as_deref() else {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch012,
            Severity::Error,
            format!("group {group_id} requests frame clearance but has no frame_uuid"),
        )];
    };
    let Some(frame) = facts.frames.iter().find(|frame| frame.uuid == frame_uuid) else {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch012,
            Severity::Error,
            format!("group {group_id} references missing schematic frame UUID {frame_uuid}"),
        )];
    };
    let bounds = frame.bounds_mm;
    if !valid_frame_bounds(bounds) {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch012,
            Severity::Error,
            format!("group {group_id} frame {frame_uuid} has invalid or zero-area bounds"),
        )];
    }
    let (left, top, right, bottom) = bounds;
    if right - left < TWO_SIDED_FACTOR * clearance || bottom - top < TWO_SIDED_FACTOR * clearance {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch012,
            Severity::Error,
            format!(
                "group {group_id} frame {frame_uuid} is too small to provide {clearance:.2} mm clearance on every side"
            ),
        )];
    }
    let mut diagnostics =
        validate_member_frame_clearance(group_id, group, facts, frame_uuid, bounds, clearance);
    diagnostics.extend(validate_member_pin_frame_clearance(
        group_id, group, facts, frame_uuid, bounds, clearance,
    ));
    diagnostics.extend(validate_member_field_frame_clearance(
        group_id, group, facts, frame_uuid, bounds, clearance,
    ));
    diagnostics.extend(validate_description_frame_clearance(
        group_id, group, facts, frame_uuid, bounds, clearance,
    ));
    diagnostics.extend(validate_group_symbol_primitive_clearance(
        group_id, group, facts, frame_uuid, bounds, clearance,
    ));
    diagnostics.extend(validate_group_wire_clearance(
        group_id, group, facts, frame_uuid, bounds, clearance,
    ));
    diagnostics.extend(validate_group_label_clearance(
        group_id, group, facts, frame_uuid, bounds, clearance,
    ));
    diagnostics.extend(validate_group_annotation_box_clearance(
        group_id, group, facts, frame_uuid, bounds, clearance,
    ));
    diagnostics.extend(validate_group_plain_text_clearance(
        group_id, facts, frame_uuid, bounds, clearance,
    ));
    diagnostics.extend(validate_group_graphic_clearance(
        group_id, facts, frame_uuid, bounds, clearance,
    ));
    diagnostics.extend(validate_group_image_clearance(
        group_id, facts, frame_uuid, bounds, clearance,
    ));
    diagnostics
}
/// Checks that all four frame coordinates are finite and that the rectangle has positive width and height.
///
/// # Returns
///
/// `true` when all coordinates are finite and the frame has positive width and height.
fn valid_frame_bounds((left, top, right, bottom): (f64, f64, f64, f64)) -> bool {
    left < right
        && top < bottom
        && [left, top, right, bottom]
            .iter()
            .all(|value| value.is_finite())
}
/// Checks each group member's symbol origin against the minimum clearance inset from the frame edges.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
/// * `clearance` — Minimum required distance from each frame edge, in millimetres.
///
/// # Returns
///
/// Diagnostics when a group member's symbol origin is closer to a frame edge than the declared minimum clearance.
fn validate_member_frame_clearance(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    frame_uuid: &str,
    bounds: (f64, f64, f64, f64),
    clearance: f64,
) -> Vec<Diagnostic> {
    let (left, top, right, bottom) = bounds;
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
                    BuiltinRuleId::Sch012,
                Severity::Error,
                format!(
                    "group {group_id} member {reference} has no symbol origin for frame-clearance validation"
                ),
            ));
            continue;
        };
        let inside_clearance = x_mm >= left + clearance - GEOMETRY_TOLERANCE_MM
            && x_mm <= right - clearance + GEOMETRY_TOLERANCE_MM
            && y_mm >= top + clearance - GEOMETRY_TOLERANCE_MM
            && y_mm <= bottom - clearance + GEOMETRY_TOLERANCE_MM;
        if !inside_clearance {
            diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch012,
                Severity::Error,
                format!(
                    "group {group_id} member {reference} origin is within {clearance:.2} mm of frame {frame_uuid} edge"
                ),
            ));
        }
    }
    diagnostics
}
/// Checks every placed pin on a group member against the minimum frame-edge clearance.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
/// * `clearance` — Minimum required distance from each frame edge, in millimetres.
///
/// # Returns
///
/// Diagnostics when a placed pin belonging to a group member lies inside the required frame-edge clearance zone.
fn validate_member_pin_frame_clearance(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    frame_uuid: &str,
    bounds: (f64, f64, f64, f64),
    clearance: f64,
) -> Vec<Diagnostic> {
    let (left, top, right, bottom) = bounds;
    let mut diagnostics = Vec::new();
    for pin in facts
        .pins
        .iter()
        .filter(|pin| group.members.contains(&pin.reference))
    {
        let (x_mm, y_mm) = pin.position_mm;
        let inside_clearance = x_mm >= left + clearance - GEOMETRY_TOLERANCE_MM
            && x_mm <= right - clearance + GEOMETRY_TOLERANCE_MM
            && y_mm >= top + clearance - GEOMETRY_TOLERANCE_MM
            && y_mm <= bottom - clearance + GEOMETRY_TOLERANCE_MM;
        if !inside_clearance {
            diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch012,
                Severity::Error,
                format!(
                    "group {group_id} member pin {}.{} does not maintain {clearance:.2} mm clearance from frame {frame_uuid}",
                    pin.reference, pin.number
                ),
            ));
        }
    }
    diagnostics
}
/// Checks the visible reference and value fields of group members against the minimum frame-edge clearance.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
/// * `clearance` — Minimum required distance from each frame edge, in millimetres.
///
/// # Returns
///
/// Diagnostics when a visible reference or value field lies inside the required frame-edge clearance zone.
fn validate_member_field_frame_clearance(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    frame_uuid: &str,
    bounds: (f64, f64, f64, f64),
    clearance: f64,
) -> Vec<Diagnostic> {
    let (left, top, right, bottom) = bounds;
    let mut diagnostics = Vec::new();
    for field in facts
        .visible_fields
        .iter()
        .filter(|field| group.members.contains(&field.reference))
    {
        let (field_left, field_top, field_right, field_bottom) = field.bounds_mm;
        let inside_clearance = field_left >= left + clearance - GEOMETRY_TOLERANCE_MM
            && field_top >= top + clearance - GEOMETRY_TOLERANCE_MM
            && field_right <= right - clearance + GEOMETRY_TOLERANCE_MM
            && field_bottom <= bottom - clearance + GEOMETRY_TOLERANCE_MM;
        if !inside_clearance {
            diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch012,
                Severity::Error,
                format!(
                    "group {group_id} member field {} {} does not maintain {clearance:.2} mm clearance from frame {frame_uuid}",
                    field.reference, field.name
                ),
            ));
        }
    }
    diagnostics
}
/// Checks that stroked symbol-library geometry stays the requested distance from the frame edges.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
/// * `clearance` — Minimum required distance from each frame edge, in millimetres.
///
/// # Returns
///
/// Diagnostics for stroked symbol primitives that approach the frame edges more closely than the declared clearance.
fn validate_group_symbol_primitive_clearance(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
    clearance: f64,
) -> Vec<Diagnostic> {
    facts
        .symbol_primitives
        .iter()
        .filter(|primitive| group.members.contains(&primitive.reference))
        .filter(|primitive| !symbol_primitive_inside_frame(primitive, frame_bounds, clearance))
        .map(|primitive| {
            Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch012,
                Severity::Error,
                format!(
                    "group {group_id} member {} symbol primitive does not maintain {clearance:.2} mm clearance from frame {frame_uuid}",
                    primitive.reference
                ),
            )
        })
        .collect()
}
/// Checks that wire segments belonging to the group stay the requested distance from the frame edges.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
/// * `clearance` — Minimum required distance from each frame edge, in millimetres.
///
/// # Returns
///
/// Diagnostics for group wire segments that enter the required frame-edge clearance zone.
fn validate_group_wire_clearance(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
    clearance: f64,
) -> Vec<Diagnostic> {
    let (component_ids, member_components) = group_wire_components(group, facts);
    facts
        .wires
        .iter()
        .filter(|wire| {
            wire_belongs_to_group(wire, &component_ids, &member_components)
                || wire_overlaps_frame(wire, frame_bounds)
        })
        .filter(|wire| {
            !point_inside_frame_with_clearance(frame_bounds, wire.start_mm, clearance)
                || !point_inside_frame_with_clearance(frame_bounds, wire.end_mm, clearance)
        })
        .map(|wire| {
            Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch012,
                Severity::Error,
                format!(
                    "group {group_id} member wire segment from ({:.2}, {:.2}) to ({:.2}, {:.2}) does not maintain {clearance:.2} mm clearance from frame {frame_uuid}",
                    wire.start_mm.0, wire.start_mm.1, wire.end_mm.0, wire.end_mm.1
                ),
            )
        })
        .collect()
}
/// Checks that labels attached to group wires stay the requested distance from the frame edges.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
/// * `clearance` — Minimum required distance from each frame edge, in millimetres.
///
/// # Returns
///
/// Diagnostics for labels attached to the group's wire components that violate the frame-edge clearance.
fn validate_group_label_clearance(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
    clearance: f64,
) -> Vec<Diagnostic> {
    let (component_ids, member_components) = group_wire_components(group, facts);
    facts
        .labels
        .iter()
        .filter(|label| {
            let key = format!("{:.*}:{:.*}", COORDINATE_KEY_DECIMAL_PLACES, label.position_mm.0, COORDINATE_KEY_DECIMAL_PLACES, label.position_mm.1);
            let member_wire_label = component_ids
                .get(&key)
                .is_some_and(|component| member_components.contains(component));
            member_wire_label
                || point_inside_frame(frame_bounds, label.position_mm)
                || label
                    .bounds_mm
                    .is_some_and(|bounds| rectangles_overlap(bounds, frame_bounds))
        })
        .filter(|label| {
            label.bounds_mm.map_or_else(
                || !point_inside_frame_with_clearance(frame_bounds, label.position_mm, clearance),
                |bounds| !rectangle_inside_frame_with_clearance(bounds, frame_bounds, clearance),
            )
        })
        .map(|label| {
            Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch012,
                Severity::Error,
                format!(
                    "group {group_id} member wire label {} {:?} does not maintain {clearance:.2} mm clearance from frame {frame_uuid}",
                    label.kind, label.name
                ),
            )
        })
        .collect()
}
/// Checks that group annotation boxes stay the requested distance from the frame edges.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
/// * `clearance` — Minimum required distance from each frame edge, in millimetres.
///
/// # Returns
///
/// Diagnostics for annotation text boxes that do not preserve the configured distance from frame edges.
fn validate_group_annotation_box_clearance(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
    clearance: f64,
) -> Vec<Diagnostic> {
    facts
        .text_boxes
        .iter()
        .filter(|text_box| text_box.uuid != group.description_uuid)
        .filter_map(|text_box| {
            let bounds = text_box.bounds_mm?;
            (rectangles_overlap(bounds, frame_bounds)
                && !rectangle_inside_frame_with_clearance(bounds, frame_bounds, clearance))
            .then(|| {
                Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch012,
                    Severity::Error,
                    format!(
                        "group {group_id} annotation text box {} does not maintain {clearance:.2} mm clearance from frame {frame_uuid}",
                        text_box.uuid
                    ),
                )
            })
        })
        .collect()
}
/// Checks that standalone text stays the requested distance from the frame edges.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
/// * `clearance` — Minimum required distance from each frame edge, in millimetres.
///
/// # Returns
///
/// Diagnostics for standalone text whose bounds do not preserve the configured distance from frame edges.
fn validate_group_plain_text_clearance(
    group_id: &str,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
    clearance: f64,
) -> Vec<Diagnostic> {
    facts
        .texts
        .iter()
        .filter_map(|text| {
            let bounds = text.bounds_mm?;
            (rectangles_overlap(bounds, frame_bounds)
                && !rectangle_inside_frame_with_clearance(bounds, frame_bounds, clearance))
            .then(|| {
                Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch012,
                    Severity::Error,
                    format!(
                        "group {group_id} plain text annotation {:?} does not maintain {clearance:.2} mm clearance from frame {frame_uuid}",
                        text.text
                    ),
                )
            })
        })
        .collect()
}
/// Checks that sheet graphics stay the requested distance from the frame edges.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
/// * `clearance` — Minimum required distance from each frame edge, in millimetres.
///
/// # Returns
///
/// Diagnostics for sheet-level graphics whose stroked bounds enter the configured frame-edge clearance zone.
fn validate_group_graphic_clearance(
    group_id: &str,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
    clearance: f64,
) -> Vec<Diagnostic> {
    facts
        .graphics
        .iter()
        .filter(|graphic| graphic.uuid.as_deref() != Some(frame_uuid))
        .filter_map(|graphic| {
            let bounds = graphic.bounds_mm?;
            (rectangles_overlap(bounds, frame_bounds)
                && !rectangle_inside_frame_with_clearance(bounds, frame_bounds, clearance))
            .then(|| {
                Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch012,
                    Severity::Error,
                    format!(
                        "group {group_id} vector graphic {} {:?} does not maintain {clearance:.2} mm clearance from frame {frame_uuid}",
                        graphic.kind, graphic.uuid
                    ),
                )
            })
        })
        .collect()
}
/// Checks that bitmap image bounds stay the requested distance from the frame edges.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
/// * `clearance` — Minimum required distance from each frame edge, in millimetres.
///
/// # Returns
///
/// Diagnostics for bitmap image bounds that do not preserve the configured distance from frame edges.
fn validate_group_image_clearance(
    group_id: &str,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
    clearance: f64,
) -> Vec<Diagnostic> {
    facts
        .images
        .iter()
        .filter_map(|image| {
            let bounds = image.bounds_mm?;
            (rectangles_overlap(bounds, frame_bounds)
                && !rectangle_inside_frame_with_clearance(bounds, frame_bounds, clearance))
            .then(|| {
                Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch012,
                    Severity::Error,
                    format!(
                        "group {group_id} bitmap image {:?} does not maintain {clearance:.2} mm clearance from frame {frame_uuid}",
                        image.uuid
                    ),
                )
            })
        })
        .collect()
}
/// Checks that the group's referenced description text box remains inside the sheet frame with the required clearance on all four sides. A missing description box is treated as absent metadata; a present box without usable geometry produces an SCH012 diagnostic.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
/// * `clearance` — Minimum required distance from each frame edge, in millimetres.
///
/// # Returns
///
/// SCH012 diagnostics when the referenced description box lacks usable bounds or fails the required inset on any frame edge.
fn validate_description_frame_clearance(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    frame_uuid: &str,
    bounds: (f64, f64, f64, f64),
    clearance: f64,
) -> Vec<Diagnostic> {
    let (left, top, right, bottom) = bounds;
    let mut diagnostics = Vec::new();
    if let Some(text_box) = facts
        .text_boxes
        .iter()
        .find(|text_box| text_box.uuid == group.description_uuid)
    {
        if let Some((box_left, box_top, box_right, box_bottom)) = text_box.bounds_mm {
            let text_inside_clearance = box_left >= left + clearance - GEOMETRY_TOLERANCE_MM
                && box_top >= top + clearance - GEOMETRY_TOLERANCE_MM
                && box_right <= right - clearance + GEOMETRY_TOLERANCE_MM
                && box_bottom <= bottom - clearance + GEOMETRY_TOLERANCE_MM;
            if !text_inside_clearance {
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch012,
                    Severity::Error,
                    format!(
                        "group {group_id} description text box {} does not maintain {clearance:.2} mm clearance from frame {frame_uuid}",
                        text_box.uuid
                    ),
                ));
            }
        } else {
            diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch012,
                Severity::Error,
                format!(
                    "group {group_id} description text box {} has no usable geometry for frame-clearance validation",
                    text_box.uuid
                ),
            ));
        }
    }
    diagnostics
}
