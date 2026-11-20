//! Functional-group placement spacing, cluster compactness, body clearance, and alignment contracts.

use super::GroupAlignmentAxis;
use super::GroupIntent;
use super::SchematicFacts;
use super::geometry::{body_geometry_for_reference, minimum_body_gap};
use crate::core::BuiltinRuleId;
use crate::core::Diagnostic;
use crate::core::Severity;
use crate::rules::generic::GEOMETRY_TOLERANCE_MM;

/// Checks a group's origin spacing and cluster span, supported body clearance, and row/column alignment.
///
/// # Arguments
///
/// * `group_id` — Stable profile identifier for the group; it is included in any diagnostic produced by this check.
/// * `group` — The group-intent record containing its members and declared placement constraints.
/// * `facts` — Normalized schematic facts, including component origins and transformed symbol bodies.
///
/// # Returns
///
/// SCH006 diagnostics for invalid constraints, member origins that violate spacing/span/alignment,
/// body outlines that violate the configured clearance, or geometry that cannot be verified.
pub(super) fn validate_group_spacing(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    if let Some(minimum_spacing) = group.minimum_member_origin_spacing_mm {
        if !minimum_spacing.is_finite() || minimum_spacing <= 0.0 {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch006,
                Severity::Error,
                format!(
                    "group {group_id} minimum_member_origin_spacing_mm must be a finite positive distance"
                ),
            ));
        } else {
            diagnostics.extend(validate_member_origin_spacing(
                group_id,
                group,
                facts,
                minimum_spacing,
            ));
        }
    }

    if let Some(maximum_span) = group.maximum_member_origin_span_mm {
        if !maximum_span.is_finite() || maximum_span < 0.0 {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch006,
                Severity::Error,
                format!(
                    "group {group_id} maximum_member_origin_span_mm must be a finite non-negative distance"
                ),
            ));
        } else {
            diagnostics.extend(validate_member_origin_span(
                group_id,
                group,
                facts,
                maximum_span,
            ));
        }
    }

    if let Some(minimum_clearance) = group.minimum_member_body_clearance_mm {
        if !minimum_clearance.is_finite() || minimum_clearance < 0.0 {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch006,
                Severity::Error,
                format!(
                    "group {group_id} minimum_member_body_clearance_mm must be a finite non-negative distance"
                ),
            ));
        } else {
            diagnostics.extend(validate_member_body_clearance(
                group_id,
                group,
                facts,
                minimum_clearance,
            ));
        }
    }

    match (group.alignment_axis, group.alignment_tolerance_mm) {
        (None, None) => {}
        (Some(_), None) => diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch006,
            Severity::Error,
            format!("group {group_id} alignment_axis requires alignment_tolerance_mm"),
        )),
        (None, Some(_)) => diagnostics.push(Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch006,
            Severity::Error,
            format!("group {group_id} alignment_tolerance_mm requires alignment_axis"),
        )),
        (Some(axis), Some(tolerance_mm)) => {
            if !tolerance_mm.is_finite() || tolerance_mm < 0.0 {
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch006,
                    Severity::Error,
                    format!(
                        "group {group_id} alignment_tolerance_mm must be a finite non-negative distance"
                    ),
                ));
            } else {
                diagnostics.extend(validate_member_alignment(
                    group_id,
                    group,
                    facts,
                    axis,
                    tolerance_mm,
                ));
            }
        }
    }
    diagnostics
}

