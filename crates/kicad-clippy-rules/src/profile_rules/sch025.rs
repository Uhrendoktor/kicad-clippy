//! Checks required test-point coverage for project-critical nets.

use super::{ComponentKind, LintProfile, ProfileRuleContext};
use crate::Rule;
use kicad_clippy_core::{BuiltinRuleId, Diagnostic, Severity};
use kicad_clippy_erc::Netlist;

/// Validates required critical nets against declared test-point pins and optional netlist evidence.
///
/// # Arguments
///
/// * `profile` — Project profile listing critical nets and test-point component pin mappings.
/// * `netlist` — Optional exported schematic netlist used to verify each declared test-point pin's actual net.
///
/// # Returns
///
/// SCH025 errors for missing or mismatched test-point declarations and warnings when netlist evidence is unavailable.
pub(super) fn validate_required_test_point_nets(
    profile: &LintProfile,
    netlist: Option<&Netlist>,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for expected_net in &profile.required_test_point_nets {
        if expected_net.trim().is_empty() || expected_net.trim() != expected_net {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch025,
                Severity::Error,
                "required_test_point_nets entries must be trimmed, non-empty net names",
            ));
            continue;
        }
        let candidates = profile
            .components
            .iter()
            .filter(|(_, intent)| {
                intent.kind == ComponentKind::TestPoint && intent.role.0 == "test_point"
            })
            .flat_map(|(reference, intent)| {
                intent
                    .pin_nets
                    .iter()
                    .filter(|(_, net)| net == &expected_net)
                    .map(|(pin, _)| (reference.clone(), pin.clone()))
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch025,
                Severity::Error,
                format!(
                    "required test-point net {expected_net:?} has no profile-declared test_point component pin mapping"
                ),
            ));
            continue;
        }
        let Some(netlist) = netlist else {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch025,
                Severity::Warning,
                format!(
                    "test-point declarations for required net {expected_net:?} are not verified against an exported KiCad netlist"
                ),
            ));
            continue;
        };
        for (reference, pin) in candidates {
            match netlist.net_for_pin(&reference, &pin) {
                Some(actual_net) if actual_net == expected_net => {}
                Some(actual_net) => diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch025,
                    Severity::Error,
                    format!(
                        "{reference}.{pin} is declared as a test point for {expected_net:?}, but the exported netlist assigns it to {actual_net:?}"
                    ),
                )),
                None => diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch025,
                    Severity::Error,
                    format!(
                        "{reference}.{pin} is declared as a test point for {expected_net:?}, but is absent from the exported KiCad netlist"
                    ),
                )),
            }
        }
    }
    diagnostics
}

/// Concrete implementation of SCH025.
pub(super) struct Sch025;

impl<'a> Rule<ProfileRuleContext<'a>> for Sch025 {
    /// Returns the stable SCH025 identifier.
    ///
    /// # Returns
    ///
    /// The identifier associated with required test-point coverage.
    fn id(&self) -> BuiltinRuleId {
        BuiltinRuleId::Sch025
    }

    /// Evaluates profile-declared test-point coverage for critical nets.
    ///
    /// # Arguments
    ///
    /// * `context` — Profile declarations and optional exported-netlist evidence for the current schematic.
    ///
    /// # Returns
    ///
    /// Diagnostics for required nets without declared or netlist-confirmed test-point pins.
    fn check(&self, context: &ProfileRuleContext<'a>) -> Vec<Diagnostic> {
        validate_required_test_point_nets(context.profile, context.netlist)
    }
}

