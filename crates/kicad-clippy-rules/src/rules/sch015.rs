//! Checks `SCH015` placement of explicit no-connect markers on transformed symbol pins.

use super::GEOMETRY_TOLERANCE_MM;
use super::at_point;
use super::collect_nodes;
use super::collect_placed_pin_positions;
use super::distance;
use super::point_from_at;
use super::span_of;
use kicad_clippy_core::BuiltinRuleId;
use kicad_clippy_core::Diagnostic;
use kiutils_sexpr::Node;

/// Checks that explicit no-connect markers coincide with supported transformed pins.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// SCH015 diagnostics for no-connect markers that do not coincide with a supported transformed symbol-pin anchor.
pub(super) fn check_no_connect_markers(root: &Node) -> Vec<Diagnostic> {
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

/// Concrete implementation of built-in rule 015.
pub(super) struct Sch015;
impl<'a> crate::Rule<crate::RuleContext<'a>> for Sch015 {
    /// Returns the stable 015 identifier.
    ///
    /// # Returns
    ///
    /// The stable 015 identifier.
    fn id(&self) -> BuiltinRuleId {
        BuiltinRuleId::Sch015
    }
    /// Evaluates explicit no-connect marker placement on transformed symbol pins.
    ///
    /// # Arguments
    ///
    /// * context — the parsed schematic containing symbols and no-connect markers.
    ///
    /// # Returns
    ///
    /// Diagnostics for explicit no-connect marker placement on transformed symbol pins.
    fn check(&self, context: &crate::RuleContext<'a>) -> Vec<Diagnostic> {
        let Some(root) = context.document().cst().nodes.first() else {
            return Vec::new();
        };
        check_no_connect_markers(root)
    }
}
