use std::collections::{BTreeMap, BTreeSet};

use geo::algorithm::line_intersection::line_intersection;
use geo::{
    Contains, Coord, Distance, Euclidean, Intersects, Line, LineIntersection, LineString,
    Point as GeoPoint, Polygon, Rect as GeoRect,
    algorithm::sweep::{Cross, Intersections},
};
use kicad_clippy_core::{
    BuiltinRuleId, ByteSpan, Diagnostic, RelatedLocation, SchematicLocation, Severity,
};
use kicad_clippy_parser::SchematicDocument;
use kiutils_sexpr::{Atom, Node};
use kurbo::{
    Arc as CurveArc, PathEl, Point as CurvePoint, Shape, Vec2, flatten as flatten_curve_path,
};

use crate::{RuleContext, RulePass};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Segment {
    pub start: Point,
    pub end: Point,
    pub span: ByteSpan,
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
pub struct SchematicPinFact {
    /// Placed-symbol reference.
    pub reference: String,
    /// Embedded library-symbol pin number.
    pub number: String,
    /// Transformed pin position on the sheet, in millimetres.
    pub position_mm: (f64, f64),
}

#[derive(Clone, Debug)]
pub struct SchematicFieldFact {
    /// Placed-symbol reference.
    pub reference: String,
    /// `KiCad` property name, such as `Reference`, `Value`, or `Footprint`.
    pub name: String,
    /// Estimated visible glyph bounds as (left, top, right, bottom), in millimetres.
    pub bounds_mm: (f64, f64, f64, f64),
}

#[derive(Clone, Debug)]
pub struct SchematicPrimitiveFact {
    /// Placed-symbol reference that owns this primitive.
    pub reference: String,
    /// Transformed path points in sheet coordinates, or empty for a circle.
    pub points_mm: Vec<(f64, f64)>,
    /// Whether the path is closed.
    pub closed: bool,
    /// Optional transformed circle (centre x, centre y, radius).
    pub circle_mm: Option<(f64, f64, f64)>,
    /// Approximate stroke width in millimetres.
    pub stroke_width_mm: f64,
}

#[derive(Clone, Debug)]
pub struct SchematicLabelFact {
    /// Label form: `local`, `hierarchical_label`, or `global_label`.
    pub kind: String,
    /// Label name.
    pub name: String,
    /// Label anchor on the sheet, in millimetres.
    pub position_mm: (f64, f64),
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
    /// Resolved placed-symbol pin coordinates.
    pub pins: Vec<SchematicPinFact>,
    /// Estimated bounds for visible symbol property glyphs.
    pub visible_fields: Vec<SchematicFieldFact>,
    /// Transformed outlines from embedded symbol primitives.
    pub symbol_primitives: Vec<SchematicPrimitiveFact>,
    /// Label anchors used to ensure labels are not mistaken for physical wires.
    pub labels: Vec<SchematicLabelFact>,
    /// Explicit junction marker coordinates.
    pub junctions: Vec<(f64, f64)>,
}

impl SchematicFacts {
    #[must_use]
    pub fn from_document(document: &SchematicDocument) -> Self {
        let Some(root) = document.cst().nodes.first() else {
            return Self::default();
        };
        let source = document.source().text();
        let segments = collect_segments(root, source);
        let junctions = collect_points(root, source, "junction");
        Self::from_document_with_geometry(document, &segments, &junctions)
    }

    pub(crate) fn from_document_with_geometry(
        document: &SchematicDocument,
        segments: &[Segment],
        junctions: &[Point],
    ) -> Self {
        let Some(root) = document.cst().nodes.first() else {
            return Self::default();
        };
        let source = document.source().text();
        Self {
            components: collect_component_facts(root, source),
            text_boxes: collect_text_box_facts(root, source),
            groups: collect_group_facts(root, source),
            frames: collect_frame_facts(root, source),
            wires: segments
                .iter()
                .map(|segment| SchematicWireFact {
                    start_mm: (segment.start.x, segment.start.y),
                    end_mm: (segment.end.x, segment.end.y),
                })
                .collect(),
            pins: collect_pin_facts(root, source),
            visible_fields: collect_symbol_field_facts(root, source),
            symbol_primitives: collect_symbol_primitive_facts(root, source),
            labels: collect_label_facts(root, source),
            junctions: junctions.iter().map(|point| (point.x, point.y)).collect(),
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

fn collect_label_facts(root: &Node, source: &str) -> Vec<SchematicLabelFact> {
    collect_labels(root, source)
        .into_iter()
        .map(|(kind, name, point, _span)| SchematicLabelFact {
            kind,
            name,
            position_mm: (point.x, point.y),
        })
        .collect()
}

fn collect_pin_facts(root: &Node, source: &str) -> Vec<SchematicPinFact> {
    let Some(library_symbols) = child_list(root, "lib_symbols") else {
        return Vec::new();
    };
    let mut pins = Vec::new();
    for instance in children(root, "symbol") {
        let Some(lib_id) = child_list(instance, "lib_id")
            .and_then(list_items)
            .and_then(|items| items.get(1))
            .and_then(|item| atom_value(item, source))
        else {
            continue;
        };
        let Some(transform) = read_symbol_transform(instance, source) else {
            continue;
        };
        let Some(library_symbol) = children(library_symbols, "symbol").find(|definition| {
            list_items(definition)
                .and_then(|items| items.get(1))
                .and_then(|item| atom_value(item, source))
                .is_some_and(|name| name == lib_id)
        }) else {
            continue;
        };
        for pin in embedded_symbol_pins(library_symbol, transform.unit, transform.convert, source) {
            let Some(number) = child_list(pin, "number")
                .and_then(list_items)
                .and_then(|items| items.get(1))
                .and_then(|item| atom_value(item, source))
            else {
                continue;
            };
            let Some(at) = child_list(pin, "at").and_then(list_items) else {
                continue;
            };
            let (Some(pin_x), Some(pin_y)) = (
                at.get(1).and_then(|item| number_value(item, source)),
                at.get(2).and_then(|item| number_value(item, source)),
            ) else {
                continue;
            };
            let Some((x_offset, y_offset)) = transform_symbol_point(
                pin_x,
                pin_y,
                transform.rotation,
                transform.mirror.as_deref(),
            ) else {
                continue;
            };
            pins.push(SchematicPinFact {
                reference: transform.reference.clone(),
                number,
                position_mm: (transform.origin.x + x_offset, transform.origin.y + y_offset),
            });
        }
    }
    pins
}

fn collect_symbol_field_facts(root: &Node, source: &str) -> Vec<SchematicFieldFact> {
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
            let Some(name) = items.get(1).and_then(|item| atom_value(item, source)) else {
                continue;
            };
            let Some(value) = items.get(2).and_then(|item| atom_value(item, source)) else {
                continue;
            };
            if value.trim().is_empty() {
                continue;
            }
            let Some(bounds) = text_visual_bounds(&value, property, source) else {
                continue;
            };
            fields.push(SchematicFieldFact {
                reference: reference.clone(),
                name,
                bounds_mm: (
                    bounds.x,
                    bounds.y,
                    bounds.x + bounds.width,
                    bounds.y + bounds.height,
                ),
            });
        }
    }
    fields
}

fn collect_symbol_primitive_facts(root: &Node, source: &str) -> Vec<SchematicPrimitiveFact> {
    let Some(library_symbols) = child_list(root, "lib_symbols") else {
        return Vec::new();
    };
    let definitions: Vec<_> = children(library_symbols, "symbol").collect();
    children(root, "symbol")
        .flat_map(|instance| collect_instance_primitives(instance, &definitions, source))
        .collect()
}

fn collect_instance_primitives(
    instance: &Node,
    definitions: &[&Node],
    source: &str,
) -> Vec<SchematicPrimitiveFact> {
    let Some(transform) = read_symbol_transform(instance, source) else {
        return Vec::new();
    };
    let Some(lib_id) = child_list(instance, "lib_id")
        .and_then(list_items)
        .and_then(|items| items.get(1))
        .and_then(|item| atom_value(item, source))
    else {
        return Vec::new();
    };
    let Some(definition) = definitions.iter().copied().find(|definition| {
        list_items(definition)
            .and_then(|items| items.get(1))
            .and_then(|item| atom_value(item, source))
            .is_some_and(|name| name == lib_id)
    }) else {
        return Vec::new();
    };
    selected_symbol_units(definition, transform.unit, transform.convert, source)
        .into_iter()
        .flat_map(|unit| collect_primitives_from_unit(unit, &transform, source))
        .collect()
}

fn selected_symbol_units<'a>(
    definition: &'a Node,
    unit: u32,
    convert: u32,
    source: &str,
) -> Vec<&'a Node> {
    let sub_symbols: Vec<_> = children(definition, "symbol").collect();
    if sub_symbols.is_empty() {
        return vec![definition];
    }
    sub_symbols
        .into_iter()
        .filter(|sub_symbol| {
            list_items(sub_symbol)
                .and_then(|items| items.get(1))
                .and_then(|item| atom_value(item, source))
                .and_then(|name| sub_symbol_unit_convert(&name))
                .is_some_and(|(sub_unit, sub_convert)| {
                    (sub_unit == 0 || sub_unit == unit) && sub_convert == convert
                })
        })
        .collect()
}

