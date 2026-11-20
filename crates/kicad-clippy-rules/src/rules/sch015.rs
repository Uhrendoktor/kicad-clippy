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
use kicad_clippy_core::Severity;
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

/// Reports explicit no-connect flags placed on pins whose effective library type already
/// denotes a non-connectable pin. This scan covers all resolved placed-symbol pins, not
/// only pins named by a project profile.
///
/// # Arguments
///
/// * `facts` — Normalized pin and no-connect marker facts for the current schematic.
///
/// # Returns
///
/// SCH015 errors for redundant flags on pins of effective type `unconnected` or `no_connect`.
fn check_redundant_no_connect_markers(facts: &crate::generic::SchematicFacts) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for marker in &facts.no_connects {
        for pin in facts.pins.iter().filter(|pin| {
            (pin.position_mm.0 - marker.center_mm.0).hypot(pin.position_mm.1 - marker.center_mm.1)
                < GEOMETRY_TOLERANCE_MM
        }) {
            if let Some(electrical_type @ ("unconnected" | "no_connect")) =
                pin.electrical_type.as_deref()
            {
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch015,
                    Severity::Error,
                    format!(
                        "no-connect marker on {}.{} is redundant because its effective library pin type is {electrical_type:?}",
                        pin.reference, pin.number
                    ),
                ));
            }
        }
    }
    diagnostics
}

/// Concrete implementation of built-in rule 015.
pub(super) struct Sch015;
impl<'a> crate::Rule<crate::RuleContext<'a>> for Sch015 {
    /// Returns the stable 015 identifier.
    ///
    /// # Returns
    ///
    /// The stable SCH015 identifier used to associate diagnostics with this rule.
    fn id(&self) -> BuiltinRuleId {
        BuiltinRuleId::Sch015
    }
    /// Evaluates explicit no-connect marker placement on transformed symbol pins.
    ///
    /// # Arguments
    ///
    /// * `context` — the parsed schematic containing symbols and no-connect markers.
    ///
    /// # Returns
    ///
    /// Diagnostics for explicit no-connect marker placement on transformed symbol pins.
    fn check(&self, context: &crate::RuleContext<'a>) -> Vec<Diagnostic> {
        let Some(root) = context.document().cst().nodes.first() else {
            return Vec::new();
        };
        let mut diagnostics = check_no_connect_markers(root);
        diagnostics.extend(check_redundant_no_connect_markers(context.facts()));
        diagnostics
    }
}

#[cfg(test)]
/// Tests redundant no-connect marker detection against effective library pin types.
mod tests {
    #![allow(clippy::expect_used)]

    use super::check_redundant_no_connect_markers;
    use crate::generic::SchematicFacts;
    use kicad_clippy_parser::SchematicDocument;

    /// Builds a one-pin embedded-symbol fixture with an optional redundant no-connect marker.
    ///
    /// # Arguments
    ///
    /// * `pin_type` — Effective library electrical type to assign to the pin.
    /// * `with_marker` — Whether to place a no-connect flag at the transformed pin anchor.
    ///
    /// # Returns
    ///
    /// A parsed schematic document containing U1.1 and the requested marker state.
    fn schematic(pin_type: &str, with_marker: bool) -> SchematicDocument {
        let marker = if with_marker {
            r#"(no_connect (at 10 10) (uuid "redundant-marker"))"#
        } else {
            ""
        };
        let source = format!(
            r#"(kicad_sch
                (lib_symbols
                    (symbol "Test:OnePin"
                        (symbol "OnePin_1_1"
                            (pin {pin_type} line (at 0 0 0) (length 1.27)
                                (name "1") (number "1"))
                        )
                    )
                )
                (symbol (lib_id "Test:OnePin") (at 10 10 0) (unit 1)
                    (property "Reference" "U1" (at 10 9 0)
                        (effects (font (size 1 1))))
                )
                {marker}
            )"#
        );
        SchematicDocument::from_text("redundant-no-connect.kicad_sch", &source)
            .expect("fixture parses")
    }

    /// Reports a marker on a library pin whose type is already unconnected.
    #[test]
    fn redundant_flag_on_unconnected_pin_is_reported() {
        let document = schematic("unconnected", true);
        let facts = SchematicFacts::from_document(&document);
        let diagnostics = check_redundant_no_connect_markers(&facts);
        assert!(diagnostics.iter().any(|item| {
            item.code == "SCH015"
                && item.message.contains("U1.1")
                && item.message.contains("unconnected")
        }));
    }

    /// Reports a marker on a library pin whose type is already `no_connect`.
    #[test]
    fn redundant_flag_on_no_connect_pin_is_reported() {
        let document = schematic("no_connect", true);
        let facts = SchematicFacts::from_document(&document);
        let diagnostics = check_redundant_no_connect_markers(&facts);
        assert!(diagnostics.iter().any(|item| {
            item.code == "SCH015"
                && item.message.contains("U1.1")
                && item.message.contains("no_connect")
        }));
    }

    /// Keeps a no-connect marker on an ordinary passive pin valid.
    #[test]
    fn marker_on_connectable_passive_pin_is_not_redundant() {
        let document = schematic("passive", true);
        let facts = SchematicFacts::from_document(&document);
        assert_eq!(check_redundant_no_connect_markers(&facts), Vec::new());
    }
}
