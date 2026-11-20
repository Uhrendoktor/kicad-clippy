//! Checks `SCH022` decoupling target-pin, terminal, return-net, and cluster verification.

use super::ComponentIntent;
use super::ComponentKind;
use super::DecouplingIntent;
use super::SchematicFacts;
use super::valid_pin_ref;
use crate::generic::GEOMETRY_TOLERANCE_MM;
use kicad_clippy_core::BuiltinRuleId;
use kicad_clippy_core::Diagnostic;
use kicad_clippy_core::Severity;
use kicad_clippy_erc::Netlist;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

/// Validates a capacitor's decoupling target and supply/return net declarations.
///
/// # Arguments
///
/// * `reference` — Reference designator of the component whose intent is being checked.
/// * `contract` — Optional decoupling contract declared for this component.
/// * `netlist` — Optional `KiCad`-exported flattened netlist; supplies logical pin-to-net assignments when present.
///
/// # Returns
///
/// Diagnostics when a decoupling capacitor lacks a target pin, target terminal, return terminal, or declared return net.
pub(super) fn validate_decoupling_intent(
    reference: &str,
    contract: Option<&DecouplingIntent>,
    netlist: Option<&Netlist>,
) -> Vec<Diagnostic> {
    let Some(contract) = contract else {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch022,
            Severity::Error,
            format!(
                "{reference} is a decoupling capacitor but does not identify its target pin and terminal/net contract"
            ),
        )];
    };
    let mut diagnostics = Vec::new();
    if !valid_pin_ref(&contract.target_pin) {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch022,
            Severity::Error,
            format!(
                "{reference} has invalid decoupling target pin {:?}",
                contract.target_pin
            ),
        ));
    }
    if contract.target_terminal.is_empty()
        || contract.return_terminal.is_empty()
        || contract.return_net.is_empty()
        || contract.cluster.is_empty()
    {
        diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch022,
            Severity::Error,
            format!(
                "{reference} decoupling metadata must define target/return terminals, return net, and cluster"
            ),
        ));
    }
    if contract.target_terminal == contract.return_terminal {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch022,
            Severity::Error,
            format!("{reference} maps both decoupling terminals to the same capacitor terminal"),
        ));
    }
    if contract
        .max_target_distance_mm
        .is_some_and(|distance| !distance.is_finite() || distance < 0.0)
    {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch022,
            Severity::Error,
            format!("{reference} max_target_distance_mm must be a finite non-negative distance"),
        ));
    }
    if let Some(netlist) = netlist {
        diagnostics.extend(validate_decoupling_nets(reference, contract, netlist));
    }
    diagnostics
}
/// Checks that the decoupling capacitor connects to its declared target net and return net.
///
/// # Arguments
///
/// * `reference` — Reference designator of the component whose intent is being checked.
/// * `contract` — Optional decoupling contract declared for this component.
/// * `netlist` — Optional `KiCad`-exported flattened netlist; supplies logical pin-to-net assignments when present.
///
/// # Returns
///
/// Diagnostics when the capacitor's exported target or return pin resolves to a different net than its decoupling contract declares.
fn validate_decoupling_nets(
    reference: &str,
    contract: &DecouplingIntent,
    netlist: &Netlist,
) -> Vec<Diagnostic> {
    let Some((target_reference, target_pin)) = contract.target_pin.rsplit_once('.') else {
        return Vec::new();
    };
    let target_net = netlist.net_for_pin(target_reference, target_pin);
    let capacitor_target_net = netlist.net_for_pin(reference, &contract.target_terminal);
    let capacitor_return_net = netlist.net_for_pin(reference, &contract.return_terminal);
    let mut diagnostics = Vec::new();

    match (target_net, capacitor_target_net) {
        (Some(target), Some(capacitor)) if target == capacitor => {}
        (Some(target), Some(capacitor)) => diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch022,
            Severity::Error,
            format!(
                "{reference} terminal {} is on {capacitor}, but {}.{} is on {target}",
                contract.target_terminal, target_reference, target_pin
            ),
        )),
        (None, _) => diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch022,
            Severity::Error,
            format!("target pin {target_reference}.{target_pin} is absent from the exported KiCad netlist"),
        )),
        (_, None) => diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch022,
            Severity::Error,
            format!(
                "{}.{} is absent from the exported KiCad netlist",
                reference, contract.target_terminal
            ),
        )),
    }

    match capacitor_return_net {
        Some(actual) if actual == contract.return_net => {}
        Some(actual) => diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch022,
            Severity::Error,
            format!(
                "{reference} return terminal {} is on {actual}, expected {}",
                contract.return_terminal, contract.return_net
            ),
        )),
        None => diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch022,
            Severity::Error,
            format!(
                "{}.{} is absent from the exported KiCad netlist",
                reference, contract.return_terminal
            ),
        )),
    }
    diagnostics
}

