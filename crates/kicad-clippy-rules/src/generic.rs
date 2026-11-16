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

#[derive(Clone, Copy, Debug)]
struct Bounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Clone, Debug)]
pub struct ComponentFact {
    pub reference: String,
    /// UUID of the placed schematic symbol instance.
    pub uuid: Option<String>,
    /// Schematic symbol anchor position in millimetres.
    pub position_mm: Option<(f64, f64)>,
}

#[derive(Clone, Debug)]
pub struct SchematicGroupFact {
    /// Name shown by the native `KiCad` group.
    pub name: String,
    /// Stable UUID from the group's id field.
    pub uuid: String,
    /// UUIDs of schematic objects that belong to the group.
    pub member_uuids: BTreeSet<String>,
}

#[derive(Clone, Debug)]
pub struct SchematicFrameFact {
    /// UUID of the top-level schematic rectangle.
    pub uuid: String,
    /// Frame bounds as (left, top, right, bottom), in millimetres.
    pub bounds_mm: (f64, f64, f64, f64),
}

#[derive(Clone, Copy, Debug)]
pub struct SchematicWireFact {
    /// First endpoint of the wire segment in millimetres.
    pub start_mm: (f64, f64),
    /// Second endpoint of the wire segment in millimetres.
    pub end_mm: (f64, f64),
}

#[derive(Clone, Debug)]
pub struct TextBoxFact {
    /// UUID of the schematic text box.
    pub uuid: String,
    /// Rendered text content.
    pub text: String,
    /// Approximate text-box rectangle as (left, top, right, bottom), in millimetres.
    pub bounds_mm: Option<(f64, f64, f64, f64)>,
}

#[derive(Clone, Debug, Default)]
pub struct SchematicFacts {
    /// Placed symbol instances.
    pub components: Vec<ComponentFact>,
    /// Text boxes used for group documentation.
    pub text_boxes: Vec<TextBoxFact>,
    /// Native `KiCad` schematic groups.
    pub groups: Vec<SchematicGroupFact>,
    /// Top-level rectangular frames on the schematic sheet.
    pub frames: Vec<SchematicFrameFact>,
    /// Two-ended wire segments used by frame-scoped layout rules.
    pub wires: Vec<SchematicWireFact>,
}

impl SchematicFacts {
    pub fn from_document(document: &SchematicDocument) -> Self {
        let Some(root) = document.cst().nodes.first() else {
            return Self::default();
        };
        let source = document.source().text();
        Self {
            components: collect_component_facts(root, source),
            text_boxes: collect_text_box_facts(root, source),
            groups: collect_group_facts(root, source),
            frames: collect_frame_facts(root, source),
            wires: collect_wire_facts(root, source),
        }
    }
}

fn collect_component_facts(root: &Node, source: &str) -> Vec<ComponentFact> {
    children(root, "symbol")
        .filter_map(|node| {
            let reference = children(node, "property").find_map(|property| {
                let items = list_items(property)?;
                if atom_value(items.get(1)?, source)? == "Reference" {
                    atom_value(items.get(2)?, source)
                } else {
                    None
                }
            })?;
            if reference.starts_with('#') || reference.is_empty() {
                return None;
            }
            let uuid = child_list(node, "uuid")
                .and_then(list_items)
                .and_then(|items| items.get(1))
                .and_then(|item| atom_value(item, source));
            let position_mm = child_list(node, "at")
                .and_then(list_items)
                .and_then(|at| Some((number(at.get(1)?, source)?, number(at.get(2)?, source)?)));
            Some(ComponentFact {
                reference,
                uuid,
                position_mm,
            })
        })
        .collect()
}

fn collect_text_box_facts(root: &Node, source: &str) -> Vec<TextBoxFact> {
    children(root, "text_box")
        .filter_map(|node| {
            let items = list_items(node)?;
            let text = atom_value(items.get(1)?, source)?;
            let uuid = child_list(node, "uuid")
                .and_then(list_items)
                .and_then(|items| items.get(1))
                .and_then(|item| atom_value(item, source))?;
            let bounds_mm = text_box_bounds(node, source).map(|bounds| {
                (
                    bounds.x,
                    bounds.y,
                    bounds.x + bounds.width,
                    bounds.y + bounds.height,
                )
            });
            Some(TextBoxFact {
                uuid,
                text,
                bounds_mm,
            })
        })
        .collect()
}

fn collect_group_facts(root: &Node, source: &str) -> Vec<SchematicGroupFact> {
    children(root, "group")
        .filter_map(|node| {
            let items = list_items(node)?;
            let name = atom_value(items.get(1)?, source)?;
            let uuid = child_list(node, "id")
                .and_then(list_items)
                .and_then(|items| items.get(1))
                .and_then(|item| atom_value(item, source))?;
            let member_uuids = child_list(node, "members")
                .and_then(list_items)
                .into_iter()
                .flatten()
                .skip(1)
                .filter_map(|item| atom_value(item, source))
                .collect();
            Some(SchematicGroupFact {
                name,
                uuid,
                member_uuids,
            })
        })
        .collect()
}

fn collect_frame_facts(root: &Node, source: &str) -> Vec<SchematicFrameFact> {
    children(root, "rectangle")
        .filter_map(|node| {
            let uuid = child_list(node, "uuid")
                .and_then(list_items)
                .and_then(|items| items.get(1))
                .and_then(|item| atom_value(item, source))?;
            let start = child_list(node, "start")
                .and_then(list_items)
                .and_then(|items| {
                    Some((
                        number(items.get(1)?, source)?,
                        number(items.get(2)?, source)?,
                    ))
                })?;
            let end = child_list(node, "end")
                .and_then(list_items)
                .and_then(|items| {
                    Some((
                        number(items.get(1)?, source)?,
                        number(items.get(2)?, source)?,
                    ))
                })?;
            Some(SchematicFrameFact {
                uuid,
                bounds_mm: (
                    start.0.min(end.0),
                    start.1.min(end.1),
                    start.0.max(end.0),
                    start.1.max(end.1),
                ),
            })
        })
        .collect()
}

