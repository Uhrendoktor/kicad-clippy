//! Checks `SCH004` profile-declared physical pin-connectivity and wire-separation contracts.

use super::COORDINATE_KEY_DECIMAL_PLACES;
use super::CommonPinNetGroup;
use super::DirectWirePair;
use super::MINIMUM_PIN_GROUP_SIZE;
use super::PhysicalWireGroup;
use super::SchematicFacts;
use super::SeparateWirePair;
use super::valid_pin_ref;
use crate::generic::GEOMETRY_TOLERANCE_MM;
use crate::generic::physical_wire_component_ids;
use kicad_clippy_core::BuiltinRuleId;
use kicad_clippy_core::Diagnostic;
use kicad_clippy_core::Severity;
use kicad_clippy_erc::Netlist;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

/// Checks declared pin pairs and forbidden labels against physical wire connectivity, not merely matching net names.
///
/// # Arguments
///
/// * `pairs` — Declared physical-wire pin pairs, each expressed as two REF.PIN references.
/// * `forbidden_labels` — Names that are not permitted on the selected physical wire components.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
///
/// # Returns
///
/// Diagnostics for declared direct-wire pairs that are not physically connected, or for forbidden labels attached to those wire components.
pub(super) fn validate_direct_wire_connectivity(
    pairs: &[DirectWirePair],
    forbidden_labels: &BTreeSet<String>,
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    if pairs.is_empty() && forbidden_labels.is_empty() {
        return Vec::new();
    }
    let component_ids = physical_wire_component_ids(facts);
    let mut protected_components = BTreeSet::new();
    let mut diagnostics = Vec::new();

    for pair in pairs {
        let (mut pair_diagnostics, pair_components) =
            validate_direct_wire_pair(pair, facts, &component_ids);
        diagnostics.append(&mut pair_diagnostics);
        protected_components.extend(pair_components);
    }
    diagnostics.extend(validate_forbidden_direct_wire_labels(
        forbidden_labels,
        &protected_components,
        &component_ids,
        facts,
    ));
    diagnostics
}
/// Checks that both REF.PIN endpoints share one physical wire-only component and do not have disallowed label anchors.
///
/// # Arguments
///
/// * `pair` — The direct-wire or separate-wire pair contract being validated.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `component_ids` — Map from tolerance-normalized schematic coordinates to physical wire-component identifiers.
///
/// # Returns
///
/// The diagnostics produced for this pair and the physical wire-component identifiers protected by its declaration.
fn validate_direct_wire_pair(
    pair: &DirectWirePair,
    facts: &SchematicFacts,
    component_ids: &BTreeMap<String, usize>,
) -> (Vec<Diagnostic>, BTreeSet<usize>) {
    let mut diagnostics = Vec::new();
    let mut protected_components = BTreeSet::new();
    let pair_name = format!("{} / {}", pair.first, pair.second);
    let (Some((first_reference, first_number)), Some((second_reference, second_number))) =
        (pair.first.rsplit_once('.'), pair.second.rsplit_once('.'))
    else {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch004,
            Severity::Error,
            format!("direct-wire pair {pair_name} must use REF.PIN notation"),
        ));
        return (diagnostics, protected_components);
    };
    if !valid_pin_ref(&pair.first) || !valid_pin_ref(&pair.second) || pair.first == pair.second {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch004,
            Severity::Error,
            format!("direct-wire pair {pair_name} must identify two distinct valid pins"),
        ));
        return (diagnostics, protected_components);
    }

    let first_pin = facts
        .pins
        .iter()
        .find(|pin| pin.reference == first_reference && pin.number == first_number);
    let second_pin = facts
        .pins
        .iter()
        .find(|pin| pin.reference == second_reference && pin.number == second_number);
    let (Some(first_pin), Some(second_pin)) = (first_pin, second_pin) else {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch004,
            Severity::Error,
            format!(
                "direct-wire pair {pair_name} references a missing or unsupported embedded pin"
            ),
        ));
        return (diagnostics, protected_components);
    };

    let first_point = first_pin.position_mm;
    let second_point = second_pin.position_mm;
    let first_attached = point_is_on_wire(first_point, facts);
    let second_attached = point_is_on_wire(second_point, facts);
    if !first_attached {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch004,
            Severity::Error,
            format!(
                "pin {first_reference}.{first_number} is not attached to a physical wire segment"
            ),
        ));
    }
    if !second_attached {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch004,
            Severity::Error,
            format!(
                "pin {second_reference}.{second_number} is not attached to a physical wire segment"
            ),
        ));
    }
    if first_attached && second_attached {
        let first_key = format!(
            "{:.*}:{:.*}",
            COORDINATE_KEY_DECIMAL_PLACES,
            first_point.0,
            COORDINATE_KEY_DECIMAL_PLACES,
            first_point.1
        );
        let second_key = format!(
            "{:.*}:{:.*}",
            COORDINATE_KEY_DECIMAL_PLACES,
            second_point.0,
            COORDINATE_KEY_DECIMAL_PLACES,
            second_point.1
        );
        let first_component = component_ids.get(&first_key).copied();
        let second_component = component_ids.get(&second_key).copied();
        if first_component != second_component {
            diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch004,
                Severity::Error,
                format!(
                    "{} and {} must share one physical wire-only network; labels do not establish this connection",
                    pair.first, pair.second
                ),
            ));
        }
        protected_components.extend(first_component);
        protected_components.extend(second_component);
    }

    append_direct_wire_pair_label_diagnostics(pair, facts, &mut diagnostics);
    (diagnostics, protected_components)
}

