use std::collections::BTreeMap;

use kicad_clippy_core::ByteSpan;
use kicad_clippy_parser::SchematicDocument;
use kiutils_sexpr::{Atom, Node};

/// Physical point in KiCad schematic millimetres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    fn new(x: f64, y: f64) -> Self {
        Self { x: round3(x), y: round3(y) }
    }

    pub fn distance(self, other: Self) -> f64 {
        ((self.x - other.x).powi(2) + (self.y - other.y).powi(2)).sqrt()
    }
}

/// Axis-aligned rectangle represented by its minimum and maximum coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rect {
    pub min: Point,
    pub max: Point,
}

impl Rect {
    pub fn from_xywh(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            min: Point::new(x, y),
            max: Point::new(x + width, y + height),
        }
    }

    pub fn width(self) -> f64 { self.max.x - self.min.x }

    pub fn contains(self, point: Point, margin: f64) -> bool {
        point.x >= self.min.x + margin - TOL
            && point.x <= self.max.x - margin + TOL
            && point.y >= self.min.y + margin - TOL
            && point.y <= self.max.y - margin + TOL
    }

    pub fn contains_rect(self, other: Self, margin: f64) -> bool {
        other.min.x >= self.min.x + margin - TOL
            && other.min.y >= self.min.y + margin - TOL
            && other.max.x <= self.max.x - margin + TOL
            && other.max.y <= self.max.y - margin + TOL
    }

    pub fn overlaps(self, other: Self) -> bool {
        self.min.x < other.max.x - TOL
            && self.max.x > other.min.x + TOL
            && self.min.y < other.max.y - TOL
            && self.max.y > other.min.y + TOL
    }

    pub fn gap(self, other: Self) -> f64 {
        let dx = (other.min.x - self.max.x).max(self.min.x - other.max.x).max(0.0);
        let dy = (other.min.y - self.max.y).max(self.min.y - other.max.y).max(0.0);
        (dx * dx + dy * dy).sqrt()
    }
}

/// One orthogonal or diagonal wire segment from the schematic source.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Segment {
    pub start: Point,
    pub end: Point,
}

/// Label kind and presentation geometry.
#[derive(Clone, Debug)]
pub(crate) struct Label<'a> {
    pub kind: &'a str,
    pub name: String,
    pub point: Point,
    pub angle: i32,
    pub bounds: Rect,
    pub node: &'a Node,
}

/// Visible symbol field, including its estimated rendered glyph bounds.
#[derive(Clone, Debug)]
pub(crate) struct Field<'a> {
    pub name: String,
    pub point: Point,
    pub bounds: Rect,
    pub node: &'a Node,
}

/// A placed schematic symbol instance.
#[derive(Clone, Debug)]
pub(crate) struct Symbol<'a> {
    pub reference: String,
    pub value: String,
    pub lib_id: String,
    pub point: Point,
    pub dnp: bool,
    pub fields: Vec<Field<'a>>,
    pub node: &'a Node,
}

/// An embedded library pin resolved into drawing coordinates.
#[derive(Clone, Debug)]
pub(crate) struct Pin {
    pub reference: String,
    pub number: String,
    pub point: Point,
    pub angle: i32,
}

/// A schematic text box and its physical extent.
#[derive(Clone, Debug)]
pub(crate) struct TextBox<'a> {
    pub text: String,
    pub bounds: Rect,
    pub outlined: bool,
    pub node: &'a Node,
}

/// A transformed primitive from an embedded symbol definition.
#[derive(Clone, Debug)]
pub(crate) enum Primitive {
    Polyline { points: Vec<Point>, closed: bool, stroke: f64 },
    Circle { center: Point, radius: f64, stroke: f64 },
}

/// Geometry and source index built once from the lossless schematic CST.
pub(crate) struct SchematicModel<'a> {
    pub wires: Vec<Segment>,
    pub labels: Vec<Label<'a>>,
    pub symbols: Vec<Symbol<'a>>,
    pub pins: Vec<Pin>,
    pub text_boxes: Vec<TextBox<'a>>,
    pub frames: Vec<(Rect, &'a Node)>,
    pub junctions: Vec<Point>,
    pub no_connects: Vec<Point>,
    pub primitives: BTreeMap<String, Vec<Primitive>>,
    pub uuids: Vec<(String, Option<ByteSpan>)>,
    pub title_fields: BTreeMap<String, String>,
}

