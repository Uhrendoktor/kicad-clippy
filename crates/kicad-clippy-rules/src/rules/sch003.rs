//! Checks `SCH003` visible label-glyph clearance from connected wire geometry.

use super::Segment;
use super::at_span;
use super::collect_label_nodes;
use super::strict_line_hits_bounds;
use super::text_visual_bounds;
use kicad_clippy_core::BuiltinRuleId;
use kicad_clippy_core::Diagnostic;
use kiutils_sexpr::Node;

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
pub(super) fn check_label_wire_clearance(root: &Node, segments: &[Segment]) -> Vec<Diagnostic> {
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

/// Concrete implementation of built-in rule 003.
pub(super) struct Sch003;
impl<'a> crate::Rule<crate::RuleContext<'a>> for Sch003 {
    /// Returns the stable 003 identifier.
    ///
    /// # Returns
    ///
    /// The stable SCH003 identifier used to associate diagnostics with this rule.
    fn id(&self) -> BuiltinRuleId {
        BuiltinRuleId::Sch003
    }
    /// Evaluates visible label-glyph clearance from connected wire geometry.
    ///
    /// # Arguments
    ///
    /// * `context` — the parsed schematic and normalized wire segments.
    ///
    /// # Returns
    ///
    /// Diagnostics for visible label-glyph clearance from connected wire geometry.
    fn check(&self, context: &crate::RuleContext<'a>) -> Vec<Diagnostic> {
        let Some(root) = context.document().cst().nodes.first() else {
            return Vec::new();
        };
        check_label_wire_clearance(root, context.segments)
    }
}