fn collect_primitives_from_unit(
    unit: &Node,
    transform: &SymbolTransform,
    source: &str,
) -> Vec<SchematicPrimitiveFact> {
    let mut primitives = Vec::new();
    for kind in [
        "rectangle",
        "polyline",
        "polygon",
        "circle",
        "arc",
        "bezier",
    ] {
        for node in children(unit, kind) {
            if let Some(primitive) = parse_symbol_primitive(node, transform, source) {
                primitives.push(primitive);
            }
        }
    }
    primitives
}

fn parse_symbol_primitive(
    node: &Node,
    transform: &SymbolTransform,
    source: &str,
) -> Option<SchematicPrimitiveFact> {
    let stroke_width_mm = symbol_primitive_stroke_width(node, source);
    if head(node) == Some("circle") {
        let center = child_list(node, "center").and_then(list_items)?;
        let radius = child_list(node, "radius")
            .and_then(list_items)
            .and_then(|items| items.get(1))
            .and_then(|item| number(item, source))?
            .abs();
        let local_x = number(center.get(1)?, source)?;
        let local_y = number(center.get(2)?, source)?;
        let (world_x, world_y) = transform_symbol_point(
            local_x,
            local_y,
            transform.rotation,
            transform.mirror.as_deref(),
        )?;
        return Some(SchematicPrimitiveFact {
            reference: transform.reference.clone(),
            points_mm: Vec::new(),
            closed: false,
            circle_mm: Some((
                transform.origin.x + world_x,
                transform.origin.y + world_y,
                radius,
            )),
            stroke_width_mm,
        });
    }

    let kind = head(node)?;
    let local_points = match kind {
        "rectangle" => rectangle_primitive_points(node, source)?,
        "polyline" | "polygon" => polyline_primitive_points(node, source)?,
        "arc" => arc_primitive_points(node, source)?,
        "bezier" => bezier_primitive_points(node, source)?,
        _ => return None,
    };
    let mut points_mm = transform_primitive_points(&local_points, transform)?;
    let last_point = *points_mm.last()?;
    let repeated_endpoint = points_mm.len() > 2
        && (points_mm[0].0 - last_point.0).abs() <= 0.001
        && (points_mm[0].1 - last_point.1).abs() <= 0.001;
    let closed = kind == "rectangle" || kind == "polygon" || repeated_endpoint;
    if closed && repeated_endpoint {
        points_mm.pop();
    }
    (points_mm.len() >= 2).then(|| SchematicPrimitiveFact {
        reference: transform.reference.clone(),
        points_mm,
        closed,
        circle_mm: None,
        stroke_width_mm,
    })
}