pub(crate) const TOL: f64 = 0.001;

impl<'a> SchematicModel<'a> {
    pub fn new(document: &'a SchematicDocument) -> Self {
        let root = &document.cst().nodes[0];
        let wires = direct_nodes(root, "wire")
            .into_iter()
            .flat_map(|wire| {
                let points = direct_nodes(wire, "pts")
                    .first()
                    .map_or_else(Vec::new, |pts| direct_nodes(pts, "xy"));
                if points.len() == 2 {
                    match (xy(points[0]), xy(points[1])) {
                        (Some(start), Some(end)) => vec![Segment { start, end }],
                        _ => Vec::new(),
                    }
                } else {
                    Vec::new()
                }
            })
            .collect();

        let labels = ["label", "hierarchical_label", "global_label"]
            .into_iter()
            .flat_map(|kind| {
                direct_nodes(root, kind)
                    .into_iter()
                    .filter_map(move |node| {
                        let name = direct_atom(node, kind)?;
                        let (point, angle) = at(node)?;
                        Some(Label {
                            kind,
                            name: name.to_owned(),
                            point,
                            angle,
                            bounds: text_bounds(name, node, point, angle),
                            node,
                        })
                    })
            })
            .collect();

        let lib_root = direct_nodes(root, "lib_symbols").into_iter().next();
        let library: BTreeMap<String, &Node> = lib_root.map_or_else(BTreeMap::new, |lib| {
            direct_nodes(lib, "symbol")
                .into_iter()
                .filter_map(|item| Some((child_atom(item, "symbol")?.to_owned(), item)))
                .collect()
        });

        let mut symbols = Vec::new();
        let mut pins = Vec::new();
        let mut primitives = BTreeMap::new();
        for instance in direct_nodes(root, "symbol") {
            let Some(reference) = property_value(instance, "Reference") else { continue; };
            let Some((point, rotation)) = at(instance) else { continue; };
            let value = property_value(instance, "Value").unwrap_or_default();
            let lib_id = child_atom(instance, "lib_id").unwrap_or_default().to_owned();
            let dnp = child_atom(instance, "dnp").is_some_and(|flag| flag == "yes");
            let fields = direct_nodes(instance, "property")
                .into_iter()
                .filter_map(|field| {
                    let name = atom_at(field, 1)?.to_owned();
                    let value = atom_at(field, 2)?.to_owned();
                    let (field_point, angle) = at(field)?;
                    Some(Field {
                        name,
                        point: field_point,
                        bounds: text_bounds(&value, field, field_point, angle),
                        node: field,
                    })
                })
                .collect();
            let symbol = Symbol {
                reference: reference.to_owned(),
                value,
                lib_id,
                point,
                dnp,
                fields,
                node: instance,
            };
            if let Some(definition) = library.get(&lib_id) {
                pins.extend(extract_pins(reference, *definition, point, rotation));
                let body = extract_primitives(*definition, point, rotation);
                if !body.is_empty() {
                    primitives.insert(reference.to_owned(), body);
                }
            }
            symbols.push(symbol);
        }

        let text_boxes = direct_nodes(root, "text_box")
            .into_iter()
            .filter_map(|node| {
                let text = atom_at(node, 1)?.to_owned();
                let (point, _) = at(node)?;
                let size = direct_nodes(node, "size").first().copied()?;
                let width = atom_at(size, 1)?.parse::<f64>().ok()?;
                let height = atom_at(size, 2)?.parse::<f64>().ok()?;
                let stroke = direct_nodes(node, "stroke").first().copied();
                let stroke_width = stroke.and_then(|stroke| child_atom(stroke, "width"))
                    .and_then(|number| number.parse::<f64>().ok())
                    .unwrap_or(0.0);
                Some(TextBox {
                    text,
                    bounds: Rect::from_xywh(point.x, point.y, width, height),
                    outlined: stroke_width > 0.0,
                    node,
                })
            })
            .collect();

        let frames = direct_nodes(root, "rectangle")
            .into_iter()
            .filter_map(|node| {
                let start = direct_nodes(node, "start").first().copied().and_then(xy)?;
                let end = direct_nodes(node, "end").first().copied().and_then(xy)?;
                Some((Rect {
                    min: Point::new(start.x.min(end.x), start.y.min(end.y)),
                    max: Point::new(start.x.max(end.x), start.y.max(end.y)),
                }, node))
            })
            .collect();

        let junctions = direct_nodes(root, "junction")
            .into_iter()
            .filter_map(|node| at(node).map(|(point, _)| point))
            .collect();
        let no_connects = direct_nodes(root, "no_connect")
            .into_iter()
            .filter_map(|node| at(node).map(|(point, _)| point))
            .collect();

        let uuids = all_nodes(root, "uuid")
            .into_iter()
            .filter_map(|node| {
                let id = atom_at(node, 1)?.to_owned();
                Some((id, span(node)))
            })
            .collect();

        let title_fields = direct_nodes(root, "title_block")
            .first()
            .map_or_else(BTreeMap::new, |block| {
                ["title", "date", "rev", "company", "comment"]
                    .into_iter()
                    .filter_map(|key| {
                        let value = child_atom(block, key)?;
                        Some((key.to_owned(), value.to_owned()))
                    })
                    .collect()
            });

        Self {
            wires,
            labels,
            symbols,
            pins,
            text_boxes,
            frames,
            junctions,
            no_connects,
            primitives,
            uuids,
            title_fields,
        }
    }

