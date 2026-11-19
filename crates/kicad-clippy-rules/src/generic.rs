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
    Affine, Arc as CurveArc, CubicBez as CurveCubicBez, Line as CurveLine, PathEl,
    PathSeg as CurvePathSeg, Point as CurvePoint, Rect as CurveRect, Shape, Triangle, Vec2,
    flatten as flatten_curve_path,
};
use vector_text_core::{Point as VectorPoint, Renderer};
use vector_text_newstroke::NewstrokeRenderer;

use crate::{RuleContext, RulePass};

/// Disjoint-set forest used to resolve connected physical wire components.
#[derive(Debug)]
struct DisjointSet {
    /// Parent index for each element; a root is identified by a parent equal to its own index.
    parents: Vec<usize>,
    /// Number of elements in each root's component, used for union by size.
    sizes: Vec<usize>,
}

impl DisjointSet {
    /// Creates one disjoint set for each index in the supplied range.
    ///
    /// # Arguments
    ///
    /// * `length` — Number of elements to initialize.
    ///
    /// # Returns
    ///
    /// A disjoint-set forest containing `length` independent elements.
    fn new(length: usize) -> Self {
        Self {
            parents: (0..length).collect(),
            sizes: vec![1; length],
        }
    }

    /// Merges the sets containing the two supplied element indices.
    ///
    /// # Arguments
    ///
    /// * `first` — Index of the first element whose disjoint set is to be merged with the set containing the second element.
    /// * `second` — Index of the second element whose disjoint set is to be merged with the set containing the first element.
    fn union(&mut self, first: usize, second: usize) {
        let mut first_root = self.find(first);
        let mut second_root = self.find(second);
        if first_root == second_root {
            return;
        }
        if self.sizes[first_root] < self.sizes[second_root] {
            std::mem::swap(&mut first_root, &mut second_root);
        }
        self.parents[second_root] = first_root;
        self.sizes[first_root] += self.sizes[second_root];
    }

    /// Returns the representative root for the supplied element index.
    ///
    /// # Arguments
    ///
    /// * `index` — Element index whose representative is requested.
    ///
    /// # Returns
    ///
    /// The representative root index for the element's current set.
    fn find(&mut self, index: usize) -> usize {
        let parent = self.parents[index];
        if parent != index {
            let root = self.find(parent);
            self.parents[index] = root;
        }
        self.parents[index]
    }
}

/// Coordinate equality tolerance used for schematic geometry, in millimetres.
pub const GEOMETRY_TOLERANCE_MM: f64 = 0.001;
/// Additional clearance applied when testing wire-to-obstacle spacing, in millimetres.
pub const STROKE_CLEARANCE_MARGIN_MM: f64 = 0.01;
/// Floating-point epsilon used to bound geometric numerical error.
const GEOMETRY_NUMERIC_EPSILON: f64 = 1.0e-9;
/// Number of degrees in a complete turn.
const FULL_ROTATION_DEGREES: f64 = 360.0;
/// Number of degrees in a quarter turn.
const QUARTER_ROTATION_DEGREES: f64 = 90.0;
/// Number of degrees in a half turn.
const HALF_ROTATION_DEGREES: f64 = 180.0;
/// Number of degrees in a three-quarter turn.
const THREE_QUARTER_ROTATION_DEGREES: f64 = 270.0;
/// Supported cardinal rotation angles in degrees.
const CARDINAL_ROTATION_DEGREES: [f64; 4] = [
    0.0,
    QUARTER_ROTATION_DEGREES,
    HALF_ROTATION_DEGREES,
    THREE_QUARTER_ROTATION_DEGREES,
];
/// Supported cardinal angles including the equivalent full-turn endpoint.
const CARDINAL_ROTATION_DEGREES_WITH_WRAP: [f64; 5] = [
    0.0,
    QUARTER_ROTATION_DEGREES,
    HALF_ROTATION_DEGREES,
    THREE_QUARTER_ROTATION_DEGREES,
    FULL_ROTATION_DEGREES,
];
/// Default bitmap resolution when metadata does not declare a resolution, in pixels per inch.
const DEFAULT_BITMAP_IMAGE_PPI: u32 = 300;
/// Floating-point form of the default bitmap resolution, in pixels per inch.
const DEFAULT_BITMAP_IMAGE_PPI_F64: f64 = 300.0;
/// `KiCad` format version that introduced the legacy bitmap resolution behavior.
const LEGACY_BITMAP_PPI_FORMAT_VERSION: u32 = 20_230_121;
/// `KiCad` format version that migrated bitmap resolution handling.
const MIGRATED_BITMAP_PPI_FORMAT_VERSION: u32 = 20_260_623;
/// Millimetres in one inch.
const MILLIMETRES_PER_INCH: f64 = 25.4;
/// Ten times the millimetres-per-inch conversion factor.
const TENTHS_OF_MILLIMETRE_PER_INCH: u64 = 254;
/// Centimetres in one metre.
const CENTIMETRES_PER_METRE: u32 = 100;
/// Minimum pixels per metre required before deriving a pixels-per-inch value.
const MINIMUM_PIXELS_PER_METRE_FOR_PPI: u32 = CENTIMETRES_PER_METRE;
/// Maximum accepted width or height of an embedded bitmap, in pixels.
const MAX_BITMAP_DIMENSION_PIXELS: u32 = 1_000_000;
/// Minimum triangle area accepted for stable circular-arc reconstruction, in square millimetres.
const MINIMUM_CURVE_TRIANGLE_AREA_MM2: f64 = 2.5e-10;
/// Number of subdivision intervals used to sample an elliptical arc.
const ELLIPSE_ARC_SUBDIVISION_STEPS: u8 = 20;
/// Millimetres represented by one mil.
pub const MILLIMETRES_PER_MIL: f64 = 0.0254;
/// Dimensionless multiplier representing one half.
pub(super) const HALF_FACTOR: f64 = 0.5;
/// Factor used when a dimension applies symmetrically to both sides of an object.
pub(super) const TWO_SIDED_FACTOR: f64 = 2.0;
/// Default stroke width for embedded symbol primitives, in millimetres.
const DEFAULT_SYMBOL_PRIMITIVE_STROKE_WIDTH_MM: f64 = 0.254;
/// Full fixed X-marker dimension specified by KiCad, in mils.
const NO_CONNECT_MARKER_SIZE_MILS: f64 = 48.0;
/// Minimum extra width required by an unbordered single-line annotation, in millimetres.
const UNBORDERED_TEXT_BOX_EXTRA_WIDTH_MM: f64 = 10.16;
/// Required equal inset on each side of a serialized KiCad text box, in millimetres.
const TEXT_BOX_EQUAL_PADDING_MM: f64 = 1.27;
/// Width factor used to leave room for rendered glyph overhang.
const TEXT_GLYPH_WIDTH_SAFETY_FACTOR: f64 = 1.30;
/// Line-height factor used when estimating multiline Newstroke text bounds.
const TEXT_LINE_HEIGHT_SAFETY_FACTOR: f64 = 1.55;
/// Floating-point round-off bound measured in machine epsilons.
const FLOATING_POINT_ROUNDOFF_ULPS: f64 = 64.0;
/// Bounds expansion applied across both sides of a rectangle, in millimetres.
const TWO_SIDED_GEOMETRY_TOLERANCE_MM: f64 = TWO_SIDED_FACTOR * GEOMETRY_TOLERANCE_MM;
/// Minimum JPEG JFIF application-segment payload length, in bytes.
const JPEG_JFIF_MINIMUM_SEGMENT_BYTES: usize = 12;
/// Two-dimensional point used for schematic geometry; its `x` and `y` coordinates are measured in millimetres.
pub type Point = Coord<f64>;
/// One schematic line segment with a source span for diagnostics.
#[derive(Clone, Copy, Debug)]
pub struct Segment {
    /// First endpoint of the segment in schematic coordinates.
    pub start: Point,
    /// Second endpoint of the segment in schematic coordinates.
    pub end: Point,
    /// Original source byte span of the wire expression.
    pub span: ByteSpan,
}

/// Exact circular geometry reconstructed from `KiCad`'s start/mid/end arc points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SchematicArcFact {
    /// Circle center in schematic millimetres.
    pub center_mm: (f64, f64),
    /// Circle radius in millimetres.
    pub radius_mm: f64,
    /// Angle of the start point around the circle, in radians.
    pub start_angle_rad: f64,
    /// Directed angular sweep from the start point to the end point, in radians.
    pub sweep_rad: f64,
}
/// Axis-aligned rectangle in schematic coordinates, measured in millimetres.
#[derive(Clone, Copy, Debug)]
struct Bounds {
    /// Left coordinate of the rectangle in millimetres.
    x: f64,
    /// Top coordinate of the rectangle in millimetres.
    y: f64,
    /// Rectangle width in millimetres.
    width: f64,
    /// Rectangle height in millimetres.
    height: f64,
}
/// Placed symbol identity and origin extracted from the schematic.
#[derive(Clone, Debug)]
pub struct ComponentFact {
    /// Placed component reference designator.
    pub reference: String,
    /// KiCad UUID of the placed symbol, when available.
    pub uuid: Option<String>,
    /// Symbol origin in millimetres, or `None` when no origin could be resolved.
    pub position_mm: Option<(f64, f64)>,
}
/// Native KiCad group name, UUID, and member UUIDs.
#[derive(Clone, Debug)]
pub struct SchematicGroupFact {
    /// Native KiCad group name.
    pub name: String,
    /// UUID identifying the native group.
    pub uuid: String,
    /// UUIDs of the schematic items declared as group members.
    pub member_uuids: BTreeSet<String>,
}
/// Rectangular sheet frame identified by its KiCad UUID.
#[derive(Clone, Debug)]
pub struct SchematicFrameFact {
    /// UUID identifying the rectangular frame object.
    pub uuid: String,
    /// Frame rectangle as `(left, top, right, bottom)`, in millimetres.
    pub bounds_mm: (f64, f64, f64, f64),
}
/// Wire endpoint coordinates and source offset used for diagnostics and per-wire lookups.
#[derive(Clone, Copy, Debug)]
pub struct SchematicWireFact {
    /// First wire endpoint in millimetres.
    pub start_mm: (f64, f64),
    /// Second wire endpoint in millimetres.
    pub end_mm: (f64, f64),
    /// Byte offset of the wire expression in the original schematic source.
    pub span_start: usize,
}
/// Placed symbol pin reference, number, and schematic coordinate.
#[derive(Clone, Debug)]
pub struct SchematicPinFact {
    /// Reference designator of the placed symbol owning this pin.
    pub reference: String,
    /// Pin number as serialized by the symbol library.
    pub number: String,
    /// Pin anchor coordinate in schematic millimetres.
    pub position_mm: (f64, f64),
}
/// Visible symbol field and its estimated text bounds.
#[derive(Clone, Debug)]
pub struct SchematicFieldFact {
    /// Reference designator of the symbol owning the field.
    pub reference: String,
    /// Field name, typically `Reference` or `Value`.
    pub name: String,
    /// Estimated visible glyph bounds in schematic millimetres.
    pub bounds_mm: (f64, f64, f64, f64),
}
/// Transformed symbol or sheet primitive geometry used by collision and containment checks.
#[derive(Clone, Debug)]
pub struct SchematicPrimitiveFact {
    /// Reference designator of the placed symbol owning this primitive, when it is embedded geometry.
    pub reference: String,
    /// Transformed path points in schematic millimetres.
    pub points_mm: Vec<(f64, f64)>,
    /// Whether the primitive path encloses a filled interior.
    pub closed: bool,
    /// Circle center and radius when the primitive is circular.
    pub circle_mm: Option<(f64, f64, f64)>,
    /// Reconstructed circular-arc geometry when the primitive is an arc.
    pub arc_mm: Option<SchematicArcFact>,
    /// Four transformed control points when the primitive is a cubic Bézier.
    pub bezier_mm: Option<[(f64, f64); 4]>,
    /// Declared primitive stroke width in millimetres.
    pub stroke_width_mm: f64,
}

/// A hierarchical sheet frame and its connection pins.
#[derive(Clone, Debug)]
pub struct SchematicSheetFact {
    /// UUID identifying the hierarchical sheet.
    pub uuid: String,
    /// Displayed sheet name.
    pub name: String,
    /// Sheet frame and body geometry.
    pub primitive: SchematicPrimitiveFact,
    /// Placed sheet pins with their coordinates and numbers.
    pub pins: Vec<SchematicPinFact>,
}

/// A schematic bus line or wire-to-bus entry used as a wire obstacle.
#[derive(Clone, Debug)]
pub struct SchematicBusObstacleFact {
    /// Bus primitive kind used to select the intersection model.
    pub kind: String,
    /// Bus path geometry in schematic coordinates.
    pub primitive: SchematicPrimitiveFact,
    /// Sheet or symbol pins associated with the bus obstacle.
    pub pins: Vec<SchematicPinFact>,
}

/// A fixed-size X-shaped schematic no-connect marker.
#[derive(Clone, Debug)]
pub struct SchematicNoConnectFact {
    /// Reference designator of the placed symbol carrying the marker.
    pub reference: String,
    /// Center of the no-connect X marker in millimetres.
    pub center_mm: (f64, f64),
    /// Half of the fixed marker width, in millimetres.
    pub half_size_mm: f64,
}
/// Transformed text or text-box obstacle extracted from a library symbol.
#[derive(Clone, Debug)]
pub struct SchematicSymbolTextFact {
    /// Reference designator of the owning placed symbol.
    pub reference: String,
    /// Library text object kind, such as text or text_box.
    pub kind: String,
    /// Decoded text content.
    pub text: String,
    /// Transformed visible glyph or text-box bounds in schematic millimetres.
    pub bounds_mm: (f64, f64, f64, f64),
}
/// Label kind, text, anchor, and optional visible glyph bounds.
#[derive(Clone, Debug)]
pub struct SchematicLabelFact {
    /// Label kind, such as local, global, or hierarchical.
    pub kind: String,
    /// Decoded label text.
    pub name: String,
    /// Label anchor coordinate in schematic millimetres.
    pub position_mm: (f64, f64),
    /// Estimated visible glyph bounds, when they can be resolved.
    pub bounds_mm: Option<(f64, f64, f64, f64)>,
}
/// Schematic text-box identity, content, and optional rectangular bounds.
#[derive(Clone, Debug)]
pub struct TextBoxFact {
    /// UUID identifying the text-box object.
    pub uuid: String,
    /// Decoded text content.
    pub text: String,
    /// Text-box rectangle in schematic millimetres, when geometry is valid.
    pub bounds_mm: Option<(f64, f64, f64, f64)>,
}

/// A free-standing schematic text object and its estimated visible glyph bounds.
#[derive(Clone, Debug)]
pub struct SchematicTextFact {
    /// Decoded standalone schematic text.
    pub text: String,
    /// Estimated visible text glyph bounds, when available.
    pub bounds_mm: Option<(f64, f64, f64, f64)>,
}

/// A top-level vector graphic and its approximate sheet-space bounds.
#[derive(Clone, Debug)]
pub struct SchematicGraphicFact {
    /// Graphic primitive kind.
    pub kind: String,
    /// KiCad UUID of the graphic, when present.
    pub uuid: Option<String>,
    /// Stroke-expanded axis-aligned bounds, when calculable.
    pub bounds_mm: Option<(f64, f64, f64, f64)>,
    /// Ordered transformed path points in schematic millimetres.
    pub points_mm: Vec<(f64, f64)>,
    /// Whether the path has a closed interior.
    pub closed: bool,
    /// Circle center and radius when this is a circular primitive.
    pub circle_mm: Option<(f64, f64, f64)>,
    /// Reconstructed arc geometry when this is a circular arc.
    pub arc_mm: Option<SchematicArcFact>,
    /// Cubic Bézier control points when this is a Bézier primitive.
    pub bezier_mm: Option<[(f64, f64); 4]>,
    /// Declared graphic stroke width in millimetres.
    pub stroke_width_mm: f64,
}

/// A bitmap's centre-derived envelope and orientation-aware rectangular footprint.
#[derive(Clone, Copy, Debug)]
struct BitmapImageGeometry {
    /// Axis-aligned bounds enclosing the rotated image quadrilateral, in millimetres.
    bounds_mm: (f64, f64, f64, f64),
    /// Four rotated image corners in sheet coordinates.
    corners_mm: [(f64, f64); 4],
}

/// An embedded bitmap image with bounds derived from its pixel size, PPI and scale.
#[derive(Clone, Debug)]
pub struct SchematicImageFact {
    /// KiCad UUID of the embedded image, when present.
    pub uuid: Option<String>,
    /// Resolved image bounds in millimetres, or `None` when its encoding is unsupported.
    pub bounds_mm: Option<(f64, f64, f64, f64)>,
    /// Orientation-aware image corners, or `None` when its geometry cannot be resolved.
    pub corners_mm: Option<[(f64, f64); 4]>,
}
/// Normalized schematic facts collected once and shared by the built-in rule passes.
#[derive(Clone, Debug, Default)]
pub struct SchematicFacts {
    /// Placed component references and origins.
    pub components: Vec<ComponentFact>,
    /// Top-level annotation text boxes and their geometry.
    pub text_boxes: Vec<TextBoxFact>,
    /// Top-level standalone text and estimated glyph bounds.
    pub texts: Vec<SchematicTextFact>,
    /// Top-level vector graphics and their path/stroke geometry.
    pub graphics: Vec<SchematicGraphicFact>,
    /// Embedded bitmap image geometry.
    pub images: Vec<SchematicImageFact>,
    /// Native KiCad group declarations and member UUIDs.
    pub groups: Vec<SchematicGroupFact>,
    /// Rectangular sheet frames.
    pub frames: Vec<SchematicFrameFact>,
    /// Wire segments with endpoint coordinates and source offsets.
    pub wires: Vec<SchematicWireFact>,
    /// Placed symbol pins with reference, number, and position.
    pub pins: Vec<SchematicPinFact>,
    /// Visible Reference/Value fields and estimated glyph bounds.
    pub visible_fields: Vec<SchematicFieldFact>,
    /// Selected-unit embedded symbol primitives transformed to sheet coordinates.
    pub symbol_primitives: Vec<SchematicPrimitiveFact>,
    /// Hierarchical sheet frames and sheet pins.
    pub sheets: Vec<SchematicSheetFact>,
    /// Bus line geometry and associated pins.
    pub bus_obstacles: Vec<SchematicBusObstacleFact>,
    /// Fixed-size no-connect marker locations.
    pub no_connects: Vec<SchematicNoConnectFact>,
    /// Transformed library-symbol text and text boxes.
    pub symbol_texts: Vec<SchematicSymbolTextFact>,
    /// Schematic labels with kind, text, anchors, and glyph bounds.
    pub labels: Vec<SchematicLabelFact>,
    /// Explicit wire junction coordinates.
    pub junctions: Vec<(f64, f64)>,
    /// Whether schematic-local labels, directives, or rule areas assign netclasses.
    pub has_netclass_assignments: bool,
}

