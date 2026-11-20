//! Supported transformed symbol-body shapes and distance calculations for profile layout checks.

use crate::generic::{GEOMETRY_TOLERANCE_MM, SchematicFacts, SchematicPrimitiveFact};
use crate::HALF_FACTOR;

/// Resolved symbol-body geometry for one placed reference.
///
/// Curve-based primitives are marked unresolved because their body outline is
/// not represented by the original control points.
#[derive(Clone, Debug, Default)]
pub(super) struct BodyGeometrySet {
    /// Supported body shapes transformed into schematic coordinates.
    pub(super) shapes: Vec<BodyShape>,
    /// Whether one or more primitives could not be represented without guessing.
    pub(super) has_unsupported_geometry: bool,
}

/// A supported closed body, circle, or open stroked body path.
#[derive(Clone, Debug)]
pub(super) enum BodyShape {
    /// Circular body represented by its center and radius.
    Circle {
        /// Circle center in schematic millimetres.
        center_mm: (f64, f64),
        /// Circle radius in millimetres.
        radius_mm: f64,
        /// Half of the declared stroke width in millimetres.
        stroke_half_mm: f64,
    },
    /// Filled or closed polygonal body.
    Polygon {
        /// Ordered polygon vertices in schematic millimetres.
        points_mm: Vec<(f64, f64)>,
        /// Half of the declared stroke width in millimetres.
        stroke_half_mm: f64,
    },
    /// Open polyline body represented as a stroked path.
    Path {
        /// Ordered path vertices in schematic millimetres.
        points_mm: Vec<(f64, f64)>,
        /// Half of the declared stroke width in millimetres.
        stroke_half_mm: f64,
    },
}

/// Collects the supported selected-unit body primitives belonging to one reference.
///
/// # Arguments
///
/// * `reference` — Placed component reference designator.
/// * `facts` — Normalized schematic facts containing transformed symbol primitives.
///
/// # Returns
///
/// Supported body shapes plus an unresolved flag when a curve or invalid primitive
/// prevents the complete body outline from being represented.
pub(super) fn body_geometry_for_reference(
    reference: &str,
    facts: &SchematicFacts,
) -> BodyGeometrySet {
    let mut body = BodyGeometrySet::default();
    for primitive in facts
        .symbol_primitives
        .iter()
        .filter(|primitive| primitive.reference == reference)
    {
        match body_shape_from_primitive(primitive) {
            Some(shape) => body.shapes.push(shape),
            None => body.has_unsupported_geometry = true,
        }
    }
    body
}

/// Returns the minimum clearance between two complete supported symbol bodies.
///
/// # Arguments
///
/// * `first` — First placed symbol's resolved body geometry.
/// * `second` — Second placed symbol's resolved body geometry.
///
/// # Returns
///
/// The non-negative minimum edge-to-edge clearance in millimetres, or `None`
/// when either body is missing or contains unsupported geometry.
pub(super) fn minimum_body_gap(
    first: &BodyGeometrySet,
    second: &BodyGeometrySet,
) -> Option<f64> {
    if !body_is_complete(first) || !body_is_complete(second) {
        return None;
    }
    let mut minimum = f64::INFINITY;
    for first_shape in &first.shapes {
        for second_shape in &second.shapes {
            minimum = minimum.min(shape_gap(first_shape, second_shape));
        }
    }
    minimum.is_finite().then_some(minimum.max(0.0))
}