/// Adds diagnostics for non-global labels attached to either direct-wire pin anchor.
///
/// # Arguments
///
/// * `pair` — Direct-wire pair whose endpoint anchors are being checked.
/// * `facts` — Schematic facts containing the resolved label positions.
/// * `diagnostics` — Diagnostic collection to extend with any invalid label overlaps.
fn append_direct_wire_pair_label_diagnostics(
    pair: &DirectWirePair,
    facts: &SchematicFacts,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let endpoints = [
        (pair.first.as_str(), pair.first.rsplit_once('.')),
        (pair.second.as_str(), pair.second.rsplit_once('.')),
    ];
    for (pin_reference, parsed_reference) in endpoints {
        let Some((reference, number)) = parsed_reference else {
            continue;
        };
        let Some(pin) = facts
            .pins
            .iter()
            .find(|pin| pin.reference == reference && pin.number == number)
        else {
            continue;
        };
        for label in &facts.labels {
            if label.kind != "global_label" && same_wire_point(label.position_mm, pin.position_mm) {
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch004,
                    Severity::Error,
                    format!(
                        "direct-wire pin {pin_reference} shares its anchor with {} {:?}; use a continuous wire-only connection",
                        label.kind, label.name
                    ),
                ));
            }
        }
    }
}
/// Reports forbidden labels that appear on wire components protected by a direct-wire-pair declaration.
///
/// # Arguments
///
/// * `forbidden_labels` — Names that are not permitted on the selected physical wire components.
/// * `protected_components` — Wire-component identifiers that are covered by a direct-wire-pair declaration.
/// * `component_ids` — Map from tolerance-normalized schematic coordinates to physical wire-component identifiers.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
///
/// # Returns
///
/// Diagnostics for forbidden label kinds or names found on physical wire components protected by a direct-wire contract.
fn validate_forbidden_direct_wire_labels(
    forbidden_labels: &BTreeSet<String>,
    protected_components: &BTreeSet<usize>,
    component_ids: &BTreeMap<String, usize>,
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for label in facts.labels.iter().filter(|label| {
        if label.kind != "label" || !forbidden_labels.contains(&label.name) {
            return false;
        }
        let key = format!(
            "{:.*}:{:.*}",
            COORDINATE_KEY_DECIMAL_PLACES,
            label.position_mm.0,
            COORDINATE_KEY_DECIMAL_PLACES,
            label.position_mm.1
        );
        component_ids
            .get(&key)
            .is_some_and(|component| protected_components.contains(component))
    }) {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch004,
            Severity::Error,
            format!(
                "local label {:?} is forbidden on a direct-wire-only network",
                label.name
            ),
        ));
    }
    diagnostics
}
/// Validates common-net group declarations against physical wire topology and the optional exported netlist.
///
/// # Arguments
///
/// * `groups` — Map of profile group identifiers to their associated group-intent records.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `netlist` — Optional `KiCad`-exported flattened netlist; supplies logical pin-to-net assignments when present.
///
/// # Returns
///
/// Diagnostics for common-pin groups whose members fail physical-wire, label, or exported-netlist connectivity requirements.
pub(super) fn validate_common_pin_groups(
    groups: &[CommonPinNetGroup],
    facts: &SchematicFacts,
    netlist: Option<&Netlist>,
) -> Vec<Diagnostic> {
    if groups.is_empty() {
        return Vec::new();
    }
    let component_ids = physical_wire_component_ids(facts);
    let mut diagnostics = Vec::new();
    for group in groups {
        diagnostics.extend(validate_common_pin_group(group, facts, &component_ids));
        diagnostics.extend(validate_common_pin_group_netlist(group, netlist));
    }
    diagnostics
}
/// Checks that every pin in one common-net group resolves to the same physical wire component and has the required label.
///
/// # Arguments
///
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `component_ids` — Map from tolerance-normalized schematic coordinates to physical wire-component identifiers.
///
/// # Returns
///
/// Diagnostics for missing group pins, disconnected wire components, or an absent/mismatched label on the common physical net.
fn validate_common_pin_group(
    group: &CommonPinNetGroup,
    facts: &SchematicFacts,
    component_ids: &BTreeMap<String, usize>,
) -> Vec<Diagnostic> {
    if let Some(invalid) = common_pin_group_configuration_error(group) {
        return vec![invalid];
    }
    let (roots, mut diagnostics) = resolve_common_pin_group_components(group, facts, component_ids);
    if group.wire_connected && roots.len() > 1 {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch004,
            Severity::Error,
            format!(
                "configured pins {:?} must share one physical {} wire network",
                group.pins, group.name
            ),
        ));
    }
    if group.single_label {
        let matching_labels = common_pin_group_label_count(group, &roots, facts, component_ids);
        if matching_labels != 1 {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch004,
                Severity::Error,
                format!(
                    "configured pins {:?} must share exactly one {} label; found {matching_labels}",
                    group.pins, group.name
                ),
            ));
        }
    }
    diagnostics
}
/// Checks the exported net assignments for a common-pin group when a netlist is available.
///
/// # Arguments
///
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `netlist` — Optional `KiCad`-exported flattened netlist; supplies logical pin-to-net assignments when present.
///
/// # Returns
///
/// Diagnostics when the exported netlist assigns group pins to different logical nets or to a net name other than the declared name.
fn validate_common_pin_group_netlist(
    group: &CommonPinNetGroup,
    netlist: Option<&Netlist>,
) -> Vec<Diagnostic> {
    if !group.netlist_connected {
        return Vec::new();
    }
    let Some(netlist) = netlist else {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch004,
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
                BuiltinRuleId::Sch004,
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
            BuiltinRuleId::Sch004,
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
/// Validates the physical membership and label constraints of each declared wire group.
///
/// # Arguments
///
/// * `groups` — Map of profile group identifiers to their associated group-intent records.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
///
/// # Returns
///
/// Diagnostics for physical-wire groups whose declared pin members do not share the required wire-only component.
pub(super) fn validate_physical_wire_groups(
    groups: &[PhysicalWireGroup],
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    if groups.is_empty() {
        return Vec::new();
    }
    let component_ids = physical_wire_component_ids(facts);
    groups
        .iter()
        .flat_map(|group| validate_physical_wire_group(group, facts, &component_ids))
        .collect()
}
/// Checks that the declared pins of one physical-wire group resolve to the required connected wire component.
///
/// # Arguments
///
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `component_ids` — Map from tolerance-normalized schematic coordinates to physical wire-component identifiers.
///
/// # Returns
///
/// Diagnostics for pins that are missing, unattached, or split across more than one physical wire component.
fn validate_physical_wire_group(
    group: &PhysicalWireGroup,
    facts: &SchematicFacts,
    component_ids: &BTreeMap<String, usize>,
) -> Vec<Diagnostic> {
    let unique_pins: BTreeSet<_> = group.pins.iter().collect();
    if group.name.trim().is_empty() {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch004,
            Severity::Error,
            "physical wire group must have a non-empty name",
        )];
    }
    if group.pins.len() < MINIMUM_PIN_GROUP_SIZE || unique_pins.len() != group.pins.len() {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch004,
            Severity::Error,
            format!(
                "physical wire group {:?} must list at least two distinct REF.PIN entries",
                group.name
            ),
        )];
    }

    let mut roots = BTreeSet::new();
    let mut diagnostics = Vec::new();
    for pin_ref in &group.pins {
        let Some((reference, pin_number)) = pin_ref.rsplit_once('.') else {
            diagnostics.push(invalid_physical_wire_reference(&group.name, pin_ref));
            continue;
        };
        if !valid_pin_ref(pin_ref) {
            diagnostics.push(invalid_physical_wire_reference(&group.name, pin_ref));
            continue;
        }
        let Some(pin) = facts
            .pins
            .iter()
            .find(|pin| pin.reference == reference && pin.number == pin_number)
        else {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch004,
                Severity::Error,
                format!(
                    "physical wire group {:?} references missing or unsupported pin {pin_ref}",
                    group.name
                ),
            ));
            continue;
        };
        if !point_is_on_wire(pin.position_mm, facts) {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch004,
                Severity::Error,
                format!(
                    "{pin_ref} in physical wire group {:?} is not attached to a physical wire",
                    group.name
                ),
            ));
            continue;
        }
        let key = format!(
            "{:.*}:{:.*}",
            COORDINATE_KEY_DECIMAL_PLACES,
            pin.position_mm.0,
            COORDINATE_KEY_DECIMAL_PLACES,
            pin.position_mm.1
        );
        if let Some(component) = component_ids.get(&key).copied() {
            roots.insert(component);
        } else {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch004,
                Severity::Error,
                format!(
                    "{pin_ref} in physical wire group {:?} has no resolved physical wire component",
                    group.name
                ),
            ));
        }
    }

    if roots.len() > 1 {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch004,
            Severity::Error,
            format!(
                "pins {:?} in physical wire group {:?} must share one physical wire-only network; matching labels do not establish this connection",
                group.pins, group.name
            ),
        ));
    }
    diagnostics
}
/// Builds an SCH004 error for a physical-wire group member that is not a valid REF.PIN identifier.
///
/// # Arguments
///
/// * `group_name` — Human-readable group name used in the diagnostic message.
/// * `pin_ref` — A single pin identifier in REF.PIN form.
///
/// # Returns
///
/// An SCH004 error diagnostic identifying the malformed pin reference.
fn invalid_physical_wire_reference(group_name: &str, pin_ref: &str) -> Diagnostic {
    Diagnostic::for_builtin_rule(
        BuiltinRuleId::Sch004,
        Severity::Error,
        format!("physical wire group {group_name:?} contains invalid pin reference {pin_ref:?}"),
    )
}
/// Checks that each declared pair of pins belongs to separate physical and logical nets.
///
/// # Arguments
///
/// * `pairs` — Declared physical-wire pin pairs, each expressed as two REF.PIN references.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `netlist` — Optional `KiCad`-exported flattened netlist; supplies logical pin-to-net assignments when present.
///
/// # Returns
///
/// Diagnostics for separate-wire pairs that share a physical wire component or an exported logical net.
pub(super) fn validate_separate_wire_pairs(
    pairs: &[SeparateWirePair],
    facts: &SchematicFacts,
    netlist: Option<&Netlist>,
) -> Vec<Diagnostic> {
    if pairs.is_empty() {
        return Vec::new();
    }
    let component_ids = physical_wire_component_ids(facts);
    let mut diagnostics = Vec::new();
    for pair in pairs {
        diagnostics.extend(validate_separate_wire_pair(
            pair,
            facts,
            &component_ids,
            netlist,
        ));
    }
    diagnostics
}
/// Checks that the two pins in a separate-wire contract do not share a physical wire component or an exported logical net.
///
/// # Arguments
///
/// * `pair` — The direct-wire or separate-wire pair contract being validated.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `component_ids` — Map from tolerance-normalized schematic coordinates to physical wire-component identifiers.
/// * `netlist` — Optional `KiCad`-exported flattened netlist; supplies logical pin-to-net assignments when present.
///
/// # Returns
///
/// Diagnostics when either pin is missing or the two pins share a physical wire component or logical net.
fn validate_separate_wire_pair(
    pair: &SeparateWirePair,
    facts: &SchematicFacts,
    component_ids: &BTreeMap<String, usize>,
    netlist: Option<&Netlist>,
) -> Vec<Diagnostic> {
    let pair_name = format!("{} / {}", pair.first, pair.second);
    let mut diagnostics = Vec::new();
    if !valid_pin_ref(&pair.first) || !valid_pin_ref(&pair.second) || pair.first == pair.second {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch004,
            Severity::Error,
            format!(
                "separate-wire pair {pair_name} must identify two distinct valid REF.PIN entries"
            ),
        )];
    }

    let (Some((first_reference, first_number)), Some((second_reference, second_number))) =
        (pair.first.rsplit_once('.'), pair.second.rsplit_once('.'))
    else {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch004,
            Severity::Error,
            format!("separate-wire pair {pair_name} must use REF.PIN notation"),
        )];
    };
    let first_pin = facts
        .pins
        .iter()
        .find(|pin| pin.reference == first_reference && pin.number == first_number);
    let second_pin = facts
        .pins
        .iter()
        .find(|pin| pin.reference == second_reference && pin.number == second_number);
    let (Some(first_pin), Some(second_pin)) = (first_pin, second_pin) else {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch004,
            Severity::Error,
            format!(
                "separate-wire pair {pair_name} references a missing or unsupported embedded pin"
            ),
        )];
    };

    let physical_component = |point: (f64, f64)| {
        if !point_is_on_wire(point, facts) {
            return None;
        }
        let key = format!(
            "{:.*}:{:.*}",
            COORDINATE_KEY_DECIMAL_PLACES, point.0, COORDINATE_KEY_DECIMAL_PLACES, point.1
        );
        component_ids.get(&key).copied()
    };
    let first_component = physical_component(first_pin.position_mm);
    let second_component = physical_component(second_pin.position_mm);
    if matches!((first_component, second_component), (Some(first), Some(second)) if first == second)
    {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch004,
            Severity::Error,
            format!(
                "pins {} and {} must remain on separate physical wire-only networks",
                pair.first, pair.second
            ),
        ));
    }

    if pair.netlist_separate {
        let Some(netlist) = netlist else {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch004,
                Severity::Warning,
                format!("separate-wire pair {pair_name} requests netlist_separate but no exported KiCad netlist was supplied"),
            ));
            return diagnostics;
        };
        let first_net = netlist.net_for_pin(first_reference, first_number);
        let second_net = netlist.net_for_pin(second_reference, second_number);
        match (first_net, second_net) {
            (Some(first), Some(second)) if first == second => {
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch004,
                    Severity::Error,
                    format!(
                        "pins {} and {} must resolve to different exported KiCad nets, but both resolve to {first:?}",
                        pair.first, pair.second
                    ),
                ));
            }
            (None, _) => diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch004,
                Severity::Error,
                format!("{} in separate-wire pair {pair_name} is absent from the exported KiCad netlist", pair.first),
            )),
            (_, None) => diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch004,
                Severity::Error,
                format!("{} in separate-wire pair {pair_name} is absent from the exported KiCad netlist", pair.second),
            )),
            (Some(_), Some(_)) => {}
        }
    }
    diagnostics
}
/// Returns a configuration error when a common-pin group has an empty name, too few pins, duplicate pins, or malformed pin references.
///
/// # Arguments
///
/// * `group` — The group-intent record containing its members and declared constraints.
///
/// # Returns
///
/// A configuration diagnostic when the group declaration is invalid, or `None` when the declaration is well formed.
fn common_pin_group_configuration_error(group: &CommonPinNetGroup) -> Option<Diagnostic> {
    let group_name = group.name.trim();
    let unique_pins: BTreeSet<_> = group.pins.iter().collect();
    let message = if group_name.is_empty() {
        Some("common pin group must define a non-empty expected label/net name".to_owned())
    } else if group.pins.len() < MINIMUM_PIN_GROUP_SIZE || unique_pins.len() != group.pins.len() {
        Some(format!(
            "common pin group {group_name:?} must list at least two distinct REF.PIN entries"
        ))
    } else {
        None
    };
    message.map(|message| {
        Diagnostic::for_builtin_rule(BuiltinRuleId::Sch004, Severity::Error, message)
    })
}
/// Resolves the physical wire components touched by a common-pin group and reports pins that cannot be mapped to the schematic.
///
/// # Arguments
///
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `component_ids` — Map from tolerance-normalized schematic coordinates to physical wire-component identifiers.
///
/// # Returns
///
/// The physical wire components resolved from the declared pins, together with diagnostics for pins that could not be resolved.
fn resolve_common_pin_group_components(
    group: &CommonPinNetGroup,
    facts: &SchematicFacts,
    component_ids: &BTreeMap<String, usize>,
) -> (BTreeSet<usize>, Vec<Diagnostic>) {
    let mut roots = BTreeSet::new();
    let mut diagnostics = Vec::new();
    for pin_ref in &group.pins {
        let Some((reference, pin_number)) = pin_ref.rsplit_once('.') else {
            diagnostics.push(invalid_common_pin_reference(&group.name, pin_ref));
            continue;
        };
        if !valid_pin_ref(pin_ref) {
            diagnostics.push(invalid_common_pin_reference(&group.name, pin_ref));
            continue;
        }
        let pin = facts
            .pins
            .iter()
            .find(|item| item.reference == reference && item.number == pin_number);
        let Some(pin) = pin else {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch004,
                Severity::Error,
                format!(
                    "common pin group {:?} references missing or unsupported pin {pin_ref}",
                    group.name
                ),
            ));
            continue;
        };
        if !point_is_on_wire(pin.position_mm, facts) {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch004,
                Severity::Error,
                format!(
                    "{pin_ref} in common pin group {:?} is not attached to a physical wire",
                    group.name
                ),
            ));
            continue;
        }
        let key = format!(
            "{:.*}:{:.*}",
            COORDINATE_KEY_DECIMAL_PLACES,
            pin.position_mm.0,
            COORDINATE_KEY_DECIMAL_PLACES,
            pin.position_mm.1
        );
        let Some(component) = component_ids.get(&key).copied() else {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch004,
                Severity::Error,
                format!(
                    "{pin_ref} in common pin group {:?} has no resolved physical wire component",
                    group.name
                ),
            ));
            continue;
        };
        roots.insert(component);
        if !component_has_label(component, group.name.trim(), facts, component_ids) {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch004,
                Severity::Error,
                format!("{pin_ref} must belong to common net {}", group.name),
            ));
        }
    }
    (roots, diagnostics)
}
/// Builds an SCH004 error for a common-pin group member that is not a valid REF.PIN identifier.
///
/// # Arguments
///
/// * `group_name` — Human-readable group name used in the diagnostic message.
/// * `pin_ref` — A single pin identifier in REF.PIN form.
///
/// # Returns
///
/// An SCH004 error diagnostic identifying the malformed pin reference.
fn invalid_common_pin_reference(group_name: &str, pin_ref: &str) -> Diagnostic {
    Diagnostic::for_builtin_rule(
        BuiltinRuleId::Sch004,
        Severity::Error,
        format!("common pin group {group_name:?} contains invalid pin reference {pin_ref:?}"),
    )
}
/// Counts how many declared pins belong to a physical component carrying the requested common-net label.
///
/// # Arguments
///
/// * `group` — The group-intent record containing its members and declared constraints.
/// * `roots` — Physical wire-component identifiers already resolved for the group's declared pins.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `component_ids` — Map from tolerance-normalized schematic coordinates to physical wire-component identifiers.
///
/// # Returns
///
/// The number of declared pins that resolve to a component carrying the expected label.
fn common_pin_group_label_count(
    group: &CommonPinNetGroup,
    roots: &BTreeSet<usize>,
    facts: &SchematicFacts,
    component_ids: &BTreeMap<String, usize>,
) -> usize {
    facts
        .labels
        .iter()
        .filter(|label| {
            if label.name != group.name.trim() {
                return false;
            }
            let key = format!(
                "{:.*}:{:.*}",
                COORDINATE_KEY_DECIMAL_PLACES,
                label.position_mm.0,
                COORDINATE_KEY_DECIMAL_PLACES,
                label.position_mm.1
            );
            component_ids
                .get(&key)
                .is_some_and(|component| roots.contains(component))
        })
        .count()
}
/// Checks whether a physical wire component carries a label with the requested exact name.
///
/// # Arguments
///
/// * `component` — Physical wire-component identifier to inspect.
/// * `expected_name` — Exact label text expected on the selected wire component.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
/// * `component_ids` — Map from tolerance-normalized schematic coordinates to physical wire-component identifiers.
///
/// # Returns
///
/// `true` when the physical component carries a label whose text exactly matches `expected_name`.
fn component_has_label(
    component: usize,
    expected_name: &str,
    facts: &SchematicFacts,
    component_ids: &BTreeMap<String, usize>,
) -> bool {
    facts.labels.iter().any(|label| {
        if label.name != expected_name {
            return false;
        }
        let key = format!(
            "{:.*}:{:.*}",
            COORDINATE_KEY_DECIMAL_PLACES,
            label.position_mm.0,
            COORDINATE_KEY_DECIMAL_PLACES,
            label.position_mm.1
        );
        component_ids.get(&key) == Some(&component)
    })
}
/// Checks whether a schematic coordinate lies on any parsed wire segment within the geometry tolerance.
///
/// # Arguments
///
/// * `point` — Schematic coordinate to test, in millimetres.
/// * `facts` — Normalized schematic facts, including placed pins, wire segments, labels, frames, fields, and graphics.
///
/// # Returns
///
/// `true` when the coordinate lies on a wire segment within the schematic geometry tolerance.
pub(super) fn point_is_on_wire(point: (f64, f64), facts: &SchematicFacts) -> bool {
    facts.wires.iter().any(|wire| {
        let horizontal = (wire.start_mm.1 - wire.end_mm.1).abs() <= GEOMETRY_TOLERANCE_MM;
        if horizontal {
            (point.1 - wire.start_mm.1).abs() <= GEOMETRY_TOLERANCE_MM
                && point.0 >= wire.start_mm.0.min(wire.end_mm.0) - GEOMETRY_TOLERANCE_MM
                && point.0 <= wire.start_mm.0.max(wire.end_mm.0) + GEOMETRY_TOLERANCE_MM
        } else if (wire.start_mm.0 - wire.end_mm.0).abs() <= GEOMETRY_TOLERANCE_MM {
            (point.0 - wire.start_mm.0).abs() <= GEOMETRY_TOLERANCE_MM
                && point.1 >= wire.start_mm.1.min(wire.end_mm.1) - GEOMETRY_TOLERANCE_MM
                && point.1 <= wire.start_mm.1.max(wire.end_mm.1) + GEOMETRY_TOLERANCE_MM
        } else {
            false
        }
    })
}
/// Compares two schematic coordinates using the shared geometry tolerance.
///
/// # Arguments
///
/// * `first` — First schematic point or pin endpoint in the comparison.
/// * `second` — Second schematic point or pin endpoint in the comparison.
///
/// # Returns
///
/// `true` when both coordinates match within the schematic geometry tolerance.
fn same_wire_point(first: (f64, f64), second: (f64, f64)) -> bool {
    (first.0 - second.0).abs() <= GEOMETRY_TOLERANCE_MM
        && (first.1 - second.1).abs() <= GEOMETRY_TOLERANCE_MM
}

