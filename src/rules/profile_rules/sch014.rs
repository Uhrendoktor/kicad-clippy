//! Checks `SCH014` component pin-to-net expectations against the exported netlist.

use super::CommonPinNetGroup;
use super::ComponentIntent;
use super::SeparateWirePair;
use super::valid_pin_ref;
use crate::core::BuiltinRuleId;
use crate::core::Diagnostic;
use crate::core::Severity;
use crate::erc::Netlist;
use std::collections::BTreeSet;

/// Checks the declared pin-to-net expectations against the exported logical netlist.
///
/// # Arguments
///
/// * `reference` — Reference designator of the component whose intent is being checked.
/// * `intent` — Component-intent record declaring the electrical role and connection contract.
/// * `netlist` — Optional `KiCad`-exported flattened netlist; supplies logical pin-to-net assignments when present.
///
/// # Returns
///
/// Diagnostics when a declared pin-to-net mapping is missing from or disagrees with the exported netlist.
pub(super) fn validate_pin_net_contract(
    reference: &str,
    intent: &ComponentIntent,
    netlist: Option<&Netlist>,
) -> Vec<Diagnostic> {
    let mut mappings = intent.pin_nets.clone();
    let mut diagnostics = Vec::new();
    for (key, expected_net) in &intent.connections {
        let Some(pin) = key.strip_prefix("pin.") else {
            continue;
        };
        if let Some(previous) = mappings.insert(pin.to_owned(), expected_net.clone()) {
            if previous != *expected_net {
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch014,
                    Severity::Error,
                    format!(
                        "{reference} pin {pin} has conflicting expected nets {previous:?} and {expected_net:?}"
                    ),
                ));
            }
        }
    }
    if mappings.is_empty() {
        return diagnostics;
    }
    let Some(netlist) = netlist else {
        diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch014,
            Severity::Warning,
            format!(
                "{reference} declares pin-to-net contracts but no KiCad netlist was supplied; the contracts were not checked"
            ),
        ));
        return diagnostics;
    };

    for (pin, expected_net) in mappings {
        if pin.trim().is_empty() || expected_net.trim().is_empty() {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch014,
                Severity::Error,
                format!("{reference} pin-to-net mapping must use non-empty pin and net names"),
            ));
            continue;
        }
        match netlist.net_for_pin(reference, &pin) {
            Some(actual_net) if actual_net == expected_net => {}
            Some(actual_net) => diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch014,
                Severity::Error,
                format!(
                    "{reference}.{pin} is connected to {actual_net}, but the profile expects {expected_net}"
                ),
            )),
            None => diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch014,
                Severity::Error,
                format!(
                    "{reference}.{pin} is absent from the exported KiCad netlist but is assigned to {expected_net} in the profile"
                ),
            )),
        }
    }
    diagnostics
}

/// Checks a common-pin group's logical netlist connectivity independently of physical wire topology.
///
/// # Arguments
///
/// * `group` — Profile declaration whose pin references must share one exported logical net.
/// * `netlist` — Optional flattened `KiCad` netlist used as the authoritative pin-to-net map.
///
/// # Returns
///
/// SCH014 diagnostics when a requested group is not on one exported net, contains absent pins, or cannot be checked.
fn validate_common_pin_group_netlist(
    group: &CommonPinNetGroup,
    netlist: Option<&Netlist>,
) -> Vec<Diagnostic> {
    if !group.netlist_connected {
        return Vec::new();
    }
    let Some(netlist) = netlist else {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch014,
            Severity::Warning,
            format!(
                "common pin group {:?} requests netlist_connected but no exported KiCad netlist was supplied",
                group.name
            ),
        )];
    };

    let mut nets = BTreeSet::new();
    let mut diagnostics = Vec::new();
    for pin_ref in &group.pins {
        let Some((reference, pin_number)) = pin_ref.rsplit_once('.') else {
            continue;
        };
        if !valid_pin_ref(pin_ref) {
            continue;
        }
        match netlist.net_for_pin(reference, pin_number) {
            Some(net_name) => {
                nets.insert(net_name.to_owned());
            }
            None => diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch014,
                Severity::Error,
                format!(
                    "{pin_ref} in common pin group {:?} is absent from the exported KiCad netlist",
                    group.name
                ),
            )),
        }
    }
    if nets.len() > 1 {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch014,
            Severity::Error,
            format!(
                "pins in common pin group {:?} resolve to different exported KiCad nets: {}",
                group.name,
                nets.into_iter().collect::<Vec<_>>().join(", ")
            ),
        ));
    }
    diagnostics
}