/// Returns the minimum distance between a complete symbol body and a visible field rectangle.
///
/// # Arguments
///
/// * `body` — Placed symbol's resolved body geometry.
/// * `bounds_mm` — Visible field glyph rectangle as left, top, right, bottom coordinates.
///
/// # Returns
///
/// Zero when the glyph rectangle intersects or lies within a body; otherwise the
/// shortest edge-to-edge gap in millimetres. Returns `None` for incomplete geometry.
pub(super) fn minimum_body_to_bounds_gap(
    body: &BodyGeometrySet,
    bounds_mm: (f64, f64, f64, f64),
) -> Option<f64> {
    if !body_is_complete(body) {
        return None;
    }
    let (left, top, right, bottom) = bounds_mm;
    if ![left, top, right, bottom].iter().all(|value| value.is_finite())
        || right <= left
        || bottom <= top
    {
        return None;
    }
    let rectangle = [
        (left, top),
        (right, top),
        (right, bottom),
        (left, bottom),
    ];
    let mut minimum = f64::INFINITY;
    for shape in &body.shapes {
        let gap = match shape {
            BodyShape::Circle {
                center_mm,
                radius_mm,
                stroke_half_mm,
            } => {
                point_polygon_gap(*center_mm, &rectangle)
                    - radius_mm
                    - stroke_half_mm
            }
            BodyShape::Polygon {
                points_mm,
                stroke_half_mm,
            } => polygon_polygon_gap(points_mm, &rectangle) - stroke_half_mm,
            BodyShape::Path {
                points_mm,
                stroke_half_mm,
            } => path_polygon_gap(points_mm, &rectangle) - stroke_half_mm,
        };
        minimum = minimum.min(gap.max(0.0));
    }
    minimum.is_finite().then_some(minimum.max(0.0))
}

/// Returns whether at least one supported shape is available and no curve geometry is unresolved.
fn body_is_complete(body: &BodyGeometrySet) -> bool {
    !body.has_unsupported_geometry && !body.shapes.is_empty()
}

/// Returns the non-negative edge-to-edge gap between two supported body shapes.
fn shape_gap(first: &BodyShape, second: &BodyShape) -> f64 {
    match (first, second) {
        (
            BodyShape::Circle {
                center_mm: first_center,
                radius_mm: first_radius,
                stroke_half_mm: first_stroke,
            },
            BodyShape::Circle {
                center_mm: second_center,
                radius_mm: second_radius,
                stroke_half_mm: second_stroke,
            },
        ) => point_distance(*first_center, *second_center)
            - first_radius
            - second_radius
            - first_stroke
            - second_stroke,
        (
            BodyShape::Circle {
                center_mm,
                radius_mm,
                stroke_half_mm,
            },
            BodyShape::Polygon {
                points_mm,
                stroke_half_mm: polygon_stroke,
            },
        )
        | (
            BodyShape::Polygon {
                points_mm,
                stroke_half_mm: polygon_stroke,
            },
            BodyShape::Circle {
                center_mm,
                radius_mm,
                stroke_half_mm,
            },
        ) => point_polygon_gap(*center_mm, points_mm)
            - radius_mm
            - stroke_half
            - polygon_stroke,
        (
            BodyShape::Circle {
                center_mm,
                radius_mm,
                stroke_half_mm,
            },
            BodyShape::Path {
                points_mm,
                stroke_half_mm: path_stroke,
            },
        )
        | (
            BodyShape::Path {
                points_mm,
                stroke_half_mm: path_stroke,
            },
            BodyShape::Circle {
                center_mm,
                radius_mm,
                stroke_half_mm,
            },
        ) => point_path_distance(*center_mm, points_mm)
            - radius_mm
            - stroke_half
            - path_stroke,
        (
            BodyShape::Polygon {
                points_mm: first_points,
                stroke_half_mm: first_stroke,
            },
            BodyShape::Polygon {
                points_mm: second_points,
                stroke_half_mm: second_stroke,
            },
        ) => polygon_polygon_gap(first_points, second_points) - first_stroke - second_stroke,
        (
            BodyShape::Polygon {
                points_mm: polygon,
                stroke_half_mm: polygon_stroke,
            },
            BodyShape::Path {
                points_mm: path,
                stroke_half_mm: path_stroke,
            },
        )
        | (
            BodyShape::Path {
                points_mm: path,
                stroke_half_mm: path_stroke,
            },
            BodyShape::Polygon {
                points_mm: polygon,
                stroke_half_mm: polygon_stroke,
            },
        ) => path_polygon_gap(path, polygon) - polygon_stroke - path_stroke,
        (
            BodyShape::Path {
                points_mm: first_points,
                stroke_half_mm: first_stroke,
            },
            BodyShape::Path {
                points_mm: second_points,
                stroke_half_mm: second_stroke,
            },
        ) => path_path_gap(first_points, second_points) - first_stroke - second_stroke,
    }
}

