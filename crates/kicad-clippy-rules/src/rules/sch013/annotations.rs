//! Annotation and text-family obstacle checks for SCH013.

use super::*;

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
pub(super) fn wire_text_annotation_obstacle_diagnostics(
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