impl SchematicFacts {
    /// Extracts normalized component, pin, wire, label, graphic, and annotation facts from a parsed schematic.
    ///
    /// # Arguments
    ///
    /// * `document` — Parsed schematic document whose S-expression tree and source spans are used for fact extraction.
    ///
    /// # Returns
    ///
    /// A `SchematicFacts` value containing the normalized schematic objects and their geometry.
    #[must_use]
    pub fn from_document(document: &SchematicDocument) -> Self {
        let Some(root) = document.cst().nodes.first() else {
            return Self::default();
        };
        let segments = collect_segments(root);
        let junctions = collect_points(root, "junction");
        Self::from_document_with_geometry(document, &segments, &junctions)
    }
    /// Builds schematic facts while reusing the caller's already-collected wire segments and junction points.
    ///
    /// # Arguments
    ///
    /// * `document` — Parsed schematic document whose S-expression tree and source spans are used for fact extraction.
    /// * `segments` — Parsed wire segments used to determine geometry, topology, or obstacle intersections.
    /// * `junctions` — Explicit schematic junction coordinates used when resolving wire connectivity.
    ///
    /// # Returns
    ///
    /// A `SchematicFacts` value containing the normalized schematic objects and their geometry.
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
            texts: collect_plain_text_facts(root),
            graphics: collect_sheet_graphic_facts(root),
            images: collect_sheet_image_facts(root),
            groups: collect_group_facts(root),
            frames: collect_frame_facts(root),
            wires: segments
                .iter()
                .map(|segment| SchematicWireFact {
                    start_mm: (segment.start.x, segment.start.y),
                    end_mm: (segment.end.x, segment.end.y),
                    span_start: segment.span.start,
                })
                .collect(),
            pins: collect_pin_facts(root),
            visible_fields: collect_symbol_field_facts(root),
            symbol_primitives: collect_symbol_primitive_facts(root),
            sheets: collect_sheet_facts(root),
            bus_obstacles: collect_bus_obstacle_facts(root),
            no_connects: collect_no_connect_facts(root),
            symbol_texts: collect_symbol_text_facts(root),
            labels: collect_label_facts(root),
            junctions: junctions.iter().map(|point| (point.x, point.y)).collect(),
            has_netclass_assignments: schematic_has_graphical_netclass_assignments(root),
        }
    }
}
/// Extracts placed symbol references, UUIDs, and origins from the schematic.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// Normalized component facts records or coordinates derived from the input.
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
/// Extracts text-box identifiers, text content, and available geometric bounds.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// Normalized text box facts records or coordinates derived from the input.
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
/// Checks whether a table cell uses a supported cardinal text rotation.
///
/// # Arguments
///
/// * `cell` — Table cell node whose style or text rotation is being checked.
///
/// # Returns
///
/// Whether the cell's serialized text angle is one of the supported cardinal rotations.
fn table_cell_text_rotation_is_supported(cell: &Node) -> bool {
    let Some(angle) = child_list(cell, "at")
        .and_then(list_items)
        .and_then(|items| items.get(3))
        .and_then(number)
    else {
        return false;
    };
    if !angle.is_finite() {
        return false;
    }
    let normalized = angle.rem_euclid(FULL_ROTATION_DEGREES);
    CARDINAL_ROTATION_DEGREES
        .iter()
        .any(|supported| (normalized - supported).abs() <= GEOMETRY_TOLERANCE_MM)
}
/// Checks whether all populated cells in a table use supported cardinal text rotations.
///
/// # Arguments
///
/// * `table` — Table node whose dimensions, cells, strokes, and orientation are being resolved.
///
/// # Returns
///
/// Whether every cell in the table uses a supported cardinal text rotation.
fn table_text_rotations_are_supported(table: &Node) -> bool {
    let Some(cells) = child_list(table, "cells") else {
        return false;
    };
    let mut cell_iterator = children(cells, "table_cell");
    cell_iterator
        .next()
        .is_some_and(table_cell_text_rotation_is_supported)
        && cell_iterator.all(table_cell_text_rotation_is_supported)
}
/// Extracts top-level plain-text objects while excluding text represented by supported table cells.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// Normalized plain text facts records or coordinates derived from the input.
fn collect_plain_text_facts(root: &Node) -> Vec<SchematicTextFact> {
    let table_cells = children(root, "table")
        .filter(|table| table_text_rotations_are_supported(table))
        .flat_map(|table| {
            child_list(table, "cells")
                .into_iter()
                .flat_map(|cells| children(cells, "table_cell"))
        });
    children(root, "text")
        .chain(table_cells)
        .filter_map(|node| {
            let text = list_items(node)
                .and_then(|items| items.get(1))
                .and_then(atom_value)?;
            if text.trim().is_empty() {
                return None;
            }
            let bounds_mm = text_visual_bounds(text, node).map(|bounds| {
                (
                    bounds.x,
                    bounds.y,
                    bounds.x + bounds.width,
                    bounds.y + bounds.height,
                )
            });
            Some(SchematicTextFact {
                text: text.to_owned(),
                bounds_mm,
            })
        })
        .collect()
}
/// Extracts top-level sheet primitives and their bounds, paths, and stroke widths.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// Normalized sheet graphic facts records or coordinates derived from the input.
fn collect_sheet_graphic_facts(root: &Node) -> Vec<SchematicGraphicFact> {
    let mut graphics = Vec::new();
    for kind in [
        "rectangle",
        "polyline",
        "polygon",
        "circle",
        "arc",
        "bezier",
        "ellipse",
        "ellipse_arc",
    ] {
        for node in children(root, kind) {
            let uuid = child_list(node, "uuid")
                .and_then(list_items)
                .and_then(|items| items.get(1))
                .and_then(atom_value)
                .map(str::to_owned);
            let stroke_width_mm = symbol_primitive_stroke_width(node);
            let circle_mm = if kind == "circle" {
                let center = child_list(node, "center").and_then(list_items);
                let radius = child_list(node, "radius")
                    .and_then(list_items)
                    .and_then(|items| items.get(1))
                    .and_then(number)
                    .map(f64::abs);
                center.and_then(|center| {
                    Some((number(center.get(1)?)?, number(center.get(2)?)?, radius?))
                })
            } else {
                None
            };
            let arc_mm = if kind == "arc" {
                arc_primitive_geometry(node)
            } else {
                None
            };
            let bezier_mm = if kind == "bezier" {
                bezier_primitive_control_points(node)
            } else {
                None
            };
            let mut points_mm = if kind == "circle" {
                Vec::new()
            } else {
                match kind {
                    "rectangle" => rectangle_primitive_points(node),
                    "polyline" | "polygon" => polyline_primitive_points(node),
                    "arc" => arc_primitive_points(node),
                    "bezier" => bezier_primitive_points(node),
                    "ellipse" => ellipse_primitive_points(node, false),
                    "ellipse_arc" => ellipse_primitive_points(node, true),
                    _ => None,
                }
                .unwrap_or_default()
            };
            let repeated_endpoint = points_mm.len() > 2
                && (points_mm[0].0 - points_mm[points_mm.len() - 1].0).abs()
                    <= GEOMETRY_TOLERANCE_MM
                && (points_mm[0].1 - points_mm[points_mm.len() - 1].1).abs()
                    <= GEOMETRY_TOLERANCE_MM;
            let closed = kind == "rectangle"
                || kind == "polygon"
                || kind == "ellipse"
                || (kind != "ellipse_arc" && repeated_endpoint);
            if closed && repeated_endpoint {
                points_mm.pop();
            }
            let bounds_mm = sheet_graphic_bounds(node, kind);
            graphics.push(SchematicGraphicFact {
                kind: kind.to_owned(),
                uuid,
                bounds_mm,
                points_mm,
                closed,
                circle_mm,
                arc_mm,
                bezier_mm,
                stroke_width_mm,
            });
        }
    }
    graphics
}
/// Defines the default table stroke width mm constant.
const DEFAULT_TABLE_STROKE_WIDTH_MM: f64 = 0.1524;
/// Resolved table origin, rotation, row/column sizes, and cell spans.
#[derive(Clone, Debug)]
struct SchematicTableLayout {
    /// Table UUID, when KiCad serializes one.
    uuid: Option<String>,
    /// Table insertion point in schematic millimetres.
    origin_mm: (f64, f64),
    /// Cardinal table rotation as a number of quarter turns.
    rotation_quadrants: u8,
    /// Column widths in millimetres.
    column_widths_mm: Vec<f64>,
    /// Row heights in millimetres.
    row_heights_mm: Vec<f64>,
    /// Row/column spans for merged cells in row-major order.
    cell_spans: Vec<(usize, usize)>,
}
/// Parses an S-expression atom as a non-negative integer.
///
/// # Arguments
///
/// * `node` — S-expression node whose properties, coordinates, or geometry are being decoded.
///
/// # Returns
///
/// The parsed non-negative `usize` value, or `None` when the atom is missing or is not an unsigned integer.
fn nonnegative_integer(node: &Node) -> Option<usize> {
    atom_value(node)?.parse::<usize>().ok()
}
/// Parses a strictly positive integer from an S-expression child.
///
/// # Arguments
///
/// * `node` — S-expression node whose properties, coordinates, or geometry are being decoded.
///
/// # Returns
///
/// The parsed integer when it is greater than zero; zero, missing, or malformed values return `None`.
fn positive_integer(node: &Node) -> Option<usize> {
    nonnegative_integer(node).filter(|value| *value > 0)
}
/// Reads a named table dimension list and converts its entries to numeric millimetre values.
///
/// # Arguments
///
/// * `table` — Table node whose dimensions, cells, strokes, and orientation are being resolved.
/// * `name` — S-expression property name, label, or lookup key to read.
///
/// # Returns
///
/// The finite, strictly positive numeric entries from the named row-height or column-width list, in millimetres; `None` if the list is invalid.
fn table_dimension_values(table: &Node, name: &str) -> Option<Vec<f64>> {
    let values = child_list(table, name).and_then(list_items)?;
    let values: Option<Vec<_>> = values.iter().skip(1).map(number).collect();
    let values = values?;
    values
        .iter()
        .all(|value| value.is_finite() && *value > 0.0)
        .then_some(values)
}
/// Reads a cell's row/column span, defaulting to a single cell when no span is serialized.
///
/// # Arguments
///
/// * `cell` — Table cell node whose style or text rotation is being checked.
///
/// # Returns
///
/// The `(row_span, column_span)` pair; `(1, 1)` is returned when the cell has no serialized span.
fn table_cell_span(cell: &Node) -> Option<(usize, usize)> {
    let Some(span) = child_list(cell, "span") else {
        return Some((1, 1));
    };
    let span = list_items(span)?;
    Some((
        nonnegative_integer(span.get(1)?)?,
        nonnegative_integer(span.get(2)?)?,
    ))
}
/// Converts a finite cardinal table angle into the corresponding quarter-turn count.
///
/// # Arguments
///
/// * `angle` — Rotation angle in degrees as serialized by KiCad.
///
/// # Returns
///
/// The quarter-turn count for a supported cardinal angle, or `None` when the angle is non-finite or not a quarter-turn.
fn cardinal_table_rotation_quadrants(angle: f64) -> Option<u8> {
    if !angle.is_finite() {
        return None;
    }
    let normalized = angle.rem_euclid(FULL_ROTATION_DEGREES);
    match CARDINAL_ROTATION_DEGREES_WITH_WRAP
        .iter()
        .position(|supported| (normalized - supported).abs() <= GEOMETRY_TOLERANCE_MM)?
    {
        0 | 4 => Some(0),
        1 => Some(1),
        2 => Some(2),
        3 => Some(3),
        _ => None,
    }
}
/// Builds a table layout from its origin, rotation, column widths, row heights, and merged-cell spans.
///
/// # Arguments
///
/// * `table` — Table node whose dimensions, cells, strokes, and orientation are being resolved.
///
/// # Returns
///
/// A validated table layout containing its grid dimensions, row/column positions, and cell spans; `None` when the serialized layout is inconsistent.
fn schematic_table_layout(table: &Node) -> Option<SchematicTableLayout> {
    let columns = child_list(table, "column_count")
        .and_then(list_items)
        .and_then(|items| items.get(1))
        .and_then(positive_integer)?;
    let cells_node = child_list(table, "cells")?;
    let cells: Vec<_> = children(cells_node, "table_cell").collect();
    if cells.is_empty() || cells.len() % columns != 0 {
        return None;
    }
    let rows = cells.len() / columns;
    let column_widths_mm = table_dimension_values(table, "column_widths")?;
    let row_heights_mm = table_dimension_values(table, "row_heights")?;
    if column_widths_mm.len() != columns
        || row_heights_mm.len() != rows
        || !column_widths_mm.iter().sum::<f64>().is_finite()
        || !row_heights_mm.iter().sum::<f64>().is_finite()
    {
        return None;
    }

    let at = child_list(cells[0], "at").and_then(list_items)?;
    let rotation_quadrants = cardinal_table_rotation_quadrants(number(at.get(3)?)?)?;
    let origin_mm = (number(at.get(1)?)?, number(at.get(2)?)?);
    if !origin_mm.0.is_finite() || !origin_mm.1.is_finite() {
        return None;
    }

    let cell_spans: Option<Vec<_>> = cells.iter().map(|cell| table_cell_span(cell)).collect();
    let cell_spans = cell_spans?;
    for (index, (column_span, row_span)) in cell_spans.iter().copied().enumerate() {
        let row = index / columns;
        let column = index % columns;
        let column_end = column.checked_add(column_span)?;
        let row_end = row.checked_add(row_span)?;
        if (column_span > 0 && column_end > columns) || (row_span > 0 && row_end > rows) {
            return None;
        }
    }

    let uuid = child_list(table, "uuid")
        .and_then(list_items)
        .and_then(|items| items.get(1))
        .and_then(atom_value)
        .map(str::to_owned);

    let layout = SchematicTableLayout {
        uuid,
        origin_mm,
        rotation_quadrants,
        column_widths_mm,
        row_heights_mm,
        cell_spans,
    };
    let column_offsets = cumulative_positions(0.0, &layout.column_widths_mm);
    let row_offsets = cumulative_positions(0.0, &layout.row_heights_mm);
    for (index, cell) in cells.iter().enumerate() {
        let cell_at = child_list(cell, "at").and_then(list_items)?;
        let cell_rotation = number(cell_at.get(3)?)?;
        if cardinal_table_rotation_quadrants(cell_rotation) != Some(rotation_quadrants) {
            return None;
        }
        let row = index / columns;
        let column = index % columns;
        let expected = table_grid_point(&layout, column_offsets[column], row_offsets[row]);
        let actual = (number(cell_at.get(1)?)?, number(cell_at.get(2)?)?);
        if (actual.0 - expected.0).abs() > GEOMETRY_TOLERANCE_MM
            || (actual.1 - expected.1).abs() > GEOMETRY_TOLERANCE_MM
        {
            return None;
        }
    }

    Some(layout)
}
/// Maps a table-local grid offset into schematic coordinates after applying the table's cardinal rotation.
///
/// # Arguments
///
/// * `layout` — Resolved table layout containing its origin, rotation, grid dimensions, and cell spans.
/// * `x_offset` — Horizontal displacement in the table's unrotated local coordinate system, in millimetres.
/// * `y_offset` — Vertical displacement in the table's unrotated local coordinate system, in millimetres.
///
/// # Returns
///
/// The transformed schematic coordinate in millimetres.
fn table_grid_point(layout: &SchematicTableLayout, x_offset: f64, y_offset: f64) -> (f64, f64) {
    let (x, y) = match layout.rotation_quadrants {
        0 => (x_offset, y_offset),
        1 => (y_offset, -x_offset),
        2 => (-x_offset, -y_offset),
        _ => (-y_offset, x_offset),
    };
    (layout.origin_mm.0 + x, layout.origin_mm.1 + y)
}
/// Reads a table-style boolean flag and uses the supplied default when the flag is absent.
///
/// # Arguments
///
/// * `group` — Table-style or group node providing the requested option or declared intent.
/// * `name` — S-expression property name, label, or lookup key to read.
/// * `default` — Fallback boolean used when the requested style flag is not serialized.
///
/// # Returns
///
/// The flag's `yes` state, or the supplied default when the property or group is absent.
fn table_flag(group: Option<&Node>, name: &str, default: bool) -> bool {
    group
        .and_then(|group| child_list(group, name))
        .and_then(list_items)
        .and_then(|items| items.get(1))
        .and_then(atom_value)
        .map_or(default, |value| value == "yes")
}
/// Reads a table style's stroke width and returns zero when no positive width is configured.
///
/// # Arguments
///
/// * `group` — Table-style or group node providing the requested option or declared intent.
///
/// # Returns
///
/// The positive stroke width in millimetres, or zero when no width is declared.
fn table_stroke_width_mm(group: Option<&Node>) -> f64 {
    group
        .and_then(|group| child_list(group, "stroke"))
        .and_then(|stroke| child_list(stroke, "width"))
        .and_then(list_items)
        .and_then(|items| items.get(1))
        .and_then(number)
        .filter(|width| width.is_finite() && *width > 0.0)
        .unwrap_or(DEFAULT_TABLE_STROKE_WIDTH_MM)
}
/// Builds cumulative grid coordinates from an origin and a sequence of row or column sizes.
///
/// # Arguments
///
/// * `origin` — Starting coordinate from which cumulative grid positions are measured, in millimetres.
/// * `sizes` — Ordered row heights or column widths, in millimetres.
///
/// # Returns
///
/// Normalized cumulative positions records or coordinates derived from the input.
fn cumulative_positions(origin: f64, sizes: &[f64]) -> Vec<f64> {
    let mut positions = Vec::with_capacity(sizes.len() + 1);
    positions.push(origin);
    for size in sizes {
        positions.push(positions.last().copied().unwrap_or(origin) + size);
    }
    positions
}
/// Creates a graphic fact for one table border or separator segment, including stroke-expanded bounds.
///
/// # Arguments
///
/// * `kind` — Schematic object kind used to select parsing behavior or identify the resulting fact.
/// * `uuid` — Optional KiCad UUID associated with the source object.
/// * `start` — Starting coordinate of the segment or curve, in millimetres.
/// * `end` — Ending coordinate of the segment or curve, in millimetres.
/// * `stroke_width_mm` — Declared primitive stroke width, in millimetres.
///
/// # Returns
///
/// The table line fact result derived from the schematic geometry.
fn table_line_fact(
    kind: &str,
    uuid: Option<&str>,
    start: (f64, f64),
    end: (f64, f64),
    stroke_width_mm: f64,
) -> SchematicGraphicFact {
    let margin = stroke_width_mm.mul_add(HALF_FACTOR, STROKE_CLEARANCE_MARGIN_MM);
    SchematicGraphicFact {
        kind: kind.to_owned(),
        uuid: uuid.map(str::to_owned),
        bounds_mm: Some((
            start.0.min(end.0) - margin,
            start.1.min(end.1) - margin,
            start.0.max(end.0) + margin,
            start.1.max(end.1) + margin,
        )),
        points_mm: vec![start, end],
        closed: false,
        circle_mm: None,
        arc_mm: None,
        bezier_mm: None,
        stroke_width_mm,
    }
}
/// Appends the four outer table-border segments to the extracted graphic facts.
///
/// # Arguments
///
/// * `graphics` — Mutable collection receiving extracted table graphic facts.
/// * `layout` — Resolved table layout containing its origin, rotation, grid dimensions, and cell spans.
/// * `stroke_width_mm` — Declared primitive stroke width, in millimetres.
fn append_table_outline(
    graphics: &mut Vec<SchematicGraphicFact>,
    layout: &SchematicTableLayout,
    stroke_width_mm: f64,
) {
    let right = layout.column_widths_mm.iter().sum::<f64>();
    let bottom = layout.row_heights_mm.iter().sum::<f64>();
    let corners = [
        table_grid_point(layout, 0.0, 0.0),
        table_grid_point(layout, right, 0.0),
        table_grid_point(layout, right, bottom),
        table_grid_point(layout, 0.0, bottom),
    ];
    for index in 0..corners.len() {
        graphics.push(table_line_fact(
            "table border",
            layout.uuid.as_deref(),
            corners[index],
            corners[(index + 1) % corners.len()],
            stroke_width_mm,
        ));
    }
}
/// Appends enabled header, row, and column separator segments while omitting merged-cell boundaries.
///
/// # Arguments
///
/// * `graphics` — Mutable collection receiving extracted table graphic facts.
/// * `layout` — Resolved table layout containing its origin, rotation, grid dimensions, and cell spans.
/// * `header_enabled` — Whether the table header separator is enabled.
/// * `columns_enabled` — Whether column separators are enabled.
/// * `rows_enabled` — Whether row separators are enabled.
/// * `border_width_mm` — Width of the table's external border stroke, in millimetres.
/// * `separator_width_mm` — Width of enabled internal separator strokes, in millimetres.
fn append_table_separators(
    graphics: &mut Vec<SchematicGraphicFact>,
    layout: &SchematicTableLayout,
    header_enabled: bool,
    columns_enabled: bool,
    rows_enabled: bool,
    border_width_mm: f64,
    separator_width_mm: f64,
) {
    let column_positions = cumulative_positions(0.0, &layout.column_widths_mm);
    let row_positions = cumulative_positions(0.0, &layout.row_heights_mm);
    let columns = layout.column_widths_mm.len();
    let rows = layout.row_heights_mm.len();

    for row in 0..rows {
        for column in 0..columns {
            let (column_span, row_span) = layout.cell_spans[row * columns + column];
            if column_span == 0 || row_span == 0 || column + column_span == columns {
                continue;
            }
            let stroke_width_mm = if row == 0 && header_enabled {
                border_width_mm
            } else if columns_enabled {
                separator_width_mm
            } else {
                continue;
            };
            graphics.push(table_line_fact(
                "table separator",
                layout.uuid.as_deref(),
                table_grid_point(
                    layout,
                    column_positions[column + column_span],
                    row_positions[row],
                ),
                table_grid_point(
                    layout,
                    column_positions[column + column_span],
                    row_positions[row + row_span],
                ),
                stroke_width_mm,
            ));
        }
    }

    for row in 0..rows {
        for column in 0..columns {
            let (column_span, row_span) = layout.cell_spans[row * columns + column];
            if column_span == 0 || row_span == 0 || row + row_span == rows {
                continue;
            }
            let stroke_width_mm = if row == 0 && header_enabled {
                border_width_mm
            } else if rows_enabled {
                separator_width_mm
            } else {
                continue;
            };
            graphics.push(table_line_fact(
                "table separator",
                layout.uuid.as_deref(),
                table_grid_point(
                    layout,
                    column_positions[column],
                    row_positions[row + row_span],
                ),
                table_grid_point(
                    layout,
                    column_positions[column + column_span],
                    row_positions[row + row_span],
                ),
                stroke_width_mm,
            ));
        }
    }
}
/// Extracts enabled table borders and separators for tables with a supported layout.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// Normalized sheet table stroke facts records or coordinates derived from the input.
fn collect_sheet_table_stroke_facts(root: &Node) -> Vec<SchematicGraphicFact> {
    let mut graphics = Vec::new();
    for table in children(root, "table") {
        let Some(layout) = schematic_table_layout(table) else {
            continue;
        };
        let border = child_list(table, "border");
        let separators = child_list(table, "separators");
        let border_width_mm = table_stroke_width_mm(border);
        let separator_width_mm = table_stroke_width_mm(separators);
        if table_flag(border, "external", true) {
            append_table_outline(&mut graphics, &layout, border_width_mm);
        }
        append_table_separators(
            &mut graphics,
            &layout,
            table_flag(border, "header", true),
            table_flag(separators, "cols", true),
            table_flag(separators, "rows", true),
            border_width_mm,
            separator_width_mm,
        );
    }
    graphics
}
/// Calculates stroke-expanded bounds for a top-level sheet graphic primitive.
///
/// # Arguments
///
/// * `node` — S-expression node whose properties, coordinates, or geometry are being decoded.
/// * `kind` — Schematic object kind used to select parsing behavior or identify the resulting fact.
///
/// # Returns
///
/// The graphic's axis-aligned sheet bounds expanded by half its stroke width; `None` when the primitive geometry cannot be decoded.
fn sheet_graphic_bounds(node: &Node, kind: &str) -> Option<(f64, f64, f64, f64)> {
    let stroke_margin =
        symbol_primitive_stroke_width(node).mul_add(HALF_FACTOR, STROKE_CLEARANCE_MARGIN_MM);
    if kind == "circle" {
        let center = child_list(node, "center").and_then(list_items)?;
        let radius = child_list(node, "radius")
            .and_then(list_items)
            .and_then(|items| items.get(1))
            .and_then(number)?
            .abs()
            + stroke_margin;
        let x = number(center.get(1)?)?;
        let y = number(center.get(2)?)?;
        return Some((x - radius, y - radius, x + radius, y + radius));
    }
    let points = match kind {
        "rectangle" => rectangle_primitive_points(node)?,
        "polyline" | "polygon" => polyline_primitive_points(node)?,
        "arc" => arc_primitive_points(node)?,
        "bezier" => bezier_primitive_points(node)?,
        "ellipse" => ellipse_primitive_points(node, false)?,
        "ellipse_arc" => ellipse_primitive_points(node, true)?,
        _ => return None,
    };
    let min_x = points
        .iter()
        .map(|point| point.0)
        .fold(f64::INFINITY, f64::min);
    let min_y = points
        .iter()
        .map(|point| point.1)
        .fold(f64::INFINITY, f64::min);
    let max_x = points
        .iter()
        .map(|point| point.0)
        .fold(f64::NEG_INFINITY, f64::max);
    let max_y = points
        .iter()
        .map(|point| point.1)
        .fold(f64::NEG_INFINITY, f64::max);
    (min_x.is_finite() && min_y.is_finite() && max_x.is_finite() && max_y.is_finite()).then_some((
        min_x - stroke_margin,
        min_y - stroke_margin,
        max_x + stroke_margin,
        max_y + stroke_margin,
    ))
}
/// Calculates the orientation-aware corners and axis-aligned bounds of a scaled bitmap image.
///
/// # Arguments
///
/// * `center` — Image or arc center coordinate in schematic millimetres.
/// * `width_mm` — Bitmap width after applying image scale, in millimetres.
/// * `height_mm` — Bitmap height after applying image scale, in millimetres.
/// * `orientation_degrees` — Clockwise image orientation in degrees.
///
/// # Returns
///
/// The four rotated bitmap corners and their enclosing sheet-space bounds, or `None` for non-finite or non-positive dimensions.
fn rotated_bitmap_image_geometry(
    center: Point,
    width_mm: f64,
    height_mm: f64,
    orientation_degrees: f64,
) -> Option<BitmapImageGeometry> {
    if ![center.x, center.y, width_mm, height_mm, orientation_degrees]
        .iter()
        .all(|value| value.is_finite())
        || width_mm <= 0.0
        || height_mm <= 0.0
    {
        return None;
    }

    let radians = orientation_degrees
        .rem_euclid(FULL_ROTATION_DEGREES)
        .to_radians();
    let (sin, cos) = radians.sin_cos();
    let half_width = width_mm * HALF_FACTOR;
    let half_height = height_mm * HALF_FACTOR;
    let corners = [
        (-half_width, -half_height),
        (half_width, -half_height),
        (half_width, half_height),
        (-half_width, half_height),
    ]
    .map(|(x, y)| {
        (
            y.mul_add(sin, x.mul_add(cos, center.x)),
            y.mul_add(cos, (-x).mul_add(sin, center.y)),
        )
    });
    if !corners.iter().all(|(x, y)| x.is_finite() && y.is_finite()) {
        return None;
    }

    let left = corners
        .iter()
        .map(|point| point.0)
        .fold(f64::INFINITY, f64::min);
    let top = corners
        .iter()
        .map(|point| point.1)
        .fold(f64::INFINITY, f64::min);
    let right = corners
        .iter()
        .map(|point| point.0)
        .fold(f64::NEG_INFINITY, f64::max);
    let bottom = corners
        .iter()
        .map(|point| point.1)
        .fold(f64::NEG_INFINITY, f64::max);
    Some(BitmapImageGeometry {
        bounds_mm: (left, top, right, bottom),
        corners_mm: corners,
    })
}
/// Extracts embedded image facts, including pixel-derived dimensions, scale, rotation, and source UUID.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// Normalized sheet image facts records or coordinates derived from the input.
fn collect_sheet_image_facts(root: &Node) -> Vec<SchematicImageFact> {
    let format_version = child_list(root, "version")
        .and_then(list_items)
        .and_then(|items| items.get(1))
        .and_then(atom_value)
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(MIGRATED_BITMAP_PPI_FORMAT_VERSION);

    children(root, "image")
        .map(|node| {
            let uuid = child_list(node, "uuid")
                .and_then(list_items)
                .and_then(|items| items.get(1))
                .and_then(atom_value)
                .map(str::to_owned);
            let geometry = child_list(node, "data")
                .and_then(list_items)
                .map(|items| {
                    items
                        .iter()
                        .skip(1)
                        .filter_map(atom_value)
                        .collect::<String>()
                })
                .and_then(|encoded| parse_bitmap_image_metadata(&encoded))
                .and_then(|metadata| {
                    let position = point_from_at(node)?;
                    let orientation = match child_list(node, "rotate") {
                        Some(rotate) => number(list_items(rotate)?.get(1)?)?,
                        None => 0.0,
                    };
                    let scale = child_list(node, "scale")
                        .and_then(list_items)
                        .and_then(|items| items.get(1))
                        .and_then(number)
                        .filter(|scale| scale.is_finite() && *scale > 0.0)
                        .unwrap_or(1.0);
                    let ppi = if format_version <= LEGACY_BITMAP_PPI_FORMAT_VERSION {
                        DEFAULT_BITMAP_IMAGE_PPI_F64
                    } else if format_version < MIGRATED_BITMAP_PPI_FORMAT_VERSION {
                        f64::from(metadata.legacy_ppi)
                    } else {
                        f64::from(metadata.ppi)
                    };
                    let width =
                        f64::from(metadata.width_pixels) * MILLIMETRES_PER_INCH / ppi * scale;
                    let height =
                        f64::from(metadata.height_pixels) * MILLIMETRES_PER_INCH / ppi * scale;
                    rotated_bitmap_image_geometry(position, width, height, orientation)
                });
            let bounds_mm = geometry.map(|geometry| geometry.bounds_mm);
            let corners_mm = geometry.map(|geometry| geometry.corners_mm);
            SchematicImageFact {
                uuid,
                bounds_mm,
                corners_mm,
            }
        })
        .collect()
}
/// Pixel dimensions and current/legacy PPI values decoded from a supported bitmap header.
#[derive(Clone, Copy, Debug)]
struct BitmapImageMetadata {
    /// Decoded bitmap width in pixels.
    width_pixels: u32,
    /// Decoded bitmap height in pixels.
    height_pixels: u32,
    /// Current bitmap resolution in pixels per inch.
    ppi: u32,
    /// Legacy resolution value used by older KiCad file versions.
    legacy_ppi: u32,
}
/// Decodes the supported bitmap header fields needed to determine image dimensions and PPI.
///
/// # Arguments
///
/// * `encoded` — Base64-encoded bitmap payload from the schematic.
///
/// # Returns
///
/// Pixel dimensions and effective PPI parsed from a supported PNG or JPEG header, or `None` when the Base64 data has no supported image header.
fn parse_bitmap_image_metadata(encoded: &str) -> Option<BitmapImageMetadata> {
    /// Maximum decoded header prefix retained when inspecting an embedded bitmap.
    const MAX_BITMAP_HEADER_BYTES: usize = 256 * 1024;
    let bytes = decode_base64_prefix(encoded, MAX_BITMAP_HEADER_BYTES);
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        parse_png_image_metadata(&bytes)
    } else if bytes.starts_with(&[0xff, 0xd8]) {
        parse_jpeg_image_metadata(&bytes)
    } else {
        None
    }
}
/// Decodes at most the requested prefix of a base64-encoded bitmap payload.
///
/// # Arguments
///
/// * `encoded` — Base64-encoded bitmap payload from the schematic.
/// * `max_bytes` — Maximum number of decoded bytes to retain from the image payload.
///
/// # Returns
///
/// Normalized decode base64 prefix records or coordinates derived from the input.
fn decode_base64_prefix(encoded: &str, max_bytes: usize) -> Vec<u8> {
    let mut decoded = Vec::with_capacity(max_bytes.min(encoded.len() * 3 / 4));
    let mut quartet = [0_u8; 4];
    let mut quartet_len = 0;

    for byte in encoded.bytes() {
        if byte.is_ascii_whitespace() {
            continue;
        }
        if byte == b'=' {
            break;
        }
        let Some(value) = base64_digit(byte) else {
            return Vec::new();
        };
        quartet[quartet_len] = value;
        quartet_len += 1;

        if quartet_len == 4 {
            decoded.push((quartet[0] << 2) | (quartet[1] >> 4));
            decoded.push((quartet[1] << 4) | (quartet[2] >> 2));
            decoded.push((quartet[2] << 6) | quartet[3]);
            quartet_len = 0;
            if decoded.len() >= max_bytes {
                decoded.truncate(max_bytes);
                break;
            }
        }
    }

    if decoded.len() < max_bytes {
        match quartet_len {
            2 => decoded.push((quartet[0] << 2) | (quartet[1] >> 4)),
            3 => {
                decoded.push((quartet[0] << 2) | (quartet[1] >> 4));
                decoded.push((quartet[1] << 4) | (quartet[2] >> 2));
            }
            _ => {}
        }
    }
    decoded.truncate(max_bytes);
    decoded
}
/// Converts one base64 alphabet byte to its six-bit digit value.
///
/// # Arguments
///
/// * `byte` — byte to be used in this geometry or parsing operation.
///
/// # Returns
///
/// The six-bit digit represented by an ASCII Base64 alphabet byte, or `None` for any other byte.
const fn base64_digit(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}
/// Reads one big-endian 32-bit integer from a byte slice at the requested offset.
///
/// # Arguments
///
/// * `bytes` — Raw PNG/JPEG or network-order bytes being inspected.
/// * `offset` — Byte offset at which the integer is read.
///
/// # Returns
///
/// The decoded unsigned 32-bit integer, or `None` when the byte range is incomplete.
fn read_be_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}
/// Reads one big-endian 16-bit integer from a byte slice at the requested offset.
///
/// # Arguments
///
/// * `bytes` — Raw PNG/JPEG or network-order bytes being inspected.
/// * `offset` — Byte offset at which the integer is read.
///
/// # Returns
///
/// The decoded unsigned 16-bit integer, or `None` when the byte range is incomplete.
fn read_be_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}
/// Reads width, height, and resolution metadata from a PNG header and its supported resolution chunk.
///
/// # Arguments
///
/// * `bytes` — Raw PNG/JPEG or network-order bytes being inspected.
///
/// # Returns
///
/// PNG pixel width, pixel height, and resolution decoded from its header and supported physical-pixel-density chunk, or `None` when required metadata is invalid or absent.
fn parse_png_image_metadata(bytes: &[u8]) -> Option<BitmapImageMetadata> {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return None;
    }
    let mut width_pixels = None;
    let mut height_pixels = None;
    let mut ppi = DEFAULT_BITMAP_IMAGE_PPI;
    let mut legacy_ppi = DEFAULT_BITMAP_IMAGE_PPI;
    let mut offset: usize = 8;

    while offset.checked_add(12)? <= bytes.len() {
        let chunk_length = usize::try_from(read_be_u32(bytes, offset)?).ok()?;
        let chunk_type = bytes.get(offset + 4..offset + 8)?;
        let data_start = offset + 8;
        let data_end = data_start.checked_add(chunk_length)?;
        if data_end.checked_add(4)? > bytes.len() {
            break;
        }

        match chunk_type {
            b"IHDR" if chunk_length == 13 => {
                width_pixels = Some(read_be_u32(bytes, data_start)?);
                height_pixels = Some(read_be_u32(bytes, data_start + 4)?);
            }
            b"pHYs" if chunk_length == 9 => {
                let pixels_per_meter = read_be_u32(bytes, data_start)?;
                let unit_is_meter = bytes[data_start + 8] == 1;
                if unit_is_meter && pixels_per_meter > MINIMUM_PIXELS_PER_METRE_FOR_PPI {
                    ppi = pixels_per_meter_to_ppi(pixels_per_meter);
                    legacy_ppi = pixels_per_cm_to_ppi(pixels_per_meter / CENTIMETRES_PER_METRE);
                }
            }
            b"IDAT" | b"IEND" => break,
            _ => {}
        }
        offset = data_end + 4;
    }

    let width_pixels =
        width_pixels.filter(|value| *value > 0 && *value <= MAX_BITMAP_DIMENSION_PIXELS)?;
    let height_pixels =
        height_pixels.filter(|value| *value > 0 && *value <= MAX_BITMAP_DIMENSION_PIXELS)?;
    Some(BitmapImageMetadata {
        width_pixels,
        height_pixels,
        ppi: ppi.max(1),
        legacy_ppi: legacy_ppi.max(1),
    })
}
/// Converts pixels per metre to pixels per inch using integer rounding.
///
/// # Arguments
///
/// * `pixels_per_meter` — PNG resolution value expressed in pixels per metre.
///
/// # Returns
///
/// The PNG density converted to a nearest-integer pixels-per-inch value, clamped to a minimum of one PPI.
fn pixels_per_meter_to_ppi(pixels_per_meter: u32) -> u32 {
    // Round pixels/metre * 2.54 / 100 to the nearest integer without a float cast.
    rounded_ratio_to_u32(
        u64::from(pixels_per_meter) * TENTHS_OF_MILLIMETRE_PER_INCH,
        10_000,
    )
}
/// Converts legacy pixels-per-centimetre metadata to pixels per inch using integer rounding.
///
/// # Arguments
///
/// * `pixels_per_cm` — Legacy bitmap resolution expressed in pixels per centimetre.
///
/// # Returns
///
/// The legacy density converted to a nearest-integer pixels-per-inch value, clamped to a minimum of one PPI.
fn pixels_per_cm_to_ppi(pixels_per_cm: u32) -> u32 {
    // Legacy KiCad first truncated pixels/metre to integer pixels/cm.
    rounded_ratio_to_u32(
        u64::from(pixels_per_cm) * TENTHS_OF_MILLIMETRE_PER_INCH,
        u64::from(CENTIMETRES_PER_METRE),
    )
}
/// Rounds an integer numerator/denominator ratio and clamps the result to a valid positive `u32`.
///
/// # Arguments
///
/// * `numerator` — Integer numerator of the ratio being rounded.
/// * `denominator` — Positive integer denominator of the ratio.
///
/// # Returns
///
/// The quotient rounded to the nearest integer, saturated at `u32::MAX`, and clamped to at least one.
fn rounded_ratio_to_u32(numerator: u64, denominator: u64) -> u32 {
    let rounded = numerator.saturating_add(denominator / 2) / denominator;
    u32::try_from(rounded).unwrap_or(u32::MAX).max(1)
}
/// Reads image dimensions and JFIF density metadata from a JPEG header.
///
/// # Arguments
///
/// * `bytes` — Raw PNG/JPEG or network-order bytes being inspected.
///
/// # Returns
///
/// JPEG pixel dimensions and JFIF density when the corresponding header markers are present and valid, or `None` when they cannot be decoded.
fn parse_jpeg_image_metadata(bytes: &[u8]) -> Option<BitmapImageMetadata> {
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return None;
    }
    let mut width_pixels = None;
    let mut height_pixels = None;
    let mut ppi = DEFAULT_BITMAP_IMAGE_PPI;
    let mut offset = 2;

    while offset + 4 <= bytes.len() {
        if bytes[offset] != 0xff {
            offset += 1;
            continue;
        }
        while offset < bytes.len() && bytes[offset] == 0xff {
            offset += 1;
        }
        let marker = *bytes.get(offset)?;
        offset += 1;
        if marker == 0xd9 || marker == 0xda {
            break;
        }
        if marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        let segment_length = usize::from(read_be_u16(bytes, offset)?);
        if segment_length < 2 {
            return None;
        }
        let data_start = offset + 2;
        let data_end = offset.checked_add(segment_length)?;
        let segment = bytes.get(data_start..data_end)?;

        if marker == 0xe0
            && segment.len() >= JPEG_JFIF_MINIMUM_SEGMENT_BYTES
            && segment.starts_with(b"JFIF\0")
        {
            let units = segment[7];
            let density_x = u32::from(u16::from_be_bytes([segment[8], segment[9]]));
            ppi = match units {
                1 => density_x,
                2 => pixels_per_cm_to_ppi(density_x),
                _ => DEFAULT_BITMAP_IMAGE_PPI,
            };
        }

        if matches!(
            marker,
            0xc0 | 0xc1
                | 0xc2
                | 0xc3
                | 0xc5
                | 0xc6
                | 0xc7
                | 0xc9
                | 0xca
                | 0xcb
                | 0xcd
                | 0xce
                | 0xcf
        ) && segment.len() >= 5
        {
            height_pixels = Some(u32::from(u16::from_be_bytes([segment[1], segment[2]])));
            width_pixels = Some(u32::from(u16::from_be_bytes([segment[3], segment[4]])));
        }

        offset = data_end;
    }

    Some(BitmapImageMetadata {
        width_pixels: width_pixels.filter(|value| *value > 0)?,
        height_pixels: height_pixels.filter(|value| *value > 0)?,
        ppi: ppi.max(1),
        legacy_ppi: ppi.max(1),
    })
}
/// Extracts native KiCad schematic groups, their UUIDs, names, and member UUIDs.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// Normalized group facts records or coordinates derived from the input.
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
/// Extracts rectangular sheet frames and their axis-aligned bounds.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// Normalized frame facts records or coordinates derived from the input.
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
/// Extracts label kind, text, anchor position, and visible glyph bounds.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// Normalized label facts records or coordinates derived from the input.
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
/// Resolves placed symbol-pin coordinates through the selected library symbol unit and transform.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// Normalized pin facts records or coordinates derived from the input.
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
/// Extracts hierarchical sheet instances, their frame geometry, and sheet pins.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// Normalized sheet facts records or coordinates derived from the input.
fn collect_sheet_facts(root: &Node) -> Vec<SchematicSheetFact> {
    children(root, "sheet")
        .filter_map(|sheet| {
            let uuid = child_list(sheet, "uuid")
                .and_then(list_items)
                .and_then(|items| items.get(1))
                .and_then(atom_value)?
                .to_owned();
            let name = children(sheet, "property")
                .find_map(|property| {
                    let items = list_items(property)?;
                    (atom_value(items.get(1)?)? == "Sheetname")
                        .then(|| atom_value(items.get(2)?))
                        .flatten()
                })
                .unwrap_or("unnamed")
                .to_owned();
            let at = child_list(sheet, "at").and_then(list_items)?;
            let size = child_list(sheet, "size").and_then(list_items)?;
            let origin = (number(at.get(1)?)?, number(at.get(2)?)?);
            let width = number(size.get(1)?)?;
            let height = number(size.get(2)?)?;
            if !origin.0.is_finite()
                || !origin.1.is_finite()
                || !width.is_finite()
                || !height.is_finite()
                || width <= 0.0
                || height <= 0.0
            {
                return None;
            }

            let reference = uuid.clone();
            let primitive = SchematicPrimitiveFact {
                reference: reference.clone(),
                points_mm: vec![
                    origin,
                    (origin.0 + width, origin.1),
                    (origin.0 + width, origin.1 + height),
                    (origin.0, origin.1 + height),
                ],
                closed: true,
                circle_mm: None,
                arc_mm: None,
                bezier_mm: None,
                stroke_width_mm: symbol_primitive_stroke_width(sheet),
            };
            let pins = children(sheet, "pin")
                .filter_map(|pin| {
                    let name = list_items(pin)
                        .and_then(|items| items.get(1))
                        .and_then(atom_value)?;
                    let at = child_list(pin, "at").and_then(list_items)?;
                    let position_mm = (number(at.get(1)?)?, number(at.get(2)?)?);
                    (position_mm.0.is_finite() && position_mm.1.is_finite()).then(|| {
                        SchematicPinFact {
                            reference: reference.clone(),
                            number: name.to_owned(),
                            position_mm,
                        }
                    })
                })
                .collect();

            Some(SchematicSheetFact {
                uuid,
                name,
                primitive,
                pins,
            })
        })
        .collect()
}
/// Reads an object's UUID and formats it with the supplied object-kind prefix.
///
/// # Arguments
///
/// * `node` — S-expression node whose properties, coordinates, or geometry are being decoded.
/// * `prefix` — prefix to be used in this geometry or parsing operation.
///
/// # Returns
///
/// The formatted value used by schematic geometry lookups.
fn object_reference(node: &Node, prefix: &str) -> String {
    child_list(node, "uuid")
        .and_then(list_items)
        .and_then(|items| items.get(1))
        .and_then(atom_value)
        .map_or_else(
            || format!("{prefix}-{}", span_of(node).start),
            str::to_owned,
        )
}
/// Half-width of KiCad's fixed-size no-connect X marker, in millimetres.
const NO_CONNECT_MARKER_HALF_SIZE_MM: f64 =
    NO_CONNECT_MARKER_SIZE_MILS * MILLIMETRES_PER_MIL * HALF_FACTOR;