/// Concrete implementation of built-in rule SCH004.
pub(super) struct Sch004;
impl<'a> crate::Rule<super::ProfileRuleContext<'a>> for Sch004 {
    /// Returns the stable SCH004 identifier.
    ///
    /// # Returns
    ///
    /// The stable SCH004 identifier used to associate diagnostics with this rule.
    fn id(&self) -> BuiltinRuleId {
        BuiltinRuleId::Sch004
    }
    /// Evaluates every declared physical connectivity and separation contract.
    ///
    /// # Arguments
    ///
    /// * `context` — Project profile, normalized physical wire facts, and optional exported netlist.
    ///
    /// # Returns
    ///
    /// Diagnostics for direct-wire, common-pin, physical-wire-group, and separate-wire-pair violations.
    fn check(&self, context: &super::ProfileRuleContext<'a>) -> Vec<Diagnostic> {
        let profile = context.profile;
        let facts = context.facts;
        let mut diagnostics = validate_direct_wire_connectivity(
            &profile.direct_wire_pairs,
            &profile.forbidden_direct_wire_labels,
            facts,
        );
        diagnostics.extend(validate_common_pin_groups(
            &profile.common_pin_groups,
            facts,
            context.netlist,
        ));
        diagnostics.extend(validate_physical_wire_groups(
            &profile.physical_wire_groups,
            facts,
        ));
        diagnostics.extend(validate_separate_wire_pairs(
            &profile.separate_wire_pairs,
            facts,
            context.netlist,
        ));
        diagnostics
    }
}