fn symbol_primitive_stroke_width(node: &Node, source: &str) -> f64 {
    child_list(node, "stroke")
        .and_then(|stroke| child_list(stroke, "width"))
        .and_then(list_items)
        .and_then(|items| items.get(1))
        .and_then(|item| number(item, source))
        .filter(|width| width.is_finite() && width.abs() > 0.001)
        .map_or(0.254, f64::abs)
}

fn rectangle_primitive_points(node: &Node, source: &str) -> Option<Vec<(f64, f64)>> {
    let start = child_list(node, "start").and_then(list_items)?;
    let end = child_list(node, "end").and_then(list_items)?;
    let x1 = number(start.get(1)?, source)?;
    let y1 = number(start.get(2)?, source)?;
    let x2 = number(end.get(1)?, source)?;
    let y2 = number(end.get(2)?, source)?;
    Some(vec![(x1, y1), (x2, y1), (x2, y2), (x1, y2)])
}

fn polyline_primitive_points(node: &Node, source: &str) -> Option<Vec<(f64, f64)>> {
    let points = child_list(node, "pts")?;
    let points: Vec<_> = children(points, "xy")
        .filter_map(|point| {
            let items = list_items(point)?;
            Some((
                number(items.get(1)?, source)?,
                number(items.get(2)?, source)?,
            ))
        })
        .collect();
    (points.len() >= 2).then_some(points)
}

fn arc_primitive_points(node: &Node, source: &str) -> Option<Vec<(f64, f64)>> {
    let point = |name| {
        let items = child_list(node, name).and_then(list_items)?;
        Some((
            number(items.get(1)?, source)?,
            number(items.get(2)?, source)?,
        ))
    };
    Some(sample_arc_through_three_points(
        point("start")?,
        point("mid")?,
        point("end")?,
    ))
}

const CURVE_FLATTEN_TOLERANCE_MM: f64 = 0.01;

fn flattened_curve_points(
    path: impl IntoIterator<Item = PathEl>,
    start: (f64, f64),
    end: (f64, f64),
) -> Vec<(f64, f64)> {
    let mut points = Vec::new();
    flatten_curve_path(path, CURVE_FLATTEN_TOLERANCE_MM, |element| {
        if let PathEl::MoveTo(point) | PathEl::LineTo(point) = element {
            points.push((point.x, point.y));
        }
    });

    if points.is_empty() {
        return vec![start, end];
    }
    points[0] = start;
    if let Some(last) = points.last_mut() {
        *last = end;
    }
    points
}

fn bezier_primitive_points(node: &Node, source: &str) -> Option<Vec<(f64, f64)>> {
    let points_node = child_list(node, "pts")?;
    let coordinates: Option<Vec<_>> = children(points_node, "xy")
        .map(|point| {
            let items = list_items(point)?;
            Some((
                number(items.get(1)?, source)?,
                number(items.get(2)?, source)?,
            ))
        })
        .collect();
    let coordinates = coordinates?;
    let [start, control_one, control_two, end] = coordinates.as_slice() else {
        return None;
    };
    let path = [
        PathEl::MoveTo(CurvePoint::new(start.0, start.1)),
        PathEl::CurveTo(
            CurvePoint::new(control_one.0, control_one.1),
            CurvePoint::new(control_two.0, control_two.1),
            CurvePoint::new(end.0, end.1),
        ),
    ];
    Some(flattened_curve_points(path, *start, *end))
}

fn sample_arc_through_three_points(
    start: (f64, f64),
    mid: (f64, f64),
    end: (f64, f64),
) -> Vec<(f64, f64)> {
    let determinant = 2.0
        * end.0.mul_add(
            start.1 - mid.1,
            mid.0.mul_add(end.1 - start.1, start.0 * (mid.1 - end.1)),
        );
    if determinant.abs() <= 1.0e-9 {
        return vec![start, mid, end];
    }

    let start_squared = start.0.mul_add(start.0, start.1 * start.1);
    let mid_squared = mid.0.mul_add(mid.0, mid.1 * mid.1);
    let end_squared = end.0.mul_add(end.0, end.1 * end.1);
    let center_x = end_squared.mul_add(
        start.1 - mid.1,
        mid_squared.mul_add(end.1 - start.1, start_squared * (mid.1 - end.1)),
    ) / determinant;
    let center_y = end_squared.mul_add(
        mid.0 - start.0,
        mid_squared.mul_add(start.0 - end.0, start_squared * (end.0 - mid.0)),
    ) / determinant;
    let radius = (start.0 - center_x).hypot(start.1 - center_y);
    if !radius.is_finite() || radius <= 1.0e-9 {
        return vec![start, mid, end];
    }

    let start_angle = (start.1 - center_y).atan2(start.0 - center_x);
    let mid_angle = (mid.1 - center_y).atan2(mid.0 - center_x);
    let end_angle = (end.1 - center_y).atan2(end.0 - center_x);
    let counter_clockwise_sweep = (end_angle - start_angle).rem_euclid(std::f64::consts::TAU);
    let mid_sweep = (mid_angle - start_angle).rem_euclid(std::f64::consts::TAU);
    let sweep = if mid_sweep <= counter_clockwise_sweep + 1.0e-9 {
        counter_clockwise_sweep
    } else {
        counter_clockwise_sweep - std::f64::consts::TAU
    };
    if sweep.abs() <= 1.0e-9 {
        return vec![start, mid, end];
    }

    let arc = CurveArc::new(
        CurvePoint::new(center_x, center_y),
        Vec2::new(radius, radius),
        start_angle,
        sweep,
        0.0,
    );
    flattened_curve_points(arc.path_elements(CURVE_FLATTEN_TOLERANCE_MM), start, end)
}