/// Collects fixed-size no-connect marker geometry without assuming an unavailable per-marker pen width.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// Normalized no connect facts records or coordinates derived from the input.
fn collect_no_connect_facts(root: &Node) -> Vec<SchematicNoConnectFact> {
    children(root, "no_connect")
        .filter_map(|node| {
            let at = child_list(node, "at").and_then(list_items)?;
            let center_mm = (number(at.get(1)?)?, number(at.get(2)?)?);
            (center_mm.0.is_finite() && center_mm.1.is_finite()).then(|| SchematicNoConnectFact {
                reference: object_reference(node, "no-connect"),
                center_mm,
                half_size_mm: NO_CONNECT_MARKER_HALF_SIZE_MM,
            })
        })
        .collect()
}
/// Extracts schematic bus polylines as stroked obstacles for wire-clearance analysis.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// Normalized bus obstacle facts records or coordinates derived from the input.
fn collect_bus_obstacle_facts(root: &Node) -> Vec<SchematicBusObstacleFact> {
    let mut obstacles = Vec::new();
    for bus in children(root, "bus") {
        let Some(points) = polyline_primitive_points(bus) else {
            continue;
        };
        let [start, end] = points.as_slice() else {
            continue;
        };
        if !start.0.is_finite() || !start.1.is_finite() || !end.0.is_finite() || !end.1.is_finite()
        {
            continue;
        }
        let reference = object_reference(bus, "bus");
        obstacles.push(SchematicBusObstacleFact {
            kind: "bus line".to_owned(),
            primitive: SchematicPrimitiveFact {
                reference,
                points_mm: points,
                closed: false,
                circle_mm: None,
                arc_mm: None,
                bezier_mm: None,
                stroke_width_mm: symbol_primitive_stroke_width(bus),
            },
            pins: Vec::new(),
        });
    }

    for entry in children(root, "bus_entry") {
        let Some(at) = child_list(entry, "at").and_then(list_items) else {
            continue;
        };
        let Some(size) = child_list(entry, "size").and_then(list_items) else {
            continue;
        };
        let Some(start_x) = at.get(1).and_then(number) else {
            continue;
        };
        let Some(start_y) = at.get(2).and_then(number) else {
            continue;
        };
        let Some(size_x) = size.get(1).and_then(number) else {
            continue;
        };
        let Some(size_y) = size.get(2).and_then(number) else {
            continue;
        };
        let start = (start_x, start_y);
        let end = (start_x + size_x, start_y + size_y);
        if ![start.0, start.1, end.0, end.1]
            .iter()
            .all(|value| value.is_finite())
            || distance(
                Point {
                    x: start.0,
                    y: start.1,
                },
                Point { x: end.0, y: end.1 },
            ) <= GEOMETRY_TOLERANCE_MM
        {
            continue;
        }

        let reference = object_reference(entry, "bus-entry");
        let pins = [("start", start), ("end", end)]
            .into_iter()
            .map(|(name, (x, y))| SchematicPinFact {
                reference: reference.clone(),
                number: name.to_owned(),
                position_mm: (x, y),
            })
            .collect();
        obstacles.push(SchematicBusObstacleFact {
            kind: "bus entry".to_owned(),
            primitive: SchematicPrimitiveFact {
                reference,
                points_mm: vec![start, end],
                closed: false,
                circle_mm: None,
                arc_mm: None,
                bezier_mm: None,
                stroke_width_mm: symbol_primitive_stroke_width(entry),
            },
            pins,
        });
    }

    obstacles
}
/// Extracts visible symbol reference/value fields and their estimated text bounds.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// Normalized symbol field facts records or coordinates derived from the input.
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
/// Collects selected-unit embedded symbol primitives after applying the placed symbol transform.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// Normalized symbol primitive facts records or coordinates derived from the input.
fn collect_symbol_primitive_facts(root: &Node) -> Vec<SchematicPrimitiveFact> {
    let Some(library_symbols) = child_list(root, "lib_symbols") else {
        return Vec::new();
    };
    let definitions: Vec<_> = children(library_symbols, "symbol").collect();
    children(root, "symbol")
        .flat_map(|instance| collect_instance_primitives(instance, &definitions))
        .collect()
}
/// Transforms the primitives belonging to a placed symbol's selected library unit.
///
/// # Arguments
///
/// * `instance` — Placed symbol instance node containing reference, transform, unit, and conversion metadata.
/// * `definitions` — Library symbol definitions available to resolve the placed symbol.
///
/// # Returns
///
/// Normalized instance primitives records or coordinates derived from the input.
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
/// Extracts embedded library-symbol text and text boxes as transformed wire-obstacle geometry.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// Normalized symbol text facts records or coordinates derived from the input.
fn collect_symbol_text_facts(root: &Node) -> Vec<SchematicSymbolTextFact> {
    let Some(library_symbols) = child_list(root, "lib_symbols") else {
        return Vec::new();
    };
    let definitions: Vec<_> = children(library_symbols, "symbol").collect();
    let mut texts = Vec::new();

    for instance in children(root, "symbol") {
        let Some(transform) = read_symbol_transform(instance) else {
            continue;
        };
        let Some(lib_id) = child_list(instance, "lib_id")
            .and_then(list_items)
            .and_then(|items| items.get(1))
            .and_then(atom_value)
        else {
            continue;
        };
        let Some(definition) = definitions.iter().copied().find(|definition| {
            list_items(definition)
                .and_then(|items| items.get(1))
                .and_then(atom_value)
                .is_some_and(|name| name == lib_id)
        }) else {
            continue;
        };

        for unit in selected_symbol_units(definition, transform.unit, transform.convert) {
            texts.extend(collect_symbol_text_from_unit(unit, &transform));
        }
    }

    texts
}
/// Transforms text and text-box objects from one library unit into sheet coordinates.
///
/// # Arguments
///
/// * `unit` — Selected library symbol unit number.
/// * `transform` — Resolved transform that maps library-local coordinates to the schematic sheet.
///
/// # Returns
///
/// Normalized symbol text from unit records or coordinates derived from the input.
fn collect_symbol_text_from_unit(
    unit: &Node,
    transform: &SymbolTransform,
) -> Vec<SchematicSymbolTextFact> {
    let mut texts = Vec::new();
    for (kind, nodes) in [
        ("text", children(unit, "text")),
        ("text_box", children(unit, "text_box")),
    ] {
        for node in nodes {
            if property_is_hidden(node) {
                continue;
            }
            let Some(text) = list_items(node)
                .and_then(|items| items.get(1))
                .and_then(atom_value)
            else {
                continue;
            };
            let local_bounds = if kind == "text" {
                text_visual_bounds(text, node)
            } else {
                text_box_bounds(node)
            };
            let Some(bounds) =
                local_bounds.and_then(|bounds| transform_symbol_bounds(bounds, transform))
            else {
                continue;
            };
            texts.push(SchematicSymbolTextFact {
                reference: transform.reference.clone(),
                kind: kind.to_owned(),
                text: text.to_owned(),
                bounds_mm: (
                    bounds.x,
                    bounds.y,
                    bounds.x + bounds.width,
                    bounds.y + bounds.height,
                ),
            });
        }
    }
    texts
}
/// Transforms all four corners of a symbol-local rectangle and returns the enclosing sheet-coordinate bounds.
///
/// # Arguments
///
/// * `bounds` — Axis-aligned rectangle or primitive bounds in schematic millimetres.
/// * `transform` — Resolved transform that maps library-local coordinates to the schematic sheet.
///
/// # Returns
///
/// The axis-aligned sheet rectangle enclosing all four transformed corners of the library-space bounds, or `None` if a corner cannot be transformed.
fn transform_symbol_bounds(bounds: Bounds, transform: &SymbolTransform) -> Option<Bounds> {
    let points = [
        (bounds.x, bounds.y),
        (bounds.x + bounds.width, bounds.y),
        (bounds.x + bounds.width, bounds.y + bounds.height),
        (bounds.x, bounds.y + bounds.height),
    ];
    let transformed = transform_primitive_points(&points, transform)?;
    if transformed
        .iter()
        .any(|(x, y)| !x.is_finite() || !y.is_finite())
    {
        return None;
    }
    let left = transformed
        .iter()
        .map(|point| point.0)
        .fold(f64::INFINITY, f64::min);
    let top = transformed
        .iter()
        .map(|point| point.1)
        .fold(f64::INFINITY, f64::min);
    let right = transformed
        .iter()
        .map(|point| point.0)
        .fold(f64::NEG_INFINITY, f64::max);
    let bottom = transformed
        .iter()
        .map(|point| point.1)
        .fold(f64::NEG_INFINITY, f64::max);
    Some(Bounds {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}
/// Selects the library symbol units matching the placed instance's unit and conversion number.
///
/// # Arguments
///
/// * `definition` — Embedded library-symbol definition node containing the sub-symbol declarations for its available units and conversions.
/// * `unit` — Selected library symbol unit number.
/// * `convert` — Selected library symbol conversion variant.
///
/// # Returns
///
/// Normalized selected symbol units records or coordinates derived from the input.
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
/// Extracts and transforms supported geometric primitives from one library symbol unit.
///
/// # Arguments
///
/// * `unit` — Selected library symbol unit number.
/// * `transform` — Resolved transform that maps library-local coordinates to the schematic sheet.
///
/// # Returns
///
/// Normalized primitives from unit records or coordinates derived from the input.
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
/// Parses a supported symbol-library primitive and maps its geometry into sheet coordinates.
///
/// # Arguments
///
/// * `node` — S-expression node whose properties, coordinates, or geometry are being decoded.
/// * `transform` — Resolved transform that maps library-local coordinates to the schematic sheet.
///
/// # Returns
///
/// A transformed primitive fact containing its supported path/circle/arc/Bézier geometry and stroke width, or `None` for malformed or unsupported primitives.
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
            arc_mm: None,
            bezier_mm: None,
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
    let arc_mm = if kind == "arc" {
        let [start, mid, end] = arc_primitive_control_points(node)?;
        let controls = transform_primitive_points(&[start, mid, end], transform)?;
        circular_arc_through_three_points(controls[0], controls[1], controls[2])
    } else {
        None
    };
    let bezier_mm = if kind == "bezier" {
        let controls = bezier_primitive_control_points(node)?;
        let transformed = transform_primitive_points(&controls, transform)?;
        Some([
            transformed[0],
            transformed[1],
            transformed[2],
            transformed[3],
        ])
    } else {
        None
    };
    let last_point = *points_mm.last()?;
    let repeated_endpoint = points_mm.len() > 2
        && (points_mm[0].0 - last_point.0).abs() <= GEOMETRY_TOLERANCE_MM
        && (points_mm[0].1 - last_point.1).abs() <= GEOMETRY_TOLERANCE_MM;
    let closed = kind == "rectangle" || kind == "polygon" || repeated_endpoint;
    if closed && repeated_endpoint {
        points_mm.pop();
    }
    (points_mm.len() >= 2).then(|| SchematicPrimitiveFact {
        reference: transform.reference.clone(),
        points_mm,
        closed,
        circle_mm: None,
        arc_mm,
        bezier_mm,
        stroke_width_mm,
    })
}
/// Reads a symbol primitive's declared stroke width, defaulting to zero when it is missing.
///
/// # Arguments
///
/// * `node` — S-expression node whose properties, coordinates, or geometry are being decoded.
///
/// # Returns
///
/// The positive stroke width in millimetres, or zero when no width is declared.
fn symbol_primitive_stroke_width(node: &Node) -> f64 {
    child_list(node, "stroke")
        .and_then(|stroke| child_list(stroke, "width"))
        .and_then(list_items)
        .and_then(|items| items.get(1))
        .and_then(number)
        .filter(|width| width.is_finite() && width.abs() > GEOMETRY_TOLERANCE_MM)
        .map_or(DEFAULT_SYMBOL_PRIMITIVE_STROKE_WIDTH_MM, f64::abs)
}
/// Reads the two corners of a rectangle primitive from its S-expression.
///
/// # Arguments
///
/// * `node` — S-expression node whose properties, coordinates, or geometry are being decoded.
///
/// # Returns
///
/// The four rectangle vertices derived from its serialized start and end coordinates, or `None` when either corner is incomplete.
fn rectangle_primitive_points(node: &Node) -> Option<Vec<(f64, f64)>> {
    let start = child_list(node, "start").and_then(list_items)?;
    let end = child_list(node, "end").and_then(list_items)?;
    let x1 = number(start.get(1)?)?;
    let y1 = number(start.get(2)?)?;
    let x2 = number(end.get(1)?)?;
    let y2 = number(end.get(2)?)?;
    Some(vec![(x1, y1), (x2, y1), (x2, y2), (x1, y2)])
}
/// Reads the ordered coordinate list from a polyline or polygon primitive.
///
/// # Arguments
///
/// * `node` — S-expression node whose properties, coordinates, or geometry are being decoded.
///
/// # Returns
///
/// The ordered vertices from the primitive's `pts/xy` list, or `None` when the list is missing or has too few valid points.
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
/// Reads the start, midpoint, and end coordinates of a three-point arc.
///
/// # Arguments
///
/// * `node` — S-expression node whose properties, coordinates, or geometry are being decoded.
///
/// # Returns
///
/// The `(start, midpoint, end)` coordinates defining the circular arc, or `None` when any coordinate is missing or invalid.
fn arc_primitive_control_points(node: &Node) -> Option<[(f64, f64); 3]> {
    let point = |name| {
        let items = child_list(node, name).and_then(list_items)?;
        Some((number(items.get(1)?)?, number(items.get(2)?)?))
    };
    Some([point("start")?, point("mid")?, point("end")?])
}
/// Reconstructs the center, radius, start angle, and directed sweep of a three-point circular arc.
///
/// # Arguments
///
/// * `node` — S-expression node whose properties, coordinates, or geometry are being decoded.
///
/// # Returns
///
/// A circular-arc fact containing its center, radius, start angle, and directed sweep, or `None` for collinear or degenerate control points.
fn arc_primitive_geometry(node: &Node) -> Option<SchematicArcFact> {
    let [start, mid, end] = arc_primitive_control_points(node)?;
    circular_arc_through_three_points(start, mid, end)
}
/// Samples a three-point circular arc into a sequence of points for path-based geometry checks.
///
/// # Arguments
///
/// * `node` — S-expression node whose properties, coordinates, or geometry are being decoded.
///
/// # Returns
///
/// Sampled points along the circular arc for path-based geometry checks, or `None` when the three serialized points do not define a valid arc.
fn arc_primitive_points(node: &Node) -> Option<Vec<(f64, f64)>> {
    let [start, mid, end] = arc_primitive_control_points(node)?;
    Some(sample_arc_through_three_points(start, mid, end))
}
/// Defines the curve flatten tolerance mm constant.
const CURVE_FLATTEN_TOLERANCE_MM: f64 = 0.01;
/// Flattens a Kurbo path into schematic-coordinate points using the configured curve tolerance.
///
/// # Arguments
///
/// * `path` — Kurbo path elements or path segment being evaluated.
/// * `start` — Starting coordinate of the segment or curve, in millimetres.
/// * `end` — Ending coordinate of the segment or curve, in millimetres.
///
/// # Returns
///
/// Normalized ed curve points records or coordinates derived from the input.
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
/// Reads the four control points of a cubic Bézier primitive.
///
/// # Arguments
///
/// * `node` — S-expression node whose properties, coordinates, or geometry are being decoded.
///
/// # Returns
///
/// The four control points of the cubic Bézier primitive, or `None` when its point list is missing or malformed.
fn bezier_primitive_control_points(node: &Node) -> Option<[(f64, f64); 4]> {
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
    Some([*start, *control_one, *control_two, *end])
}
/// Flattens a cubic Bézier primitive into a sequence of schematic-coordinate points.
///
/// # Arguments
///
/// * `node` — S-expression node whose properties, coordinates, or geometry are being decoded.
///
/// # Returns
///
/// A flattened sequence of sheet-coordinate points approximating the cubic Bézier curve, or `None` when the control points cannot be evaluated.
fn bezier_primitive_points(node: &Node) -> Option<Vec<(f64, f64)>> {
    let [start, control_one, control_two, end] = bezier_primitive_control_points(node)?;
    let path = [
        PathEl::MoveTo(CurvePoint::new(start.0, start.1)),
        PathEl::CurveTo(
            CurvePoint::new(control_one.0, control_one.1),
            CurvePoint::new(control_two.0, control_two.1),
            CurvePoint::new(end.0, end.1),
        ),
    ];
    Some(flattened_curve_points(path, start, end))
}
/// Samples a circular arc passing through the supplied start, midpoint, and end points.
///
/// # Arguments
///
/// * `start` — Starting coordinate of the segment or curve, in millimetres.
/// * `mid` — mid to be used in this geometry or parsing operation.
/// * `end` — Ending coordinate of the segment or curve, in millimetres.
///
/// # Returns
///
/// Normalized arc through three points records or coordinates derived from the input.
fn sample_arc_through_three_points(
    start: (f64, f64),
    mid: (f64, f64),
    end: (f64, f64),
) -> Vec<(f64, f64)> {
    let Some(arc) = circular_arc_through_three_points(start, mid, end) else {
        return vec![start, mid, end];
    };
    let path = CurveArc::new(
        CurvePoint::new(arc.center_mm.0, arc.center_mm.1),
        Vec2::new(arc.radius_mm, arc.radius_mm),
        arc.start_angle_rad,
        arc.sweep_rad,
        0.0,
    );
    flattened_curve_points(path.path_elements(CURVE_FLATTEN_TOLERANCE_MM), start, end)
}
/// Recursively subdivides an elliptical arc until its midpoint deviation is within the flattening tolerance.
///
/// # Arguments
///
/// * `start_angle` — start angle to be used in this geometry or parsing operation.
/// * `start` — Starting coordinate of the segment or curve, in millimetres.
/// * `end_angle` — end angle to be used in this geometry or parsing operation.
/// * `end` — Ending coordinate of the segment or curve, in millimetres.
/// * `depth` — Remaining recursive subdivision depth for the ellipse approximation.
/// * `evaluate` — Parametric evaluator returning a sampled point along the elliptical arc.
/// * `points` — Ordered schematic-coordinate points defining a primitive or path.
///
/// # Returns
///
/// `Some(())` when subdivision completes and appends the required samples to the output vector; `None` when the evaluation closure cannot produce a midpoint.
fn subdivide_ellipse_arc(
    start_angle: f64,
    start: (f64, f64),
    end_angle: f64,
    end: (f64, f64),
    depth: u8,
    evaluate: &impl Fn(f64) -> Option<(f64, f64)>,
    points: &mut Vec<(f64, f64)>,
) -> Option<()> {
    if depth == 0 {
        points.push(end);
        return Some(());
    }

    let middle_angle = f64::midpoint(start_angle, end_angle);
    let middle = evaluate(middle_angle)?;
    let chord_middle = (f64::midpoint(start.0, end.0), f64::midpoint(start.1, end.1));
    let error_x = middle.0 - chord_middle.0;
    let error_y = middle.1 - chord_middle.1;
    let error_squared = error_x.mul_add(error_x, error_y * error_y);
    let tolerance_squared = CURVE_FLATTEN_TOLERANCE_MM * CURVE_FLATTEN_TOLERANCE_MM;
    if error_squared <= tolerance_squared {
        points.push(end);
    } else {
        subdivide_ellipse_arc(
            start_angle,
            start,
            middle_angle,
            middle,
            depth - 1,
            evaluate,
            points,
        )?;
        subdivide_ellipse_arc(
            middle_angle,
            middle,
            end_angle,
            end,
            depth - 1,
            evaluate,
            points,
        )?;
    }
    Some(())
}
/// Samples an ellipse or elliptical arc from its center, radii, rotation, and optional arc sweep.
///
/// # Arguments
///
/// * `node` — S-expression node whose properties, coordinates, or geometry are being decoded.
/// * `is_arc` — is arc to be used in this geometry or parsing operation.
///
/// # Returns
///
/// Sampled vertices for the full ellipse or the declared elliptical arc, including its rotation; `None` when its center, radii, or sweep are invalid.
fn ellipse_primitive_points(node: &Node, is_arc: bool) -> Option<Vec<(f64, f64)>> {
    let center = child_list(node, "center").and_then(list_items)?;
    let center = (number(center.get(1)?)?, number(center.get(2)?)?);
    let major_radius = child_list(node, "major_radius")
        .and_then(list_items)
        .and_then(|items| items.get(1))
        .and_then(number)?;
    let minor_radius = child_list(node, "minor_radius")
        .and_then(list_items)
        .and_then(|items| items.get(1))
        .and_then(number)?;
    let rotation = child_list(node, "rotation_angle")
        .and_then(list_items)
        .and_then(|items| items.get(1))
        .and_then(number)
        .unwrap_or(0.0);
    if ![center.0, center.1, major_radius, minor_radius, rotation]
        .iter()
        .all(|value| value.is_finite())
        || major_radius <= 0.0
        || minor_radius <= 0.0
    {
        return None;
    }

    let start_degrees = if is_arc {
        child_list(node, "start_angle")
            .and_then(list_items)
            .and_then(|items| items.get(1))
            .and_then(number)
            .unwrap_or(0.0)
    } else {
        0.0
    };
    let end_degrees = if is_arc {
        child_list(node, "end_angle")
            .and_then(list_items)
            .and_then(|items| items.get(1))
            .and_then(number)
            .unwrap_or(QUARTER_ROTATION_DEGREES)
    } else {
        FULL_ROTATION_DEGREES
    };
    if !start_degrees.is_finite() || !end_degrees.is_finite() {
        return None;
    }

    let mut sweep_degrees = end_degrees - start_degrees;
    if !sweep_degrees.is_finite() {
        return None;
    }
    if sweep_degrees >= FULL_ROTATION_DEGREES || sweep_degrees <= -FULL_ROTATION_DEGREES {
        sweep_degrees = FULL_ROTATION_DEGREES;
    } else if sweep_degrees < 0.0 {
        sweep_degrees += FULL_ROTATION_DEGREES;
    }

    let start_angle = start_degrees.to_radians().rem_euclid(std::f64::consts::TAU);
    let end_angle = start_angle + sweep_degrees.to_radians();
    let (sin_rotation, cos_rotation) = rotation.to_radians().sin_cos();
    let evaluate = |angle: f64| {
        let (sin_angle, cos_angle) = angle.sin_cos();
        let local_x = major_radius * cos_angle;
        let local_y = minor_radius * sin_angle;
        let x = center.0 + local_x.mul_add(cos_rotation, -local_y * sin_rotation);
        let y = center.1 + local_x.mul_add(sin_rotation, local_y * cos_rotation);
        (x.is_finite() && y.is_finite()).then_some((x, y))
    };

    let start = evaluate(start_angle)?;
    let end = evaluate(end_angle)?;
    let mut points = vec![start];
    subdivide_ellipse_arc(
        start_angle,
        start,
        end_angle,
        end,
        ELLIPSE_ARC_SUBDIVISION_STEPS,
        &evaluate,
        &mut points,
    )?;
    Some(points)
}
/// Reconstructs circular-arc center, radius, and directed sweep from three non-collinear points.
///
/// # Arguments
///
/// * `start` — Starting coordinate of the segment or curve, in millimetres.
/// * `mid` — mid to be used in this geometry or parsing operation.
/// * `end` — Ending coordinate of the segment or curve, in millimetres.
///
/// # Returns
///
/// A circular-arc fact reconstructed from the three points, or `None` when they are collinear or numerically degenerate.
fn circular_arc_through_three_points(
    start: (f64, f64),
    mid: (f64, f64),
    end: (f64, f64),
) -> Option<SchematicArcFact> {
    let triangle = Triangle::new(
        CurvePoint::new(start.0, start.1),
        CurvePoint::new(mid.0, mid.1),
        CurvePoint::new(end.0, end.1),
    );
    // Preserve the previous near-collinear fallback before asking Kurbo for
    // the circumcircle; its circumcircle method assumes non-zero triangle area.
    if triangle.area().abs() <= MINIMUM_CURVE_TRIANGLE_AREA_MM2 {
        return None;
    }

    let circle = triangle.circumscribed_circle();
    let center_x = circle.center.x;
    let center_y = circle.center.y;
    let radius = circle.radius.abs();
    if !center_x.is_finite()
        || !center_y.is_finite()
        || !radius.is_finite()
        || radius <= GEOMETRY_NUMERIC_EPSILON
    {
        return None;
    }

    let start_angle = (start.1 - center_y).atan2(start.0 - center_x);
    let mid_angle = (mid.1 - center_y).atan2(mid.0 - center_x);
    let end_angle = (end.1 - center_y).atan2(end.0 - center_x);
    let counter_clockwise_sweep = (end_angle - start_angle).rem_euclid(std::f64::consts::TAU);
    let mid_sweep = (mid_angle - start_angle).rem_euclid(std::f64::consts::TAU);
    let sweep = if mid_sweep <= counter_clockwise_sweep + GEOMETRY_NUMERIC_EPSILON {
        counter_clockwise_sweep
    } else {
        counter_clockwise_sweep - std::f64::consts::TAU
    };
    if sweep.abs() <= GEOMETRY_NUMERIC_EPSILON {
        return None;
    }

    Some(SchematicArcFact {
        center_mm: (center_x, center_y),
        radius_mm: radius,
        start_angle_rad: start_angle,
        sweep_rad: sweep,
    })
}
/// Applies a placed-symbol affine transform to each primitive point.
///
/// # Arguments
///
/// * `points` — Ordered schematic-coordinate points defining a primitive or path.
/// * `transform` — Resolved transform that maps library-local coordinates to the schematic sheet.
///
/// # Returns
///
/// Each input point transformed from library-local coordinates into sheet coordinates, or `None` when any point cannot be transformed.
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
    /// Diagnostics emitted by all registered passes.
    pub diagnostics: Vec<Diagnostic>,
    /// Number of wire segments analyzed in this run.
    pub wires: usize,
    /// Number of placed components collected from the schematic.
    pub components: usize,
}