/// Returns the distance from a point to a polygonal area, including zero for containment.
fn point_polygon_gap(point: (f64, f64), polygon: &[(f64, f64)]) -> f64 {
    if point_in_polygon(point, polygon) {
        return 0.0;
    }
    polygon_edges(polygon)
        .map(|(start, end)| point_segment_distance(point, start, end))
        .fold(f64::INFINITY, f64::min)
}

/// Returns the edge-to-edge gap between two polygonal areas, treating overlap and containment as zero.
fn polygon_polygon_gap(first: &[(f64, f64)], second: &[(f64, f64)]) -> f64 {
    if first.is_empty() || second.is_empty() {
        return f64::INFINITY;
    }
    if point_in_polygon(first[0], second)
        || point_in_polygon(second[0], first)
        || polygon_edges(first).any(|(a, b)| {
            polygon_edges(second).any(|(c, d)| segments_intersect(a, b, c, d))
        })
    {
        return 0.0;
    }
    polygon_edges(first)
        .flat_map(|(a, b)| {
            polygon_edges(second).map(move |(c, d)| segment_segment_distance(a, b, c, d))
        })
        .fold(f64::INFINITY, f64::min)
}

/// Returns the edge-to-edge gap between an open path and a polygonal area.
fn path_polygon_gap(path: &[(f64, f64)], polygon: &[(f64, f64)]) -> f64 {
    if path.len() < 2 || polygon.len() < 3 {
        return f64::INFINITY;
    }
    if path.iter().any(|point| point_in_polygon(*point, polygon))
        || path.windows(2).any(|segment| {
            polygon_edges(polygon)
                .any(|(a, b)| segments_intersect(segment[0], segment[1], a, b))
        })
    {
        return 0.0;
    }
    path.windows(2)
        .flat_map(|segment| {
            polygon_edges(polygon)
                .map(move |(a, b)| segment_segment_distance(segment[0], segment[1], a, b))
        })
        .fold(f64::INFINITY, f64::min)
}

/// Returns the minimum edge-to-edge distance between two open polylines.
fn path_path_gap(first: &[(f64, f64)], second: &[(f64, f64)]) -> f64 {
    if first.len() < 2 || second.len() < 2 {
        return f64::INFINITY;
    }
    first
        .windows(2)
        .flat_map(|first_segment| {
            second.windows(2).map(move |second_segment| {
                segment_segment_distance(
                    first_segment[0],
                    first_segment[1],
                    second_segment[0],
                    second_segment[1],
                )
            })
        })
        .fold(f64::INFINITY, f64::min)
}

/// Returns the shortest distance between a point and an open polyline.
fn point_path_distance(point: (f64, f64), path: &[(f64, f64)]) -> f64 {
    path.windows(2)
        .map(|segment| point_segment_distance(point, segment[0], segment[1]))
        .fold(f64::INFINITY, f64::min)
}

/// Iterates the closed edges of a polygon with at least three vertices.
fn polygon_edges(points: &[(f64, f64)]) -> impl Iterator<Item = ((f64, f64), (f64, f64))> + '_ {
    points
        .iter()
        .enumerate()
        .map(move |(index, point)| (*point, points[(index + 1) % points.len()]))
}

