//! PCB facts and profile-driven board checks that complement native KiCad DRC.

use kicad_clippy_core::{BuiltinRuleId, Diagnostic, Severity};
use kicad_clippy_erc::Netlist;
use kicad_clippy_parser::BoardDocument;
use kiutils_sexpr::{Atom, Node};

use crate::{ComponentKind, DecouplingIntent, LintProfile};

/// Millimetre tolerance used when comparing PCB pad-center distances.
const PCB_DISTANCE_TOLERANCE_MM: f64 = 0.001;

/// A placed PCB pad with its board-space anchor and connectivity label.
#[derive(Clone, Debug)]
struct BoardPadFact {
    /// Footprint reference designator that owns this pad.
    reference: String,
    /// Serialized footprint pad number.
    number: String,
    /// Board net name assigned to the pad, or None when it is unconnected/unresolved.
    net_name: Option<String>,
    /// Pad anchor transformed into board coordinates, in millimetres.
    position_mm: (f64, f64),
}

/// Normalized board facts currently required by PCB001.
#[derive(Clone, Debug, Default)]
struct BoardFacts {
    /// Placed footprint pads with transformed board-space anchors.
    pads: Vec<BoardPadFact>,
}

impl BoardFacts {
    /// Extracts placed footprint pad anchors from a parsed PCB.
    ///
    /// # Arguments
    ///
    /// * `document` — Lossless PCB document whose footprint and pad expressions are normalized into board facts.
    ///
    /// # Returns
    ///
    /// Board-space pad facts for footprints placed on F.Cu or B.Cu.
    fn from_document(document: &BoardDocument) -> Self {
        let Some(root) = document.cst().nodes.first() else {
            return Self::default();
        };
        let mut pads = Vec::new();
        for footprint in children(root, "footprint") {
            let Some(reference) = footprint_reference(footprint) else {
                continue;
            };
            let Some((origin_x, origin_y, footprint_angle)) = coordinate_at(footprint) else {
                continue;
            };
            let layer = child_list(footprint, "layer")
                .and_then(list_items)
                .and_then(|items| items.get(1))
                .and_then(atom_text);
            if !matches!(layer, Some("F.Cu" | "B.Cu")) {
                continue;
            }
            let (scale_x, scale_y) = footprint_scale(footprint).unwrap_or((1.0, 1.0));
            if !scale_x.is_finite() || !scale_y.is_finite() || scale_x <= 0.0 || scale_y <= 0.0 {
                continue;
            }
            // KiCad serializes the flipped pad-local coordinates and the adjusted footprint angle.
            // Apply the stored transform as-is; mirroring or negating it again would double-flip B.Cu pads.
            let radians = footprint_angle.to_radians();
            let (sin_angle, cos_angle) = radians.sin_cos();
            for pad in children(footprint, "pad") {
                let Some(number) = list_items(pad)
                    .and_then(|items| items.get(1))
                    .and_then(atom_text)
                    .filter(|number| !number.trim().is_empty())
                else {
                    continue;
                };
                let Some((mut local_x, mut local_y, _pad_angle)) = coordinate_at(pad) else {
                    continue;
                };
                local_x *= scale_x;
                local_y *= scale_y;
                // The footprint transform applies scale, then its serialized rotation, then translation.
                let rotated_x = local_x.mul_add(cos_angle, local_y * sin_angle);
                let rotated_y = local_y.mul_add(cos_angle, -local_x * sin_angle);
                let net_name = child_list(pad, "net")
                    .and_then(list_items)
                    .and_then(|items| items.get(2))
                    .and_then(atom_text)
                    .map(str::to_owned)
                    .filter(|name| !name.trim().is_empty());
                pads.push(BoardPadFact {
                    reference: reference.clone(),
                    number: number.to_owned(),
                    net_name,
                    position_mm: (origin_x + rotated_x, origin_y + rotated_y),
                });
            }
        }
        Self { pads }
    }
}