fn transform_primitive_points(
    points: &[(f64, f64)],
    transform: &SymbolTransform,
) -> Option<Vec<(f64, f64)>> {
    points
        .iter()
        .map(|(x, y)| {
            let (offset_x, offset_y) =
                transform_symbol_point(*x, *y, transform.rotation, transform.mirror.as_deref())?;
            Some((transform.origin.x + offset_x, transform.origin.y + offset_y))
        })
        .collect()
}

fn number_value(node: &Node, source: &str) -> Option<f64> {
    number(node, source)
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

/// Built-in geometry and pin-placement analysis pass.
pub struct BuiltinGeometryPass;

impl RulePass for BuiltinGeometryPass {
    fn name(&self) -> &'static str {
        "01-kicad-clippy.geometry"
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        run_geometry_checks(context)
    }
}

/// Built-in physical-wire topology and label analysis pass.
pub struct BuiltinLabelPass;

impl RulePass for BuiltinLabelPass {
    fn name(&self) -> &'static str {
        "02-kicad-clippy.labels"
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        run_label_checks(context)
    }
}

/// Built-in annotation and visible-field layout analysis pass.
pub struct BuiltinAnnotationPass;

impl RulePass for BuiltinAnnotationPass {
    fn name(&self) -> &'static str {
        "03-kicad-clippy.annotations"
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        run_annotation_checks(context)
    }
}

/// Run reusable rules that do not require a project profile.
#[must_use]
pub fn run_builtin_rules(document: &SchematicDocument) -> RuleRun {
    crate::RuleEngine::with_builtins().run(document)
}

#[must_use]
pub fn run_geometry_checks(context: &RuleContext<'_>) -> Vec<Diagnostic> {
    let Some(root) = context.document().cst().nodes.first() else {
        return Vec::new();
    };
    let source = context.document().source().text();
    let mut diagnostics = check_wire_geometry(root, context.segments, context.junctions);
    diagnostics.extend(check_pin_orientation(root, source, context.segments));
    diagnostics.extend(check_no_connect_markers(root, source));
    diagnostics.extend(check_wire_obstacles(
        root,
        source,
        context.segments,
        context.facts(),
    ));
    diagnostics
}

#[must_use]
pub fn run_label_checks(context: &RuleContext<'_>) -> Vec<Diagnostic> {
    let Some(root) = context.document().cst().nodes.first() else {
        return Vec::new();
    };
    let source = context.document().source().text();
    let mut diagnostics = check_label_topology(root, source, context.segments);
    diagnostics.extend(check_label_wire_clearance(root, source, context.segments));
    diagnostics.extend(check_label_text_collisions(root, source));
    diagnostics.extend(check_label_kind_consistency(root, source));
    diagnostics
}

#[must_use]
pub fn run_annotation_checks(context: &RuleContext<'_>) -> Vec<Diagnostic> {
    let Some(root) = context.document().cst().nodes.first() else {
        return Vec::new();
    };
    let source = context.document().source().text();
    let mut diagnostics = check_text_box_layout(root, source, context.segments);
    diagnostics.extend(check_block_annotation_structure(root, source));
    diagnostics.extend(check_symbol_field_clearance(
        root,
        source,
        context.segments,
        context.facts(),
    ));
    diagnostics
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

fn check_wire_obstacles(
    root: &Node,
    source: &str,
    segments: &[Segment],
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    let fields = collect_visible_field_bounds(root, source);
    let text_boxes: Vec<_> = children(root, "text_box")
        .enumerate()
        .filter_map(|(index, node)| text_box_bounds(node, source).map(|bounds| (index, bounds)))
        .collect();
    let mut diagnostics = Vec::new();

    for (wire_index, segment) in segments.iter().enumerate() {
        let mut reported_symbols = BTreeSet::new();
        for primitive in &facts.symbol_primitives {
            if !reported_symbols.contains(&primitive.reference)
                && wire_crosses_symbol_primitive(*segment, primitive, &facts.pins)
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
        for pin in &facts.pins {
            let point = Point {
                x: pin.position_mm.0,
                y: pin.position_mm.1,
            };
            if point_on_segment_any(point, segment.start, segment.end)
                && !is_endpoint(point, *segment)
            {
                diagnostics.push(at_segment(
                    BuiltinRuleId::Sch013,
                    &format!(
                        "wire {wire_index} passes through intermediate pin {}.{}",
                        pin.reference, pin.number
                    ),
                    *segment,
                ));
            }
        }
        for (field_name, bounds, _span) in &fields {
            if strict_line_hits_bounds(*segment, *bounds) {
                diagnostics.push(at_segment(
                    BuiltinRuleId::Sch013,
                    &format!("wire {wire_index} passes through symbol field {field_name}"),
                    *segment,
                ));
            }
        }
        for (box_index, bounds) in &text_boxes {
            if strict_line_hits_bounds(*segment, *bounds) {
                diagnostics.push(at_segment(
                    BuiltinRuleId::Sch013,
                    &format!("wire {wire_index} penetrates annotation text box {box_index}"),
                    *segment,
                ));
            }
        }
    }
    diagnostics
}

fn collect_visible_field_bounds(root: &Node, source: &str) -> Vec<(String, Bounds, ByteSpan)> {
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
            let Some(name) = items.get(1).and_then(|item| atom_value(item, source)) else {
                continue;
            };
            if !matches!(name.as_str(), "Reference" | "Value") {
                continue;
            }
            let Some(value) = items.get(2).and_then(|item| atom_value(item, source)) else {
                continue;
            };
            if let Some(bounds) = text_visual_bounds(&value, property, source) {
                fields.push((format!("{reference} {name}"), bounds, span_of(property)));
            }
        }
    }
    fields
}