/// Built-in geometry and pin-placement analysis pass.
pub struct BuiltinGeometryPass;

impl RulePass for BuiltinGeometryPass {
    /// Returns the stable registry key for the built-in geometry and pin-placement analysis pass.
    ///
    /// # Returns
    ///
    /// The unique pass name `01-kicad-clippy.geometry`, used to order geometry checks before label and annotation checks.
    fn name(&self) -> &'static str {
        "01-kicad-clippy.geometry"
    }
    /// Runs wire geometry, pin-orientation, no-connect-marker, and wire-obstacle checks for the current schematic.
    ///
    /// # Arguments
    ///
    /// * `context` — Shared rule context containing schematic facts and project-resolved width settings.
    ///
    /// # Returns
    ///
    /// Diagnostics emitted by this pass's wire-geometry, pin-orientation, no-connect, and obstacle checks; empty when those checks find no violation.
    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        run_geometry_checks(context)
    }
}

/// Built-in physical-wire topology and label analysis pass.
pub struct BuiltinLabelPass;

impl RulePass for BuiltinLabelPass {
    /// Returns the stable registry key for the built-in physical-wire and label analysis pass.
    ///
    /// # Returns
    ///
    /// The unique pass name `02-kicad-clippy.labels`, used to order label-topology checks after geometry checks.
    fn name(&self) -> &'static str {
        "02-kicad-clippy.labels"
    }
    /// Runs physical wire-island, label identity, label/wire overlap, and label-kind checks for the current schematic.
    ///
    /// # Arguments
    ///
    /// * `context` — Shared rule context containing schematic facts and project-resolved width settings.
    ///
    /// # Returns
    ///
    /// Diagnostics emitted by physical label-topology and label-overlap checks; empty when labels are consistent with the wire geometry.
    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        run_label_checks(context)
    }
}

/// Built-in annotation and visible-field layout analysis pass.
pub struct BuiltinAnnotationPass;

impl RulePass for BuiltinAnnotationPass {
    /// Returns the stable registry key for the built-in annotation and visible-field analysis pass.
    ///
    /// # Returns
    ///
    /// The unique pass name `03-kicad-clippy.annotations`, used to order annotation checks after geometry and label checks.
    fn name(&self) -> &'static str {
        "03-kicad-clippy.annotations"
    }
    /// Runs text-box layout, multiline annotation-border, and visible symbol-field clearance checks for the current schematic.
    ///
    /// # Arguments
    ///
    /// * `context` — Shared rule context containing schematic facts and project-resolved width settings.
    ///
    /// # Returns
    ///
    /// Diagnostics emitted by text-box, block-annotation, and symbol-field layout checks; empty when the annotation layout is valid.
    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        run_annotation_checks(context)
    }
}

/// Runs the registered built-in schematic rules and returns their combined diagnostics and run statistics.
///
/// # Arguments
///
/// * `document` — Parsed schematic document whose S-expression tree and source spans are used for fact extraction.
///
/// # Returns
///
/// A `RuleRun` containing the combined diagnostics and counts of analyzed wires and components.
#[must_use]
pub fn run_builtin_rules(document: &SchematicDocument) -> RuleRun {
    crate::RuleEngine::with_builtins().run(document)
}
/// Runs geometry, orientation, no-connect, and wire-obstacle checks over one schematic analysis context.
///
/// # Arguments
///
/// * `context` — Shared rule context containing schematic facts and project-resolved width settings.
///
/// # Returns
///
/// All diagnostics from wire geometry, pin orientation, no-connect marker, and wire-obstacle checks; an empty vector means those checks found no issue.
#[must_use]
pub fn run_geometry_checks(context: &RuleContext<'_>) -> Vec<Diagnostic> {
    let Some(root) = context.document().cst().nodes.first() else {
        return Vec::new();
    };
    let mut diagnostics = check_wire_geometry(root, context.segments, context.junctions);
    diagnostics.extend(check_pin_orientation(root, context.segments));
    diagnostics.extend(check_no_connect_markers(root));
    let mut wire_stroke_widths = collect_explicit_wire_stroke_widths(root);
    for (span_start, width_mm) in context.project_wire_stroke_widths_mm {
        wire_stroke_widths.entry(*span_start).or_insert(*width_mm);
    }
    let default_wire_stroke_width_mm = if schematic_has_graphical_netclass_assignments(root) {
        None
    } else {
        context.default_wire_stroke_width_mm
    };
    diagnostics.extend(check_wire_obstacles(
        root,
        context.segments,
        context.facts(),
        &wire_stroke_widths,
        default_wire_stroke_width_mm,
        context.default_schematic_line_stroke_width_mm,
    ));
    diagnostics
}
/// Detects schematic-local netclass assignments that make project-only width inference unsafe.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// `true` when the schematic contains a directive, rule area, or label property assigning a netclass; such assignments make project-only width inference unsafe.
fn schematic_has_graphical_netclass_assignments(root: &Node) -> bool {
    let mut has_assignments = false;
    walk(root, &mut |node| match head(node) {
        Some("directive_label" | "rule_area") => has_assignments = true,
        Some("label" | "hierarchical_label" | "global_label")
            if children(node, "property").any(|property| {
                list_items(property)
                    .and_then(|items| items.get(1))
                    .and_then(atom_value)
                    .is_some_and(|name| matches!(name, "Net Class" | "Netclass"))
            }) =>
        {
            has_assignments = true;
        }
        _ => {}
    });
    has_assignments
}
/// Runs physical label-topology, label/wire glyph, and label-text consistency checks.
///
/// # Arguments
///
/// * `context` — Shared rule context containing schematic facts and project-resolved width settings.
///
/// # Returns
///
/// Diagnostics for physical label topology, label/wire glyph clearance, label text overlap, and inconsistent label kinds.
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
/// Runs annotation text-box geometry and multiline block-annotation checks.
///
/// # Arguments
///
/// * `context` — Shared rule context containing schematic facts and project-resolved width settings.
///
/// # Returns
///
/// Diagnostics for text-box layout, multiline annotation borders, and visible symbol-field clearance.
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
/// Represents the supported pinaxis alternatives.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PinAxis {
    /// Wire axis parallel to the X coordinate.
    Horizontal,
    /// Wire axis parallel to the Y coordinate.
    Vertical,
}

