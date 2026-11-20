//! Checks `SCH008` annotation text-box dimensions, overlap, margins, and wire penetration.

use super::Bounds;
use super::GEOMETRY_TOLERANCE_MM;
use super::Segment;
use super::TEXT_BOX_EQUAL_PADDING_MM;
use super::UNBORDERED_TEXT_BOX_EXTRA_WIDTH_MM;
use super::at_span;
use super::atom_value;
use super::bounds_overlap;
use super::child_list;
use super::collect_nodes;
use super::line_hits_bounds;
use super::list_items;
use super::number;
use super::required_text_box_size;
use super::span_of;
use crate::core::BuiltinRuleId;
use crate::core::ByteSpan;
use crate::core::Diagnostic;
use kiutils_sexpr::Node;

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
pub(super) fn check_text_box_layout(root: &Node, segments: &[Segment]) -> Vec<Diagnostic> {
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
/// The parsed text-box record and any associated diagnostics; the record is `None` when required geometry or font metrics are missing or invalid.
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
/// Validated text-box content, font, padding, stroke, bounds, and source span, or `None` with one diagnostic when a required field is malformed or unavailable.
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

/// Concrete implementation of built-in rule 008.
pub(super) struct Sch008;
impl<'a> crate::rules::Rule<crate::rules::RuleContext<'a>> for Sch008 {
    /// Returns the stable 008 identifier.
    ///
    /// # Returns
    ///
    /// The stable SCH008 identifier used to associate diagnostics with this rule.
    fn id(&self) -> BuiltinRuleId {
        BuiltinRuleId::Sch008
    }
    /// Evaluates annotation text-box dimensions, overlap, margins, and wire penetration.
    ///
    /// # Arguments
    ///
    /// * `context` — the parsed schematic and normalized wire segments.
    ///
    /// # Returns
    ///
    /// Diagnostics for annotation text-box dimensions, overlap, margins, and wire penetration.
    fn check(&self, context: &crate::rules::RuleContext<'a>) -> Vec<Diagnostic> {
        let Some(root) = context.document().cst().nodes.first() else {
            return Vec::new();
        };
        check_text_box_layout(root, context.segments)
    }
}
