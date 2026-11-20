//! Checks `SCH005` alignment between connected wire axes and transformed symbol-pin axes.

use super::GEOMETRY_TOLERANCE_MM;
use super::Point;
use super::Segment;
use super::SymbolTransform;
use super::at_point;
use super::atom_value;
use super::child_list;
use super::children;
use super::embedded_symbol_pins;
use super::list_items;
use super::number;
use super::point_on_segment;
use super::read_symbol_transform;
use super::span_of;
use super::symbol_transform_affine;
use crate::core::BuiltinRuleId;
use crate::core::Diagnostic;
use kiutils_sexpr::Node;
use kurbo::Point as CurvePoint;

/// Represents the supported horizontal, vertical, or oblique pin-axis alternatives.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PinAxis {
    /// Wire axis parallel to the X coordinate.
    Horizontal,
    /// Wire axis parallel to the Y coordinate.
    Vertical,
    /// Wire axis at a non-cardinal angle to both schematic coordinates.
    Oblique,
}

impl PinAxis {
    /// Returns the lowercase orientation token (`horizontal` or `vertical`) used when describing the pin's permitted wire axis.
    ///
    /// # Returns
    ///
    /// The lowercase axis token: `horizontal` for an X-parallel pin or `vertical` for a Y-parallel pin.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Horizontal => "horizontal",
            Self::Vertical => "vertical",
            Self::Oblique => "oblique",
        }
    }
}

/// Checks connected wire axes against the transformed orientation of placed symbol pins.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
/// * `segments` — Parsed wire segments used to determine geometry, topology, or obstacle intersections.
///
/// # Returns
///
/// SCH005 diagnostics for connected wire axes that do not align with the transformed orientation of their symbol pins.
pub(super) fn check_pin_orientation(root: &Node, segments: &[Segment]) -> Vec<Diagnostic> {
    let Some(library_symbols) = child_list(root, "lib_symbols") else {
        return Vec::new();
    };
    children(root, "symbol")
        .flat_map(|instance| check_instance_pin_orientation(instance, library_symbols, segments))
        .collect()
}
/// Checks one placed symbol's connected pins against its resolved library pin orientation.
///
/// # Arguments
///
/// * `instance` — Placed symbol instance node containing reference, transform, unit, and conversion metadata.
/// * `library_symbols` — Root node of the embedded library-symbol collection used to resolve the placed instance's referenced symbol definition.
/// * `segments` — Parsed wire segments used to determine geometry, topology, or obstacle intersections.
///
/// # Returns
///
/// SCH005 diagnostics for pins on the selected placed symbol whose attached wire direction conflicts with the library pin orientation.
fn check_instance_pin_orientation(
    instance: &Node,
    library_symbols: &Node,
    segments: &[Segment],
) -> Vec<Diagnostic> {
    let Some(transform) = read_symbol_transform(instance) else {
        return Vec::new();
    };
    if transform.reference.starts_with('#') {
        return Vec::new();
    }
    let Some(lib_id) = child_list(instance, "lib_id")
        .and_then(list_items)
        .and_then(|items| items.get(1))
        .and_then(atom_value)
    else {
        return Vec::new();
    };
    let Some(library_symbol) = children(library_symbols, "symbol").find(|definition| {
        list_items(definition)
            .and_then(|items| items.get(1))
            .and_then(atom_value)
            .is_some_and(|name| name == lib_id)
    }) else {
        return Vec::new();
    };

    embedded_symbol_pins(library_symbol, transform.unit, transform.convert)
        .into_iter()
        .filter_map(|pin| pin_orientation_diagnostic(instance, pin, &transform, segments))
        .collect()
}