/// Resolves a unique placed-symbol pin fact by reference and pin number.
///
/// Returns `None` for absent or ambiguous pin geometry so callers can report
/// an unverified contract instead of selecting an arbitrary multi-unit pin.
///
/// # Arguments
///
/// * `facts` — Normalized placed-symbol pin facts for the current schematic.
/// * `reference` — Component reference designator.
/// * `number` — Serialized library pin number.
///
/// # Returns
///
/// The unique matching placed pin, or `None` when geometry cannot be resolved unambiguously.
fn unique_pin_fact<'a>(
    facts: &'a SchematicFacts,
    reference: &str,
    number: &str,
) -> Option<&'a crate::generic::SchematicPinFact> {
    let mut matches = facts
        .pins
        .iter()
        .filter(|pin| pin.reference == reference && pin.number == number);
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

/// Checks the optional maximum schematic distance between a decoupling capacitor terminal
/// and its declared target IC pin.
///
/// # Arguments
///
/// * `reference` — Reference designator of the decoupling capacitor.
/// * `contract` — Explicit capacitor target/terminal contract.
/// * `facts` — Normalized transformed pin coordinates from the schematic.
///
/// # Returns
///
/// SCH022 errors for distances above the configured maximum, or warnings when either pin
/// coordinate is absent or ambiguous.
fn validate_decoupling_target_distance(
    reference: &str,
    contract: &DecouplingIntent,
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    let Some(maximum_distance_mm) = contract.max_target_distance_mm else {
        return Vec::new();
    };
    if !maximum_distance_mm.is_finite() || maximum_distance_mm < 0.0 {
        return Vec::new();
    }
    let Some((target_reference, target_number)) = contract.target_pin.rsplit_once('.') else {
        return Vec::new();
    };
    let (Some(target_pin), Some(capacitor_pin)) = (
        unique_pin_fact(facts, target_reference, target_number),
        unique_pin_fact(facts, reference, &contract.target_terminal),
    ) else {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch022,
            Severity::Warning,
            format!(
                "{reference} max_target_distance_mm is unverified because target pin {} or capacitor terminal {} has missing or ambiguous placed-symbol geometry",
                contract.target_pin, contract.target_terminal
            ),
        )];
    };
    let actual_distance_mm = (target_pin.position_mm.0 - capacitor_pin.position_mm.0)
        .hypot(target_pin.position_mm.1 - capacitor_pin.position_mm.1);
    if actual_distance_mm > maximum_distance_mm + GEOMETRY_TOLERANCE_MM {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch022,
            Severity::Error,
            format!(
                "{reference} target terminal {} is {:.2} mm from {}, exceeding max_target_distance_mm of {:.2} mm",
                contract.target_terminal,
                actual_distance_mm,
                contract.target_pin,
                maximum_distance_mm
            ),
        )];
    }
    Vec::new()
}

