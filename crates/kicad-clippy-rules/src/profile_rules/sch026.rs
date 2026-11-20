//! Checks selected schematic anchors against the project-declared grid.

use super::{GridAlignmentIntent, GridAlignmentTarget, LintProfile, ProfileRuleContext};
use crate::Rule;
use crate::generic::GEOMETRY_TOLERANCE_MM;
use kicad_clippy_core::{BuiltinRuleId, Diagnostic, SchematicLocation, Severity};

/// Concrete implementation of SCH026.
pub(super) struct Sch026;

/// Validates the profile grid settings before any schematic coordinate is evaluated.
///
/// # Arguments
///
/// * `intent` — Opt-in spacing, origin, tolerance, target selection and exemption settings.
///
/// # Returns
///
/// SCH026 errors for invalid spacing, tolerance, origin, empty target selection, or malformed exemptions.
fn validate_grid_configuration(intent: &GridAlignmentIntent) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    if !intent.spacing_mm.is_finite() || intent.spacing_mm <= 0.0 {
        diagnostics.push(configuration_error(
            "grid_alignment.spacing_mm must be a finite positive distance",
        ));
    }
    if !intent.tolerance_mm.is_finite()
        || intent.tolerance_mm < 0.0
        || (intent.spacing_mm.is_finite()
            && intent.spacing_mm > 0.0
            && intent.tolerance_mm >= intent.spacing_mm * crate::HALF_FACTOR)
    {
        diagnostics.push(configuration_error(
            "grid_alignment.tolerance_mm must be finite, non-negative, and less than half the grid spacing",
        ));
    }
    if intent
        .origin_mm
        .iter()
        .any(|coordinate| !coordinate.is_finite())
    {
        diagnostics.push(configuration_error(
            "grid_alignment.origin_mm must contain two finite coordinates",
        ));
    }
    if intent.targets.is_empty() {
        diagnostics.push(configuration_error(
            "grid_alignment.targets must select at least one object family",
        ));
    }
    for pin in &intent.exempt_pins {
        if !super::valid_pin_ref(pin) {
            diagnostics.push(configuration_error(format!(
                "grid_alignment.exempt_pins contains invalid pin reference {pin:?}"
            )));
        }
    }
    for label in &intent.exempt_labels {
        let Some((kind, name)) = label.split_once(':') else {
            diagnostics.push(configuration_error(format!(
                "grid_alignment.exempt_labels entry {label:?} must use kind:name syntax"
            )));
            continue;
        };
        if !matches!(kind, "local" | "global" | "hierarchical")
            || kind.trim().is_empty()
            || name.trim().is_empty()
            || name.trim() != name
        {
            diagnostics.push(configuration_error(format!(
                "grid_alignment.exempt_labels entry {label:?} must use local, global, or hierarchical kind and a trimmed non-empty label name"
            )));
        }
    }
    if intent
        .exempt_coordinates_mm
        .iter()
        .any(|point| point.iter().any(|coordinate| !coordinate.is_finite()))
    {
        diagnostics.push(configuration_error(
            "grid_alignment.exempt_coordinates_mm must contain only finite [x, y] coordinates",
        ));
    }
    diagnostics
}

/// Creates one profile-configuration error owned by SCH026.
///
/// # Arguments
///
/// * `message` — Explanation of the invalid grid-alignment setting.
///
/// # Returns
///
/// An SCH026 error diagnostic without a schematic coordinate.
fn configuration_error(message: impl Into<String>) -> Diagnostic {
    Diagnostic::for_builtin_rule(BuiltinRuleId::Sch026, Severity::Error, message)
}

/// Checks whether an anchor matches an explicitly exempted sheet coordinate.
///
/// # Arguments
///
/// * `intent` — Grid contract containing exact coordinate exemptions.
/// * `point` — Candidate anchor coordinate in sheet millimetres.
///
/// # Returns
///
/// `true` when one exempted coordinate matches within the shared geometry equality tolerance.
fn coordinate_is_exempt(intent: &GridAlignmentIntent, point: (f64, f64)) -> bool {
    intent.exempt_coordinates_mm.iter().any(|exempt| {
        (point.0 - exempt[0]).abs() <= GEOMETRY_TOLERANCE_MM
            && (point.1 - exempt[1]).abs() <= GEOMETRY_TOLERANCE_MM
    })
}

