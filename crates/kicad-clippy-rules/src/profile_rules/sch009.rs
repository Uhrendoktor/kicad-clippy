//! Checks `SCH009` containment of group members and their associated schematic geometry.

use crate::generic::GEOMETRY_TOLERANCE_MM;
use crate::generic::HALF_FACTOR;
use crate::generic::physical_wire_component_ids;
use crate::generic::STROKE_CLEARANCE_MARGIN_MM;
use kicad_clippy_core::BuiltinRuleId;
use kicad_clippy_core::Diagnostic;
use kicad_clippy_core::Severity;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use super::COORDINATE_KEY_DECIMAL_PLACES;
use super::group_wire_components;
use super::GroupIntent;
use super::point_inside_frame_with_clearance;
use super::point_inside_frame;
use super::point_is_on_wire;
use super::rectangle_inside_frame_with_clearance;
use super::rectangle_inside_frame;
use super::rectangles_overlap;
use super::SchematicFacts;
use super::symbol_primitive_inside_frame;
use super::valid_frame_bounds;
use super::validate_group_annotation_box_containment;
use super::validate_group_containment;
use super::validate_group_description_containment;
use super::validate_group_field_containment;
use super::validate_group_graphic_containment;
use super::validate_group_image_containment;
use super::validate_group_label_containment;
use super::validate_group_pin_containment;
use super::validate_group_plain_text_containment;
use super::validate_group_symbol_primitive_containment;
use super::validate_group_wire_containment;
use super::wire_belongs_to_group;
use super::wire_overlaps_frame;

