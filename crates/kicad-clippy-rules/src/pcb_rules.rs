//! PCB facts and profile-driven board checks that complement native KiCad DRC.

use std::collections::BTreeMap;

use kicad_clippy_core::{BuiltinRuleId, Diagnostic, Severity};
use kicad_clippy_erc::Netlist;
use kicad_clippy_parser::BoardDocument;
use kiutils_sexpr::{Atom, Node};

use crate::{ComponentKind, LintProfile};

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
    /// * document — Lossless board document with decoded footprint and pad expressions.
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
            let Some(is_back) = match layer {
                Some("F.Cu") => Some(false),
                Some("B.Cu") => Some(true),
                _ => None,
            } else {
                continue;
            };
            let (scale_x, scale_y) = footprint_scale(footprint).unwrap_or((1.0, 1.0));
            if !scale_x.is_finite()
                || !scale_y.is_finite()
                || scale_x <= 0.0
                || scale_y <= 0.0
            {
                continue;
            }
            let effective_angle = if is_back {
                -footprint_angle
            } else {
                footprint_angle
            };
            let radians = effective_angle.to_radians();
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
                if is_back {
                    // KiCad mirrors pad-local Y around the footprint X axis when a footprint is flipped.
                    local_y = -local_y;
                }
                // The footprint transform applies scale, then rotation, then translation.
                let rotated_x = local_x * cos_angle + local_y * sin_angle;
                let rotated_y = local_y * cos_angle - local_x * sin_angle;
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

/// Parses a footprint reference from its modern property or legacy fp_text node.
///
/// # Arguments
///
/// * footprint — Placed footprint S-expression.
///
/// # Returns
///
/// The non-empty footprint reference, or None when it is missing.
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

/// Resolves a serialized X/Y coordinate and optional angle from an at child.
///
/// # Arguments
///
/// * node — Footprint or pad expression with an at child.
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
/// * footprint — Placed footprint S-expression.
///
/// # Returns
///
/// The X/Y scale pair, or None when no explicit scale is serialized.
fn footprint_scale(footprint: &Node) -> Option<(f64, f64)> {
    let items = child_list(footprint, "scale").and_then(list_items)?;
    Some((items.get(1).and_then(number)?, items.get(2).and_then(number)?))
}

/// Resolves a unique board pad by footprint reference and pad number.
///
/// # Arguments
///
/// * facts — Normalized board pad facts.
/// * reference — Footprint reference designator.
/// * number — Pad number to resolve.
///
/// # Returns
///
/// The unique matching pad, or None for an absent or ambiguous match.
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