/// Returns whether a point is inside or on the boundary of a polygon.
fn point_in_polygon(point: (f64, f64), polygon: &[(f64, f64)]) -> bool {
    if polygon.len() < 3 {
        return false;
    }
    if polygon_edges(polygon).any(|(start, end)| {
        point_segment_distance(point, start, end) <= GEOMETRY_TOLERANCE_MM
    }) {
        return true;
    }
    let mut inside = false;
    for (start, end) in polygon_edges(polygon) {
        if (start.1 > point.1) != (end.1 > point.1) {
            let crossing_x = (end.0 - start.0)
                .mul_add((point.1 - start.1) / (end.1 - start.1), start.0);
            if point.0 < crossing_x {
                inside = !inside;
            }
        }
    }
    inside
}

/// Returns the shortest Euclidean distance between a point and a finite segment.
fn point_segment_distance(point: (f64, f64), start: (f64, f64), end: (f64, f64)) -> f64 {
    let delta_x = end.0 - start.0;
    let delta_y = end.1 - start.1;
    let length_squared = delta_x.mul_add(delta_x, delta_y * delta_y);
    if length_squared <= GEOMETRY_TOLERANCE_MM * GEOMETRY_TOLERANCE_MM {
        return point_distance(point, start);
    }
    let projection = (((point.0 - start.0) * delta_x + (point.1 - start.1) * delta_y)
        / length_squared)
        .clamp(0.0, 1.0);
    let nearest = (
        projection.mul_add(delta_x, start.0),
        projection.mul_add(delta_y, start.1),
    );
    point_distance(point, nearest)
}

/// Returns the minimum distance between two finite segments, or zero when they intersect.
fn segment_segment_distance(
    first_start: (f64, f64),
    first_end: (f64, f64),
    second_start: (f64, f64),
    second_end: (f64, f64),
) -> f64 {
    if segments_intersect(first_start, first_end, second_start, second_end) {
        return 0.0;
    }
    point_segment_distance(first_start, second_start, second_end)
        .min(point_segment_distance(first_end, second_start, second_end))
        .min(point_segment_distance(second_start, first_start, first_end))
        .min(point_segment_distance(second_end, first_start, first_end))
}

/// Returns whether two finite segments intersect, including collinear and tolerance-close endpoints.
fn segments_intersect(
    first_start: (f64, f64),
    first_end: (f64, f64),
    second_start: (f64, f64),
    second_end: (f64, f64),
) -> bool {
    let first_first = orientation_sign(first_start, first_end, second_start);
    let first_second = orientation_sign(first_start, first_end, second_end);
    let second_first = orientation_sign(second_start, second_end, first_start);
    let second_second = orientation_sign(second_start, second_end, first_end);
    (first_first != first_second && second_first != second_second)
        || (first_first == 0 && point_on_segment(second_start, first_start, first_end))
        || (first_second == 0 && point_on_segment(second_end, first_start, first_end))
        || (second_first == 0 && point_on_segment(first_start, second_start, second_end))
        || (second_second == 0 && point_on_segment(first_end, second_start, second_end))
}

/// Returns the tolerance-aware orientation sign of three points.
fn orientation_sign(start: (f64, f64), end: (f64, f64), point: (f64, f64)) -> i8 {
    let cross = (end.0 - start.0) * (point.1 - start.1)
        - (end.1 - start.1) * (point.0 - start.0);
    let scale = point_distance(start, end).max(1.0);
    if cross.abs() <= GEOMETRY_TOLERANCE_MM * scale {
        0
    } else if cross < 0.0 {
        -1
    } else {
        1
    }
}

/// Returns whether a point lies within the axis-aligned bounds of a segment.
fn point_on_segment(point: (f64, f64), start: (f64, f64), end: (f64, f64)) -> bool {
    point.0 >= start.0.min(end.0) - GEOMETRY_TOLERANCE_MM
        && point.0 <= start.0.max(end.0) + GEOMETRY_TOLERANCE_MM
        && point.1 >= start.1.min(end.1) - GEOMETRY_TOLERANCE_MM
        && point.1 <= start.1.max(end.1) + GEOMETRY_TOLERANCE_MM
}

/// Returns the Euclidean distance between two schematic points.
fn point_distance(first: (f64, f64), second: (f64, f64)) -> f64 {
    (first.0 - second.0).hypot(first.1 - second.1)
}