/// Checks every profile-declared required IC power pin has at least one decoupler contract.
///
/// The declaration is opt-in through `components.<IC>.attributes.required_decoupling_pins`.
/// Pin existence is checked against KiCad's exported netlist when one is available.
///
/// # Arguments
///
/// * `components` — All component-intent declarations in the project profile.
/// * `netlist` — Optional flattened KiCad netlist used to verify each required IC pin exists.
///
/// # Returns
///
/// SCH022 diagnostics for invalid lists, absent declared pins, or required pins without a decoupler.
fn validate_required_decoupling_coverage(
    components: &BTreeMap<String, ComponentIntent>,
    netlist: Option<&Netlist>,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for (reference, intent) in components {
        let Some(raw_required_pins) = intent.attributes.get("required_decoupling_pins") else {
            continue;
        };
        if intent.kind != ComponentKind::IntegratedCircuit {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch022,
                Severity::Error,
                format!(
                    "{reference} declares required_decoupling_pins but is not an integrated_circuit"
                ),
            ));
            continue;
        }
        let tokens: Vec<_> = raw_required_pins.split(',').map(str::trim).collect();
        let mut required_pins = BTreeSet::new();
        for pin in tokens {
            if pin.is_empty() || !valid_pin_ref(&format!("{reference}.{pin}")) {
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch022,
                    Severity::Error,
                    format!(
                        "{reference} required_decoupling_pins contains invalid pin number {pin:?}"
                    ),
                ));
                continue;
            }
            if !required_pins.insert(pin.to_owned()) {
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch022,
                    Severity::Warning,
                    format!("{reference} required_decoupling_pins repeats pin {pin}"),
                ));
            }
        }
        if required_pins.is_empty() {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch022,
                Severity::Error,
                format!("{reference} required_decoupling_pins must list at least one pin number"),
            ));
            continue;
        }

        for pin in required_pins {
            if let Some(netlist) = netlist {
                if netlist.net_for_pin(reference, &pin).is_none() {
                    diagnostics.push(Diagnostic::for_builtin_rule(
                        BuiltinRuleId::Sch022,
                        Severity::Error,
                        format!("{reference}.{pin} is declared as requiring decoupling but is absent from the exported KiCad netlist"),
                    ));
                }
            }
            let target = format!("{reference}.{pin}");
            let covered = components.values().any(|candidate| {
                candidate.kind == ComponentKind::Capacitor
                    && candidate.role.0 == "decoupling"
                    && candidate
                        .decoupling
                        .as_ref()
                        .is_some_and(|contract| contract.target_pin == target)
            });
            if !covered {
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch022,
                    Severity::Error,
                    format!("{target} is declared as requiring decoupling but no decoupling capacitor targets it"),
                ));
            }
        }
        if netlist.is_none() {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch022,
                Severity::Warning,
                format!("{reference} required decoupling pin existence was not verified because no KiCad netlist was supplied"),
            ));
        }
    }
    diagnostics
}

/// Checks that capacitors assigned to the same decoupling target are grouped consistently.
///
/// # Arguments
///
/// * `components` — Map of component reference designators to their intent records.
///
/// # Returns
///
/// Diagnostics when capacitors assigned to one decoupling target are not grouped consistently or declare conflicting target/return nets.
pub(super) fn validate_decoupler_groups(
    components: &BTreeMap<String, ComponentIntent>,
) -> Vec<Diagnostic> {
    let mut by_target: BTreeMap<&str, Vec<&DecouplingIntent>> = BTreeMap::new();
    for intent in components.values() {
        if intent.kind == ComponentKind::Capacitor && intent.role.0 == "decoupling" {
            if let Some(contract) = &intent.decoupling {
                by_target
                    .entry(&contract.target_pin)
                    .or_default()
                    .push(contract);
            }
        }
    }
    let mut diagnostics = Vec::new();
    for (target, capacitors) in by_target {
        let returns: BTreeSet<_> = capacitors
            .iter()
            .map(|item| item.return_net.as_str())
            .collect();
        let clusters: BTreeSet<_> = capacitors
            .iter()
            .map(|item| item.cluster.as_str())
            .collect();
        if returns.len() > 1 {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch022,
                Severity::Error,
                format!(
                    "decouplers for {target} disagree on their return net: {}",
                    returns.into_iter().collect::<Vec<_>>().join(", ")
                ),
            ));
        }
        if clusters.len() > 1 {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch022,
                Severity::Warning,
                format!(
                    "decouplers for {target} are split across cluster identifiers: {}",
                    clusters.into_iter().collect::<Vec<_>>().join(", ")
                ),
            ));
        }
    }
    diagnostics
}

