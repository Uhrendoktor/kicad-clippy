//! Checks `SCH018` overlap between estimated visible label-glyph bounds.

use super::at_span;
use super::atom_value;
use super::bounds_overlap;
use super::children;
use super::collect_label_nodes;
use super::collect_nodes;
use super::list_items;
use super::sch010::{property_is_hidden, text_box_bounds};
use super::text_visual_bounds;
use kicad_clippy_core::BuiltinRuleId;
use kicad_clippy_core::Diagnostic;
use kiutils_sexpr::Node;

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

/// Concrete implementation of built-in rule 018.
pub(super) struct Sch018;
impl<'a> crate::Rule<crate::RuleContext<'a>> for Sch018 {
    /// Returns the stable 018 identifier.
    ///
    /// # Returns
    ///
    /// The stable SCH018 identifier used to associate diagnostics with this rule.
    fn id(&self) -> BuiltinRuleId {
        BuiltinRuleId::Sch018
    }
    /// Evaluates overlaps between visible label glyphs and other text objects.
    ///
    /// # Arguments
    ///
    /// * `context` — the parsed schematic containing labels and visible text objects.
    ///
    /// # Returns
    ///
    /// Diagnostics for overlaps between visible label glyphs and other text objects.
    fn check(&self, context: &crate::RuleContext<'a>) -> Vec<Diagnostic> {
        let Some(root) = context.document().cst().nodes.first() else {
            return Vec::new();
        };
        check_label_text_collisions(root)
    }
}
