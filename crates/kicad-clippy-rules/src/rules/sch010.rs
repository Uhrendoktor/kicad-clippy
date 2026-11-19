//! Checks `SCH010` visible symbol-field clearance from nearby schematic objects.

use crate::HALF_FACTOR;
use geo::Contains;
use geo::Coord;
use geo::Distance;
use geo::Euclidean;
use geo::Intersects;
use geo::LineString;
use geo::Point as GeoPoint;
use geo::Polygon;
use geo::Rect as GeoRect;
use kicad_clippy_core::BuiltinRuleId;
use kicad_clippy_core::Diagnostic;
use kiutils_sexpr::Node;
use super::Bounds;
use super::GEOMETRY_TOLERANCE_MM;
use super::Point;
use super::SchematicFacts;
use super::SchematicPrimitiveFact;
use super::Segment;
use super::TWO_SIDED_GEOMETRY_TOLERANCE_MM;
use super::at_span;
use super::bounds_overlap;
use super::child_list;
use super::children;
use super::collect_label_nodes;
use super::collect_visible_field_bounds;
use super::distance;
use super::list_items;
use super::number;
use super::strict_line_hits_bounds;
use super::text_visual_bounds;

/// Checks visible symbol fields against other fields, symbol bodies, wires, pins, labels, and annotation boxes.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
/// * `segments` — Parsed wire segments used to determine geometry, topology, or obstacle intersections.
/// * `facts` — Normalized facts extracted from the schematic.
///
/// # Returns
///
/// SCH010 diagnostics for visible reference/value fields that collide with other fields, symbols, wires, pins, labels, or annotations.
pub(super) fn check_symbol_field_clearance(
    root: &Node,
    segments: &[Segment],
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    let fields = collect_visible_field_bounds(root);
    let label_bounds: Vec<_> = collect_label_nodes(root)
        .into_iter()
        .filter_map(|(kind, name, _point, _span, node)| {
            text_visual_bounds(&name, node).map(|bounds| (kind, name, bounds))
        })
        .collect();
    let text_boxes: Vec<_> = children(root, "text_box")
        .filter_map(text_box_bounds)
        .collect();
    let mut diagnostics = Vec::new();

    for (index, (field_label, bounds, span)) in fields.iter().enumerate() {
        let (reference, name) = field_label
            .split_once(' ')
            .unwrap_or((field_label.as_str(), ""));
        for primitive in &facts.symbol_primitives {
            if !symbol_primitive_intersects_bounds(*bounds, primitive) {
                continue;
            }
            let contained_value = primitive.reference == reference
                && name == "Value"
                && symbol_primitive_contains_bounds(*bounds, primitive);
            if contained_value {
                continue;
            }
            let description = if primitive.reference == reference {
                "its own symbol body"
            } else {
                "another symbol body"
            };
            diagnostics.push(at_span(
                BuiltinRuleId::Sch010,
                format!(
                    "{field_label} overlaps {description} {}",
                    primitive.reference
                ),
                *span,
            ));
        }

        for segment in segments {
            if strict_line_hits_bounds(*segment, *bounds) {
                diagnostics.push(at_span(
                    BuiltinRuleId::Sch010,
                    format!("{field_label} overlaps a wire segment"),
                    *span,
                ));
            }
        }
        for pin in &facts.pins {
            let point = Point {
                x: pin.position_mm.0,
                y: pin.position_mm.1,
            };
            if point_in_bounds(point, *bounds) {
                diagnostics.push(at_span(
                    BuiltinRuleId::Sch010,
                    format!(
                        "{field_label} overlaps pin {}.{}",
                        pin.reference, pin.number
                    ),
                    *span,
                ));
            }
        }
        for (kind, label, label_bounds) in &label_bounds {
            if bounds_overlap(*bounds, *label_bounds) {
                diagnostics.push(at_span(
                    BuiltinRuleId::Sch010,
                    format!("{field_label} overlaps {kind} {label:?}"),
                    *span,
                ));
            }
        }
        for box_bounds in &text_boxes {
            if bounds_overlap(*bounds, *box_bounds) {
                diagnostics.push(at_span(
                    BuiltinRuleId::Sch010,
                    format!("{field_label} overlaps an annotation text box"),
                    *span,
                ));
            }
        }
        for (other_label, other_bounds, _other_span) in fields.iter().skip(index + 1) {
            if bounds_overlap(*bounds, *other_bounds) {
                diagnostics.push(at_span(
                    BuiltinRuleId::Sch010,
                    format!("visible symbol fields overlap: {field_label} and {other_label}"),
                    *span,
                ));
            }
        }
    }
    diagnostics
}
/// Converts axis-aligned schematic bounds into a `geo` rectangle.
///
/// # Arguments
///
/// * `bounds` — Axis-aligned rectangle or primitive bounds in schematic millimetres.
///
/// # Returns
///
/// A normalized `geo::Rect<f64>` enclosing the supplied schematic bounds, with the lesser coordinates as its minimum corner.
fn geo_rect(bounds: Bounds) -> GeoRect<f64> {
    let opposite_x = bounds.x + bounds.width;
    let opposite_y = bounds.y + bounds.height;
    GeoRect::new(
        Coord {
            x: bounds.x.min(opposite_x),
            y: bounds.y.min(opposite_y),
        },
        Coord {
            x: bounds.x.max(opposite_x),
            y: bounds.y.max(opposite_y),
        },
    )
}
/// Tests whether a point lies within schematic bounds, expanded by the geometry tolerance.
///
/// # Arguments
///
/// * `point` — Schematic coordinate being tested, in millimetres.
/// * `bounds` — Axis-aligned rectangle or primitive bounds in schematic millimetres.
///
/// # Returns
///
/// `true` when the point lies inside the rectangle after expanding its edges by the schematic geometry tolerance.
fn point_in_bounds(point: Point, bounds: Bounds) -> bool {
    let expanded = Bounds {
        x: bounds.x - GEOMETRY_TOLERANCE_MM,
        y: bounds.y - GEOMETRY_TOLERANCE_MM,
        width: bounds.width + TWO_SIDED_GEOMETRY_TOLERANCE_MM,
        height: bounds.height + TWO_SIDED_GEOMETRY_TOLERANCE_MM,
    };
    geo_rect(expanded).contains(&GeoPoint::new(point.x, point.y))
}
/// Calculates the Euclidean distance from a point to an axis-aligned bounds rectangle.
///
/// # Arguments
///
/// * `point` — Schematic coordinate being tested, in millimetres.
/// * `bounds` — Axis-aligned rectangle or primitive bounds in schematic millimetres.
///
/// # Returns
///
/// The shortest Euclidean distance in millimetres.
fn point_bounds_distance(point: Point, bounds: Bounds) -> f64 {
    Euclidean.distance(&GeoPoint::new(point.x, point.y), &geo_rect(bounds))
}
/// Converts a closed schematic primitive with at least three points into a polygon.
///
/// # Arguments
///
/// * `primitive` — Embedded or sheet-level primitive whose body and stroke are checked against the wire.
///
/// # Returns
///
/// A polygon built from a closed primitive with at least three points, or `None` for open or incomplete geometry.
fn schematic_polygon(primitive: &SchematicPrimitiveFact) -> Option<Polygon<f64>> {
    if !primitive.closed || primitive.points_mm.len() < 3 {
        return None;
    }
    let mut ring: Vec<_> = primitive
        .points_mm
        .iter()
        .map(|(x, y)| Coord { x: *x, y: *y })
        .collect();
    if ring.first() != ring.last() {
        if let Some(first) = ring.first().copied() {
            ring.push(first);
        }
    }
    Some(Polygon::new(LineString::new(ring), Vec::new()))
}
/// Converts a schematic primitive's path points into a `geo` line string.
///
/// # Arguments
///
/// * `primitive` — Embedded or sheet-level primitive whose body and stroke are checked against the wire.
///
/// # Returns
///
/// A `geo::LineString<f64>` containing the primitive's ordered sheet-coordinate points.
fn schematic_path(primitive: &SchematicPrimitiveFact) -> LineString<f64> {
    LineString::new(
        primitive
            .points_mm
            .iter()
            .map(|(x, y)| Coord { x: *x, y: *y })
            .collect(),
    )
}
/// Tests whether the stroked geometry of a symbol primitive intersects the supplied bounds.
///
/// # Arguments
///
/// * `bounds` — Axis-aligned rectangle or primitive bounds in schematic millimetres.
/// * `primitive` — Embedded or sheet-level primitive whose body and stroke are checked against the wire.
///
/// # Returns
///
/// `true` when the primitive's stroked geometry intersects the supplied rectangle.
fn symbol_primitive_intersects_bounds(bounds: Bounds, primitive: &SchematicPrimitiveFact) -> bool {
    if let Some((center_x, center_y, radius)) = primitive.circle_mm {
        let center = Point {
            x: center_x,
            y: center_y,
        };
        return point_bounds_distance(center, bounds)
            <= primitive.stroke_width_mm.mul_add(HALF_FACTOR, radius) + GEOMETRY_TOLERANCE_MM;
    }

    let rectangle = geo_rect(bounds);
    schematic_polygon(primitive).map_or_else(
        || schematic_path(primitive).intersects(&rectangle),
        |polygon| polygon.intersects(&rectangle),
    )
}
/// Tests whether the supplied bounds are contained within a closed symbol primitive's interior.
///
/// # Arguments
///
/// * `bounds` — Axis-aligned rectangle or primitive bounds in schematic millimetres.
/// * `primitive` — Embedded or sheet-level primitive whose body and stroke are checked against the wire.
///
/// # Returns
///
/// `true` when the entire supplied rectangle lies inside the closed primitive's interior after accounting for its stroke.
fn symbol_primitive_contains_bounds(bounds: Bounds, primitive: &SchematicPrimitiveFact) -> bool {
    if let Some((center_x, center_y, radius)) = primitive.circle_mm {
        let inner_radius = primitive.stroke_width_mm.mul_add(-HALF_FACTOR, radius);
        if inner_radius <= 0.0 {
            return false;
        }
        let center = Point {
            x: center_x,
            y: center_y,
        };
        // Circle containment remains explicit because the current primitive
        // fact stores a circle as center/radius rather than a polygonal ring.
        let opposite_x = bounds.x + bounds.width;
        let opposite_y = bounds.y + bounds.height;
        return [
            Point {
                x: bounds.x,
                y: bounds.y,
            },
            Point {
                x: opposite_x,
                y: bounds.y,
            },
            Point {
                x: opposite_x,
                y: opposite_y,
            },
            Point {
                x: bounds.x,
                y: opposite_y,
            },
        ]
        .iter()
        .all(|corner| distance(*corner, center) <= inner_radius + GEOMETRY_TOLERANCE_MM);
    }

    schematic_polygon(primitive).is_some_and(|polygon| polygon.contains(&geo_rect(bounds)))
}
/// Checks whether a symbol property has its `KiCad` hide flag enabled.
///
/// # Arguments
///
/// * `property` — Symbol property node whose visibility flag is being checked.
///
/// # Returns
///
/// `true` when the property's `effects` list contains `KiCad`'s `hide` flag.
fn property_is_hidden(property: &Node) -> bool {
    child_list(property, "effects").is_some_and(|effects| child_list(effects, "hide").is_some())
}
/// Reads a text box's anchor and size and returns its axis-aligned bounds.
///
/// # Arguments
///
/// * `node` — S-expression node whose properties, coordinates, or geometry are being decoded.
///
/// # Returns
///
/// Axis-aligned bounds of the text box in schematic coordinates, or `None` when its anchor or dimensions are unavailable.
fn text_box_bounds(node: &Node) -> Option<Bounds> {
    let at = child_list(node, "at").and_then(list_items)?;
    let size = child_list(node, "size").and_then(list_items)?;
    Some(Bounds {
        x: number(at.get(1)?)?,
        y: number(at.get(2)?)?,
        width: number(size.get(1)?)?,
        height: number(size.get(2)?)?,
    })
}