/// Checks that every declared group member and its attached geometry lies inside the group's referenced frame.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
///
/// # Returns
///
/// Diagnostics for group members or attached schematic objects that extend outside the frame assigned to the group.
pub(super) fn validate_group_containment(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    let Some(frame_uuid) = group.frame_uuid.as_deref() else {
        return Vec::new();
    };
    let Some(frame) = facts.frames.iter().find(|frame| frame.uuid == frame_uuid) else {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch009,
            Severity::Error,
            format!("group {group_id} references missing schematic frame UUID {frame_uuid}"),
        )];
    };
    let bounds = frame.bounds_mm;
    if !valid_frame_bounds(bounds) {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch009,
            Severity::Error,
            format!("group {group_id} frame {frame_uuid} has invalid or zero-area bounds"),
        )];
    }

    let mut diagnostics = Vec::new();
    for reference in &group.members {
        let Some(component) = facts
            .components
            .iter()
            .find(|component| component.reference == *reference)
        else {
            continue;
        };
        let Some(position) = component.position_mm else {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch009,
                Severity::Error,
                format!(
                    "group {group_id} member {reference} has no placed-symbol origin for frame containment validation"
                ),
            ));
            continue;
        };
        if !point_inside_frame(bounds, position) {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch009,
                Severity::Error,
                format!(
                    "group {group_id} member {reference} origin ({:.2}, {:.2}) mm is outside frame {frame_uuid}",
                    position.0, position.1
                ),
            ));
        }
    }
    diagnostics.extend(validate_group_pin_containment(
        group_id, group, facts, frame_uuid, bounds,
    ));
    diagnostics.extend(validate_group_field_containment(
        group_id, group, facts, frame_uuid, bounds,
    ));
    diagnostics.extend(validate_group_description_containment(
        group_id, group, facts, frame_uuid, bounds,
    ));
    diagnostics.extend(validate_group_symbol_primitive_containment(
        group_id, group, facts, frame_uuid, bounds,
    ));
    diagnostics.extend(validate_group_wire_containment(
        group_id, group, facts, frame_uuid, bounds,
    ));
    diagnostics.extend(validate_group_label_containment(
        group_id, group, facts, frame_uuid, bounds,
    ));
    diagnostics.extend(validate_group_annotation_box_containment(
        group_id, group, facts, frame_uuid, bounds,
    ));
    diagnostics.extend(validate_group_plain_text_containment(
        group_id, facts, frame_uuid, bounds,
    ));
    diagnostics.extend(validate_group_graphic_containment(
        group_id, facts, frame_uuid, bounds,
    ));
    diagnostics.extend(validate_group_image_containment(
        group_id, facts, frame_uuid, bounds,
    ));
    diagnostics
}
/// Checks that every pin belonging to a group member lies inside the frame bounds.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
///
/// # Returns
///
/// Diagnostics for pins belonging to group members whose anchors lie outside the referenced frame.
fn validate_group_pin_containment(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
) -> Vec<Diagnostic> {
    facts
        .pins
        .iter()
        .filter(|pin| group.members.contains(&pin.reference))
        .filter(|pin| !point_inside_frame(frame_bounds, pin.position_mm))
        .map(|pin| {
            Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch009,
                Severity::Error,
                format!(
                    "group {group_id} member pin {}.{} at ({:.2}, {:.2}) mm is outside frame {frame_uuid}",
                    pin.reference, pin.number, pin.position_mm.0, pin.position_mm.1
                ),
            )
        })
        .collect()
}
/// Checks that visible reference/value fields for group members lie inside the frame bounds.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
///
/// # Returns
///
/// Diagnostics for visible reference or value fields belonging to group members that extend outside the frame.
fn validate_group_field_containment(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
) -> Vec<Diagnostic> {
    facts
        .visible_fields
        .iter()
        .filter(|field| group.members.contains(&field.reference))
        .filter(|field| !rectangle_inside_frame(field.bounds_mm, frame_bounds))
        .map(|field| {
            Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch009,
                Severity::Error,
                format!(
                    "group {group_id} member field {} {} extends outside frame {frame_uuid}",
                    field.reference, field.name
                ),
            )
        })
        .collect()
}
/// Checks that the description text box referenced by the group lies inside the frame bounds.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
///
/// # Returns
///
/// Diagnostics when the group's referenced description text box extends beyond the frame bounds.
fn validate_group_description_containment(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
) -> Vec<Diagnostic> {
    let Some(description) = facts
        .text_boxes
        .iter()
        .find(|text_box| text_box.uuid == group.description_uuid)
    else {
        return Vec::new();
    };
    let Some(description_bounds) = description.bounds_mm else {
        return Vec::new();
    };
    if rectangle_inside_frame(description_bounds, frame_bounds) {
        return Vec::new();
    }
    vec![Diagnostic::for_builtin_rule(
        BuiltinRuleId::Sch009,
        Severity::Error,
        format!(
            "group {group_id} description text box {} extends outside frame {frame_uuid}",
            group.description_uuid
        ),
    )]
}
/// Checks that symbol-library primitives for group members lie inside the frame bounds.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
///
/// # Returns
///
/// Diagnostics for embedded symbol primitives whose transformed geometry extends outside the group frame.
fn validate_group_symbol_primitive_containment(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
) -> Vec<Diagnostic> {
    facts
        .symbol_primitives
        .iter()
        .filter(|primitive| group.members.contains(&primitive.reference))
        .filter(|primitive| !symbol_primitive_inside_frame(primitive, frame_bounds, 0.0))
        .map(|primitive| {
            Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch009,
                Severity::Error,
                format!(
                    "group {group_id} member {} symbol primitive extends outside frame {frame_uuid}",
                    primitive.reference
                ),
            )
        })
        .collect()
}
/// Tests whether a symbol primitive's stroked geometry is contained within the supplied frame rectangle.
///
/// # Arguments
///
/// * `primitive` — The transformed symbol primitive whose stroked geometry must fit inside the frame.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
/// * `clearance` — Minimum required distance from each frame edge, in millimetres.
///
/// # Returns
///
/// `true` when the primitive's stroked geometry is fully contained in the frame's clearance inset.
pub(super) fn symbol_primitive_inside_frame(
    primitive: &crate::generic::SchematicPrimitiveFact,
    frame_bounds: (f64, f64, f64, f64),
    clearance: f64,
) -> bool {
    let stroke_margin = if primitive.stroke_width_mm.is_finite() {
        primitive.stroke_width_mm.abs() * HALF_FACTOR
    } else {
        0.0
    };
    let margin = clearance + stroke_margin + STROKE_CLEARANCE_MARGIN_MM;
    if let Some((center_x, center_y, radius)) = primitive.circle_mm {
        let radius = radius.abs();
        return rectangle_inside_frame_with_clearance(
            (
                center_x - radius,
                center_y - radius,
                center_x + radius,
                center_y + radius,
            ),
            frame_bounds,
            margin,
        );
    }
    !primitive.points_mm.is_empty()
        && primitive
            .points_mm
            .iter()
            .all(|point| point_inside_frame_with_clearance(frame_bounds, *point, margin))
}
/// Resolves the connected wire components that contain the group's member pins and returns the corresponding pin-to-component map.
///
/// # Arguments
///
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
///
/// # Returns
///
/// Mappings from normalized pin coordinates to their physical wire-component identifiers, together with the related component lookup needed by the caller.
pub(super) fn group_wire_components(
    group: &GroupIntent,
    facts: &SchematicFacts,
) -> (BTreeMap<String, usize>, BTreeSet<usize>) {
    let component_ids = physical_wire_component_ids(facts);
    let member_components = facts
        .pins
        .iter()
        .filter(|pin| group.members.contains(&pin.reference))
        .filter(|pin| point_is_on_wire(pin.position_mm, facts))
        .filter_map(|pin| {
            let key = format!(
                "{:.*}:{:.*}",
                COORDINATE_KEY_DECIMAL_PLACES,
                pin.position_mm.0,
                COORDINATE_KEY_DECIMAL_PLACES,
                pin.position_mm.1
            );
            component_ids.get(&key).copied()
        })
        .collect();
    (component_ids, member_components)
}
/// Checks whether a wire segment belongs to one of the physical components reached from the group's member pins.
///
/// # Arguments
///
/// * `wire` — Parsed wire segment being tested for membership in the group's connected components.
/// * `component_ids` — Map from tolerance-normalized schematic coordinates to physical wire-component identifiers.
/// * `member_components` — Physical wire-component identifiers reached from the group's member pins.
///
/// # Returns
///
/// `true` when the wire's connected component is among the components reached from the group pins.
pub(super) fn wire_belongs_to_group(
    wire: &crate::generic::SchematicWireFact,
    component_ids: &BTreeMap<String, usize>,
    member_components: &BTreeSet<usize>,
) -> bool {
    [
        (wire.start_mm.0, wire.start_mm.1),
        (wire.end_mm.0, wire.end_mm.1),
    ]
    .iter()
    .filter_map(|(x, y)| component_ids.get(&format!("{x:.3}:{y:.3}")))
    .any(|component| member_components.contains(component))
}
/// Checks that every wire segment in the group's physical components remains inside the referenced frame.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
///
/// # Returns
///
/// Diagnostics for wire segments in the group's physical components that extend outside the referenced frame.
fn validate_group_wire_containment(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
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
            !point_inside_frame(frame_bounds, wire.start_mm)
                || !point_inside_frame(frame_bounds, wire.end_mm)
        })
        .map(|wire| {
            Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch009,
                Severity::Error,
                format!(
                    "group {group_id} member wire segment from ({:.2}, {:.2}) to ({:.2}, {:.2}) extends outside frame {frame_uuid}",
                    wire.start_mm.0, wire.start_mm.1, wire.end_mm.0, wire.end_mm.1
                ),
            )
        })
        .collect()
}
/// Checks that labels attached to the group's physical wire components lie inside the referenced frame.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
///
/// # Returns
///
/// Diagnostics for labels attached to the group's physical wire components when their anchors lie outside the frame.
fn validate_group_label_containment(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
) -> Vec<Diagnostic> {
    let (component_ids, member_components) = group_wire_components(group, facts);
    facts
        .labels
        .iter()
        .filter(|label| {
            let key = format!(
                "{:.*}:{:.*}",
                COORDINATE_KEY_DECIMAL_PLACES,
                label.position_mm.0,
                COORDINATE_KEY_DECIMAL_PLACES,
                label.position_mm.1
            );
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
                || !point_inside_frame(frame_bounds, label.position_mm),
                |bounds| !rectangle_inside_frame(bounds, frame_bounds),
            )
        })
        .map(|label| {
            Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch009,
                Severity::Error,
                format!(
                    "group {group_id} member wire label {} {:?} extends outside frame {frame_uuid}",
                    label.kind, label.name
                ),
            )
        })
        .collect()
}
/// Checks that annotation text boxes associated with group members lie inside the referenced frame.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
///
/// # Returns
///
/// Diagnostics for associated annotation text boxes that overlap the frame but are not fully contained by it.
fn validate_group_annotation_box_containment(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
) -> Vec<Diagnostic> {
    facts
        .text_boxes
        .iter()
        .filter(|text_box| text_box.uuid != group.description_uuid)
        .filter_map(|text_box| {
            let bounds = text_box.bounds_mm?;
            (rectangles_overlap(bounds, frame_bounds)
                && !rectangle_inside_frame(bounds, frame_bounds))
            .then(|| {
                Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch009,
                    Severity::Error,
                    format!(
                        "group {group_id} annotation text box {} extends outside frame {frame_uuid}",
                        text_box.uuid
                    ),
                )
            })
        })
        .collect()
}
/// Checks that standalone schematic text lies inside the referenced frame.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
///
/// # Returns
///
/// Diagnostics for standalone schematic text that overlaps the frame but extends beyond its edges.
fn validate_group_plain_text_containment(
    group_id: &str,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
) -> Vec<Diagnostic> {
    facts
        .texts
        .iter()
        .filter_map(|text| {
            let bounds = text.bounds_mm?;
            (rectangles_overlap(bounds, frame_bounds)
                && !rectangle_inside_frame(bounds, frame_bounds))
            .then(|| {
                Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch009,
                    Severity::Error,
                    format!(
                        "group {group_id} plain text annotation {:?} extends outside frame {frame_uuid}",
                        text.text
                    ),
                )
            })
        })
        .collect()
}
/// Checks that sheet-level graphical primitives lie inside the referenced frame.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
///
/// # Returns
///
/// Diagnostics for sheet-level graphic bounds that overlap the frame but are not fully contained.
fn validate_group_graphic_containment(
    group_id: &str,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
) -> Vec<Diagnostic> {
    facts
        .graphics
        .iter()
        .filter(|graphic| graphic.uuid.as_deref() != Some(frame_uuid))
        .filter_map(|graphic| {
            let bounds = graphic.bounds_mm?;
            (rectangles_overlap(bounds, frame_bounds)
                && !rectangle_inside_frame(bounds, frame_bounds))
            .then(|| {
                Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch009,
                    Severity::Error,
                    format!(
                        "group {group_id} vector graphic {} {:?} extends outside frame {frame_uuid}",
                        graphic.kind, graphic.uuid
                    ),
                )
            })
        })
        .collect()
}
/// Checks that bitmap images associated with the group lie inside the referenced frame.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `frame_uuid` — UUID of the schematic frame assigned to this group.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
///
/// # Returns
///
/// Diagnostics for bitmap image bounds that overlap the frame but extend beyond its edges.
fn validate_group_image_containment(
    group_id: &str,
    facts: &SchematicFacts,
    frame_uuid: &str,
    frame_bounds: (f64, f64, f64, f64),
) -> Vec<Diagnostic> {
    facts
        .images
        .iter()
        .filter_map(|image| {
            let bounds = image.bounds_mm?;
            (rectangles_overlap(bounds, frame_bounds)
                && !rectangle_inside_frame(bounds, frame_bounds))
            .then(|| {
                Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch009,
                    Severity::Error,
                    format!(
                        "group {group_id} bitmap image {:?} extends outside frame {frame_uuid}",
                        image.uuid
                    ),
                )
            })
        })
        .collect()
}
/// Tests whether a wire segment intersects or crosses the supplied frame boundary.
///
/// # Arguments
///
/// * `wire` — Parsed wire segment being tested for membership in the group's connected components.
/// * `frame_bounds` — Frame bounds in millimetres, ordered as (left, top, right, bottom).
///
/// # Returns
///
/// `true` when the wire segment intersects the frame rectangle or its boundary.
pub(super) fn wire_overlaps_frame(
    wire: &crate::generic::SchematicWireFact,
    frame_bounds: (f64, f64, f64, f64),
) -> bool {
    let (x1, y1) = wire.start_mm;
    let (x2, y2) = wire.end_mm;
    let (left, top, right, bottom) = frame_bounds;
    if (y1 - y2).abs() < GEOMETRY_TOLERANCE_MM {
        y1 >= top - GEOMETRY_TOLERANCE_MM
            && y1 <= bottom + GEOMETRY_TOLERANCE_MM
            && x1.min(x2) <= right + GEOMETRY_TOLERANCE_MM
            && x1.max(x2) >= left - GEOMETRY_TOLERANCE_MM
    } else if (x1 - x2).abs() < GEOMETRY_TOLERANCE_MM {
        x1 >= left - GEOMETRY_TOLERANCE_MM
            && x1 <= right + GEOMETRY_TOLERANCE_MM
            && y1.min(y2) <= bottom + GEOMETRY_TOLERANCE_MM
            && y1.max(y2) >= top - GEOMETRY_TOLERANCE_MM
    } else {
        rectangles_overlap(
            (x1.min(x2), y1.min(y2), x1.max(x2), y1.max(y2)),
            frame_bounds,
        )
    }
}
