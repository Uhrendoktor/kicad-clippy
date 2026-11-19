//! Electrical marker and combined sheet/table/bus obstacle checks for SCH013.

use super::super::SchematicNoConnectFact;
use super::{at_segment, segment_intersection_points, wire_within_sampled_path_stroke, BuiltinRuleId, Diagnostic, Point, SchematicBusObstacleFact, SchematicGraphicFact, SchematicSheetFact, Segment};

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
pub(super) struct WireObstacleStrokeWidths {
    /// Effective wire stroke width in millimetres.
    pub(super) wire_mm: f64,
    /// Schematic default pen width used for no-connect markers, when the project supplies it.
    pub(super) no_connect_marker_pen_mm: Option<f64>,
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
pub(super) fn sheet_table_bus_obstacle_diagnostics(
    wire_index: usize,
    segment: Segment,
    sheets: &[SchematicSheetFact],
    table_strokes: &[SchematicGraphicFact],
    bus_obstacles: &[SchematicBusObstacleFact],
    no_connects: &[SchematicNoConnectFact],
    stroke_widths: WireObstacleStrokeWidths,
) -> Vec<Diagnostic> {
    let wire_stroke_width_mm = stroke_widths.wire_mm;
    let mut diagnostics = super::graphics::sheet_frame_obstacle_diagnostics(
        wire_index,
        segment,
        sheets,
        wire_stroke_width_mm,
    );
    diagnostics.extend(super::graphics::table_stroke_obstacle_diagnostics(
        wire_index,
        segment,
        table_strokes,
        wire_stroke_width_mm,
    ));
    diagnostics.extend(super::graphics::bus_obstacle_diagnostics(
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
