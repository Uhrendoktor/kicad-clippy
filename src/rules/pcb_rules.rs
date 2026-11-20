//! PCB facts and profile-driven board checks that complement native KiCad DRC.

use crate::core::{BuiltinRuleId, Diagnostic, Severity};
use crate::erc::Netlist;
use crate::parser::BoardDocument;
use kiutils_sexpr::{Atom, Node};

use crate::rules::{ComponentKind, DecouplingIntent, LintProfile};

/// Millimetre tolerance used when comparing PCB pad-center distances and uniform pad scaling.
const PCB_DISTANCE_TOLERANCE_MM: f64 = 0.001;
/// Component metadata key for the opt-in minimum width of a test-point contact pad.
const MINIMUM_PROBE_PAD_WIDTH_ATTRIBUTE: &str = "minimum_probe_pad_width_mm";

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
    /// Whether the pad is assigned to copper and solder-mask layers for surface exposure.
    probe_exposed: bool,
    /// Minimum supported contact width after a uniform footprint scale, or None when the shape/transform is unsupported.
    probe_contact_width_mm: Option<f64>,
}

/// Normalized board facts currently required by PCB001 and PCB009.
#[derive(Clone, Debug, Default)]
struct BoardFacts {
    /// Placed footprint pads with transformed board-space anchors.
    pads: Vec<BoardPadFact>,
}

impl BoardFacts {
    /// Extracts placed footprint pad anchors and supported probe-contact geometry from a parsed PCB.
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
            let serialized_scale_present = child_list(footprint, "scale").is_some();
            let resolved_scale = footprint_scale(footprint);
            let (scale_x, scale_y) = resolved_scale.unwrap_or((1.0, 1.0));
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
                    probe_exposed: pad_has_copper_and_mask_layers(pad),
                    probe_contact_width_mm: if serialized_scale_present && resolved_scale.is_none()
                    {
                        None
                    } else {
                        pad_minimum_contact_width_mm(pad, scale_x, scale_y)
                    },
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

/// Resolves the supported minimum contact width of a test-point pad after its footprint scale.
///
/// Only circle, rectangle, oval, and round-rectangle pads are handled. Non-uniform
/// footprint scaling and custom or other pad shapes remain unresolved rather than
/// being approximated as probeable contact area.
///
/// # Arguments
///
/// * `pad` — Serialized footprint pad expression containing its shape and size.
/// * `scale_x` — Dimensionless factor applied to pad-local X coordinates by the placed-footprint transform.
/// * `scale_y` — Dimensionless factor applied to pad-local Y coordinates by the placed-footprint transform.
///
/// # Returns
///
/// The smaller scaled pad dimension in millimetres, or `None` when the shape,
/// dimensions, or footprint scale cannot be measured conservatively.
fn pad_minimum_contact_width_mm(pad: &Node, scale_x: f64, scale_y: f64) -> Option<f64> {
    if !scale_x.is_finite()
        || !scale_y.is_finite()
        || (scale_x - scale_y).abs() > PCB_DISTANCE_TOLERANCE_MM
    {
        return None;
    }
    let shape = list_items(pad)?.get(3).and_then(atom_text)?;
    if !matches!(shape, "circle" | "rect" | "oval" | "roundrect") {
        return None;
    }
    let size = child_list(pad, "size").and_then(list_items)?;
    let width_mm = number(size.get(1)?)?;
    let height_mm = number(size.get(2)?)?;
    if width_mm <= 0.0 || height_mm <= 0.0 {
        return None;
    }
    let contact_width_mm = width_mm.min(height_mm) * scale_x;
    (contact_width_mm.is_finite() && contact_width_mm > 0.0).then_some(contact_width_mm)
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
    let Some(expected_return_net) = netlist.net_for_pin(reference, &contract.return_terminal)
    else {
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

/// Checks each required test-point net against its declared, placed, exposed board pad.
///
/// # Arguments
///
/// * `profile` — Project profile containing required critical nets and declared test-point component pin mappings.
/// * `board` — Parsed PCB containing placed footprints, pad nets, and pad layer assignments.
///
/// # Returns
///
/// PCB009 errors for missing/ambiguous pads, mismatched board nets, and pads without exposed copper/mask layers.
pub fn validate_test_point_pad_access(
    profile: &LintProfile,
    board: &BoardDocument,
) -> Vec<Diagnostic> {
    let facts = BoardFacts::from_document(board);
    let mut diagnostics = Vec::new();
    for expected_net in &profile.required_test_point_nets {
        let candidates = test_point_pin_mappings(profile, expected_net);
        if candidates.is_empty() {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Pcb009,
                Severity::Error,
                format!(
                    "required test-point net {expected_net:?} has no declared test-point pin mapping to resolve on the PCB"
                ),
            ));
            continue;
        }
        for (reference, number) in candidates {
            let Some(pad) = unique_pad(&facts, &reference, &number) else {
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Pcb009,
                    Severity::Error,
                    format!(
                        "{reference}.{number} is a required test point for net {expected_net:?}, but its PCB footprint pad is missing or ambiguous"
                    ),
                ));
                continue;
            };
            match pad.net_name.as_deref() {
                Some(actual_net) if actual_net != expected_net => {
                    diagnostics.push(Diagnostic::for_builtin_rule(
                        BuiltinRuleId::Pcb009,
                        Severity::Error,
                        format!(
                            "{reference}.{number} board pad is on {actual_net:?}, but required test-point net is {expected_net:?}"
                        ),
                    ));
                }
                None => diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Pcb009,
                    Severity::Warning,
                    format!(
                        "{reference}.{number} board pad net is unverified because KiCad assigned no net to the test-point pad"
                    ),
                )),
                _ => {}
            }
            if !pad.probe_exposed {
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Pcb009,
                    Severity::Error,
                    format!(
                        "{reference}.{number} test-point pad for net {expected_net:?} must include both a copper layer and an exposed solder-mask layer"
                    ),
                ));
            }
            if let Some(diagnostic) =
                validate_minimum_probe_pad_width(profile, &reference, &number, pad)
            {
                diagnostics.push(diagnostic);
            }
        }
    }
    diagnostics
}