fn collect_wire_facts(root: &Node, source: &str) -> Vec<SchematicWireFact> {
    collect_segments(root, source)
        .into_iter()
        .map(|segment| SchematicWireFact {
            start_mm: (segment.start.x, segment.start.y),
            end_mm: (segment.end.x, segment.end.y),
        })
        .collect()
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
    let mut diagnostics = check_wire_geometry(root, &segments, &junctions);
    diagnostics.extend(check_pin_orientation(root, source, &segments));
    diagnostics.extend(check_no_connect_markers(root, source));
    diagnostics.extend(check_label_topology(root, source, &segments));
    diagnostics.extend(check_label_wire_clearance(root, source, &segments));
    diagnostics.extend(check_text_box_layout(root, source, &segments));
    diagnostics.extend(check_block_annotation_structure(root, source));
    diagnostics.extend(check_label_text_collisions(root, source));
    diagnostics.extend(check_symbol_field_clearance(root, source));
    diagnostics.extend(check_label_kind_consistency(root, source));

    RuleRun {
        diagnostics,
        wires: segments.len(),
        components: facts.components.len(),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PinAxis {
    Horizontal,
    Vertical,
}

impl PinAxis {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Horizontal => "horizontal",
            Self::Vertical => "vertical",
        }
    }
}

#[derive(Clone, Debug)]
struct SymbolTransform {
    reference: String,
    origin: Point,
    rotation: i32,
    mirror: Option<String>,
    unit: u32,
    convert: u32,
}

fn check_pin_orientation(root: &Node, source: &str, segments: &[Segment]) -> Vec<Diagnostic> {
    let Some(library_symbols) = child_list(root, "lib_symbols") else {
        return Vec::new();
    };
    children(root, "symbol")
        .flat_map(|instance| {
            check_instance_pin_orientation(instance, library_symbols, source, segments)
        })
        .collect()
}

fn check_instance_pin_orientation(
    instance: &Node,
    library_symbols: &Node,
    source: &str,
    segments: &[Segment],
) -> Vec<Diagnostic> {
    let Some(transform) = read_symbol_transform(instance, source) else {
        return Vec::new();
    };
    if transform.reference.starts_with('#') {
        return Vec::new();
    }
    let Some(lib_id) = child_list(instance, "lib_id")
        .and_then(list_items)
        .and_then(|items| items.get(1))
        .and_then(|item| atom_value(item, source))
    else {
        return Vec::new();
    };
    let Some(library_symbol) = children(library_symbols, "symbol").find(|definition| {
        list_items(definition)
            .and_then(|items| items.get(1))
            .and_then(|item| atom_value(item, source))
            .is_some_and(|name| name == lib_id)
    }) else {
        return Vec::new();
    };

    embedded_symbol_pins(library_symbol, transform.unit, transform.convert, source)
        .into_iter()
        .filter_map(|pin| pin_orientation_diagnostic(instance, pin, &transform, source, segments))
        .collect()
}

fn check_no_connect_markers(root: &Node, source: &str) -> Vec<Diagnostic> {
    let markers = collect_nodes(root, "no_connect");
    if markers.is_empty() {
        return Vec::new();
    }
    let Some(pin_positions) = collect_placed_pin_positions(root, source) else {
        return Vec::new();
    };
    markers
        .into_iter()
        .filter_map(|marker| {
            let point = point_from_at(marker, source)?;
            let matches_pin = pin_positions
                .iter()
                .any(|pin| distance(*pin, point) < 0.001);
            (!matches_pin).then(|| {
                at_point(
                    "SCH015",
                    format!(
                        "no-connect marker at ({:.3}, {:.3}) does not coincide with a placed symbol pin",
                        point.x, point.y
                    ),
                    point,
                    span_of(marker),
                )
            })
        })
        .collect()
}

fn collect_placed_pin_positions(root: &Node, source: &str) -> Option<Vec<Point>> {
    let mut positions = Vec::new();
    let Some(library_symbols) = child_list(root, "lib_symbols") else {
        return Some(positions);
    };
    for instance in children(root, "symbol") {
        let Some(lib_id) = child_list(instance, "lib_id")
            .and_then(list_items)
            .and_then(|items| items.get(1))
            .and_then(|item| atom_value(item, source))
        else {
            continue;
        };
        let transform = read_symbol_transform(instance, source)?;
        let library_symbol = children(library_symbols, "symbol").find(|definition| {
            list_items(definition)
                .and_then(|items| items.get(1))
                .and_then(|item| atom_value(item, source))
                .is_some_and(|name| name == lib_id)
        })?;
        let pins = embedded_symbol_pins(library_symbol, transform.unit, transform.convert, source);
        if pins.is_empty() {
            return None;
        }
        for pin in pins {
            let at = child_list(pin, "at").and_then(list_items)?;
            let pin_x = number(at.get(1)?, source)?;
            let pin_y = number(at.get(2)?, source)?;
            let (x_offset, y_offset) = transform_symbol_point(
                pin_x,
                pin_y,
                transform.rotation,
                transform.mirror.as_deref(),
            )?;
            positions.push(Point {
                x: transform.origin.x + x_offset,
                y: transform.origin.y + y_offset,
            });
        }
    }
    Some(positions)
}

