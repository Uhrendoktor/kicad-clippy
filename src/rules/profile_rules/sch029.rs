//! Profile-declared base-design component population and DNP consistency checks.

use super::{ComponentIntent, ProfileRuleContext, SchematicFacts};
use crate::core::{BuiltinRuleId, Diagnostic, Severity};
use crate::rules::Rule;

/// Placeholder values that must be represented by the DNP flag rather than the Value property.
const PLACEHOLDER_VALUES: [&str; 6] = [
    "NC",
    "DNP",
    "NP",
    "NOT POPULATED",
    "DO NOT POPULATE",
    "DO NOT PLACE",
];

/// Checks one component's saved base-design population state and Value field.
///
/// # Arguments
///
/// * `reference` — Placed component reference designator being validated.
/// * `intent` — Component metadata containing the optional `population` declaration.
/// * `facts` — Normalized placed-symbol population facts extracted from the schematic.
///
/// # Returns
///
/// SCH029 diagnostics for invalid declarations, DNP mismatches, placeholder values, or unverified variant overrides.
fn validate_component_population(
    reference: &str,
    intent: &ComponentIntent,
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    let Some(population) = intent.attributes.get("population") else {
        return Vec::new();
    };
    let expected_dnp = match population.trim().to_ascii_lowercase().as_str() {
        "populated" => false,
        "dnp" => true,
        _ => {
            return vec![Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch029,
                Severity::Error,
                format!(
                    "{reference} has invalid population {population:?}; expected `populated` or `dnp`"
                ),
            )];
        }
    };
    let Some(component) = facts
        .component_populations
        .iter()
        .find(|component| component.reference == reference)
    else {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch029,
            Severity::Warning,
            format!(
                "{reference} population state is unverified because its placed symbol facts could not be resolved"
            ),
        )];
    };

    let mut diagnostics = Vec::new();
    match component.dnp {
        Some(actual_dnp) if actual_dnp != expected_dnp => {
            let expected = if expected_dnp { "DNP" } else { "populated" };
            let actual = if actual_dnp { "DNP" } else { "populated" };
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch029,
                Severity::Error,
                format!(
                    "{reference} is declared {expected} but its saved base-design DNP flag marks it {actual}"
                ),
            ));
        }
        None => diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch029,
            Severity::Warning,
            format!("{reference} population state is unverified because its serialized DNP flag is malformed"),
        )),
        Some(_) => {}
    }

    match component.value.as_deref().map(str::trim) {
        None | Some("" | "~") => diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch029,
            Severity::Error,
            format!("{reference} must retain a non-empty component Value instead of a population placeholder"),
        )),
        Some(value) if PLACEHOLDER_VALUES.contains(&value.to_ascii_uppercase().as_str()) => {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch029,
                Severity::Error,
                format!(
                    "{reference} Value {value:?} is a population placeholder; retain the real component value and use the DNP flag"
                ),
            ));
        }
        Some(_) => {}
    }

    if component.has_variant_overrides {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch029,
            Severity::Warning,
            format!(
                "{reference} has design-variant overrides; SCH029 validates only its saved base-design population state"
            ),
        ));
    }
    diagnostics
}

/// Concrete implementation of built-in rule SCH029.
pub(super) struct Sch029;

impl<'a> Rule<ProfileRuleContext<'a>> for Sch029 {
    /// Returns the stable SCH029 identifier.
    ///
    /// # Returns
    ///
    /// The SCH029 identifier used to associate population diagnostics with this rule.
    fn id(&self) -> BuiltinRuleId {
        BuiltinRuleId::Sch029
    }