/// Enforces an optional minimum test-point pad contact width declared by the component profile.
///
/// # Arguments
///
/// * `profile` — Project component intent containing an optional `minimum_probe_pad_width_mm` attribute.
/// * `reference` — Test-point reference designator associated with the resolved board pad.
/// * `number` — Serialized pad number mapped to the required net.
/// * `pad` — Resolved board pad with a supported minimum contact-width measurement when available.
///
/// # Returns
///
/// A PCB009 error for an invalid threshold or too-small pad, a warning when the requested width
/// cannot be measured conservatively, or `None` when no threshold is declared or it passes.
fn validate_minimum_probe_pad_width(
    profile: &LintProfile,
    reference: &str,
    number: &str,
    pad: &BoardPadFact,
) -> Option<Diagnostic> {
    let raw_threshold = profile
        .components
        .get(reference)?
        .attributes
        .get(MINIMUM_PROBE_PAD_WIDTH_ATTRIBUTE)?;
    let invalid_configuration = || {
        Diagnostic::for_builtin_rule(
            BuiltinRuleId::Pcb009,
            Severity::Error,
            format!(
                "{reference}.{number} {MINIMUM_PROBE_PAD_WIDTH_ATTRIBUTE} must be a finite positive distance in millimetres, but was {raw_threshold:?}"
            ),
        )
    };
    let Ok(minimum_width_mm) = raw_threshold.parse::<f64>() else {
        return Some(invalid_configuration());
    };
    if !minimum_width_mm.is_finite() || minimum_width_mm <= 0.0 {
        return Some(invalid_configuration());
    }
    let Some(actual_width_mm) = pad.probe_contact_width_mm else {
        return Some(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Pcb009,
            Severity::Warning,
            format!(
                "{reference}.{number} minimum probe-pad contact width is unverified because its pad shape or footprint scale is unsupported"
            ),
        ));
    };
    if actual_width_mm + PCB_DISTANCE_TOLERANCE_MM < minimum_width_mm {
        return Some(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Pcb009,
            Severity::Error,
            format!(
                "{reference}.{number} test-point pad has minimum contact width {actual_width_mm:.2} mm; {MINIMUM_PROBE_PAD_WIDTH_ATTRIBUTE} requires {minimum_width_mm:.2} mm"
            ),
        ));
    }
    None
}