    pub fn symbol(&self, reference: &str) -> Option<&Symbol<'a>> {
        self.symbols.iter().find(|symbol| symbol.reference == reference)
    }

    pub fn pin(&self, reference: &str, number: &str) -> Option<&Pin> {
        self.pins.iter().find(|pin| pin.reference == reference && pin.number == number)
    }

    pub fn local_label_exists(&self, point: Point, name: &str) -> bool {
        self.labels.iter().any(|label| label.name == name
            && label.kind == "label" && label.point == point)
    }

    pub fn is_wire_attached(&self, point: Point) -> bool {
        self.wires.iter().any(|wire| point_on_segment(point, wire.start, wire.end))
    }

    pub fn frame_for_group(&self, title: &str) -> Option<Rect> {
        let tab = self.text_boxes.iter().find(|item| item.text == title)?;
        let center_x = (tab.bounds.min.x + tab.bounds.max.x) / 2.0;
        self.frames.iter()
            .map(|(rect, _)| *rect)
            .filter_map(|rect| {
                if center_x < rect.min.x - TOL || center_x > rect.max.x + TOL {
                    return None;
                }
                let vertical_gap = if tab.bounds.max.y < rect.min.y {
                    rect.min.y - tab.bounds.max.y
                } else if rect.max.y < tab.bounds.min.y {
                    tab.bounds.min.y - rect.max.y
                } else {
                    0.0
                };
                (vertical_gap <= 5.08).then_some((rect, vertical_gap))
            })
            .min_by(|left, right| left.1.total_cmp(&right.1))
            .map(|(rect, _)| rect)
    }

    pub fn node_for_group_description(&self, phrases: &[String]) -> Option<&'a Node> {
        self.text_boxes.iter()
            .find(|item| phrases.iter().any(|phrase| item.text.contains(phrase)))
            .map(|item| item.node)
    }

    pub fn bounds_for_symbol(&self, reference: &str) -> Option<Rect> {
        let primitives = self.primitives.get(reference)?;
        let mut points = Vec::new();
        for primitive in primitives {
            match primitive {
                Primitive::Polyline { points: vertices, .. } => points.extend(vertices.iter().copied()),
                Primitive::Circle { center, radius, .. } => {
                    points.push(Point::new(center.x - radius, center.y - radius));
                    points.push(Point::new(center.x + radius, center.y + radius));
                }
            }
        }
        let min_x = points.iter().map(|point| point.x).min_by(f64::total_cmp)?;
        let max_x = points.iter().map(|point| point.x).max_by(f64::total_cmp)?;
        let min_y = points.iter().map(|point| point.y).min_by(f64::total_cmp)?;
        let max_y = points.iter().map(|point| point.y).max_by(f64::total_cmp)?;
        Some(Rect { min: Point::new(min_x, min_y), max: Point::new(max_x, max_y) })
    }
}