/// Checks minimum Euclidean spacing between the symbol origins of every member pair.
///
/// # Arguments
///
/// * `group_id` — Stable group key used to identify the profile declaration in emitted diagnostics.
/// * `group` — Group declaration containing the member references whose origins must be separated.
/// * `facts` — Normalized placed-component facts containing resolved symbol origins.
/// * `minimum_spacing` — Required minimum Euclidean distance between any two member origins, in millimetres.
///
/// # Returns
///
/// SCH006 diagnostics for missing origins or member pairs that violate the configured minimum spacing.
fn validate_member_origin_spacing(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    minimum_spacing: f64,
) -> Vec<Diagnostic> {
    let mut positions = Vec::new();
    let mut diagnostics = Vec::new();
    for reference in &group.members {
        let Some(component) = facts
            .components
            .iter()
            .find(|component| component.reference == *reference)
        else {
            continue;
        };
        let Some((x_mm, y_mm)) = component.position_mm else {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch006,
                Severity::Error,
                format!(
                    "group {group_id} member {reference} has no symbol-origin position for spacing validation"
                ),
            ));
            continue;
        };
        positions.push((reference.as_str(), x_mm, y_mm));
    }

    for (index, first) in positions.iter().enumerate() {
        for second in positions.iter().skip(index + 1) {
            let actual_spacing = (first.1 - second.1).hypot(first.2 - second.2);
            if actual_spacing + GEOMETRY_TOLERANCE_MM < minimum_spacing {
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch006,
                    Severity::Error,
                    format!(
                        "group {group_id} members {} and {} have symbol-origin spacing {:.2} mm; minimum is {:.2} mm",
                        first.0, second.0, actual_spacing, minimum_spacing
                    ),
                ));
            }
        }
    }
    diagnostics
}

/// Checks the maximum Euclidean distance between every pair of resolved member origins.
///
/// # Arguments
///
/// * `group_id` — Stable group key used to identify the profile declaration in emitted diagnostics.
/// * `group` — Group declaration containing the member references whose cluster diameter is constrained.
/// * `facts` — Normalized placed-component facts containing resolved symbol origins.
/// * `maximum_span` — Maximum allowed pairwise origin distance, in millimetres.
///
/// # Returns
///
/// SCH006 errors for member pairs beyond the configured span and warnings when an origin is unresolved.
fn validate_member_origin_span(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    maximum_span: f64,
) -> Vec<Diagnostic> {
    let mut positions = Vec::new();
    let mut diagnostics = Vec::new();
    for reference in &group.members {
        let Some(component) = facts
            .components
            .iter()
            .find(|component| component.reference == *reference)
        else {
            continue;
        };
        let Some((x_mm, y_mm)) = component.position_mm else {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch006,
                Severity::Warning,
                format!(
                    "group {group_id} member {reference} has no resolved symbol origin; cluster span is unverified"
                ),
            ));
            continue;
        };
        positions.push((reference.as_str(), x_mm, y_mm));
    }

    for (index, first) in positions.iter().enumerate() {
        for second in positions.iter().skip(index + 1) {
            let actual_span = (first.1 - second.1).hypot(first.2 - second.2);
            if actual_span > maximum_span + GEOMETRY_TOLERANCE_MM {
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch006,
                    Severity::Error,
                    format!(
                        "group {group_id} members {} and {} have symbol-origin span {:.2} mm; maximum is {:.2} mm",
                        first.0, second.0, actual_span, maximum_span
                    ),
                ));
            }
        }
    }
    diagnostics
}

/// Checks minimum edge-to-edge clearance between supported body shapes for every member pair.
///
/// # Arguments
///
/// * `group_id` — Stable group key used to identify the profile declaration in emitted diagnostics.
/// * `group` — Group declaration containing the member references whose body outlines are compared.
/// * `facts` — Normalized transformed symbol primitives collected from the selected library units.
/// * `minimum_clearance` — Required minimum edge-to-edge distance between member bodies, in millimetres.
///
/// # Returns
///
/// SCH006 diagnostics for insufficient body clearance or incomplete body geometry reported as unverified.
fn validate_member_body_clearance(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    minimum_clearance: f64,
) -> Vec<Diagnostic> {
    let mut bodies = Vec::new();
    let mut diagnostics = Vec::new();
    for reference in &group.members {
        let body = body_geometry_for_reference(reference, facts);
        if body.shapes.is_empty() || body.has_unsupported_geometry {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch006,
                Severity::Warning,
                format!(
                    "group {group_id} member {reference} body clearance is unverified because its supported transformed outline is incomplete"
                ),
            ));
            continue;
        }
        bodies.push((reference.as_str(), body));
    }

    for (index, first) in bodies.iter().enumerate() {
        for second in bodies.iter().skip(index + 1) {
            let Some(actual_clearance) = minimum_body_gap(&first.1, &second.1) else {
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch006,
                    Severity::Warning,
                    format!(
                        "group {group_id} body clearance between {} and {} is unverified",
                        first.0, second.0
                    ),
                ));
                continue;
            };
            if actual_clearance + GEOMETRY_TOLERANCE_MM < minimum_clearance {
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch006,
                    Severity::Error,
                    format!(
                        "group {group_id} members {} and {} have body clearance {:.2} mm; minimum is {:.2} mm",
                        first.0, second.0, actual_clearance, minimum_clearance
                    ),
                ));
            }
        }
    }
    diagnostics
}

