//! Checks `SCH002` physical wire-island topology, label reuse, and connected-component identity.

use super::DisjointSet;
use super::GEOMETRY_TOLERANCE_MM;
use super::Point;
use super::SchematicFacts;
use super::Segment;
use super::at_point;
use super::collect_labels;
use super::collect_points;
use super::distance;
use super::is_endpoint;
use super::point_on_segment;
use kicad_clippy_core::BuiltinRuleId;
use kicad_clippy_core::ByteSpan;
use kicad_clippy_core::Diagnostic;
use kicad_clippy_core::Severity;
use kiutils_sexpr::Node;
use std::collections::BTreeMap;

/// Checks label reuse, duplicate anchors, physical wire-island membership, and hierarchical wire-twig termination.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
/// * `segments` — Parsed wire segments used to determine geometry, topology, or obstacle intersections.
///
/// # Returns
///
/// SCH002 diagnostics for duplicate labels, inconsistent label connectivity, and hierarchical wire-twig termination.
pub(super) fn check_label_topology(root: &Node, segments: &[Segment]) -> Vec<Diagnostic> {
    let labels = collect_labels(root);
    let junctions = collect_points(root, "junction");
    let mut points = Vec::new();
    for segment in segments {
        points.push(segment.start);
        points.push(segment.end);
    }
    points.extend(junctions.iter().copied());
    points.extend(labels.iter().map(|(_, _, point, _)| *point));
    points.sort_by_key(|point| point_key(*point));
    points.dedup_by_key(|point| point_key(*point));

    let (component_ids, endpoint_degree) = wire_components(segments, &points, &junctions);

    let mut totals: BTreeMap<String, usize> = BTreeMap::new();
    let mut island_uses: BTreeMap<(String, usize), usize> = BTreeMap::new();
    let mut diagnostics = Vec::new();
    for (kind, label, point, span) in labels {
        let key = point_key(point);
        if kind == "label" {
            *totals.entry(label.clone()).or_default() += 1;
            let island = component_ids.get(&key).copied().unwrap_or_default();
            *island_uses.entry((label.clone(), island)).or_default() += 1;
            if !segments
                .iter()
                .any(|segment| point_on_segment(point, segment))
            {
                diagnostics.push(at_point(
                    BuiltinRuleId::Sch002,
                    format!(
                        "local label {label:?} at ({:.3}, {:.3}) is not attached to a wire",
                        point.x, point.y
                    ),
                    point,
                    span,
                ));
            }
        } else if kind == "hierarchical_label" {
            let endpoint_count = endpoint_degree.get(&key).copied().unwrap_or_default();
            if endpoint_count != 1
                || !segments
                    .iter()
                    .any(|segment| point_on_segment(point, segment))
            {
                diagnostics.push(at_point(BuiltinRuleId::Sch002,
                    format!("hierarchical label {label:?} at ({:.3}, {:.3}) must sit at the end of one wire twig", point.x, point.y),
                    point,
                    span,
                ));
            }
        }
    }
    for ((label, _island), count) in island_uses {
        if count > 1 {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch002,
                Severity::Error,
                format!("local label {label:?} occurs {count} times on one physical wire network"),
            ));
        }
    }
    for (label, count) in totals {
        if count < 2 {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch002,
                Severity::Error,
                format!("local label {label:?} is used only {count} time(s)"),
            ));
        }
    }
    diagnostics
}
/// Builds physical wire-connected components from segment endpoints, junctions, and intermediate wire points.
///
/// # Arguments
///
/// * `segments` — Parsed wire segments used to determine geometry, topology, or obstacle intersections.
/// * `points` — Ordered schematic-coordinate points defining a primitive or path.
/// * `junctions` — Explicit schematic junction coordinates used when resolving wire connectivity.
///
/// # Returns
///
/// A coordinate-to-component map and endpoint-degree counts for the physical wire graph.
fn wire_components(
    segments: &[Segment],
    points: &[Point],
    junctions: &[Point],
) -> (BTreeMap<String, usize>, BTreeMap<String, usize>) {
    // Callers provide points sorted and deduplicated by the same tolerance-aware
    // key used by the output map. Assign compact indices once, then let the
    // established disjoint-set implementation handle connectivity.
    let point_indices: BTreeMap<_, _> = points
        .iter()
        .enumerate()
        .map(|(index, point)| (point_key(*point), index))
        .collect();
    let mut components = DisjointSet::new(points.len());
    let mut endpoint_degree: BTreeMap<String, usize> = BTreeMap::new();

    for segment in segments {
        *endpoint_degree.entry(point_key(segment.start)).or_default() += 1;
        *endpoint_degree.entry(point_key(segment.end)).or_default() += 1;

        let horizontal = (segment.start.y - segment.end.y).abs() < GEOMETRY_TOLERANCE_MM;
        let mut split: Vec<_> = points
            .iter()
            .copied()
            .filter(|point| point_on_segment(*point, segment))
            .filter(|point| {
                is_endpoint(*point, *segment)
                    || !has_unmarked_wire_endpoint(*point, segments, junctions)
            })
            .collect();
        split.sort_by(|first, second| {
            if horizontal {
                first.x.total_cmp(&second.x)
            } else {
                first.y.total_cmp(&second.y)
            }
        });

        for pair in split.windows(2) {
            let first_key = point_key(pair[0]);
            let second_key = point_key(pair[1]);
            if let (Some(&first), Some(&second)) = (
                point_indices.get(&first_key),
                point_indices.get(&second_key),
            ) {
                components.union(first, second);
            }
        }
    }

    let component_ids = points
        .iter()
        .enumerate()
        .map(|(index, point)| (point_key(*point), components.find(index)))
        .collect();

    (component_ids, endpoint_degree)
}
/// Formats a schematic point into the stable tolerance-normalized coordinate key used by physical topology lookups.
///
/// # Arguments
///
/// * `point` — Schematic coordinate being tested, in millimetres.
///
/// # Returns
///
/// A stable coordinate key used by physical wire-component lookup.
fn point_key(point: Point) -> String {
    format!("{:.3}:{:.3}", point.x, point.y)
}