/// Parses a footprint reference from its modern property or legacy `fp_text` node.
///
/// # Arguments
///
/// * `footprint` — Placed footprint expression whose reference property or legacy reference text is inspected.
///
/// # Returns
///
/// The non-empty footprint reference, or `None` when it is missing.
fn footprint_reference(footprint: &Node) -> Option<String> {
    for property in children(footprint, "property") {
        let items = list_items(property)?;
        if items.get(1).and_then(atom_text) == Some("Reference") {
            return items
                .get(2)
                .and_then(atom_text)
                .filter(|reference| !reference.trim().is_empty())
                .map(str::to_owned);
        }
    }
    for field in children(footprint, "fp_text") {
        let items = list_items(field)?;
        if items.get(1).and_then(atom_text) == Some("reference") {
            return items
                .get(2)
                .and_then(atom_text)
                .filter(|reference| !reference.trim().is_empty())
                .map(str::to_owned);
        }
    }
    None
}

/// Resolves a serialized X/Y coordinate and optional angle from an `at` child.
///
/// # Arguments
///
/// * `node` — Footprint or pad expression whose serialized `at` coordinates provide its local position and angle.
///
/// # Returns
///
/// Finite X/Y coordinates and angle in degrees; omitted angles default to zero.
fn coordinate_at(node: &Node) -> Option<(f64, f64, f64)> {
    let items = child_list(node, "at").and_then(list_items)?;
    let x = items.get(1).and_then(number)?;
    let y = items.get(2).and_then(number)?;
    let angle = items.get(3).and_then(number).unwrap_or(0.0);
    (x.is_finite() && y.is_finite() && angle.is_finite()).then_some((x, y, angle))
}

/// Reads an optional positive footprint scale from its serialized transform.
///
/// # Arguments
///
/// * `footprint` — Placed footprint expression whose optional non-uniform scale pair is being read.
///
/// # Returns
///
/// The X/Y scale pair, or `None` when no explicit scale is serialized.
fn footprint_scale(footprint: &Node) -> Option<(f64, f64)> {
    let items = child_list(footprint, "scale").and_then(list_items)?;
    Some((
        items.get(1).and_then(number)?,
        items.get(2).and_then(number)?,
    ))
}

/// Resolves a unique board pad by footprint reference and pad number.
///
/// # Arguments
///
/// * `facts` — Normalized board pad facts extracted from each placed footprint in the PCB.
/// * `reference` — Reference designator identifying the footprint that should own the requested pad.
/// * `number` — Serialized pad number to resolve within that specific footprint.
///
/// # Returns
///
/// The unique matching pad, or `None` for an absent or ambiguous match.
fn unique_pad<'a>(
    facts: &'a BoardFacts,
    reference: &str,
    number: &str,
) -> Option<&'a BoardPadFact> {
    let mut matches = facts
        .pads
        .iter()
        .filter(|pad| pad.reference == reference && pad.number == number);
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

/// Resolved board-pad context shared by the independent PCB001 checks.
struct ResolvedDecouplingPads<'a> {
    /// Reference designator of the target IC.
    target_reference: &'a str,
    /// Serialized pin number of the IC supply pad.
    target_number: &'a str,
    /// Resolved board pad belonging to the target IC supply pin.
    target_pad: &'a BoardPadFact,
    /// Resolved board pad belonging to the capacitor target terminal.
    capacitor_pad: &'a BoardPadFact,
    /// Optional resolved board pad belonging to the capacitor return terminal.
    return_pad: Option<&'a BoardPadFact>,
}

/// Runs PCB001 for each capacitor whose profile declares a maximum target pad-center distance.
///
/// # Arguments
///
/// * `profile` — Component-intent metadata containing optional decoupling pad-distance thresholds.
/// * `board` — Parsed PCB with transformed footprint pad anchors and board net names.
/// * `netlist` — Optional exported schematic netlist used to cross-check target and return pad nets.
///
/// # Returns
///
/// PCB001 errors for excessive or inconsistent pad distances, and warnings when board facts are incomplete.
pub fn validate_decoupling_pad_distances(
    profile: &LintProfile,
    board: &BoardDocument,
    netlist: Option<&Netlist>,
) -> Vec<Diagnostic> {
    let facts = BoardFacts::from_document(board);
    profile
        .components
        .iter()
        .filter_map(|(reference, intent)| {
            if intent.kind != ComponentKind::Capacitor || intent.role.0 != "decoupling" {
                return None;
            }
            let contract = intent.decoupling.as_ref()?;
            let maximum_distance = contract.max_target_pad_distance_mm?;
            Some(validate_one_decoupling_contract(
                reference,
                contract,
                maximum_distance,
                &facts,
                netlist,
            ))
        })
        .flatten()
        .collect()
}

