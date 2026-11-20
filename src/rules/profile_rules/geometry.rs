//! Supported transformed symbol-body shapes and distance calculations for profile layout checks.

use crate::rules::HALF_FACTOR;
use crate::rules::generic::{GEOMETRY_TOLERANCE_MM, SchematicFacts, SchematicPrimitiveFact};

/// Conservative clearance margin for curve paths flattened by the shared schematic fact extractor, in millimetres.
const BODY_CURVE_FLATTEN_TOLERANCE_MM: f64 = 0.01;

/// Resolved symbol-body geometry for one placed reference.
///
/// Circular arcs and cubic Béziers use the already-transformed, adaptively
/// flattened path points from schematic facts. Their flattening tolerance is
/// subtracted conservatively from measured clearances.
#[derive(Clone, Debug, Default)]
pub(super) struct BodyGeometrySet {
    /// Supported body shapes transformed into schematic coordinates.
    pub(super) shapes: Vec<BodyShape>,
    /// Whether one or more primitives could not be represented without guessing.
    pub(super) has_unsupported_geometry: bool,
}

/// A supported closed body, circle, or open stroked body path, including flattened curves.
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
/// * `reference` — Reference designator used to select transformed primitives belonging to this placed symbol.
/// * `facts` — Normalized schematic facts containing transformed symbol primitives.
///
/// # Returns
///
/// Supported body shapes plus an unresolved flag when an invalid or incomplete primitive
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
pub(super) fn minimum_body_gap(first: &BodyGeometrySet, second: &BodyGeometrySet) -> Option<f64> {
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
    if ![left, top, right, bottom]
        .iter()
        .all(|value| value.is_finite())
        || right <= left
        || bottom <= top
    {
        return None;
    }
    let rectangle = [(left, top), (right, top), (right, bottom), (left, bottom)];
    let mut minimum = f64::INFINITY;
    for shape in &body.shapes {
        let gap = match shape {
            BodyShape::Circle {
                center_mm,
                radius_mm,
                stroke_half_mm,
            } => point_polygon_gap(*center_mm, &rectangle) - radius_mm - stroke_half_mm,
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

/// Checks whether a body geometry set is complete enough for reliable distance measurements.
///
/// # Arguments
///
/// * `body` — Resolved shapes collected for one component, including its unsupported-geometry marker.
///
/// # Returns
///
/// `true` when the body has at least one supported shape and no unresolved primitives.
fn body_is_complete(body: &BodyGeometrySet) -> bool {
    !body.has_unsupported_geometry && !body.shapes.is_empty()
}

/// Calculates edge-to-edge clearance between two supported body shapes, including their strokes.
///
/// # Arguments
///
/// * `first` — First circle, polygon, or open path whose boundary contributes to the clearance.
/// * `second` — Second circle, polygon, or open path whose boundary contributes to the clearance.
///
/// # Returns
///
/// The non-negative distance between the two supported shapes after subtracting stroke half-widths.
fn shape_gap(first: &BodyShape, second: &BodyShape) -> f64 {
    match first {
        BodyShape::Circle {
            center_mm,
            radius_mm,
            stroke_half_mm,
        } => circle_to_shape_gap(*center_mm, *radius_mm, *stroke_half_mm, second),
        BodyShape::Polygon {
            points_mm,
            stroke_half_mm,
        } => polygon_to_shape_gap(points_mm, *stroke_half_mm, second),
        BodyShape::Path {
            points_mm,
            stroke_half_mm,
        } => path_to_shape_gap(points_mm, *stroke_half_mm, second),
    }
}

/// Calculates clearance from a stroked circle to any supported body shape.
///
/// # Arguments
///
/// * `center` — Centre of the first circular body in schematic millimetres.
/// * `radius` — Radius of the first circular body in schematic millimetres.
/// * `stroke_half` — Half the first circle's rendered stroke width.
/// * `second_shape` — Second circle, polygon, or open-path body geometry compared with the first shape.
///
/// # Returns
///
/// The non-negative edge-to-edge distance after subtracting both shapes' stroke widths.
fn circle_to_shape_gap(
    center: (f64, f64),
    radius: f64,
    stroke_half: f64,
    second_shape: &BodyShape,
) -> f64 {
    match second_shape {
        BodyShape::Circle {
            center_mm,
            radius_mm,
            stroke_half_mm,
        } => circle_circle_shape_gap(
            center,
            radius,
            stroke_half,
            *center_mm,
            *radius_mm,
            *stroke_half_mm,
        ),
        BodyShape::Polygon {
            points_mm,
            stroke_half_mm,
        } => circle_polygon_shape_gap(center, radius, stroke_half, points_mm, *stroke_half_mm),
        BodyShape::Path {
            points_mm,
            stroke_half_mm,
        } => circle_path_shape_gap(center, radius, stroke_half, points_mm, *stroke_half_mm),
    }
}

/// Calculates clearance from a stroked polygon to any supported body shape.
///
/// # Arguments
///
/// * `polygon_vertices` — Ordered outline vertices of the first polygon in schematic millimetres.
/// * `polygon_stroke_half` — Half the first polygon's rendered stroke width.
/// * `second_shape` — Second circle, polygon, or open-path body geometry compared with the first shape.
///
/// # Returns
///
/// The non-negative edge-to-edge distance after subtracting both shapes' stroke widths.
fn polygon_to_shape_gap(
    polygon_vertices: &[(f64, f64)],
    polygon_stroke_half: f64,
    second_shape: &BodyShape,
) -> f64 {
    match second_shape {
        BodyShape::Circle {
            center_mm,
            radius_mm,
            stroke_half_mm,
        } => circle_polygon_shape_gap(
            *center_mm,
            *radius_mm,
            *stroke_half_mm,
            polygon_vertices,
            polygon_stroke_half,
        ),
        BodyShape::Polygon {
            points_mm,
            stroke_half_mm,
        } => polygon_polygon_shape_gap(
            polygon_vertices,
            polygon_stroke_half,
            points_mm,
            *stroke_half_mm,
        ),
        BodyShape::Path {
            points_mm,
            stroke_half_mm,
        } => polygon_path_shape_gap(
            points_mm,
            *stroke_half_mm,
            polygon_vertices,
            polygon_stroke_half,
        ),
    }
}

/// Calculates clearance from a stroked open path to any supported body shape.
///
/// # Arguments
///
/// * `path_vertices` — Ordered centreline points of the first open path in schematic millimetres.
/// * `path_stroke_half` — Half the first path's rendered stroke width.
/// * `second_shape` — Second circle, polygon, or open-path body geometry compared with the first shape.
///
/// # Returns
///
/// The non-negative edge-to-edge distance after subtracting both shapes' stroke widths.
fn path_to_shape_gap(
    path_vertices: &[(f64, f64)],
    path_stroke_half: f64,
    second_shape: &BodyShape,
) -> f64 {
    match second_shape {
        BodyShape::Circle {
            center_mm,
            radius_mm,
            stroke_half_mm,
        } => circle_path_shape_gap(
            *center_mm,
            *radius_mm,
            *stroke_half_mm,
            path_vertices,
            path_stroke_half,
        ),
        BodyShape::Polygon {
            points_mm,
            stroke_half_mm,
        } => polygon_path_shape_gap(path_vertices, path_stroke_half, points_mm, *stroke_half_mm),
        BodyShape::Path {
            points_mm,
            stroke_half_mm,
        } => path_path_shape_gap(path_vertices, path_stroke_half, points_mm, *stroke_half_mm),
    }
}

/// Measures clearance between two stroked circular body primitives.
///
/// # Arguments
///
/// * `first_center` — Center of the first circle in schematic millimetres.
/// * `first_radius` — Radius of the first circle in schematic millimetres.
/// * `first_stroke` — Half the first circle's stroke width.
/// * `second_center` — Center of the second circle in schematic millimetres.
/// * `second_radius` — Radius of the second circle in schematic millimetres.
/// * `second_stroke` — Half the second circle's stroke width.
///
/// # Returns
///
/// The non-negative edge-to-edge distance between the circles.
fn circle_circle_shape_gap(
    first_center: (f64, f64),
    first_radius: f64,
    first_stroke: f64,
    second_center: (f64, f64),
    second_radius: f64,
    second_stroke: f64,
) -> f64 {
    point_distance(first_center, second_center)
        - first_radius
        - second_radius
        - first_stroke
        - second_stroke
}

/// Measures clearance between a stroked circle and a stroked polygon.
///
/// # Arguments
///
/// * `center` — Circle center in schematic millimetres.
/// * `radius` — Circle radius in schematic millimetres.
/// * `circle_stroke` — Half the circle stroke width.
/// * `polygon` — Ordered polygon vertices in schematic millimetres.
/// * `polygon_stroke` — Half the polygon stroke width.
///
/// # Returns
///
/// The non-negative edge-to-edge distance between the circle and polygon.
fn circle_polygon_shape_gap(
    center: (f64, f64),
    radius: f64,
    circle_stroke: f64,
    polygon: &[(f64, f64)],
    polygon_stroke: f64,
) -> f64 {
    point_polygon_gap(center, polygon) - radius - circle_stroke - polygon_stroke
}

/// Measures clearance between a stroked circle and a stroked open path.
///
/// # Arguments
///
/// * `center` — Circle center in schematic millimetres.
/// * `radius` — Circle radius in schematic millimetres.
/// * `circle_stroke` — Half the circle stroke width.
/// * `path` — Ordered open-path points in schematic millimetres.
/// * `path_stroke` — Half the path stroke width.
///
/// # Returns
///
/// The non-negative edge-to-edge distance between the circle and path.
fn circle_path_shape_gap(
    center: (f64, f64),
    radius: f64,
    circle_stroke: f64,
    path: &[(f64, f64)],
    path_stroke: f64,
) -> f64 {
    point_path_distance(center, path) - radius - circle_stroke - path_stroke
}

/// Measures clearance between two stroked polygons.
///
/// # Arguments
///
/// * `first_polygon_vertices` — Complete ordered vertex list defining the first polygon outline in schematic millimetres.
/// * `first_stroke` — Half the first polygon's rendered outline stroke width.
/// * `second_polygon_vertices` — Complete ordered vertex list defining the second polygon outline in schematic millimetres.
/// * `second_stroke` — Half the second polygon's rendered outline stroke width.
///
/// # Returns
///
/// The non-negative edge-to-edge distance between the polygonal areas.
fn polygon_polygon_shape_gap(
    first_polygon_vertices: &[(f64, f64)],
    first_stroke: f64,
    second_polygon_vertices: &[(f64, f64)],
    second_stroke: f64,
) -> f64 {
    polygon_polygon_gap(first_polygon_vertices, second_polygon_vertices)
        - first_stroke
        - second_stroke
}

/// Measures clearance between a stroked polygon and a stroked open path.
///
/// # Arguments
///
/// * `path` — Ordered open-path points in schematic millimetres.
/// * `path_stroke` — Half the path stroke width.
/// * `polygon` — Ordered polygon vertices in schematic millimetres.
/// * `polygon_stroke` — Half the polygon stroke width.
///
/// # Returns
///
/// The non-negative edge-to-edge distance between the polygon and path.
fn polygon_path_shape_gap(
    path: &[(f64, f64)],
    path_stroke: f64,
    polygon: &[(f64, f64)],
    polygon_stroke: f64,
) -> f64 {
    path_polygon_gap(path, polygon) - polygon_stroke - path_stroke
}

/// Measures clearance between two stroked open paths.
///
/// # Arguments
///
/// * `first` — First path's ordered points in schematic millimetres.
/// * `first_stroke` — Half the first path stroke width.
/// * `second` — Second path's ordered points in schematic millimetres.
/// * `second_stroke` — Half the second path stroke width.
///
/// # Returns
///
/// The non-negative edge-to-edge distance between the paths.
fn path_path_shape_gap(
    first: &[(f64, f64)],
    first_stroke: f64,
    second: &[(f64, f64)],
    second_stroke: f64,
) -> f64 {
    path_path_gap(first, second) - first_stroke - second_stroke
}

/// Measures the distance from a point to a polygonal area, including zero for containment.
///
/// # Arguments
///
/// * `point` — Schematic-coordinate point whose distance to the polygonal area is measured.
/// * `polygon` — Ordered vertices of the filled polygon in schematic millimetres.
///
/// # Returns
///
/// The shortest non-negative distance from the point to the polygonal area or its boundary.
fn point_polygon_gap(point: (f64, f64), polygon: &[(f64, f64)]) -> f64 {
    if point_in_polygon(point, polygon) {
        return 0.0;
    }
    polygon_edges(polygon)
        .map(|(start, end)| point_segment_distance(point, start, end))
        .fold(f64::INFINITY, f64::min)
}

/// Measures the edge-to-edge gap between two polygonal areas, treating overlap and containment as zero.
///
/// # Arguments
///
/// * `first` — Ordered vertices of the first filled polygon in schematic millimetres.
/// * `second` — Ordered vertices of the second filled polygon in schematic millimetres.
///
/// # Returns
///
/// The shortest non-negative distance between polygon boundaries, or zero when their areas overlap.
fn polygon_polygon_gap(first: &[(f64, f64)], second: &[(f64, f64)]) -> f64 {
    if first.is_empty() || second.is_empty() {
        return f64::INFINITY;
    }
    if point_in_polygon(first[0], second)
        || point_in_polygon(second[0], first)
        || polygon_edges(first)
            .any(|(a, b)| polygon_edges(second).any(|(c, d)| segments_intersect(a, b, c, d)))
    {
        return 0.0;
    }
    polygon_edges(first)
        .flat_map(|(a, b)| {
            polygon_edges(second).map(move |(c, d)| segment_segment_distance(a, b, c, d))
        })
        .fold(f64::INFINITY, f64::min)
}

/// Measures the edge-to-edge gap between an open path and a polygonal area.
///
/// # Arguments
///
/// * `path` — Ordered vertices of the stroked open path in schematic millimetres.
/// * `polygon` — Ordered vertices of the filled polygonal area in schematic millimetres.
///
/// # Returns
///
/// The shortest non-negative distance between the path and polygon, or zero when they intersect.
fn path_polygon_gap(path: &[(f64, f64)], polygon: &[(f64, f64)]) -> f64 {
    if path.len() < 2 || polygon.len() < 3 {
        return f64::INFINITY;
    }
    if path.iter().any(|point| point_in_polygon(*point, polygon))
        || path.windows(2).any(|segment| {
            polygon_edges(polygon).any(|(a, b)| segments_intersect(segment[0], segment[1], a, b))
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

/// Measures the minimum edge-to-edge distance between two open polylines.
///
/// # Arguments
///
/// * `first` — Ordered vertices of the first open path in schematic millimetres.
/// * `second` — Ordered vertices of the second open path in schematic millimetres.
///
/// # Returns
///
/// The shortest non-negative distance between any segment pair from the two paths.
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

/// Measures the shortest distance between a point and an open polyline.
///
/// # Arguments
///
/// * `point` — Schematic-coordinate point whose distance to the path is measured.
/// * `path` — Ordered vertices defining the open path in schematic millimetres.
///
/// # Returns
///
/// The shortest Euclidean distance from the point to any segment of the path.
fn point_path_distance(point: (f64, f64), path: &[(f64, f64)]) -> f64 {
    path.windows(2)
        .map(|segment| point_segment_distance(point, segment[0], segment[1]))
        .fold(f64::INFINITY, f64::min)
}

/// Iterates each edge, including the closing edge, of a polygon with at least three vertices.
///
/// # Arguments
///
/// * `points` — Ordered polygon vertices in schematic millimetres.
///
/// # Returns
///
/// An iterator over each consecutive vertex pair and the final-to-first closing edge.
fn polygon_edges(points: &[(f64, f64)]) -> impl Iterator<Item = ((f64, f64), (f64, f64))> + '_ {
    points
        .iter()
        .enumerate()
        .map(move |(index, point)| (*point, points[(index + 1) % points.len()]))
}

/// Tests whether a point lies inside or on the boundary of a polygon.
///
/// # Arguments
///
/// * `point` — Schematic-coordinate point to test against the polygon.
/// * `polygon` — Ordered vertices defining the filled polygon in schematic millimetres.
///
/// # Returns
///
/// `true` when the point is inside the polygon or within the configured boundary tolerance.
fn point_in_polygon(point: (f64, f64), polygon: &[(f64, f64)]) -> bool {
    if polygon.len() < 3 {
        return false;
    }
    if polygon_edges(polygon)
        .any(|(start, end)| point_segment_distance(point, start, end) <= GEOMETRY_TOLERANCE_MM)
    {
        return true;
    }
    let mut inside = false;
    for (start, end) in polygon_edges(polygon) {
        if (start.1 > point.1) != (end.1 > point.1) {
            let crossing_x =
                (end.0 - start.0).mul_add((point.1 - start.1) / (end.1 - start.1), start.0);
            if point.0 < crossing_x {
                inside = !inside;
            }
        }
    }
    inside
}

/// Measures the shortest Euclidean distance between a point and a finite segment.
///
/// # Arguments
///
/// * `point` — Schematic-coordinate point whose distance to the segment is measured.
/// * `start` — First endpoint of the finite segment in schematic millimetres.
/// * `end` — Second endpoint of the finite segment in schematic millimetres.
///
/// # Returns
///
/// The shortest Euclidean distance from the point to the finite segment.
fn point_segment_distance(point: (f64, f64), start: (f64, f64), end: (f64, f64)) -> f64 {
    let delta_x = end.0 - start.0;
    let delta_y = end.1 - start.1;
    let length_squared = delta_x.mul_add(delta_x, delta_y * delta_y);
    if length_squared <= GEOMETRY_TOLERANCE_MM * GEOMETRY_TOLERANCE_MM {
        return point_distance(point, start);
    }
    let projection = ((point.1 - start.1).mul_add(delta_y, (point.0 - start.0) * delta_x)
        / length_squared)
        .clamp(0.0, 1.0);
    let nearest = (
        projection.mul_add(delta_x, start.0),
        projection.mul_add(delta_y, start.1),
    );
    point_distance(point, nearest)
}

/// Measures the minimum distance between two finite segments, returning zero at an intersection.
///
/// # Arguments
///
/// * `first_start` — First endpoint of the first segment in schematic millimetres.
/// * `first_end` — Second endpoint of the first segment in schematic millimetres.
/// * `second_start` — First endpoint of the second segment in schematic millimetres.
/// * `second_end` — Second endpoint of the second segment in schematic millimetres.
///
/// # Returns
///
/// The shortest Euclidean distance between the finite segments, or zero when they intersect.
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

/// Tests whether two finite segments intersect, including collinear and tolerance-close endpoints.
///
/// # Arguments
///
/// * `first_start` — First endpoint of the first segment in schematic millimetres.
/// * `first_end` — Second endpoint of the first segment in schematic millimetres.
/// * `second_start` — First endpoint of the second segment in schematic millimetres.
/// * `second_end` — Second endpoint of the second segment in schematic millimetres.
///
/// # Returns
///
/// `true` when the finite segments cross, overlap, or touch within geometry tolerance.
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

/// Determines the tolerance-aware orientation of a point relative to a directed segment.
///
/// # Arguments
///
/// * `start` — First endpoint of the directed segment in schematic millimetres.
/// * `end` — Second endpoint of the directed segment in schematic millimetres.
/// * `point` — Schematic-coordinate point whose orientation is classified.
///
/// # Returns
///
/// A negative, zero, or positive sign for clockwise, collinear, or counter-clockwise orientation.
fn orientation_sign(start: (f64, f64), end: (f64, f64), point: (f64, f64)) -> i8 {
    let cross = (end.1 - start.1).mul_add(
        -(point.0 - start.0),
        (end.0 - start.0) * (point.1 - start.1),
    );
    let scale = point_distance(start, end).max(1.0);
    if cross.abs() <= GEOMETRY_TOLERANCE_MM * scale {
        0
    } else if cross < 0.0 {
        -1
    } else {
        1
    }
}

/// Checks whether a point lies within the axis-aligned bounds of a segment.
///
/// # Arguments
///
/// * `point` — Schematic-coordinate point to test against the segment bounds.
/// * `start` — First endpoint of the finite segment in schematic millimetres.
/// * `end` — Second endpoint of the finite segment in schematic millimetres.
///
/// # Returns
///
/// `true` when both point coordinates lie within the segment's tolerance-expanded bounds.
fn point_on_segment(point: (f64, f64), start: (f64, f64), end: (f64, f64)) -> bool {
    point.0 >= start.0.min(end.0) - GEOMETRY_TOLERANCE_MM
        && point.0 <= start.0.max(end.0) + GEOMETRY_TOLERANCE_MM
        && point.1 >= start.1.min(end.1) - GEOMETRY_TOLERANCE_MM
        && point.1 <= start.1.max(end.1) + GEOMETRY_TOLERANCE_MM
}

/// Measures the Euclidean distance between two schematic points.
///
/// # Arguments
///
/// * `first` — First schematic-coordinate point in millimetres.
/// * `second` — Second schematic-coordinate point in millimetres.
///
/// # Returns
///
/// The Euclidean separation between the two points in millimetres.
fn point_distance(first: (f64, f64), second: (f64, f64)) -> f64 {
    (first.0 - second.0).hypot(first.1 - second.1)
}

/// Checks whether both coordinates of a schematic point are finite.
///
/// # Arguments
///
/// * `point` — Schematic-coordinate pair being checked for finite numeric values.
///
/// # Returns
///
/// `true` when both coordinates are finite floating-point numbers.
const fn finite_point(point: (f64, f64)) -> bool {
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
    if primitive
        .points_mm
        .iter()
        .any(|point| !finite_point(*point))
    {
        return None;
    }
    let curve_margin_mm = if primitive.arc_mm.is_some() || primitive.bezier_mm.is_some() {
        BODY_CURVE_FLATTEN_TOLERANCE_MM
    } else {
        0.0
    };
    let conservative_stroke_half_mm = stroke_half_mm + curve_margin_mm;
    if primitive.closed && primitive.points_mm.len() >= 3 {
        Some(BodyShape::Polygon {
            points_mm: primitive.points_mm.clone(),
            stroke_half_mm: conservative_stroke_half_mm,
        })
    } else if primitive.points_mm.len() >= 2 {
        Some(BodyShape::Path {
            points_mm: primitive.points_mm.clone(),
            stroke_half_mm: conservative_stroke_half_mm,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BODY_CURVE_FLATTEN_TOLERANCE_MM, BodyGeometrySet, BodyShape, minimum_body_gap,
        minimum_body_to_bounds_gap,
    };

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

    /// Uses flattened circular-arc and cubic Bézier paths for conservative body-gap calculations.
    #[test]
    fn body_geometry_accepts_flattened_arc_and_bezier_outlines() {
        use super::body_geometry_for_reference;
        use crate::rules::generic::{SchematicArcFact, SchematicFacts, SchematicPrimitiveFact};

        let arc = SchematicPrimitiveFact {
            reference: "U1".to_owned(),
            points_mm: vec![
                (-1.0, 0.0),
                (-0.707, 0.707),
                (0.0, 1.0),
                (0.707, 0.707),
                (1.0, 0.0),
            ],
            closed: false,
            circle_mm: None,
            arc_mm: Some(SchematicArcFact {
                center_mm: (0.0, 0.0),
                radius_mm: 1.0,
                start_angle_rad: std::f64::consts::PI,
                sweep_rad: -std::f64::consts::PI,
            }),
            bezier_mm: None,
            stroke_width_mm: 0.2,
        };
        let bezier = SchematicPrimitiveFact {
            reference: "U2".to_owned(),
            points_mm: vec![(4.0, 0.0), (5.0, 0.75), (6.0, 0.75), (7.0, 0.0)],
            closed: false,
            circle_mm: None,
            arc_mm: None,
            bezier_mm: Some([(4.0, 0.0), (5.0, 1.0), (6.0, 1.0), (7.0, 0.0)]),
            stroke_width_mm: 0.0,
        };
        let facts = SchematicFacts {
            symbol_primitives: vec![arc, bezier],
            ..SchematicFacts::default()
        };

        let arc_body = body_geometry_for_reference("U1", &facts);
        assert!(!arc_body.has_unsupported_geometry);
        assert!(matches!(
            arc_body.shapes.as_slice(),
            [BodyShape::Path { stroke_half_mm, .. }]
                if (*stroke_half_mm - 0.11).abs() < 1.0e-9
        ));

        let bezier_body = body_geometry_for_reference("U2", &facts);
        assert!(!bezier_body.has_unsupported_geometry);
        assert!(matches!(
            bezier_body.shapes.as_slice(),
            [BodyShape::Path { stroke_half_mm, .. }]
                if (*stroke_half_mm - BODY_CURVE_FLATTEN_TOLERANCE_MM).abs() < 1.0e-9
        ));

        let distant_body = BodyGeometrySet {
            shapes: vec![BodyShape::Polygon {
                points_mm: vec![(9.0, -1.0), (10.0, -1.0), (10.0, 2.0), (9.0, 2.0)],
                stroke_half_mm: 0.0,
            }],
            has_unsupported_geometry: false,
        };
        assert!(minimum_body_gap(&bezier_body, &distant_body).is_some());
    }

    /// Preserves closed cubic Bézier outlines as filled body polygons.
    #[test]
    fn body_geometry_accepts_closed_bezier_outlines() {
        use super::body_geometry_for_reference;
        use crate::rules::generic::{SchematicFacts, SchematicPrimitiveFact};

        let primitive = SchematicPrimitiveFact {
            reference: "U1".to_owned(),
            points_mm: vec![(0.0, 0.0), (1.0, -1.0), (2.0, 0.0), (1.0, 1.0)],
            closed: true,
            circle_mm: None,
            arc_mm: None,
            bezier_mm: Some([(0.0, 0.0), (1.0, -1.0), (2.0, 0.0), (1.0, 1.0)]),
            stroke_width_mm: 0.0,
        };
        let facts = SchematicFacts {
            symbol_primitives: vec![primitive],
            ..SchematicFacts::default()
        };

        let body = body_geometry_for_reference("U1", &facts);
        assert!(!body.has_unsupported_geometry);
        assert!(matches!(
            body.shapes.as_slice(),
            [BodyShape::Polygon { .. }]
        ));
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
