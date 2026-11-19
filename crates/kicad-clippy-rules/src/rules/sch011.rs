//! Checks `SCH011` multiline block annotations for a visible border.

use geo::Contains;
use kicad_clippy_core::BuiltinRuleId;
use kicad_clippy_core::Diagnostic;
use kiutils_sexpr::Node;
use super::at_span;
use super::atom_value;
use super::check_block_annotation_structure;
use super::child_list;
use super::collect_nodes;
use super::fn;
use super::list_items;
use super::number;

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