/// Runs PCB001 for each capacitor whose profile declares a maximum target pad-center distance.
///
/// # Arguments
///
/// * profile — Component-intent metadata containing optional decoupling pad-distance thresholds.
/// * board — Parsed PCB with transformed footprint pad anchors and board net names.
/// * netlist — Optional exported schematic netlist used to cross-check expected target/capacitor nets.
///
/// # Returns
///
/// PCB001 errors for excessive or inconsistent pad distances, and warnings when board facts are incomplete.
pub(crate) fn validate_decoupling_pad_distances(
    profile: &LintProfile,
    board: &BoardDocument,
    netlist: Option<&Netlist>,
) -> Vec<Diagnostic> {
    let facts = BoardFacts::from_document(board);
    let mut diagnostics = Vec::new();
    for (reference, intent) in &profile.components {
        if intent.kind != ComponentKind::Capacitor || intent.role.0 != "decoupling" {
            continue;
        }
        let Some(contract) = intent.decoupling.as_ref() else {
            continue;
        };
        let Some(maximum_distance) = contract.max_target_pad_distance_mm else {
            continue;
        };
        if !maximum_distance.is_finite() || maximum_distance < 0.0 {
            continue;
        }
        let Some((target_reference, target_number)) = contract.target_pin.rsplit_once('.') else {
            continue;
        };
        let (Some(target_pad), Some(capacitor_pad)) = (
            unique_pad(&facts, target_reference, target_number),
            unique_pad(&facts, reference, &contract.target_terminal),
        ) else {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Pcb001,
                Severity::Warning,
                format!(
                    "{reference} PCB001 pad distance is unverified because board pad {} or {}.{} is absent or ambiguous",
                    contract.target_pin, reference, contract.target_terminal
                ),
            ));
            continue;
        };

        match (&target_pad.net_name, &capacitor_pad.net_name) {
            (Some(target_net), Some(capacitor_net)) if target_net != capacitor_net => {
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Pcb001,
                    Severity::Error,
                    format!(
                        "{reference} decoupling pad {} is on board net {capacitor_net:?}, but {} is on {target_net:?}",
                        contract.target_terminal, contract.target_pin
                    ),
                ));
                continue;
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

        if let Some(netlist) = netlist {
            let target_net = netlist.net_for_pin(target_reference, target_number);
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
                    continue;
                }
                (Some(expected_target), _) if target_pad.net_name.as_deref() != Some(expected_target) => {
                    diagnostics.push(Diagnostic::for_builtin_rule(
                        BuiltinRuleId::Pcb001,
                        Severity::Error,
                        format!(
                            "{reference} board pad net {:?} for {} differs from schematic netlist net {expected_target:?}",
                            target_pad.net_name, contract.target_pin
                        ),
                    ));
                    continue;
                }
                (Some(_), Some(expected_capacitor))
                    if capacitor_pad.net_name.as_deref() != Some(expected_capacitor) =>
                {
                    diagnostics.push(Diagnostic::for_builtin_rule(
                        BuiltinRuleId::Pcb001,
                        Severity::Error,
                        format!(
                            "{reference} capacitor board pad net {:?} differs from schematic netlist net {expected_capacitor:?}",
                            capacitor_pad.net_name
                        ),
                    ));
                    continue;
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
        } else {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Pcb001,
                Severity::Warning,
                format!(
                    "{reference} PCB001 board pad distance is evaluated, but schematic net parity is unverified because no exported netlist was supplied"
                ),
            ));
        }

        let distance = (target_pad.position_mm.0 - capacitor_pad.position_mm.0)
            .hypot(target_pad.position_mm.1 - capacitor_pad.position_mm.1);
        if distance > maximum_distance + PCB_DISTANCE_TOLERANCE_MM {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Pcb001,
                Severity::Error,
                format!(
                    "{reference} capacitor pad {} is {distance:.2} mm from target pad {}, exceeding max_target_pad_distance_mm of {maximum_distance:.2} mm",
                    contract.target_terminal, contract.target_pin
                ),
            ));
        }
    }
    diagnostics
}

/// Reports an unverified warning for each pad-distance contract when no board was supplied.
///
/// # Arguments
///
/// * profile — Component-intent declarations with optional PCB001 thresholds.
///
/// # Returns
///
/// PCB001 warnings stating that no board evidence was supplied for the declared check.
pub(crate) fn validate_missing_board(profile: &LintProfile) -> Vec<Diagnostic> {
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
fn list_items(node: &Node) -> Option<&[Node]> {
    if let Node::List { items, .. } = node {
        Some(items)
    } else {
        None
    }
}

/// Returns a list node's symbolic head.
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
fn children<'a>(node: &'a Node, name: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
    list_items(node)
        .into_iter()
        .flatten()
        .skip(1)
        .filter(move |child| head(child) == Some(name))
}

/// Returns the first direct child list with the requested head.
fn child_list<'a>(node: &'a Node, name: &'a str) -> Option<&'a Node> {
    children(node, name).next()
}

/// Decodes a plain or quoted S-expression atom.
fn atom_text(node: &Node) -> Option<&str> {
    let Node::Atom { atom, .. } = node else {
        return None;
    };
    match atom {
        Atom::Symbol(value) | Atom::Quoted(value) => Some(value),
    }
}

/// Parses an atom as a finite floating-point value.
fn number(node: &Node) -> Option<f64> {
    let value = atom_text(node)?.parse::<f64>().ok()?;
    value.is_finite().then_some(value)
}

#[cfg(test)]
mod tests {
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
                    (pad "1" smd rect (at 2 0) (size 1 1) (layers "F.Cu") (net 1 "VDD"))
                )
                (footprint "Package:Part" (layer "B.Cu") (at 100 50 90)
                    (property "Reference" "C1")
                    (pad "1" smd rect (at 2 0) (size 1 1) (layers "B.Cu") (net 1 "VDD"))
                )
            )"#,
        )
        .expect("board fixture parses");
        let facts = BoardFacts::from_document(&board);
        let target = super::unique_pad(&facts, "U1", "1").expect("front pad found");
        let capacitor = super::unique_pad(&facts, "C1", "1").expect("back pad found");
        assert!((target.position_mm.0 - 100.0).abs() < 0.001);
        assert!((target.position_mm.1 - 48.0).abs() < 0.001);
        assert!((capacitor.position_mm.0 - 100.0).abs() < 0.001);
        assert!((capacitor.position_mm.1 - 52.0).abs() < 0.001);
    }
}