/// Concrete implementation of built-in rule SCH022.
pub(super) struct Sch022;
impl<'a> crate::Rule<super::ProfileRuleContext<'a>> for Sch022 {
    /// Returns the stable SCH022 identifier.
    ///
    /// # Returns
    ///
    /// The stable SCH022 identifier used to associate diagnostics with this rule.
    fn id(&self) -> BuiltinRuleId {
        BuiltinRuleId::Sch022
    }
    /// Evaluates decoupling target/terminal/net contracts and consistency between capacitors sharing a target.
    ///
    /// # Arguments
    ///
    /// * `context` — Component intent declarations and the optional exported netlist.
    ///
    /// # Returns
    ///
    /// Diagnostics for invalid decoupling metadata, mismatched terminal nets, or inconsistent decoupler clusters.
    fn check(&self, context: &super::ProfileRuleContext<'a>) -> Vec<Diagnostic> {
        let mut diagnostics = validate_decoupler_groups(&context.profile.components);
        diagnostics.extend(validate_required_decoupling_coverage(
            &context.profile.components,
            context.netlist,
        ));
        for (reference, intent) in &context.profile.components {
            if intent.kind == ComponentKind::Capacitor && intent.role.0 == "decoupling" {
                if let Some(contract) = intent.decoupling.as_ref() {
                    diagnostics.extend(validate_decoupling_intent(
                        reference,
                        Some(contract),
                        context.netlist,
                    ));
                    diagnostics.extend(validate_decoupling_target_distance(
                        reference,
                        contract,
                        context.facts,
                    ));
                } else {
                    diagnostics.extend(validate_decoupling_intent(
                        reference,
                        None,
                        context.netlist,
                    ));
                }
            }
        }
        diagnostics
    }
}

#[cfg(test)]
/// Tests optional decoupling coverage and transformed pin-distance contracts.
mod tests {
    #![allow(clippy::expect_used)]

    use super::{
        validate_decoupling_intent, validate_decoupling_target_distance,
        validate_required_decoupling_coverage,
    };
    use crate::generic::{SchematicFacts, SchematicPinFact};
    use crate::metadata::{ComponentIntent, ComponentKind, ComponentRole, DecouplingIntent};
    use kicad_clippy_erc::{Netlist, PinRef};
    use std::collections::BTreeMap;

    /// Builds one minimal component intent with optional decoupling metadata and attributes.
    ///
    /// # Arguments
    ///
    /// * `kind` — Component family for the intent.
    /// * `role` — Functional role for the intent.
    /// * `decoupling` — Optional decoupling target/terminal contract.
    /// * `attributes` — Extensible role-specific profile attributes.
    ///
    /// # Returns
    ///
    /// A component intent record for SCH022 tests.
    fn intent(
        kind: ComponentKind,
        role: &str,
        decoupling: Option<DecouplingIntent>,
        attributes: BTreeMap<String, String>,
    ) -> ComponentIntent {
        ComponentIntent {
            kind,
            role: ComponentRole(role.to_owned()),
            function: Some("Test function".to_owned()),
            rationale: Some("Test rationale".to_owned()),
            evidence: None,
            decoupling,
            connections: BTreeMap::new(),
            pin_nets: BTreeMap::new(),
            attributes,
            max_field_body_gap_mm: BTreeMap::new(),
            voltage_rating_min_v: None,
            power_rating_min_w: None,
        }
    }

    /// Creates a decoupling declaration targeting U1.1 through C1.1 and returning through C1.2.
    ///
    /// # Arguments
    ///
    /// * `maximum_distance_mm` — Optional maximum schematic pin-anchor distance.
    ///
    /// # Returns
    ///
    /// A decoupling intent with distinct target/return terminals.
    fn decoupling(maximum_distance_mm: Option<f64>) -> DecouplingIntent {
        DecouplingIntent {
            target_pin: "U1.1".to_owned(),
            target_terminal: "1".to_owned(),
            return_terminal: "2".to_owned(),
            return_net: "GND".to_owned(),
            cluster: "U1.SUPPLY".to_owned(),
            max_target_distance_mm: maximum_distance_mm,
        }
    }