    /// Evaluates explicit base-design population declarations for metadata-bearing components.
    ///
    /// # Arguments
    ///
    /// * `context` — Project component intent and normalized saved schematic population facts.
    ///
    /// # Returns
    ///
    /// Diagnostics for mismatched DNP flags, placeholder Value text, invalid profile declarations, or unverified variant overrides.
    fn check(&self, context: &ProfileRuleContext<'a>) -> Vec<Diagnostic> {
        context
            .profile
            .components
            .iter()
            .flat_map(|(reference, intent)| {
                validate_component_population(reference, intent, context.facts)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::validate_component_population;
    use crate::core::{BuiltinRuleId, Severity};
    use crate::parser::SchematicDocument;
    use crate::rules::generic::SchematicFacts;
    use crate::rules::metadata::{ComponentIntent, ComponentKind, ComponentRole};
    use std::collections::BTreeMap;

    /// Creates component metadata with one explicit base-design population expectation.
    ///
    /// # Arguments
    ///
    /// * `population` — Expected population state, normally `populated` or `dnp`.
    ///
    /// # Returns
    ///
    /// A component intent carrying the requested `attributes.population` value.
    fn intent(population: &str) -> ComponentIntent {
        ComponentIntent {
            kind: ComponentKind::Resistor,
            role: ComponentRole("pull_up".to_owned()),
            function: Some("Optional bias resistor".to_owned()),
            rationale: Some("Provides a bias path when required".to_owned()),
            evidence: None,
            decoupling: None,
            connections: BTreeMap::new(),
            pin_nets: BTreeMap::new(),
            attributes: BTreeMap::from([("population".to_owned(), population.to_owned())]),
            max_field_body_gap_mm: BTreeMap::new(),
            voltage_rating_min_v: None,
            power_rating_min_w: None,
        }
    }

    /// Parses a placed symbol whose Value, DNP flag, and optional variant child can be varied.
    ///
    /// # Arguments
    ///
    /// * `value` — Value property text serialized on the placed symbol.
    /// * `dnp` — Base-design DNP flag value serialized by `KiCad`, either `yes` or `no`.
    /// * `variant_override` — Whether a design-variant override node is included.
    ///
    /// # Returns
    ///
    /// Normalized component-population facts extracted from one parsed placed symbol.
    fn population_facts(value: &str, dnp: &str, variant_override: bool) -> SchematicFacts {
        let variant = if variant_override {
            r#"(instances (project "design" (path "/" (variant "optional" (dnp yes)))))"#
        } else {
            ""
        };
        let source = format!(
            r#"(kicad_sch
                (symbol (lib_id "Device:R") (at 10 10 0) (unit 1)
                    (property "Reference" "R1" (at 10 10 0) (effects (font (size 1 1))))
                    (property "Value" "{value}" (at 10 10 0) (effects (font (size 1 1))))
                    (dnp {dnp})
                    {variant}
                )
            )"#
        );
        let document = SchematicDocument::from_text("population-test.kicad_sch", source)
            .expect("population schematic fixture parses");
        SchematicFacts::from_document(&document)
    }

    /// Accepts a populated component whose base-design DNP flag is disabled and Value is real.
    #[test]
    fn sch029_accepts_populated_component_with_real_value() {
        let diagnostics = validate_component_population(
            "R1",
            &intent("populated"),
            &population_facts("10k", "no", false),
        );
        assert_eq!(diagnostics, Vec::new());
    }

    /// Accepts a DNP component when the DNP flag is enabled and its real value is retained.
    #[test]
    fn sch029_accepts_dnp_component_with_real_value() {
        let diagnostics = validate_component_population(
            "R1",
            &intent("dnp"),
            &population_facts("10k", "yes", false),
        );
        assert_eq!(diagnostics, Vec::new());
    }

    /// Rejects a populated declaration when the saved base design marks the symbol DNP.
    #[test]
    fn sch029_rejects_population_flag_mismatch() {
        let diagnostics = validate_component_population(
            "R1",
            &intent("populated"),
            &population_facts("10k", "yes", false),
        );
        assert!(diagnostics.iter().any(|item| {
            item.code == BuiltinRuleId::Sch029.as_str()
                && item.severity == Severity::Error
                && item.message.contains("declared populated")
        }));
    }

    /// Rejects placeholder Value text instead of the actual component value for a DNP part.
    #[test]
    fn sch029_rejects_dnp_placeholder_in_value() {
        let diagnostics = validate_component_population(
            "R1",
            &intent("dnp"),
            &population_facts("DNP", "yes", false),
        );
        assert!(diagnostics.iter().any(|item| {
            item.code == BuiltinRuleId::Sch029.as_str()
                && item.severity == Severity::Error
                && item.message.contains("retain the real component value")
        }));
    }

    /// Warns that an arbitrary selected design variant is not validated when an override exists.
    #[test]
    fn sch029_warns_when_variant_override_exists() {
        let diagnostics = validate_component_population(
            "R1",
            &intent("populated"),
            &population_facts("10k", "no", true),
        );
        assert!(diagnostics.iter().any(|item| {
            item.code == BuiltinRuleId::Sch029.as_str()
                && item.severity == Severity::Warning
                && item
                    .message
                    .contains("validates only its saved base-design")
        }));
    }

    /// Rejects unknown population values in the profile rather than silently guessing a state.
    #[test]
    fn sch029_rejects_unknown_population_value() {
        let diagnostics = validate_component_population(
            "R1",
            &intent("maybe"),
            &population_facts("10k", "no", false),
        );
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, BuiltinRuleId::Sch029.as_str());
        assert_eq!(diagnostics[0].severity, Severity::Error);
    }
}
