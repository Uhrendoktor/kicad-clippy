use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
};

use geo::algorithm::line_intersection::line_intersection;
use geo::{
    BooleanOps, Contains, Coord, Distance, Euclidean, Intersects, Line, LineIntersection,
    LineString, MultiLineString, Point as GeoPoint, Polygon, Rect as GeoRect,
    algorithm::{
        line_measures::Length,
        sweep::{Cross, Intersections},
    },
};
use kicad_clippy_core::{
    BuiltinRuleId, ByteSpan, Diagnostic, RelatedLocation, SchematicLocation, Severity,
};
use kicad_clippy_parser::SchematicDocument;
use kiutils_sexpr::{Atom, Node};
use kurbo::{
    Affine, Arc as CurveArc, PathEl, Point as CurvePoint, Rect as CurveRect, Shape, Triangle, Vec2,
    flatten as flatten_curve_path,
};
use petgraph::unionfind::UnionFind;
use vector_text_core::{Point as VectorPoint, Renderer};
use vector_text_newstroke::NewstrokeRenderer;

use crate::{RuleContext, RulePass};

pub type Point = Coord<f64>;

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
    /// Estimated visible glyph bounds in sheet coordinates, when resolvable.
    pub bounds_mm: Option<(f64, f64, f64, f64)>,
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
        let segments = collect_segments(root);
        let junctions = collect_points(root, "junction");
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
        Self {
            components: collect_component_facts(root),
            text_boxes: collect_text_box_facts(root),
            groups: collect_group_facts(root),
            frames: collect_frame_facts(root),
            wires: segments
                .iter()
                .map(|segment| SchematicWireFact {
                    start_mm: (segment.start.x, segment.start.y),
                    end_mm: (segment.end.x, segment.end.y),
                })
                .collect(),
            pins: collect_pin_facts(root),
            visible_fields: collect_symbol_field_facts(root),
            symbol_primitives: collect_symbol_primitive_facts(root),
            labels: collect_label_facts(root),
            junctions: junctions.iter().map(|point| (point.x, point.y)).collect(),
        }
    }
}