/// Builds the coordinate key used to retrieve a physical wire component from a pin or wire endpoint position.
///
/// # Arguments
///
/// * `x_mm` — x mm to be used in this geometry or parsing operation.
/// * `y_mm` — y mm to be used in this geometry or parsing operation.
///
/// # Returns
///
/// A stable coordinate key used by physical wire-component lookup.
#[must_use]
pub fn physical_wire_component_key(x_mm: f64, y_mm: f64) -> String {
    point_key(Point { x: x_mm, y: y_mm })
}
/// Checks whether a point is another wire's endpoint without an explicit junction connecting the crossing.
///
/// # Arguments
///
/// * `point` — Schematic coordinate being tested, in millimetres.
/// * `segments` — Parsed wire segments used to determine geometry, topology, or obstacle intersections.
/// * `junctions` — Explicit schematic junction coordinates used when resolving wire connectivity.
///
/// # Returns
///
/// `true` when another wire terminates at this coordinate and no explicit junction marks the connection.
fn has_unmarked_wire_endpoint(point: Point, segments: &[Segment], junctions: &[Point]) -> bool {
    let terminates_another_wire = segments.iter().any(|other| is_endpoint(point, *other));
    terminates_another_wire
        && !junctions
            .iter()
            .any(|junction| distance(*junction, point) < GEOMETRY_TOLERANCE_MM)
}
/// Returns the physical wire-component identifier for each normalized endpoint or junction coordinate in the schematic.
///
/// # Arguments
///
/// * `facts` — Normalized facts extracted from the schematic.
///
/// # Returns
///
/// A map from tolerance-normalized coordinates to physical wire-component identifiers.
#[must_use]
pub fn physical_wire_component_ids(facts: &SchematicFacts) -> BTreeMap<String, usize> {
    let segments: Vec<_> = facts
        .wires
        .iter()
        .map(|wire| Segment {
            start: Point {
                x: wire.start_mm.0,
                y: wire.start_mm.1,
            },
            end: Point {
                x: wire.end_mm.0,
                y: wire.end_mm.1,
            },
            span: ByteSpan::new(0, 0),
        })
        .collect();
    let mut points = Vec::new();
    for wire in &facts.wires {
        points.push(Point {
            x: wire.start_mm.0,
            y: wire.start_mm.1,
        });
        points.push(Point {
            x: wire.end_mm.0,
            y: wire.end_mm.1,
        });
    }
    points.extend(facts.pins.iter().map(|pin| Point {
        x: pin.position_mm.0,
        y: pin.position_mm.1,
    }));
    points.extend(facts.labels.iter().map(|label| Point {
        x: label.position_mm.0,
        y: label.position_mm.1,
    }));
    let junctions: Vec<_> = facts
        .junctions
        .iter()
        .map(|junction| Point {
            x: junction.0,
            y: junction.1,
        })
        .collect();
    points.extend(junctions.iter().copied());
    points.sort_by_key(|point| point_key(*point));
    points.dedup_by_key(|point| point_key(*point));
    wire_components(&segments, &points, &junctions).0
}

/// Concrete implementation of built-in rule 002.
pub(super) struct Sch002;
impl<'a> crate::Rule<crate::RuleContext<'a>> for Sch002 {
    /// Returns the stable 002 identifier.
    ///
    /// # Returns
    ///
    /// The stable SCH002 identifier used to associate diagnostics with this rule.
    fn id(&self) -> BuiltinRuleId {
        BuiltinRuleId::Sch002
    }
    /// Evaluates physical wire-island topology and label reuse.
    ///
    /// # Arguments
    ///
    /// * `context` — the parsed schematic and normalized wire segments.
    ///
    /// # Returns
    ///
    /// Diagnostics for physical wire-island topology and label reuse.
    fn check(&self, context: &crate::RuleContext<'a>) -> Vec<Diagnostic> {
        let Some(root) = context.document().cst().nodes.first() else {
            return Vec::new();
        };
        check_label_topology(root, context.segments)
    }
}
