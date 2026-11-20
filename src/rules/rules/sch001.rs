//! Checks `SCH001` wire endpoints, segment geometry, overlaps, crossings, and unmarked junctions.

use super::GEOMETRY_TOLERANCE_MM;
use super::Point;
use super::Segment;
use super::at_segment;
use super::at_span;
use super::child_list;
use super::children;
use super::distance;
use super::is_endpoint;
#[cfg(test)]
pub(super) use super::line_hits_bounds;
use super::sch013::geo_line;
use super::{head, span_of, walk};
use crate::core::BuiltinRuleId;
use crate::core::Diagnostic;
use crate::core::RelatedLocation;
use crate::core::SchematicLocation;
use geo::Line;
use geo::LineIntersection;
use geo::algorithm::sweep::Cross;
use geo::algorithm::sweep::Intersections;
use kiutils_sexpr::Node;

/// Wire segment paired with its stable index in the current analysis run.
#[derive(Clone, Copy)]
struct IndexedSegment {
    /// Stable position of the segment in the current segment collection.
    index: usize,
    /// Wire segment geometry and source span.
    segment: Segment,
}

impl Cross for IndexedSegment {
    /// Floating-point scalar used for geometric coordinates, distances, and stroke measurements.
    type Scalar = f64;
    /// Returns a `geo` line segment joining this indexed wire segment's start and end coordinates.
    ///
    /// # Returns
    ///
    /// A `geo` line segment joining the supplied endpoints.
    fn line(&self) -> Line<f64> {
        geo_line(self.segment.start, self.segment.end)
    }
}
/// Checks wire endpoint counts, direction, zero length, overlap, crossing, and unmarked T-junction rules.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
/// * `segments` — Parsed wire segments used to determine geometry, topology, or obstacle intersections.
/// * `junctions` — Explicit schematic junction coordinates used when resolving wire connectivity.
///
/// # Returns
///
/// SCH001 diagnostics for invalid endpoints, diagonal or zero-length segments, overlaps, crossings, and unmarked T-junctions.
pub(super) fn check_wire_geometry(
    root: &Node,
    segments: &[Segment],
    junctions: &[Point],
) -> Vec<Diagnostic> {
    let mut diagnostics = collect_invalid_wire_diagnostics(root);
    for segment in segments {
        if distance(segment.start, segment.end) < GEOMETRY_TOLERANCE_MM {
            diagnostics.push(at_segment(
                BuiltinRuleId::Sch001,
                "wire has zero length",
                *segment,
            ));
        } else if (segment.start.x - segment.end.x).abs() > GEOMETRY_TOLERANCE_MM
            && (segment.start.y - segment.end.y).abs() > GEOMETRY_TOLERANCE_MM
        {
            diagnostics.push(at_segment(
                BuiltinRuleId::Sch001,
                "wire is diagonal; route with orthogonal segments",
                *segment,
            ));
        }
    }

    // Bentley–Ottmann sweep: the geometry library finds candidate wire pairs
    // without the handwritten all-pairs scan. We retain KiCad-specific rules
    // for zero-length/diagonal wires, explicit junction markers and diagnostics.
    let indexed = segments
        .iter()
        .copied()
        .enumerate()
        .map(|(index, segment)| IndexedSegment { index, segment });
    for (first, second, intersection) in Intersections::from_iter(indexed) {
        if first.index == second.index {
            continue;
        }
        let segment = first.segment;
        match intersection {
            LineIntersection::Collinear { intersection } => {
                let overlap = Point {
                    x: intersection.start.x,
                    y: intersection.start.y,
                };
                let overlap_end = Point {
                    x: intersection.end.x,
                    y: intersection.end.y,
                };
                if distance(overlap, overlap_end) > GEOMETRY_TOLERANCE_MM {
                    diagnostics.push(at_segment(
                        BuiltinRuleId::Sch001,
                        "collinear wire segments overlap",
                        segment,
                    ));
                }
            }
            LineIntersection::SinglePoint { intersection, .. } => {
                let point = intersection;
                let first_end = is_endpoint(point, first.segment);
                let second_end = is_endpoint(point, second.segment);
                if !first_end && !second_end {
                    let mut diagnostic = at_segment(
                        BuiltinRuleId::Sch001,
                        "wire crossing is ambiguous; split the net or reroute",
                        segment,
                    );
                    diagnostic.schematic_location = Some(SchematicLocation {
                        x_mm: point.x,
                        y_mm: point.y,
                    });
                    diagnostic.related.push(RelatedLocation {
                        description: "intersecting wire".to_owned(),
                        uuid: None,
                        location: Some(SchematicLocation {
                            x_mm: point.x,
                            y_mm: point.y,
                        }),
                    });
                    diagnostics.push(diagnostic);
                } else if first_end != second_end
                    && !junctions
                        .iter()
                        .any(|junction| distance(*junction, point) < GEOMETRY_TOLERANCE_MM)
                {
                    let mut diagnostic = at_segment(
                        BuiltinRuleId::Sch001,
                        "T-junction requires an explicit junction marker",
                        segment,
                    );
                    diagnostic.schematic_location = Some(SchematicLocation {
                        x_mm: point.x,
                        y_mm: point.y,
                    });
                    diagnostics.push(diagnostic);
                }
            }
        }
    }

    diagnostics
}
/// Reports malformed wire paths, including wires with invalid point counts or zero-length segments.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// SCH001 diagnostics for wire paths with an invalid point count or zero-length segments.
fn collect_invalid_wire_diagnostics(root: &Node) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    walk(root, &mut |node| {
        if head(node) != Some("wire") {
            return;
        }
        let point_count = child_list(node, "pts")
            .map(|points| children(points, "xy").count())
            .unwrap_or_default();
        if point_count != 2 {
            diagnostics.push(at_span(
                BuiltinRuleId::Sch001,
                "wire must have exactly two endpoints",
                span_of(node),
            ));
        }
    });
    diagnostics
}

/// Concrete implementation of built-in rule 001.
pub(super) struct Sch001;
impl<'a> crate::rules::Rule<crate::rules::RuleContext<'a>> for Sch001 {
    /// Returns the stable 001 identifier.
    ///
    /// # Returns
    ///
    /// The stable SCH001 identifier used to associate diagnostics with this rule.
    fn id(&self) -> BuiltinRuleId {
        BuiltinRuleId::Sch001
    }
    /// Evaluates wire endpoint counts, direction, crossings, overlaps, and unmarked junctions.
    ///
    /// # Arguments
    ///
    /// * `context` — the parsed schematic, straight wire segments, and explicit junction coordinates.
    ///
    /// # Returns
    ///
    /// Diagnostics for wire endpoint counts, direction, crossings, overlaps, and unmarked junctions.
    fn check(&self, context: &crate::rules::RuleContext<'a>) -> Vec<Diagnostic> {
        let Some(root) = context.document().cst().nodes.first() else {
            return Vec::new();
        };
        check_wire_geometry(root, context.segments, context.junctions)
    }
}
