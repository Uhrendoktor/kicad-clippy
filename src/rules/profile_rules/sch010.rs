//! Profile-driven maximum gaps between visible Reference/Value fields and symbol bodies.

use super::ComponentIntent;
use super::ProfileRuleContext;
use super::geometry::{body_geometry_for_reference, minimum_body_to_bounds_gap};
use crate::core::BuiltinRuleId;
use crate::core::Diagnostic;
use crate::core::Severity;
use crate::rules::Rule;
use crate::rules::generic::GEOMETRY_TOLERANCE_MM;
use crate::rules::generic::SchematicFacts;
use std::collections::BTreeMap;

/// Checks one component's visible fields against its profile-declared maximum body gaps.
///
/// # Arguments
///
/// * `reference` — Reference designator used to match one placed symbol's transformed outline and visible fields.
/// * `maximum_gaps_mm` — Map from each visible field name to its maximum estimated glyph-rectangle-to-body distance in millimetres.
/// * `facts` — Normalized transformed body primitives and visible field bounds.
///
/// # Returns
///
/// SCH010 diagnostics for invalid thresholds, unresolved field/body geometry, or fields
/// whose estimated visible bounds lie farther from the body than the declared maximum.
fn validate_component_field_body_gaps(
    reference: &str,
    maximum_gaps_mm: &BTreeMap<String, f64>,
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    let body = body_geometry_for_reference(reference, facts);
    let mut diagnostics = Vec::new();
    for (field_name, maximum_gap_mm) in maximum_gaps_mm {
        if field_name != "Reference" && field_name != "Value" {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch010,
                Severity::Error,
                format!(
                    "{reference} max_field_body_gap_mm contains unsupported field {field_name:?}; supported fields are Reference and Value"
                ),
            ));
            continue;
        }
        if !maximum_gap_mm.is_finite() || *maximum_gap_mm < 0.0 {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch010,
                Severity::Error,
                format!(
                    "{reference} {field_name} max_field_body_gap_mm must be a finite non-negative distance"
                ),
            ));
            continue;
        }
        let Some(field) = facts
            .visible_fields
            .iter()
            .find(|field| field.reference == reference && field.name == *field_name)
        else {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch010,
                Severity::Warning,
                format!(
                    "{reference} {field_name} field is not visible or its glyph bounds could not be resolved; maximum body gap is unverified"
                ),
            ));
            continue;
        };
        let Some(actual_gap_mm) = minimum_body_to_bounds_gap(&body, field.bounds_mm) else {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch010,
                Severity::Warning,
                format!(
                    "{reference} {field_name} field body gap is unverified because its selected-unit body outline is missing or contains unsupported curve geometry"
                ),
            ));
            continue;
        };
        if actual_gap_mm > *maximum_gap_mm + GEOMETRY_TOLERANCE_MM {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch010,
                Severity::Error,
                format!(
                    "{reference} {field_name} field is {actual_gap_mm:.2} mm from its symbol body; maximum is {maximum_gap_mm:.2} mm"
                ),
            ));
        }
    }
    diagnostics
}

/// Concrete implementation of profile-declared SCH010 maximum field-to-body gaps.
pub(super) struct Sch010;

impl<'a> Rule<ProfileRuleContext<'a>> for Sch010 {
    /// Returns the stable SCH010 identifier.
    ///
    /// # Returns
    ///
    /// The stable SCH010 identifier used to associate diagnostics with this rule.
    fn id(&self) -> BuiltinRuleId {
        BuiltinRuleId::Sch010
    }

    /// Evaluates per-component Reference/Value maximum body gaps.
    ///
    /// # Arguments
    ///
    /// * `context` — Component profile declarations and normalized transformed schematic facts.
    ///
    /// # Returns
    ///
    /// SCH010 diagnostics for profile-declared field/body gap violations or unverified geometry.
    fn check(&self, context: &ProfileRuleContext<'a>) -> Vec<Diagnostic> {
        context
            .profile
            .components
            .iter()
            .flat_map(|(reference, intent): (&String, &ComponentIntent)| {
                validate_component_field_body_gaps(
                    reference,
                    &intent.max_field_body_gap_mm,
                    context.facts,
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::validate_component_field_body_gaps;
    use crate::core::{BuiltinRuleId, Severity};
    use crate::rules::generic::{SchematicFacts, SchematicFieldFact, SchematicPrimitiveFact};
    use std::collections::BTreeMap;

    /// Creates a simple rectangular body and one visible field for SCH010 gap tests.
    ///
    /// # Arguments
    ///
    /// * `field_bounds` — Left, top, right, and bottom coordinates of the estimated field glyph rectangle.
    ///
    /// # Returns
    ///
    /// Normalized facts containing the U1 rectangular body and its visible Reference field.
    fn body_and_field_facts(field_bounds: (f64, f64, f64, f64)) -> SchematicFacts {
        let mut facts = SchematicFacts::default();
        facts.symbol_primitives.push(SchematicPrimitiveFact {
            reference: "U1".to_owned(),
            points_mm: vec![(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0)],
            closed: true,
            circle_mm: None,
            arc_mm: None,
            bezier_mm: None,
            stroke_width_mm: 0.0,
        });
        facts.visible_fields.push(SchematicFieldFact {
            reference: "U1".to_owned(),
            name: "Reference".to_owned(),
            bounds_mm: field_bounds,
        });
        facts
    }

    /// Confirms a field inside or near the body satisfies its configured maximum gap.
    #[test]
    fn field_body_gap_accepts_glyph_inside_body() {
        let facts = body_and_field_facts((0.5, 0.5, 1.5, 1.5));
        let thresholds = BTreeMap::from([("Reference".to_owned(), 1.0)]);
        assert_eq!(
            validate_component_field_body_gaps("U1", &thresholds, &facts).as_slice(),
            &[]
        );
    }

    /// Confirms an excessive field-to-body gap is reported using visible glyph bounds.
    #[test]
    fn field_body_gap_rejects_glyph_beyond_maximum() {
        let facts = body_and_field_facts((5.0, 0.5, 6.0, 1.5));
        let thresholds = BTreeMap::from([("Reference".to_owned(), 2.0)]);
        let diagnostics = validate_component_field_body_gaps("U1", &thresholds, &facts);
        assert!(diagnostics.iter().any(|item| {
            item.code == BuiltinRuleId::Sch010.as_str()
                && item.message.contains("3.00 mm")
                && item.message.contains("maximum is 2.00 mm")
        }));
    }

    /// Confirms unsupported curved body geometry is reported as unverified, not guessed.
    #[test]
    fn field_body_gap_warns_for_unresolved_curved_outline() {
        let mut facts = body_and_field_facts((5.0, 0.5, 6.0, 1.5));
        facts.symbol_primitives.push(SchematicPrimitiveFact {
            reference: "U1".to_owned(),
            points_mm: vec![(0.0, 0.0), (1.0, 1.0), (2.0, 0.0)],
            closed: false,
            circle_mm: None,
            arc_mm: None,
            bezier_mm: Some([(0.0, 0.0), (0.5, 0.0), (1.5, 1.0), (2.0, 1.0)]),
            stroke_width_mm: 0.0,
        });
        let thresholds = BTreeMap::from([("Reference".to_owned(), 2.0)]);
        let diagnostics = validate_component_field_body_gaps("U1", &thresholds, &facts);
        assert!(diagnostics.iter().any(|item| {
            item.severity == Severity::Warning && item.message.contains("unverified")
        }));
    }
}