    /// Rejects a required supply pin that has no capacitor profile contract.
    #[test]
    fn required_power_pin_must_have_a_decoupler_contract() {
        let components = BTreeMap::from([(
            "U1".to_owned(),
            intent(
                ComponentKind::IntegratedCircuit,
                "controller",
                None,
                BTreeMap::from([("required_decoupling_pins".to_owned(), "1, 2".to_owned())]),
            ),
        )]);
        let diagnostics = validate_required_decoupling_coverage(&components, None);
        assert!(diagnostics.iter().any(|item| {
            item.code == "SCH022"
                && item.message.contains("U1.1")
                && item.message.contains("no decoupling capacitor targets it")
        }));
        assert!(diagnostics.iter().any(|item| {
            item.code == "SCH022"
                && item.message.contains("U1.2")
                && item.message.contains("no decoupling capacitor targets it")
        }));
    }

    /// Accepts declared supply pins that each have at least one matching decoupler intent.
    #[test]
    fn required_power_pin_coverage_accepts_matching_decoupler_contracts() {
        let components = BTreeMap::from([
            (
                "U1".to_owned(),
                intent(
                    ComponentKind::IntegratedCircuit,
                    "controller",
                    None,
                    BTreeMap::from([("required_decoupling_pins".to_owned(), "1".to_owned())]),
                ),
            ),
            (
                "C1".to_owned(),
                intent(
                    ComponentKind::Capacitor,
                    "decoupling",
                    Some(decoupling(None)),
                    BTreeMap::new(),
                ),
            ),
        ]);
        let diagnostics = validate_required_decoupling_coverage(&components, None);
        assert!(!diagnostics.iter().any(|item| {
            item.code == "SCH022" && item.message.contains("no decoupling capacitor targets")
        }));
    }

    /// Verifies a decoupler distance using transformed pin anchors rather than component origins.
    #[test]
    fn decoupler_target_distance_uses_pin_anchors() {
        let facts = SchematicFacts {
            pins: vec![
                SchematicPinFact {
                    reference: "U1".to_owned(),
                    number: "1".to_owned(),
                    position_mm: (0.0, 0.0),
                    electrical_type: Some("power_in".to_owned()),
                },
                SchematicPinFact {
                    reference: "C1".to_owned(),
                    number: "1".to_owned(),
                    position_mm: (3.0, 4.0),
                    electrical_type: Some("passive".to_owned()),
                },
            ],
            ..SchematicFacts::default()
        };
        let diagnostics = validate_decoupling_target_distance("C1", &decoupling(Some(4.0)), &facts);
        assert!(diagnostics.iter().any(|item| {
            item.code == "SCH022"
                && item.message.contains("5.00 mm")
                && item.message.contains("exceeding max_target_distance_mm")
        }));
        assert!(
            validate_decoupling_target_distance("C1", &decoupling(Some(5.0)), &facts).is_empty()
        );
    }

    /// Rejects an invalid negative placement-distance threshold.
    #[test]
    fn negative_decoupler_distance_threshold_is_rejected() {
        let diagnostics = validate_decoupling_intent("C1", Some(&decoupling(Some(-1.0))), None);
        assert!(diagnostics.iter().any(|item| {
            item.code == "SCH022" && item.message.contains("finite non-negative distance")
        }));
    }

    /// Confirms a required power pin missing from the exported netlist is diagnosed.
    #[test]
    fn required_power_pin_must_exist_in_exported_netlist() {
        let components = BTreeMap::from([(
            "U1".to_owned(),
            intent(
                ComponentKind::IntegratedCircuit,
                "controller",
                None,
                BTreeMap::from([("required_decoupling_pins".to_owned(), "7".to_owned())]),
            ),
        )]);
        let netlist = Netlist {
            pin_nets: BTreeMap::from([(
                PinRef {
                    reference: "U1".to_owned(),
                    pin: "1".to_owned(),
                },
                "VDD".to_owned(),
            )]),
            ..Netlist::default()
        };
        assert!(
            validate_required_decoupling_coverage(&components, Some(&netlist))
                .iter()
                .any(|item| {
                    item.code == "SCH022"
                        && item.message.contains("U1.7")
                        && item
                            .message
                            .contains("absent from the exported KiCad netlist")
                })
        );
    }
}