fn wire_crosses_symbol_primitive(
    wire: Segment,
    primitive: &SchematicPrimitiveFact,
    pins: &[SchematicPinFact],
) -> bool {
    if let Some((center_x, center_y, radius)) = primitive.circle_mm {
        let center = Point {
            x: center_x,
            y: center_y,
        };
        let inner_radius = radius - primitive.stroke_width_mm / 2.0;
        return inner_radius > 0.0
            && point_segment_distance(center, wire.start, wire.end) < inner_radius - 0.001;
    }

    let to_point = |(x, y): (f64, f64)| Point { x, y };
    let mut edges: Vec<_> = primitive
        .points_mm
        .windows(2)
        .map(|pair| (to_point(pair[0]), to_point(pair[1])))
        .collect();
    if primitive.closed && primitive.points_mm.len() > 2 {
        if let (Some(last), Some(first)) = (
            primitive.points_mm.last().copied(),
            primitive.points_mm.first().copied(),
        ) {
            edges.push((to_point(last), to_point(first)));
        }
    }
    for (edge_start, edge_end) in edges {
        for point in segment_intersection_points(wire, edge_start, edge_end) {
            if !wire_intersection_is_allowed_pin(point, wire, &primitive.reference, pins) {
                return true;
            }
        }
    }
    if primitive.closed {
        let midpoint = Point {
            x: f64::midpoint(wire.start.x, wire.end.x),
            y: f64::midpoint(wire.start.y, wire.end.y),
        };
        if point_in_polygon(midpoint, &primitive.points_mm)
            || (!wire_endpoint_is_pin(wire.start, &primitive.reference, pins)
                && point_in_polygon(wire.start, &primitive.points_mm))
            || (!wire_endpoint_is_pin(wire.end, &primitive.reference, pins)
                && point_in_polygon(wire.end, &primitive.points_mm))
        {
            return true;
        }
    }
    false
}

fn wire_intersection_is_allowed_pin(
    point: Point,
    wire: Segment,
    reference: &str,
    pins: &[SchematicPinFact],
) -> bool {
    (distance(point, wire.start) <= 0.001 && wire_endpoint_is_pin(wire.start, reference, pins))
        || (distance(point, wire.end) <= 0.001 && wire_endpoint_is_pin(wire.end, reference, pins))
}

fn wire_endpoint_is_pin(endpoint: Point, reference: &str, pins: &[SchematicPinFact]) -> bool {
    pins.iter().any(|pin| {
        pin.reference == reference
            && distance(
                endpoint,
                Point {
                    x: pin.position_mm.0,
                    y: pin.position_mm.1,
                },
            ) <= 0.001
    })
}

fn geo_line(start: Point, end: Point) -> Line<f64> {
    Line::new(
        Coord {
            x: start.x,
            y: start.y,
        },
        Coord { x: end.x, y: end.y },
    )
}

/// Returns all point endpoints of a segment intersection, including both ends
/// of a collinear overlap. `GeoRust` owns the intersection classification and
/// construction; this adapter only converts to the schematic point type.
fn segment_intersection_points(
    first: Segment,
    second_start: Point,
    second_end: Point,
) -> Vec<Point> {
    let first_line = geo_line(first.start, first.end);
    let second_line = geo_line(second_start, second_end);

    match line_intersection(first_line, second_line) {
        Some(LineIntersection::SinglePoint { intersection, .. }) => {
            vec![Point {
                x: intersection.x,
                y: intersection.y,
            }]
        }
        Some(LineIntersection::Collinear { intersection }) => {
            let points = [
                Point {
                    x: intersection.start.x,
                    y: intersection.start.y,
                },
                Point {
                    x: intersection.end.x,
                    y: intersection.end.y,
                },
            ];
            if distance(points[0], points[1]) <= 0.001 {
                vec![points[0]]
            } else {
                points.into_iter().collect()
            }
        }
        None => Vec::new(),
    }
}

fn point_on_segment_any(point: Point, start: Point, end: Point) -> bool {
    Euclidean.distance(&GeoPoint::new(point.x, point.y), &geo_line(start, end)) <= 0.001
}