/// Evaluates the board evidence for one profile-declared decoupling capacitor.
///
/// # Arguments
///
/// * `reference` — Reference designator of the capacitor whose contract is evaluated.
/// * `contract` — Decoupling target, target/return terminal, and expected return-net metadata.
/// * `maximum_distance` — Maximum allowed target-pad to capacitor-target-pad center distance, in millimetres.
/// * `facts` — Normalized placed-board pad facts extracted from the supplied PCB.
/// * `netlist` — Optional exported schematic netlist used to compare target and return connectivity.
///
/// # Returns
///
/// PCB001 diagnostics for missing/ambiguous pads, net mismatches, and distances above the declared maximum.
fn validate_one_decoupling_contract(
    reference: &str,
    contract: &DecouplingIntent,
    maximum_distance: f64,
    facts: &BoardFacts,
    netlist: Option<&Netlist>,
) -> Vec<Diagnostic> {
    if !maximum_distance.is_finite() || maximum_distance < 0.0 {
        return Vec::new();
    }
    let Some((target_reference, target_number)) = contract.target_pin.rsplit_once('.') else {
        return Vec::new();
    };
    let (Some(target_pad), Some(capacitor_pad)) = (
        unique_pad(facts, target_reference, target_number),
        unique_pad(facts, reference, &contract.target_terminal),
    ) else {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Pcb001,
            Severity::Warning,
            format!(
                "{reference} PCB001 pad distance is unverified because board pad {} or {}.{} is absent or ambiguous",
                contract.target_pin, reference, contract.target_terminal
            ),
        )];
    };

    let return_pad = unique_pad(facts, reference, &contract.return_terminal);
    let resolved_pads = ResolvedDecouplingPads {
        target_reference,
        target_number,
        target_pad,
        capacitor_pad,
        return_pad,
    };
    let mut diagnostics = validate_board_pad_nets(reference, contract, &resolved_pads);
    diagnostics.extend(validate_schematic_board_net_parity(
        reference,
        contract,
        &resolved_pads,
        netlist,
    ));
    diagnostics.extend(validate_target_pad_distance(
        reference,
        contract,
        maximum_distance,
        &resolved_pads,
    ));
    diagnostics
}

/// Checks the board-side net assignments required by the decoupling contract.
///
/// # Arguments
///
/// * `reference` — Reference designator of the decoupling capacitor whose pads are checked.
/// * `contract` — Declared capacitor target and return terminals plus the expected return net.
/// * `pads` — Resolved target IC pad, capacitor target pad, and optional capacitor return pad.
///
/// # Returns
///
/// PCB001 errors for board-net mismatches and warnings when required pad-net evidence is unavailable.
fn validate_board_pad_nets(
    reference: &str,
    contract: &DecouplingIntent,
    pads: &ResolvedDecouplingPads<'_>,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    match pads.return_pad.and_then(|pad| pad.net_name.as_deref()) {
        Some(actual_return_net) if actual_return_net != contract.return_net => {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Pcb001,
                Severity::Error,
                format!(
                    "{reference} capacitor return pad {} is on board net {actual_return_net:?}, not the declared return net {:?}",
                    contract.return_terminal, contract.return_net
                ),
            ));
        }
        None => diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Pcb001,
            Severity::Warning,
            format!(
                "{reference} PCB001 return-pad net is unverified because capacitor pad {} is absent, ambiguous, or has no assigned board net",
                contract.return_terminal
            ),
        )),
        _ => {}
    }
    match (&pads.target_pad.net_name, &pads.capacitor_pad.net_name) {
        (Some(target_net), Some(capacitor_net)) if target_net != capacitor_net => {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Pcb001,
                Severity::Error,
                format!(
                    "{reference} decoupling pad {} is on board net {capacitor_net:?}, but {} is on {target_net:?}",
                    contract.target_terminal, contract.target_pin
                ),
            ));
        }
        (None, _) | (_, None) => diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Pcb001,
            Severity::Warning,
            format!(
                "{reference} PCB001 pad net consistency is unverified because {} or {}.{} has no assigned board net",
                contract.target_pin, reference, contract.target_terminal
            ),
        )),
        _ => {}
    }
    diagnostics
}


