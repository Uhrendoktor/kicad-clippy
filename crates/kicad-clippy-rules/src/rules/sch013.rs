//! Checks `SCH013` wire clearance from transformed schematic obstacles and effective stroke widths.

use geo::algorithm::line_intersection::line_intersection;
use geo::Contains;
use geo::Coord;
use geo::Distance;
use geo::Euclidean;
use geo::Line;
use geo::LineIntersection;
use geo::LineString;
use geo::Point as GeoPoint;
use geo::Polygon;
use kicad_clippy_core::BuiltinRuleId;
use kicad_clippy_core::ByteSpan;
use kicad_clippy_core::Diagnostic;
use kiutils_sexpr::Node;
use kurbo::CubicBez as CurveCubicBez;
use kurbo::Line as CurveLine;
use kurbo::PathSeg as CurvePathSeg;
use kurbo::Point as CurvePoint;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use super::at_segment;
use super::Bounds;
use super::children;
use super::collect_visible_field_bounds;
use super::CURVE_FLATTEN_TOLERANCE_MM;
use super::distance;
use super::FLOATING_POINT_ROUNDOFF_ULPS;
use super::GEOMETRY_NUMERIC_EPSILON;
use super::GEOMETRY_TOLERANCE_MM;
use super::HALF_FACTOR;
use super::is_endpoint;
use super::Point;
use super::SchematicArcFact;
use super::SchematicBusObstacleFact;
use super::SchematicFacts;
use super::SchematicGraphicFact;
use super::SchematicNoConnectFact;
use super::SchematicPinFact;
use super::SchematicPrimitiveFact;
use super::SchematicSheetFact;
use super::SchematicSymbolTextFact;
use super::Segment;
use super::strict_line_hits_bounds;
use super::collect_sheet_table_stroke_facts;
use super::sch010::text_box_bounds;
use super::TWO_SIDED_FACTOR;

