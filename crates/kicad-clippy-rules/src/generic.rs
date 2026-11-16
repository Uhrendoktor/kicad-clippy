use std::collections::{BTreeMap, BTreeSet};

use kicad_clippy_core::{ByteSpan, Diagnostic, RelatedLocation, SchematicLocation, Severity};
use kicad_clippy_parser::SchematicDocument;
use kiutils_sexpr::{Atom, Node};

#[derive(Clone, Copy, Debug, PartialEq)]
struct Point {
    x: f64,
    y: f64,
}

#[derive(Clone, Copy, Debug)]
struct Segment {
    start: Point,
    end: Point,
    span: ByteSpan,
}

#[derive(Clone, Debug)]
pub struct ComponentFact {
    pub reference: String,
}

#[derive(Clone, Debug, Default)]
pub struct SchematicFacts {
    pub components: Vec<ComponentFact>,
}

impl SchematicFacts {
    pub fn from_document(document: &SchematicDocument) -> Self {
        let mut facts = Self::default();
        let Some(root) = document.cst().nodes.first() else {
            return facts;
        };
        walk(root, &mut |node| {
            if head(node) != Some("symbol") {
                return;
            }
            let reference = children(node, "property").find_map(|property| {
                let items = list_items(property)?;
                if atom_value(items.get(1)?, document.source().text())? == "Reference" {
                    atom_value(items.get(2)?, document.source().text())
                } else {
                    None
                }
            });
            if let Some(reference) = reference {
                if !reference.starts_with('#') && !reference.is_empty() {
                    facts.components.push(ComponentFact { reference });
                }
            }
        });
        facts
    }
}

/// Result of the built-in rule pass.
#[derive(Clone, Debug, Default)]
pub struct RuleRun {
    /// Emitted custom-rule diagnostics.
    pub diagnostics: Vec<Diagnostic>,
    /// Number of schematic wires inspected.
    pub wires: usize,
    /// Number of placed components inspected.
    pub components: usize,
}

/// Run reusable rules that do not require a project profile.
#[must_use]
pub fn run_builtin_rules(document: &SchematicDocument) -> RuleRun {
    let facts = SchematicFacts::from_document(document);
    let Some(root) = document.cst().nodes.first() else {
        return RuleRun::default();
    };
    let source = document.source().text();
    let segments = collect_segments(root, source);
    let junctions = collect_points(root, source, "junction");
    let mut diagnostics = check_wire_geometry(&segments, &junctions);
    diagnostics.extend(check_label_topology(root, source, &segments));
    diagnostics.extend(check_label_kind_consistency(root, source));

    RuleRun {
        diagnostics,
        wires: segments.len(),
        components: facts.components.len(),
    }
}