impl PinAxis {
    /// Calculates as str from the supplied schematic geometry or S-expression data.
    ///
    /// # Returns
    ///
    /// The as str result derived from the schematic geometry.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Horizontal => "horizontal",
            Self::Vertical => "vertical",
        }
    }
}
/// Placed-symbol identity and transform parameters used to map library geometry to the sheet.
#[derive(Clone, Debug)]
struct SymbolTransform {
    /// Reference designator of the placed symbol.
    reference: String,
    /// Symbol origin in schematic coordinates.
    origin: Point,
    /// Placed-symbol rotation in degrees.
    rotation: f64,
    /// Optional mirror mode applied by KiCad.
    mirror: Option<String>,
    /// Selected library symbol unit number.
    unit: u32,
    /// Selected library symbol conversion number.
    convert: u32,
}
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
fn check_wire_obstacles(
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
/// Collects visible symbol Reference and Value fields with estimated glyph bounds and source spans.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// Normalized visible field bounds records or coordinates derived from the input.
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
fn segment_arc_intersection_points(wire: Segment, arc: SchematicArcFact) -> Vec<Point> {
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
fn segment_bezier_intersection_points(
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
/// The cubic bezier path segment result derived from the schematic geometry.
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
fn wire_within_circle_stroke(
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
fn wire_crosses_sheet_graphic_with_wire_stroke(
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
fn wire_crosses_symbol_primitive_with_wire_stroke(
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
fn geo_line(start: Point, end: Point) -> Line<f64> {
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
fn point_on_segment_any(point: Point, start: Point, end: Point) -> bool {
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
/// Checks connected wire axes against the transformed orientation of placed symbol pins.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
/// * `segments` — Parsed wire segments used to determine geometry, topology, or obstacle intersections.
///
/// # Returns
///
/// SCH005 diagnostics for connected wire axes that do not align with the transformed orientation of their symbol pins.
fn check_pin_orientation(root: &Node, segments: &[Segment]) -> Vec<Diagnostic> {
    let Some(library_symbols) = child_list(root, "lib_symbols") else {
        return Vec::new();
    };
    children(root, "symbol")
        .flat_map(|instance| check_instance_pin_orientation(instance, library_symbols, segments))
        .collect()
}
/// Checks one placed symbol's connected pins against its resolved library pin orientation.
///
/// # Arguments
///
/// * `instance` — Placed symbol instance node containing reference, transform, unit, and conversion metadata.
/// * `library_symbols` — Root node of the embedded library-symbol collection used to resolve the placed instance's referenced symbol definition.
/// * `segments` — Parsed wire segments used to determine geometry, topology, or obstacle intersections.
///
/// # Returns
///
/// SCH005 diagnostics for pins on the selected placed symbol whose attached wire direction conflicts with the library pin orientation.
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
/// Checks that explicit no-connect markers coincide with supported transformed pins.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// SCH015 diagnostics for no-connect markers that do not coincide with a supported transformed symbol-pin anchor.
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
                .any(|pin| distance(*pin, point) < GEOMETRY_TOLERANCE_MM);
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
/// Resolves all placed symbol-pin coordinates when the required library geometry is available.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// Resolved pin coordinates keyed by component reference and pin number, or `None` when required symbol-library geometry is unavailable.
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
/// Reads a placed symbol's origin, rotation, mirror, unit, and conversion metadata.
///
/// # Arguments
///
/// * `instance` — Placed symbol instance node containing reference, transform, unit, and conversion metadata.
///
/// # Returns
///
/// The placed symbol's origin, rotation, mirror state, unit, and conversion metadata, or `None` when required instance data is missing.
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
/// Reads an unsigned integer property from a placed symbol instance.
///
/// # Arguments
///
/// * `instance` — Placed symbol instance node containing reference, transform, unit, and conversion metadata.
/// * `name` — S-expression property name, label, or lookup key to read.
///
/// # Returns
///
/// The unsigned integer stored in the named instance property, or `None` when the property is absent or malformed.
fn instance_integer(instance: &Node, name: &str) -> Option<u32> {
    child_list(instance, name)
        .and_then(list_items)
        .and_then(|items| items.get(1))
        .and_then(atom_value)
        .and_then(|value| value.parse().ok())
}
/// Selects the pin definitions belonging to the requested library unit and conversion.
///
/// # Arguments
///
/// * `library_symbol` — Embedded library-symbol definition containing the unit-specific pin declarations for the placed component.
/// * `unit` — Selected library symbol unit number.
/// * `convert` — Selected library symbol conversion variant.
///
/// # Returns
///
/// Normalized embedded symbol pins records or coordinates derived from the input.
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
/// Builds a diagnostic when a wire direction conflicts with the transformed pin axis.
///
/// # Arguments
///
/// * `instance` — Placed symbol instance node containing reference, transform, unit, and conversion metadata.
/// * `pin` — Embedded library pin node containing the pin's local position, rotation angle, number, and electrical name.
/// * `transform` — Resolved transform that maps library-local coordinates to the schematic sheet.
/// * `segments` — Parsed wire segments used to determine geometry, topology, or obstacle intersections.
///
/// # Returns
///
/// A diagnostic when the supplied geometry violates the rule, or `None` when the geometry is acceptable.
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
        PinAxis::Horizontal => (segment.start.y - segment.end.y).abs() < GEOMETRY_TOLERANCE_MM,
        PinAxis::Vertical => (segment.start.x - segment.end.x).abs() < GEOMETRY_TOLERANCE_MM,
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
/// Parses the unit and conversion suffixes from a library sub-symbol name.
///
/// # Arguments
///
/// * `name` — S-expression property name, label, or lookup key to read.
///
/// # Returns
///
/// The unit number and conversion index encoded in the library sub-symbol name, or `None` when the suffix is not valid.
fn sub_symbol_unit_convert(name: &str) -> Option<(u32, u32)> {
    let (prefix, convert) = name.rsplit_once('_')?;
    let (_, unit) = prefix.rsplit_once('_')?;
    Some((unit.parse().ok()?, convert.parse().ok()?))
}
/// Returns the nearest exact cardinal rotation when the supplied angle is a supported quarter-turn.
///
/// # Arguments
///
/// * `angle` — Rotation angle in degrees as serialized by KiCad.
///
/// # Returns
///
/// The normalized angle for an exact quarter-turn orientation, or `None` when the supplied angle is not cardinal.
fn cardinal_rotation(angle: f64) -> Option<i32> {
    let normalized = angle.rem_euclid(FULL_ROTATION_DEGREES);
    [
        (0.0, 0),
        (QUARTER_ROTATION_DEGREES, 90),
        (HALF_ROTATION_DEGREES, 180),
        (THREE_QUARTER_ROTATION_DEGREES, 270),
    ]
    .into_iter()
    .find_map(|(degrees, rotation)| {
        ((normalized - degrees).abs() <= GEOMETRY_TOLERANCE_MM).then_some(rotation)
    })
}

/// Builds the affine transform for a symbol's rotation and supported mirror state.
///
/// # Arguments
///
/// * `rotation_degrees` — Placed symbol rotation in degrees.
/// * `mirror` — Optional KiCad mirror mode for the placed symbol.
///
/// # Returns
///
/// The affine transform for the symbol's origin, rotation, and supported mirror state, or `None` for an unsupported transform.
fn symbol_transform_affine(rotation_degrees: f64, mirror: Option<&str>) -> Option<Affine> {
    if !rotation_degrees.is_finite() {
        return None;
    }
    let rotation = rotation_degrees
        .rem_euclid(FULL_ROTATION_DEGREES)
        .to_radians();
    let mirror = match mirror {
        Some("x") => Affine::FLIP_Y,
        Some("y") => Affine::FLIP_X,
        None => Affine::IDENTITY,
        Some(_) => return None,
    };

    Some(Affine::FLIP_Y * Affine::rotate(rotation) * mirror)
}
/// Transforms a library-local coordinate into schematic coordinates.
///
/// # Arguments
///
/// * `x` — x to be used in this geometry or parsing operation.
/// * `y` — y to be used in this geometry or parsing operation.
/// * `rotation_degrees` — Placed symbol rotation in degrees.
/// * `mirror` — Optional KiCad mirror mode for the placed symbol.
///
/// # Returns
///
/// The transformed schematic coordinate in millimetres.
fn transform_symbol_point(
    x: f64,
    y: f64,
    rotation_degrees: f64,
    mirror: Option<&str>,
) -> Option<(f64, f64)> {
    let point = symbol_transform_affine(rotation_degrees, mirror)? * CurvePoint::new(x, y);
    Some((point.x, point.y))
}
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
    /// Calculates line from the supplied schematic geometry or S-expression data.
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
fn check_wire_geometry(root: &Node, segments: &[Segment], junctions: &[Point]) -> Vec<Diagnostic> {
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
/// Detects intersections between visible label glyph bounds and wire interiors.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
/// * `segments` — Parsed wire segments used to determine geometry, topology, or obstacle intersections.
///
/// # Returns
///
/// SCH003 diagnostics for visible label glyph bounds that overlap wire interiors.
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
/// Validates text-box size, font settings, required geometry, and overlap with wires or other annotations.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
/// * `segments` — Parsed wire segments used to determine geometry, topology, or obstacle intersections.
///
/// # Returns
///
/// SCH008 diagnostics for invalid text-box dimensions/font settings, missing geometry, or overlap with wires and annotations.
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
/// Validated text-box bounds and source span used by the annotation interaction checks.
#[derive(Clone, Debug)]
struct TextBoxRecord {
    /// Text-box identifier used in diagnostics.
    id: String,
    /// Validated text-box rectangle in schematic millimetres.
    bounds: Bounds,
    /// Original source byte span of the text-box expression.
    span: ByteSpan,
}
/// Parses one text box, validates its metrics, and returns the geometry record with any associated diagnostics.
///
/// # Arguments
///
/// * `node` — S-expression node whose properties, coordinates, or geometry are being decoded.
/// * `index` — Zero-based index of the schematic item being inspected.
///
/// # Returns
///
/// The inspect text box result derived from the schematic geometry.
fn inspect_text_box(node: &Node, index: usize) -> (Option<TextBoxRecord>, Vec<Diagnostic>) {
    let (metrics, error) = read_text_box_metrics(node, index);
    let Some(metrics) = metrics else {
        return (None, error.into_iter().collect());
    };
    let (required_width, required_height) =
        required_text_box_size(&metrics.label, metrics.font_size, metrics.padding);
    let mut diagnostics = Vec::new();
    if metrics.bounds.width + GEOMETRY_TOLERANCE_MM < required_width
        || metrics.bounds.height + GEOMETRY_TOLERANCE_MM < required_height
    {
        diagnostics.push(at_span(BuiltinRuleId::Sch008,
            format!(
                "text_box {index} is undersized ({:.2}x{:.2} mm); needs {required_width:.2}x{required_height:.2} mm",
                metrics.bounds.width, metrics.bounds.height
            ),
            metrics.span,
        ));
    }
    if metrics.stroke_width <= 0.0
        && metrics.bounds.width + GEOMETRY_TOLERANCE_MM
            < required_width + UNBORDERED_TEXT_BOX_EXTRA_WIDTH_MM
    {
        diagnostics.push(at_span(BuiltinRuleId::Sch008,
            format!(
                "unbordered text_box {index} needs at least {:.2} mm width for reliable single-line rendering",
                required_width + UNBORDERED_TEXT_BOX_EXTRA_WIDTH_MM
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
/// Parsed text-box label, geometry, font settings, padding, stroke, and source span.
#[derive(Clone, Debug)]
struct TextBoxMetrics {
    /// Decoded text-box contents.
    label: String,
    /// Parsed text-box rectangle in schematic millimetres.
    bounds: Bounds,
    /// Text font size in millimetres.
    font_size: f64,
    /// Text-box padding in millimetres.
    padding: f64,
    /// Border stroke width in millimetres.
    stroke_width: f64,
    /// Original source byte span of the text-box expression.
    span: ByteSpan,
}
/// Reads text-box content, font size, padding, stroke width, bounds, and source span.
///
/// # Arguments
///
/// * `node` — S-expression node whose properties, coordinates, or geometry are being decoded.
/// * `index` — Zero-based index of the schematic item being inspected.
///
/// # Returns
///
/// The read text box metrics result derived from the schematic geometry.
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
    if pads.len() != 4
        || pads
            .iter()
            .any(|pad| (pad - TEXT_BOX_EQUAL_PADDING_MM).abs() > GEOMETRY_TOLERANCE_MM)
    {
        return (
            None,
            Some(at_span(
                BuiltinRuleId::Sch008,
                format!(
                    "text_box {index} must have {TEXT_BOX_EQUAL_PADDING_MM:.2} mm equal padding"
                ),
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
/// Finds overlaps between text boxes and intersections between text-box bounds and wire segments.
///
/// # Arguments
///
/// * `boxes` — boxes to be used in this geometry or parsing operation.
/// * `segments` — Parsed wire segments used to determine geometry, topology, or obstacle intersections.
///
/// # Returns
///
/// SCH008 diagnostics for text-box overlap or intersections between text-box bounds and wire segments.
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
/// Checks that multiline text boxes have a visible border.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// SCH011 diagnostics for multiline block annotations that do not have a visible border.
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
/// Collects all descendant or direct child nodes with the requested S-expression head, according to the helper's traversal contract.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
/// * `name` — S-expression property name, label, or lookup key to read.
///
/// # Returns
///
/// Borrowed syntax-tree nodes matching the requested S-expression head.
fn collect_nodes<'a>(root: &'a Node, name: &'a str) -> Vec<&'a Node> {
    children(root, name).collect()
}
/// Extracts label kind, text, anchor position, source span, and original syntax node.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// Normalized label nodes records or coordinates derived from the input.
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
/// Estimates visible glyph bounds for a text value from its position, font effects, and justification.
///
/// # Arguments
///
/// * `value` — Text whose rendered glyph bounds are being estimated.
/// * `node` — S-expression text node supplying font size, angle, justification, and visibility effects.
///
/// # Returns
///
/// Estimated axis-aligned bounds of the rendered text glyphs in schematic coordinates, or `None` when its position or font metrics are unavailable.
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
    if !angle.is_finite() {
        return None;
    }
    let normalized_angle = angle.rem_euclid(FULL_ROTATION_DEGREES);

    let justify = effects.and_then(|effects| child_list(effects, "justify"));
    let justify_items = justify.and_then(list_items);
    let justify_name = justify_items
        .and_then(|items| items.get(1))
        .and_then(atom_value)
        .unwrap_or("center");
    let left = match justify_name {
        "left" => 0.0,
        "right" => -width,
        _ => -width * HALF_FACTOR,
    };

    // KiCad's anchor and justification define a local text rectangle. Kurbo
    // applies the cardinal rotation and returns its world-space AABB.
    let (left, top, rotation) = if (normalized_angle - QUARTER_ROTATION_DEGREES).abs()
        < GEOMETRY_TOLERANCE_MM
    {
        (left, -height * HALF_FACTOR, -std::f64::consts::FRAC_PI_2)
    } else if (normalized_angle - THREE_QUARTER_ROTATION_DEGREES).abs() < GEOMETRY_TOLERANCE_MM {
        (left, -height * HALF_FACTOR, std::f64::consts::FRAC_PI_2)
    } else {
        let top_aligned = justify_items
            .and_then(|items| items.get(2))
            .and_then(atom_value)
            .is_some_and(|value| value == "top");
        let top = if top_aligned { 0.0 } else { -height };
        if (normalized_angle - HALF_ROTATION_DEGREES).abs() < GEOMETRY_TOLERANCE_MM {
            // A 180-degree turn reflects the justified text rectangle around
            // its anchor; its AABB stays axis-aligned but changes sides.
            (-left - width, -top - height, 0.0)
        } else {
            (left, top, 0.0)
        }
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
/// Defines the newstroke glyph limit constant.
const NEWSTROKE_GLYPH_LIMIT: u32 = 0x27ff;
/// Defines the newstroke measure glyph constant.
const NEWSTROKE_MEASURE_GLYPH: &str = "H";
/// Lazily constructs the reference glyph used to normalize NewStroke text metrics.
///
/// # Returns
///
/// A lazily initialized reference glyph and its cap-height measurement.
fn newstroke_sentinel() -> &'static (Vec<VectorPoint>, f64) {
    /// Lazily initialized glyph and cap-height reference used by NewStroke text measurements.
    static SENTINEL: OnceLock<(Vec<VectorPoint>, f64)> = OnceLock::new();
    SENTINEL.get_or_init(|| {
        let points = NewstrokeRenderer::render_text(NEWSTROKE_MEASURE_GLYPH, ());
        let min_y = points.iter().map(|point| point.y).min().unwrap_or(0);
        let max_y = points.iter().map(|point| point.y).max().unwrap_or(0);
        (points, f64::from(max_y - min_y))
    })
}

/// Measures NewStroke text width and height relative to the reference glyph.
///
/// # Arguments
///
/// * `value` — String whose Newstroke glyph widths and line heights are measured.
///
/// # Returns
///
/// Estimated text width and height ratios relative to the reference glyph.
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
/// Calculates the minimum text-box width and height needed for the supplied text, font size, and padding.
///
/// # Arguments
///
/// * `value` — Text content that must fit inside the annotation box.
/// * `font_size` — Requested font height in millimetres.
/// * `padding` — Required internal padding around rendered glyph bounds, in millimetres.
///
/// # Returns
///
/// Minimum text-box width and height in millimetres for the supplied text and padding.
fn required_text_box_size(value: &str, font_size: f64, padding: f64) -> (f64, f64) {
    let lines: Vec<_> = value.lines().collect();
    let line_count = u32::try_from(lines.len().max(1)).unwrap_or(u32::MAX);
    let max_width = lines
        .iter()
        .map(|line| newstroke_text_metrics_ratio(line).0 * font_size)
        .fold(0.0_f64, f64::max);
    (
        TWO_SIDED_FACTOR.mul_add(
            padding,
            (max_width * TEXT_GLYPH_WIDTH_SAFETY_FACTOR).max(font_size),
        ),
        (f64::from(line_count) * font_size)
            .mul_add(TEXT_LINE_HEIGHT_SAFETY_FACTOR, TWO_SIDED_FACTOR * padding),
    )
}
/// Tests whether two axis-aligned bounds overlap by more than the schematic geometry tolerance.
///
/// # Arguments
///
/// * `first` — First element index or geometric endpoint participating in the operation.
/// * `second` — Second element index or geometric endpoint participating in the operation.
///
/// # Returns
///
/// `true` when the two rectangles overlap in both axes by more than the geometry tolerance; edge-only contact is not an overlap.
fn bounds_overlap(first: Bounds, second: Bounds) -> bool {
    (first.x + first.width).min(second.x + second.width) - first.x.max(second.x)
        > GEOMETRY_TOLERANCE_MM
        && (first.y + first.height).min(second.y + second.height) - first.y.max(second.y)
            > GEOMETRY_TOLERANCE_MM
}
/// Tests whether a wire segment intersects a rectangle, optionally requiring a strict interior crossing.
///
/// # Arguments
///
/// * `segment` — Wire segment being checked for obstacle intersection or clearance.
/// * `bounds` — Axis-aligned rectangle or primitive bounds in schematic millimetres.
/// * `strict` — Whether the rectangle test requires an interior crossing instead of allowing boundary contact.
///
/// # Returns
///
/// `true` when the wire intersects the rectangle, with `strict` controlling whether boundary-only contact is excluded.
fn wire_overlaps_bounds(segment: Segment, bounds: Bounds, strict: bool) -> bool {
    /// Geometric tolerance used by the wire-to-bounds intersection test.
    const TOLERANCE: f64 = GEOMETRY_TOLERANCE_MM;

    let horizontal = (segment.start.y - segment.end.y).abs() < TOLERANCE;
    let vertical = (segment.start.x - segment.end.x).abs() < TOLERANCE;
    if !horizontal && !vertical {
        return false;
    }

    let inset = if strict { TOLERANCE } else { -TOLERANCE };
    let clipped_bounds = if horizontal {
        Bounds {
            y: bounds.y + inset,
            height: TWO_SIDED_FACTOR.mul_add(-inset, bounds.height),
            ..bounds
        }
    } else {
        Bounds {
            x: bounds.x + inset,
            width: TWO_SIDED_FACTOR.mul_add(-inset, bounds.width),
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
/// Checks a wire segment against a rectangle, including boundary contact.
///
/// # Arguments
///
/// * `segment` — Wire segment being checked for obstacle intersection or clearance.
/// * `bounds` — Axis-aligned rectangle or primitive bounds in schematic millimetres.
///
/// # Returns
///
/// `true` when the segment intersects the rectangle, including contact with its boundary.
fn line_hits_bounds(segment: Segment, bounds: Bounds) -> bool {
    wire_overlaps_bounds(segment, bounds, false)
}
/// Checks a wire segment against the interior of a rectangle without treating boundary-only contact as a hit.
///
/// # Arguments
///
/// * `segment` — Wire segment being checked for obstacle intersection or clearance.
/// * `bounds` — Axis-aligned rectangle or primitive bounds in schematic millimetres.
///
/// # Returns
///
/// `true` only when the segment crosses the rectangle's interior rather than touching its boundary.
fn strict_line_hits_bounds(segment: Segment, bounds: Bounds) -> bool {
    wire_overlaps_bounds(segment, bounds, true)
}
/// Creates a built-in-rule diagnostic and associates it with the supplied source byte span.
///
/// # Arguments
///
/// * `code` — Rule identifier associated with the diagnostic.
/// * `message` — Concise user-facing explanation of the detected rule violation, including enough context to identify the offending object or geometry.
/// * `span` — Original source byte span to attach to the diagnostic.
///
/// # Returns
///
/// A diagnostic with the supplied rule code and message, linked to the exact half-open byte range in the original schematic source.
fn at_span(code: BuiltinRuleId, message: impl Into<String>, span: ByteSpan) -> Diagnostic {
    let mut diagnostic = Diagnostic::for_builtin_rule(code, Severity::Error, message);
    diagnostic.span = Some(span);
    diagnostic
}
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
/// Detects overlaps between label glyph bounds and other labels, visible fields, or annotation boxes.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// SCH018 diagnostics for overlapping label glyphs and collisions with visible fields or annotation boxes.
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
/// Checks visible symbol fields against other fields, symbol bodies, wires, pins, labels, and annotation boxes.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
/// * `segments` — Parsed wire segments used to determine geometry, topology, or obstacle intersections.
/// * `facts` — Normalized facts extracted from the schematic.
///
/// # Returns
///
/// SCH010 diagnostics for visible reference/value fields that collide with other fields, symbols, wires, pins, labels, or annotations.
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
/// Converts axis-aligned schematic bounds into a `geo` rectangle.
///
/// # Arguments
///
/// * `bounds` — Axis-aligned rectangle or primitive bounds in schematic millimetres.
///
/// # Returns
///
/// The geo rect result derived from the schematic geometry.
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
/// Tests whether a point lies within schematic bounds, expanded by the geometry tolerance.
///
/// # Arguments
///
/// * `point` — Schematic coordinate being tested, in millimetres.
/// * `bounds` — Axis-aligned rectangle or primitive bounds in schematic millimetres.
///
/// # Returns
///
/// `true` when the point lies inside the rectangle after expanding its edges by the schematic geometry tolerance.
fn point_in_bounds(point: Point, bounds: Bounds) -> bool {
    let expanded = Bounds {
        x: bounds.x - GEOMETRY_TOLERANCE_MM,
        y: bounds.y - GEOMETRY_TOLERANCE_MM,
        width: bounds.width + TWO_SIDED_GEOMETRY_TOLERANCE_MM,
        height: bounds.height + TWO_SIDED_GEOMETRY_TOLERANCE_MM,
    };
    geo_rect(expanded).contains(&GeoPoint::new(point.x, point.y))
}
/// Calculates the Euclidean distance from a point to an axis-aligned bounds rectangle.
///
/// # Arguments
///
/// * `point` — Schematic coordinate being tested, in millimetres.
/// * `bounds` — Axis-aligned rectangle or primitive bounds in schematic millimetres.
///
/// # Returns
///
/// The shortest Euclidean distance in millimetres.
fn point_bounds_distance(point: Point, bounds: Bounds) -> f64 {
    Euclidean.distance(&GeoPoint::new(point.x, point.y), &geo_rect(bounds))
}
/// Converts a closed schematic primitive with at least three points into a polygon.
///
/// # Arguments
///
/// * `primitive` — Embedded or sheet-level primitive whose body and stroke are checked against the wire.
///
/// # Returns
///
/// A polygon built from a closed primitive with at least three points, or `None` for open or incomplete geometry.
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
/// Converts a schematic primitive's path points into a `geo` line string.
///
/// # Arguments
///
/// * `primitive` — Embedded or sheet-level primitive whose body and stroke are checked against the wire.
///
/// # Returns
///
/// The schematic path result derived from the schematic geometry.
fn schematic_path(primitive: &SchematicPrimitiveFact) -> LineString<f64> {
    LineString::new(
        primitive
            .points_mm
            .iter()
            .map(|(x, y)| Coord { x: *x, y: *y })
            .collect(),
    )
}
/// Tests whether the stroked geometry of a symbol primitive intersects the supplied bounds.
///
/// # Arguments
///
/// * `bounds` — Axis-aligned rectangle or primitive bounds in schematic millimetres.
/// * `primitive` — Embedded or sheet-level primitive whose body and stroke are checked against the wire.
///
/// # Returns
///
/// `true` when the primitive's stroked geometry intersects the supplied rectangle.
fn symbol_primitive_intersects_bounds(bounds: Bounds, primitive: &SchematicPrimitiveFact) -> bool {
    if let Some((center_x, center_y, radius)) = primitive.circle_mm {
        let center = Point {
            x: center_x,
            y: center_y,
        };
        return point_bounds_distance(center, bounds)
            <= radius + primitive.stroke_width_mm * HALF_FACTOR + GEOMETRY_TOLERANCE_MM;
    }

    let rectangle = geo_rect(bounds);
    schematic_polygon(primitive).map_or_else(
        || schematic_path(primitive).intersects(&rectangle),
        |polygon| polygon.intersects(&rectangle),
    )
}
/// Tests whether the supplied bounds are contained within a closed symbol primitive's interior.
///
/// # Arguments
///
/// * `bounds` — Axis-aligned rectangle or primitive bounds in schematic millimetres.
/// * `primitive` — Embedded or sheet-level primitive whose body and stroke are checked against the wire.
///
/// # Returns
///
/// `true` when the entire supplied rectangle lies inside the closed primitive's interior after accounting for its stroke.
fn symbol_primitive_contains_bounds(bounds: Bounds, primitive: &SchematicPrimitiveFact) -> bool {
    if let Some((center_x, center_y, radius)) = primitive.circle_mm {
        let inner_radius = radius - primitive.stroke_width_mm * HALF_FACTOR;
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
        .all(|corner| distance(*corner, center) <= inner_radius + GEOMETRY_TOLERANCE_MM);
    }

    schematic_polygon(primitive).is_some_and(|polygon| polygon.contains(&geo_rect(bounds)))
}
/// Checks whether a symbol property has its KiCad hide flag enabled.
///
/// # Arguments
///
/// * `property` — Symbol property node whose visibility flag is being checked.
///
/// # Returns
///
/// `true` when the property's `effects` list contains KiCad's `hide` flag.
fn property_is_hidden(property: &Node) -> bool {
    child_list(property, "effects").is_some_and(|effects| child_list(effects, "hide").is_some())
}
/// Reads a text box's anchor and size and returns its axis-aligned bounds.
///
/// # Arguments
///
/// * `node` — S-expression node whose properties, coordinates, or geometry are being decoded.
///
/// # Returns
///
/// Axis-aligned bounds of the text box in schematic coordinates, or `None` when its anchor or dimensions are unavailable.
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
/// Detects identical label text used with inconsistent local/global/hierarchical label kinds.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// SCH019 diagnostics for identical label text reused with conflicting local, global, or hierarchical label kinds.
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
/// Visits a syntax-tree node and each nested child node in depth-first order.
///
/// # Arguments
///
/// * `node` — S-expression node whose properties, coordinates, or geometry are being decoded.
/// * `visit` — Callback invoked for each syntax-tree node during depth-first traversal.
fn walk<'a>(node: &'a Node, visit: &mut impl FnMut(&'a Node)) {
    visit(node);
    if let Node::List { items, .. } = node {
        for item in items {
            walk(item, visit);
        }
    }
}
/// Returns the child slice when the supplied syntax-tree node is a list.
///
/// # Arguments
///
/// * `node` — S-expression node whose properties, coordinates, or geometry are being decoded.
///
/// # Returns
///
/// The child-node slice when the node is an S-expression list, or `None` when it is an atom.
fn list_items(node: &Node) -> Option<&[Node]> {
    if let Node::List { items, .. } = node {
        Some(items)
    } else {
        None
    }
}
/// Returns the leading symbol of a syntax-tree list node.
///
/// # Arguments
///
/// * `node` — S-expression node whose properties, coordinates, or geometry are being decoded.
///
/// # Returns
///
/// The leading symbol of an S-expression list, or `None` when the node has no symbolic head.
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
/// Iterates over direct child-list nodes whose head symbol matches the requested name.
///
/// # Arguments
///
/// * `node` — S-expression node whose properties, coordinates, or geometry are being decoded.
/// * `name` — S-expression property name, label, or lookup key to read.
///
/// # Returns
///
/// An iterator over matching direct child-list nodes.
fn children<'a>(node: &'a Node, name: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
    list_items(node)
        .into_iter()
        .flatten()
        .filter(move |child| head(child) == Some(name))
}
/// Returns the decoded string value of a symbol or quoted atom.
///
/// # Arguments
///
/// * `node` — S-expression node whose properties, coordinates, or geometry are being decoded.
///
/// # Returns
///
/// The decoded text of a plain or quoted S-expression atom, or `None` for a list node.
fn atom_value(node: &Node) -> Option<&str> {
    let Node::Atom { atom, .. } = node else {
        return None;
    };
    match atom {
        Atom::Symbol(value) | Atom::Quoted(value) => Some(value.as_str()),
    }
}
/// Parses an atom as a floating-point number.
///
/// # Arguments
///
/// * `node` — S-expression node whose properties, coordinates, or geometry are being decoded.
///
/// # Returns
///
/// The finite `f64` value parsed from the atom, or `None` when parsing fails or produces a non-finite value.
fn number(node: &Node) -> Option<f64> {
    atom_value(node)?.parse().ok()
}
/// Returns the first direct child list with the requested head symbol.
///
/// # Arguments
///
/// * `node` — S-expression node whose properties, coordinates, or geometry are being decoded.
/// * `name` — S-expression property name, label, or lookup key to read.
///
/// # Returns
///
/// The first direct child list whose head matches `name`, or `None` when no such child exists.
fn child_list<'a>(node: &'a Node, name: &'a str) -> Option<&'a Node> {
    children(node, name).next()
}
/// Reads an `(x, y)` coordinate from an S-expression `at` node.
///
/// # Arguments
///
/// * `node` — S-expression node whose properties, coordinates, or geometry are being decoded.
///
/// # Returns
///
/// The `(x, y)` coordinate decoded from an S-expression `at` node, or `None` when either coordinate is missing or invalid.
fn point_from_at(node: &Node) -> Option<Point> {
    let at = child_list(node, "at")?;
    let items = list_items(at)?;
    Some(Point {
        x: number(items.get(1)?)?,
        y: number(items.get(2)?)?,
    })
}
/// Returns the source byte span attached to an S-expression node.
///
/// # Arguments
///
/// * `node` — S-expression node whose properties, coordinates, or geometry are being decoded.
///
/// # Returns
///
/// The span of result derived from the schematic geometry.
const fn span_of(node: &Node) -> ByteSpan {
    match node {
        Node::List { span, .. } | Node::Atom { span, .. } => ByteSpan::new(span.start, span.end),
    }
}
/// Collects explicit positive wire stroke widths, indexed by each wire's source offset.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// An ordered lookup map derived from the parsed schematic geometry.
fn collect_explicit_wire_stroke_widths(root: &Node) -> BTreeMap<usize, f64> {
    let mut widths = BTreeMap::new();
    walk(root, &mut |node| {
        if head(node) != Some("wire") {
            return;
        }
        let width = child_list(node, "stroke")
            .and_then(|stroke| child_list(stroke, "width"))
            .and_then(list_items)
            .and_then(|items| items.get(1))
            .and_then(number);
        if let Some(width) = width.filter(|width| width.is_finite() && *width > 0.0) {
            widths.insert(span_of(node).start, width);
        }
    });
    widths
}
/// Extracts wire segments and their source byte spans from a schematic syntax tree.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// Normalized segments records or coordinates derived from the input.
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
/// Collects coordinate points from all nodes with the requested S-expression head.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
/// * `name` — S-expression property name, label, or lookup key to read.
///
/// # Returns
///
/// Normalized points records or coordinates derived from the input.
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
/// Extracts label kind, text, anchor coordinate, and source span from the schematic.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// Normalized labels records or coordinates derived from the input.
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
/// Tests whether a point lies on the finite wire segment.
///
/// # Arguments
///
/// * `point` — Schematic coordinate being tested, in millimetres.
/// * `segment` — Wire segment being checked for obstacle intersection or clearance.
///
/// # Returns
///
/// `true` when the point lies on the supplied finite wire segment within the schematic geometry tolerance.
fn point_on_segment(point: Point, segment: &Segment) -> bool {
    point_on_segment_any(point, segment.start, segment.end)
}
/// Tests whether a point coincides with either endpoint of a wire segment.
///
/// # Arguments
///
/// * `point` — Schematic coordinate being tested, in millimetres.
/// * `segment` — Wire segment being checked for obstacle intersection or clearance.
///
/// # Returns
///
/// `true` when the point matches either endpoint of the supplied wire segment within the geometry tolerance.
fn is_endpoint(point: Point, segment: Segment) -> bool {
    distance(point, segment.start) < GEOMETRY_TOLERANCE_MM
        || distance(point, segment.end) < GEOMETRY_TOLERANCE_MM
}
/// Calculates Euclidean distance between two schematic points.
///
/// # Arguments
///
/// * `first` — First element index or geometric endpoint participating in the operation.
/// * `second` — Second element index or geometric endpoint participating in the operation.
///
/// # Returns
///
/// The shortest Euclidean distance in millimetres.
fn distance(first: Point, second: Point) -> f64 {
    (first.x - second.x).hypot(first.y - second.y)
}
/// Creates a diagnostic associated with the source span and geometry of a wire segment.
///
/// # Arguments
///
/// * `code` — Rule identifier associated with the diagnostic.
/// * `message` — Concise user-facing explanation of the detected rule violation, including enough context to identify the offending object or geometry.
/// * `segment` — Wire segment being checked for obstacle intersection or clearance.
///
/// # Returns
///
/// A diagnostic whose source span covers the serialized wire segment, allowing an editor to highlight the offending wire.
fn at_segment(code: BuiltinRuleId, message: &str, segment: Segment) -> Diagnostic {
    let mut diagnostic = Diagnostic::new(code, Severity::Error, message);
    diagnostic.span = Some(segment.span);
    diagnostic
}
/// Creates a diagnostic associated with the supplied schematic coordinate and source span.
///
/// # Arguments
///
/// * `code` — Rule identifier associated with the diagnostic.
/// * `message` — Concise user-facing explanation of the detected rule violation, including enough context to identify the offending object or geometry.
/// * `point` — Schematic coordinate being tested, in millimetres.
/// * `span` — Original source byte span to attach to the diagnostic.
///
/// # Returns
///
/// A diagnostic linked to the original source span and the supplied sheet-space X/Y coordinate in millimetres.
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
/// Unit tests for schematic fact extraction, geometry, connectivity, and rule diagnostics.
mod tests {
    #![allow(clippy::expect_used)]
    use super::{HALF_FACTOR, run_builtin_rules};
    use kicad_clippy_parser::SchematicDocument;
    use kurbo::{ParamCurve, ParamCurveDeriv};
    /// Builds a minimal valid schematic document around the supplied S-expression body for geometry tests.
    ///
    /// # Arguments
    ///
    /// * `body` — S-expression items inserted under the generated `kicad_sch` root.
    ///
    /// # Returns
    ///
    /// The document result derived from the schematic geometry.
    fn document(body: &str) -> SchematicDocument {
        SchematicDocument::from_text("fixture.kicad_sch", format!("(kicad_sch {body})"))
            .expect("fixture parses")
    }
    /// Calculates sch013 checks both no connect marker diagonals from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_checks_both_no_connect_marker_diagonals() {
        let document = document(
            r#"
            (wire (pts (xy -1 0) (xy 1 0)))
            (no_connect (at 0 0) (uuid "nc-center"))
            "#,
        );

        assert!(run_builtin_rules(&document).diagnostics.iter().any(|item| {
            item.code == "SCH013" && item.message.contains("no-connect marker nc-center")
        }));
    }
    /// Calculates sch013 uses project default line width for no connect marker pen from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_uses_project_default_line_width_for_no_connect_marker_pen() {
        let schematic = document(
            r#"
            (wire (pts (xy 0.35 0.55) (xy 0.55 0.75))
              (stroke (width 0.2)))
            (no_connect (at 0 0) (uuid "nc-near-stroke"))
            "#,
        );
        let engine = crate::RuleEngine::with_builtins();

        assert!(
            !engine.run(&schematic).diagnostics.iter().any(|item| {
                item.code == "SCH013" && item.message.contains("no-connect marker")
            })
        );
        assert!(
            engine
                .run_with_project_stroke_widths(&schematic, None, Some(0.1524))
                .diagnostics
                .iter()
                .any(|item| {
                    item.code == "SCH013"
                        && item.message.contains("no-connect marker nc-near-stroke")
                })
        );
    }
    /// Calculates sch013 does not expand no connect marker with an assumed pen width from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_does_not_expand_no_connect_marker_with_an_assumed_pen_width() {
        let document = document(
            r#"
            (wire (pts (xy -1 0.7) (xy 1 0.7)))
            (no_connect (at 0 0) (uuid "nc-near-miss"))
            "#,
        );

        assert!(
            !run_builtin_rules(&document).diagnostics.iter().any(|item| {
                item.code == "SCH013" && item.message.contains("no-connect marker")
            })
        );
    }
    /// Calculates sch013 checks wire crossings of schematic bus lines from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_checks_wire_crossings_of_schematic_bus_lines() {
        let document = document(
            r#"
            (wire (pts (xy 1 -1) (xy 1 1)))
            (bus (pts (xy 0 0) (xy 2 0))
              (stroke (width 0.2))
              (uuid "bus-uuid"))
            "#,
        );

        assert!(
            run_builtin_rules(&document)
                .diagnostics
                .iter()
                .any(|item| { item.code == "SCH013" && item.message.contains("bus line") })
        );
    }
    /// Calculates sch013 allows wire endpoint at bus entry without exempting other crossings from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_allows_wire_endpoint_at_bus_entry_without_exempting_other_crossings() {
        let attached = document(
            r#"
            (wire (pts (xy -1 1) (xy 0 1)))
            (bus (pts (xy 0 2) (xy 2 2))
              (stroke (width 0.2))
              (uuid "bus-uuid"))
            (bus_entry (at 0 1) (size 0 1)
              (stroke (width 0.2))
              (uuid "bus-entry-uuid"))
            "#,
        );

        assert!(
            !run_builtin_rules(&attached)
                .diagnostics
                .iter()
                .any(|item| { item.code == "SCH013" && item.message.contains("bus entry") })
        );

        let through_entry = document(
            r#"
            (wire (pts (xy 0 1) (xy 0 1.5)))
            (bus_entry (at 0 1) (size 0 1)
              (stroke (width 0.2))
              (uuid "bus-entry-uuid"))
            "#,
        );

        assert!(
            run_builtin_rules(&through_entry)
                .diagnostics
                .iter()
                .any(|item| { item.code == "SCH013" && item.message.contains("bus entry") })
        );
    }
    /// Calculates sch013 checks hierarchical sheet frame away from sheet pins from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_checks_hierarchical_sheet_frame_away_from_sheet_pins() {
        let document = document(
            r#"
            (wire (pts (xy -1 2) (xy 1 2)))
            (sheet (at 0 0) (size 4 3)
              (stroke (width 0.2))
              (uuid "sheet-uuid")
              (property "Sheetname" "Child")
              (property "Sheetfile" "child.kicad_sch")
              (pin "DATA" input (at 0 1 180) (effects (font (size 1 1)))
                (uuid "sheet-pin-uuid")))
            "#,
        );

        assert!(
            run_builtin_rules(&document).diagnostics.iter().any(|item| {
                item.code == "SCH013" && item.message.contains("hierarchical sheet")
            })
        );
    }
    /// Calculates sch013 allows wire endpoint at matching sheet pin from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_allows_wire_endpoint_at_matching_sheet_pin() {
        let document = document(
            r#"
            (wire (pts (xy -1 1) (xy 0 1)))
            (sheet (at 0 0) (size 4 3)
              (stroke (width 0.2))
              (uuid "sheet-uuid")
              (property "Sheetname" "Child")
              (property "Sheetfile" "child.kicad_sch")
              (pin "DATA" input (at 0 1 180) (effects (font (size 1 1)))
                (uuid "sheet-pin-uuid")))
            "#,
        );

        assert!(
            !run_builtin_rules(&document).diagnostics.iter().any(|item| {
                item.code == "SCH013" && item.message.contains("hierarchical sheet")
            })
        );
    }
    /// Calculates sch013 sheet pin exemption does not allow wire to continue inside sheet from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_sheet_pin_exemption_does_not_allow_wire_to_continue_inside_sheet() {
        let document = document(
            r#"
            (wire (pts (xy 0 1) (xy 1 1)))
            (sheet (at 0 0) (size 4 3)
              (stroke (width 0.2))
              (uuid "sheet-uuid")
              (property "Sheetname" "Child")
              (property "Sheetfile" "child.kicad_sch")
              (pin "DATA" input (at 0 1 180) (effects (font (size 1 1)))
                (uuid "sheet-pin-uuid")))
            "#,
        );

        assert!(
            run_builtin_rules(&document).diagnostics.iter().any(|item| {
                item.code == "SCH013" && item.message.contains("hierarchical sheet")
            })
        );
    }
    /// Calculates sch013 checks enabled table external borders and column separators from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_checks_enabled_table_external_borders_and_column_separators() {
        let document = document(
            r#"
            (wire (pts (xy 1 -1) (xy 1 2)))
            (table
              (column_count 2)
              (column_widths 1 1)
              (row_heights 1)
              (cells
                (table_cell "" (at 0 0 0) (size 1 1))
                (table_cell "" (at 1 0 0) (size 1 1)))
              (border (external yes) (header no) (stroke (width 0.2)))
              (separators (rows no) (cols yes) (stroke (width 0.1)))
              (uuid "table-uuid"))
            "#,
        );

        let diagnostics = run_builtin_rules(&document).diagnostics;
        assert!(
            diagnostics
                .iter()
                .any(|item| { item.code == "SCH013" && item.message.contains("table border") })
        );
        assert!(
            diagnostics
                .iter()
                .any(|item| { item.code == "SCH013" && item.message.contains("table separator") })
        );
    }
    /// Calculates sch013 checks cardinally rotated table borders and separators from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_checks_cardinally_rotated_table_borders_and_separators() {
        let fixtures = [
            (90, "0", "-1", "0.5", "-1.5", "0.5", "-0.5"),
            (180, "-1", "0", "-1.5", "-0.5", "-0.5", "-0.5"),
            (270, "0", "1", "-0.5", "0.5", "-0.5", "1.5"),
        ];
        for (angle, second_x, second_y, start_x, start_y, end_x, end_y) in fixtures {
            let source = format!(
                r#"
                (wire (pts (xy {start_x} {start_y}) (xy {end_x} {end_y})))
                (table
                  (column_count 2)
                  (column_widths 1 1)
                  (row_heights 1)
                  (cells
                    (table_cell "" (at 0 0 {angle}) (size 1 1))
                    (table_cell "" (at {second_x} {second_y} {angle}) (size 1 1)))
                  (border (external yes) (header no) (stroke (width 0.2)))
                  (separators (rows no) (cols yes) (stroke (width 0.1)))
                  (uuid "rotated-table"))
                "#
            );
            let document = document(&source);
            assert!(
                run_builtin_rules(&document).diagnostics.iter().any(|item| {
                    item.code == "SCH013" && item.message.contains("table separator")
                }),
                "expected a rotated table separator finding at {angle} degrees"
            );
        }
    }
    /// Calculates sch013 checks cardinally rotated table external borders from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_checks_cardinally_rotated_table_external_borders() {
        let fixtures = [
            (90, "0", "-1", "-0.5", "-0.5", "0.5", "-0.5"),
            (180, "-1", "0", "-2.5", "-0.5", "-1.5", "-0.5"),
            (270, "0", "1", "-0.5", "-0.5", "-0.5", "0.5"),
        ];
        for (angle, second_x, second_y, start_x, start_y, end_x, end_y) in fixtures {
            let source = format!(
                r#"
                (wire (pts (xy {start_x} {start_y}) (xy {end_x} {end_y})))
                (table
                  (column_count 2)
                  (column_widths 1 1)
                  (row_heights 1)
                  (cells
                    (table_cell "" (at 0 0 {angle}) (size 1 1))
                    (table_cell "" (at {second_x} {second_y} {angle}) (size 1 1)))
                  (border (external yes) (header no) (stroke (width 0.2)))
                  (separators (rows no) (cols no))
                  (uuid "rotated-border-table"))
                "#
            );
            let document = document(&source);
            assert!(
                run_builtin_rules(&document)
                    .diagnostics
                    .iter()
                    .any(|item| { item.code == "SCH013" && item.message.contains("table border") }),
                "expected a rotated table external-border finding at {angle} degrees"
            );
        }
    }
    /// Calculates sch013 does not use unrotated table coordinates after rotation from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_does_not_use_unrotated_table_coordinates_after_rotation() {
        let document = document(
            r#"
            (wire (pts (xy 1 0.2) (xy 1 0.8)))
            (table
              (column_count 2)
              (column_widths 1 1)
              (row_heights 1)
              (cells
                (table_cell "" (at 0 0 90) (size 1 1))
                (table_cell "" (at 0 -1 90) (size 1 1)))
              (border (external yes) (header no) (stroke (width 0.2)))
              (separators (rows no) (cols yes) (stroke (width 0.1))))
            "#,
        );
        assert!(
            !run_builtin_rules(&document)
                .diagnostics
                .iter()
                .any(|item| { item.code == "SCH013" && item.message.contains("table") })
        );
    }
    /// Calculates sch013 skips tables with inconsistent cell rotations or positions from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_skips_tables_with_inconsistent_cell_rotations_or_positions() {
        let mixed_rotations = document(
            r#"
            (wire (pts (xy 0.5 -1.5) (xy 0.5 -0.5)))
            (table
              (column_count 2)
              (column_widths 1 1)
              (row_heights 1)
              (cells
                (table_cell "" (at 0 0 90) (size 1 1))
                (table_cell "" (at 0 -1 0) (size 1 1)))
              (border (external yes) (header no) (stroke (width 0.2)))
              (separators (rows no) (cols yes) (stroke (width 0.1))))
            "#,
        );
        assert!(
            !run_builtin_rules(&mixed_rotations)
                .diagnostics
                .iter()
                .any(|item| { item.code == "SCH013" && item.message.contains("table") })
        );

        let inconsistent_positions = document(
            r#"
            (wire (pts (xy 0.5 -1.5) (xy 0.5 -0.5)))
            (table
              (column_count 2)
              (column_widths 1 1)
              (row_heights 1)
              (cells
                (table_cell "" (at 0 0 90) (size 1 1))
                (table_cell "" (at 1 0 90) (size 1 1)))
              (border (external yes) (header no) (stroke (width 0.2)))
              (separators (rows no) (cols yes) (stroke (width 0.1))))
            "#,
        );
        assert!(
            !run_builtin_rules(&inconsistent_positions)
                .diagnostics
                .iter()
                .any(|item| { item.code == "SCH013" && item.message.contains("table") })
        );
    }
    /// Calculates sch013 ignores disabled table strokes and merged cell separators from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_ignores_disabled_table_strokes_and_merged_cell_separators() {
        let no_strokes = document(
            r#"
            (wire (pts (xy 1 0.2) (xy 1 0.8)))
            (table
              (column_count 2)
              (column_widths 1 1)
              (row_heights 1)
              (cells
                (table_cell "" (at 0 0 0) (size 2 1) (span 2 1))
                (table_cell "" (at 2 0 0) (size 0 0) (span 0 0)))
              (border (external no) (header no) (stroke (width 0.2)))
              (separators (rows no) (cols yes) (stroke (width 0.1))))
            "#,
        );
        assert!(
            !run_builtin_rules(&no_strokes)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH013" && item.message.contains("table"))
        );

        let disabled_separators = document(
            r#"
            (wire (pts (xy 1 0.2) (xy 1 0.8)))
            (table
              (column_count 2)
              (column_widths 1 1)
              (row_heights 1)
              (cells
                (table_cell "" (at 0 0 0) (size 1 1))
                (table_cell "" (at 1 0 0) (size 1 1)))
              (border (external no) (header no))
              (separators (rows no) (cols no)))
            "#,
        );
        assert!(
            !run_builtin_rules(&disabled_separators)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH013" && item.message.contains("table"))
        );
    }
    /// Calculates sch013 checks table cell text with estimated glyph bounds from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_checks_table_cell_text_with_estimated_glyph_bounds() {
        let document = document(
            r#"
            (wire (pts (xy 1 0.4) (xy 1 0.9)))
            (table
              (column_count 1)
              (column_widths 4)
              (row_heights 2)
              (cells
                (table_cell "R1" (at 1 0.5 0) (size 4 2)
                  (effects (font (size 1 1)))))
              (border (external no) (header no))
              (separators (rows no) (cols no)))
            "#,
        );
        assert!(run_builtin_rules(&document).diagnostics.iter().any(|item| {
            item.code == "SCH013" && item.message.contains("free-standing schematic text")
        }));
    }
    /// Calculates sch013 uses reflected text bounds for 180 degree table cells from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_uses_reflected_text_bounds_for_180_degree_table_cells() {
        let rotated = document(
            r#"
            (wire (pts (xy 1 0.6) (xy 1 0.9)))
            (table
              (column_count 1)
              (column_widths 4)
              (row_heights 2)
              (cells
                (table_cell "R1" (at 1 0.5 180) (size 4 2)
                  (effects (font (size 1 1)))))
              (border (external no) (header no))
              (separators (rows no) (cols no)))
            "#,
        );
        assert!(run_builtin_rules(&rotated).diagnostics.iter().any(|item| {
            item.code == "SCH013" && item.message.contains("free-standing schematic text")
        }));

        let unrotated = document(
            r#"
            (wire (pts (xy 1 0.6) (xy 1 0.9)))
            (table
              (column_count 1)
              (column_widths 4)
              (row_heights 2)
              (cells
                (table_cell "R1" (at 1 0.5 0) (size 4 2)
                  (effects (font (size 1 1)))))
              (border (external no) (header no))
              (separators (rows no) (cols no)))
            "#,
        );
        assert!(
            !run_builtin_rules(&unrotated)
                .diagnostics
                .iter()
                .any(|item| {
                    item.code == "SCH013" && item.message.contains("free-standing schematic text")
                })
        );
    }
    /// Calculates sch013 skips table cell text for unsupported non cardinal rotation from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_skips_table_cell_text_for_unsupported_non_cardinal_rotation() {
        let document = document(
            r#"
            (wire (pts (xy 1 0.4) (xy 1 0.9)))
            (table
              (column_count 1)
              (column_widths 4)
              (row_heights 2)
              (cells
                (table_cell "R1" (at 1 0.5 45) (size 4 2)
                  (effects (font (size 1 1)))))
              (border (external no) (header no))
              (separators (rows no) (cols no)))
            "#,
        );
        assert!(
            !run_builtin_rules(&document).diagnostics.iter().any(|item| {
                item.code == "SCH013" && item.message.contains("free-standing schematic text")
            })
        );
    }
    /// Calculates geo rectangle clipping preserves wire overlap tolerance from the supplied schematic geometry or S-expression data.
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
    /// Calculates analytic arc intersection detects a curve hit missed by its sampled chord from the supplied schematic geometry or S-expression data.
    #[test]
    fn analytic_arc_intersection_detects_a_curve_hit_missed_by_its_sampled_chord() {
        let arc = super::SchematicArcFact {
            center_mm: (0.0, 0.0),
            radius_mm: 2.0,
            start_angle_rad: 0.0,
            sweep_rad: std::f64::consts::FRAC_PI_2,
        };
        let angle = 0.37_f64;
        let point_at_radius = |radius: f64| super::Point {
            x: radius * angle.cos(),
            y: radius * angle.sin(),
        };
        let wire = super::Segment {
            start: point_at_radius(2.0),
            end: point_at_radius(2.01),
            span: kicad_clippy_core::ByteSpan::new(0, 0),
        };

        let midpoint = (2.0 * HALF_FACTOR.sqrt(), 2.0 * HALF_FACTOR.sqrt());
        let sampled = super::sample_arc_through_three_points((2.0, 0.0), midpoint, (0.0, 2.0));
        assert!(!sampled.windows(2).any(|pair| {
            !super::segment_intersection_points(
                wire,
                super::Point {
                    x: pair[0].0,
                    y: pair[0].1,
                },
                super::Point {
                    x: pair[1].0,
                    y: pair[1].1,
                },
            )
            .is_empty()
        }));
        let graphic = super::SchematicGraphicFact {
            kind: "arc".to_owned(),
            uuid: None,
            bounds_mm: None,
            points_mm: sampled.clone(),
            closed: false,
            circle_mm: None,
            arc_mm: Some(arc),
            bezier_mm: None,
            stroke_width_mm: 0.254,
        };
        assert!(super::wire_crosses_sheet_graphic_with_wire_stroke(
            wire, &graphic, 0.0
        ));

        let primitive = super::SchematicPrimitiveFact {
            reference: "U1".to_owned(),
            points_mm: sampled,
            closed: false,
            circle_mm: None,
            arc_mm: Some(arc),
            bezier_mm: None,
            stroke_width_mm: 0.254,
        };
        assert!(super::wire_crosses_symbol_primitive_with_wire_stroke(
            wire,
            &primitive,
            &[],
            0.0
        ));
        let allowed_pin = super::SchematicPinFact {
            reference: "U1".to_owned(),
            number: "1".to_owned(),
            position_mm: (wire.start.x, wire.start.y),
        };
        assert!(!super::wire_crosses_symbol_primitive_with_wire_stroke(
            wire,
            &primitive,
            &[allowed_pin],
            0.0,
        ));

        let intersections = super::segment_arc_intersection_points(wire, arc);
        assert_eq!(intersections.len(), 1);
        assert!((intersections[0].x - wire.start.x).abs() < 1.0e-9);
        assert!((intersections[0].y - wire.start.y).abs() < 1.0e-9);
    }
    /// Calculates analytic arc intersection respects directed sweep and tangency from the supplied schematic geometry or S-expression data.
    #[test]
    fn analytic_arc_intersection_respects_directed_sweep_and_tangency() {
        let arc = super::SchematicArcFact {
            center_mm: (0.0, 0.0),
            radius_mm: 2.0,
            start_angle_rad: 0.0,
            sweep_rad: std::f64::consts::FRAC_PI_2,
        };
        let outside_angle = 3.0 * std::f64::consts::FRAC_PI_4;
        let outside_wire = super::Segment {
            start: super::Point {
                x: 2.0 * outside_angle.cos(),
                y: 2.0 * outside_angle.sin(),
            },
            end: super::Point {
                x: 2.01 * outside_angle.cos(),
                y: 2.01 * outside_angle.sin(),
            },
            span: kicad_clippy_core::ByteSpan::new(0, 0),
        };
        assert_eq!(
            super::segment_arc_intersection_points(outside_wire, arc),
            Vec::new()
        );

        let tangent_wire = super::Segment {
            start: super::Point { x: -1.0, y: 2.0 },
            end: super::Point { x: 1.0, y: 2.0 },
            span: kicad_clippy_core::ByteSpan::new(0, 0),
        };
        let tangent = super::segment_arc_intersection_points(tangent_wire, arc);
        assert_eq!(tangent.len(), 1);
        assert!(tangent[0].x.abs() < 1.0e-9);
        assert!((tangent[0].y - 2.0).abs() < 1.0e-9);
    }
    /// Calculates analytic bezier intersection detects a curve hit missed by its flattened polyline from the supplied schematic geometry or S-expression data.
    #[test]
    fn analytic_bezier_intersection_detects_a_curve_hit_missed_by_its_flattened_polyline() {
        let controls = [(0.0, 0.0), (0.0, 10.0), (10.0, 10.0), (10.0, 0.0)];
        let curve = super::CurveCubicBez::new(
            super::CurvePoint::new(controls[0].0, controls[0].1),
            super::CurvePoint::new(controls[1].0, controls[1].1),
            super::CurvePoint::new(controls[2].0, controls[2].1),
            super::CurvePoint::new(controls[3].0, controls[3].1),
        );
        let angle_parameter = 0.453_f64;
        let point = curve.eval(angle_parameter);
        let tangent = curve.deriv().eval(angle_parameter);
        let normal_length = tangent.x.hypot(tangent.y);
        let normal = (-tangent.y / normal_length, tangent.x / normal_length);
        let wire = super::Segment {
            start: super::Point {
                x: normal.0.mul_add(-0.002, point.x),
                y: normal.1.mul_add(-0.002, point.y),
            },
            end: super::Point {
                x: normal.0.mul_add(0.002, point.x),
                y: normal.1.mul_add(0.002, point.y),
            },
            span: kicad_clippy_core::ByteSpan::new(0, 0),
        };
        let path = [
            super::PathEl::MoveTo(super::CurvePoint::new(controls[0].0, controls[0].1)),
            super::PathEl::CurveTo(
                super::CurvePoint::new(controls[1].0, controls[1].1),
                super::CurvePoint::new(controls[2].0, controls[2].1),
                super::CurvePoint::new(controls[3].0, controls[3].1),
            ),
        ];
        let sampled = super::flattened_curve_points(path, controls[0], controls[3]);
        assert!(!sampled.windows(2).any(|pair| {
            !super::segment_intersection_points(
                wire,
                super::Point {
                    x: pair[0].0,
                    y: pair[0].1,
                },
                super::Point {
                    x: pair[1].0,
                    y: pair[1].1,
                },
            )
            .is_empty()
        }));
        assert_eq!(
            super::segment_bezier_intersection_points(wire, controls).len(),
            1
        );

        let graphic = super::SchematicGraphicFact {
            kind: "bezier".to_owned(),
            uuid: None,
            bounds_mm: None,
            points_mm: sampled.clone(),
            closed: false,
            circle_mm: None,
            arc_mm: None,
            bezier_mm: Some(controls),
            stroke_width_mm: 0.254,
        };
        assert!(super::wire_crosses_sheet_graphic_with_wire_stroke(
            wire, &graphic, 0.0
        ));

        let primitive = super::SchematicPrimitiveFact {
            reference: "U1".to_owned(),
            points_mm: sampled,
            closed: false,
            circle_mm: None,
            arc_mm: None,
            bezier_mm: Some(controls),
            stroke_width_mm: 0.254,
        };
        assert!(super::wire_crosses_symbol_primitive_with_wire_stroke(
            wire,
            &primitive,
            &[],
            0.0
        ));

        let pin_wire = super::Segment {
            start: super::Point {
                x: point.x,
                y: point.y,
            },
            end: wire.end,
            span: wire.span,
        };
        let allowed_pin = super::SchematicPinFact {
            reference: "U1".to_owned(),
            number: "1".to_owned(),
            position_mm: (point.x, point.y),
        };
        assert!(!super::wire_crosses_symbol_primitive_with_wire_stroke(
            pin_wire,
            &primitive,
            &[allowed_pin],
            0.0,
        ));
    }
    /// Calculates kurbo circumcircle preserves arc direction and collinear fallback from the supplied schematic geometry or S-expression data.
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
    /// Calculates kurbo affine matches kicad cardinal symbol transforms from the supplied schematic geometry or S-expression data.
    #[test]
    fn kurbo_affine_matches_kicad_cardinal_symbol_transforms() {
        let cases = [
            (0.0, None, (2.0, -3.0)),
            (90.0, None, (-3.0, -2.0)),
            (180.0, None, (-2.0, 3.0)),
            (270.0, None, (3.0, 2.0)),
            (0.0, Some("x"), (2.0, 3.0)),
            (0.0, Some("y"), (-2.0, -3.0)),
            (90.0, Some("x"), (3.0, -2.0)),
            (90.0, Some("y"), (-3.0, 2.0)),
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
        let expected_x = 3.0_f64.mul_add(-radians.sin(), 2.0 * radians.cos());
        let expected_y = -3.0_f64.mul_add(radians.cos(), 2.0 * radians.sin());
        assert!((diagonal.0 - expected_x).abs() < 0.001);
        assert!((diagonal.1 - expected_y).abs() < 0.001);
        assert!(super::transform_symbol_point(2.0, 3.0, 0.0, Some("diagonal")).is_none());
        assert!(super::transform_symbol_point(2.0, 3.0, f64::NAN, None).is_none());
    }
    /// Defines the embedded pin constant.
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
    /// Calculates non cardinal symbol rotation resolves embedded pin and primitive geometry from the supplied schematic geometry or S-expression data.
    #[test]
    fn non_cardinal_symbol_rotation_resolves_embedded_pin_and_primitive_geometry() {
        let source = EMBEDDED_PIN
            .replace("(at 10 10 0)", "(at 10 10 45)")
            .replace(
                "(pin passive",
                "(rectangle (start 0 0) (end 2 1) (stroke (width 0.254) (type default)) (fill (type none)))\n              (pin passive",
            );
        let doc = document(&source);
        let facts = super::SchematicFacts::from_document(&doc);
        assert_eq!(facts.pins.len(), 1);
        let expected = 10.0 - 2.54 * HALF_FACTOR.sqrt();
        assert!((facts.pins[0].position_mm.0 - expected).abs() < 0.001);
        assert!((facts.pins[0].position_mm.1 - expected).abs() < 0.001);

        assert_eq!(facts.symbol_primitives.len(), 1);
        let rotated_corner = facts.symbol_primitives[0].points_mm[1];
        let expected_corner = (
            10.0 + 2.0 * HALF_FACTOR.sqrt(),
            10.0 - 2.0 * HALF_FACTOR.sqrt(),
        );
        assert!((rotated_corner.0 - expected_corner.0).abs() < 0.001);
        assert!((rotated_corner.1 - expected_corner.1).abs() < 0.001);
        assert!(
            !run_builtin_rules(&doc)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH005")
        );
    }
    /// Calculates sch013 detects wire crossing a non cardinal symbol body from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_detects_wire_crossing_a_non_cardinal_symbol_body() {
        let source = EMBEDDED_PIN
            .replace("(at 10 10 0)", "(at 10 10 45)")
            .replace(
                "(pin passive",
                "(rectangle (start 0 0) (end 2 1) (stroke (width 0.254) (type default)) (fill (type none)))\n              (pin passive",
            );
        let doc = document(&format!("{source} (wire (pts (xy 9 9) (xy 12 9)))"));

        assert!(run_builtin_rules(&doc).diagnostics.iter().any(|item| {
            item.code == "SCH013" && item.message.contains("penetrates symbol body J1")
        }));
    }
    /// Calculates sch015 accepts no connect marker on non cardinal symbol pin from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch015_accepts_no_connect_marker_on_non_cardinal_symbol_pin() {
        let source = EMBEDDED_PIN.replace("(at 10 10 0)", "(at 10 10 45)");
        let unmarked = document(&source);
        let facts = super::SchematicFacts::from_document(&unmarked);
        let (x, y) = facts.pins[0].position_mm;
        let marked = document(&format!(
            "{source} (no_connect (at {x:.3} {y:.3}) (uuid \"no-connect-rotated-pin\"))"
        ));

        assert!(
            !run_builtin_rules(&marked)
                .diagnostics
                .iter()
                .any(|item| item.code == "SCH015")
        );
    }
    /// Calculates pin wire must follow the embedded pin axis from the supplied schematic geometry or S-expression data.
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
    /// Calculates correctly aligned pin wire is accepted from the supplied schematic geometry or S-expression data.
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
    /// Calculates pin axis and position follow symbol rotation and mirroring from the supplied schematic geometry or S-expression data.
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
    /// Calculates embedded library fields are not counted as placed components from the supplied schematic geometry or S-expression data.
    #[test]
    fn embedded_library_fields_are_not_counted_as_placed_components() {
        let doc = document(EMBEDDED_PIN);
        let run = run_builtin_rules(&doc);
        assert_eq!(run.components, 1);
    }
    /// Calculates no connect marker must coincide with a placed symbol pin from the supplied schematic geometry or S-expression data.
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
    /// Calculates correctly placed no connect marker is accepted even for power symbols from the supplied schematic geometry or S-expression data.
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
    /// Calculates quoted atoms use parser decoded escape sequences from the supplied schematic geometry or S-expression data.
    #[test]
    fn quoted_atoms_use_parser_decoded_escape_sequences() {
        let doc = document(r#"(label "first\nsecond" (at 1 1 0))"#);
        let root = doc.cst().nodes.first().expect("schematic root");
        let labels = super::collect_labels(root);
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].1, "first\nsecond");
    }
    /// Calculates newstroke text height uses rendered glyph geometry from the supplied schematic geometry or S-expression data.
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
    /// Calculates text bounds use kurbo for cardinal rotation and justification from the supplied schematic geometry or S-expression data.
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
                10.0 - width * HALF_FACTOR,
                20.0 - height,
                width,
                height,
            ),
            (
                90.0,
                "left",
                "center",
                10.0 - height * HALF_FACTOR,
                20.0 - width,
                height,
                width,
            ),
            (
                90.0,
                "right",
                "center",
                10.0 - height * HALF_FACTOR,
                20.0,
                height,
                width,
            ),
            (
                90.0,
                "center",
                "center",
                10.0 - height * HALF_FACTOR,
                20.0 - width * HALF_FACTOR,
                height,
                width,
            ),
            (
                270.0,
                "left",
                "center",
                10.0 - height * HALF_FACTOR,
                20.0,
                height,
                width,
            ),
            (
                270.0,
                "right",
                "center",
                10.0 - height * HALF_FACTOR,
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
    /// Calculates text widths use kicad newstroke glyph advances from the supplied schematic geometry or S-expression data.
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
    /// Calculates unsupported unicode does not panic or shift newstroke metrics from the supplied schematic geometry or S-expression data.
    #[test]
    fn unsupported_unicode_does_not_panic_or_shift_newstroke_metrics() {
        let supported = super::newstroke_text_metrics_ratio("AH");
        let with_unsupported = super::newstroke_text_metrics_ratio("A\u{10ffff}H");
        assert!((supported.0 - with_unsupported.0).abs() < 0.001);
        assert!((supported.1 - with_unsupported.1).abs() < 0.001);
    }
    /// Calculates sch013 detects wire crossing a top level sheet circle from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_detects_wire_crossing_a_top_level_sheet_circle() {
        let doc = document(
            r"
            (circle (center 5 5) (radius 2)
              (stroke (width 0.254) (type default)) (fill (type none)))
            (wire (pts (xy 1 5) (xy 9 5)))
            ",
        );
        assert!(run_builtin_rules(&doc).diagnostics.iter().any(|item| {
            item.code == "SCH013" && item.message.contains("sheet graphic circle")
        }));
    }
    /// Calculates sch013 detects wire crossing an embedded bitmap image from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_detects_wire_crossing_an_embedded_bitmap_image() {
        let doc = document(
            r"(image (at 5 5) (scale 1)
                  (data iVBORw0KGgoAAAANSUhEUgAAAHgAAAA8CAYAAACtrX6oAAAACXBIWXMAAC4jAAAuIwF4pT92AAAAoklEQVR4nO3RAQkAMAzAsPk3vak4h5IoKHSWtPkdwFsGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcd1OAFhGyMXxbAAAAAElFTkSuQmCC))
                (wire (pts (xy 1 5) (xy 9 5)))",
        );
        assert!(run_builtin_rules(&doc).diagnostics.iter().any(|item| {
            item.code == "SCH013" && item.message.contains("embedded bitmap image")
        }));
    }
    /// Calculates sch013 uses rotated bitmap footprint instead of its axis aligned bounds from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_uses_rotated_bitmap_footprint_instead_of_its_axis_aligned_bounds() {
        /// Defines the image constant.
        const IMAGE: &str = r"
            (image (at 5 5) (rotate 45) (scale 1)
              (data iVBORw0KGgoAAAANSUhEUgAAAHgAAAA8CAYAAACtrX6oAAAACXBIWXMAAC4jAAAuIwF4pT92AAAAoklEQVR4nO3RAQkAMAzAsPk3vak4h5IoKHSWtPkdwFsGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcZ3CcwXEGxxkcd1OAFhGyMXxbAAAAAElFTkSuQmCC))
        ";

        let outside_footprint = document(&format!("{IMAGE} (wire (pts (xy 0 0.1) (xy 1 0.1)))"));
        assert!(!run_builtin_rules(&outside_footprint)
            .diagnostics
            .iter()
            .any(|item| item.code == "SCH013" && item.message.contains("embedded bitmap image")));

        let crossing_footprint = document(&format!("{IMAGE} (wire (pts (xy 0 5) (xy 10 5)))"));
        assert!(run_builtin_rules(&crossing_footprint)
            .diagnostics
            .iter()
            .any(|item| item.code == "SCH013" && item.message.contains("embedded bitmap image")));
    }
    /// Calculates diagonal wires are reported from the supplied schematic geometry or S-expression data.
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
    /// Calculates local labels must be reused from the supplied schematic geometry or S-expression data.
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
    /// Calculates repeated local label on one wire is rejected from the supplied schematic geometry or S-expression data.
    #[test]
    fn repeated_local_label_on_one_wire_is_rejected() {
        let doc = document(
            r#"(wire (pts (xy 0 0) (xy 4 0))) (label "NET" (at 1 0 0)) (label "NET" (at 3 0 0))"#,
        );
        assert!(run_builtin_rules(&doc).diagnostics.iter().any(|item| {
            item.code == "SCH002" && item.message.contains("one physical wire network")
        }));
    }
    /// Calculates repeated local label joins distinct wire islands from the supplied schematic geometry or S-expression data.
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
    /// Defines the triangle symbol constant.
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
    /// Evaluates wire obstacle uses actual triangle outline not its bounding box using the wire's geometry and the related schematic objects.
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
    /// Calculates sch013 checks free standing schematic text as a wire obstacle from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_checks_free_standing_schematic_text_as_a_wire_obstacle() {
        let obstructed = document(
            r#"
            (text "KEEP OUT" (at 10 10 0)
              (effects (font (size 1 1))))
            (wire (pts (xy 8 9.5) (xy 12 9.5)))
            "#,
        );
        let facts = super::SchematicFacts::from_document(&obstructed);
        assert_eq!(facts.texts.len(), 1);
        assert!(facts.texts[0].bounds_mm.is_some());
        assert!(
            run_builtin_rules(&obstructed)
                .diagnostics
                .iter()
                .any(|item| {
                    item.code == "SCH013"
                        && item.message.contains("free-standing schematic text")
                        && item.message.contains("KEEP OUT")
                })
        );

        let clear = document(
            r#"
            (text "KEEP OUT" (at 10 10 0)
              (effects (font (size 1 1))))
            (wire (pts (xy 8 11) (xy 12 11)))
            "#,
        );
        assert!(!run_builtin_rules(&clear).diagnostics.iter().any(|item| {
            item.code == "SCH013" && item.message.contains("free-standing schematic text")
        }));
    }
    /// Calculates sch013 checks embedded symbol text and text box obstacles from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_checks_embedded_symbol_text_and_text_box_obstacles() {
        let doc = document(
            r#"
            (lib_symbols
              (symbol "Test:Annotated"
                (symbol "Annotated_1_1"
                  (text "SENSE" (at 0 0 0) (effects (font (size 1 1))))
                  (text_box "NOTE" (at 5 0) (size 2 2)
                    (margins 0.2 0.2 0.2 0.2)
                    (stroke (width 0.1)) (effects (font (size 1 1))))
                )
              )
            )
            (symbol (lib_id "Test:Annotated") (at 10 10 0) (unit 1)
              (property "Reference" "U1" (at 10 10 0) (effects (font (size 1 1))))
            )
            (wire (pts (xy 9 10.5) (xy 11 10.5)))
            (wire (pts (xy 14 9) (xy 18 9)))
            "#,
        );
        let facts = super::SchematicFacts::from_document(&doc);
        assert_eq!(facts.symbol_texts.len(), 2);
        assert!(facts.symbol_texts.iter().any(|text| text.kind == "text"));
        assert!(
            facts
                .symbol_texts
                .iter()
                .any(|text| text.kind == "text_box")
        );
        let diagnostics = run_builtin_rules(&doc).diagnostics;
        assert!(diagnostics.iter().any(|item| {
            item.code == "SCH013"
                && item.message.contains("embedded symbol text")
                && item.message.contains("U1")
                && item.message.contains("SENSE")
        }));
        assert!(diagnostics.iter().any(|item| {
            item.code == "SCH013"
                && item.message.contains("embedded symbol text_box")
                && item.message.contains("U1")
                && item.message.contains("NOTE")
        }));
    }
    /// Calculates sch013 transforms embedded symbol text obstacles with symbol mirrors and rotation from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_transforms_embedded_symbol_text_obstacles_with_symbol_mirrors_and_rotation() {
        let source = r#"
            (lib_symbols
              (symbol "Test:Annotated"
                (symbol "Annotated_1_1"
                  (text "ROTATED" (at 1 0 0) (effects (font (size 1 1))))
                )
              )
            )
            (symbol (lib_id "Test:Annotated") (at 10 10 90) (mirror x) (unit 1)
              (property "Reference" "U1" (at 10 10 0) (effects (font (size 1 1))))
            )
            (wire (pts (xy 9.5 10.7) (xy 10.5 10.7)))
        "#;
        let doc = document(source);
        let facts = super::SchematicFacts::from_document(&doc);
        assert_eq!(facts.symbol_texts.len(), 1);
        let bounds = facts.symbol_texts[0].bounds_mm;
        assert!(bounds.0.is_finite() && bounds.1.is_finite());
        assert!(bounds.2 > bounds.0 && bounds.3 > bounds.1);
        assert!(run_builtin_rules(&doc).diagnostics.iter().any(|item| {
            item.code == "SCH013"
                && item.message.contains("embedded symbol text")
                && item.message.contains("ROTATED")
        }));
    }
    /// Calculates sch013 checks declared stroke width for open sheet paths from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_checks_declared_stroke_width_for_open_sheet_paths() {
        let near = document(
            r"
            (polyline (pts (xy 0 -1) (xy 0 1))
              (stroke (width 0.4)) (fill (type none)))
            (wire (pts (xy 0.15 -0.5) (xy 0.15 0.5)))
            ",
        );
        assert!(run_builtin_rules(&near).diagnostics.iter().any(|item| {
            item.code == "SCH013" && item.message.contains("sheet graphic polyline")
        }));

        let clear = document(
            r"
            (polyline (pts (xy 0 -1) (xy 0 1))
              (stroke (width 0.4)) (fill (type none)))
            (wire (pts (xy 0.21 -0.5) (xy 0.21 0.5)))
            ",
        );
        assert!(!run_builtin_rules(&clear).diagnostics.iter().any(|item| {
            item.code == "SCH013" && item.message.contains("sheet graphic polyline")
        }));
    }
    /// Calculates sch013 checks declared stroke width for embedded symbol paths from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_checks_declared_stroke_width_for_embedded_symbol_paths() {
        /// Defines the embedded stroke symbol constant.
        const EMBEDDED_STROKE_SYMBOL: &str = r#"
            (lib_symbols
              (symbol "Test:Stroked"
                (symbol "Stroked_1_1"
                  (polyline (pts (xy 0 -1) (xy 0 1))
                    (stroke (width 0.4)) (fill (type none)))
                )
              )
            )
            (symbol (lib_id "Test:Stroked") (at 10 10 0) (unit 1)
              (property "Reference" "U1" (at 10 10 0) (effects (font (size 1 1))))
            )
        "#;
        let near = document(&format!(
            "{EMBEDDED_STROKE_SYMBOL} (wire (pts (xy 10.15 9.5) (xy 10.15 10.5)))"
        ));
        assert!(
            run_builtin_rules(&near)
                .diagnostics
                .iter()
                .any(|item| { item.code == "SCH013" && item.message.contains("symbol body U1") })
        );

        let clear = document(&format!(
            "{EMBEDDED_STROKE_SYMBOL} (wire (pts (xy 10.21 9.5) (xy 10.21 10.5)))"
        ));
        assert!(
            !run_builtin_rules(&clear)
                .diagnostics
                .iter()
                .any(|item| { item.code == "SCH013" && item.message.contains("symbol body U1") })
        );
    }
    /// Calculates sch013 checks embedded symbol arc and bezier stroke clearance from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_checks_embedded_symbol_arc_and_bezier_stroke_clearance() {
        let arc = super::SchematicArcFact {
            center_mm: (0.0, 0.0),
            radius_mm: 2.0,
            start_angle_rad: 0.0,
            sweep_rad: std::f64::consts::FRAC_PI_2,
        };
        let angle = 0.37_f64;
        let radial_point = |radius: f64| super::Point {
            x: radius * angle.cos(),
            y: radius * angle.sin(),
        };
        let arc_wire = super::Segment {
            start: radial_point(2.1),
            end: radial_point(2.2),
            span: kicad_clippy_core::ByteSpan::new(0, 0),
        };
        let arc_primitive = super::SchematicPrimitiveFact {
            reference: "U1".to_owned(),
            points_mm: super::sample_arc_through_three_points(
                (2.0, 0.0),
                (2.0 * HALF_FACTOR.sqrt(), 2.0 * HALF_FACTOR.sqrt()),
                (0.0, 2.0),
            ),
            closed: false,
            circle_mm: None,
            arc_mm: Some(arc),
            bezier_mm: None,
            stroke_width_mm: 0.3,
        };
        assert!(super::wire_crosses_symbol_primitive_with_wire_stroke(
            arc_wire,
            &arc_primitive,
            &[],
            0.0,
        ));

        let controls = [(0.0, 0.0), (0.0, 10.0), (10.0, 10.0), (10.0, 0.0)];
        let curve = super::CurveCubicBez::new(
            super::CurvePoint::new(controls[0].0, controls[0].1),
            super::CurvePoint::new(controls[1].0, controls[1].1),
            super::CurvePoint::new(controls[2].0, controls[2].1),
            super::CurvePoint::new(controls[3].0, controls[3].1),
        );
        let point = curve.eval(0.453);
        let tangent = curve.deriv().eval(0.453);
        let tangent_length = tangent.x.hypot(tangent.y);
        let tangent = (tangent.x / tangent_length, tangent.y / tangent_length);
        let normal = (-tangent.1, tangent.0);
        let bezier_wire = super::Segment {
            start: super::Point {
                x: tangent.0.mul_add(-0.02, normal.0.mul_add(0.1, point.x)),
                y: tangent.1.mul_add(-0.02, normal.1.mul_add(0.1, point.y)),
            },
            end: super::Point {
                x: tangent.0.mul_add(0.02, normal.0.mul_add(0.1, point.x)),
                y: tangent.1.mul_add(0.02, normal.1.mul_add(0.1, point.y)),
            },
            span: kicad_clippy_core::ByteSpan::new(0, 0),
        };
        let bezier_primitive = super::SchematicPrimitiveFact {
            reference: "U1".to_owned(),
            points_mm: Vec::new(),
            closed: false,
            circle_mm: None,
            arc_mm: None,
            bezier_mm: Some(controls),
            stroke_width_mm: 0.3,
        };
        assert!(super::wire_crosses_symbol_primitive_with_wire_stroke(
            bezier_wire,
            &bezier_primitive,
            &[],
            0.0,
        ));
        let narrow_stroke = super::SchematicPrimitiveFact {
            stroke_width_mm: 0.18,
            ..bezier_primitive
        };
        assert!(!super::wire_crosses_symbol_primitive_with_wire_stroke(
            bezier_wire,
            &narrow_stroke,
            &[],
            0.0,
        ));
    }
    /// Calculates sch013 pin endpoint exemption is limited to local stroke contact from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_pin_endpoint_exemption_is_limited_to_local_stroke_contact() {
        let primitive = super::SchematicPrimitiveFact {
            reference: "U1".to_owned(),
            points_mm: vec![(0.0, 0.1), (2.0, 0.1)],
            closed: false,
            circle_mm: None,
            arc_mm: None,
            bezier_mm: None,
            stroke_width_mm: 0.4,
        };
        let wire = super::Segment {
            start: super::Point { x: 0.0, y: 0.0 },
            end: super::Point { x: 2.0, y: 0.0 },
            span: kicad_clippy_core::ByteSpan::new(0, 0),
        };
        let pin = super::SchematicPinFact {
            reference: "U1".to_owned(),
            number: "1".to_owned(),
            position_mm: (0.0, 0.0),
        };
        assert!(super::wire_crosses_symbol_primitive_with_wire_stroke(
            wire,
            &primitive,
            &[pin],
            0.0,
        ));
    }
    /// Calculates sch013 uses curve distance for bezier stroke clearance from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_uses_curve_distance_for_bezier_stroke_clearance() {
        let controls = [(0.0, 0.0), (0.0, 10.0), (10.0, 10.0), (10.0, 0.0)];
        let curve = super::CurveCubicBez::new(
            super::CurvePoint::new(controls[0].0, controls[0].1),
            super::CurvePoint::new(controls[1].0, controls[1].1),
            super::CurvePoint::new(controls[2].0, controls[2].1),
            super::CurvePoint::new(controls[3].0, controls[3].1),
        );
        let parameter = 0.453_f64;
        let point = curve.eval(parameter);
        let tangent = curve.deriv().eval(parameter);
        let tangent_length = tangent.x.hypot(tangent.y);
        let tangent = (tangent.x / tangent_length, tangent.y / tangent_length);
        let normal = (-tangent.1, tangent.0);
        let offset = 0.1_f64;
        let along = 0.02_f64;
        let wire = super::Segment {
            start: super::Point {
                x: tangent.0.mul_add(-along, normal.0.mul_add(offset, point.x)),
                y: tangent.1.mul_add(-along, normal.1.mul_add(offset, point.y)),
            },
            end: super::Point {
                x: tangent.0.mul_add(along, normal.0.mul_add(offset, point.x)),
                y: tangent.1.mul_add(along, normal.1.mul_add(offset, point.y)),
            },
            span: kicad_clippy_core::ByteSpan::new(0, 0),
        };
        let graphic = super::SchematicGraphicFact {
            kind: "bezier".to_owned(),
            uuid: None,
            bounds_mm: None,
            points_mm: Vec::new(),
            closed: false,
            circle_mm: None,
            arc_mm: None,
            bezier_mm: Some(controls),
            stroke_width_mm: 0.3,
        };
        assert!(super::wire_crosses_sheet_graphic_with_wire_stroke(
            wire, &graphic, 0.0
        ));

        let narrow_stroke = super::SchematicGraphicFact {
            stroke_width_mm: 0.18,
            ..graphic
        };
        assert!(!super::wire_crosses_sheet_graphic_with_wire_stroke(
            wire,
            &narrow_stroke,
            0.0
        ));
    }
    /// Calculates sch013 applies arc stroke clearance to the selected sweep from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_applies_arc_stroke_clearance_to_the_selected_sweep() {
        let arc = super::SchematicArcFact {
            center_mm: (0.0, 0.0),
            radius_mm: 2.0,
            start_angle_rad: 0.0,
            sweep_rad: std::f64::consts::FRAC_PI_2,
        };
        let midpoint = (2.0 * HALF_FACTOR.sqrt(), 2.0 * HALF_FACTOR.sqrt());
        let points = super::sample_arc_through_three_points((2.0, 0.0), midpoint, (0.0, 2.0));
        let angle = 0.37_f64;
        let radial_point = |radius: f64| super::Point {
            x: radius * angle.cos(),
            y: radius * angle.sin(),
        };
        let wire = super::Segment {
            start: radial_point(2.1),
            end: radial_point(2.2),
            span: kicad_clippy_core::ByteSpan::new(0, 0),
        };
        let graphic = super::SchematicGraphicFact {
            kind: "arc".to_owned(),
            uuid: None,
            bounds_mm: None,
            points_mm: points,
            closed: false,
            circle_mm: None,
            arc_mm: Some(arc),
            bezier_mm: None,
            stroke_width_mm: 0.3,
        };
        assert!(super::wire_crosses_sheet_graphic_with_wire_stroke(
            wire, &graphic, 0.0
        ));

        let narrow_stroke = super::SchematicGraphicFact {
            stroke_width_mm: 0.18,
            ..graphic
        };
        assert!(!super::wire_crosses_sheet_graphic_with_wire_stroke(
            wire,
            &narrow_stroke,
            0.0
        ));
    }
    /// Evaluates wire obstacle uses exact intersections for top level three point arcs using the wire's geometry and the related schematic objects.
    #[test]
    fn wire_obstacle_uses_exact_intersections_for_top_level_three_point_arcs() {
        let doc = document(
            r"
            (arc (start 2 0) (mid 1.4142135623730951 1.4142135623730951) (end 0 2)
              (stroke (width 0.254)) (fill (type none)))
            (wire (pts
              (xy 1.864654691212069 0.723230863929924)
              (xy 1.8739779646681292 0.7268470182495735)))
            ",
        );
        assert!(
            run_builtin_rules(&doc).diagnostics.iter().any(|item| {
                item.code == "SCH013" && item.message.contains("sheet graphic arc")
            })
        );
    }
    /// Evaluates wire obstacle uses exact intersections for top level cubic beziers using the wire's geometry and the related schematic objects.
    #[test]
    fn wire_obstacle_uses_exact_intersections_for_top_level_cubic_beziers() {
        let doc = document(
            r"
            (bezier
              (pts (xy 0 0) (xy 0 10) (xy 10 10) (xy 10 0))
              (stroke (width 0.254)) (fill (type none)))
            (wire (pts
              (xy 4.297449166763041 7.431765034435725)
              (xy 4.296703753236958 7.435694965564273)))
            ",
        );
        assert!(run_builtin_rules(&doc).diagnostics.iter().any(|item| {
            item.code == "SCH013" && item.message.contains("sheet graphic bezier")
        }));
    }
    /// Evaluates wire obstacle detects wire near the true three point arc path using the wire's geometry and the related schematic objects.
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
    /// Calculates arc midpoint selects the correct sweep direction from the supplied schematic geometry or S-expression data.
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
    /// Evaluates wire obstacle detects actual cubic bezier curve using the wire's geometry and the related schematic objects.
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
    /// Calculates bezier control polygon is not treated as the curve from the supplied schematic geometry or S-expression data.
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
    /// Calculates sch013 combines explicit wire and sheet graphic stroke widths from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_combines_explicit_wire_and_sheet_graphic_stroke_widths() {
        let combined = document(
            r"
            (polyline (pts (xy 0 -1) (xy 0 1))
              (stroke (width 0.2)) (fill (type none)))
            (wire (pts (xy 0.19 -0.5) (xy 0.19 0.5))
              (stroke (width 0.2) (type default)))
            ",
        );
        assert!(run_builtin_rules(&combined).diagnostics.iter().any(|item| {
            item.code == "SCH013" && item.message.contains("sheet graphic polyline")
        }));

        let zero_width = document(
            r"
            (polyline (pts (xy 0 -1) (xy 0 1))
              (stroke (width 0.2)) (fill (type none)))
            (wire (pts (xy 0.19 -0.5) (xy 0.19 0.5))
              (stroke (width 0) (type default)))
            ",
        );
        assert!(
            !run_builtin_rules(&zero_width)
                .diagnostics
                .iter()
                .any(|item| {
                    item.code == "SCH013" && item.message.contains("sheet graphic polyline")
                })
        );

        let netclass_default = document(
            r"
            (polyline (pts (xy 0 -1) (xy 0 1))
              (stroke (width 0.2)) (fill (type none)))
            (wire (pts (xy 0.19 -0.5) (xy 0.19 0.5)))
            ",
        );
        assert!(
            !run_builtin_rules(&netclass_default)
                .diagnostics
                .iter()
                .any(|item| {
                    item.code == "SCH013" && item.message.contains("sheet graphic polyline")
                })
        );

        let clear = document(
            r"
            (polyline (pts (xy 0 -1) (xy 0 1))
              (stroke (width 0.2)) (fill (type none)))
            (wire (pts (xy 0.21 -0.5) (xy 0.21 0.5))
              (stroke (width 0.2) (type default)))
            ",
        );
        assert!(!run_builtin_rules(&clear).diagnostics.iter().any(|item| {
            item.code == "SCH013" && item.message.contains("sheet graphic polyline")
        }));
    }
    /// Calculates sch013 uses project default wire width when no assignment override exists from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_uses_project_default_wire_width_when_no_assignment_override_exists() {
        let schematic = document(
            r"
            (polyline (pts (xy 0 -1) (xy 0 1))
              (stroke (width 0.2)) (fill (type none)))
            (wire (pts (xy 0.19 -0.5) (xy 0.19 0.5)))
            ",
        );
        let result = crate::RuleEngine::with_builtins()
            .run_with_default_wire_stroke_width(&schematic, Some(0.2));
        assert!(result.diagnostics.iter().any(|item| {
            item.code == "SCH013" && item.message.contains("sheet graphic polyline")
        }));
    }
    /// Calculates sch013 does not apply project default when graphical assignments exist from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_does_not_apply_project_default_when_graphical_assignments_exist() {
        for assignment in [
            r#"(directive_label "HighSpeed" (property "Net Class" "HighSpeed"))"#,
            r"(rule_area (pts (xy 0 0) (xy 1 1)))",
            r#"(label "NET" (at 0 0 0) (property "Net Class" "HighSpeed"))"#,
        ] {
            let schematic = document(&format!(
                r"
                (polyline (pts (xy 0 -1) (xy 0 1))
                  (stroke (width 0.2)) (fill (type none)))
                (wire (pts (xy 0.19 -0.5) (xy 0.19 0.5)))
                {assignment}
                "
            ));
            let result = crate::RuleEngine::with_builtins()
                .run_with_default_wire_stroke_width(&schematic, Some(0.2));
            assert!(!result.diagnostics.iter().any(|item| {
                item.code == "SCH013" && item.message.contains("sheet graphic polyline")
            }));
        }
    }
    /// Calculates sch013 combines explicit wire and embedded symbol strokes from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_combines_explicit_wire_and_embedded_symbol_strokes() {
        let primitive = super::SchematicPrimitiveFact {
            reference: "U1".to_owned(),
            points_mm: vec![(0.0, -1.0), (0.0, 1.0)],
            closed: false,
            circle_mm: None,
            arc_mm: None,
            bezier_mm: None,
            stroke_width_mm: 0.2,
        };
        let wire = super::Segment {
            start: super::Point { x: 0.19, y: -0.5 },
            end: super::Point { x: 0.19, y: 0.5 },
            span: kicad_clippy_core::ByteSpan::new(0, 0),
        };
        assert!(!super::wire_crosses_symbol_primitive_with_wire_stroke(
            wire,
            &primitive,
            &[],
            0.0
        ));
        assert!(super::wire_crosses_symbol_primitive_with_wire_stroke(
            wire,
            &primitive,
            &[],
            0.2,
        ));
    }
    /// Calculates sch013 explicit wire width expands circle stroke ring from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_explicit_wire_width_expands_circle_stroke_ring() {
        let center = super::Point { x: 0.0, y: 0.0 };
        let wire = super::Segment {
            start: super::Point { x: 2.15, y: -0.05 },
            end: super::Point { x: 2.15, y: 0.05 },
            span: kicad_clippy_core::ByteSpan::new(0, 0),
        };
        assert!(!super::wire_within_circle_stroke(
            wire, center, 2.0, 0.2, 0.0
        ));
        assert!(super::wire_within_circle_stroke(
            wire, center, 2.0, 0.2, 0.2
        ));
    }
    /// Calculates sch013 checks rotated ellipse stroke proximity and preserves clearance from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_checks_rotated_ellipse_stroke_proximity_and_preserves_clearance() {
        let near_ellipse = document(
            r"
            (ellipse (center 0 0) (major_radius 2) (minor_radius 1)
              (rotation_angle 45) (stroke (width 0.2)) (fill (type none)))
            (wire (pts (xy 1.3 1.41421356237) (xy 1.5 1.41421356237)))
            ",
        );
        assert!(
            run_builtin_rules(&near_ellipse)
                .diagnostics
                .iter()
                .any(|item| {
                    item.code == "SCH013" && item.message.contains("sheet graphic ellipse")
                })
        );

        let clear_ellipse = document(
            r"
            (ellipse (center 0 0) (major_radius 2) (minor_radius 1)
              (rotation_angle 45) (stroke (width 0.2)) (fill (type none)))
            (wire (pts (xy 1.3 1.7) (xy 1.5 1.7)))
            ",
        );
        assert!(
            !run_builtin_rules(&clear_ellipse)
                .diagnostics
                .iter()
                .any(|item| {
                    item.code == "SCH013" && item.message.contains("sheet graphic ellipse")
                })
        );
    }
    /// Calculates sch013 checks elliptical arc stroke without blocking its interior from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_checks_elliptical_arc_stroke_without_blocking_its_interior() {
        let on_arc = document(
            r"
            (ellipse_arc (center 0 0) (major_radius 2) (minor_radius 1)
              (rotation_angle 0) (start_angle 0) (end_angle 90)
              (stroke (width 0.2)) (fill (type none)))
            (wire (pts (xy 1.3 0.70710678118) (xy 1.5 0.70710678118)))
            ",
        );
        assert!(run_builtin_rules(&on_arc).diagnostics.iter().any(|item| {
            item.code == "SCH013" && item.message.contains("sheet graphic ellipse_arc")
        }));

        let inside_open_arc = document(
            r"
            (ellipse_arc (center 0 0) (major_radius 2) (minor_radius 1)
              (rotation_angle 0) (start_angle 0) (end_angle 90)
              (stroke (width 0.2)) (fill (type none)))
            (wire (pts (xy 0.4 0.25) (xy 0.6 0.25)))
            ",
        );
        assert!(
            !run_builtin_rules(&inside_open_arc)
                .diagnostics
                .iter()
                .any(|item| {
                    item.code == "SCH013" && item.message.contains("sheet graphic ellipse_arc")
                })
        );
    }
    /// Calculates sch013 checks circle stroke proximity and preserves interior detection from the supplied schematic geometry or S-expression data.
    #[test]
    fn sch013_checks_circle_stroke_proximity_and_preserves_interior_detection() {
        let near_sheet_circle = document(
            r"
            (circle (center 0 0) (radius 2)
              (stroke (width 0.4)) (fill (type none)))
            (wire (pts (xy 2.1 -0.05) (xy 2.15 0.05)))
            ",
        );
        assert!(
            run_builtin_rules(&near_sheet_circle)
                .diagnostics
                .iter()
                .any(|item| {
                    item.code == "SCH013" && item.message.contains("sheet graphic circle")
                })
        );

        let clear_sheet_circle = document(
            r"
            (circle (center 0 0) (radius 2)
              (stroke (width 0.4)) (fill (type none)))
            (wire (pts (xy 2.25 -0.05) (xy 2.3 0.05)))
            ",
        );
        assert!(
            !run_builtin_rules(&clear_sheet_circle)
                .diagnostics
                .iter()
                .any(|item| {
                    item.code == "SCH013" && item.message.contains("sheet graphic circle")
                })
        );

        let near_symbol_circle = document(
            r#"
            (lib_symbols
              (symbol "Test:Circle"
                (symbol "Circle_1_1"
                  (circle (center 0 0) (radius 2)
                    (stroke (width 0.4)) (fill (type none)))
                )
              )
            )
            (symbol (lib_id "Test:Circle") (at 10 10 0) (unit 1)
              (property "Reference" "U1" (at 10 10 0) (effects (font (size 1 1))))
            )
            (wire (pts (xy 12.1 9.95) (xy 12.15 10.05)))
            "#,
        );
        assert!(
            run_builtin_rules(&near_symbol_circle)
                .diagnostics
                .iter()
                .any(|item| { item.code == "SCH013" && item.message.contains("symbol body U1") })
        );

        let clear_symbol_circle = document(
            r#"
            (lib_symbols
              (symbol "Test:Circle"
                (symbol "Circle_1_1"
                  (circle (center 0 0) (radius 2)
                    (stroke (width 0.4)) (fill (type none)))
                )
              )
            )
            (symbol (lib_id "Test:Circle") (at 10 10 0) (unit 1)
              (property "Reference" "U1" (at 10 10 0) (effects (font (size 1 1))))
            )
            (wire (pts (xy 12.25 9.95) (xy 12.3 10.05)))
            "#,
        );
        assert!(
            !run_builtin_rules(&clear_symbol_circle)
                .diagnostics
                .iter()
                .any(|item| { item.code == "SCH013" && item.message.contains("symbol body U1") })
        );

        let interior_symbol_circle = document(
            r#"
            (lib_symbols
              (symbol "Test:Circle"
                (symbol "Circle_1_1"
                  (circle (center 0 0) (radius 2)
                    (stroke (width 0.4)) (fill (type none)))
                )
              )
            )
            (symbol (lib_id "Test:Circle") (at 10 10 0) (unit 1)
              (property "Reference" "U1" (at 10 10 0) (effects (font (size 1 1))))
            )
            (wire (pts (xy 9 10) (xy 11 10)))
            "#,
        );
        assert!(
            run_builtin_rules(&interior_symbol_circle)
                .diagnostics
                .iter()
                .any(|item| { item.code == "SCH013" && item.message.contains("symbol body U1") })
        );
    }
    /// Evaluates wire obstacle detects circle primitive interiors using the wire's geometry and the related schematic objects.
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
    /// Evaluates wire may start at its intended symbol pin without body penetration using the wire's geometry and the related schematic objects.
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
    /// Evaluates wire obstacle rejects intermediate pins visible fields and text boxes using the wire's geometry and the related schematic objects.
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
    /// Calculates sweep line reports crossing wires with the intersection location from the supplied schematic geometry or S-expression data.
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
    /// Calculates sweep line requires an explicit marker for t junctions from the supplied schematic geometry or S-expression data.
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
    /// Evaluates wire overlap is rejected using the wire's geometry and the related schematic objects.
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
    /// Evaluates wire through label glyph is rejected using the wire's geometry and the related schematic objects.
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
    /// Calculates undersized text box is rejected from the supplied schematic geometry or S-expression data.
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
    /// Calculates free standing text is rejected from the supplied schematic geometry or S-expression data.
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
    /// Calculates multiline unbordered text box is rejected from the supplied schematic geometry or S-expression data.
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
    /// Calculates overlapping label glyphs are rejected from the supplied schematic geometry or S-expression data.
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
    /// Calculates visible symbol fields may not overlap each other from the supplied schematic geometry or S-expression data.
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
    /// Calculates visible symbol fields may not overlap annotation boxes from the supplied schematic geometry or S-expression data.
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
    /// Calculates visible reference field may not be embedded in symbol body from the supplied schematic geometry or S-expression data.
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
    /// Calculates value field fully inside closed symbol body is allowed from the supplied schematic geometry or S-expression data.
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
    /// Calculates visible symbol field may not overlap wire segment or pin from the supplied schematic geometry or S-expression data.
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
    /// Calculates visible symbol field may not overlap label glyph from the supplied schematic geometry or S-expression data.
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
    /// Calculates separated visible symbol fields are accepted from the supplied schematic geometry or S-expression data.
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
    /// Calculates label glyph may not overlap symbol field from the supplied schematic geometry or S-expression data.
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
    /// Calculates label glyph may not overlap annotation box from the supplied schematic geometry or S-expression data.
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
    /// Calculates same label cannot be local and hierarchical from the supplied schematic geometry or S-expression data.
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