#[cfg(test)]
/// Tests profile-declared test-point coverage and netlist confirmation.
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::validate_required_test_point_nets;
    use crate::LintProfile;
    use kicad_clippy_core::Severity;
    use kicad_clippy_erc::{Netlist, PinRef};
    use std::collections::BTreeMap;

    /// Parses a version-one profile requiring a VDD test point.
    ///
    /// # Arguments
    ///
    /// * `include_test_point` — Whether TP1 pin 1 is declared as the VDD test point.
    ///
    /// # Returns
    ///
    /// A profile with required VDD coverage and an optional TP1 component declaration.
    fn profile(include_test_point: bool) -> LintProfile {
        let source = if include_test_point {
            r#"{
                "schema_version": 1,
                "required_test_point_nets": ["VDD"],
                "components": {
                    "TP1": {
                        "kind": "test_point",
                        "role": "test_point",
                        "function": "VDD measurement access",
                        "rationale": "Provides a production test pad",
                        "pin_nets": {"1": "VDD"}
                    }
                }
            }"#
        } else {
            r#"{"schema_version":1,"required_test_point_nets":["VDD"]}"#
        };
        serde_json::from_str(source).expect("profile fixture parses")
    }

    /// Builds an exported schematic netlist with one supplied TP1.1 assignment.
    ///
    /// # Arguments
    ///
    /// * `actual_net` — Optional actual net assigned to TP1 pin 1; omission leaves the pin out of the netlist.
    ///
    /// # Returns
    ///
    /// A flattened netlist containing only the optional TP1.1 pin-to-net mapping.
    fn netlist(actual_net: Option<&str>) -> Netlist {
        let mut pin_nets = BTreeMap::new();
        if let Some(net) = actual_net {
            pin_nets.insert(
                PinRef {
                    reference: "TP1".to_owned(),
                    pin: "1".to_owned(),
                },
                net.to_owned(),
            );
        }
        Netlist {
            pin_nets,
            ..Netlist::default()
        }
    }

    /// Accepts a declared test point whose exported pin is on the required net.
    #[test]
    fn required_test_point_is_confirmed_by_exported_netlist() {
        let diagnostics =
            validate_required_test_point_nets(&profile(true), Some(&netlist(Some("VDD"))));
        assert!(diagnostics.is_empty());
    }

    /// Rejects a required critical net without a profile-declared test-point pin mapping.
    #[test]
    fn required_net_needs_a_declared_test_point() {
        let diagnostics = validate_required_test_point_nets(&profile(false), Some(&netlist(None)));
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "SCH025");
        assert!(diagnostics[0].message.contains("no profile-declared"));
    }

    /// Reports a warning when the test-point declaration cannot be compared with an exported netlist.
    #[test]
    fn missing_netlist_makes_declared_test_point_unverified() {
        let diagnostics = validate_required_test_point_nets(&profile(true), None);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, Severity::Warning);
        assert!(diagnostics[0].message.contains("not verified"));
    }


    /// Rejects a declared test-point pin that is missing from the exported netlist.
    #[test]
    fn test_point_pin_must_exist_in_exported_netlist() {
        let diagnostics = validate_required_test_point_nets(&profile(true), Some(&netlist(None)));
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "SCH025");
        assert!(diagnostics[0].message.contains("absent from the exported"));
    }

    /// Rejects a test-point pin assigned to a different net in the exported netlist.
    #[test]
    fn test_point_pin_must_belong_to_required_net() {
        let diagnostics =
            validate_required_test_point_nets(&profile(true), Some(&netlist(Some("GND"))));
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "SCH025");
        assert!(diagnostics[0].message.contains("assigns it to \"GND\""));
    }

    /// Rejects whitespace-only and untrimmed required net names.
    #[test]
    fn invalid_required_net_names_are_rejected() {
        let mut profile = profile(true);
        profile.required_test_point_nets.insert("   ".to_owned());
        profile.required_test_point_nets.insert(" VCC ".to_owned());
        let diagnostics =
            validate_required_test_point_nets(&profile, Some(&netlist(Some("VDD"))));
        assert_eq!(
            diagnostics
                .iter()
                .filter(|item| item.code == "SCH025" && item.message.contains("trimmed"))
                .count(),
            2
        );
    }
}
