//! Checks `SCH011` multiline block annotations for a visible border.

use super::at_span;
use super::atom_value;
use super::child_list;
use super::collect_nodes;
use super::list_items;
use super::number;
use super::span_of;
use kicad_clippy_core::BuiltinRuleId;
use kicad_clippy_core::Diagnostic;
use kiutils_sexpr::Node;

/// Checks that multiline text boxes have a visible border.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// SCH011 diagnostics for multiline block annotations that do not have a visible border.
pub(super) fn check_block_annotation_structure(root: &Node) -> Vec<Diagnostic> {
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

/// Concrete implementation of built-in rule 011.
pub(super) struct Sch011;
impl<'a> crate::Rule<crate::RuleContext<'a>> for Sch011 {
    /// Returns the stable 011 identifier.
    ///
    /// # Returns
    ///
    /// The stable 011 identifier.
    fn id(&self) -> BuiltinRuleId {
        BuiltinRuleId::Sch011
    }
    /// Evaluates multiline annotation border and structure requirements.
    ///
    /// # Arguments
    ///
    /// * context — the parsed schematic containing annotation nodes.
    ///
    /// # Returns
    ///
    /// Diagnostics for multiline annotation border and structure requirements.
    fn check(&self, context: &crate::RuleContext<'a>) -> Vec<Diagnostic> {
        let Some(root) = context.document().cst().nodes.first() else {
            return Vec::new();
        };
        check_block_annotation_structure(root)
    }
}