fn check_wire_geometry(segments: &[Segment], junctions: &[Point]) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for (index, segment) in segments.iter().enumerate() {
        if distance(segment.start, segment.end) < 0.001 {
            diagnostics.push(at_segment("SCH001", "wire has zero length", *segment));
        } else if (segment.start.x - segment.end.x).abs() > 0.001
            && (segment.start.y - segment.end.y).abs() > 0.001
        {
            diagnostics.push(at_segment(
                "SCH001",
                "wire is diagonal; route with orthogonal segments",
                *segment,
            ));
        }
        for other in segments.iter().skip(index + 1) {
            if let Some(point) = intersection(*segment, *other) {
                let first_end = is_endpoint(point, *segment);
                let second_end = is_endpoint(point, *other);
                if !first_end && !second_end {
                    let mut diagnostic = at_segment(
                        "SCH001",
                        "wire crossing is ambiguous; split the net or reroute",
                        *segment,
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
                        .any(|junction| distance(*junction, point) < 0.001)
                {
                    let mut diagnostic = at_segment(
                        "SCH001",
                        "T-junction requires an explicit junction marker",
                        *segment,
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

fn check_label_topology(root: &Node, source: &str, segments: &[Segment]) -> Vec<Diagnostic> {
    let labels = collect_labels(root, source);
    let junctions = collect_points(root, source, "junction");
    let mut points = Vec::new();
    for segment in segments {
        points.push(segment.start);
        points.push(segment.end);
    }
    points.extend(junctions);
    points.extend(labels.iter().map(|(_, _, point, _)| *point));
    points.sort_by_key(|point| point_key(*point));
    points.dedup_by_key(|point| point_key(*point));

    let mut adjacency: BTreeMap<String, Vec<String>> = points
        .iter()
        .map(|point| (point_key(*point), Vec::new()))
        .collect();
    let mut endpoint_degree: BTreeMap<String, usize> = BTreeMap::new();
    for segment in segments {
        *endpoint_degree.entry(point_key(segment.start)).or_default() += 1;
        *endpoint_degree.entry(point_key(segment.end)).or_default() += 1;
        let horizontal = (segment.start.y - segment.end.y).abs() < 0.001;
        let mut split: Vec<_> = points
            .iter()
            .copied()
            .filter(|point| point_on_segment(*point, segment))
            .collect();
        split.sort_by(|first, second| {
            if horizontal {
                first.x.total_cmp(&second.x)
            } else {
                first.y.total_cmp(&second.y)
            }
        });
        for pair in split.windows(2) {
            let first = point_key(pair[0]);
            let second = point_key(pair[1]);
            adjacency
                .entry(first.clone())
                .or_default()
                .push(second.clone());
            adjacency.entry(second).or_default().push(first);
        }
    }

    let mut component_ids = BTreeMap::new();
    let mut next_component = 0_usize;
    for start in adjacency.keys() {
        if component_ids.contains_key(start) {
            continue;
        }
        let mut pending = vec![start.clone()];
        while let Some(point) = pending.pop() {
            if component_ids.contains_key(&point) {
                continue;
            }
            component_ids.insert(point.clone(), next_component);
            if let Some(neighbors) = adjacency.get(&point) {
                pending.extend(
                    neighbors
                        .iter()
                        .filter(|neighbor| !component_ids.contains_key(*neighbor))
                        .cloned(),
                );
            }
        }
        next_component += 1;
    }

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
                    "SCH002",
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
                diagnostics.push(at_point(
                    "SCH002",
                    format!("hierarchical label {label:?} at ({:.3}, {:.3}) must sit at the end of one wire twig", point.x, point.y),
                    point,
                    span,
                ));
            }
        }
    }
    for ((label, _island), count) in island_uses {
        if count > 1 {
            diagnostics.push(Diagnostic::new(
                "SCH002",
                Severity::Error,
                format!("local label {label:?} occurs {count} times on one physical wire network"),
            ));
        }
    }
    for (label, count) in totals {
        if count < 2 {
            diagnostics.push(Diagnostic::new(
                "SCH002",
                Severity::Error,
                format!("local label {label:?} is used only {count} time(s)"),
            ));
        }
    }
    diagnostics
}

fn point_key(point: Point) -> String {
    format!("{:.3}:{:.3}", point.x, point.y)
}

fn check_label_kind_consistency(root: &Node, source: &str) -> Vec<Diagnostic> {
    let mut label_kinds: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (kind, label, _point, _span) in collect_labels(root, source) {
        label_kinds.entry(label).or_default().insert(kind);
    }
    label_kinds
        .into_iter()
        .filter_map(|(label, kinds)| {
            (kinds.len() > 1).then(|| {
                Diagnostic::new(
                    "SCH019",
                    Severity::Error,
                    format!(
                        "label {label:?} mixes types: {}; use one scope consistently",
                        kinds.into_iter().collect::<Vec<_>>().join(", ")
                    ),
                )
            })
        })
        .collect()
}

fn walk<'a>(node: &'a Node, visit: &mut impl FnMut(&'a Node)) {
    visit(node);
    if let Node::List { items, .. } = node {
        for item in items {
            walk(item, visit);
        }
    }
}

fn list_items(node: &Node) -> Option<&[Node]> {
    if let Node::List { items, .. } = node {
        Some(items)
    } else {
        None
    }
}

fn head(node: &Node) -> Option<&str> {
    let Node::List { items, .. } = node else {
        return None;
    };
    let Node::Atom {
        atom: Atom::Symbol(value),
        ..
    } = items.first()?
    else {
        return None;
    };
    Some(value)
}

fn children<'a>(node: &'a Node, name: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
    list_items(node)
        .into_iter()
        .flatten()
        .filter(move |child| head(child) == Some(name))
}

fn atom_value(node: &Node, source: &str) -> Option<String> {
    let Node::Atom { span, .. } = node else {
        return None;
    };
    source
        .get(span.start..span.end)
        .map(|value| value.trim_matches('"').to_owned())
}

fn number(node: &Node, source: &str) -> Option<f64> {
    atom_value(node, source)?.parse().ok()
}

fn child_list<'a>(node: &'a Node, name: &'a str) -> Option<&'a Node> {
    children(node, name).next()
}

fn point_from_at(node: &Node, source: &str) -> Option<Point> {
    let at = child_list(node, "at")?;
    let items = list_items(at)?;
    Some(Point {
        x: number(items.get(1)?, source)?,
        y: number(items.get(2)?, source)?,
    })
}

const fn span_of(node: &Node) -> ByteSpan {
    match node {
        Node::List { span, .. } | Node::Atom { span, .. } => ByteSpan::new(span.start, span.end),
    }
}

fn collect_segments(root: &Node, source: &str) -> Vec<Segment> {
    let mut segments = Vec::new();
    walk(root, &mut |node| {
        if head(node) != Some("wire") {
            return;
        }
        let Some(points) = child_list(node, "pts") else {
            return;
        };
        let xy: Vec<_> = children(points, "xy")
            .filter_map(|point| {
                let items = list_items(point)?;
                Some(Point {
                    x: number(items.get(1)?, source)?,
                    y: number(items.get(2)?, source)?,
                })
            })
            .collect();
        if xy.len() == 2 {
            segments.push(Segment {
                start: xy[0],
                end: xy[1],
                span: span_of(node),
            });
        }
    });
    segments
}

fn collect_points(root: &Node, source: &str, name: &str) -> Vec<Point> {
    let mut points = Vec::new();
    walk(root, &mut |node| {
        if head(node) == Some(name) {
            if let Some(point) = point_from_at(node, source) {
                points.push(point);
            }
        }
    });
    points
}

fn collect_labels(root: &Node, source: &str) -> Vec<(String, String, Point, ByteSpan)> {
    let mut labels = Vec::new();
    walk(root, &mut |node| {
        let Some(kind) = head(node) else {
            return;
        };
        if !matches!(kind, "label" | "hierarchical_label" | "global_label") {
            return;
        }
        let Some(items) = list_items(node) else {
            return;
        };
        let Some(label) = items.get(1).and_then(|item| atom_value(item, source)) else {
            return;
        };
        if let Some(point) = point_from_at(node, source) {
            labels.push((kind.to_owned(), label, point, span_of(node)));
        }
    });
    labels
}

fn point_on_segment(point: Point, segment: &Segment) -> bool {
    let tolerance = 0.001;
    if (segment.start.y - segment.end.y).abs() < tolerance {
        (point.y - segment.start.y).abs() < tolerance
            && point.x >= segment.start.x.min(segment.end.x) - tolerance
            && point.x <= segment.start.x.max(segment.end.x) + tolerance
    } else if (segment.start.x - segment.end.x).abs() < tolerance {
        (point.x - segment.start.x).abs() < tolerance
            && point.y >= segment.start.y.min(segment.end.y) - tolerance
            && point.y <= segment.start.y.max(segment.end.y) + tolerance
    } else {
        false
    }
}

fn is_endpoint(point: Point, segment: Segment) -> bool {
    distance(point, segment.start) < 0.001 || distance(point, segment.end) < 0.001
}

fn distance(first: Point, second: Point) -> f64 {
    (first.x - second.x).hypot(first.y - second.y)
}

fn intersection(first: Segment, second: Segment) -> Option<Point> {
    let first_horizontal = (first.start.y - first.end.y).abs() < 0.001;
    let second_horizontal = (second.start.y - second.end.y).abs() < 0.001;
    if first_horizontal == second_horizontal {
        return None;
    }
    let (horizontal, vertical) = if first_horizontal {
        (first, second)
    } else {
        (second, first)
    };
    let point = Point {
        x: vertical.start.x,
        y: horizontal.start.y,
    };
    (point_on_segment(point, &horizontal) && point_on_segment(point, &vertical)).then_some(point)
}

fn at_segment(code: &str, message: &str, segment: Segment) -> Diagnostic {
    let mut diagnostic = Diagnostic::new(code, Severity::Error, message);
    diagnostic.span = Some(segment.span);
    diagnostic
}

fn at_point(code: &str, message: String, point: Point, span: ByteSpan) -> Diagnostic {
    let mut diagnostic = Diagnostic::new(code, Severity::Error, message);
    diagnostic.span = Some(span);
    diagnostic.schematic_location = Some(SchematicLocation {
        x_mm: point.x,
        y_mm: point.y,
    });
    diagnostic
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::run_builtin_rules;
    use kicad_clippy_parser::SchematicDocument;

    fn document(body: &str) -> SchematicDocument {
        SchematicDocument::from_text("fixture.kicad_sch", format!("(kicad_sch {body})"))
            .expect("fixture parses")
    }

    #[test]
    fn diagonal_wires_are_reported() {
        let doc = document(r"(wire (pts (xy 0 0) (xy 2 2)))");
        assert!(
            run_builtin_rules(&doc)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH001")
        );
    }

    #[test]
    fn local_labels_must_be_reused() {
        let doc = document(r#"(label "ONE" (at 1 1 0) (effects (font (size 1 1))))"#);
        assert!(
            run_builtin_rules(&doc)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH002")
        );
    }

    #[test]
    fn repeated_local_label_on_one_wire_is_rejected() {
        let doc = document(
            r#"(wire (pts (xy 0 0) (xy 4 0))) (label "NET" (at 1 0 0)) (label "NET" (at 3 0 0))"#,
        );
        assert!(run_builtin_rules(&doc).diagnostics.iter().any(|item| {
            item.code == "SCH002" && item.message.contains("one physical wire network")
        }));
    }

    #[test]
    fn repeated_local_label_joins_distinct_wire_islands() {
        let doc = document(
            r#"(wire (pts (xy 0 0) (xy 1 0))) (wire (pts (xy 3 0) (xy 4 0))) (label "NET" (at 0.5 0 0)) (label "NET" (at 3.5 0 0))"#,
        );
        assert!(
            !run_builtin_rules(&doc)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH002")
        );
    }

    #[test]
    fn same_label_cannot_be_local_and_hierarchical() {
        let doc = document(
            r#"(label "NET" (at 1 1 0) (effects (font (size 1 1)))) (hierarchical_label "NET" (shape input) (at 2 2 0) (effects (font (size 1 1))))"#,
        );
        assert!(
            run_builtin_rules(&doc)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH019")
        );
    }
}