/// Reports required test-point nets whose board-level exposure cannot be checked without a PCB.
///
/// # Arguments
///
/// * `profile` — Project profile containing the critical test-point nets that need board verification.
///
/// # Returns
///
/// PCB009 warnings for required test-point nets when no PCB document was supplied.
pub fn validate_missing_test_point_board(profile: &LintProfile) -> Vec<Diagnostic> {
    profile
        .required_test_point_nets
        .iter()
        .map(|net| {
            Diagnostic::for_builtin_rule(
                BuiltinRuleId::Pcb009,
                Severity::Warning,
                format!(
                    "test-point pad net, exposure, and any declared minimum contact width for required net {net:?} are unverified because no .kicad_pcb board was supplied"
                ),
            )
        })
        .collect()
}

/// Returns declared test-point component pins whose expected net matches the required project net.
///
/// # Arguments
///
/// * `profile` — Profile containing component kinds, roles, and per-pin expected-net mappings.
/// * `expected_net` — Critical net whose test-point pads are being resolved.
///
/// # Returns
///
/// Owned component-reference/pin-number pairs for profile-declared test points assigned to the requested net.
fn test_point_pin_mappings(profile: &LintProfile, expected_net: &str) -> Vec<(String, String)> {
    profile
        .components
        .iter()
        .filter(|(_, intent)| {
            intent.kind == ComponentKind::TestPoint && intent.role.0 == "test_point"
        })
        .flat_map(|(reference, intent)| {
            intent
                .pin_nets
                .iter()
                .filter(|(_, net)| net.trim() == expected_net)
                .map(|(number, _)| (reference.clone(), number.clone()))
        })
        .collect()
}