/// Returns whether both coordinates of a schematic point are finite.
fn finite_point(point: (f64, f64)) -> bool {
    point.0.is_finite() && point.1.is_finite()
}

/// Extracts the supported body geometry from a primitive fact.
///
/// # Arguments
///
/// * `primitive` — Transformed primitive fact selected for the placed symbol.
///
/// # Returns
///
/// A supported circle, polygon, or line path, or `None` when the primitive is unsupported.
fn body_shape_from_primitive(primitive: &SchematicPrimitiveFact) -> Option<BodyShape> {
    let stroke_half_mm = primitive.stroke_width_mm.max(0.0) * HALF_FACTOR;
    if let Some((center_x, center_y, radius_mm)) = primitive.circle_mm {
        if !center_x.is_finite()
            || !center_y.is_finite()
            || !radius_mm.is_finite()
            || radius_mm <= 0.0
        {
            return None;
        }
        return Some(BodyShape::Circle {
            center_mm: (center_x, center_y),
            radius_mm,
            stroke_half_mm,
        });
    }
    if primitive.arc_mm.is_some() || primitive.bezier_mm.is_some() {
        return None;
    }
    if primitive.points_mm.iter().any(|point| !finite_point(*point)) {
        return None;
    }
    if primitive.closed && primitive.points_mm.len() >= 3 {
        Some(BodyShape::Polygon {
            points_mm: primitive.points_mm.clone(),
            stroke_half_mm,
        })
    } else if primitive.points_mm.len() >= 2 {
        Some(BodyShape::Path {
            points_mm: primitive.points_mm.clone(),
            stroke_half_mm,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{BodyGeometrySet, BodyShape, minimum_body_gap, minimum_body_to_bounds_gap};

    /// Checks polygon and circle distances, including overlap and field bounds inside a body.
    #[test]
    fn body_geometry_distances_cover_polygons_circles_and_fields() {
        let first = BodyGeometrySet {
            shapes: vec![BodyShape::Polygon {
                points_mm: vec![(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0)],
                stroke_half_mm: 0.0,
            }],
            has_unsupported_geometry: false,
        };
        let second = BodyGeometrySet {
            shapes: vec![BodyShape::Polygon {
                points_mm: vec![(5.0, 0.0), (7.0, 0.0), (7.0, 2.0), (5.0, 2.0)],
                stroke_half_mm: 0.0,
            }],
            has_unsupported_geometry: false,
        };
        assert_eq!(minimum_body_gap(&first, &second), Some(3.0));
        assert_eq!(
            minimum_body_to_bounds_gap(&first, (-1.0, 0.5, 1.0, 1.5)),
            Some(0.0)
        );
        assert_eq!(
            minimum_body_to_bounds_gap(&first, (4.0, 0.5, 5.0, 1.5)),
            Some(2.0)
        );

        let circle = BodyGeometrySet {
            shapes: vec![BodyShape::Circle {
                center_mm: (10.0, 1.0),
                radius_mm: 1.0,
                stroke_half_mm: 0.0,
            }],
            has_unsupported_geometry: false,
        };
        assert_eq!(minimum_body_gap(&second, &circle), Some(2.0));
    }

    /// Checks that open paths contribute their stroke width to body-to-body clearance.
    #[test]
    fn body_geometry_path_clearance_subtracts_both_strokes() {
        let first = BodyGeometrySet {
            shapes: vec![BodyShape::Path {
                points_mm: vec![(0.0, 0.0), (0.0, 2.0)],
                stroke_half_mm: 0.25,
            }],
            has_unsupported_geometry: false,
        };
        let second = BodyGeometrySet {
            shapes: vec![BodyShape::Path {
                points_mm: vec![(2.0, 0.0), (2.0, 2.0)],
                stroke_half_mm: 0.25,
            }],
            has_unsupported_geometry: false,
        };
        assert_eq!(minimum_body_gap(&first, &second), Some(1.5));
    }
}