fn read_symbol_transform(instance: &Node, source: &str) -> Option<SymbolTransform> {
    let reference = children(instance, "property").find_map(|property| {
        let items = list_items(property)?;
        if atom_value(items.get(1)?, source)? == "Reference" {
            atom_value(items.get(2)?, source)
        } else {
            None
        }
    })?;
    let at = child_list(instance, "at").and_then(list_items)?;
    let origin = Point {
        x: number(at.get(1)?, source)?,
        y: number(at.get(2)?, source)?,
    };
    let rotation = cardinal_rotation(
        at.get(3)
            .and_then(|item| number(item, source))
            .unwrap_or(0.0),
    )?;
    let unit = instance_integer(instance, "unit", source).unwrap_or(1);
    let convert = instance_integer(instance, "convert", source).unwrap_or(1);
    let mirror = child_list(instance, "mirror")
        .and_then(list_items)
        .and_then(|items| items.get(1))
        .and_then(|item| atom_value(item, source));
    if mirror
        .as_ref()
        .is_some_and(|value| value != "x" && value != "y")
    {
        return None;
    }
    Some(SymbolTransform {
        reference,
        origin,
        rotation,
        mirror,
        unit,
        convert,
    })
}

fn instance_integer(instance: &Node, name: &str, source: &str) -> Option<u32> {
    child_list(instance, name)
        .and_then(list_items)
        .and_then(|items| items.get(1))
        .and_then(|item| atom_value(item, source))
        .and_then(|value| value.parse().ok())
}

fn embedded_symbol_pins<'a>(
    library_symbol: &'a Node,
    unit: u32,
    convert: u32,
    source: &str,
) -> Vec<&'a Node> {
    let mut pin_nodes = Vec::new();
    for sub_symbol in children(library_symbol, "symbol") {
        let Some(name) = list_items(sub_symbol)
            .and_then(|items| items.get(1))
            .and_then(|item| atom_value(item, source))
        else {
            continue;
        };
        let Some((sub_unit, sub_convert)) = sub_symbol_unit_convert(&name) else {
            continue;
        };
        if (sub_unit == 0 || sub_unit == unit) && sub_convert == convert {
            pin_nodes.extend(children(sub_symbol, "pin"));
        }
    }
    if pin_nodes.is_empty() {
        pin_nodes.extend(children(library_symbol, "pin"));
    }
    pin_nodes
}

fn pin_orientation_diagnostic(
    instance: &Node,
    pin: &Node,
    transform: &SymbolTransform,
    source: &str,
    segments: &[Segment],
) -> Option<Diagnostic> {
    let at = child_list(pin, "at").and_then(list_items)?;
    let pin_x = number(at.get(1)?, source)?;
    let pin_y = number(at.get(2)?, source)?;
    let pin_angle = number(at.get(3)?, source)?;
    let (x_offset, y_offset) = transform_symbol_point(
        pin_x,
        pin_y,
        transform.rotation,
        transform.mirror.as_deref(),
    )?;
    let point = Point {
        x: transform.origin.x + x_offset,
        y: transform.origin.y + y_offset,
    };
    let (axis_x, axis_y) =
        transform_symbol_vector(pin_angle, transform.rotation, transform.mirror.as_deref())?;
    let expected_axis = if axis_x.abs() >= axis_y.abs() {
        PinAxis::Horizontal
    } else {
        PinAxis::Vertical
    };
    let touching_segments: Vec<_> = segments
        .iter()
        .filter(|segment| point_on_segment(point, segment))
        .collect();
    if touching_segments.is_empty() {
        return None;
    }
    let aligned = touching_segments.iter().any(|segment| match expected_axis {
        PinAxis::Horizontal => (segment.start.y - segment.end.y).abs() < 0.001,
        PinAxis::Vertical => (segment.start.x - segment.end.x).abs() < 0.001,
    });
    if aligned {
        return None;
    }
    let pin_number = child_list(pin, "number")
        .and_then(list_items)
        .and_then(|items| items.get(1))
        .and_then(|item| atom_value(item, source))
        .unwrap_or_else(|| "?".to_owned());
    Some(at_point(
        "SCH005",
        format!(
            "{} pin {pin_number} faces {} but its connected wire is not aligned with the pin",
            transform.reference,
            expected_axis.as_str()
        ),
        point,
        span_of(instance),
    ))
}

fn sub_symbol_unit_convert(name: &str) -> Option<(u32, u32)> {
    let (prefix, convert) = name.rsplit_once('_')?;
    let (_, unit) = prefix.rsplit_once('_')?;
    Some((unit.parse().ok()?, convert.parse().ok()?))
}

fn cardinal_rotation(angle: f64) -> Option<i32> {
    let normalized = angle.rem_euclid(360.0);
    [(0.0, 0), (90.0, 90), (180.0, 180), (270.0, 270)]
        .into_iter()
        .find_map(|(degrees, rotation)| ((normalized - degrees).abs() <= 0.001).then_some(rotation))
}

fn transform_symbol_point(
    mut x: f64,
    mut y: f64,
    rotation: i32,
    mirror: Option<&str>,
) -> Option<(f64, f64)> {
    match mirror {
        Some("x") => y = -y,
        Some("y") => x = -x,
        None => {}
        Some(_) => return None,
    }
    Some(match rotation {
        0 => (x, -y),
        90 => (-y, -x),
        180 => (-x, y),
        270 => (y, x),
        _ => return None,
    })
}

fn transform_symbol_vector(angle: f64, rotation: i32, mirror: Option<&str>) -> Option<(f64, f64)> {
    let radians = angle.to_radians();
    let (mut x, mut y) = (radians.cos(), radians.sin());
    match mirror {
        Some("x") => y = -y,
        Some("y") => x = -x,
        None => {}
        Some(_) => return None,
    }
    Some(match rotation {
        0 => (x, -y),
        90 => (-y, -x),
        180 => (-x, y),
        270 => (y, x),
        _ => return None,
    })
}