/// Finds wire crossings or clearance violations against transformed embedded-symbol text.
///
/// # Arguments
///
/// * `wire_index` — Stable index of the wire within the current segment collection.
/// * `wire` — Wire segment whose collision or clearance is being evaluated.
/// * `texts` — Transformed embedded-symbol text obstacles to compare with the wire.
///
/// # Returns
///
/// SCH013 diagnostics for wires whose stroke envelopes collide with visible text embedded in placed symbols.
fn embedded_symbol_text_obstacle_diagnostics(
    wire_index: usize,
    wire: Segment,
    texts: &[SchematicSymbolTextFact],
) -> Vec<Diagnostic> {
    texts
        .iter()
        .filter_map(|text| {
            let (left, top, right, bottom) = text.bounds_mm;
            let bounds = Bounds {
                x: left,
                y: top,
                width: right - left,
                height: bottom - top,
            };
            strict_line_hits_bounds(wire, bounds).then(|| {
                at_segment(
                    BuiltinRuleId::Sch013,
                    &format!(
                        "wire {wire_index} passes through embedded symbol {} {} {:?}",
                        text.kind, text.reference, text.text
                    ),
                    wire,
                )
            })
        })
        .collect()
}
/// Finds wire intersections or insufficient clearance against a schematic bus obstacle.
///
/// # Arguments
///
/// * `wire_index` — Stable index of the wire within the current segment collection.
/// * `segment` — Wire segment being checked for obstacle intersection or clearance.
/// * `obstacles` — Extracted bus or graphic obstacle geometry.
/// * `wire_stroke_width_mm` — wire stroke width mm to be used in this geometry or parsing operation.
///
/// # Returns
///
/// SCH013 diagnostics for wire collisions or insufficient stroke clearance around bus paths and their pins.
fn bus_obstacle_diagnostics(
    wire_index: usize,
    segment: Segment,
    obstacles: &[SchematicBusObstacleFact],
    wire_stroke_width_mm: f64,
) -> Vec<Diagnostic> {
    obstacles
        .iter()
        .filter(|obstacle| {
            wire_crosses_symbol_primitive_with_wire_stroke(
                segment,
                &obstacle.primitive,
                &obstacle.pins,
                wire_stroke_width_mm,
            )
        })
        .map(|obstacle| {
            at_segment(
                BuiltinRuleId::Sch013,
                &format!(
                    "wire {wire_index} intersects {} {:?}",
                    obstacle.kind, obstacle.primitive.reference
                ),
                segment,
            )
        })
        .collect()
}
/// Finds wire segments that collide with hierarchical sheet frames or their stroke envelopes.
///
/// # Arguments
///
/// * `wire_index` — Stable index of the wire within the current segment collection.
/// * `segment` — Wire segment being checked for obstacle intersection or clearance.
/// * `sheets` — Hierarchical sheet-frame facts used as wire obstacles.
/// * `wire_stroke_width_mm` — wire stroke width mm to be used in this geometry or parsing operation.
///
/// # Returns
///
/// SCH013 diagnostics for wire collisions with hierarchical sheet frames or their stroked edges.
fn sheet_frame_obstacle_diagnostics(
    wire_index: usize,
    segment: Segment,
    sheets: &[SchematicSheetFact],
    wire_stroke_width_mm: f64,
) -> Vec<Diagnostic> {
    sheets
        .iter()
        .filter(|sheet| {
            wire_crosses_symbol_primitive_with_wire_stroke(
                segment,
                &sheet.primitive,
                &sheet.pins,
                wire_stroke_width_mm,
            )
        })
        .map(|sheet| {
            at_segment(
                BuiltinRuleId::Sch013,
                &format!(
                    "wire {wire_index} penetrates hierarchical sheet {:?} ({})",
                    sheet.name, sheet.uuid
                ),
                segment,
            )
        })
        .collect()
}
/// Finds wire intersections or clearance violations against enabled table border and separator strokes.
///
/// # Arguments
///
/// * `wire_index` — Stable index of the wire within the current segment collection.
/// * `segment` — Wire segment being checked for obstacle intersection or clearance.
/// * `table_strokes` — Enabled table-border and separator strokes used as obstacles.
/// * `wire_stroke_width_mm` — wire stroke width mm to be used in this geometry or parsing operation.
///
/// # Returns
///
/// SCH013 diagnostics for wires that intersect enabled table borders or separator strokes.
fn table_stroke_obstacle_diagnostics(
    wire_index: usize,
    segment: Segment,
    table_strokes: &[SchematicGraphicFact],
    wire_stroke_width_mm: f64,
) -> Vec<Diagnostic> {
    table_strokes
        .iter()
        .filter(|table_stroke| {
            wire_crosses_sheet_graphic_with_wire_stroke(segment, table_stroke, wire_stroke_width_mm)
        })
        .map(|table_stroke| {
            at_segment(
                BuiltinRuleId::Sch013,
                &format!(
                    "wire {wire_index} penetrates sheet graphic {} {:?}",
                    table_stroke.kind, table_stroke.uuid
                ),
                segment,
            )
        })
        .collect()
}
/// Tests whether a wire intersects either diagonal stroke of a no-connect marker.
///
/// # Arguments
///
/// * `wire` — Wire segment whose collision or clearance is being evaluated.
/// * `diagonal` — diagonal to be used in this geometry or parsing operation.
/// * `wire_stroke_width_mm` — wire stroke width mm to be used in this geometry or parsing operation.
///
/// # Returns
///
/// `true` when the wire's stroke envelope intersects either diagonal stroke of the no-connect marker.
fn wire_crosses_no_connect_diagonal(
    wire: Segment,
    diagonal: [(f64, f64); 2],
    wire_stroke_width_mm: f64,
) -> bool {
    let start = Point {
        x: diagonal[0].0,
        y: diagonal[0].1,
    };
    let end = Point {
        x: diagonal[1].0,
        y: diagonal[1].1,
    };
    let finite = [
        wire.start.x,
        wire.start.y,
        wire.end.x,
        wire.end.y,
        start.x,
        start.y,
        end.x,
        end.y,
    ]
    .iter()
    .all(|value| value.is_finite());
    (finite && !segment_intersection_points(wire, start, end).is_empty())
        || wire_within_sampled_path_stroke(wire, &diagonal, false, wire_stroke_width_mm, 0.0)
}
/// Finds wires that collide with a no-connect marker, accounting for its fixed marker geometry and available pen width.
///
/// # Arguments
///
/// * `wire_index` — Stable index of the wire within the current segment collection.
/// * `segment` — Wire segment being checked for obstacle intersection or clearance.
/// * `markers` — No-connect marker facts, including marker centers and half-size.
/// * `wire_stroke_width_mm` — wire stroke width mm to be used in this geometry or parsing operation.
/// * `default_marker_pen_width_mm` — Optional schematic default line pen width applied to no-connect marker strokes, in millimetres.
///
/// # Returns
///
/// SCH013 diagnostics for wire strokes that collide with a no-connect marker after applying its fixed marker geometry.
fn no_connect_marker_obstacle_diagnostics(
    wire_index: usize,
    segment: Segment,
    markers: &[SchematicNoConnectFact],
    wire_stroke_width_mm: f64,
    default_marker_pen_width_mm: Option<f64>,
) -> Vec<Diagnostic> {
    if !wire_stroke_width_mm.is_finite() || wire_stroke_width_mm < 0.0 {
        return Vec::new();
    }
    let marker_pen_width_mm = default_marker_pen_width_mm
        .filter(|width| width.is_finite() && *width > 0.0)
        .unwrap_or(0.0);
    let combined_stroke_width_mm = wire_stroke_width_mm + marker_pen_width_mm;
    if !combined_stroke_width_mm.is_finite() {
        return Vec::new();
    }
    markers
        .iter()
        .filter(|marker| {
            let (center_x, center_y) = marker.center_mm;
            let half = marker.half_size_mm;
            let first_diagonal = [
                (center_x - half, center_y - half),
                (center_x + half, center_y + half),
            ];
            let second_diagonal = [
                (center_x + half, center_y - half),
                (center_x - half, center_y + half),
            ];
            wire_crosses_no_connect_diagonal(segment, first_diagonal, combined_stroke_width_mm)
                || wire_crosses_no_connect_diagonal(
                    segment,
                    second_diagonal,
                    combined_stroke_width_mm,
                )
        })
        .map(|marker| {
            at_segment(
                BuiltinRuleId::Sch013,
                &format!(
                    "wire {wire_index} crosses no-connect marker {}",
                    marker.reference
                ),
                segment,
            )
        })
        .collect()
}
/// Effective wire width and optional no-connect marker pen width used by obstacle checks.
#[derive(Clone, Copy)]
struct WireObstacleStrokeWidths {
    /// Effective wire stroke width in millimetres.
    wire_mm: f64,
    /// Schematic default pen width used for no-connect markers, when the project supplies it.
    no_connect_marker_pen_mm: Option<f64>,
}
/// Combines sheet-frame, table-stroke, bus, and no-connect obstacle diagnostics for one wire.
///
/// # Arguments
///
/// * `wire_index` — Stable index of the wire within the current segment collection.
/// * `segment` — Wire segment being checked for obstacle intersection or clearance.
/// * `sheets` — Hierarchical sheet-frame facts used as wire obstacles.
/// * `table_strokes` — Enabled table-border and separator strokes used as obstacles.
/// * `bus_obstacles` — bus obstacles to be used in this geometry or parsing operation.
/// * `no_connects` — no connects to be used in this geometry or parsing operation.
/// * `stroke_widths` — Resolved explicit and project-derived stroke widths indexed by wire source offset.
///
/// # Returns
///
/// Combined SCH013 diagnostics for sheet frames, table strokes, buses, and no-connect markers near the wire.
fn sheet_table_bus_obstacle_diagnostics(
    wire_index: usize,
    segment: Segment,
    sheets: &[SchematicSheetFact],
    table_strokes: &[SchematicGraphicFact],
    bus_obstacles: &[SchematicBusObstacleFact],
    no_connects: &[SchematicNoConnectFact],
    stroke_widths: WireObstacleStrokeWidths,
) -> Vec<Diagnostic> {
    let wire_stroke_width_mm = stroke_widths.wire_mm;
    let mut diagnostics =
        sheet_frame_obstacle_diagnostics(wire_index, segment, sheets, wire_stroke_width_mm);
    diagnostics.extend(table_stroke_obstacle_diagnostics(
        wire_index,
        segment,
        table_strokes,
        wire_stroke_width_mm,
    ));
    diagnostics.extend(bus_obstacle_diagnostics(
        wire_index,
        segment,
        bus_obstacles,
        wire_stroke_width_mm,
    ));
    diagnostics.extend(no_connect_marker_obstacle_diagnostics(
        wire_index,
        segment,
        no_connects,
        wire_stroke_width_mm,
        stroke_widths.no_connect_marker_pen_mm,
    ));
    diagnostics
}
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
fn intermediate_pin_obstacle_diagnostics(
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
/// Checks each wire against embedded primitives, labels/text, buses, frames, tables, pins, images, and no-connect markers.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
/// * `segments` — Parsed wire segments used to determine geometry, topology, or obstacle intersections.
/// * `facts` — Normalized facts extracted from the schematic.
/// * `wire_stroke_widths` — wire stroke widths to be used in this geometry or parsing operation.
/// * `default_wire_stroke_width_mm` — default wire stroke width mm to be used in this geometry or parsing operation.
/// * `default_marker_pen_width_mm` — Optional schematic default line pen width applied to no-connect marker strokes, in millimetres.
///
/// # Returns
///
/// SCH013 diagnostics for collisions with symbol primitives, text, buses, frames, tables, pins, images, and no-connect markers.
pub(super) fn check_wire_obstacles(
    root: &Node,
    segments: &[Segment],
    facts: &SchematicFacts,
    wire_stroke_widths: &BTreeMap<usize, f64>,
    default_wire_stroke_width_mm: Option<f64>,
    default_marker_pen_width_mm: Option<f64>,
) -> Vec<Diagnostic> {
    let fields = collect_visible_field_bounds(root);
    let text_boxes: Vec<_> = children(root, "text_box")
        .enumerate()
        .filter_map(|(index, node)| text_box_bounds(node).map(|bounds| (index, bounds)))
        .collect();
    let table_strokes = collect_sheet_table_stroke_facts(root);
    let mut diagnostics = Vec::new();

    for (wire_index, segment) in segments.iter().enumerate() {
        let wire_stroke_width_mm = wire_stroke_widths
            .get(&segment.span.start)
            .copied()
            .or(default_wire_stroke_width_mm)
            .unwrap_or(0.0);
        let mut reported_symbols = BTreeSet::new();
        for primitive in &facts.symbol_primitives {
            if !reported_symbols.contains(&primitive.reference)
                && wire_crosses_symbol_primitive_with_wire_stroke(
                    *segment,
                    primitive,
                    &facts.pins,
                    wire_stroke_width_mm,
                )
            {
                reported_symbols.insert(primitive.reference.clone());
                diagnostics.push(at_segment(
                    BuiltinRuleId::Sch013,
                    &format!(
                        "wire {wire_index} penetrates symbol body {}",
                        primitive.reference
                    ),
                    *segment,
                ));
            }
        }
        for graphic in &facts.graphics {
            if wire_crosses_sheet_graphic_with_wire_stroke(*segment, graphic, wire_stroke_width_mm)
            {
                diagnostics.push(at_segment(
                    BuiltinRuleId::Sch013,
                    &format!(
                        "wire {wire_index} penetrates sheet graphic {} {:?}",
                        graphic.kind, graphic.uuid
                    ),
                    *segment,
                ));
            }
        }
        diagnostics.extend(sheet_table_bus_obstacle_diagnostics(
            wire_index,
            *segment,
            &facts.sheets,
            &table_strokes,
            &facts.bus_obstacles,
            &facts.no_connects,
            WireObstacleStrokeWidths {
                wire_mm: wire_stroke_width_mm,
                no_connect_marker_pen_mm: default_marker_pen_width_mm,
            },
        ));
        for image in &facts.images {
            if image
                .corners_mm
                .is_some_and(|corners| wire_crosses_bitmap_image(*segment, &corners))
            {
                diagnostics.push(at_segment(
                    BuiltinRuleId::Sch013,
                    &format!(
                        "wire {wire_index} penetrates embedded bitmap image {:?}",
                        image.uuid
                    ),
                    *segment,
                ));
            }
        }
        diagnostics.extend(wire_text_annotation_obstacle_diagnostics(
            wire_index,
            *segment,
            facts,
            &fields,
            &text_boxes,
        ));
        diagnostics.extend(intermediate_pin_obstacle_diagnostics(
            wire_index,
            *segment,
            &facts.pins,
        ));
    }
    diagnostics
}
/// Finds wire collisions with visible fields, labels, plain text, and text boxes.
///
/// # Arguments
///
/// * `wire_index` — Stable index of the wire within the current segment collection.
/// * `segment` — Wire segment being checked for obstacle intersection or clearance.
/// * `facts` — Normalized facts extracted from the schematic.
/// * `fields` — Visible symbol-field geometry used as wire or label collision obstacles.
/// * `text_boxes` — Text-box geometries used as wire or annotation obstacles.
///
/// # Returns
///
/// SCH013 diagnostics for wire collisions with visible fields, labels, plain text, and annotation text boxes.
fn wire_text_annotation_obstacle_diagnostics(
    wire_index: usize,
    segment: Segment,
    facts: &SchematicFacts,
    fields: &[(String, Bounds, ByteSpan)],
    text_boxes: &[(usize, Bounds)],
) -> Vec<Diagnostic> {
    let mut diagnostics =
        embedded_symbol_text_obstacle_diagnostics(wire_index, segment, &facts.symbol_texts);
    for (text_index, text) in facts.texts.iter().enumerate() {
        let Some((left, top, right, bottom)) = text.bounds_mm else {
            continue;
        };
        let bounds = Bounds {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        };
        if strict_line_hits_bounds(segment, bounds) {
            diagnostics.push(at_segment(
                BuiltinRuleId::Sch013,
                &format!(
                    "wire {wire_index} passes through free-standing schematic text {text_index} {:?}",
                    text.text
                ),
                segment,
            ));
        }
    }
    for (field_name, bounds, _span) in fields {
        if strict_line_hits_bounds(segment, *bounds) {
            diagnostics.push(at_segment(
                BuiltinRuleId::Sch013,
                &format!("wire {wire_index} passes through symbol field {field_name}"),
                segment,
            ));
        }
    }
    for (box_index, bounds) in text_boxes {
        if strict_line_hits_bounds(segment, *bounds) {
            diagnostics.push(at_segment(
                BuiltinRuleId::Sch013,
                &format!("wire {wire_index} penetrates annotation text box {box_index}"),
                segment,
            ));
        }
    }
    diagnostics
}

/// Calculates the intersection points between a wire segment and the circular sweep of an arc.
///
/// # Arguments
///
/// * `wire` — Wire segment whose collision or clearance is being evaluated.
/// * `arc` — arc to be used in this geometry or parsing operation.
///
/// # Returns
///
/// Normalized segment arc intersection points records or coordinates derived from the input.
pub(super) fn segment_arc_intersection_points(wire: Segment, arc: SchematicArcFact) -> Vec<Point> {
    let direction_x = wire.end.x - wire.start.x;
    let direction_y = wire.end.y - wire.start.y;
    let a = direction_x.mul_add(direction_x, direction_y * direction_y);
    if a == 0.0 || !a.is_finite() || !arc.radius_mm.is_finite() || arc.radius_mm <= 0.0 {
        return Vec::new();
    }

    let relative_x = wire.start.x - arc.center_mm.0;
    let relative_y = wire.start.y - arc.center_mm.1;
    let half_b = relative_x.mul_add(direction_x, relative_y * direction_y);
    let radius_squared = arc.radius_mm * arc.radius_mm;
    let c = relative_x.mul_add(relative_x, relative_y.mul_add(relative_y, -radius_squared));
    let a_times_c = a * c;
    let discriminant = half_b.mul_add(half_b, -a_times_c);
    if !half_b.is_finite() || !c.is_finite() || !discriminant.is_finite() {
        return Vec::new();
    }

    // Bound only floating-point roundoff in the quadratic discriminant; this is
    // not a geometric clearance tolerance.
    let roundoff = (FLOATING_POINT_ROUNDOFF_ULPS * f64::EPSILON * half_b.abs()).mul_add(
        half_b.abs(),
        (FLOATING_POINT_ROUNDOFF_ULPS * f64::EPSILON) * a_times_c.abs(),
    );
    let discriminant = if discriminant < 0.0 {
        if discriminant < -roundoff {
            return Vec::new();
        }
        0.0
    } else {
        discriminant
    };
    let square_root = discriminant.sqrt();
    let q = -half_b - square_root.copysign(half_b);
    let first_root = q / a;
    let second_root = if q == 0.0 { first_root } else { c / q };
    let root_count = if square_root == 0.0 { 1 } else { 2 };
    let mut intersections = Vec::with_capacity(root_count);

    for (index, parameter) in [first_root, second_root].into_iter().enumerate() {
        if index >= root_count
            || !(-GEOMETRY_NUMERIC_EPSILON..=1.0 + GEOMETRY_NUMERIC_EPSILON).contains(&parameter)
        {
            continue;
        }
        let parameter = parameter.clamp(0.0, 1.0);
        let point = Point {
            x: parameter.mul_add(direction_x, wire.start.x),
            y: parameter.mul_add(direction_y, wire.start.y),
        };
        let angle = (point.y - arc.center_mm.1).atan2(point.x - arc.center_mm.0);
        let directed_sweep = if arc.sweep_rad >= 0.0 {
            (angle - arc.start_angle_rad).rem_euclid(std::f64::consts::TAU)
        } else {
            (arc.start_angle_rad - angle).rem_euclid(std::f64::consts::TAU)
        };
        if directed_sweep <= arc.sweep_rad.abs() + GEOMETRY_NUMERIC_EPSILON {
            intersections.push(point);
        }
    }

    intersections
}
/// Calculates the intersection points between a wire segment and a cubic Bézier path.
///
/// # Arguments
///
/// * `wire` — Wire segment whose collision or clearance is being evaluated.
/// * `control_points` — Cubic Bézier control points in start/control/control/end order.
///
/// # Returns
///
/// Normalized segment bezier intersection points records or coordinates derived from the input.
pub(super) fn segment_bezier_intersection_points(
    wire: Segment,
    control_points: [(f64, f64); 4],
) -> Vec<Point> {
    let direction_x = wire.end.x - wire.start.x;
    let direction_y = wire.end.y - wire.start.y;
    if direction_x == 0.0 && direction_y == 0.0 {
        return Vec::new();
    }
    let curve = CurvePathSeg::Cubic(CurveCubicBez::new(
        CurvePoint::new(control_points[0].0, control_points[0].1),
        CurvePoint::new(control_points[1].0, control_points[1].1),
        CurvePoint::new(control_points[2].0, control_points[2].1),
        CurvePoint::new(control_points[3].0, control_points[3].1),
    ));
    let line = CurveLine::new(
        CurvePoint::new(wire.start.x, wire.start.y),
        CurvePoint::new(wire.end.x, wire.end.y),
    );
    if !curve.is_finite() || !line.is_finite() {
        return Vec::new();
    }

    curve
        .intersect_line(line)
        .into_iter()
        .filter(|intersection| {
            intersection.is_finite()
                && (-GEOMETRY_NUMERIC_EPSILON..=1.0 + GEOMETRY_NUMERIC_EPSILON)
                    .contains(&intersection.segment_t)
        })
        .map(|intersection| {
            let line_parameter = intersection.line_t.clamp(0.0, 1.0);
            Point {
                x: line_parameter.mul_add(direction_x, wire.start.x),
                y: line_parameter.mul_add(direction_y, wire.start.y),
            }
        })
        .collect()
}
/// Builds a Kurbo cubic Bézier segment from its four schematic-coordinate control points.
///
/// # Arguments
///
/// * `control_points` — Cubic Bézier control points in start/control/control/end order.
///
/// # Returns
///
/// A Kurbo cubic Bézier path segment constructed from the four supplied control points.
fn cubic_bezier_path_segment(control_points: [(f64, f64); 4]) -> CurvePathSeg {
    CurvePathSeg::Cubic(CurveCubicBez::new(
        CurvePoint::new(control_points[0].0, control_points[0].1),
        CurvePoint::new(control_points[1].0, control_points[1].1),
        CurvePoint::new(control_points[2].0, control_points[2].1),
        CurvePoint::new(control_points[3].0, control_points[3].1),
    ))
}
/// Defines the sch013 stroke distance accuracy mm constant.
const SCH013_STROKE_DISTANCE_ACCURACY_MM: f64 = 1.0e-6;
/// Defines the sch013 stroke distance epsilon mm constant.
const SCH013_STROKE_DISTANCE_EPSILON_MM: f64 = 1.0e-6;
/// Calculates the minimum Euclidean distance between a wire segment and a curve path segment.
///
/// # Arguments
///
/// * `wire` — Wire segment whose collision or clearance is being evaluated.
/// * `path` — Kurbo path elements or path segment being evaluated.
///
/// # Returns
///
/// The shortest Euclidean distance in millimetres.
fn wire_path_min_distance(wire: Segment, path: CurvePathSeg) -> Option<f64> {
    let line = CurveLine::new(
        CurvePoint::new(wire.start.x, wire.start.y),
        CurvePoint::new(wire.end.x, wire.end.y),
    );
    if !path.is_finite() || !line.is_finite() {
        return None;
    }
    let nearest = path.min_dist(CurvePathSeg::Line(line), SCH013_STROKE_DISTANCE_ACCURACY_MM);
    nearest.distance.is_finite().then_some(nearest.distance)
}
/// Tests whether a wire's combined stroke envelope reaches a path segment.
///
/// # Arguments
///
/// * `wire` — Wire segment whose collision or clearance is being evaluated.
/// * `path` — Kurbo path elements or path segment being evaluated.
/// * `stroke_width_mm` — Declared primitive stroke width, in millimetres.
/// * `flatten_tolerance_mm` — flatten tolerance mm to be used in this geometry or parsing operation.
///
/// # Returns
///
/// `true` when the distance between the wire and curve segment is within the combined wire/path stroke radius and flattening tolerance.
fn wire_within_path_stroke(
    wire: Segment,
    path: CurvePathSeg,
    stroke_width_mm: f64,
    flatten_tolerance_mm: f64,
) -> bool {
    if !stroke_width_mm.is_finite() || !flatten_tolerance_mm.is_finite() {
        return false;
    }
    let Some(minimum_distance) = wire_path_min_distance(wire, path) else {
        return false;
    };
    let threshold = stroke_width_mm.abs().mul_add(
        HALF_FACTOR,
        flatten_tolerance_mm.max(0.0) + SCH013_STROKE_DISTANCE_EPSILON_MM,
    );
    minimum_distance <= threshold
}
/// Tests whether a wire's stroke envelope reaches a path represented by sampled points.
///
/// # Arguments
///
/// * `wire` — Wire segment whose collision or clearance is being evaluated.
/// * `points` — Ordered schematic-coordinate points defining a primitive or path.
/// * `closed` — closed to be used in this geometry or parsing operation.
/// * `stroke_width_mm` — Declared primitive stroke width, in millimetres.
/// * `flatten_tolerance_mm` — flatten tolerance mm to be used in this geometry or parsing operation.
///
/// # Returns
///
/// `true` when the wire's stroke envelope reaches any segment of the sampled path, including the closing segment when the path is closed.
fn wire_within_sampled_path_stroke(
    wire: Segment,
    points: &[(f64, f64)],
    closed: bool,
    stroke_width_mm: f64,
    flatten_tolerance_mm: f64,
) -> bool {
    if points.len() < 2 {
        return false;
    }
    if points.windows(2).any(|pair| {
        let path = CurvePathSeg::Line(CurveLine::new(
            CurvePoint::new(pair[0].0, pair[0].1),
            CurvePoint::new(pair[1].0, pair[1].1),
        ));
        wire_within_path_stroke(wire, path, stroke_width_mm, flatten_tolerance_mm)
    }) {
        return true;
    }
    if closed && points.len() > 2 {
        if let (Some(last), Some(first)) = (points.last(), points.first()) {
            let path = CurvePathSeg::Line(CurveLine::new(
                CurvePoint::new(last.0, last.1),
                CurvePoint::new(first.0, first.1),
            ));
            return wire_within_path_stroke(wire, path, stroke_width_mm, flatten_tolerance_mm);
        }
    }
    false
}

/// Tests wire clearance against the stroked circumference of a circle.
///
/// # Arguments
///
/// * `wire` — Wire segment whose collision or clearance is being evaluated.
/// * `center` — Image or arc center coordinate in schematic millimetres.
/// * `radius_mm` — radius mm to be used in this geometry or parsing operation.
/// * `stroke_width_mm` — Declared primitive stroke width, in millimetres.
/// * `wire_stroke_width_mm` — wire stroke width mm to be used in this geometry or parsing operation.
///
/// # Returns
///
/// `true` when the wire's stroke envelope intersects the stroked circumference of the circle.
pub(super) fn wire_within_circle_stroke(
    wire: Segment,
    center: Point,
    radius_mm: f64,
    stroke_width_mm: f64,
    wire_stroke_width_mm: f64,
) -> bool {
    if !center.x.is_finite()
        || !center.y.is_finite()
        || !radius_mm.is_finite()
        || !stroke_width_mm.is_finite()
        || !wire_stroke_width_mm.is_finite()
        || !wire.start.x.is_finite()
        || !wire.start.y.is_finite()
        || !wire.end.x.is_finite()
        || !wire.end.y.is_finite()
    {
        return false;
    }

    let radius = radius_mm.abs();
    let half_stroke = stroke_width_mm.abs() * HALF_FACTOR;
    let wire_radius = wire_stroke_width_mm.max(0.0) * HALF_FACTOR;
    let inner_radius = (radius - half_stroke).max(0.0);
    let outer_radius = radius + half_stroke;
    let effective_inner_radius = (inner_radius - wire_radius).max(0.0);
    let effective_outer_radius = outer_radius + wire_radius;
    let minimum_distance = point_segment_distance(center, wire.start, wire.end);
    let maximum_distance = distance(center, wire.start).max(distance(center, wire.end));
    if !minimum_distance.is_finite() || !maximum_distance.is_finite() {
        return false;
    }

    let intersects_stroke_ring = minimum_distance
        <= effective_outer_radius + SCH013_STROKE_DISTANCE_EPSILON_MM
        && maximum_distance + SCH013_STROKE_DISTANCE_EPSILON_MM >= effective_inner_radius;
    let penetrates_existing_body_region =
        inner_radius > 0.0 && minimum_distance < inner_radius + wire_radius - GEOMETRY_TOLERANCE_MM;

    intersects_stroke_ring || penetrates_existing_body_region
}
/// Tests whether the combined wire/graphic stroke envelopes intersect.
///
/// # Arguments
///
/// * `wire` — Wire segment whose collision or clearance is being evaluated.
/// * `graphic` — Extracted graphic primitive with its path and stroke metadata.
/// * `wire_stroke_width_mm` — wire stroke width mm to be used in this geometry or parsing operation.
///
/// # Returns
///
/// `true` when the wire and sheet-graphic stroke envelopes intersect.
pub(super) fn wire_crosses_sheet_graphic_with_wire_stroke(
    wire: Segment,
    graphic: &SchematicGraphicFact,
    wire_stroke_width_mm: f64,
) -> bool {
    if !wire_stroke_width_mm.is_finite() || wire_stroke_width_mm < 0.0 {
        return false;
    }
    let combined_stroke_width_mm = graphic.stroke_width_mm + wire_stroke_width_mm;
    if !combined_stroke_width_mm.is_finite() {
        return false;
    }
    if let Some(arc) = graphic.arc_mm {
        if !segment_arc_intersection_points(wire, arc).is_empty() {
            return true;
        }
        if wire_within_sampled_path_stroke(
            wire,
            &graphic.points_mm,
            false,
            combined_stroke_width_mm,
            CURVE_FLATTEN_TOLERANCE_MM,
        ) {
            return true;
        }
    } else if let Some(bezier) = graphic.bezier_mm {
        if !segment_bezier_intersection_points(wire, bezier).is_empty() {
            return true;
        }
        if wire_within_path_stroke(
            wire,
            cubic_bezier_path_segment(bezier),
            combined_stroke_width_mm,
            0.0,
        ) {
            return true;
        }
    } else if let Some((center_x, center_y, radius)) = graphic.circle_mm {
        let center = Point {
            x: center_x,
            y: center_y,
        };
        return wire_within_circle_stroke(
            wire,
            center,
            radius,
            graphic.stroke_width_mm,
            wire_stroke_width_mm,
        );
    } else if matches!(graphic.kind.as_str(), "ellipse" | "ellipse_arc") {
        if wire_within_sampled_path_stroke(
            wire,
            &graphic.points_mm,
            graphic.closed,
            combined_stroke_width_mm,
            CURVE_FLATTEN_TOLERANCE_MM,
        ) {
            return true;
        }
    } else if wire_within_sampled_path_stroke(
        wire,
        &graphic.points_mm,
        graphic.closed,
        combined_stroke_width_mm,
        0.0,
    ) {
        return true;
    }
    if graphic.closed {
        let midpoint = Point {
            x: f64::midpoint(wire.start.x, wire.end.x),
            y: f64::midpoint(wire.start.y, wire.end.y),
        };
        return point_in_polygon(midpoint, &graphic.points_mm)
            || point_in_polygon(wire.start, &graphic.points_mm)
            || point_in_polygon(wire.end, &graphic.points_mm);
    }
    false
}
/// Tests whether a wire crosses the interior of a closed polygon, excluding boundary-only contact.
///
/// # Arguments
///
/// * `wire` — Wire segment whose collision or clearance is being evaluated.
/// * `polygon` — Ordered polygon vertices in schematic millimetres.
///
/// # Returns
///
/// `true` when the finite wire segment passes through the polygon interior; boundary-only contact does not count.
fn wire_crosses_polygon_interior(wire: Segment, polygon: &Polygon<f64>) -> bool {
    let direction_x = wire.end.x - wire.start.x;
    let direction_y = wire.end.y - wire.start.y;
    let wire_length = distance(wire.start, wire.end);
    if !wire_length.is_finite() || wire_length <= GEOMETRY_TOLERANCE_MM {
        return false;
    }

    let parameter_for = |point: Point| {
        if direction_x.abs() >= direction_y.abs() {
            (point.x - wire.start.x) / direction_x
        } else {
            (point.y - wire.start.y) / direction_y
        }
    };
    let mut parameters = vec![0.0, 1.0];
    for edge in polygon.exterior().lines() {
        for intersection in segment_intersection_points(wire, edge.start, edge.end) {
            let parameter = parameter_for(intersection);
            if (-GEOMETRY_NUMERIC_EPSILON..=1.0 + GEOMETRY_NUMERIC_EPSILON).contains(&parameter) {
                parameters.push(parameter.clamp(0.0, 1.0));
            }
        }
    }
    parameters.sort_by(f64::total_cmp);
    parameters.dedup_by(|first, second| (*first - *second).abs() <= GEOMETRY_NUMERIC_EPSILON);

    parameters.windows(2).any(|interval| {
        if interval[1] - interval[0] <= GEOMETRY_NUMERIC_EPSILON {
            return false;
        }
        let parameter = f64::midpoint(interval[0], interval[1]);
        let midpoint = GeoPoint::new(
            parameter.mul_add(direction_x, wire.start.x),
            parameter.mul_add(direction_y, wire.start.y),
        );
        polygon.contains(&midpoint)
            && Euclidean.distance(&midpoint, polygon.exterior()) > GEOMETRY_TOLERANCE_MM
    })
}
/// Tests whether a wire intersects the quadrilateral footprint of a rotated bitmap image.
///
/// # Arguments
///
/// * `wire` — Wire segment whose collision or clearance is being evaluated.
/// * `corners` — corners to be used in this geometry or parsing operation.
///
/// # Returns
///
/// `true` when the wire intersects the interior of the rotated bitmap's quadrilateral footprint.
fn wire_crosses_bitmap_image(wire: Segment, corners: &[(f64, f64); 4]) -> bool {
    let mut ring: Vec<_> = corners
        .iter()
        .map(|(x, y)| Coord { x: *x, y: *y })
        .collect();
    ring.push(ring[0]);
    let polygon = Polygon::new(LineString::new(ring), Vec::new());
    wire_crosses_polygon_interior(wire, &polygon)
}

/// Removes only the locally permitted pin-contact region from a wire segment before obstacle testing.
///
/// # Arguments
///
/// * `wire` — Wire segment whose collision or clearance is being evaluated.
/// * `reference` — Placed component reference designator associated with a pin or symbol.
/// * `pins` — Placed symbol-pin facts used to allow only local wire endpoint contact.
/// * `exclusion_distance_mm` — Radius of the local pin-contact region to exclude from the remaining wire segment, in millimetres.
///
/// # Returns
///
/// The remaining segment after removing the permitted pin-contact region at either endpoint, or `None` when the remaining segment is too short or invalid.
fn wire_without_pin_contact_region(
    wire: Segment,
    reference: &str,
    pins: &[SchematicPinFact],
    exclusion_distance_mm: f64,
) -> Option<Segment> {
    let start_is_pin = wire_endpoint_is_pin(wire.start, reference, pins);
    let end_is_pin = wire_endpoint_is_pin(wire.end, reference, pins);
    if !start_is_pin && !end_is_pin {
        return Some(wire);
    }

    let wire_length = distance(wire.start, wire.end);
    if !wire_length.is_finite() || wire_length <= 0.0 || !exclusion_distance_mm.is_finite() {
        return None;
    }

    let exclusion_distance_mm = exclusion_distance_mm.max(0.0);
    let start_exclusion = if start_is_pin {
        exclusion_distance_mm
    } else {
        0.0
    };
    let end_exclusion = if end_is_pin {
        exclusion_distance_mm
    } else {
        0.0
    };
    if start_exclusion + end_exclusion >= wire_length {
        return None;
    }

    let direction_x = (wire.end.x - wire.start.x) / wire_length;
    let direction_y = (wire.end.y - wire.start.y) / wire_length;
    Some(Segment {
        start: Point {
            x: direction_x.mul_add(start_exclusion, wire.start.x),
            y: direction_y.mul_add(start_exclusion, wire.start.y),
        },
        end: Point {
            x: (-direction_x).mul_add(end_exclusion, wire.end.x),
            y: (-direction_y).mul_add(end_exclusion, wire.end.y),
        },
        span: wire.span,
    })
}
/// Tests whether a wire centerline collides with a symbol primitive, excluding permitted pin contacts.
///
/// # Arguments
///
/// * `wire` — Wire segment whose collision or clearance is being evaluated.
/// * `primitive` — Embedded or sheet-level primitive whose body and stroke are checked against the wire.
/// * `pins` — Placed symbol-pin facts used to allow only local wire endpoint contact.
///
/// # Returns
///
/// `true` when the wire centerline intersects the symbol primitive away from an allowed pin contact.
fn symbol_primitive_has_centerline_collision(
    wire: Segment,
    primitive: &SchematicPrimitiveFact,
    pins: &[SchematicPinFact],
) -> bool {
    if let Some(arc) = primitive.arc_mm {
        return segment_arc_intersection_points(wire, arc)
            .into_iter()
            .any(|point| {
                !wire_intersection_is_allowed_pin(point, wire, &primitive.reference, pins)
            });
    }
    if let Some(bezier) = primitive.bezier_mm {
        return segment_bezier_intersection_points(wire, bezier)
            .into_iter()
            .any(|point| {
                !wire_intersection_is_allowed_pin(point, wire, &primitive.reference, pins)
            });
    }

    let closing_edge = primitive
        .points_mm
        .last()
        .copied()
        .zip(primitive.points_mm.first().copied())
        .filter(|_| primitive.closed);
    primitive
        .points_mm
        .windows(2)
        .map(|pair| (pair[0], pair[1]))
        .chain(closing_edge)
        .any(|(start, end)| {
            segment_intersection_points(
                wire,
                Point {
                    x: start.0,
                    y: start.1,
                },
                Point { x: end.0, y: end.1 },
            )
            .into_iter()
            .any(|point| !wire_intersection_is_allowed_pin(point, wire, &primitive.reference, pins))
        })
}
/// Tests whether a wire's stroke envelope collides with the stroked geometry of a symbol primitive.
///
/// # Arguments
///
/// * `wire` — Wire segment whose collision or clearance is being evaluated.
/// * `primitive` — Embedded or sheet-level primitive whose body and stroke are checked against the wire.
/// * `pins` — Placed symbol-pin facts used to allow only local wire endpoint contact.
/// * `combined_stroke_width_mm` — Combined wire and primitive stroke width used for the collision test, in millimetres.
///
/// # Returns
///
/// `true` when the wire and symbol-primitive stroke envelopes overlap outside the permitted pin-contact region.
fn symbol_primitive_wire_stroke_collision(
    wire: Segment,
    primitive: &SchematicPrimitiveFact,
    pins: &[SchematicPinFact],
    combined_stroke_width_mm: f64,
) -> bool {
    let flatten_tolerance = if primitive.arc_mm.is_some() {
        CURVE_FLATTEN_TOLERANCE_MM
    } else {
        0.0
    };
    let pin_contact_clearance = combined_stroke_width_mm.mul_add(
        HALF_FACTOR,
        TWO_SIDED_FACTOR.mul_add(SCH013_STROKE_DISTANCE_EPSILON_MM, flatten_tolerance),
    );
    let Some(clearance_wire) =
        wire_without_pin_contact_region(wire, &primitive.reference, pins, pin_contact_clearance)
    else {
        return false;
    };

    if primitive.arc_mm.is_some() {
        return wire_within_sampled_path_stroke(
            clearance_wire,
            &primitive.points_mm,
            false,
            combined_stroke_width_mm,
            flatten_tolerance,
        );
    }
    if let Some(bezier) = primitive.bezier_mm {
        return wire_within_path_stroke(
            clearance_wire,
            cubic_bezier_path_segment(bezier),
            combined_stroke_width_mm,
            0.0,
        );
    }
    wire_within_sampled_path_stroke(
        clearance_wire,
        &primitive.points_mm,
        primitive.closed,
        combined_stroke_width_mm,
        0.0,
    )
}
/// Checks a wire against a symbol primitive using both declared stroke widths and allowed pin-endpoint contacts.
///
/// # Arguments
///
/// * `wire` — Wire segment whose collision or clearance is being evaluated.
/// * `primitive` — Embedded or sheet-level primitive whose body and stroke are checked against the wire.
/// * `pins` — Placed symbol-pin facts used to allow only local wire endpoint contact.
/// * `wire_stroke_width_mm` — wire stroke width mm to be used in this geometry or parsing operation.
///
/// # Returns
///
/// `true` when the combined wire/primitive stroke geometry collides, excluding permitted contacts at placed pins.
pub(super) fn wire_crosses_symbol_primitive_with_wire_stroke(
    wire: Segment,
    primitive: &SchematicPrimitiveFact,
    pins: &[SchematicPinFact],
    wire_stroke_width_mm: f64,
) -> bool {
    if !wire_stroke_width_mm.is_finite() || wire_stroke_width_mm < 0.0 {
        return false;
    }
    let combined_stroke_width_mm = primitive.stroke_width_mm + wire_stroke_width_mm;
    if !combined_stroke_width_mm.is_finite() {
        return false;
    }

    if let Some((center_x, center_y, radius)) = primitive.circle_mm {
        return wire_within_circle_stroke(
            wire,
            Point {
                x: center_x,
                y: center_y,
            },
            radius,
            primitive.stroke_width_mm,
            wire_stroke_width_mm,
        );
    }
    if symbol_primitive_has_centerline_collision(wire, primitive, pins) {
        return true;
    }
    if symbol_primitive_wire_stroke_collision(wire, primitive, pins, combined_stroke_width_mm) {
        return true;
    }
    if !primitive.closed {
        return false;
    }

    let midpoint = Point {
        x: f64::midpoint(wire.start.x, wire.end.x),
        y: f64::midpoint(wire.start.y, wire.end.y),
    };
    point_in_polygon(midpoint, &primitive.points_mm)
        || (!wire_endpoint_is_pin(wire.start, &primitive.reference, pins)
            && point_in_polygon(wire.start, &primitive.points_mm))
        || (!wire_endpoint_is_pin(wire.end, &primitive.reference, pins)
            && point_in_polygon(wire.end, &primitive.points_mm))
}
/// Checks whether a candidate wire/primitive intersection is an allowed contact at a pin on that wire.
///
/// # Arguments
///
/// * `point` — Schematic coordinate being tested, in millimetres.
/// * `wire` — Wire segment whose collision or clearance is being evaluated.
/// * `reference` — Placed component reference designator associated with a pin or symbol.
/// * `pins` — Placed symbol-pin facts used to allow only local wire endpoint contact.
///
/// # Returns
///
/// `true` when the candidate intersection is at a wire endpoint that coincides with a pin belonging to the component.
fn wire_intersection_is_allowed_pin(
    point: Point,
    wire: Segment,
    reference: &str,
    pins: &[SchematicPinFact],
) -> bool {
    (distance(point, wire.start) <= GEOMETRY_TOLERANCE_MM
        && wire_endpoint_is_pin(wire.start, reference, pins))
        || (distance(point, wire.end) <= GEOMETRY_TOLERANCE_MM
            && wire_endpoint_is_pin(wire.end, reference, pins))
}
/// Checks whether a wire endpoint coincides with a placed pin belonging to the specified component.
///
/// # Arguments
///
/// * `endpoint` — Wire endpoint coordinate being compared with placed pin coordinates.
/// * `reference` — Placed component reference designator associated with a pin or symbol.
/// * `pins` — Placed symbol-pin facts used to allow only local wire endpoint contact.
///
/// # Returns
///
/// `true` when the endpoint lies within the geometry tolerance of a placed pin on the specified component.
fn wire_endpoint_is_pin(endpoint: Point, reference: &str, pins: &[SchematicPinFact]) -> bool {
    pins.iter().any(|pin| {
        pin.reference == reference
            && distance(
                endpoint,
                Point {
                    x: pin.position_mm.0,
                    y: pin.position_mm.1,
                },
            ) <= GEOMETRY_TOLERANCE_MM
    })
}
/// Converts two schematic points into a `geo` line segment.
///
/// # Arguments
///
/// * `start` — Starting coordinate of the segment or curve, in millimetres.
/// * `end` — Ending coordinate of the segment or curve, in millimetres.
///
/// # Returns
///
/// A `geo` line segment joining the supplied endpoints.
pub(super) fn geo_line(start: Point, end: Point) -> Line<f64> {
    Line::new(start, end)
}

/// Returns the geometric intersection points between a segment and a line defined by two endpoints.
///
/// # Arguments
///
/// * `first` — Straight schematic wire segment whose intersection with the second line is being calculated.
/// * `second_start` — Sheet-space start coordinate of the second line segment, expressed in millimetres.
/// * `second_end` — Sheet-space end coordinate of the second line segment, expressed in millimetres.
///
/// # Returns
///
/// Normalized segment intersection points records or coordinates derived from the input.
pub(super) fn segment_intersection_points(
    first: Segment,
    second_start: Point,
    second_end: Point,
) -> Vec<Point> {
    let first_line = geo_line(first.start, first.end);
    let second_line = geo_line(second_start, second_end);

    match line_intersection(first_line, second_line) {
        Some(LineIntersection::SinglePoint { intersection, .. }) => {
            vec![intersection]
        }
        Some(LineIntersection::Collinear { intersection }) => {
            let points = [intersection.start, intersection.end];
            if distance(points[0], points[1]) <= GEOMETRY_TOLERANCE_MM {
                vec![points[0]]
            } else {
                points.into_iter().collect()
            }
        }
        None => Vec::new(),
    }
}
/// Tests whether a point lies on the finite segment between two endpoints within the geometry tolerance.
///
/// # Arguments
///
/// * `point` — Schematic coordinate being tested, in millimetres.
/// * `start` — Starting coordinate of the segment or curve, in millimetres.
/// * `end` — Ending coordinate of the segment or curve, in millimetres.
///
/// # Returns
///
/// `true` when the point lies on the finite segment between the two supplied endpoints within the geometry tolerance.
pub(super) fn point_on_segment_any(point: Point, start: Point, end: Point) -> bool {
    Euclidean.distance(&GeoPoint::new(point.x, point.y), &geo_line(start, end))
        <= GEOMETRY_TOLERANCE_MM
}
/// Calculates the shortest Euclidean distance from a point to a finite segment.
///
/// # Arguments
///
/// * `point` — Schematic coordinate being tested, in millimetres.
/// * `start` — Starting coordinate of the segment or curve, in millimetres.
/// * `end` — Ending coordinate of the segment or curve, in millimetres.
///
/// # Returns
///
/// The shortest Euclidean distance in millimetres.
fn point_segment_distance(point: Point, start: Point, end: Point) -> f64 {
    Euclidean.distance(&GeoPoint::new(point.x, point.y), &geo_line(start, end))
}
/// Tests whether a point lies inside a polygon or on its boundary.
///
/// # Arguments
///
/// * `point` — Schematic coordinate being tested, in millimetres.
/// * `polygon` — Ordered polygon vertices in schematic millimetres.
///
/// # Returns
///
/// `true` when the point is inside the polygon or on its boundary.
fn point_in_polygon(point: Point, polygon: &[(f64, f64)]) -> bool {
    if polygon.len() < 3 {
        return false;
    }

    let mut ring: Vec<_> = polygon
        .iter()
        .map(|(x, y)| Coord { x: *x, y: *y })
        .collect();
    if ring.first() != ring.last() {
        if let Some(first) = ring.first().copied() {
            ring.push(first);
        }
    }

    let geometry = Polygon::new(LineString::new(ring), Vec::new());
    let query = GeoPoint::new(point.x, point.y);
    geometry.contains(&query)
        || Euclidean.distance(&query, geometry.exterior()) <= GEOMETRY_TOLERANCE_MM
}

/// Concrete implementation of built-in rule SCH013.
pub(super) struct Sch013;
impl<'a> crate::Rule<crate::RuleContext<'a>> for Sch013 {
    /// Returns the stable SCH013 identifier.
    ///
    /// # Returns
    ///
    /// The stable SCH013 identifier.
    fn id(&self) -> BuiltinRuleId { BuiltinRuleId::Sch013 }
    /// Evaluates physical wire clearance against transformed schematic obstacles.
    ///
    /// # Arguments
    ///
    /// * context — Shared schematic facts and project-resolved per-wire/default stroke widths.
    ///
    /// # Returns
    ///
    /// Diagnostics for wire-obstacle intersections or insufficient stroke clearance.
    fn check(&self, context: &crate::RuleContext<'a>) -> Vec<Diagnostic> {
        let Some(root) = context.document().cst().nodes.first() else { return Vec::new(); };
        let mut widths = super::collect_explicit_wire_stroke_widths(root);
        for (span_start, width_mm) in context.project_wire_stroke_widths_mm {
            widths.entry(*span_start).or_insert(*width_mm);
        }
        let default_wire_width = if super::schematic_has_graphical_netclass_assignments(root) {
            None
        } else {
            context.default_wire_stroke_width_mm
        };
        check_wire_obstacles(root, context.segments, context.facts(), &widths, default_wire_width, context.default_schematic_line_stroke_width_mm)
    }
}