/// Checks one selected anchor and appends an SCH026 finding when it misses the configured grid.
///
/// # Arguments
///
/// * `intent` — Validated grid settings and explicit exemptions.
/// * `description` — Description of the symbol, pin, label, junction or wire endpoint being checked.
/// * `point` — Candidate object's actual X/Y anchor on the current schematic sheet, expressed in millimetres.
/// * `diagnostics` — Findings accumulated for this grid contract.
///
/// # Returns
///
/// No value; an off-grid anchor appends an error at its actual schematic coordinate.
fn check_anchor(
    intent: &GridAlignmentIntent,
    description: &str,
    point: (f64, f64),
    diagnostics: &mut Vec<Diagnostic>,
) {
    if coordinate_is_exempt(intent, point) {
        return;
    }
    let nearest_x = intent.origin_mm[0]
        + ((point.0 - intent.origin_mm[0]) / intent.spacing_mm).round() * intent.spacing_mm;
    let nearest_y = intent.origin_mm[1]
        + ((point.1 - intent.origin_mm[1]) / intent.spacing_mm).round() * intent.spacing_mm;
    let deviation_x = (point.0 - nearest_x).abs();
    let deviation_y = (point.1 - nearest_y).abs();
    if deviation_x <= intent.tolerance_mm && deviation_y <= intent.tolerance_mm {
        return;
    }
    let mut diagnostic = Diagnostic::for_builtin_rule(
        BuiltinRuleId::Sch026,
        Severity::Error,
        format!(
            "{description} at ({:.3}, {:.3}) mm is off the configured {:.3} mm grid; nearest grid point is ({nearest_x:.3}, {nearest_y:.3}) mm (X deviation {deviation_x:.3} mm, Y deviation {deviation_y:.3} mm; tolerance {:.3} mm)",
            point.0, point.1, intent.spacing_mm, intent.tolerance_mm
        ),
    );
    diagnostic.schematic_location = Some(SchematicLocation {
        x_mm: point.0,
        y_mm: point.1,
    });
    diagnostic.explanation = Some(
        "Consistent anchor placement makes schematic wiring and annotations easier to align and maintain. Only the object families explicitly selected by the project profile are checked."
            .to_owned(),
    );
    diagnostic.help = Some(format!(
        "Move this anchor to ({nearest_x:.3}, {nearest_y:.3}) mm or add a narrowly scoped exemption to grid_alignment."
    ));
    diagnostics.push(diagnostic);
}

/// Checks component symbol origins when that target family is selected.
///
/// # Arguments
///
/// * `intent` — Validated grid settings and component-reference exemptions.
/// * `facts` — Normalized placed-component origins for the current schematic.
/// * `diagnostics` — Findings accumulated for selected symbol origins.
fn validate_symbol_origins(
    intent: &GridAlignmentIntent,
    facts: &crate::SchematicFacts,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for component in &facts.components {
        if intent.exempt_components.contains(&component.reference) {
            continue;
        }
        if let Some(position) = component.position_mm {
            check_anchor(
                intent,
                &format!("component {} symbol origin", component.reference),
                position,
                diagnostics,
            );
        } else {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch026,
                Severity::Warning,
                format!(
                    "component {} symbol origin is unverified because its placement coordinate could not be resolved",
                    component.reference
                ),
            ));
        }
    }
}

/// Checks transformed pin anchors when pin-anchor validation is selected.
///
/// # Arguments
///
/// * `intent` — Validated grid settings and exact REF.PIN exemptions.
/// * `facts` — Resolved transformed pin anchors for placed symbols.
/// * `diagnostics` — Findings accumulated for selected pin anchors.
fn validate_pin_anchors(
    intent: &GridAlignmentIntent,
    facts: &crate::SchematicFacts,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for pin in &facts.pins {
        let reference = format!("{}.{}", pin.reference, pin.number);
        if intent.exempt_pins.contains(&reference) {
            continue;
        }
        check_anchor(
            intent,
            &format!("pin {reference} anchor"),
            pin.position_mm,
            diagnostics,
        );
    }
}

/// Checks local, global and hierarchical label anchors when selected.
///
/// # Arguments
///
/// * `intent` — Validated grid settings and kind:name label exemptions.
/// * `facts` — Resolved local, global and hierarchical label anchors.
/// * `diagnostics` — Findings accumulated for selected label anchors.
fn validate_label_anchors(
    intent: &GridAlignmentIntent,
    facts: &crate::SchematicFacts,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for label in &facts.labels {
        let scope = match label.kind.as_str() {
            "label" => "local",
            "global_label" => "global",
            "hierarchical_label" => "hierarchical",
            other => other,
        };
        let selector = format!("{scope}:{}", label.name);
        if intent.exempt_labels.contains(&selector) {
            continue;
        }
        check_anchor(
            intent,
            &format!("{} label {:?} anchor", label.kind, label.name),
            label.position_mm,
            diagnostics,
        );
    }
}