/// Checks that a separate-wire pair resolves to two different exported logical nets.
///
/// Physical wire-only separation remains owned by SCH004; this check catches
/// labels and other logical connectivity that can join separate physical islands.
///
/// # Arguments
///
/// * `pair` — Declared pair of distinct REF.PIN references.
/// * `netlist` — Optional flattened `KiCad` netlist used as the authoritative pin-to-net map.
///
/// # Returns
///
/// SCH014 diagnostics when the two references resolve to one net, are absent, or the requested check is unavailable.
fn validate_separate_wire_pair_netlist(
    pair: &SeparateWirePair,
    netlist: Option<&Netlist>,
) -> Vec<Diagnostic> {
    if !pair.netlist_separate {
        return Vec::new();
    }
    let pair_name = format!("{} / {}", pair.first, pair.second);
    let Some(netlist) = netlist else {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch014,
            Severity::Warning,
            format!(
                "separate-wire pair {pair_name} requests netlist_separate but no exported KiCad netlist was supplied"
            ),
        )];
    };
    if !valid_pin_ref(&pair.first) || !valid_pin_ref(&pair.second) || pair.first == pair.second {
        return Vec::new();
    }
    let (Some((first_reference, first_number)), Some((second_reference, second_number))) =
        (pair.first.rsplit_once('.'), pair.second.rsplit_once('.'))
    else {
        return Vec::new();
    };

    let mut diagnostics = Vec::new();
    match (
        netlist.net_for_pin(first_reference, first_number),
        netlist.net_for_pin(second_reference, second_number),
    ) {
        (Some(first), Some(second)) if first == second => {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch014,
                Severity::Error,
                format!(
                    "pins {} and {} must resolve to different exported KiCad nets, but both resolve to {first:?}",
                    pair.first, pair.second
                ),
            ));
        }
        (None, _) => diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch014,
            Severity::Error,
            format!(
                "{} in separate-wire pair {pair_name} is absent from the exported KiCad netlist",
                pair.first
            ),
        )),
        (_, None) => diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch014,
            Severity::Error,
            format!(
                "{} in separate-wire pair {pair_name} is absent from the exported KiCad netlist",
                pair.second
            ),
        )),
        (Some(_), Some(_)) => {}
    }
    diagnostics
}

/// Concrete implementation of built-in rule SCH014.
pub(super) struct Sch014;
impl<'a> crate::rules::Rule<super::ProfileRuleContext<'a>> for Sch014 {
    /// Returns the stable SCH014 identifier.
    ///
    /// # Returns
    ///
    /// The stable SCH014 identifier used to associate diagnostics with this rule.
    fn id(&self) -> BuiltinRuleId {
        BuiltinRuleId::Sch014
    }
    /// Evaluates all profile-declared logical pin-to-net relationships against the exported netlist.
    ///
    /// # Arguments
    ///
    /// * `context` — Component, common-group, and separate-pair declarations plus the optional exported logical netlist.
    ///
    /// # Returns
    ///
    /// Diagnostics when declared component or group pins resolve to unexpected, absent, or shared logical nets.
    fn check(&self, context: &super::ProfileRuleContext<'a>) -> Vec<Diagnostic> {
        let mut diagnostics: Vec<_> = context
            .profile
            .components
            .iter()
            .flat_map(|(reference, intent)| {
                validate_pin_net_contract(reference, intent, context.netlist)
            })
            .collect();
        diagnostics.extend(
            context
                .profile
                .common_pin_groups
                .iter()
                .flat_map(|group| validate_common_pin_group_netlist(group, context.netlist)),
        );
        diagnostics.extend(
            context
                .profile
                .separate_wire_pairs
                .iter()
                .flat_map(|pair| validate_separate_wire_pair_netlist(pair, context.netlist)),
        );
        diagnostics
    }
}