fn check_wire_geometry(root: &Node, segments: &[Segment], junctions: &[Point]) -> Vec<Diagnostic> {
    let mut diagnostics = collect_invalid_wire_diagnostics(root);
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
            if collinear_overlap(*segment, *other) {
                diagnostics.push(at_segment(
                    "SCH001",
                    "collinear wire segments overlap",
                    *segment,
                ));
                continue;
            }
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
                "SCH001",
                "wire must have exactly two endpoints",
                span_of(node),
            ));
        }
    });
    diagnostics
}

fn collinear_overlap(first: Segment, second: Segment) -> bool {
    let first_horizontal = (first.start.y - first.end.y).abs() < 0.001;
    let second_horizontal = (second.start.y - second.end.y).abs() < 0.001;
    let first_vertical = (first.start.x - first.end.x).abs() < 0.001;
    let second_vertical = (second.start.x - second.end.x).abs() < 0.001;
    let overlap = if first_horizontal
        && second_horizontal
        && (first.start.y - second.start.y).abs() < 0.001
    {
        first
            .start
            .x
            .max(first.end.x)
            .min(second.start.x.max(second.end.x))
            - first
                .start
                .x
                .min(first.end.x)
                .max(second.start.x.min(second.end.x))
    } else if first_vertical && second_vertical && (first.start.x - second.start.x).abs() < 0.001 {
        first
            .start
            .y
            .max(first.end.y)
            .min(second.start.y.max(second.end.y))
            - first
                .start
                .y
                .min(first.end.y)
                .max(second.start.y.min(second.end.y))
    } else {
        0.0
    };
    overlap > 0.001
}

fn check_label_wire_clearance(root: &Node, source: &str, segments: &[Segment]) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for (kind, label, _point, span, node) in collect_label_nodes(root, source) {
        if !matches!(kind.as_str(), "label" | "hierarchical_label") {
            continue;
        }
        let Some(bounds) = text_visual_bounds(&label, node, source) else {
            continue;
        };
        if segments
            .iter()
            .any(|segment| strict_line_hits_bounds(*segment, bounds))
        {
            diagnostics.push(at_span(
                "SCH003",
                format!("{kind} {label:?} overlaps a wire's visible glyph area"),
                span,
            ));
        }
    }
    diagnostics
}

fn check_text_box_layout(root: &Node, source: &str, segments: &[Segment]) -> Vec<Diagnostic> {
    let mut diagnostics: Vec<_> = collect_nodes(root, "text")
        .into_iter()
        .map(|node| {
            at_span(
                "SCH008",
                "free-standing annotation text is forbidden; use a KiCad text box",
                span_of(node),
            )
        })
        .collect();
    let mut boxes = Vec::new();
    for (index, node) in collect_nodes(root, "text_box").into_iter().enumerate() {
        let (record, mut box_diagnostics) = inspect_text_box(node, index, source);
        diagnostics.append(&mut box_diagnostics);
        if let Some(record) = record {
            boxes.push(record);
        }
    }
    diagnostics.extend(check_text_box_interactions(&boxes, segments));
    diagnostics
}

#[derive(Clone, Debug)]
struct TextBoxRecord {
    id: String,
    bounds: Bounds,
    span: ByteSpan,
}

fn inspect_text_box(
    node: &Node,
    index: usize,
    source: &str,
) -> (Option<TextBoxRecord>, Vec<Diagnostic>) {
    let (metrics, error) = read_text_box_metrics(node, index, source);
    let Some(metrics) = metrics else {
        return (None, error.into_iter().collect());
    };
    let (required_width, required_height) =
        required_text_box_size(&metrics.label, metrics.font_size, metrics.padding);
    let mut diagnostics = Vec::new();
    if metrics.bounds.width + 0.001 < required_width
        || metrics.bounds.height + 0.001 < required_height
    {
        diagnostics.push(at_span(
            "SCH008",
            format!(
                "text_box {index} is undersized ({:.2}x{:.2} mm); needs {required_width:.2}x{required_height:.2} mm",
                metrics.bounds.width, metrics.bounds.height
            ),
            metrics.span,
        ));
    }
    if metrics.stroke_width <= 0.0 && metrics.bounds.width + 0.001 < required_width + 10.16 {
        diagnostics.push(at_span(
            "SCH008",
            format!(
                "unbordered text_box {index} needs at least {:.2} mm width for reliable single-line rendering",
                required_width + 10.16
            ),
            metrics.span,
        ));
    }
    (
        Some(TextBoxRecord {
            id: format!("text_box-{index}"),
            bounds: metrics.bounds,
            span: metrics.span,
        }),
        diagnostics,
    )
}

#[derive(Clone, Debug)]
struct TextBoxMetrics {
    label: String,
    bounds: Bounds,
    font_size: f64,
    padding: f64,
    stroke_width: f64,
    span: ByteSpan,
}

