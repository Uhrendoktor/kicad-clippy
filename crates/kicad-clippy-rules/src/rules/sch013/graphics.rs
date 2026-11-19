//! Symbol, sheet, bus, table, and image obstacle checks for SCH013.

use super::{at_segment, wire_crosses_bitmap_image, wire_crosses_sheet_graphic_with_wire_stroke, wire_crosses_symbol_primitive_with_wire_stroke, BuiltinRuleId, Diagnostic, SchematicBusObstacleFact, SchematicFacts, SchematicGraphicFact, SchematicSheetFact, Segment, BTreeSet};

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
pub(super) fn bus_obstacle_diagnostics(
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
pub(super) fn sheet_frame_obstacle_diagnostics(
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
pub(super) fn table_stroke_obstacle_diagnostics(
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
/// Checks one wire against selected-unit embedded symbol primitives.
///
/// A symbol is reported at most once per wire, even when multiple selected-unit
/// primitives from that symbol are intersected. This preserves the historical
/// diagnostic aggregation behavior.
///
/// # Arguments
///
/// * `wire_index` — Stable index of the wire within the current segment collection.
/// * `segment` — Wire segment whose centerline and combined stroke envelope are tested against selected-unit symbol-body geometry.
/// * `facts` — Normalized schematic facts containing embedded symbol primitives and pin anchors.
/// * `wire_stroke_width_mm` — Effective stroke width for this wire, in millimetres.
///
/// # Returns
///
/// SCH013 diagnostics for symbol bodies penetrated by the wire.
pub(super) fn symbol_primitive_obstacle_diagnostics(
    wire_index: usize,
    segment: Segment,
    facts: &SchematicFacts,
    wire_stroke_width_mm: f64,
) -> Vec<Diagnostic> {
    let mut reported_symbols = BTreeSet::new();
    let mut diagnostics = Vec::new();
    for primitive in &facts.symbol_primitives {
        if !reported_symbols.contains(&primitive.reference)
            && wire_crosses_symbol_primitive_with_wire_stroke(
                segment,
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
                segment,
            ));
        }
    }
    diagnostics
}

/// Checks one wire against top-level schematic vector graphics.
///
/// # Arguments
///
/// * `wire_index` — Stable index of the wire within the current segment collection.
/// * `segment` — Wire segment whose stroke envelope is compared with transformed graphic paths, circles, and closed-body regions.
/// * `facts` — Normalized schematic facts containing top-level graphic primitives.
/// * `wire_stroke_width_mm` — Effective stroke width for this wire, in millimetres.
///
/// # Returns
///
/// SCH013 diagnostics for sheet graphics penetrated or approached within the combined stroke envelope.
pub(super) fn sheet_graphic_obstacle_diagnostics(
    wire_index: usize,
    segment: Segment,
    facts: &SchematicFacts,
    wire_stroke_width_mm: f64,
) -> Vec<Diagnostic> {
    facts
        .graphics
        .iter()
        .filter(|graphic| {
            wire_crosses_sheet_graphic_with_wire_stroke(segment, graphic, wire_stroke_width_mm)
        })
        .map(|graphic| {
            at_segment(
                BuiltinRuleId::Sch013,
                &format!(
                    "wire {wire_index} penetrates sheet graphic {} {:?}",
                    graphic.kind, graphic.uuid
                ),
                segment,
            )
        })
        .collect()
}

/// Checks one wire against the actual quadrilateral footprint of embedded bitmap images.
///
/// # Arguments
///
/// * `wire_index` — Stable index of the wire within the current segment collection.
/// * `segment` — Wire segment tested against each embedded image's orientation-aware quadrilateral footprint.
/// * `facts` — Normalized schematic facts containing the orientation-aware bitmap image corners.
///
/// # Returns
///
/// SCH013 diagnostics for embedded PNG/JPEG images whose footprint is crossed by the wire.
pub(super) fn bitmap_image_obstacle_diagnostics(
    wire_index: usize,
    segment: Segment,
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    facts
        .images
        .iter()
        .filter(|image| {
            image
                .corners_mm
                .is_some_and(|corners| wire_crosses_bitmap_image(segment, &corners))
        })
        .map(|image| {
            at_segment(
                BuiltinRuleId::Sch013,
                &format!(
                    "wire {wire_index} penetrates embedded bitmap image {:?}",
                    image.uuid
                ),
                segment,
            )
        })
        .collect()
}