/// Builds a diagnostic when a wire direction conflicts with the transformed pin axis.
///
/// # Arguments
///
/// * `instance` — Placed symbol instance node containing reference, transform, unit, and conversion metadata.
/// * `pin` — Embedded library pin node containing the pin's local position, rotation angle, number, and electrical name.
/// * `transform` — Resolved transform that maps library-local coordinates to the schematic sheet.
/// * `segments` — Parsed wire segments used to determine geometry, topology, or obstacle intersections.
///
/// # Returns
///
/// A diagnostic when the supplied geometry violates the rule, or `None` when the geometry is acceptable.
fn pin_orientation_diagnostic(
    instance: &Node,
    pin: &Node,
    transform: &SymbolTransform,
    segments: &[Segment],
) -> Option<Diagnostic> {
    let at = child_list(pin, "at").and_then(list_items)?;
    let pin_x = number(at.get(1)?)?;
    let pin_y = number(at.get(2)?)?;
    let pin_angle = number(at.get(3)?)?;
    let affine = symbol_transform_affine(transform.rotation, transform.mirror.as_deref())?;
    let pin_offset = affine * CurvePoint::new(pin_x, pin_y);
    let point = Point {
        x: transform.origin.x + pin_offset.x,
        y: transform.origin.y + pin_offset.y,
    };
    let pin_angle = pin_angle.to_radians();
    let axis = affine * CurvePoint::new(pin_angle.cos(), pin_angle.sin());
    let axis_length = axis.x.hypot(axis.y);
    if !axis_length.is_finite() || axis_length <= GEOMETRY_TOLERANCE_MM {
        return None;
    }
    let (axis_x, axis_y) = (axis.x / axis_length, axis.y / axis_length);
    let expected_axis = if axis_y.abs() <= GEOMETRY_TOLERANCE_MM {
        PinAxis::Horizontal
    } else if axis_x.abs() <= GEOMETRY_TOLERANCE_MM {
        PinAxis::Vertical
    } else {
        PinAxis::Oblique
    };
    let touching_segments: Vec<_> = segments
        .iter()
        .filter(|segment| point_on_segment(point, segment))
        .collect();
    if touching_segments.is_empty() {
        return None;
    }
    let aligned = touching_segments.iter().any(|segment| {
        let start_deviation_mm = axis_y
            .mul_add(-(segment.start.x - point.x), axis_x * (segment.start.y - point.y))
            .abs();
        let end_deviation_mm = axis_y
            .mul_add(-(segment.end.x - point.x), axis_x * (segment.end.y - point.y))
            .abs();
        start_deviation_mm <= GEOMETRY_TOLERANCE_MM && end_deviation_mm <= GEOMETRY_TOLERANCE_MM
    });
    if aligned {
        return None;
    }
    let pin_number = child_list(pin, "number")
        .and_then(list_items)
        .and_then(|items| items.get(1))
        .and_then(atom_value)
        .unwrap_or("?");
    Some(at_point(
        BuiltinRuleId::Sch005,
        format!(
            "{} pin {pin_number} faces {} but its connected wire is not aligned with the pin",
            transform.reference,
            expected_axis.as_str()
        ),
        point,
        span_of(instance),
    ))
}

/// Concrete implementation of built-in rule 005.
pub(super) struct Sch005;
impl<'a> crate::rules::Rule<crate::rules::RuleContext<'a>> for Sch005 {
    /// Returns the stable 005 identifier.
    ///
    /// # Returns
    ///
    /// The stable SCH005 identifier used to associate diagnostics with this rule.
    fn id(&self) -> BuiltinRuleId {
        BuiltinRuleId::Sch005
    }
    /// Evaluates connected wire axes against transformed symbol-pin axes.
    ///
    /// # Arguments
    ///
    /// * `context` — the parsed schematic and normalized wire segments.
    ///
    /// # Returns
    ///
    /// Diagnostics for connected wire axes against transformed symbol-pin axes.
    fn check(&self, context: &crate::rules::RuleContext<'a>) -> Vec<Diagnostic> {
        let Some(root) = context.document().cst().nodes.first() else {
            return Vec::new();
        };
        check_pin_orientation(root, context.segments)
    }
}
