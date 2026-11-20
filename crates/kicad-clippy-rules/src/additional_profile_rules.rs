//! Additional profile-driven checks that complement native ERC.

use kicad_clippy_core::{BuiltinRuleId, Diagnostic, SchematicLocation, Severity};

use crate::{HALF_FACTOR, LintProfile, SchematicFacts};

/// Coordinates within this distance are treated as the same schematic anchor.
const GEOMETRY_TOLERANCE_MM: f64 = 0.001;
/// Component metadata key containing comma-separated pin numbers that require explicit no-connect flags.
const REQUIRED_NO_CONNECT_PINS_ATTRIBUTE: &str = "required_no_connect_pins";

/// Runs supplemental opt-in profile checks using schematic facts already extracted
/// by the main profile-validation pass.
///
/// # Arguments
///
/// * `profile` — Project-level contracts that opt into extra presentation and connectivity checks.
/// * `facts` — Normalized schematic objects, frames, pins, and markers collected once for this lint run.
///
/// # Returns
///
/// Diagnostics for every violated profile extension; an empty vector means all declared extensions passed.
pub fn validate_profile_extensions(
    profile: &LintProfile,
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    let mut diagnostics = validate_declared_frame_overlaps(profile, facts);
    diagnostics.extend(validate_required_no_connect_pins(profile, facts));
    diagnostics
}

/// Rejects overlapping frames when two different profile groups claim them.
///
/// Only frames explicitly referenced by a group's `frame_uuid` are checked;
/// arbitrary schematic rectangles are not assumed to be group boundaries.
/// Edge contact is allowed because adjacent frames may share an alignment
/// line without occupying the same area. Two groups reusing the exact same
/// frame UUID are treated as sharing one frame, not as overlapping frames.
///
/// # Arguments
///
/// * `profile` — Functional-group declarations identifying which rectangles act as group frames.
/// * `facts` — Resolved schematic frame rectangles used for geometric intersection tests.
///
/// # Returns
///
/// SCH009 diagnostics for overlapping declared frame interiors; edge contact and shared UUIDs are accepted.
fn validate_declared_frame_overlaps(
    profile: &LintProfile,
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    let declared_frames: Vec<_> = profile
        .groups
        .iter()
        .filter_map(|(group_id, group)| {
            let frame_uuid = group.frame_uuid.as_deref()?;
            let frame = facts.frames.iter().find(|frame| frame.uuid == frame_uuid)?;
            Some((group_id.as_str(), frame_uuid, frame.bounds_mm))
        })
        .collect();

    let mut diagnostics = Vec::new();
    for (index, first) in declared_frames.iter().enumerate() {
        for second in declared_frames.iter().skip(index + 1) {
            let (first_group, first_uuid, first_bounds) = *first;
            let (second_group, second_uuid, second_bounds) = *second;
            if first_uuid == second_uuid {
                continue;
            }

            let (first_left, first_top, first_right, first_bottom) = first_bounds;
            let (second_left, second_top, second_right, second_bottom) = second_bounds;
            let left = first_left.max(second_left);
            let top = first_top.max(second_top);
            let right = first_right.min(second_right);
            let bottom = first_bottom.min(second_bottom);
            let overlap_width = right - left;
            let overlap_height = bottom - top;

            if overlap_width <= GEOMETRY_TOLERANCE_MM || overlap_height <= GEOMETRY_TOLERANCE_MM {
                continue;
            }

            let mut diagnostic = Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch009,
                Severity::Error,
                format!(
                    "functional frames for groups {first_group:?} ({first_uuid}) and {second_group:?} ({second_uuid}) overlap by {overlap_width:.2} x {overlap_height:.2} mm"
                ),
            );
            diagnostic.explanation = Some(
                "Functional frames are visual boundaries for separate circuit groups. Overlapping frames make group ownership and the intended page hierarchy ambiguous."
                    .to_owned(),
            );
            diagnostic.help = Some(
                "Move or resize the declared frames so their interiors no longer overlap; preserve any intentional shared boundary as edge contact."
                    .to_owned(),
            );
            diagnostic.schematic_location = Some(SchematicLocation {
                x_mm: (left + right) * HALF_FACTOR,
                y_mm: (top + bottom) * HALF_FACTOR,
            });
            diagnostics.push(diagnostic);
        }
    }

    diagnostics
}