fn read_text_box_metrics(
    node: &Node,
    index: usize,
    source: &str,
) -> (Option<TextBoxMetrics>, Option<Diagnostic>) {
    let span = span_of(node);
    let Some(items) = list_items(node) else {
        return (
            None,
            Some(at_span(
                "SCH008",
                format!("text_box {index} is malformed"),
                span,
            )),
        );
    };
    let label = items
        .get(1)
        .and_then(|item| atom_value(item, source))
        .unwrap_or_default();
    let at = child_list(node, "at").and_then(list_items);
    let size = child_list(node, "size").and_then(list_items);
    let margins = child_list(node, "margins").and_then(list_items);
    let font_size = child_list(node, "effects")
        .and_then(|effects| child_list(effects, "font"))
        .and_then(|font| child_list(font, "size"))
        .and_then(list_items)
        .and_then(|font| font.get(1))
        .and_then(|item| number(item, source));
    let (Some(at), Some(size), Some(margins), Some(font_size)) = (at, size, margins, font_size)
    else {
        return (
            None,
            Some(at_span(
                "SCH008",
                format!("text_box {index} is missing geometry, margins or font"),
                span,
            )),
        );
    };
    let pads: Option<Vec<f64>> = margins
        .iter()
        .skip(1)
        .map(|item| number(item, source))
        .collect();
    let Some(pads) = pads else {
        return (
            None,
            Some(at_span(
                "SCH008",
                format!("text_box {index} has invalid margins"),
                span,
            )),
        );
    };
    if pads.len() != 4 || pads.iter().any(|pad| (pad - 1.27).abs() > 0.001) {
        return (
            None,
            Some(at_span(
                "SCH008",
                format!("text_box {index} must have 1.27 mm equal padding"),
                span,
            )),
        );
    }
    let dimensions = (
        at.get(1).and_then(|item| number(item, source)),
        at.get(2).and_then(|item| number(item, source)),
        size.get(1).and_then(|item| number(item, source)),
        size.get(2).and_then(|item| number(item, source)),
    );
    let (Some(x), Some(y), Some(width), Some(height)) = dimensions else {
        return (
            None,
            Some(at_span(
                "SCH008",
                format!("text_box {index} has invalid dimensions"),
                span,
            )),
        );
    };
    let stroke_width = child_list(node, "stroke")
        .and_then(|stroke| child_list(stroke, "width"))
        .and_then(list_items)
        .and_then(|width| width.get(1))
        .and_then(|item| number(item, source))
        .unwrap_or(-1.0);
    (
        Some(TextBoxMetrics {
            label,
            bounds: Bounds {
                x,
                y,
                width,
                height,
            },
            font_size,
            padding: pads[0],
            stroke_width,
            span,
        }),
        None,
    )
}

fn check_text_box_interactions(boxes: &[TextBoxRecord], segments: &[Segment]) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for (index, first) in boxes.iter().enumerate() {
        for second in boxes.iter().skip(index + 1) {
            if bounds_overlap(first.bounds, second.bounds) {
                diagnostics.push(at_span(
                    "SCH008",
                    format!("text boxes overlap: {} and {}", first.id, second.id),
                    first.span,
                ));
            }
        }
        if segments
            .iter()
            .any(|segment| line_hits_bounds(*segment, first.bounds))
        {
            diagnostics.push(at_span(
                "SCH008",
                format!("a wire penetrates text box {}", first.id),
                first.span,
            ));
        }
    }
    diagnostics
}

fn check_block_annotation_structure(root: &Node, source: &str) -> Vec<Diagnostic> {
    collect_nodes(root, "text_box")
        .into_iter()
        .filter_map(|node| {
            let items = list_items(node)?;
            let value = atom_value(items.get(1)?, source)?;
            let multiline = value.contains('\n')
                || value.contains('\r')
                || value.contains("\\n")
                || value.contains("\\r");
            let stroke_width = child_list(node, "stroke")
                .and_then(|stroke| child_list(stroke, "width"))
                .and_then(|width| list_items(width))
                .and_then(|width| width.get(1))
                .and_then(|item| number(item, source))
                .unwrap_or(-1.0);
            (multiline && stroke_width <= 0.0).then(|| {
                at_span(
                    "SCH011",
                    "multiline annotation must be a single line or have its own visible box outline",
                    span_of(node),
                )
            })
        })
        .collect()
}

fn collect_nodes<'a>(root: &'a Node, name: &'a str) -> Vec<&'a Node> {
    children(root, name).collect()
}

fn collect_label_nodes<'a>(
    root: &'a Node,
    source: &str,
) -> Vec<(String, String, Point, ByteSpan, &'a Node)> {
    let mut labels = Vec::new();
    for node in collect_nodes(root, "label")
        .into_iter()
        .chain(collect_nodes(root, "hierarchical_label"))
        .chain(collect_nodes(root, "global_label"))
    {
        let Some(items) = list_items(node) else {
            continue;
        };
        let Some(label) = items.get(1).and_then(|item| atom_value(item, source)) else {
            continue;
        };
        let Some(point) = point_from_at(node, source) else {
            continue;
        };
        let Some(kind) = head(node) else {
            continue;
        };
        labels.push((kind.to_owned(), label, point, span_of(node), node));
    }
    labels
}