/// Checks explicit junction anchors when junction validation is selected.
///
/// # Arguments
///
/// * `intent` — Validated grid settings and exact coordinate exemptions.
/// * `facts` — Explicit junction locations collected from the schematic.
/// * `diagnostics` — Findings accumulated for junction anchors.
fn validate_junctions(
    intent: &GridAlignmentIntent,
    facts: &crate::SchematicFacts,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for (index, junction) in facts.junctions.iter().enumerate() {
        check_anchor(
            intent,
            &format!("junction {} anchor", index + 1),
            *junction,
            diagnostics,
        );
    }
}

/// Checks both endpoints of physical wire segments when wire-endpoint validation is selected.
///
/// # Arguments
///
/// * `intent` — Validated grid settings and exact coordinate exemptions.
/// * `facts` — Physical wire segments with their start/end sheet coordinates.
/// * `diagnostics` — Findings accumulated for wire endpoints.
fn validate_wire_endpoints(
    intent: &GridAlignmentIntent,
    facts: &crate::SchematicFacts,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for wire in &facts.wires {
        check_anchor(
            intent,
            &format!("wire endpoint at start of segment byte {}", wire.span_start),
            wire.start_mm,
            diagnostics,
        );
        check_anchor(
            intent,
            &format!("wire endpoint at end of segment byte {}", wire.span_start),
            wire.end_mm,
            diagnostics,
        );
    }
}

/// Checks selected component, pin, label, junction and wire anchors.
///
/// # Arguments
///
/// * `profile` — Project profile containing the optional grid-alignment contract.
/// * `facts` — Normalized placed-symbol, pin, label, junction and wire facts.
///
/// # Returns
///
/// SCH026 errors for selected anchors outside the configured grid and warnings for unresolved selected symbol origins.
pub(super) fn validate_grid_alignment(
    profile: &LintProfile,
    facts: &crate::SchematicFacts,
) -> Vec<Diagnostic> {
    let Some(intent) = profile.grid_alignment.as_ref() else {
        return Vec::new();
    };
    let mut diagnostics = validate_grid_configuration(intent);
    if !diagnostics.is_empty() {
        return diagnostics;
    }
    if intent.targets.contains(&GridAlignmentTarget::SymbolOrigins) {
        validate_symbol_origins(intent, facts, &mut diagnostics);
    }
    if intent.targets.contains(&GridAlignmentTarget::PinAnchors) {
        validate_pin_anchors(intent, facts, &mut diagnostics);
    }
    if intent.targets.contains(&GridAlignmentTarget::LabelAnchors) {
        validate_label_anchors(intent, facts, &mut diagnostics);
    }
    if intent.targets.contains(&GridAlignmentTarget::Junctions) {
        validate_junctions(intent, facts, &mut diagnostics);
    }
    if intent.targets.contains(&GridAlignmentTarget::WireEndpoints) {
        validate_wire_endpoints(intent, facts, &mut diagnostics);
    }
    diagnostics
}

impl<'a> Rule<ProfileRuleContext<'a>> for Sch026 {
    /// Returns the stable SCH026 identifier.
    ///
    /// # Returns
    ///
    /// The built-in identifier assigned to project grid alignment.
    fn id(&self) -> BuiltinRuleId {
        BuiltinRuleId::Sch026
    }

    /// Evaluates selected schematic anchors against the project's configured grid.
    ///
    /// # Arguments
    ///
    /// * `context` — Project profile and normalized schematic geometry for the current sheet.
    ///
    /// # Returns
    ///
    /// SCH026 errors for off-grid anchors and warnings for unresolved selected component origins.
    fn check(&self, context: &ProfileRuleContext<'a>) -> Vec<Diagnostic> {
        validate_grid_alignment(context.profile, context.facts)
    }
}

#[cfg(test)]
/// Tests SCH026 grid configuration, target selection, exemptions and diagnostics.
mod tests {
    #![allow(clippy::expect_used)]

    use super::validate_grid_alignment;
    use crate::generic::{
        ComponentFact, SchematicFacts, SchematicLabelFact, SchematicPinFact, SchematicWireFact,
    };
    use crate::{GridAlignmentIntent, GridAlignmentTarget, LintProfile};
    use kicad_clippy_core::Severity;
    use kicad_clippy_parser::SchematicDocument;
    use std::collections::BTreeSet;