/// Checks schematic and board evidence for the capacitor's declared return terminal.
///
/// # Arguments
///
/// * `reference` — Reference designator of the capacitor whose return terminal is checked.
/// * `contract` — Decoupling metadata containing the return terminal and expected return net.
/// * `pads` — Resolved board-pad facts, including the optional capacitor return pad.
/// * `netlist` — Exported schematic netlist used to resolve the return terminal's expected net.
///
/// # Returns
///
/// PCB001 errors for confirmed return-net mismatches and a warning when the netlist mapping is absent.
fn validate_return_pad_net_parity(
    reference: &str,
    contract: &DecouplingIntent,
    pads: &ResolvedDecouplingPads<'_>,
    netlist: &Netlist,
) -> Vec<Diagnostic> {
    let Some(expected_return_net) = netlist.net_for_pin(reference, &contract.return_terminal) else {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Pcb001,
            Severity::Warning,
            format!(
                "{reference} PCB001 schematic-to-board return mapping is unverified because the exported netlist lacks {}.{}",
                reference, contract.return_terminal
            ),
        )];
    };
    let mut diagnostics = Vec::new();
    if expected_return_net != contract.return_net {
        diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Pcb001,
            Severity::Error,
            format!(
                "{reference} schematic netlist assigns return pad {} to {expected_return_net:?}, not the declared return net {:?}",
                contract.return_terminal, contract.return_net
            ),
        ));
    }
    if let Some(actual_return_net) = pads.return_pad.and_then(|pad| pad.net_name.as_deref()) {
        if actual_return_net != expected_return_net {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Pcb001,
                Severity::Error,
                format!(
                    "{reference} board return pad {} is on {actual_return_net:?}, but the schematic netlist assigns {expected_return_net:?}",
                    contract.return_terminal
                ),
            ));
        }
    }
    diagnostics
}

/// Compares board pad nets with the optional exported schematic netlist.
///
/// # Arguments
///
/// * `reference` — Reference designator of the decoupling capacitor.
/// * `contract` — Target/return pin declarations and expected return net.
/// * `pads` — Resolved target-pin identity and board pad facts for capacitor target and return terminals.
/// * `netlist` — Optional exported schematic netlist with logical pin-to-net assignments.
///
/// # Returns
///
/// PCB001 netlist-parity errors or warnings when schematic connectivity evidence is unavailable.
fn validate_schematic_board_net_parity(
    reference: &str,
    contract: &DecouplingIntent,
    pads: &ResolvedDecouplingPads<'_>,
    netlist: Option<&Netlist>,
) -> Vec<Diagnostic> {
    let Some(netlist) = netlist else {
        return vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Pcb001,
            Severity::Warning,
            format!(
                "{reference} PCB001 board pad distance is evaluated, but schematic net parity is unverified because no exported netlist was supplied"
            ),
        )];
    };

    let mut diagnostics = validate_return_pad_net_parity(reference, contract, pads, netlist);
    let target_net = netlist.net_for_pin(pads.target_reference, pads.target_number);
    let capacitor_net = netlist.net_for_pin(reference, &contract.target_terminal);
    match (target_net, capacitor_net) {
        (Some(expected_target), Some(expected_capacitor))
            if expected_target != expected_capacitor =>
        {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Pcb001,
                Severity::Error,
                format!(
                    "{reference} schematic netlist does not place capacitor terminal {} on the same net as {}",
                    contract.target_terminal, contract.target_pin
                ),
            ));
        }
        (Some(expected_target), _)
            if pads
                .target_pad
                .net_name
                .as_deref()
                .is_some_and(|actual| actual != expected_target) =>
        {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Pcb001,
                Severity::Error,
                format!(
                    "{reference} board pad net {:?} for {} differs from schematic netlist net {expected_target:?}",
                    pads.target_pad.net_name, contract.target_pin
                ),
            ));
        }
        (Some(_), Some(expected_capacitor))
            if pads
                .capacitor_pad
                .net_name
                .as_deref()
                .is_some_and(|actual| actual != expected_capacitor) =>
        {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Pcb001,
                Severity::Error,
                format!(
                    "{reference} capacitor board pad net {:?} differs from schematic netlist net {expected_capacitor:?}",
                    pads.capacitor_pad.net_name
                ),
            ));
        }
        (None, _) | (_, None) => diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Pcb001,
            Severity::Warning,
            format!(
                "{reference} PCB001 schematic-to-board pin mapping is unverified because the exported netlist lacks {} or {}.{}",
                contract.target_pin, reference, contract.target_terminal
            ),
        )),
        _ => {}
    }
    diagnostics
}