fn text_visual_bounds(value: &str, node: &Node, source: &str) -> Option<Bounds> {
    let at = child_list(node, "at").and_then(list_items)?;
    let effects = child_list(node, "effects");
    let font_size = effects
        .and_then(|effects| child_list(effects, "font"))
        .and_then(|font| child_list(font, "size"))
        .and_then(list_items)
        .and_then(|font| font.get(1))
        .and_then(|item| number(item, source))
        .unwrap_or(1.0);
    let width = (value.chars().map(char_advance).sum::<f64>() * font_size * 1.12).max(font_size);
    let height = font_size * 1.55;
    let x = number(at.get(1)?, source)?;
    let y = number(at.get(2)?, source)?;
    let angle = at
        .get(3)
        .and_then(|item| number(item, source))
        .unwrap_or(0.0);
    let justify = effects.and_then(|effects| child_list(effects, "justify"));
    let justify_items = justify.and_then(list_items);
    let justify_name = justify_items
        .and_then(|items| items.get(1))
        .and_then(|item| atom_value(item, source))
        .unwrap_or_else(|| "center".to_owned());
    let left = match justify_name.as_str() {
        "left" => x,
        "right" => x - width,
        _ => x - width / 2.0,
    };
    if (angle - 90.0).abs() < 0.001 {
        let top = if justify_name == "left" {
            y - width
        } else if justify_name == "right" {
            y
        } else {
            y - width / 2.0
        };
        return Some(Bounds {
            x: x - height / 2.0,
            y: top,
            width: height,
            height: width,
        });
    }
    if (angle - 270.0).abs() < 0.001 {
        let top = if justify_name == "left" {
            y
        } else if justify_name == "right" {
            y - width
        } else {
            y - width / 2.0
        };
        return Some(Bounds {
            x: x - height / 2.0,
            y: top,
            width: height,
            height: width,
        });
    }
    if justify_items
        .and_then(|items| items.get(2))
        .and_then(|item| atom_value(item, source))
        .is_some_and(|value| value == "top")
    {
        return Some(Bounds {
            x: left,
            y,
            width,
            height,
        });
    }
    Some(Bounds {
        x: left,
        y: y - height,
        width,
        height,
    })
}

fn char_advance(character: char) -> f64 {
    if "ilI.,:;!|'".contains(character) {
        0.30
    } else if "MW@%&".contains(character) {
        0.90
    } else if character.is_whitespace() {
        0.34
    } else if character.is_uppercase() || character.is_ascii_digit() {
        0.64
    } else {
        0.55
    }
}

fn required_text_box_size(value: &str, font_size: f64, padding: f64) -> (f64, f64) {
    let normalized = value.replace("\\n", "\n").replace("\\r", "\r");
    let lines: Vec<_> = normalized.lines().collect();
    let line_count = u32::try_from(lines.len().max(1)).unwrap_or(u32::MAX);
    let max_width = lines
        .iter()
        .map(|line| line.chars().map(char_advance).sum::<f64>())
        .fold(0.0_f64, f64::max);
    (
        2.0_f64.mul_add(padding, (max_width * font_size * 1.30).max(font_size)),
        (f64::from(line_count) * font_size).mul_add(1.55, 2.0 * padding),
    )
}

fn bounds_overlap(first: Bounds, second: Bounds) -> bool {
    (first.x + first.width).min(second.x + second.width) - first.x.max(second.x) > 0.001
        && (first.y + first.height).min(second.y + second.height) - first.y.max(second.y) > 0.001
}

fn line_hits_bounds(segment: Segment, bounds: Bounds) -> bool {
    let tolerance = 0.001;
    let horizontal = (segment.start.y - segment.end.y).abs() < tolerance;
    if horizontal {
        bounds.y - tolerance <= segment.start.y
            && segment.start.y <= bounds.y + bounds.height + tolerance
            && segment
                .start
                .x
                .max(segment.end.x)
                .min(bounds.x + bounds.width)
                - segment.start.x.min(segment.end.x).max(bounds.x)
                > tolerance
    } else if (segment.start.x - segment.end.x).abs() < tolerance {
        bounds.x - tolerance <= segment.start.x
            && segment.start.x <= bounds.x + bounds.width + tolerance
            && segment
                .start
                .y
                .max(segment.end.y)
                .min(bounds.y + bounds.height)
                - segment.start.y.min(segment.end.y).max(bounds.y)
                > tolerance
    } else {
        false
    }
}

fn strict_line_hits_bounds(segment: Segment, bounds: Bounds) -> bool {
    let tolerance = 0.001;
    let horizontal = (segment.start.y - segment.end.y).abs() < tolerance;
    if horizontal {
        bounds.y + tolerance < segment.start.y
            && segment.start.y < bounds.y + bounds.height - tolerance
            && segment
                .start
                .x
                .max(segment.end.x)
                .min(bounds.x + bounds.width)
                - segment.start.x.min(segment.end.x).max(bounds.x)
                > tolerance
    } else if (segment.start.x - segment.end.x).abs() < tolerance {
        bounds.x + tolerance < segment.start.x
            && segment.start.x < bounds.x + bounds.width - tolerance
            && segment
                .start
                .y
                .max(segment.end.y)
                .min(bounds.y + bounds.height)
                - segment.start.y.min(segment.end.y).max(bounds.y)
                > tolerance
    } else {
        false
    }
}