    /// Parses a version-one profile with the supplied grid alignment object.
    ///
    /// # Arguments
    ///
    /// * `grid` — JSON contents for the profile's grid_alignment property.
    ///
    /// # Returns
    ///
    /// A profile with the requested optional grid contract.
    fn profile(grid: &str) -> LintProfile {
        let source = format!(r#"{{"schema_version":1,"grid_alignment":{grid}}}"#);
        serde_json::from_str(&source).expect("grid profile parses")
    }

    /// Builds normalized geometry facts with one component, pin, label, junction and wire.
    ///
    /// # Returns
    ///
    /// Geometry facts suitable for exercising each SCH026 target family.
    fn facts() -> SchematicFacts {
        SchematicFacts {
            components: vec![ComponentFact {
                reference: "U1".to_owned(),
                uuid: None,
                position_mm: Some((11.1, 22.0)),
            }],
            pins: vec![SchematicPinFact {
                reference: "U2".to_owned(),
                number: "3".to_owned(),
                position_mm: (11.2, 22.0),
                electrical_type: None,
            }],
            labels: vec![SchematicLabelFact {
                kind: "label".to_owned(),
                name: "VDD".to_owned(),
                position_mm: (11.3, 22.0),
                bounds_mm: None,
            }],
            junctions: vec![(11.4, 22.0)],
            wires: vec![SchematicWireFact {
                start_mm: (11.5, 22.0),
                end_mm: (15.0, 22.0),
                span_start: 42,
            }],
            ..SchematicFacts::default()
        }
    }

    /// Uses the configured nonzero grid origin and accepts anchors within both axis tolerances.
    #[test]
    fn accepts_anchors_on_an_offset_grid_within_tolerance() {
        let profile = profile(r#"{"spacing_mm":5.0,"origin_mm":[1.0,2.0],"tolerance_mm":0.1}"#);
        let mut facts = facts();
        facts.components[0].position_mm = Some((11.05, 21.95));
        facts.labels[0].position_mm = (10.95, 22.05);
        let diagnostics = validate_grid_alignment(&profile, &facts);
        assert!(diagnostics.is_empty());
    }

    /// Reports selected default symbol-origin and label anchors outside the configured grid.
    #[test]
    fn reports_off_grid_symbol_origins_and_labels_with_locations() {
        let profile = profile(r#"{"spacing_mm":5.0,"tolerance_mm":0.01}"#);
        let diagnostics = validate_grid_alignment(&profile, &facts());
        assert_eq!(diagnostics.len(), 2);
        assert!(diagnostics.iter().all(|item| item.code == "SCH026"));
        assert!(
            diagnostics
                .iter()
                .all(|item| item.severity == Severity::Error)
        );
        assert!(
            diagnostics
                .iter()
                .any(|item| item.message.contains("component U1"))
        );
        assert!(
            diagnostics
                .iter()
                .any(|item| item.message.contains(r#"local label "VDD""#))
        );
        assert!(
            diagnostics
                .iter()
                .all(|item| item.schematic_location.is_some())
        );
    }

    /// Checks pin anchors, junctions and wire endpoints only when each target family is selected.
    #[test]
    fn checks_pin_junction_and_wire_endpoints_when_opted_in() {
        let profile = profile(
            r#"{
                "spacing_mm":5.0,
                "tolerance_mm":0.01,
                "targets":["pin_anchors","junctions","wire_endpoints"]
            }"#,
        );
        let diagnostics = validate_grid_alignment(&profile, &facts());
        assert_eq!(diagnostics.len(), 4);
        assert!(
            diagnostics
                .iter()
                .any(|item| item.message.contains("pin U2.3"))
        );
        assert!(
            diagnostics
                .iter()
                .any(|item| item.message.contains("junction 1"))
        );
        assert!(
            diagnostics
                .iter()
                .any(|item| item.message.contains("wire endpoint at start"))
        );
        assert!(
            !diagnostics
                .iter()
                .any(|item| item.message.contains("wire endpoint at end"))
        );
    }

    /// Accepts configured component, pin, label and exact-coordinate exemptions.
    #[test]
    fn applies_narrow_object_and_coordinate_exemptions() {
        let profile = profile(
            r#"{
                "spacing_mm":5.0,
                "tolerance_mm":0.01,
                "targets":["symbol_origins","pin_anchors","label_anchors","junctions"],
                "exempt_components":["U1"],
                "exempt_pins":["U2.3"],
                "exempt_labels":["local:VDD"],
                "exempt_coordinates_mm":[[11.4,22.0]]
            }"#,
        );
        assert_eq!(validate_grid_alignment(&profile, &facts()), Vec::new());
    }

    /// Defaults to checking only symbol origins and label anchors when targets are omitted.
    #[test]
    fn default_targets_do_not_duplicate_pin_or_wire_erc_checks() {
        let profile = profile(r#"{"spacing_mm":5.0}"#);
        let diagnostics = validate_grid_alignment(&profile, &facts());
        assert_eq!(diagnostics.len(), 2);
        assert!(diagnostics.iter().all(|item| {
            item.message.contains("component U1") || item.message.contains("label")
        }));
    }

    /// Reports unresolved selected component origins as unverified rather than assuming a coordinate.
    #[test]
    fn warns_when_symbol_origin_is_unresolved() {
        let profile = profile(r#"{"spacing_mm":5.0}"#);
        let mut facts = facts();
        facts.components[0].position_mm = None;
        let diagnostics = validate_grid_alignment(&profile, &facts);
        assert!(diagnostics.iter().any(|item| {
            item.code == "SCH026"
                && item.severity == Severity::Warning
                && item.message.contains("origin is unverified")
        }));
    }

    /// Maps native KiCad global-label facts to the profile's concise global exemption selector.
    #[test]
    fn global_label_exemption_matches_native_kicad_label_kind() {
        let mut facts = facts();
        facts.labels[0].kind = "global_label".to_owned();
        facts.labels[0].name = "RESET".to_owned();
        let profile = profile(
            r#"{
                "spacing_mm":5.0,
                "tolerance_mm":0.01,
                "targets":["label_anchors"],
                "exempt_labels":["global:RESET"]
            }"#,
        );
        assert!(validate_grid_alignment(&profile, &facts).is_empty());
    }

    /// Rejects unsupported scope names rather than accepting an exemption that can never match.
    #[test]
    fn rejects_unknown_label_exemption_scope() {
        let profile = profile(
            r#"{
                "spacing_mm":5.0,
                "targets":["label_anchors"],
                "exempt_labels":["global_label:RESET"]
            }"#,
        );
        let diagnostics = validate_grid_alignment(&profile, &facts());
        assert!(diagnostics.iter().any(|item| {
            item.code == "SCH026" && item.message.contains("local, global, or hierarchical")
        }));
    }

    /// Rejects invalid spacing, tolerance, empty targets, pin exemptions and coordinate exemptions.
    #[test]
    fn rejects_invalid_grid_configuration() {
        let profile = profile(
            r#"{
                "spacing_mm":5.0,
                "tolerance_mm":3.0,
                "targets":[],
                "exempt_pins":["U1."],
                "exempt_labels":["missing-colon"],
                "exempt_coordinates_mm":[[1.0,1.0]]
            }"#,
        );
        let diagnostics = validate_grid_alignment(&profile, &facts());
        assert!(diagnostics.len() >= 4);
        assert!(diagnostics.iter().all(|item| item.code == "SCH026"));
        assert!(
            diagnostics
                .iter()
                .all(|item| item.severity == Severity::Error)
        );
    }

    /// Round-trips the default grid target set in the typed profile representation.
    #[test]
    fn typed_grid_intent_retains_default_targets() {
        let profile = profile(r#"{"spacing_mm":2.54}"#);
        let intent: &GridAlignmentIntent = profile.grid_alignment.as_ref().expect("grid exists");
        assert_eq!(
            intent.targets,
            BTreeSet::from([
                GridAlignmentTarget::SymbolOrigins,
                GridAlignmentTarget::LabelAnchors
            ])
        );
    }
    /// Confirms that the profile validation entry point executes SCH026 against extracted label anchors.
    ///
    /// # Returns
    ///
    /// Ok when an off-grid label is reported through the public profile validation path.
    ///
    /// # Errors
    ///
    /// Returns an error if the schematic fixture cannot be parsed.
    #[test]
    fn profile_validation_dispatches_grid_alignment_rule() -> Result<(), Box<dyn std::error::Error>>
    {
        let schematic = SchematicDocument::from_text(
            "grid-alignment-dispatch.kicad_sch",
            r#"(kicad_sch
                (label "VDD" (at 11.3 22 0)
                    (effects (font (size 1 1))))
            )"#,
        )?;
        let profile = profile(r#"{"spacing_mm":5.0,"tolerance_mm":0.01}"#);
        let diagnostics = profile.validate(&schematic);
        assert!(
            diagnostics
                .iter()
                .any(|item| { item.code == "SCH026" && item.message.contains("local label") })
        );
        Ok(())
    }

    /// Rejects a zero grid spacing before attempting any coordinate calculation.
    #[test]
    fn rejects_non_positive_grid_spacing() {
        let profile = profile(r#"{"spacing_mm":0.0}"#);
        let diagnostics = validate_grid_alignment(&profile, &facts());
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("finite positive"));
    }
}