pub(crate) fn head(node: &Node) -> Option<&str> {
    let Node::List { items, .. } = node else { return None; };
    atom(items.first()?)
}

pub(crate) fn atom(node: &Node) -> Option<&str> {
    let Node::Atom { atom: Atom::Symbol(value) | Atom::Quoted(value), .. } = node else {
        return None;
    };
    Some(value)
}

pub(crate) fn atom_at(node: &Node, index: usize) -> Option<&str> {
    let Node::List { items, .. } = node else { return None; };
    atom(items.get(index)?)
}

pub(crate) fn direct_nodes<'a>(node: &'a Node, name: &str) -> Vec<&'a Node> {
    let Node::List { items, .. } = node else { return Vec::new(); };
    items.iter().skip(1).filter(|item| head(item) == Some(name)).collect()
}

pub(crate) fn child_atom<'a>(node: &'a Node, name: &str) -> Option<&'a str> {
    direct_nodes(node, name).first().and_then(|item| atom_at(item, 1))
}

fn direct_atom<'a>(node: &'a Node, name: &str) -> Option<&'a str> {
    atom_at(node, 1).filter(|_| head(node) == Some(name))
}

pub(crate) fn span(node: &Node) -> Option<ByteSpan> {
    match node {
        Node::List { span, .. } => Some(ByteSpan::new(span.start, span.end)),
        Node::Atom { span, .. } => Some(ByteSpan::new(span.start, span.end)),
    }
}

fn all_nodes<'a>(node: &'a Node, name: &str) -> Vec<&'a Node> {
    fn visit<'a>(node: &'a Node, name: &str, output: &mut Vec<&'a Node>) {
        if head(node) == Some(name) {
            output.push(node);
        }
        let Node::List { items, .. } = node else { return; };
        for item in items.iter().skip(1) {
            visit(item, name, output);
        }
    }
    let mut output = Vec::new();
    visit(node, name, &mut output);
    output
}

fn xy(node: &Node) -> Option<Point> {
    Some(Point::new(atom_at(node, 1)?.parse().ok()?, atom_at(node, 2)?.parse().ok()?))
}

fn at(node: &Node) -> Option<(Point, i32)> {
    let at = direct_nodes(node, "at").first().copied()?;
    Some((xy(at)?, atom_at(at, 3).and_then(|value| value.parse::<f64>().ok()).unwrap_or(0.0) as i32))
}

fn property_value<'a>(node: &'a Node, name: &str) -> Option<&'a str> {
    direct_nodes(node, "property").into_iter().find_map(|property| {
        (atom_at(property, 1) == Some(name)).then(|| atom_at(property, 2)).flatten()
    })
}

fn text_bounds(value: &str, node: &Node, anchor: Point, angle: i32) -> Rect {
    let effects = direct_nodes(node, "effects").first().copied();
    let font = effects.and_then(|effects| direct_nodes(effects, "font").first().copied())
        .and_then(|font| direct_nodes(font, "size").first().copied())
        .and_then(|size| atom_at(size, 1))
        .and_then(|size| size.parse::<f64>().ok())
        .unwrap_or(1.0);
    let justify = effects.and_then(|effects| direct_nodes(effects, "justify").first().copied());
    let left_justified = justify.is_some_and(|item| atom_at(item, 1) == Some("left"));
    let right_justified = justify.is_some_and(|item| atom_at(item, 1) == Some("right"));
    let width = value.lines().map(|line| line.chars().map(character_advance).sum::<f64>())
        .fold(font, f64::max) * font * 1.12;
    let height = font * 1.55;
    if angle.rem_euclid(360) == 90 || angle.rem_euclid(360) == 270 {
        let top = if angle.rem_euclid(360) == 90 {
            if left_justified { anchor.y - width } else if right_justified { anchor.y } else { anchor.y - width / 2.0 }
        } else if left_justified { anchor.y } else if right_justified { anchor.y - width } else { anchor.y - width / 2.0 };
        Rect::from_xywh(anchor.x - height / 2.0, top, height, width)
    } else {
        let left = if left_justified { anchor.x } else if right_justified { anchor.x - width } else { anchor.x - width / 2.0 };
        let top = if justify.is_some_and(|item| atom_at(item, 2) == Some("top")) { anchor.y } else { anchor.y - height };
        Rect::from_xywh(left, top, width, height)
    }
}