fn at_span(code: &str, message: impl Into<String>, span: ByteSpan) -> Diagnostic {
    let mut diagnostic = Diagnostic::new(code, Severity::Error, message);
    diagnostic.span = Some(span);
    diagnostic
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

    let (component_ids, endpoint_degree) = wire_components(segments, &points);

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

fn wire_components(
    segments: &[Segment],
    points: &[Point],
) -> (BTreeMap<String, usize>, BTreeMap<String, usize>) {
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
    (component_ids, endpoint_degree)
}

fn point_key(point: Point) -> String {
    format!("{:.3}:{:.3}", point.x, point.y)
}

fn check_label_text_collisions(root: &Node, source: &str) -> Vec<Diagnostic> {
    let mut labels = Vec::new();
    for (kind, label, _point, span, node) in collect_label_nodes(root, source) {
        if let Some(bounds) = text_visual_bounds(&label, node, source) {
            labels.push((format!("{kind} {label:?}"), bounds, span));
        }
    }

    let mut diagnostics = Vec::new();
    for (index, (first_name, first_bounds, first_span)) in labels.iter().enumerate() {
        for (second_name, second_bounds, _second_span) in labels.iter().skip(index + 1) {
            if bounds_overlap(*first_bounds, *second_bounds) {
                diagnostics.push(at_span(
                    "SCH018",
                    format!("label text overlaps: {first_name} and {second_name}"),
                    *first_span,
                ));
            }
        }
        for property in children(root, "symbol").flat_map(|symbol| children(symbol, "property")) {
            if property_is_hidden(property) {
                continue;
            }
            let Some(items) = list_items(property) else {
                continue;
            };
            let Some(value) = items.get(2).and_then(|item| atom_value(item, source)) else {
                continue;
            };
            if text_visual_bounds(&value, property, source)
                .is_some_and(|field_bounds| bounds_overlap(*first_bounds, field_bounds))
            {
                diagnostics.push(at_span(
                    "SCH018",
                    format!("{first_name} overlaps a visible symbol field"),
                    *first_span,
                ));
            }
        }
        for text_box in collect_nodes(root, "text_box") {
            if text_box_bounds(text_box, source)
                .is_some_and(|box_bounds| bounds_overlap(*first_bounds, box_bounds))
            {
                diagnostics.push(at_span(
                    "SCH018",
                    format!("{first_name} overlaps an annotation text box"),
                    *first_span,
                ));
            }
        }
    }
    diagnostics
}

fn check_symbol_field_clearance(root: &Node, source: &str) -> Vec<Diagnostic> {
    let mut fields = Vec::new();
    for symbol in children(root, "symbol") {
        let reference = children(symbol, "property").find_map(|property| {
            let items = list_items(property)?;
            if atom_value(items.get(1)?, source)? == "Reference" {
                atom_value(items.get(2)?, source)
            } else {
                None
            }
        });
        let Some(reference) = reference else {
            continue;
        };
        for property in children(symbol, "property") {
            if property_is_hidden(property) {
                continue;
            }
            let Some(items) = list_items(property) else {
                continue;
            };
            let Some(field_name) = items.get(1).and_then(|item| atom_value(item, source)) else {
                continue;
            };
            let Some(value) = items.get(2).and_then(|item| atom_value(item, source)) else {
                continue;
            };
            if value.trim().is_empty() {
                continue;
            }
            if let Some(bounds) = text_visual_bounds(&value, property, source) {
                fields.push((
                    format!("{reference} {field_name} {value:?}"),
                    bounds,
                    span_of(property),
                ));
            }
        }
    }

    let mut diagnostics = Vec::new();
    for (index, (first_name, first_bounds, first_span)) in fields.iter().enumerate() {
        for (second_name, second_bounds, _second_span) in fields.iter().skip(index + 1) {
            if bounds_overlap(*first_bounds, *second_bounds) {
                diagnostics.push(at_span(
                    "SCH010",
                    format!("visible symbol fields overlap: {first_name} and {second_name}"),
                    *first_span,
                ));
            }
        }
        for text_box in collect_nodes(root, "text_box") {
            if text_box_bounds(text_box, source)
                .is_some_and(|box_bounds| bounds_overlap(*first_bounds, box_bounds))
            {
                diagnostics.push(at_span(
                    "SCH010",
                    format!("{first_name} overlaps an annotation text box"),
                    *first_span,
                ));
            }
        }
    }
    diagnostics
}

fn property_is_hidden(property: &Node) -> bool {
    child_list(property, "effects").is_some_and(|effects| child_list(effects, "hide").is_some())
}

fn text_box_bounds(node: &Node, source: &str) -> Option<Bounds> {
    let at = child_list(node, "at").and_then(list_items)?;
    let size = child_list(node, "size").and_then(list_items)?;
    Some(Bounds {
        x: number(at.get(1)?, source)?,
        y: number(at.get(2)?, source)?,
        width: number(size.get(1)?, source)?,
        height: number(size.get(2)?, source)?,
    })
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

    const EMBEDDED_PIN: &str = r#"
        (lib_symbols
          (symbol "Device:Test"
            (property "Reference" "R" (at 0 2.54 0) (effects (font (size 1 1))))
            (symbol "Test_1_1"
              (pin passive line (at 0 2.54 90) (length 1.27)
                (name "1") (number "1"))
            )
          )
        )
        (symbol (lib_id "Device:Test") (at 10 10 0) (unit 1)
          (property "Reference" "J1" (at 10 10 0) (effects (font (size 1 1))))
        )
    "#;

    #[test]
    fn pin_wire_must_follow_the_embedded_pin_axis() {
        let doc = document(&format!(
            "{EMBEDDED_PIN} (wire (pts (xy 10 7.46) (xy 11.27 7.46)))"
        ));
        assert!(
            run_builtin_rules(&doc)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH005" && item.message.contains("J1 pin 1"))
        );
    }

    #[test]
    fn correctly_aligned_pin_wire_is_accepted() {
        let doc = document(&format!(
            "{EMBEDDED_PIN} (wire (pts (xy 10 7.46) (xy 10 6.19)))"
        ));
        assert!(
            !run_builtin_rules(&doc)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH005")
        );
    }

    #[test]
    fn pin_axis_and_position_follow_symbol_rotation_and_mirroring() {
        let doc = document(
            r#"
            (lib_symbols
              (symbol "Device:Test"
                (symbol "Test_1_1"
                  (pin passive line (at 2.54 0 0) (length 1.27)
                    (name "1") (number "1"))
                )
              )
            )
            (symbol (lib_id "Device:Test") (at 10 10 90) (mirror y) (unit 1)
              (property "Reference" "J2" (at 10 10 0) (effects (font (size 1 1))))
            )
            (wire (pts (xy 10 12.54) (xy 10 13.81)))
            "#,
        );
        assert!(
            !run_builtin_rules(&doc)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH005")
        );
    }

    #[test]
    fn embedded_library_fields_are_not_counted_as_placed_components() {
        let doc = document(EMBEDDED_PIN);
        let run = run_builtin_rules(&doc);
        assert_eq!(run.components, 1);
    }

    #[test]
    fn no_connect_marker_must_coincide_with_a_placed_symbol_pin() {
        let doc = document(&format!(
            "{EMBEDDED_PIN} (no_connect (at 11 11) (uuid \"no-connect-id\"))"
        ));
        assert!(
            run_builtin_rules(&doc)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH015" && item.message.contains("does not coincide"))
        );
    }

    #[test]
    fn correctly_placed_no_connect_marker_is_accepted_even_for_power_symbols() {
        let power_symbol = EMBEDDED_PIN.replace("\"J1\"", "\"#PWR001\"");
        let doc = document(&format!(
            "{power_symbol} (no_connect (at 10 7.46) (uuid \"no-connect-id\"))"
        ));
        assert!(
            !run_builtin_rules(&doc)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH015")
        );
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
    fn wire_overlap_is_rejected() {
        let doc = document(r"(wire (pts (xy 0 0) (xy 4 0))) (wire (pts (xy 2 0) (xy 6 0)))");
        assert!(
            run_builtin_rules(&doc)
                .diagnostics
                .iter()
                .any(|item| { item.code == "SCH001" && item.message.contains("overlap") })
        );
    }

    #[test]
    fn wire_through_label_glyph_is_rejected() {
        let doc = document(
            r#"(label "ABC" (at 5 5 0) (effects (font (size 1 1)))) (wire (pts (xy 3 4.2) (xy 7 4.2)))"#,
        );
        assert!(
            run_builtin_rules(&doc)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH003")
        );
    }

    #[test]
    fn undersized_text_box_is_rejected() {
        let doc = document(
            r#"(text_box "note" (at 1 1 0) (size 5 5) (margins 1.27 1.27 1.27 1.27) (stroke (width -1)) (effects (font (size 1 1))))"#,
        );
        assert!(
            run_builtin_rules(&doc)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH008")
        );
    }

    #[test]
    fn free_standing_text_is_rejected() {
        let doc = document(r#"(text "note" (at 1 1 0) (effects (font (size 1 1))))"#);
        assert!(
            run_builtin_rules(&doc)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH008")
        );
    }

    #[test]
    fn multiline_unbordered_text_box_is_rejected() {
        let doc = document(
            r#"(text_box "first line\nsecond line" (at 1 1 0) (size 40 8) (margins 1.27 1.27 1.27 1.27) (stroke (width -1)) (effects (font (size 1 1))))"#,
        );
        assert!(
            run_builtin_rules(&doc)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH011")
        );
    }

    #[test]
    fn overlapping_label_glyphs_are_rejected() {
        let doc = document(
            r#"(label "FIRST" (at 5 5 0) (effects (font (size 1 1)))) (global_label "SECOND" (shape input) (at 5 5 0) (effects (font (size 1 1))))"#,
        );
        assert!(
            run_builtin_rules(&doc)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH018")
        );
    }

    #[test]
    fn visible_symbol_fields_may_not_overlap_each_other() {
        let doc = document(
            r#"
            (symbol (lib_id "Device:R") (at 1 1 0)
              (property "Reference" "R1" (at 5 5 0) (effects (font (size 1 1))))
              (property "Value" "10k" (at 5 5 0) (effects (font (size 1 1))))
            )
            "#,
        );
        assert!(
            run_builtin_rules(&doc)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH010"
                    && item.message.contains("visible symbol fields overlap"))
        );
    }

    #[test]
    fn visible_symbol_fields_may_not_overlap_annotation_boxes() {
        let doc = document(
            r#"
            (symbol (lib_id "Device:R") (at 1 1 0)
              (property "Reference" "R1" (at 5 5 0) (effects (font (size 1 1))))
            )
            (text_box "Circuit note" (at 4 4) (size 4 3)
              (margins 0.5 0.5 0.5 0.5) (stroke (width 0.1))
              (effects (font (size 1 1))))
            "#,
        );
        assert!(
            run_builtin_rules(&doc)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH010" && item.message.contains("annotation text box"))
        );
    }

    #[test]
    fn separated_visible_symbol_fields_are_accepted() {
        let doc = document(
            r#"
            (symbol (lib_id "Device:R") (at 1 1 0)
              (property "Reference" "R1" (at 2 2 0) (effects (font (size 1 1))))
              (property "Value" "10k" (at 10 10 0) (effects (font (size 1 1))))
            )
            "#,
        );
        assert!(
            !run_builtin_rules(&doc)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH010")
        );
    }

    #[test]
    fn label_glyph_may_not_overlap_symbol_field() {
        let doc = document(
            r#"(symbol (lib_id "Device:R") (at 1 1 0) (property "Reference" "R1" (at 5 5 0) (effects (font (size 1 1))))) (label "NET" (at 5 5 0) (effects (font (size 1 1))))"#,
        );
        assert!(
            run_builtin_rules(&doc)
                .diagnostics
                .iter()
                .any(|item| { item.code == "SCH018" && item.message.contains("symbol field") })
        );
    }

    #[test]
    fn label_glyph_may_not_overlap_annotation_box() {
        let doc = document(
            r#"(text_box "Note" (at 4 4 0) (size 4 3) (margins 1.27 1.27 1.27 1.27) (stroke (width 0.1)) (effects (font (size 1 1)))) (label "NET" (at 5 5 0) (effects (font (size 1 1))))"#,
        );
        assert!(
            run_builtin_rules(&doc).diagnostics.iter().any(|item| {
                item.code == "SCH018" && item.message.contains("annotation text box")
            })
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