/// Checks the declared maximum center-to-center distance between target and capacitor pads.
///
/// # Arguments
///
/// * `reference` — Reference designator of the decoupling capacitor.
/// * `contract` — Decoupling contract identifying the target pin and capacitor target terminal.
/// * `maximum_distance` — Maximum allowed board-space pad-center distance, in millimetres.
/// * `pads` — Resolved target IC and capacitor target-terminal pad anchors on the board.
///
/// # Returns
///
/// A PCB001 error when the measured distance exceeds the configured maximum, or no diagnostics otherwise.
fn validate_target_pad_distance(
    reference: &str,
    contract: &DecouplingIntent,
    maximum_distance: f64,
    pads: &ResolvedDecouplingPads<'_>,
) -> Vec<Diagnostic> {
    let distance = (pads.target_pad.position_mm.0 - pads.capacitor_pad.position_mm.0)
        .hypot(pads.target_pad.position_mm.1 - pads.capacitor_pad.position_mm.1);
    if distance > maximum_distance + PCB_DISTANCE_TOLERANCE_MM {
        vec![Diagnostic::for_builtin_rule(
            BuiltinRuleId::Pcb001,
            Severity::Error,
            format!(
                "{reference} capacitor pad {} is {distance:.2} mm from target pad {}, exceeding max_target_pad_distance_mm of {maximum_distance:.2} mm",
                contract.target_terminal, contract.target_pin
            ),
        )]
    } else {
        Vec::new()
    }
}

/// Reports an unverified warning for each pad-distance contract when no board was supplied.
///
/// # Arguments
///
/// * `profile` — Component-intent declarations that include optional PCB001 pad-distance thresholds.
///
/// # Returns
///
/// PCB001 warnings stating that no board evidence was supplied for the declared check.
pub fn validate_missing_board(profile: &LintProfile) -> Vec<Diagnostic> {
    profile
        .components
        .iter()
        .filter_map(|(reference, intent)| {
            if intent.kind != ComponentKind::Capacitor || intent.role.0 != "decoupling" {
                return None;
            }
            let contract = intent.decoupling.as_ref()?;
            contract.max_target_pad_distance_mm?;
            Some(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Pcb001,
                Severity::Warning,
                format!(
                    "{reference} PCB001 pad-distance requirement is unverified because no .kicad_pcb board was supplied"
                ),
            ))
        })
        .collect()
}

/// Returns the direct list items of an S-expression list.
///
/// # Arguments
///
/// * `node` — S-expression node to inspect for a list value.
///
/// # Returns
///
/// The borrowed list-item slice when the node is a list, or `None` for an atom.
fn list_items(node: &Node) -> Option<&[Node]> {
    if let Node::List { items, .. } = node {
        Some(items)
    } else {
        None
    }
}

/// Returns a list node's symbolic head.
///
/// # Arguments
///
/// * `node` — S-expression node whose first item may identify the expression kind.
///
/// # Returns
///
/// The symbolic expression name when the node is a list with a symbolic first item.
fn head(node: &Node) -> Option<&str> {
    let Node::List { items, .. } = node else {
        return None;
    };
    match items.first()? {
        Node::Atom {
            atom: Atom::Symbol(value),
            ..
        } => Some(value),
        _ => None,
    }
}

/// Iterates over direct child lists with the requested head.
///
/// # Arguments
///
/// * `node` — Parent S-expression whose immediate list children are searched.
/// * `name` — Symbolic list-head name that identifies the child expressions to return.
///
/// # Returns
///
/// An iterator over matching direct child lists, preserving their source order.
fn children<'a>(node: &'a Node, name: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
    list_items(node)
        .into_iter()
        .flatten()
        .skip(1)
        .filter(move |child| head(child) == Some(name))
}

/// Returns the first direct child list with the requested head.
///
/// # Arguments
///
/// * `node` — Parent S-expression whose immediate child expressions are searched.
/// * `name` — Symbolic list-head name identifying the requested child expression.
///
/// # Returns
///
/// The first matching direct child list, or `None` when the parent has no such child.
fn child_list<'a>(node: &'a Node, name: &'a str) -> Option<&'a Node> {
    children(node, name).next()
}