/// Checks that member origins share the declared horizontal row or vertical column.
///
/// # Arguments
///
/// * `group_id` — Stable group key used to identify the profile declaration in emitted diagnostics.
/// * `group` — Group declaration containing the component references to align.
/// * `facts` — Normalized placed-component facts containing resolved symbol origins.
/// * `axis` — Horizontal-row or vertical-column direction selected by the group profile.
/// * `tolerance_mm` — Maximum permitted perpendicular origin deviation from the shared row or column.
///
/// # Returns
///
/// SCH006 diagnostics for member origins that exceed the configured alignment tolerance or remain unresolved.
fn validate_member_alignment(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
    axis: GroupAlignmentAxis,
    tolerance_mm: f64,
) -> Vec<Diagnostic> {
    let mut positions = Vec::new();
    let mut diagnostics = Vec::new();
    for reference in &group.members {
        let Some(component) = facts
            .components
            .iter()
            .find(|component| component.reference == *reference)
        else {
            continue;
        };
        let Some(position_mm) = component.position_mm else {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch006,
                Severity::Warning,
                format!(
                    "group {group_id} member {reference} has no resolved symbol origin; alignment is unverified"
                ),
            ));
            continue;
        };
        positions.push((reference.as_str(), position_mm));
    }

    for (index, first) in positions.iter().enumerate() {
        for second in positions.iter().skip(index + 1) {
            let (first_reference, (first_x, first_y)) = *first;
            let (second_reference, (second_x, second_y)) = *second;
            let deviation_mm = match axis {
                GroupAlignmentAxis::Horizontal => (first_y - second_y).abs(),
                GroupAlignmentAxis::Vertical => (first_x - second_x).abs(),
            };
            if deviation_mm > tolerance_mm + GEOMETRY_TOLERANCE_MM {
                let axis_name = match axis {
                    GroupAlignmentAxis::Horizontal => "horizontal row",
                    GroupAlignmentAxis::Vertical => "vertical column",
                };
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch006,
                    Severity::Error,
                    format!(
                        "group {group_id} members {first_reference} and {second_reference} deviate {deviation_mm:.2} mm from a shared {axis_name}; tolerance is {tolerance_mm:.2} mm"
                    ),
                ));
            }
        }
    }
    diagnostics
}

/// Concrete implementation of built-in rule SCH006.
pub(super) struct Sch006;

impl<'a> crate::rules::Rule<super::ProfileRuleContext<'a>> for Sch006 {
    /// Returns the stable SCH006 identifier.
    ///
    /// # Returns
    ///
    /// The stable SCH006 identifier used to associate diagnostics with this rule.
    fn id(&self) -> BuiltinRuleId {
        BuiltinRuleId::Sch006
    }