fn point_segment_distance(point: Point, start: Point, end: Point) -> f64 {
    Euclidean.distance(&GeoPoint::new(point.x, point.y), &geo_line(start, end))
}

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
    geometry.contains(&query) || Euclidean.distance(&query, geometry.exterior()) <= 0.001
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
                at_point(BuiltinRuleId::Sch015,
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
        BuiltinRuleId::Sch005,
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

#[derive(Clone, Copy)]
struct IndexedSegment {
    index: usize,
    segment: Segment,
}

impl Cross for IndexedSegment {
    type Scalar = f64;

    fn line(&self) -> Line<f64> {
        geo_line(self.segment.start, self.segment.end)
    }
}

fn check_wire_geometry(root: &Node, segments: &[Segment], junctions: &[Point]) -> Vec<Diagnostic> {
    let mut diagnostics = collect_invalid_wire_diagnostics(root);
    for segment in segments {
        if distance(segment.start, segment.end) < 0.001 {
            diagnostics.push(at_segment(
                BuiltinRuleId::Sch001,
                "wire has zero length",
                *segment,
            ));
        } else if (segment.start.x - segment.end.x).abs() > 0.001
            && (segment.start.y - segment.end.y).abs() > 0.001
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
                if distance(overlap, overlap_end) > 0.001 {
                    diagnostics.push(at_segment(
                        BuiltinRuleId::Sch001,
                        "collinear wire segments overlap",
                        segment,
                    ));
                }
            }
            LineIntersection::SinglePoint { intersection, .. } => {
                let point = Point {
                    x: intersection.x,
                    y: intersection.y,
                };
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
                        .any(|junction| distance(*junction, point) < 0.001)
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
                BuiltinRuleId::Sch003,
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
                BuiltinRuleId::Sch008,
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
        diagnostics.push(at_span(BuiltinRuleId::Sch008,
            format!(
                "text_box {index} is undersized ({:.2}x{:.2} mm); needs {required_width:.2}x{required_height:.2} mm",
                metrics.bounds.width, metrics.bounds.height
            ),
            metrics.span,
        ));
    }
    if metrics.stroke_width <= 0.0 && metrics.bounds.width + 0.001 < required_width + 10.16 {
        diagnostics.push(at_span(BuiltinRuleId::Sch008,
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
                BuiltinRuleId::Sch008,
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
                BuiltinRuleId::Sch008,
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
                BuiltinRuleId::Sch008,
                format!("text_box {index} has invalid margins"),
                span,
            )),
        );
    };
    if pads.len() != 4 || pads.iter().any(|pad| (pad - 1.27).abs() > 0.001) {
        return (
            None,
            Some(at_span(
                BuiltinRuleId::Sch008,
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
                BuiltinRuleId::Sch008,
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
                    BuiltinRuleId::Sch008,
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
                BuiltinRuleId::Sch008,
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
                at_span(BuiltinRuleId::Sch011,
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

fn at_span(code: BuiltinRuleId, message: impl Into<String>, span: ByteSpan) -> Diagnostic {
    let mut diagnostic = Diagnostic::for_builtin_rule(code, Severity::Error, message);
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

fn wire_components(
    segments: &[Segment],
    points: &[Point],
    junctions: &[Point],
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

fn has_unmarked_wire_endpoint(point: Point, segments: &[Segment], junctions: &[Point]) -> bool {
    let terminates_another_wire = segments.iter().any(|other| is_endpoint(point, *other));
    terminates_another_wire
        && !junctions
            .iter()
            .any(|junction| distance(*junction, point) < 0.001)
}

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
                    BuiltinRuleId::Sch018,
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
                    BuiltinRuleId::Sch018,
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
                    BuiltinRuleId::Sch018,
                    format!("{first_name} overlaps an annotation text box"),
                    *first_span,
                ));
            }
        }
    }
    diagnostics
}

fn check_symbol_field_clearance(
    root: &Node,
    source: &str,
    segments: &[Segment],
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    let fields = collect_visible_field_bounds(root, source);
    let label_bounds: Vec<_> = collect_label_nodes(root, source)
        .into_iter()
        .filter_map(|(kind, name, _point, _span, node)| {
            text_visual_bounds(&name, node, source).map(|bounds| (kind, name, bounds))
        })
        .collect();
    let text_boxes: Vec<_> = children(root, "text_box")
        .filter_map(|node| text_box_bounds(node, source))
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

fn point_in_bounds(point: Point, bounds: Bounds) -> bool {
    let expanded = Bounds {
        x: bounds.x - 0.001,
        y: bounds.y - 0.001,
        width: bounds.width + 0.002,
        height: bounds.height + 0.002,
    };
    geo_rect(expanded).contains(&GeoPoint::new(point.x, point.y))
}

fn point_bounds_distance(point: Point, bounds: Bounds) -> f64 {
    Euclidean.distance(&GeoPoint::new(point.x, point.y), &geo_rect(bounds))
}

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

fn schematic_path(primitive: &SchematicPrimitiveFact) -> LineString<f64> {
    LineString::new(
        primitive
            .points_mm
            .iter()
            .map(|(x, y)| Coord { x: *x, y: *y })
            .collect(),
    )
}

fn symbol_primitive_intersects_bounds(bounds: Bounds, primitive: &SchematicPrimitiveFact) -> bool {
    if let Some((center_x, center_y, radius)) = primitive.circle_mm {
        let center = Point {
            x: center_x,
            y: center_y,
        };
        return point_bounds_distance(center, bounds)
            <= radius + primitive.stroke_width_mm / 2.0 + 0.001;
    }

    let rectangle = geo_rect(bounds);
    schematic_polygon(primitive).map_or_else(
        || schematic_path(primitive).intersects(&rectangle),
        |polygon| polygon.intersects(&rectangle),
    )
}

fn symbol_primitive_contains_bounds(bounds: Bounds, primitive: &SchematicPrimitiveFact) -> bool {
    if let Some((center_x, center_y, radius)) = primitive.circle_mm {
        let inner_radius = radius - primitive.stroke_width_mm / 2.0;
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
        .all(|corner| distance(*corner, center) <= inner_radius + 0.001);
    }

    schematic_polygon(primitive).is_some_and(|polygon| polygon.contains(&geo_rect(bounds)))
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
                Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch019,
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

#[must_use]
pub fn collect_segments(root: &Node, source: &str) -> Vec<Segment> {
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

#[must_use]
pub fn collect_points(root: &Node, source: &str, name: &str) -> Vec<Point> {
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
    point_on_segment_any(point, segment.start, segment.end)
}

fn is_endpoint(point: Point, segment: Segment) -> bool {
    distance(point, segment.start) < 0.001 || distance(point, segment.end) < 0.001
}

fn distance(first: Point, second: Point) -> f64 {
    (first.x - second.x).hypot(first.y - second.y)
}

fn at_segment(code: BuiltinRuleId, message: &str, segment: Segment) -> Diagnostic {
    let mut diagnostic = Diagnostic::new(code, Severity::Error, message);
    diagnostic.span = Some(segment.span);
    diagnostic
}

fn at_point(code: BuiltinRuleId, message: String, point: Point, span: ByteSpan) -> Diagnostic {
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

    const TRIANGLE_SYMBOL: &str = r#"
        (lib_symbols
          (symbol "Test:Triangle"
            (symbol "Triangle_0_1"
              (polyline
                (pts (xy 0 0) (xy 4 0) (xy 0 4) (xy 0 0))
                (stroke (width 0.254))
                (fill (type none))
              )
            )
          )
        )
        (symbol (lib_id "Test:Triangle") (at 0 0 0)
          (property "Reference" "Q1" (at 10 10 0) (effects (font (size 1 1))))
        )
    "#;

    #[test]
    fn wire_obstacle_uses_actual_triangle_outline_not_its_bounding_box() {
        let outside = document(&format!(
            "{TRIANGLE_SYMBOL} (wire (pts (xy 3 -3) (xy 4 -4)))"
        ));
        assert!(
            !run_builtin_rules(&outside)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH013" && item.message.contains("symbol body Q1"))
        );

        let crossing = document(&format!(
            "{TRIANGLE_SYMBOL} (wire (pts (xy 1 -1) (xy 3 -1)))"
        ));
        assert!(
            run_builtin_rules(&crossing)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH013" && item.message.contains("symbol body Q1"))
        );
    }

    #[test]
    fn wire_obstacle_detects_wire_near_the_true_three_point_arc_path() {
        let doc = document(
            r#"
            (lib_symbols
              (symbol "Test:Arc"
                (symbol "Arc_1_1"
                  (arc (start -2 0) (mid 0 2) (end 2 0)
                    (stroke (width 0.254)) (fill (type none)))
                )
              )
            )
            (symbol (lib_id "Test:Arc") (at 10 10 0)
              (property "Reference" "U1" (at 10 10 0) (effects (font (size 1 1))))
            )
            (wire (pts (xy 11.414 8.4) (xy 11.414 8.8)))
            "#,
        );
        assert!(
            run_builtin_rules(&doc)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH013" && item.message.contains("symbol body U1"))
        );
    }

    #[test]
    fn arc_midpoint_selects_the_correct_sweep_direction() {
        let doc = document(
            r#"
            (lib_symbols
              (symbol "Test:Arc"
                (symbol "Arc_1_1"
                  (arc (start -2 0) (mid 0 -2) (end 2 0)
                    (stroke (width 0.254)) (fill (type none)))
                )
              )
            )
            (symbol (lib_id "Test:Arc") (at 10 10 0)
              (property "Reference" "U1" (at 10 10 0) (effects (font (size 1 1))))
            )
            (wire (pts (xy 11.414 8.4) (xy 11.414 8.8)))
            "#,
        );
        assert!(
            !run_builtin_rules(&doc)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH013" && item.message.contains("symbol body U1"))
        );
    }

    #[test]
    fn wire_obstacle_detects_actual_cubic_bezier_curve() {
        let doc = document(
            r#"
            (lib_symbols
              (symbol "Test:Bezier"
                (symbol "Bezier_1_1"
                  (bezier
                    (pts (xy -2 0) (xy -1 2) (xy 1 2) (xy 2 0))
                    (stroke (width 0.254))
                    (fill (type none)))
                )
              )
            )
            (symbol (lib_id "Test:Bezier") (at 10 10 0)
              (property "Reference" "U1" (at 10 10 0) (effects (font (size 1 1))))
            )
            (wire (pts (xy 11.094 8.75) (xy 11.094 9.0)))
            "#,
        );
        assert!(
            run_builtin_rules(&doc)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH013" && item.message.contains("symbol body U1"))
        );
    }

    #[test]
    fn bezier_control_polygon_is_not_treated_as_the_curve() {
        let doc = document(
            r#"
            (lib_symbols
              (symbol "Test:Bezier"
                (symbol "Bezier_1_1"
                  (bezier
                    (pts (xy -2 0) (xy -1 2) (xy 1 2) (xy 2 0))
                    (stroke (width 0.254))
                    (fill (type none)))
                )
              )
            )
            (symbol (lib_id "Test:Bezier") (at 10 10 0)
              (property "Reference" "U1" (at 10 10 0) (effects (font (size 1 1))))
            )
            (wire (pts (xy 8.9 7.9) (xy 9.1 7.9)))
            "#,
        );
        assert!(
            !run_builtin_rules(&doc)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH013" && item.message.contains("symbol body U1"))
        );
    }

    #[test]
    fn wire_obstacle_detects_circle_primitive_interiors() {
        let doc = document(
            r#"
            (lib_symbols
              (symbol "Test:Circle"
                (symbol "Circle_1_1"
                  (circle (center 0 0) (radius 2)
                    (stroke (width 0.254)) (fill (type none)))
                )
              )
            )
            (symbol (lib_id "Test:Circle") (at 10 10 0)
              (property "Reference" "U1" (at 10 10 0) (effects (font (size 1 1))))
            )
            (wire (pts (xy 9 10) (xy 11 10)))
            "#,
        );
        assert!(
            run_builtin_rules(&doc)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH013" && item.message.contains("symbol body U1"))
        );
    }

    #[test]
    fn wire_may_start_at_its_intended_symbol_pin_without_body_penetration() {
        let doc = document(
            r#"
            (lib_symbols
              (symbol "Test:PinBody"
                (symbol "PinBody_1_1"
                  (rectangle (start -1 -1) (end 1 1)
                    (stroke (width 0.254)) (fill (type none)))
                  (pin passive line (at 1 0 180) (length 1)
                    (name "1") (number "1"))
                )
              )
            )
            (symbol (lib_id "Test:PinBody") (at 10 10 0) (unit 1)
              (property "Reference" "U2" (at 10 10 0) (effects (font (size 1 1))))
            )
            (wire (pts (xy 11 10) (xy 13 10)))
            "#,
        );
        assert!(
            !run_builtin_rules(&doc)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH013" && item.message.contains("symbol body U2"))
        );
    }

    #[test]
    fn wire_obstacle_rejects_intermediate_pins_visible_fields_and_text_boxes() {
        let doc = document(
            r#"
            (lib_symbols
              (symbol "Test:TwoPins"
                (symbol "TwoPins_1_1"
                  (pin passive line (at 0 0 0) (length 1)
                    (name "1") (number "1"))
                  (pin passive line (at 1 0 0) (length 1)
                    (name "2") (number "2"))
                )
              )
            )
            (symbol (lib_id "Test:TwoPins") (at 10 10 0) (unit 1)
              (property "Reference" "U3" (at 5 5 0) (effects (font (size 1 1))))
              (property "Value" "TEST" (at 5 5 0) (effects (font (size 1 1))))
            )
            (text_box "Annotation" (at 4 4) (size 4 2)
              (margins 0.5 0.5 0.5 0.5) (stroke (width 0.1))
              (effects (font (size 1 1))))
            (wire (pts (xy 10 10) (xy 12 10)))
            (wire (pts (xy 4 4.5) (xy 6 4.5)))
            (wire (pts (xy 3 5) (xy 7 5)))
            "#,
        );
        let diagnostics = run_builtin_rules(&doc).diagnostics;
        assert!(
            diagnostics
                .iter()
                .any(|item| item.code == "SCH013" && item.message.contains("intermediate pin U3.2"))
        );
        assert!(diagnostics.iter().any(
            |item| item.code == "SCH013" && item.message.contains("symbol field U3 Reference")
        ));
        assert!(
            diagnostics
                .iter()
                .any(|item| item.code == "SCH013" && item.message.contains("annotation text box"))
        );
    }

    #[test]
    fn sweep_line_reports_crossing_wires_with_the_intersection_location() {
        let doc = document(r"(wire (pts (xy 0 0) (xy 4 0))) (wire (pts (xy 2 -2) (xy 2 2)))");
        let finding = run_builtin_rules(&doc)
            .diagnostics
            .into_iter()
            .find(|item| item.code == "SCH001" && item.message.contains("crossing"))
            .expect("crossing wire pair is reported");
        let location = finding.schematic_location.expect("crossing location");
        assert!((location.x_mm - 2.0).abs() < 0.001);
        assert!((location.y_mm - 0.0).abs() < 0.001);
    }

    #[test]
    fn sweep_line_requires_an_explicit_marker_for_t_junctions() {
        let missing = document(r"(wire (pts (xy 0 0) (xy 4 0))) (wire (pts (xy 2 0) (xy 2 2)))");
        assert!(
            run_builtin_rules(&missing)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH001" && item.message.contains("T-junction"))
        );

        let marked = document(
            r#"(wire (pts (xy 0 0) (xy 4 0))) (wire (pts (xy 2 0) (xy 2 2))) (junction (at 2 0) (diameter 0.6) (color 0 0 0 0) (uuid "junction"))"#,
        );
        assert!(
            !run_builtin_rules(&marked)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH001" && item.message.contains("T-junction"))
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
    fn visible_reference_field_may_not_be_embedded_in_symbol_body() {
        let doc = document(
            r#"
            (lib_symbols
              (symbol "Test:Body"
                (symbol "Body_1_1"
                  (rectangle (start -2 -2) (end 2 2)
                    (stroke (width 0.254)) (fill (type none)))
                )
              )
            )
            (symbol (lib_id "Test:Body") (at 10 10 0) (unit 1)
              (property "Reference" "U1" (at 10 10 0) (effects (font (size 1 1))))
            )
            "#,
        );
        assert!(
            run_builtin_rules(&doc)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH010" && item.message.contains("own symbol body"))
        );
    }

    #[test]
    fn value_field_fully_inside_closed_symbol_body_is_allowed() {
        let doc = document(
            r#"
            (lib_symbols
              (symbol "Test:Body"
                (symbol "Body_1_1"
                  (rectangle (start -3 -2) (end 3 2)
                    (stroke (width 0.254)) (fill (type none)))
                )
              )
            )
            (symbol (lib_id "Test:Body") (at 10 10 0) (unit 1)
              (property "Reference" "U1" (at 10 7 0) (effects (font (size 1 1))))
              (property "Value" "PASSIVE" (at 10 10 0) (effects (font (size 1 1))))
            )
            "#,
        );
        assert!(
            !run_builtin_rules(&doc)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH010" && item.message.contains("Value"))
        );
    }

    #[test]
    fn visible_symbol_field_may_not_overlap_wire_segment_or_pin() {
        let doc = document(
            r#"
            (lib_symbols
              (symbol "Test:Pin"
                (symbol "Pin_1_1"
                  (pin passive line (at 0 0 0) (length 1)
                    (name "1") (number "1"))
                )
              )
            )
            (symbol (lib_id "Test:Pin") (at 5 5 0) (unit 1)
              (property "Reference" "U1" (at 5 5 0) (effects (font (size 1 1))))
            )
            (wire (pts (xy 3 4.4) (xy 7 4.4)))
            "#,
        );
        let diagnostics = run_builtin_rules(&doc).diagnostics;
        assert!(
            diagnostics
                .iter()
                .any(|item| item.code == "SCH010"
                    && item.message.contains("overlaps a wire segment"))
        );
        assert!(
            diagnostics
                .iter()
                .any(|item| item.code == "SCH010" && item.message.contains("overlaps pin U1.1"))
        );
    }

    #[test]
    fn visible_symbol_field_may_not_overlap_label_glyph() {
        let doc = document(
            r#"
            (symbol (lib_id "Device:R") (at 1 1 0)
              (property "Reference" "R1" (at 5 5 0) (effects (font (size 1 1))))
            )
            (label "NET_NAME" (at 5 5 0) (effects (font (size 1 1))))
            "#,
        );
        assert!(
            run_builtin_rules(&doc)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH010" && item.message.contains("overlaps label"))
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
