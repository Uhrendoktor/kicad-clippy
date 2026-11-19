//! Checks `SCH015` placement of explicit no-connect markers on transformed symbol pins.

use kicad_clippy_core::BuiltinRuleId;
use kicad_clippy_core::Diagnostic;
use kiutils_sexpr::Node;
use super::at_point;
use super::collect_nodes;
use super::collect_placed_pin_positions;
use super::distance;
use super::GEOMETRY_TOLERANCE_MM;
use super::point_from_at;

/// Checks that explicit no-connect markers coincide with supported transformed pins.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// SCH015 diagnostics for no-connect markers that do not coincide with a supported transformed symbol-pin anchor.
pub(super) fn check_no_connect_markers(root: &Node) -> Vec<Diagnostic> {
    let markers = collect_nodes(root, "no_connect");
    if markers.is_empty() {
        return Vec::new();
    }
    let Some(pin_positions) = collect_placed_pin_positions(root) else {
        return Vec::new();
    };
    markers
        .into_iter()
        .filter_map(|marker| {
            let point = point_from_at(marker)?;
            let matches_pin = pin_positions
                .iter()
                .any(|pin| distance(*pin, point) < GEOMETRY_TOLERANCE_MM);
            (!matches_pin).then(|| {
                at_point(BuiltinRuleId::Sch015,
                    format!(
                        "no-connect marker at ({:.3}, {:.3}) does not coincide with a placed symbol pin",
                        point.x, point.y
                    ),
                    point,
                    span_of(marker),
                )
            })
        })
        .collect()
}
