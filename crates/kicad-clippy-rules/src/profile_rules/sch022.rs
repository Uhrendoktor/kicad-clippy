//! Checks `SCH022` decoupling target-pin, terminal, return-net, and cluster verification.

use kicad_clippy_core::BuiltinRuleId;
use kicad_clippy_core::Diagnostic;
use kicad_clippy_core::Severity;
use kicad_clippy_erc::Netlist;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use super::ComponentIntent;
use super::ComponentKind;
use super::DecouplingIntent;
use super::valid_pin_ref;

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
/// Checks that capacitors assigned to the same decoupling target are grouped consistently.
///
/// # Arguments
///
/// * `components` — Map of component reference designators to their intent records.
///
/// # Returns
///
/// Diagnostics when capacitors assigned to one decoupling target are not grouped consistently or declare conflicting target/return nets.
pub(super) fn validate_decoupler_groups(components: &BTreeMap<String, ComponentIntent>) -> Vec<Diagnostic> {
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