fn character_advance(character: char) -> f64 {
    if "ilI.,:;!|'".contains(character) { 0.30 }
    else if "MW@%&".contains(character) { 0.90 }
    else if character.is_whitespace() { 0.34 }
    else if character.is_uppercase() || character.is_numeric() { 0.64 }
    else { 0.55 }
}

fn round3(value: f64) -> f64 { (value * 1000.0).round() / 1000.0 }

fn extract_pins(reference: &str, definition: &Node, origin: Point, rotation: i32) -> Vec<Pin> {
    let mut result = Vec::new();
    for node in all_nodes(definition, "pin") {
        let Some(number) = direct_nodes(node, "number").first().and_then(|item| atom_at(item, 1)) else { continue; };
        let Some((local, local_angle)) = at(node) else { continue; };
        if result.iter().any(|pin: &Pin| pin.number == number) { continue; }
        let world = transform(origin, rotation, local);
        result.push(Pin {
            reference: reference.to_owned(),
            number: number.to_owned(),
            point: world,
            angle: (local_angle + rotation).rem_euclid(360),
        });
    }
    result
}

fn extract_primitives(definition: &Node, origin: Point, rotation: i32) -> Vec<Primitive> {
    let mut result = Vec::new();
    for node in all_nodes(definition, "rectangle") {
        let (Some(start), Some(end)) = (
            direct_nodes(node, "start").first().copied().and_then(xy),
            direct_nodes(node, "end").first().copied().and_then(xy),
        ) else { continue; };
        let points = [
            start,
            Point::new(end.x, start.y),
            end,
            Point::new(start.x, end.y),
        ].into_iter().map(|point| transform(origin, rotation, point)).collect();
        result.push(Primitive::Polyline { points, closed: true, stroke: stroke_width(node) });
    }
    for node in all_nodes(definition, "polyline").into_iter().chain(all_nodes(definition, "polygon")) {
        let kind = head(node).unwrap_or_default();
        let points = direct_nodes(node, "pts").first().map_or_else(Vec::new, |pts| {
            direct_nodes(pts, "xy").into_iter().filter_map(xy)
                .map(|point| transform(origin, rotation, point)).collect()
        });
        if points.len() >= 2 {
            let closed = kind == "polygon" || points.first() == points.last();
            result.push(Primitive::Polyline { points, closed, stroke: stroke_width(node) });
        }
    }
    for node in all_nodes(definition, "circle") {
        let (Some(center), Some(radius)) = (
            direct_nodes(node, "center").first().copied().and_then(xy),
            direct_nodes(node, "radius").first().and_then(|item| atom_at(item, 1)).and_then(|value| value.parse::<f64>().ok()),
        ) else { continue; };
        result.push(Primitive::Circle {
            center: transform(origin, rotation, center),
            radius: radius.abs(),
            stroke: stroke_width(node),
        });
    }
    for node in all_nodes(definition, "arc") {
        let points: Vec<Point> = ["start", "mid", "end"].iter().filter_map(|field| {
            direct_nodes(node, field).first().copied().and_then(xy)
        }).map(|point| transform(origin, rotation, point)).collect();
        if points.len() >= 2 {
            result.push(Primitive::Polyline { points, closed: false, stroke: stroke_width(node) });
        }
    }
    result
}

fn stroke_width(node: &Node) -> f64 {
    direct_nodes(node, "stroke").first().copied()
        .and_then(|stroke| child_atom(stroke, "width"))
        .and_then(|width| width.parse::<f64>().ok())
        .filter(|width| *width > 0.0).unwrap_or(0.254)
}

fn transform(origin: Point, rotation: i32, local: Point) -> Point {
    let (x, y) = match rotation.rem_euclid(360) {
        0 => (origin.x + local.x, origin.y - local.y),
        90 => (origin.x - local.y, origin.y - local.x),
        180 => (origin.x - local.x, origin.y + local.y),
        270 => (origin.x + local.y, origin.y + local.x),
        _ => (origin.x + local.x, origin.y - local.y),
    };
    Point::new(x, y)
}