/// Enforces profile-declared no-connect markers for selected placed-symbol pins.
///
/// The `required_no_connect_pins` attribute is a comma-separated list of pin
/// numbers. Each pin must resolve through the embedded symbol definition and
/// the marker must coincide with its transformed sheet coordinate. A pin whose
/// geometry cannot be resolved is reported as unverified rather than silently
/// treated as correctly marked.
///
/// # Arguments
///
/// * `profile` — Component intent declarations containing required no-connect pin numbers.
/// * `facts` — Embedded placed-symbol pin coordinates and actual no-connect marker locations.
///
/// # Returns
///
/// SCH015 diagnostics for missing, empty, or unverifiable required no-connect marker declarations.
fn validate_required_no_connect_pins(
    profile: &LintProfile,
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    for (reference, intent) in &profile.components {
        let Some(raw_pins) = intent.attributes.get(REQUIRED_NO_CONNECT_PINS_ATTRIBUTE) else {
            continue;
        };

        let required_pins: std::collections::BTreeSet<_> = raw_pins
            .split(',')
            .map(str::trim)
            .filter(|pin| !pin.is_empty())
            .collect();

        if required_pins.is_empty() {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch015,
                Severity::Error,
                format!(
                    "{reference} has an empty {REQUIRED_NO_CONNECT_PINS_ATTRIBUTE} declaration"
                ),
            ));
            continue;
        }

        for pin_number in required_pins {
            let pin_reference = format!("{reference}.{pin_number}");
            let Some(pin) = facts
                .pins
                .iter()
                .find(|pin| pin.reference == *reference && pin.number == pin_number)
            else {
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch015,
                    Severity::Warning,
                    format!(
                        "{pin_reference} is declared as requiring a no-connect marker, but its pin geometry could not be resolved; the requirement is unverified"
                    ),
                ));
                continue;
            };

            if let Some(electrical_type @ ("no_connect" | "unconnected")) =
                pin.electrical_type.as_deref()
            {
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch015,
                    Severity::Error,
                    format!(
                        "{pin_reference} has library electrical type {electrical_type:?} and must not be listed in {REQUIRED_NO_CONNECT_PINS_ATTRIBUTE}; that pin type already denotes no connection"
                    ),
                ));
                continue;
            }

            let marker_present = facts.no_connects.iter().any(|marker| {
                (marker.center_mm.0 - pin.position_mm.0)
                    .hypot(marker.center_mm.1 - pin.position_mm.1)
                    <= GEOMETRY_TOLERANCE_MM
            });

            if !marker_present {
                let mut diagnostic = Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch015,
                    Severity::Error,
                    format!(
                        "{pin_reference} is declared as intentionally unconnected but has no explicit no-connect marker"
                    ),
                );
                diagnostic.explanation = Some(
                    "The profile declares this pin as intentionally unused. An explicit marker makes that design decision visible and prevents an accidental omission from being mistaken for an unfinished connection."
                        .to_owned(),
                );
                diagnostic.help = Some(
                    "Place a KiCad no-connect flag at the transformed pin anchor, or remove the pin from the profile declaration if it is not intentionally unused."
                        .to_owned(),
                );
                diagnostic.schematic_location = Some(SchematicLocation {
                    x_mm: pin.position_mm.0,
                    y_mm: pin.position_mm.1,
                });
                diagnostics.push(diagnostic);
            }
        }
    }

    diagnostics
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::error::Error;

    use kicad_clippy_core::BuiltinRuleId;
    use kicad_clippy_parser::SchematicDocument;

    use crate::{LintProfile, SchematicFacts};

    use super::validate_declared_frame_overlaps;

    /// Creates the smallest valid profile needed by the functional-frame unit tests.
    ///
    /// # Returns
    ///
    /// A version-one profile declaring two independent groups with distinct frame UUIDs.
    fn frame_profile() -> LintProfile {
        serde_json::from_str(
            r#"{
                "schema_version": 1,
                "groups": {
                    "input": {"description_uuid": "description-input", "frame_uuid": "frame-input"},
                    "controller": {"description_uuid": "description-controller", "frame_uuid": "frame-controller"}
                }
            }"#,
        )
        .expect("valid frame profile")
    }

    /// Confirms overlapping declared functional frames produce an SCH009 error at the overlap.
    ///
    /// # Returns
    ///
    /// Ok when the overlap is reported with both group identities and a schematic location.
    ///
    /// # Errors
    ///
    /// Returns an error if the schematic fixture cannot be parsed.
    #[test]
    fn overlapping_declared_group_frames_are_reported() -> Result<(), Box<dyn Error>> {
        let document = SchematicDocument::from_text(
            "overlapping-frames.kicad_sch",
            r#"(kicad_sch
                (rectangle (start 1 1) (end 10 10) (uuid "frame-input"))
                (rectangle (start 8 4) (end 14 12) (uuid "frame-controller"))
            )"#,
        )?;
        let facts = SchematicFacts::from_document(&document);
        let diagnostics = validate_declared_frame_overlaps(&frame_profile(), &facts);

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, BuiltinRuleId::Sch009.as_str());
        assert!(diagnostics[0].message.contains("input"));
        assert!(diagnostics[0].message.contains("controller"));
        assert!(diagnostics[0].schematic_location.is_some());
        Ok(())
    }

    /// Confirms adjacent declared frame edges are permitted and do not produce a false overlap.
    ///
    /// # Returns
    ///
    /// Ok when the touching-frame fixture produces no frame-overlap diagnostics.
    ///
    /// # Errors
    ///
    /// Returns an error if the schematic fixture cannot be parsed.
    #[test]
    fn touching_group_frame_edges_are_not_reported_as_overlap() -> Result<(), Box<dyn Error>> {
        let document = SchematicDocument::from_text(
            "touching-frames.kicad_sch",
            r#"(kicad_sch
                (rectangle (start 1 1) (end 10 10) (uuid "frame-input"))
                (rectangle (start 10 1) (end 18 10) (uuid "frame-controller"))
            )"#,
        )?;
        let facts = SchematicFacts::from_document(&document);

        assert_eq!(
            validate_declared_frame_overlaps(&frame_profile(), &facts).len(),
            0
        );
        Ok(())
    }

    /// Confirms two groups may intentionally refer to the same frame UUID without a self-overlap error.
    ///
    /// # Returns
    ///
    /// Ok when shared frame ownership produces no SCH009 overlap diagnostic.
    ///
    /// # Errors
    ///
    /// Returns an error if the schematic fixture cannot be parsed.
    #[test]
    fn two_groups_claiming_the_same_frame_do_not_self_overlap() -> Result<(), Box<dyn Error>> {
        let document = SchematicDocument::from_text(
            "shared-frame.kicad_sch",
            r#"(kicad_sch
                (rectangle (start 1 1) (end 10 10) (uuid "shared-frame"))
            )"#,
        )?;
        let mut profile = frame_profile();
        for group in profile.groups.values_mut() {
            group.frame_uuid = Some("shared-frame".to_owned());
        }
        let facts = SchematicFacts::from_document(&document);

        assert_eq!(validate_declared_frame_overlaps(&profile, &facts).len(), 0);
        Ok(())
    }

    /// Builds a one-pin symbol fixture with optional marker and effective electrical type.
    ///
    /// # Arguments
    ///
    /// * `with_marker` — Whether the fixture should contain the marker at the transformed pin coordinate.
    /// * `pin_type` — Base library electrical-type token for the pin.
    /// * `alternate_type` — Optional electrical type declared by an `NC` alternate.
    /// * `select_alternate` — Whether the placed instance selects the `NC` alternate.
    ///
    /// # Returns
    ///
    /// A parsed schematic containing the embedded one-pin symbol and the requested pin-type/marker state.
    ///
    /// # Errors
    ///
    /// Returns an error if the schematic fixture cannot be parsed.
    fn no_connect_document(
        with_marker: bool,
        pin_type: &str,
        alternate_type: Option<&str>,
        select_alternate: bool,
    ) -> Result<SchematicDocument, Box<dyn Error>> {
        let marker = if with_marker {
            r#"(no_connect (at 10 10) (uuid "required-nc"))"#
        } else {
            ""
        };
        let alternate_definition = alternate_type
            .map(|type_token| format!("(alternate \"NC\" {type_token} line)"))
            .unwrap_or_default();
        let instance_alternate = if select_alternate {
            r#"(pin "1" (alternate "NC") (uuid "pin-alternate"))"#
        } else {
            ""
        };
        let source = format!(
            r#"(kicad_sch
                (lib_symbols
                    (symbol "Test:OnePin"
                        (symbol "OnePin_1_1"
                            (pin {pin_type} line (at 0 0 0) (length 1.27)
                                (name "1") (number "1") {alternate_definition})
                        )
                    )
                )
                (symbol (lib_id "Test:OnePin") (at 10 10 0) (unit 1)
                    (property "Reference" "U1" (at 10 9 0)
                        (effects (font (size 1 1))))
                    {instance_alternate}
                )
                {marker}
            )"#
        );
        Ok(SchematicDocument::from_text(
            "required-no-connect.kicad_sch",
            &source,
        )?)
    }

    /// Creates component intent that requires an explicit no-connect marker for U1 pin 1.
    ///
    /// # Returns
    ///
    /// A valid version-one profile with the required no-connect pin attribute.
    fn no_connect_profile() -> LintProfile {
        serde_json::from_str(
            r#"{
                "schema_version": 1,
                "components": {
                    "U1": {
                        "kind": "integrated_circuit",
                        "role": "controller",
                        "function": "Control logic",
                        "rationale": "Fixture for required no-connect coverage",
                        "attributes": {"required_no_connect_pins": "1"}
                    }
                }
            }"#,
        )
        .expect("valid no-connect profile")
    }

    /// Confirms a profile-declared unused pin without a flag emits an SCH015 error.
    ///
    /// # Returns
    ///
    /// Ok when the missing marker is identified by its reference and pin number.
    ///
    /// # Errors
    ///
    /// Returns an error if the schematic fixture cannot be parsed or validated.
    #[test]
    fn declared_required_no_connect_pin_must_have_a_marker() -> Result<(), Box<dyn Error>> {
        let document = no_connect_document(false, "passive", None, false)?;
        let profile = no_connect_profile();
        let diagnostics = profile.validate_with_netlist(&document, None);

        assert!(diagnostics.iter().any(|item| {
            item.code == BuiltinRuleId::Sch015.as_str()
                && item.message.contains("U1.1")
                && item.message.contains("no explicit no-connect marker")
        }));
        Ok(())
    }

    /// Rejects profile no-connect declarations for pins whose base electrical type is already unconnected.
    ///
    /// # Returns
    ///
    /// Ok when an intrinsically unconnected library pin is rejected as a redundant declaration.
    ///
    /// # Errors
    ///
    /// Returns an error if the schematic fixture cannot be parsed or validated.
    #[test]
    fn declared_required_no_connect_pin_rejects_unconnected_library_type()
    -> Result<(), Box<dyn Error>> {
        let document = no_connect_document(false, "unconnected", None, false)?;
        let facts = SchematicFacts::from_document(&document);
        assert_eq!(
            facts.pins[0].electrical_type.as_deref(),
            Some("unconnected")
        );
        let diagnostics = no_connect_profile().validate_with_netlist(&document, None);
        assert!(diagnostics.iter().any(|item| {
            item.code == BuiltinRuleId::Sch015.as_str()
                && item.message.contains("U1.1")
                && item.message.contains("must not be listed")
                && item.message.contains("unconnected")
        }));
        Ok(())
    }

    /// Rejects a required no-connect declaration when the active alternate has no-connect electrical type.
    ///
    /// # Returns
    ///
    /// Ok when the selected alternate type, rather than the passive base type, controls SCH015 validation.
    ///
    /// # Errors
    ///
    /// Returns an error if the schematic fixture cannot be parsed or validated.
    #[test]
    fn declared_required_no_connect_pin_uses_active_alternate_electrical_type()
    -> Result<(), Box<dyn Error>> {
        let document = no_connect_document(false, "passive", Some("no_connect"), true)?;
        let facts = SchematicFacts::from_document(&document);
        assert_eq!(facts.pins[0].electrical_type.as_deref(), Some("no_connect"));
        let diagnostics = no_connect_profile().validate_with_netlist(&document, None);
        assert!(diagnostics.iter().any(|item| {
            item.code == BuiltinRuleId::Sch015.as_str()
                && item.message.contains("U1.1")
                && item.message.contains("must not be listed")
                && item.message.contains("no_connect")
        }));
        Ok(())
    }

    /// Confirms a marker at the transformed embedded pin anchor satisfies the profile contract.
    ///
    /// # Returns
    ///
    /// Ok when the valid marker does not produce an SCH015 missing-marker finding.
    ///
    /// # Errors
    ///
    /// Returns an error if the schematic fixture cannot be parsed or validated.
    #[test]
    fn declared_required_no_connect_pin_accepts_a_marker_at_the_transformed_pin()
    -> Result<(), Box<dyn Error>> {
        let document = no_connect_document(true, "passive", None, false)?;
        let profile = no_connect_profile();
        let diagnostics = profile.validate_with_netlist(&document, None);

        assert!(!diagnostics.iter().any(|item| {
            item.code == BuiltinRuleId::Sch015.as_str() && item.message.contains("U1.1")
        }));
        Ok(())
    }
}