/// Decodes a plain or quoted S-expression atom.
///
/// # Arguments
///
/// * `node` — Atom node containing a symbolic or quoted serialized value.
///
/// # Returns
///
/// The decoded atom text, or `None` when the supplied node is a list.
fn atom_text(node: &Node) -> Option<&str> {
    let Node::Atom { atom, .. } = node else {
        return None;
    };
    match atom {
        Atom::Symbol(value) | Atom::Quoted(value) => Some(value),
    }
}

/// Parses an atom as a finite floating-point value.
///
/// # Arguments
///
/// * `node` — Atom node expected to contain a decimal number in the PCB serialization.
///
/// # Returns
///
/// The finite floating-point value, or `None` when parsing fails or the value is not finite.
fn number(node: &Node) -> Option<f64> {
    let value = atom_text(node)?.parse::<f64>().ok()?;
    value.is_finite().then_some(value)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::BoardFacts;
    use kicad_clippy_parser::BoardDocument;

    /// Resolves front-side and back-side pad anchors after their footprint transforms.
    #[test]
    fn board_pad_anchors_apply_rotation_and_back_side_mirroring() {
        let board = BoardDocument::from_text(
            "pad-transform.kicad_pcb",
            r#"(kicad_pcb
                (footprint "Package:Part" (layer "F.Cu") (at 100 50 90)
                    (property "Reference" "U1")
                    (pad "1" smd rect (at 2 1) (size 1 1) (layers "F.Cu") (net 1 "VDD"))
                )
                (footprint "Package:Part" (layer "B.Cu") (at 100 50 90)
                    (property "Reference" "C1")
                    (pad "1" smd rect (at 2 -1) (size 1 1) (layers "B.Cu") (net 1 "VDD"))
                )
            )"#,
        )
        .expect("board fixture parses");
        let facts = BoardFacts::from_document(&board);
        let target = super::unique_pad(&facts, "U1", "1").expect("front pad found");
        let capacitor = super::unique_pad(&facts, "C1", "1").expect("back pad found");
        assert!((target.position_mm.0 - 101.0).abs() < 0.001);
        assert!((target.position_mm.1 - 48.0).abs() < 0.001);
        assert!((capacitor.position_mm.0 - 99.0).abs() < 0.001);
        assert!((capacitor.position_mm.1 - 48.0).abs() < 0.001);
    }
}

