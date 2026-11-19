//! Checks `SCH014` component pin-to-net expectations against the exported netlist.

use kicad_clippy_core::BuiltinRuleId;
use kicad_clippy_core::Diagnostic;
use kicad_clippy_core::Severity;
use kicad_clippy_erc::Netlist;
use super::ComponentIntent;

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