pub(crate) fn point_on_segment(point: Point, start: Point, end: Point) -> bool {
    let cross = (point.x - start.x) * (end.y - start.y) - (point.y - start.y) * (end.x - start.x);
    if cross.abs() > TOL { return false; }
    let dot = (point.x - start.x) * (end.x - start.x) + (point.y - start.y) * (end.y - start.y);
    let length2 = (end.x - start.x).powi(2) + (end.y - start.y).powi(2);
    dot >= -TOL && dot <= length2 + TOL
}

pub(crate) fn point_in_polygon(point: Point, polygon: &[Point]) -> bool {
    if polygon.len() < 3 { return false; }
    let mut inside = false;
    for index in 0..polygon.len() {
        let first = polygon[index];
        let second = polygon[(index + 1) % polygon.len()];
        if point_on_segment(point, first, second) { return true; }
        if (first.y > point.y) != (second.y > point.y) {
            let crossing = first.x + (point.y - first.y) * (second.x - first.x) / (second.y - first.y);
            if point.x < crossing { inside = !inside; }
        }
    }
    inside
}

pub(crate) fn segment_intersections(a: Point, b: Point, c: Point, d: Point) -> Vec<Point> {
    let (rx, ry) = (b.x - a.x, b.y - a.y);
    let (sx, sy) = (d.x - c.x, d.y - c.y);
    let denominator = rx * sy - ry * sx;
    let (qx, qy) = (c.x - a.x, c.y - a.y);
    if denominator.abs() <= TOL { return Vec::new(); }
    let t = (qx * sy - qy * sx) / denominator;
    let u = (qx * ry - qy * rx) / denominator;
    if (-TOL..=1.0 + TOL).contains(&t) && (-TOL..=1.0 + TOL).contains(&u) {
        vec![Point::new(a.x + t * rx, a.y + t * ry)]
    } else { Vec::new() }
}

pub(crate) fn primitive_intersects_segment(primitive: &Primitive, start: Point, end: Point) -> bool {
    match primitive {
        Primitive::Circle { center, radius, stroke } => {
            point_segment_distance(*center, start, end) < *radius + stroke / 2.0 - TOL
        }
        Primitive::Polyline { points, closed, .. } => {
            if *closed && point_in_polygon(midpoint(start, end), points) { return true; }
            let edge_count = if *closed { points.len() } else { points.len().saturating_sub(1) };
            (0..edge_count).any(|index| {
                let a = points[index];
                let b = points[(index + 1) % points.len()];
                !segment_intersections(start, end, a, b).is_empty()
            })
        }
    }
}

fn point_segment_distance(point: Point, start: Point, end: Point) -> f64 {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let length2 = dx * dx + dy * dy;
    if length2 <= TOL * TOL { return point.distance(start); }
    let projection = (((point.x - start.x) * dx + (point.y - start.y) * dy) / length2).clamp(0.0, 1.0);
    let closest = Point::new(start.x + projection * dx, start.y + projection * dy);
    point.distance(closest)
}

pub(crate) fn midpoint(first: Point, second: Point) -> Point {
    Point::new((first.x + second.x) / 2.0, (first.y + second.y) / 2.0)
}

pub(crate) fn line_hits_rect(start: Point, end: Point, rect: Rect, strict: bool) -> bool {
    if (start.y - end.y).abs() <= TOL {
        let inside_y = if strict {
            start.y > rect.min.y + TOL && start.y < rect.max.y - TOL
        } else {
            start.y >= rect.min.y - TOL && start.y <= rect.max.y + TOL
        };
        let overlap = end.x.max(start.x).min(rect.max.x) - end.x.min(start.x).max(rect.min.x);
        inside_y && overlap > TOL
    } else if (start.x - end.x).abs() <= TOL {
        let inside_x = if strict {
            start.x > rect.min.x + TOL && start.x < rect.max.x - TOL
        } else {
            start.x >= rect.min.x - TOL && start.x <= rect.max.x + TOL
        };
        let overlap = end.y.max(start.y).min(rect.max.y) - end.y.min(start.y).max(rect.min.y);
        inside_x && overlap > TOL
    } else { false }
}