    /// Evaluates declared origin spacing/span, body-clearance, and alignment constraints.
    ///
    /// # Arguments
    ///
    /// * `context` — Project group declarations and normalized schematic component/body geometry.
    ///
    /// # Returns
    ///
    /// Diagnostics for invalid spacing/span/alignment settings, excessive body proximity, or unverified outlines.
    fn check(&self, context: &super::ProfileRuleContext<'a>) -> Vec<Diagnostic> {
        context
            .profile
            .groups
            .iter()
            .flat_map(|(id, group)| validate_group_spacing(id, group, context.facts))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::validate_group_spacing;
    use crate::core::BuiltinRuleId;
    use crate::rules::GroupAlignmentAxis;
    use crate::rules::generic::{ComponentFact, SchematicFacts, SchematicPrimitiveFact};
    use crate::rules::metadata::GroupIntent;

    /// Creates a transformed rectangular outline for one placed component.
    ///
    /// # Arguments
    ///
    /// * `reference` — Placed component reference designator assigned to the generated outline.
    /// * `left` — Left X coordinate of the rectangle in schematic millimetres.
    /// * `right` — Right X coordinate of the rectangle in schematic millimetres.
    ///
    /// # Returns
    ///
    /// A closed four-vertex body primitive spanning the requested X coordinates.
    fn rectangle(reference: &str, left: f64, right: f64) -> SchematicPrimitiveFact {
        SchematicPrimitiveFact {
            reference: reference.to_owned(),
            points_mm: vec![(left, 0.0), (right, 0.0), (right, 2.0), (left, 2.0)],
            closed: true,
            circle_mm: None,
            arc_mm: None,
            bezier_mm: None,
            stroke_width_mm: 0.0,
        }
    }

    /// Confirms body-to-body clearance is checked between transformed rectangles rather than origins.
    #[test]
    fn group_minimum_body_clearance_uses_transformed_outlines() {
        let facts = SchematicFacts {
            components: vec![
                ComponentFact {
                    reference: "R1".to_owned(),
                    uuid: None,
                    position_mm: Some((0.0, 0.0)),
                },
                ComponentFact {
                    reference: "C1".to_owned(),
                    uuid: None,
                    position_mm: Some((5.0, 0.0)),
                },
            ],
            symbol_primitives: vec![rectangle("R1", 0.0, 2.0), rectangle("C1", 5.0, 7.0)],
            ..SchematicFacts::default()
        };
        let group = GroupIntent {
            members: vec!["R1".to_owned(), "C1".to_owned()],
            minimum_member_body_clearance_mm: Some(4.0),
            ..GroupIntent::default()
        };
        let diagnostics = validate_group_spacing("power", &group, &facts);
        assert!(diagnostics.iter().any(|item| {
            item.code == BuiltinRuleId::Sch006.as_str()
                && item.message.contains("body clearance")
                && item.message.contains("3.00 mm")
        }));
    }

    /// Confirms horizontal and vertical origin alignment use a perpendicular tolerance.
    #[test]
    fn group_member_alignment_checks_perpendicular_axis() {
        let facts = SchematicFacts {
            components: vec![
                ComponentFact {
                    reference: "R1".to_owned(),
                    uuid: None,
                    position_mm: Some((0.0, 0.0)),
                },
                ComponentFact {
                    reference: "C1".to_owned(),
                    uuid: None,
                    position_mm: Some((5.0, 0.5)),
                },
            ],
            ..SchematicFacts::default()
        };
        let group = GroupIntent {
            members: vec!["R1".to_owned(), "C1".to_owned()],
            alignment_axis: Some(GroupAlignmentAxis::Horizontal),
            alignment_tolerance_mm: Some(0.2),
            ..GroupIntent::default()
        };
        assert!(
            validate_group_spacing("power", &group, &facts)
                .iter()
                .any(|item| item.message.contains("horizontal row"))
        );
    }

    /// Rejects a functional group whose resolved member origins exceed the declared cluster diameter.
    #[test]
    fn group_maximum_member_origin_span_enforces_cluster_compactness() {
        let facts = SchematicFacts {
            components: vec![
                ComponentFact { reference: "R1".to_owned(), uuid: None, position_mm: Some((0.0, 0.0)) },
                ComponentFact { reference: "C1".to_owned(), uuid: None, position_mm: Some((5.0, 0.0)) },
                ComponentFact { reference: "Q1".to_owned(), uuid: None, position_mm: Some((15.0, 0.0)) },
            ],
            ..SchematicFacts::default()
        };
        let group = GroupIntent {
            members: vec!["R1".to_owned(), "C1".to_owned(), "Q1".to_owned()],
            maximum_member_origin_span_mm: Some(12.0),
            ..GroupIntent::default()
        };
        let diagnostics = validate_group_spacing("power", &group, &facts);
        assert!(diagnostics.iter().any(|item| {
            item.code == BuiltinRuleId::Sch006.as_str()
                && item.message.contains("R1 and Q1")
                && item.message.contains("15.00 mm")
                && item.message.contains("maximum is 12.00 mm")
        }));
    }

    /// Accepts a functional group whose full pairwise origin span is within the configured bound.
    #[test]
    fn group_maximum_member_origin_span_accepts_compact_clusters() {
        let facts = SchematicFacts {
            components: vec![
                ComponentFact { reference: "R1".to_owned(), uuid: None, position_mm: Some((0.0, 0.0)) },
                ComponentFact { reference: "C1".to_owned(), uuid: None, position_mm: Some((5.0, 0.0)) },
                ComponentFact { reference: "Q1".to_owned(), uuid: None, position_mm: Some((9.0, 0.0)) },
            ],
            ..SchematicFacts::default()
        };
        let group = GroupIntent {
            members: vec!["R1".to_owned(), "C1".to_owned(), "Q1".to_owned()],
            maximum_member_origin_span_mm: Some(10.0),
            ..GroupIntent::default()
        };
        let diagnostics = validate_group_spacing("power", &group, &facts);
        assert!(!diagnostics.iter().any(|item| {
            item.code == BuiltinRuleId::Sch006.as_str()
                && item.message.contains("symbol-origin span")
        }));
    }

    /// Rejects negative or non-finite maximum member-origin span thresholds.
    #[test]
    fn group_maximum_member_origin_span_requires_finite_nonnegative_value() {
        for invalid_span in [-1.0, f64::INFINITY] {
            let group = GroupIntent {
                maximum_member_origin_span_mm: Some(invalid_span),
                ..GroupIntent::default()
            };
            let diagnostics = validate_group_spacing("power", &group, &SchematicFacts::default());
            assert!(diagnostics.iter().any(|item| {
                item.code == BuiltinRuleId::Sch006.as_str()
                    && item.message.contains("maximum_member_origin_span_mm must be a finite non-negative distance")
            }));
        }
    }

    /// Warns rather than guessing cluster compactness when a member origin is unresolved.
    #[test]
    fn group_maximum_member_origin_span_warns_for_unresolved_origin() {
        let facts = SchematicFacts {
            components: vec![
                ComponentFact { reference: "R1".to_owned(), uuid: None, position_mm: Some((0.0, 0.0)) },
                ComponentFact { reference: "C1".to_owned(), uuid: None, position_mm: None },
            ],
            ..SchematicFacts::default()
        };
        let group = GroupIntent {
            members: vec!["R1".to_owned(), "C1".to_owned()],
            maximum_member_origin_span_mm: Some(10.0),
            ..GroupIntent::default()
        };
        let diagnostics = validate_group_spacing("power", &group, &facts);
        assert!(diagnostics.iter().any(|item| {
            item.code == BuiltinRuleId::Sch006.as_str()
                && item.severity == crate::core::Severity::Warning
                && item.message.contains("cluster span is unverified")
        }));
    }

    /// Confirms curve-based or absent member body outlines are reported as unverified.
    #[test]
    fn group_body_clearance_warns_for_unresolved_outline() {
        let mut facts = SchematicFacts::default();
        facts.symbol_primitives.push(SchematicPrimitiveFact {
            reference: "R1".to_owned(),
            points_mm: vec![(0.0, 0.0), (1.0, 1.0), (2.0, 0.0)],
            closed: false,
            circle_mm: None,
            arc_mm: None,
            bezier_mm: Some([(0.0, 0.0), (0.5, 0.0), (1.5, 1.0), (2.0, 1.0)]),
            stroke_width_mm: 0.0,
        });
        let group = GroupIntent {
            members: vec!["R1".to_owned()],
            minimum_member_body_clearance_mm: Some(0.5),
            ..GroupIntent::default()
        };
        assert!(
            validate_group_spacing("power", &group, &facts)
                .iter()
                .any(|item| item.severity == crate::core::Severity::Warning)
        );
    }
}