#[cfg(test)]
mod contract_tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::{validate_decoupling_pad_distances, validate_missing_board};
    use crate::LintProfile;
    use kicad_clippy_parser::BoardDocument;

    /// Builds an exported netlist containing only the supplied decoupling target and terminal mappings.
    ///
    /// # Arguments
    ///
    /// * `target_net` — Optional exported net assignment for IC pad U1.1.
    /// * `capacitor_net` — Optional exported net assignment for capacitor target pad C1.1.
    /// * `return_net` — Optional exported net assignment for capacitor return pad C1.2.
    ///
    /// # Returns
    ///
    /// A flattened netlist with exactly the pin-to-net assignments provided by the test.
    fn decoupling_netlist(
        target_net: Option<&str>,
        capacitor_net: Option<&str>,
        return_net: Option<&str>,
    ) -> kicad_clippy_erc::Netlist {
        let mut pin_nets = std::collections::BTreeMap::new();
        for (reference, pin, net) in [
            ("U1", "1", target_net),
            ("C1", "1", capacitor_net),
            ("C1", "2", return_net),
        ] {
            if let Some(net) = net {
                pin_nets.insert(
                    kicad_clippy_erc::PinRef {
                        reference: reference.to_owned(),
                        pin: pin.to_owned(),
                    },
                    net.to_owned(),
                );
            }
        }
        kicad_clippy_erc::Netlist {
            pin_nets,
            ..kicad_clippy_erc::Netlist::default()
        }
    }

    /// Builds a board whose capacitor return pad is absent while target-side pad nets remain available.
    ///
    /// # Returns
    ///
    /// A parsed PCB with U1.1 and C1.1 assigned to VDD but without C1.2 board geometry.
    fn board_without_return_pad() -> BoardDocument {
        BoardDocument::from_text(
            "missing-return-pad.kicad_pcb",
            r#"(kicad_pcb
                (footprint "Package:IC" (layer "F.Cu") (at 100 50 90)
                    (property "Reference" "U1")
                    (pad "1" smd rect (at 2 0) (size 1 1) (layers "F.Cu") (net 1 "VDD"))
                )
                (footprint "Package:Capacitor" (layer "F.Cu") (at 104 50 90)
                    (property "Reference" "C1")
                    (pad "1" smd rect (at 2 0) (size 1 1) (layers "F.Cu") (net 1 "VDD"))
                )
            )"#,
        )
        .expect("board fixture parses")
    }

    /// Does not treat an absent capacitor return pad as a confirmed netlist mismatch.
    #[test]
    fn pcb001_keeps_missing_return_pad_unverified_when_netlist_exists() {
        let diagnostics = validate_decoupling_pad_distances(
            &profile(10.0),
            &board_without_return_pad(),
            Some(&decoupling_netlist(Some("VDD"), Some("VDD"), Some("GND"))),
        );
        assert!(diagnostics.iter().any(|item| {
            item.code == "PCB001"
                && item.severity == kicad_clippy_core::Severity::Warning
                && item.message.contains("return-pad net is unverified")
        }));
        assert!(!diagnostics.iter().any(|item| {
            item.code == "PCB001" && item.message.contains("board return pad 2 is on")
        }));
    }

    /// Does not treat a missing board net assignment as a confirmed target-pad net mismatch.
    #[test]
    fn pcb001_keeps_missing_target_pad_net_unverified_when_netlist_exists() {
        let board = BoardDocument::from_text(
            "missing-target-net.kicad_pcb",
            r#"(kicad_pcb
                (footprint "Package:IC" (layer "F.Cu") (at 100 50 90)
                    (property "Reference" "U1")
                    (pad "1" smd rect (at 2 0) (size 1 1) (layers "F.Cu"))
                )
                (footprint "Package:Capacitor" (layer "F.Cu") (at 104 50 90)
                    (property "Reference" "C1")
                    (pad "1" smd rect (at 2 0) (size 1 1) (layers "F.Cu") (net 1 "VDD"))
                    (pad "2" smd rect (at 0 2) (size 1 1) (layers "F.Cu") (net 2 "GND"))
                )
            )"#,
        )
        .expect("board fixture parses");
        let diagnostics = validate_decoupling_pad_distances(
            &profile(10.0),
            &board,
            Some(&decoupling_netlist(Some("VDD"), Some("VDD"), Some("GND"))),
        );
        assert!(diagnostics.iter().any(|item| {
            item.code == "PCB001"
                && item.severity == kicad_clippy_core::Severity::Warning
                && item.message.contains("pad net consistency is unverified")
        }));
        assert!(!diagnostics.iter().any(|item| {
            item.code == "PCB001" && item.message.contains("differs from schematic netlist net")
        }));
    }

    /// Parses a profile containing a single declared decoupling pad-distance contract.
    ///
    /// # Arguments
    ///
    /// * `maximum_distance_mm` — Maximum allowed board-space pad-center distance.
    ///
    /// # Returns
    ///
    /// A version-one profile with U1.1 as the target pin and C1.1 as the capacitor terminal.
    fn profile(maximum_distance_mm: f64) -> LintProfile {
        let source = format!(
            r#"{{"schema_version":1,"components":{{"C1":{{
                "kind":"capacitor","role":"decoupling",
                "function":"Decouples U1 supply","rationale":"Local supply bypass",
                "decoupling":{{
                    "target_pin":"U1.1","target_terminal":"1","return_terminal":"2",
                    "return_net":"GND","cluster":"U1.VDD",
                    "max_target_pad_distance_mm":{maximum_distance_mm}
                }}
            }}}}}}"#
        );
        serde_json::from_str(&source).expect("profile fixture parses")
    }

    /// Builds a board fixture with rotated front-side footprints and one assigned pad net.
    ///
    /// # Arguments
    ///
    /// * `capacitor_x` — X-origin of capacitor C1 in board coordinates.
    /// * `capacitor_net` — Board-net name assigned to the capacitor target-terminal pad C1.1.
    /// * `return_net` — Board-net name assigned to capacitor return-terminal pad C1.2.
    ///
    /// # Returns
    ///
    /// A parsed PCB containing U1.1, C1.1, and C1.2 at their transformed pad anchors.
    fn board(capacitor_x: f64, capacitor_net: &str, return_net: &str) -> BoardDocument {
        let source = format!(
            r#"(kicad_pcb
                (footprint "Package:IC" (layer "F.Cu") (at 100 50 90)
                    (property "Reference" "U1")
                    (pad "1" smd rect (at 2 0) (size 1 1) (layers "F.Cu") (net 1 "VDD"))
                )
                (footprint "Package:Capacitor" (layer "F.Cu") (at {capacitor_x} 50 90)
                    (property "Reference" "C1")
                    (pad "1" smd rect (at 2 0) (size 1 1) (layers "F.Cu") (net 1 "{capacitor_net}"))
                    (pad "2" smd rect (at 0 2) (size 1 1) (layers "F.Cu") (net 2 "{return_net}"))
                )
            )"#
        );
        BoardDocument::from_text("decoupling.kicad_pcb", source).expect("board fixture parses")
    }

    /// Reports a target-pad distance that exceeds the project-declared PCB threshold.
    #[test]
    fn pcb001_rejects_excessive_pad_center_distance() {
        let diagnostics =
            validate_decoupling_pad_distances(&profile(3.0), &board(104.0, "VDD", "GND"), None);
        assert!(diagnostics.iter().any(|item| {
            item.code == "PCB001"
                && item.message.contains("4.00 mm")
                && item.message.contains("max_target_pad_distance_mm")
        }));
    }

    /// Accepts pads at the configured distance boundary within the geometry tolerance.
    #[test]
    fn pcb001_accepts_pad_distance_at_threshold() {
        let diagnostics =
            validate_decoupling_pad_distances(&profile(4.0), &board(104.0, "VDD", "GND"), None);
        assert!(!diagnostics.iter().any(|item| {
            item.code == "PCB001" && item.severity == kicad_clippy_core::Severity::Error
        }));
    }

    /// Rejects a capacitor target pad assigned to a different board net than the target IC pad.
    #[test]
    fn pcb001_rejects_mismatched_board_pad_nets() {
        let diagnostics =
            validate_decoupling_pad_distances(&profile(10.0), &board(104.0, "GND", "GND"), None);
        assert!(
            diagnostics
                .iter()
                .any(|item| { item.code == "PCB001" && item.message.contains("but U1.1 is on") })
        );
    }

    /// Warns when the declared target or capacitor pad cannot be uniquely resolved.
    #[test]
    fn pcb001_reports_missing_pad_geometry_as_unverified() {
        let missing_board = BoardDocument::from_text(
            "missing-pad.kicad_pcb",
            r#"(kicad_pcb
                (footprint "Package:Capacitor" (layer "F.Cu") (at 104 50 90)
                    (property "Reference" "C1")
                    (pad "1" smd rect (at 2 0) (size 1 1) (layers "F.Cu") (net 1 "VDD"))
                )
            )"#,
        )
        .expect("board fixture parses");
        let unresolved = validate_decoupling_pad_distances(&profile(10.0), &missing_board, None);
        assert!(unresolved.iter().any(|item| {
            item.code == "PCB001"
                && item.severity == kicad_clippy_core::Severity::Warning
                && item.message.contains("absent or ambiguous")
        }));
    }

    /// Rejects a capacitor return pad that is assigned to a different board net than its contract.
    #[test]
    fn pcb001_rejects_mismatched_return_pad_net() {
        let diagnostics =
            validate_decoupling_pad_distances(&profile(10.0), &board(104.0, "VDD", "VDD"), None);
        assert!(diagnostics.iter().any(|item| {
            item.code == "PCB001"
                && item.severity == kicad_clippy_core::Severity::Error
                && item.message.contains("return pad 2")
                && item.message.contains("not the declared return net")
        }));
    }

    /// Reports an unverified warning when a profile requests pad distance but no board is supplied.
    #[test]
    fn pcb001_warns_when_board_evidence_is_not_supplied() {
        let diagnostics = validate_missing_board(&profile(2.0));
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "PCB001");
        assert!(diagnostics[0].message.contains("no .kicad_pcb board"));
    }
}