/// Returns whether a PCB pad uses at least one copper layer and one solder-mask opening layer.
///
/// # Arguments
///
/// * `pad` — Serialized footprint pad expression containing its effective layer list.
///
/// # Returns
///
/// `true` when the pad layer list contains copper and solder-mask layers; otherwise `false`.
fn pad_has_copper_and_mask_layers(pad: &Node) -> bool {
    let Some(items) = child_list(pad, "layers").and_then(list_items) else {
        return false;
    };
    let layers: Vec<_> = items.iter().skip(1).filter_map(atom_text).collect();
    let front_exposed = (layers.contains(&"F.Cu") || layers.contains(&"*.Cu"))
        && (layers.contains(&"F.Mask") || layers.contains(&"*.Mask"));
    let back_exposed = (layers.contains(&"B.Cu") || layers.contains(&"*.Cu"))
        && (layers.contains(&"B.Mask") || layers.contains(&"*.Mask"));
    front_exposed || back_exposed
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
    use crate::parser::BoardDocument;

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
    use crate::parser::BoardDocument;
    use crate::rules::LintProfile;

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
    ) -> crate::erc::Netlist {
        let mut pin_nets = std::collections::BTreeMap::new();
        for (reference, pin, net) in [
            ("U1", "1", target_net),
            ("C1", "1", capacitor_net),
            ("C1", "2", return_net),
        ] {
            if let Some(net) = net {
                pin_nets.insert(
                    crate::erc::PinRef {
                        reference: reference.to_owned(),
                        pin: pin.to_owned(),
                    },
                    net.to_owned(),
                );
            }
        }
        crate::erc::Netlist {
            pin_nets,
            ..crate::erc::Netlist::default()
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
                && item.severity == crate::core::Severity::Warning
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
                && item.severity == crate::core::Severity::Warning
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
            item.code == "PCB001" && item.severity == crate::core::Severity::Error
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
                && item.severity == crate::core::Severity::Warning
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
                && item.severity == crate::core::Severity::Error
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

#[cfg(test)]
mod test_point_tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::{BoardFacts, validate_missing_test_point_board, validate_test_point_pad_access};
    use crate::parser::BoardDocument;
    use crate::rules::LintProfile;

    /// Parses a version-one profile requiring a VDD test point at TP1 pin 1.
    ///
    /// # Returns
    ///
    /// A project profile that declares TP1.1 as a test point for required net VDD.
    fn test_point_profile() -> LintProfile {
        serde_json::from_str(
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
            }"#,
        )
        .expect("test-point profile parses")
    }

    /// Builds a profile requiring the supplied minimum test-point contact width.
    ///
    /// # Arguments
    ///
    /// * `minimum_width` — Required smaller pad dimension in millimetres, serialized as a component attribute.
    ///
    /// # Returns
    ///
    /// A profile whose TP1.1 test-point pad must meet the requested minimum contact width.
    fn minimum_probe_width_profile(minimum_width: &str) -> LintProfile {
        let source = format!(
            r#"{{"schema_version":1,"required_test_point_nets":["VDD"],"components":{{"TP1":{{
                "kind":"test_point","role":"test_point",
                "function":"VDD measurement access",
                "rationale":"Provides a production test pad",
                "pin_nets":{{"1":"VDD"}},
                "attributes":{{"minimum_probe_pad_width_mm":"{minimum_width}"}}
            }}}}}}"#
        );
        serde_json::from_str(&source).expect("minimum probe-width profile parses")
    }

    /// Builds a board containing TP1.1 with configurable net, footprint side, pad kind and layers.
    ///
    /// # Arguments
    ///
    /// * `net` — Optional net name serialized on TP1.1.
    /// * `footprint_layer` — Side on which the test-point footprint is placed, F.Cu or B.Cu.
    /// * `pad_kind` — Serialized pad kind, such as `smd` or `thru_hole`.
    /// * `layers` — Complete pad layer expression, including the outer layers list.
    ///
    /// # Returns
    ///
    /// A parsed PCB containing a single placed TP1 footprint and its pin-1 test pad.
    fn test_point_board(
        net: Option<&str>,
        footprint_layer: &str,
        pad_kind: &str,
        layers: &str,
    ) -> BoardDocument {
        test_point_board_with_pad_geometry(
            net,
            footprint_layer,
            pad_kind,
            "circle",
            "1 1",
            "",
            layers,
        )
    }

    /// Builds a board fixture containing TP1.1 with explicit pad shape, size, and optional footprint scale.
    ///
    /// # Arguments
    ///
    /// * `net` — Optional board net assigned to the pad.
    /// * `footprint_layer` — Front or back copper layer on which the footprint is placed.
    /// * `pad_kind` — Serialized pad technology token such as `smd` or `thru_hole`.
    /// * `shape` — Serialized KiCad pad-shape token that selects the supported nominal contact-geometry model.
    /// * `size` — Serialized X/Y pad dimensions in millimetres.
    /// * `scale` — Optional complete `(scale x y)` footprint expression.
    /// * `layers` — Complete serialized pad layer-list expression.
    ///
    /// # Returns
    ///
    /// A parsed PCB containing one placed TP1 footprint and its configured pad.
    fn test_point_board_with_pad_geometry(
        net: Option<&str>,
        footprint_layer: &str,
        pad_kind: &str,
        shape: &str,
        size: &str,
        scale: &str,
        layers: &str,
    ) -> BoardDocument {
        let net_expression = net.map_or_else(String::new, |name| format!("(net 1 \"{name}\")"));
        let source = format!(
            r#"(kicad_pcb
                (footprint "TestPoint:TP" (layer "{footprint_layer}") (at 10 10)
                    {scale}
                    (property "Reference" "TP1")
                    (pad "1" {pad_kind} {shape} (at 0 0) (size {size}) {layers} {net_expression})
                )
            )"#
        );
        BoardDocument::from_text("test-point.kicad_pcb", source).expect("board parses")
    }

    /// Accepts a test-point pad assigned to the required net with both copper and mask exposure.
    #[test]
    fn pcb009_accepts_exposed_test_point_pad_on_required_net() {
        let diagnostics = validate_test_point_pad_access(
            &test_point_profile(),
            &test_point_board(Some("VDD"), "F.Cu", "smd", r#"(layers "F.Cu" "F.Mask")"#),
        );
        assert_eq!(diagnostics, Vec::new());
    }

    /// Rejects a placed test-point pad assigned to the wrong board net.
    #[test]
    fn pcb009_rejects_test_point_pad_on_wrong_net() {
        let diagnostics = validate_test_point_pad_access(
            &test_point_profile(),
            &test_point_board(Some("GND"), "F.Cu", "smd", r#"(layers "F.Cu" "F.Mask")"#),
        );
        assert!(diagnostics.iter().any(|item| {
            item.code == "PCB009"
                && item.severity == crate::core::Severity::Error
                && item.message.contains("on \"GND\"")
        }));
    }

    /// Rejects a test-point pad that lacks a solder-mask opening layer.
    #[test]
    fn pcb009_rejects_tented_test_point_pad() {
        let diagnostics = validate_test_point_pad_access(
            &test_point_profile(),
            &test_point_board(Some("VDD"), "F.Cu", "smd", r#"(layers "F.Cu")"#),
        );
        assert!(diagnostics.iter().any(|item| {
            item.code == "PCB009" && item.message.contains("exposed solder-mask layer")
        }));
    }

    /// Rejects a profile-declared test-point footprint pin that is absent from the PCB.
    #[test]
    fn pcb009_rejects_missing_test_point_pad() {
        let board = BoardDocument::from_text("missing-test-point.kicad_pcb", "(kicad_pcb)")
            .expect("board parses");
        let diagnostics = validate_test_point_pad_access(&test_point_profile(), &board);
        assert!(diagnostics.iter().any(|item| {
            item.code == "PCB009" && item.message.contains("missing or ambiguous")
        }));
    }

    /// Warns when required test-point exposure cannot be evaluated without a board.
    #[test]
    fn pcb009_warns_when_board_is_not_supplied() {
        let diagnostics = validate_missing_test_point_board(&test_point_profile());
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "PCB009");
        assert_eq!(diagnostics[0].severity, crate::core::Severity::Warning);
    }

    /// Accepts a back-side SMD test-point pad only when its back-side mask is exposed.
    #[test]
    fn pcb009_accepts_back_side_test_point_exposure() {
        let diagnostics = validate_test_point_pad_access(
            &test_point_profile(),
            &test_point_board(Some("VDD"), "B.Cu", "smd", r#"(layers "B.Cu" "B.Mask")"#),
        );
        assert_eq!(diagnostics, Vec::new());
    }

    /// Rejects a front-side copper pad whose only solder-mask opening is on the back side.
    #[test]
    fn pcb009_rejects_mask_exposure_on_the_wrong_side() {
        let diagnostics = validate_test_point_pad_access(
            &test_point_profile(),
            &test_point_board(Some("VDD"), "F.Cu", "smd", r#"(layers "F.Cu" "B.Mask")"#),
        );
        assert!(diagnostics.iter().any(|item| {
            item.code == "PCB009" && item.message.contains("exposed solder-mask layer")
        }));
    }

    /// Accepts a standard test-point pad whose smaller dimension meets the declared minimum.
    #[test]
    fn pcb009_accepts_declared_minimum_probe_pad_width() {
        let diagnostics = validate_test_point_pad_access(
            &minimum_probe_width_profile("0.8"),
            &test_point_board_with_pad_geometry(
                Some("VDD"),
                "F.Cu",
                "smd",
                "roundrect",
                "1.0 0.9",
                "",
                r#"(layers "F.Cu" "F.Mask")"#,
            ),
        );
        assert_eq!(diagnostics, Vec::new());
    }

    /// Rejects a supported test-point pad whose smaller dimension is below the declared minimum.
    #[test]
    fn pcb009_rejects_test_point_pad_below_minimum_probe_width() {
        let diagnostics = validate_test_point_pad_access(
            &minimum_probe_width_profile("0.8"),
            &test_point_board_with_pad_geometry(
                Some("VDD"),
                "F.Cu",
                "smd",
                "rect",
                "0.6 1.2",
                "",
                r#"(layers "F.Cu" "F.Mask")"#,
            ),
        );
        assert!(diagnostics.iter().any(|item| {
            item.code == "PCB009"
                && item.severity == crate::core::Severity::Error
                && item.message.contains("0.60 mm")
                && item.message.contains("requires 0.80 mm")
        }));
    }

    /// Reports contact width as unverified for a custom pad whose usable contact area is not modelled.
    #[test]
    fn pcb009_warns_for_custom_test_point_pad_contact_geometry() {
        let diagnostics = validate_test_point_pad_access(
            &minimum_probe_width_profile("0.8"),
            &test_point_board_with_pad_geometry(
                Some("VDD"),
                "F.Cu",
                "smd",
                "custom",
                "1.0 1.0",
                "",
                r#"(layers "F.Cu" "F.Mask")"#,
            ),
        );
        assert!(diagnostics.iter().any(|item| {
            item.code == "PCB009"
                && item.severity == crate::core::Severity::Warning
                && item.message.contains("contact width is unverified")
        }));
    }

    /// Reports contact width as unverified when a non-uniform footprint scale makes the pad transform ambiguous.
    #[test]
    fn pcb009_warns_for_non_uniform_test_point_footprint_scale() {
        let diagnostics = validate_test_point_pad_access(
            &minimum_probe_width_profile("0.8"),
            &test_point_board_with_pad_geometry(
                Some("VDD"),
                "F.Cu",
                "smd",
                "rect",
                "1.0 1.0",
                "(scale 2 1)",
                r#"(layers "F.Cu" "F.Mask")"#,
            ),
        );
        assert!(diagnostics.iter().any(|item| {
            item.code == "PCB009"
                && item.severity == crate::core::Severity::Warning
                && item.message.contains("footprint scale is unsupported")
        }));
    }

    /// Reports minimum probe-pad width as unverified when an explicit footprint scale is malformed.
    #[test]
    fn pcb009_warns_for_malformed_test_point_footprint_scale() {
        let diagnostics = validate_test_point_pad_access(
            &minimum_probe_width_profile("0.8"),
            &test_point_board_with_pad_geometry(
                Some("VDD"),
                "F.Cu",
                "smd",
                "rect",
                "1.0 1.0",
                "(scale malformed 1)",
                r#"(layers "F.Cu" "F.Mask")"#,
            ),
        );
        assert!(diagnostics.iter().any(|item| {
            item.code == "PCB009"
                && item.severity == crate::core::Severity::Warning
                && item.message.contains("footprint scale is unsupported")
        }));
    }

    /// Rejects a profile-declared minimum contact width that is not a finite positive distance.
    #[test]
    fn pcb009_rejects_invalid_minimum_probe_pad_width_configuration() {
        let diagnostics = validate_test_point_pad_access(
            &minimum_probe_width_profile("NaN"),
            &test_point_board(Some("VDD"), "F.Cu", "smd", r#"(layers "F.Cu" "F.Mask")"#),
        );
        assert!(diagnostics.iter().any(|item| {
            item.code == "PCB009"
                && item.severity == crate::core::Severity::Error
                && item.message.contains("must be a finite positive distance")
        }));
    }

    /// Accepts plated through-hole test-point pads whose wildcard layers expose copper and mask on both sides.
    #[test]
    fn pcb009_accepts_through_hole_wildcard_copper_and_mask_layers() {
        let board = test_point_board(
            Some("VDD"),
            "F.Cu",
            "thru_hole",
            r#"(layers "*.Cu" "*.Mask")"#,
        );
        let facts = BoardFacts::from_document(&board);
        let pad = super::unique_pad(&facts, "TP1", "1").expect("test-point pad resolves");
        assert!(pad.probe_exposed);
    }
}