fn collect_component_facts(root: &Node) -> Vec<ComponentFact> {
    children(root, "symbol")
        .filter_map(|node| {
            let reference = children(node, "property").find_map(|property| {
                let items = list_items(property)?;
                if atom_value(items.get(1)?)? == "Reference" {
                    atom_value(items.get(2)?)
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
                .and_then(atom_value);
            let position_mm = child_list(node, "at")
                .and_then(list_items)
                .and_then(|at| Some((number(at.get(1)?)?, number(at.get(2)?)?)));
            Some(ComponentFact {
                reference: reference.to_owned(),
                uuid: uuid.map(str::to_owned),
                position_mm,
            })
        })
        .collect()
}

fn collect_text_box_facts(root: &Node) -> Vec<TextBoxFact> {
    children(root, "text_box")
        .filter_map(|node| {
            let items = list_items(node)?;
            let text = atom_value(items.get(1)?)?;
            let uuid = child_list(node, "uuid")
                .and_then(list_items)
                .and_then(|items| items.get(1))
                .and_then(atom_value)?;
            let bounds_mm = text_box_bounds(node).map(|bounds| {
                (
                    bounds.x,
                    bounds.y,
                    bounds.x + bounds.width,
                    bounds.y + bounds.height,
                )
            });
            Some(TextBoxFact {
                uuid: uuid.to_owned(),
                text: text.to_owned(),
                bounds_mm,
            })
        })
        .collect()
}

fn collect_group_facts(root: &Node) -> Vec<SchematicGroupFact> {
    children(root, "group")
        .filter_map(|node| {
            let items = list_items(node)?;
            let name = atom_value(items.get(1)?)?;
            let uuid = child_list(node, "id")
                .and_then(list_items)
                .and_then(|items| items.get(1))
                .and_then(atom_value)?;
            let member_uuids = child_list(node, "members")
                .and_then(list_items)
                .into_iter()
                .flatten()
                .skip(1)
                .filter_map(atom_value)
                .map(str::to_owned)
                .collect();
            Some(SchematicGroupFact {
                name: name.to_owned(),
                uuid: uuid.to_owned(),
                member_uuids,
            })
        })
        .collect()
}

fn collect_frame_facts(root: &Node) -> Vec<SchematicFrameFact> {
    children(root, "rectangle")
        .filter_map(|node| {
            let uuid = child_list(node, "uuid")
                .and_then(list_items)
                .and_then(|items| items.get(1))
                .and_then(atom_value)?;
            let start = child_list(node, "start")
                .and_then(list_items)
                .and_then(|items| Some((number(items.get(1)?)?, number(items.get(2)?)?)))?;
            let end = child_list(node, "end")
                .and_then(list_items)
                .and_then(|items| Some((number(items.get(1)?)?, number(items.get(2)?)?)))?;
            Some(SchematicFrameFact {
                uuid: uuid.to_owned(),
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

fn collect_label_facts(root: &Node) -> Vec<SchematicLabelFact> {
    collect_label_nodes(root)
        .into_iter()
        .map(|(kind, name, point, _span, node)| SchematicLabelFact {
            kind,
            bounds_mm: text_visual_bounds(&name, node).map(|bounds| {
                (
                    bounds.x,
                    bounds.y,
                    bounds.x + bounds.width,
                    bounds.y + bounds.height,
                )
            }),
            name,
            position_mm: (point.x, point.y),
        })
        .collect()
}

fn collect_pin_facts(root: &Node) -> Vec<SchematicPinFact> {
    let Some(library_symbols) = child_list(root, "lib_symbols") else {
        return Vec::new();
    };
    let mut pins = Vec::new();
    for instance in children(root, "symbol") {
        let Some(lib_id) = child_list(instance, "lib_id")
            .and_then(list_items)
            .and_then(|items| items.get(1))
            .and_then(atom_value)
        else {
            continue;
        };
        let Some(transform) = read_symbol_transform(instance) else {
            continue;
        };
        let Some(library_symbol) = children(library_symbols, "symbol").find(|definition| {
            list_items(definition)
                .and_then(|items| items.get(1))
                .and_then(atom_value)
                .is_some_and(|name| name == lib_id)
        }) else {
            continue;
        };
        for pin in embedded_symbol_pins(library_symbol, transform.unit, transform.convert) {
            let Some(pin_number) = child_list(pin, "number")
                .and_then(list_items)
                .and_then(|items| items.get(1))
                .and_then(atom_value)
            else {
                continue;
            };
            let Some(at) = child_list(pin, "at").and_then(list_items) else {
                continue;
            };
            let (Some(pin_x), Some(pin_y)) =
                (at.get(1).and_then(number), at.get(2).and_then(number))
            else {
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
                number: pin_number.to_owned(),
                position_mm: (transform.origin.x + x_offset, transform.origin.y + y_offset),
            });
        }
    }
    pins
}

fn collect_symbol_field_facts(root: &Node) -> Vec<SchematicFieldFact> {
    let mut fields = Vec::new();
    for symbol in children(root, "symbol") {
        let reference = children(symbol, "property").find_map(|property| {
            let items = list_items(property)?;
            if atom_value(items.get(1)?)? == "Reference" {
                atom_value(items.get(2)?)
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
            let Some(name) = items.get(1).and_then(atom_value) else {
                continue;
            };
            let Some(value) = items.get(2).and_then(atom_value) else {
                continue;
            };
            if value.trim().is_empty() {
                continue;
            }
            let Some(bounds) = text_visual_bounds(value, property) else {
                continue;
            };
            fields.push(SchematicFieldFact {
                reference: reference.to_owned(),
                name: name.to_owned(),
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

fn collect_symbol_primitive_facts(root: &Node) -> Vec<SchematicPrimitiveFact> {
    let Some(library_symbols) = child_list(root, "lib_symbols") else {
        return Vec::new();
    };
    let definitions: Vec<_> = children(library_symbols, "symbol").collect();
    children(root, "symbol")
        .flat_map(|instance| collect_instance_primitives(instance, &definitions))
        .collect()
}

fn collect_instance_primitives(
    instance: &Node,
    definitions: &[&Node],
) -> Vec<SchematicPrimitiveFact> {
    let Some(transform) = read_symbol_transform(instance) else {
        return Vec::new();
    };
    let Some(lib_id) = child_list(instance, "lib_id")
        .and_then(list_items)
        .and_then(|items| items.get(1))
        .and_then(atom_value)
    else {
        return Vec::new();
    };
    let Some(definition) = definitions.iter().copied().find(|definition| {
        list_items(definition)
            .and_then(|items| items.get(1))
            .and_then(atom_value)
            .is_some_and(|name| name == lib_id)
    }) else {
        return Vec::new();
    };
    selected_symbol_units(definition, transform.unit, transform.convert)
        .into_iter()
        .flat_map(|unit| collect_primitives_from_unit(unit, &transform))
        .collect()
}

fn selected_symbol_units(definition: &Node, unit: u32, convert: u32) -> Vec<&Node> {
    let sub_symbols: Vec<_> = children(definition, "symbol").collect();
    if sub_symbols.is_empty() {
        return vec![definition];
    }
    sub_symbols
        .into_iter()
        .filter(|sub_symbol| {
            list_items(sub_symbol)
                .and_then(|items| items.get(1))
                .and_then(atom_value)
                .and_then(sub_symbol_unit_convert)
                .is_some_and(|(sub_unit, sub_convert)| {
                    (sub_unit == 0 || sub_unit == unit) && sub_convert == convert
                })
        })
        .collect()
}

fn collect_primitives_from_unit(
    unit: &Node,
    transform: &SymbolTransform,
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
            if let Some(primitive) = parse_symbol_primitive(node, transform) {
                primitives.push(primitive);
            }
        }
    }
    primitives
}

fn parse_symbol_primitive(
    node: &Node,
    transform: &SymbolTransform,
) -> Option<SchematicPrimitiveFact> {
    let stroke_width_mm = symbol_primitive_stroke_width(node);
    if head(node) == Some("circle") {
        let center = child_list(node, "center").and_then(list_items)?;
        let radius = child_list(node, "radius")
            .and_then(list_items)
            .and_then(|items| items.get(1))
            .and_then(number)?
            .abs();
        let local_x = number(center.get(1)?)?;
        let local_y = number(center.get(2)?)?;
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
        "rectangle" => rectangle_primitive_points(node)?,
        "polyline" | "polygon" => polyline_primitive_points(node)?,
        "arc" => arc_primitive_points(node)?,
        "bezier" => bezier_primitive_points(node)?,
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

fn symbol_primitive_stroke_width(node: &Node) -> f64 {
    child_list(node, "stroke")
        .and_then(|stroke| child_list(stroke, "width"))
        .and_then(list_items)
        .and_then(|items| items.get(1))
        .and_then(number)
        .filter(|width| width.is_finite() && width.abs() > 0.001)
        .map_or(0.254, f64::abs)
}

fn rectangle_primitive_points(node: &Node) -> Option<Vec<(f64, f64)>> {
    let start = child_list(node, "start").and_then(list_items)?;
    let end = child_list(node, "end").and_then(list_items)?;
    let x1 = number(start.get(1)?)?;
    let y1 = number(start.get(2)?)?;
    let x2 = number(end.get(1)?)?;
    let y2 = number(end.get(2)?)?;
    Some(vec![(x1, y1), (x2, y1), (x2, y2), (x1, y2)])
}

fn polyline_primitive_points(node: &Node) -> Option<Vec<(f64, f64)>> {
    let points = child_list(node, "pts")?;
    let points: Vec<_> = children(points, "xy")
        .filter_map(|point| {
            let items = list_items(point)?;
            Some((number(items.get(1)?)?, number(items.get(2)?)?))
        })
        .collect();
    (points.len() >= 2).then_some(points)
}

fn arc_primitive_points(node: &Node) -> Option<Vec<(f64, f64)>> {
    let point = |name| {
        let items = child_list(node, name).and_then(list_items)?;
        Some((number(items.get(1)?)?, number(items.get(2)?)?))
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

fn bezier_primitive_points(node: &Node) -> Option<Vec<(f64, f64)>> {
    let points_node = child_list(node, "pts")?;
    let coordinates: Option<Vec<_>> = children(points_node, "xy")
        .map(|point| {
            let items = list_items(point)?;
            Some((number(items.get(1)?)?, number(items.get(2)?)?))
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
    let triangle = Triangle::new(
        CurvePoint::new(start.0, start.1),
        CurvePoint::new(mid.0, mid.1),
        CurvePoint::new(end.0, end.1),
    );
    // Preserve the previous near-collinear fallback before asking Kurbo for
    // the circumcircle; its circumcircle method assumes non-zero triangle area.
    if triangle.area().abs() <= 2.5e-10 {
        return vec![start, mid, end];
    }

    let circle = triangle.circumscribed_circle();
    let center_x = circle.center.x;
    let center_y = circle.center.y;
    let radius = circle.radius.abs();
    if !center_x.is_finite() || !center_y.is_finite() || !radius.is_finite() || radius <= 1.0e-9 {
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
    let mut diagnostics = check_wire_geometry(root, context.segments, context.junctions);
    diagnostics.extend(check_pin_orientation(root, context.segments));
    diagnostics.extend(check_no_connect_markers(root));
    diagnostics.extend(check_wire_obstacles(
        root,
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
    let mut diagnostics = check_label_topology(root, context.segments);
    diagnostics.extend(check_label_wire_clearance(root, context.segments));
    diagnostics.extend(check_label_text_collisions(root));
    diagnostics.extend(check_label_kind_consistency(root));
    diagnostics
}

#[must_use]
pub fn run_annotation_checks(context: &RuleContext<'_>) -> Vec<Diagnostic> {
    let Some(root) = context.document().cst().nodes.first() else {
        return Vec::new();
    };
    let mut diagnostics = check_text_box_layout(root, context.segments);
    diagnostics.extend(check_block_annotation_structure(root));
    diagnostics.extend(check_symbol_field_clearance(
        root,
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
    rotation: f64,
    mirror: Option<String>,
    unit: u32,
    convert: u32,
}

fn check_wire_obstacles(
    root: &Node,
    segments: &[Segment],
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    let fields = collect_visible_field_bounds(root);
    let text_boxes: Vec<_> = children(root, "text_box")
        .enumerate()
        .filter_map(|(index, node)| text_box_bounds(node).map(|bounds| (index, bounds)))
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

fn collect_visible_field_bounds(root: &Node) -> Vec<(String, Bounds, ByteSpan)> {
    let mut fields = Vec::new();
    for symbol in children(root, "symbol") {
        let reference = children(symbol, "property").find_map(|property| {
            let items = list_items(property)?;
            if atom_value(items.get(1)?)? == "Reference" {
                atom_value(items.get(2)?)
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
            let Some(name) = items.get(1).and_then(atom_value) else {
                continue;
            };
            if !matches!(name, "Reference" | "Value") {
                continue;
            }
            let Some(value) = items.get(2).and_then(atom_value) else {
                continue;
            };
            if let Some(bounds) = text_visual_bounds(value, property) {
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
    Line::new(start, end)
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
            vec![intersection]
        }
        Some(LineIntersection::Collinear { intersection }) => {
            let points = [intersection.start, intersection.end];
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

fn check_pin_orientation(root: &Node, segments: &[Segment]) -> Vec<Diagnostic> {
    let Some(library_symbols) = child_list(root, "lib_symbols") else {
        return Vec::new();
    };
    children(root, "symbol")
        .flat_map(|instance| check_instance_pin_orientation(instance, library_symbols, segments))
        .collect()
}

fn check_instance_pin_orientation(
    instance: &Node,
    library_symbols: &Node,
    segments: &[Segment],
) -> Vec<Diagnostic> {
    let Some(transform) = read_symbol_transform(instance) else {
        return Vec::new();
    };
    if transform.reference.starts_with('#') {
        return Vec::new();
    }
    let Some(lib_id) = child_list(instance, "lib_id")
        .and_then(list_items)
        .and_then(|items| items.get(1))
        .and_then(atom_value)
    else {
        return Vec::new();
    };
    let Some(library_symbol) = children(library_symbols, "symbol").find(|definition| {
        list_items(definition)
            .and_then(|items| items.get(1))
            .and_then(atom_value)
            .is_some_and(|name| name == lib_id)
    }) else {
        return Vec::new();
    };

    embedded_symbol_pins(library_symbol, transform.unit, transform.convert)
        .into_iter()
        .filter_map(|pin| pin_orientation_diagnostic(instance, pin, &transform, segments))
        .collect()
}

fn check_no_connect_markers(root: &Node) -> Vec<Diagnostic> {
    let markers = collect_nodes(root, "no_connect");
    if markers.is_empty() {
        return Vec::new();
    }
    let Some(pin_positions) = collect_placed_pin_positions(root) else {
        return Vec::new();
    };
    markers
        .into_iter()
        .filter_map(|marker| {
            let point = point_from_at(marker)?;
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

fn collect_placed_pin_positions(root: &Node) -> Option<Vec<Point>> {
    let mut positions = Vec::new();
    let Some(library_symbols) = child_list(root, "lib_symbols") else {
        return Some(positions);
    };
    for instance in children(root, "symbol") {
        let Some(lib_id) = child_list(instance, "lib_id")
            .and_then(list_items)
            .and_then(|items| items.get(1))
            .and_then(atom_value)
        else {
            continue;
        };
        let transform = read_symbol_transform(instance)?;
        let library_symbol = children(library_symbols, "symbol").find(|definition| {
            list_items(definition)
                .and_then(|items| items.get(1))
                .and_then(atom_value)
                .is_some_and(|name| name == lib_id)
        })?;
        let pins = embedded_symbol_pins(library_symbol, transform.unit, transform.convert);
        if pins.is_empty() {
            return None;
        }
        for pin in pins {
            let at = child_list(pin, "at").and_then(list_items)?;
            let pin_x = number(at.get(1)?)?;
            let pin_y = number(at.get(2)?)?;
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

fn read_symbol_transform(instance: &Node) -> Option<SymbolTransform> {
    let reference = children(instance, "property").find_map(|property| {
        let items = list_items(property)?;
        if atom_value(items.get(1)?)? == "Reference" {
            atom_value(items.get(2)?)
        } else {
            None
        }
    })?;
    let at = child_list(instance, "at").and_then(list_items)?;
    let origin = Point {
        x: number(at.get(1)?)?,
        y: number(at.get(2)?)?,
    };
    let rotation = at.get(3).and_then(number).unwrap_or(0.0);
    if !rotation.is_finite() {
        return None;
    }
    let unit = instance_integer(instance, "unit").unwrap_or(1);
    let convert = instance_integer(instance, "convert").unwrap_or(1);
    let mirror = child_list(instance, "mirror")
        .and_then(list_items)
        .and_then(|items| items.get(1))
        .and_then(atom_value);
    if mirror.is_some_and(|value| value != "x" && value != "y") {
        return None;
    }
    Some(SymbolTransform {
        reference: reference.to_owned(),
        origin,
        rotation,
        mirror: mirror.map(str::to_owned),
        unit,
        convert,
    })
}

fn instance_integer(instance: &Node, name: &str) -> Option<u32> {
    child_list(instance, name)
        .and_then(list_items)
        .and_then(|items| items.get(1))
        .and_then(atom_value)
        .and_then(|value| value.parse().ok())
}

fn embedded_symbol_pins(library_symbol: &Node, unit: u32, convert: u32) -> Vec<&Node> {
    let mut pin_nodes = Vec::new();
    for sub_symbol in children(library_symbol, "symbol") {
        let Some(name) = list_items(sub_symbol)
            .and_then(|items| items.get(1))
            .and_then(atom_value)
        else {
            continue;
        };
        let Some((sub_unit, sub_convert)) = sub_symbol_unit_convert(name) else {
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
    segments: &[Segment],
) -> Option<Diagnostic> {
    let at = child_list(pin, "at").and_then(list_items)?;
    let pin_x = number(at.get(1)?)?;
    let pin_y = number(at.get(2)?)?;
    let pin_angle = number(at.get(3)?)?;
    // SCH005 models only horizontal/vertical pin axes; other rotations are
    // now usable for transformed geometry but remain outside this rule.
    cardinal_rotation(transform.rotation)?;
    let affine = symbol_transform_affine(transform.rotation, transform.mirror.as_deref())?;
    let pin_offset = affine * CurvePoint::new(pin_x, pin_y);
    let point = Point {
        x: transform.origin.x + pin_offset.x,
        y: transform.origin.y + pin_offset.y,
    };
    let pin_angle = pin_angle.to_radians();
    let axis = affine * CurvePoint::new(pin_angle.cos(), pin_angle.sin());
    let (axis_x, axis_y) = (axis.x, axis.y);
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
        .and_then(atom_value)
        .unwrap_or("?");
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

/// Builds the common `KiCad` symbol-local to schematic-space linear transform.
///
/// `KiCad`'s symbol coordinates use a Y-up library frame while schematic sheet
/// coordinates are Y-down. Kurbo applies any finite rotation and the supported
/// mirror modes, avoiding a separate hand-written geometry path for diagonal
/// symbol placements.
fn symbol_transform_affine(rotation_degrees: f64, mirror: Option<&str>) -> Option<Affine> {
    if !rotation_degrees.is_finite() {
        return None;
    }
    let rotation = rotation_degrees.rem_euclid(360.0).to_radians();
    let mirror = match mirror {
        Some("x") => Affine::FLIP_Y,
        Some("y") => Affine::FLIP_X,
        None => Affine::IDENTITY,
        Some(_) => return None,
    };

    Some(Affine::FLIP_Y * Affine::rotate(rotation) * mirror)
}

fn transform_symbol_point(
    x: f64,
    y: f64,
    rotation_degrees: f64,
    mirror: Option<&str>,
) -> Option<(f64, f64)> {
    let point = symbol_transform_affine(rotation_degrees, mirror)? * CurvePoint::new(x, y);
    Some((point.x, point.y))
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

fn check_label_wire_clearance(root: &Node, segments: &[Segment]) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for (kind, label, _point, span, node) in collect_label_nodes(root) {
        if !matches!(kind.as_str(), "label" | "hierarchical_label") {
            continue;
        }
        let Some(bounds) = text_visual_bounds(&label, node) else {
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

fn check_text_box_layout(root: &Node, segments: &[Segment]) -> Vec<Diagnostic> {
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
        let (record, mut box_diagnostics) = inspect_text_box(node, index);
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

fn inspect_text_box(node: &Node, index: usize) -> (Option<TextBoxRecord>, Vec<Diagnostic>) {
    let (metrics, error) = read_text_box_metrics(node, index);
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
    let label = items.get(1).and_then(atom_value).unwrap_or_default();
    let at = child_list(node, "at").and_then(list_items);
    let size = child_list(node, "size").and_then(list_items);
    let margins = child_list(node, "margins").and_then(list_items);
    let font_size = child_list(node, "effects")
        .and_then(|effects| child_list(effects, "font"))
        .and_then(|font| child_list(font, "size"))
        .and_then(list_items)
        .and_then(|font| font.get(1))
        .and_then(number);
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
    let pads: Option<Vec<f64>> = margins.iter().skip(1).map(number).collect();
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
        at.get(1).and_then(number),
        at.get(2).and_then(number),
        size.get(1).and_then(number),
        size.get(2).and_then(number),
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
        .and_then(number)
        .unwrap_or(-1.0);
    (
        Some(TextBoxMetrics {
            label: label.to_owned(),
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

fn check_block_annotation_structure(root: &Node) -> Vec<Diagnostic> {
    collect_nodes(root, "text_box")
        .into_iter()
        .filter_map(|node| {
            let items = list_items(node)?;
            let value = atom_value(items.get(1)?)?;
            let multiline = value.contains('\n') || value.contains('\r');
            let stroke_width = child_list(node, "stroke")
                .and_then(|stroke| child_list(stroke, "width"))
                .and_then(|width| list_items(width))
                .and_then(|width| width.get(1))
                .and_then(number)
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

fn collect_label_nodes(root: &Node) -> Vec<(String, String, Point, ByteSpan, &Node)> {
    let mut labels = Vec::new();
    for node in collect_nodes(root, "label")
        .into_iter()
        .chain(collect_nodes(root, "hierarchical_label"))
        .chain(collect_nodes(root, "global_label"))
    {
        let Some(items) = list_items(node) else {
            continue;
        };
        let Some(label) = items.get(1).and_then(atom_value) else {
            continue;
        };
        let Some(point) = point_from_at(node) else {
            continue;
        };
        let Some(kind) = head(node) else {
            continue;
        };
        labels.push((
            kind.to_owned(),
            label.to_owned(),
            point,
            span_of(node),
            node,
        ));
    }
    labels
}

fn text_visual_bounds(value: &str, node: &Node) -> Option<Bounds> {
    let at = child_list(node, "at").and_then(list_items)?;
    let effects = child_list(node, "effects");
    let font_size = effects
        .and_then(|effects| child_list(effects, "font"))
        .and_then(|font| child_list(font, "size"))
        .and_then(list_items)
        .and_then(|font| font.get(1))
        .and_then(number)
        .unwrap_or(1.0);
    let (width_ratio, height_ratio) = newstroke_text_metrics_ratio(value);
    let width = (width_ratio * font_size).max(font_size);
    let height = (height_ratio * font_size).max(font_size);
    let anchor = Vec2::new(number(at.get(1)?)?, number(at.get(2)?)?);
    let angle = at.get(3).and_then(number).unwrap_or(0.0);

    let justify = effects.and_then(|effects| child_list(effects, "justify"));
    let justify_items = justify.and_then(list_items);
    let justify_name = justify_items
        .and_then(|items| items.get(1))
        .and_then(atom_value)
        .unwrap_or("center");
    let left = match justify_name {
        "left" => 0.0,
        "right" => -width,
        _ => -width / 2.0,
    };

    // KiCad's anchor and justification define a local text rectangle. Kurbo
    // applies the cardinal rotation and returns its world-space AABB.
    let (top, rotation) = if (angle - 90.0).abs() < 0.001 {
        (-height / 2.0, -std::f64::consts::FRAC_PI_2)
    } else if (angle - 270.0).abs() < 0.001 {
        (-height / 2.0, std::f64::consts::FRAC_PI_2)
    } else {
        let top_aligned = justify_items
            .and_then(|items| items.get(2))
            .and_then(atom_value)
            .is_some_and(|value| value == "top");
        (if top_aligned { 0.0 } else { -height }, 0.0)
    };

    let local_bounds = CurveRect::new(left, top, left + width, top + height);
    let transform = Affine::translate(anchor) * Affine::rotate(rotation);
    let bounds = transform.transform_rect_bbox(local_bounds);

    Some(Bounds {
        x: bounds.x0,
        y: bounds.y0,
        width: bounds.width(),
        height: bounds.height(),
    })
}

const NEWSTROKE_GLYPH_LIMIT: u32 = 0x27ff;
const NEWSTROKE_MEASURE_GLYPH: &str = "H";

fn newstroke_sentinel() -> &'static (Vec<VectorPoint>, f64) {
    static SENTINEL: OnceLock<(Vec<VectorPoint>, f64)> = OnceLock::new();
    SENTINEL.get_or_init(|| {
        let points = NewstrokeRenderer::render_text(NEWSTROKE_MEASURE_GLYPH, ());
        let min_y = points.iter().map(|point| point.y).min().unwrap_or(0);
        let max_y = points.iter().map(|point| point.y).max().unwrap_or(0);
        (points, f64::from(max_y - min_y))
    })
}

/// Returns `NewStroke`'s total glyph advance normalized by its measured cap height.
///
/// The upstream renderer exposes points rather than a text-measurement method.
/// Appending a known glyph makes its translated start position equal to the
/// preceding text's full advance, including spaces and trailing blank glyphs.
fn newstroke_text_metrics_ratio(value: &str) -> (f64, f64) {
    let (sentinel, cap_height) = newstroke_sentinel();
    let Some(sentinel_first) = sentinel.first() else {
        return (0.0, 0.0);
    };
    if *cap_height <= 0.0 {
        return (0.0, 0.0);
    }

    // The upstream renderer indexes a fixed Unicode table. Filter out-of-range
    // scalars before calling it; unsupported in-range characters are skipped by
    // the renderer itself.
    let mut measured = String::with_capacity(value.len() + NEWSTROKE_MEASURE_GLYPH.len());
    measured.extend(
        value
            .chars()
            .filter(|character| u32::from(*character) < NEWSTROKE_GLYPH_LIMIT),
    );
    measured.push_str(NEWSTROKE_MEASURE_GLYPH);

    let rendered = NewstrokeRenderer::render_text(&measured, ());
    let Some(sentinel_start) = rendered.len().checked_sub(sentinel.len()) else {
        return (0.0, 0.0);
    };
    let Some(rendered_sentinel_first) = rendered.get(sentinel_start) else {
        return (0.0, 0.0);
    };
    let advance_units = i32::from(rendered_sentinel_first.x) - i32::from(sentinel_first.x);
    let glyph_points = &rendered[..sentinel_start];
    let glyph_height = match (
        glyph_points.iter().map(|point| point.y).min(),
        glyph_points.iter().map(|point| point.y).max(),
    ) {
        (Some(min_y), Some(max_y)) => f64::from(max_y - min_y),
        _ => *cap_height,
    };

    (
        f64::from(advance_units) / cap_height,
        glyph_height / cap_height,
    )
}

fn required_text_box_size(value: &str, font_size: f64, padding: f64) -> (f64, f64) {
    let lines: Vec<_> = value.lines().collect();
    let line_count = u32::try_from(lines.len().max(1)).unwrap_or(u32::MAX);
    let max_width = lines
        .iter()
        .map(|line| newstroke_text_metrics_ratio(line).0 * font_size)
        .fold(0.0_f64, f64::max);
    (
        2.0_f64.mul_add(padding, (max_width * 1.30).max(font_size)),
        (f64::from(line_count) * font_size).mul_add(1.55, 2.0 * padding),
    )
}

fn bounds_overlap(first: Bounds, second: Bounds) -> bool {
    (first.x + first.width).min(second.x + second.width) - first.x.max(second.x) > 0.001
        && (first.y + first.height).min(second.y + second.height) - first.y.max(second.y) > 0.001
}

fn wire_overlaps_bounds(segment: Segment, bounds: Bounds, strict: bool) -> bool {
    const TOLERANCE: f64 = 0.001;

    let horizontal = (segment.start.y - segment.end.y).abs() < TOLERANCE;
    let vertical = (segment.start.x - segment.end.x).abs() < TOLERANCE;
    if !horizontal && !vertical {
        return false;
    }

    let inset = if strict { TOLERANCE } else { -TOLERANCE };
    let clipped_bounds = if horizontal {
        Bounds {
            y: bounds.y + inset,
            height: 2.0_f64.mul_add(-inset, bounds.height),
            ..bounds
        }
    } else {
        Bounds {
            x: bounds.x + inset,
            width: 2.0_f64.mul_add(-inset, bounds.width),
            ..bounds
        }
    };
    if clipped_bounds.width <= 0.0 || clipped_bounds.height <= 0.0 {
        return false;
    }

    // Geo's line clip includes polygon boundaries. Preserve the old strict
    // comparison at the tolerance edge before delegating interval clipping.
    let axis = if horizontal {
        segment.start.y
    } else {
        segment.start.x
    };
    let (lower, upper) = if horizontal {
        (clipped_bounds.y, clipped_bounds.y + clipped_bounds.height)
    } else {
        (clipped_bounds.x, clipped_bounds.x + clipped_bounds.width)
    };
    if (strict && (axis <= lower || axis >= upper)) || (!strict && (axis < lower || axis > upper)) {
        return false;
    }

    let wire = MultiLineString::new(vec![LineString::new(vec![segment.start, segment.end])]);
    let clipped = geo_rect(clipped_bounds).to_polygon().clip(&wire, false);

    Euclidean.length(&clipped) > TOLERANCE
}

fn line_hits_bounds(segment: Segment, bounds: Bounds) -> bool {
    wire_overlaps_bounds(segment, bounds, false)
}

fn strict_line_hits_bounds(segment: Segment, bounds: Bounds) -> bool {
    wire_overlaps_bounds(segment, bounds, true)
}

fn at_span(code: BuiltinRuleId, message: impl Into<String>, span: ByteSpan) -> Diagnostic {
    let mut diagnostic = Diagnostic::for_builtin_rule(code, Severity::Error, message);
    diagnostic.span = Some(span);
    diagnostic
}

fn check_label_topology(root: &Node, segments: &[Segment]) -> Vec<Diagnostic> {
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
    let mut components = UnionFind::<usize>::new(points.len());
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

fn check_label_text_collisions(root: &Node) -> Vec<Diagnostic> {
    let mut labels = Vec::new();
    for (kind, label, _point, span, node) in collect_label_nodes(root) {
        if let Some(bounds) = text_visual_bounds(&label, node) {
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
            let Some(value) = items.get(2).and_then(atom_value) else {
                continue;
            };
            if text_visual_bounds(value, property)
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
            if text_box_bounds(text_box)
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

fn check_label_kind_consistency(root: &Node) -> Vec<Diagnostic> {
    let mut label_kinds: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (kind, label, _point, _span) in collect_labels(root) {
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

fn atom_value(node: &Node) -> Option<&str> {
    let Node::Atom { atom, .. } = node else {
        return None;
    };
    match atom {
        Atom::Symbol(value) | Atom::Quoted(value) => Some(value.as_str()),
    }
}

fn number(node: &Node) -> Option<f64> {
    atom_value(node)?.parse().ok()
}

fn child_list<'a>(node: &'a Node, name: &'a str) -> Option<&'a Node> {
    children(node, name).next()
}

fn point_from_at(node: &Node) -> Option<Point> {
    let at = child_list(node, "at")?;
    let items = list_items(at)?;
    Some(Point {
        x: number(items.get(1)?)?,
        y: number(items.get(2)?)?,
    })
}

const fn span_of(node: &Node) -> ByteSpan {
    match node {
        Node::List { span, .. } | Node::Atom { span, .. } => ByteSpan::new(span.start, span.end),
    }
}

#[must_use]
pub fn collect_segments(root: &Node) -> Vec<Segment> {
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
                    x: number(items.get(1)?)?,
                    y: number(items.get(2)?)?,
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
pub fn collect_points(root: &Node, name: &str) -> Vec<Point> {
    let mut points = Vec::new();
    walk(root, &mut |node| {
        if head(node) == Some(name) {
            if let Some(point) = point_from_at(node) {
                points.push(point);
            }
        }
    });
    points
}

fn collect_labels(root: &Node) -> Vec<(String, String, Point, ByteSpan)> {
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
        let Some(label) = items.get(1).and_then(atom_value) else {
            return;
        };
        if let Some(point) = point_from_at(node) {
            labels.push((kind.to_owned(), label.to_owned(), point, span_of(node)));
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

    #[test]
    fn geo_rectangle_clipping_preserves_wire_overlap_tolerance() {
        let bounds = super::Bounds {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        };
        let horizontal = |y| super::Segment {
            start: super::Point { x: -1.0, y },
            end: super::Point { x: 11.0, y },
            span: kicad_clippy_core::ByteSpan::new(0, 0),
        };
        let vertical = |x| super::Segment {
            start: super::Point { x, y: -1.0 },
            end: super::Point { x, y: 11.0 },
            span: kicad_clippy_core::ByteSpan::new(0, 0),
        };

        assert!(super::line_hits_bounds(horizontal(0.0), bounds));
        assert!(!super::strict_line_hits_bounds(horizontal(0.0), bounds));
        assert!(super::strict_line_hits_bounds(horizontal(5.0), bounds));
        assert!(super::line_hits_bounds(horizontal(-0.0005), bounds));
        assert!(super::line_hits_bounds(horizontal(-0.001), bounds));
        assert!(!super::line_hits_bounds(horizontal(-0.002), bounds));
        assert!(!super::strict_line_hits_bounds(horizontal(0.001), bounds));
        assert!(super::strict_line_hits_bounds(horizontal(0.0011), bounds));
        assert!(!super::strict_line_hits_bounds(horizontal(9.999), bounds));

        assert!(super::line_hits_bounds(vertical(10.0), bounds));
        assert!(!super::strict_line_hits_bounds(vertical(10.0), bounds));
        assert!(super::strict_line_hits_bounds(vertical(5.0), bounds));
        assert!(!super::strict_line_hits_bounds(vertical(0.001), bounds));
        assert!(super::strict_line_hits_bounds(vertical(0.0011), bounds));
        assert!(!super::strict_line_hits_bounds(vertical(9.999), bounds));
        assert!(!super::line_hits_bounds(
            super::Segment {
                start: super::Point { x: -1.0, y: -1.0 },
                end: super::Point { x: 11.0, y: 11.0 },
                span: kicad_clippy_core::ByteSpan::new(0, 0),
            },
            bounds,
        ));
    }

    #[test]
    fn kurbo_circumcircle_preserves_arc_direction_and_collinear_fallback() {
        let upper = super::sample_arc_through_three_points((-2.0, 0.0), (0.0, 2.0), (2.0, 0.0));
        assert_eq!(upper.first(), Some(&(-2.0, 0.0)));
        assert_eq!(upper.last(), Some(&(2.0, 0.0)));
        assert!(upper.iter().any(|point| point.1 > 1.9));

        let lower = super::sample_arc_through_three_points((-2.0, 0.0), (0.0, -2.0), (2.0, 0.0));
        assert_eq!(lower.first(), Some(&(-2.0, 0.0)));
        assert_eq!(lower.last(), Some(&(2.0, 0.0)));
        assert!(lower.iter().any(|point| point.1 < -1.9));

        let collinear = super::sample_arc_through_three_points((0.0, 0.0), (1.0, 0.0), (2.0, 0.0));
        assert_eq!(collinear, [(0.0, 0.0), (1.0, 0.0), (2.0, 0.0)]);
    }

    #[test]
    fn kurbo_affine_matches_kicad_cardinal_symbol_transforms() {
        let cases = [
            (0, None, (2.0, -3.0)),
            (90, None, (-3.0, -2.0)),
            (180, None, (-2.0, 3.0)),
            (270, None, (3.0, 2.0)),
            (0, Some("x"), (2.0, 3.0)),
            (0, Some("y"), (-2.0, -3.0)),
            (90, Some("x"), (3.0, -2.0)),
            (90, Some("y"), (-3.0, 2.0)),
        ];

        for (rotation, mirror, expected) in cases {
            let actual = super::transform_symbol_point(2.0, 3.0, rotation, mirror)
                .expect("supported KiCad transform");
            assert!((actual.0 - expected.0).abs() < 0.001);
            assert!((actual.1 - expected.1).abs() < 0.001);
        }
        let diagonal = super::transform_symbol_point(2.0, 3.0, 45.0, None)
            .expect("finite non-cardinal KiCad transform");
        let radians = 45.0_f64.to_radians();
        let expected_x = 2.0 * radians.cos() - 3.0 * radians.sin();
        let expected_y = -(2.0 * radians.sin() + 3.0 * radians.cos());
        assert!((diagonal.0 - expected_x).abs() < 0.001);
        assert!((diagonal.1 - expected_y).abs() < 0.001);
        assert!(super::transform_symbol_point(2.0, 3.0, 0.0, Some("diagonal")).is_none());
        assert!(super::transform_symbol_point(2.0, 3.0, f64::NAN, None).is_none());
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
    fn non_cardinal_symbol_rotation_resolves_embedded_pin_geometry() {
        let source = EMBEDDED_PIN.replace("(at 10 10 0)", "(at 10 10 45)");
        let doc = document(&source);
        let facts = super::SchematicFacts::from_document(&doc);
        assert_eq!(facts.pins.len(), 1);
        let expected = 10.0 - 2.54 / 2.0_f64.sqrt();
        assert!((facts.pins[0].position_mm.0 - expected).abs() < 0.001);
        assert!((facts.pins[0].position_mm.1 - expected).abs() < 0.001);
        assert!(
            !run_builtin_rules(&doc)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH005")
        );
    }

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
    fn quoted_atoms_use_parser_decoded_escape_sequences() {
        let doc = document(r#"(label "first\nsecond" (at 1 1 0))"#);
        let root = doc.cst().nodes.first().expect("schematic root");
        let labels = super::collect_labels(root);
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].1, "first\nsecond");
    }

    #[test]
    fn newstroke_text_height_uses_rendered_glyph_geometry() {
        let cap = super::newstroke_text_metrics_ratio("H").1;
        let narrow = super::newstroke_text_metrics_ratio("i").1;
        let lowercase = super::newstroke_text_metrics_ratio("g").1;
        let blank = super::newstroke_text_metrics_ratio(" ").1;

        assert!((cap - 1.0).abs() < 0.001);
        assert!(narrow > 0.0 && narrow <= cap);
        assert!(lowercase > 0.0 && lowercase <= cap);
        assert!((blank - cap).abs() < 0.001);
    }

    #[test]
    fn text_bounds_use_kurbo_for_cardinal_rotation_and_justification() {
        let (width, height) = super::newstroke_text_metrics_ratio("AB");
        let cases = [
            (0.0, "left", "bottom", 10.0, 20.0 - height, width, height),
            (0.0, "left", "top", 10.0, 20.0, width, height),
            (
                0.0,
                "center",
                "bottom",
                10.0 - width / 2.0,
                20.0 - height,
                width,
                height,
            ),
            (
                90.0,
                "left",
                "center",
                10.0 - height / 2.0,
                20.0 - width,
                height,
                width,
            ),
            (
                90.0,
                "right",
                "center",
                10.0 - height / 2.0,
                20.0,
                height,
                width,
            ),
            (
                90.0,
                "center",
                "center",
                10.0 - height / 2.0,
                20.0 - width / 2.0,
                height,
                width,
            ),
            (
                270.0,
                "left",
                "center",
                10.0 - height / 2.0,
                20.0,
                height,
                width,
            ),
            (
                270.0,
                "right",
                "center",
                10.0 - height / 2.0,
                20.0 - width,
                height,
                width,
            ),
        ];

        for (angle, horizontal, vertical, x, y, expected_width, expected_height) in cases {
            let source = format!(
                r#"(label "AB" (at 10 20 {angle}) (effects (font (size 1 1)) (justify {horizontal} {vertical})))"#
            );
            let doc = document(&source);
            let root = doc.cst().nodes.first().expect("schematic root");
            let node = super::children(root, "label").next().expect("label node");
            let bounds = super::text_visual_bounds("AB", node).expect("text bounds");

            assert!((bounds.x - x).abs() < 0.001);
            assert!((bounds.y - y).abs() < 0.001);
            assert!((bounds.width - expected_width).abs() < 0.001);
            assert!((bounds.height - expected_height).abs() < 0.001);
        }
    }

    #[test]
    fn text_widths_use_kicad_newstroke_glyph_advances() {
        let cap_height = 21.0;
        let cases = [
            ("A", 18.0),
            ("H", 22.0),
            ("M", 24.0),
            ("I", 10.0),
            (" ", 16.0),
            ("A H", 56.0),
        ];

        for (text, expected_units) in cases {
            let actual = super::newstroke_text_metrics_ratio(text).0 * cap_height;
            assert!(
                (actual - expected_units).abs() < 0.001,
                "unexpected NewStroke advance for {text:?}: {actual}"
            );
        }
    }

    #[test]
    fn unsupported_unicode_does_not_panic_or_shift_newstroke_metrics() {
        let supported = super::newstroke_text_metrics_ratio("AH");
        let with_unsupported = super::newstroke_text_metrics_ratio("A\u{10ffff}H");
        assert!((supported.0 - with_unsupported.0).abs() < 0.001);
        assert!((supported.1 - with_unsupported.1).abs() < 0.001);
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