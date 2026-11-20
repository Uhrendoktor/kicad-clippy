//! Checks UUID-linked functional-frame title tabs, title containment, and frame spacing.

use super::{GroupIntent, LintProfile, ProfileRuleContext, SchematicFacts};
use crate::Rule;
use crate::generic::GEOMETRY_TOLERANCE_MM;
use crate::generic::SchematicGraphicFact;
use kicad_clippy_core::{BuiltinRuleId, Diagnostic, Severity};

/// Concrete implementation of SCH028.
pub(super) struct Sch028;

/// Validates one group's optional frame-tab-title relationship.
///
/// # Arguments
///
/// * `group_id` — Stable profile key identifying the functional group.
/// * `group` — Group intent containing frame, title-tab, and title-text-box UUIDs.
/// * `facts` — Normalized frame, graphic, and text-box geometry from the loaded schematic.
///
/// # Returns
///
/// SCH028 errors for incomplete, missing, ambiguous, unattached, or out-of-tab title geometry; unresolved bounds are warnings.
fn validate_group_title_layout(
    group_id: &str,
    group: &GroupIntent,
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    let has_tab = group.frame_title_tab_uuid.is_some();
    let has_title = group.frame_title_text_box_uuid.is_some();
    if !has_tab && !has_title {
        return Vec::new();
    }
    if has_tab != has_title {
        return vec![error(format!(
            "group {group_id} must declare frame_title_tab_uuid and frame_title_text_box_uuid together"
        ))];
    }
    let Some(frame_uuid) = group.frame_uuid.as_deref() else {
        return vec![error(format!(
            "group {group_id} declares a frame title tab but has no frame_uuid"
        ))];
    };
    let Some(tab_uuid) = group.frame_title_tab_uuid.as_deref() else {
        return Vec::new();
    };
    let Some(title_uuid) = group.frame_title_text_box_uuid.as_deref() else {
        return Vec::new();
    };

    let frame_bounds = match unique_frame_bounds(facts, frame_uuid) {
        Ok(bounds) => bounds,
        Err(message) => return vec![error(format!("group {group_id} {message}"))],
    };
    let tab = match unique_title_tab(facts, tab_uuid) {
        Ok(tab) => tab,
        Err(message) => return vec![error(format!("group {group_id} {message}"))],
    };
    let Some(tab_bounds) = raw_rectangle_bounds(tab) else {
        return vec![warning(format!(
            "group {group_id} title tab {tab_uuid} has unresolved rectangular geometry; title-tab layout is unverified"
        ))];
    };
    let mut diagnostics = Vec::new();
    if !rectangles_share_edge(frame_bounds, tab_bounds) {
        diagnostics.push(error(format!(
            "group {group_id} title tab {tab_uuid} does not share a non-zero edge segment with frame {frame_uuid}"
        )));
    }
    match unique_text_box_bounds(facts, title_uuid) {
        Ok(Some(title_bounds)) => {
            if !rectangle_contains(tab_bounds, title_bounds) {
                diagnostics.push(error(format!(
                    "group {group_id} title text box {title_uuid} extends outside title tab {tab_uuid}"
                )));
            }
        }
        Ok(None) => diagnostics.push(warning(format!(
            "group {group_id} title text box {title_uuid} has unresolved bounds; title containment is unverified"
        ))),
        Err(message) => diagnostics.push(error(format!("group {group_id} {message}"))),
    }
    diagnostics
}

/// Resolves a unique, valid functional-frame rectangle by UUID.
///
/// # Arguments
///
/// * `facts` — Normalized schematic facts containing parsed top-level rectangles.
/// * `uuid` — UUID declared as the functional group's frame.
///
/// # Returns
///
/// The rectangle bounds, or a static explanation when the UUID is missing, ambiguous, or invalid.
///
/// # Errors
///
/// Returns an error when the UUID is missing, ambiguous, or has invalid geometry.
fn unique_frame_bounds(facts: &SchematicFacts, uuid: &str) -> Result<Rectangle, &'static str> {
    let mut matches = facts.frames.iter().filter(|frame| frame.uuid == uuid);
    let Some(frame) = matches.next() else {
        return Err("references a missing frame UUID");
    };
    if matches.next().is_some() {
        return Err("references an ambiguous frame UUID");
    }
    valid_rectangle(frame.bounds_mm).ok_or("references a frame with invalid or zero-area bounds")
}

/// Resolves a unique top-level rectangular graphic by its declared UUID.
///
/// # Arguments
///
/// * `facts` — Normalized schematic facts containing top-level graphic geometry.
/// * `uuid` — UUID declared as the group's title tab.
///
/// # Returns
///
/// The matching rectangular graphic, or a static explanation when missing or ambiguous.
///
/// # Errors
///
/// Returns an error when the title-tab UUID is missing or ambiguous.
fn unique_title_tab<'a>(
    facts: &'a SchematicFacts,
    uuid: &str,
) -> Result<&'a SchematicGraphicFact, &'static str> {
    let mut matches = facts
        .graphics
        .iter()
        .filter(|graphic| graphic.kind == "rectangle" && graphic.uuid.as_deref() == Some(uuid));
    let Some(tab) = matches.next() else {
        return Err("references a missing rectangular title-tab UUID");
    };
    if matches.next().is_some() {
        return Err("references an ambiguous rectangular title-tab UUID");
    }
    Ok(tab)
}

/// Resolves a unique text box and returns its axis-aligned rectangle when available.
///
/// # Arguments
///
/// * `facts` — Normalized schematic facts containing parsed text boxes.
/// * `uuid` — UUID declared as the group's title text box.
///
/// # Returns
///
/// The title text-box rectangle, None for unresolved geometry, or an error for a missing or ambiguous UUID.
///
/// # Errors
///
/// Returns an error when the title text-box UUID is missing or ambiguous.
fn unique_text_box_bounds(
    facts: &SchematicFacts,
    uuid: &str,
) -> Result<Option<Rectangle>, &'static str> {
    let mut matches = facts
        .text_boxes
        .iter()
        .filter(|text_box| text_box.uuid == uuid);
    let Some(text_box) = matches.next() else {
        return Err("references a missing title text-box UUID");
    };
    if matches.next().is_some() {
        return Err("references an ambiguous title text-box UUID");
    }
    Ok(text_box.bounds_mm.and_then(valid_rectangle))
}

/// Derives the raw axis-aligned rectangle from a four-corner sheet rectangle.
///
/// # Arguments
///
/// * `graphic` — Top-level rectangle graphic whose path points define the title tab.
///
/// # Returns
///
/// The four-corner rectangle when the path is a valid axis-aligned rectangle; otherwise None.
fn raw_rectangle_bounds(graphic: &SchematicGraphicFact) -> Option<Rectangle> {
    if !graphic.closed || graphic.points_mm.len() != FOUR_RECTANGLE_CORNERS {
        return None;
    }
    if graphic
        .points_mm
        .iter()
        .any(|(x, y)| !x.is_finite() || !y.is_finite())
    {
        return None;
    }
    let left = graphic
        .points_mm
        .iter()
        .map(|point| point.0)
        .fold(f64::INFINITY, f64::min);
    let top = graphic
        .points_mm
        .iter()
        .map(|point| point.1)
        .fold(f64::INFINITY, f64::min);
    let right = graphic
        .points_mm
        .iter()
        .map(|point| point.0)
        .fold(f64::NEG_INFINITY, f64::max);
    let bottom = graphic
        .points_mm
        .iter()
        .map(|point| point.1)
        .fold(f64::NEG_INFINITY, f64::max);
    let bounds = valid_rectangle((left, top, right, bottom))?;
    let expected = [
        (bounds.0, bounds.1),
        (bounds.2, bounds.1),
        (bounds.2, bounds.3),
        (bounds.0, bounds.3),
    ];
    expected
        .iter()
        .all(|corner| {
            graphic
                .points_mm
                .iter()
                .filter(|point| points_close(**point, *corner))
                .count()
                == 1
        })
        .then_some(bounds)
}

/// Validates finite positive-width and positive-height rectangle coordinates.
///
/// # Arguments
///
/// * `bounds` — Left, top, right, and bottom coordinates in schematic millimetres.
///
/// # Returns
///
/// The original rectangle when every coordinate is finite and both dimensions are positive.
fn valid_rectangle(bounds: Rectangle) -> Option<Rectangle> {
    let (left, top, right, bottom) = bounds;
    (left.is_finite()
        && top.is_finite()
        && right.is_finite()
        && bottom.is_finite()
        && right - left > GEOMETRY_TOLERANCE_MM
        && bottom - top > GEOMETRY_TOLERANCE_MM)
        .then_some(bounds)
}

/// Checks whether a frame and its title tab share a horizontal or vertical edge segment.
///
/// # Arguments
///
/// * `frame` — Functional-frame rectangle bounds.
/// * `tab` — Title-tab rectangle bounds.
///
/// # Returns
///
/// True when one tab edge coincides with a frame edge and overlaps it by more than the geometry tolerance.
fn rectangles_share_edge(frame: Rectangle, tab: Rectangle) -> bool {
    let horizontal_overlap = frame.2.min(tab.2) - frame.0.max(tab.0);
    let vertical_overlap = frame.3.min(tab.3) - frame.1.max(tab.1);
    let shares_horizontal = horizontal_overlap > GEOMETRY_TOLERANCE_MM
        && ((tab.3 - frame.1).abs() <= GEOMETRY_TOLERANCE_MM
            || (tab.1 - frame.3).abs() <= GEOMETRY_TOLERANCE_MM);
    let shares_vertical = vertical_overlap > GEOMETRY_TOLERANCE_MM
        && ((tab.2 - frame.0).abs() <= GEOMETRY_TOLERANCE_MM
            || (tab.0 - frame.2).abs() <= GEOMETRY_TOLERANCE_MM);
    shares_horizontal || shares_vertical
}

/// Checks whether a title rectangle is fully contained by its title-tab rectangle.
///
/// # Arguments
///
/// * `outer` — Title-tab rectangle.
/// * `inner` — Title text-box rectangle.
///
/// # Returns
///
/// True when all four inner edges remain inside the outer rectangle within the geometry tolerance.
fn rectangle_contains(outer: Rectangle, inner: Rectangle) -> bool {
    inner.0 >= outer.0 - GEOMETRY_TOLERANCE_MM
        && inner.1 >= outer.1 - GEOMETRY_TOLERANCE_MM
        && inner.2 <= outer.2 + GEOMETRY_TOLERANCE_MM
        && inner.3 <= outer.3 + GEOMETRY_TOLERANCE_MM
}

/// Checks whether two points match within the shared geometry tolerance on both axes.
///
/// # Arguments
///
/// * `first` — First schematic coordinate.
/// * `second` — Second schematic coordinate.
///
/// # Returns
///
/// True when X and Y coordinate differences are both within the tolerance.
fn points_close(first: (f64, f64), second: (f64, f64)) -> bool {
    (first.0 - second.0).abs() <= GEOMETRY_TOLERANCE_MM
        && (first.1 - second.1).abs() <= GEOMETRY_TOLERANCE_MM
}

/// Checks minimum Euclidean separation between distinct declared functional frames.
///
/// # Arguments
///
/// * `profile` — Profile containing per-group minimum frame-gap declarations.
/// * `facts` — Normalized schematic frame rectangles from the currently loaded sheet.
///
/// # Returns
///
/// SCH028 errors for invalid thresholds, unresolved frames, or frame pairs closer than their effective configured gap.
fn validate_minimum_frame_gaps(profile: &LintProfile, facts: &SchematicFacts) -> Vec<Diagnostic> {
    if !profile
        .groups
        .values()
        .any(|group| group.minimum_frame_gap_mm.is_some())
    {
        return Vec::new();
    }
    let mut diagnostics = Vec::new();
    let mut declared = Vec::new();
    for (group_id, group) in &profile.groups {
        let minimum_gap = match group.minimum_frame_gap_mm {
            Some(value) if value.is_finite() && value >= 0.0 => value,
            Some(_) => {
                diagnostics.push(error(format!(
                    "group {group_id} minimum_frame_gap_mm must be finite and non-negative"
                )));
                0.0
            }
            None => 0.0,
        };
        let Some(frame_uuid) = group.frame_uuid.as_deref() else {
            if group.minimum_frame_gap_mm.is_some() {
                diagnostics.push(error(format!(
                    "group {group_id} requests minimum_frame_gap_mm but has no frame_uuid"
                )));
            }
            continue;
        };
        match unique_frame_bounds(facts, frame_uuid) {
            Ok(bounds) => declared.push(DeclaredFrame {
                group_id: group_id.as_str(),
                uuid: frame_uuid,
                bounds,
                minimum_gap_mm: minimum_gap,
            }),
            Err(message) => diagnostics.push(error(format!("group {group_id} {message}"))),
        }
    }
    diagnostics.extend(compare_declared_frame_gaps(&declared));
    diagnostics
}

/// Compares each pair of distinct declared frames using the larger threshold.
///
/// # Arguments
///
/// * `declared` — Groups with valid minimum-gap declarations and resolved rectangular frames.
///
/// # Returns
///
/// SCH028 errors for distinct frame pairs that do not maintain the stricter configured gap.
fn compare_declared_frame_gaps(declared: &[DeclaredFrame<'_>]) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for (index, first) in declared.iter().enumerate() {
        for second in declared.iter().skip(index + 1) {
            if first.uuid == second.uuid {
                continue;
            }
            let required = first.minimum_gap_mm.max(second.minimum_gap_mm);
            let actual = rectangle_gap(first.bounds, second.bounds);
            if actual + GEOMETRY_TOLERANCE_MM < required {
                diagnostics.push(error(format!(
                    "group frames {} ({}) and {} ({}) are {actual:.2} mm apart; minimum is {required:.2} mm",
                    first.group_id, first.uuid, second.group_id, second.uuid
                )));
            }
        }
    }
    diagnostics
}

/// Calculates shortest Euclidean edge-to-edge distance between axis-aligned rectangles.
///
/// # Arguments
///
/// * `first` — First frame rectangle.
/// * `second` — Second frame rectangle.
///
/// # Returns
///
/// The Euclidean distance between disjoint rectangles, or zero when they overlap or touch.
fn rectangle_gap(first: Rectangle, second: Rectangle) -> f64 {
    let horizontal = (first.0 - second.2).max(second.0 - first.2).max(0.0);
    let vertical = (first.1 - second.3).max(second.1 - first.3).max(0.0);
    horizontal.hypot(vertical)
}

/// Creates an SCH028 error diagnostic.
///
/// # Arguments
///
/// * `message` — Actionable explanation of the frame-title contract violation.
///
/// # Returns
///
/// An error diagnostic associated with SCH028.
fn error(message: impl Into<String>) -> Diagnostic {
    Diagnostic::for_builtin_rule(BuiltinRuleId::Sch028, Severity::Error, message)
}

/// Creates an SCH028 warning for geometry that cannot be verified.
///
/// # Arguments
///
/// * `message` — Explanation of the unresolved frame-title geometry.
///
/// # Returns
///
/// A warning diagnostic associated with SCH028.
fn warning(message: impl Into<String>) -> Diagnostic {
    Diagnostic::for_builtin_rule(BuiltinRuleId::Sch028, Severity::Warning, message)
}

impl<'a> Rule<ProfileRuleContext<'a>> for Sch028 {
    /// Returns the stable SCH028 identifier.
    ///
    /// # Returns
    ///
    /// The identifier associated with functional-frame title layout.
    fn id(&self) -> BuiltinRuleId {
        BuiltinRuleId::Sch028
    }

    /// Evaluates the configured title-tab and frame-spacing contracts.
    ///
    /// # Arguments
    ///
    /// * `context` — Group profile declarations and normalized schematic geometry.
    ///
    /// # Returns
    ///
    /// Diagnostics for title tabs detached from their frames, titles outside tabs, or insufficient frame spacing.
    fn check(&self, context: &ProfileRuleContext<'a>) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for (group_id, group) in &context.profile.groups {
            diagnostics.extend(validate_group_title_layout(group_id, group, context.facts));
        }
        diagnostics.extend(validate_minimum_frame_gaps(context.profile, context.facts));
        diagnostics
    }
}

type Rectangle = (f64, f64, f64, f64);

/// Minimum number of corners in a rectangular tab path.
const FOUR_RECTANGLE_CORNERS: usize = 4;

struct DeclaredFrame<'a> {
    group_id: &'a str,
    uuid: &'a str,
    bounds: Rectangle,
    minimum_gap_mm: f64,
}

#[cfg(test)]
/// Tests frame title-tab attachment, title containment, and minimum group-frame spacing.
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::{
        DeclaredFrame, compare_declared_frame_gaps, rectangle_gap, validate_group_title_layout,
        validate_minimum_frame_gaps,
    };
    use crate::generic::SchematicGraphicFact;
    use crate::{GroupIntent, LintProfile, SchematicFacts, SchematicFrameFact, TextBoxFact};
    use kicad_clippy_core::Severity;

    /// Creates normalized facts with one frame, one rectangular title tab, and one title text box.
    ///
    /// # Arguments
    ///
    /// * `frame` — Frame bounds in left/top/right/bottom order.
    /// * `tab` — Title-tab bounds in left/top/right/bottom order.
    /// * `title` — Optional title-text-box bounds in left/top/right/bottom order.
    ///
    /// # Returns
    ///
    /// Normalized schematic facts containing the requested UUID-linked title-layout geometry.
    fn title_facts(
        frame: (f64, f64, f64, f64),
        tab: (f64, f64, f64, f64),
        title: Option<(f64, f64, f64, f64)>,
    ) -> SchematicFacts {
        let mut facts = SchematicFacts::default();
        facts.frames.push(SchematicFrameFact {
            uuid: "frame-1".to_owned(),
            bounds_mm: frame,
        });
        facts.graphics.push(SchematicGraphicFact {
            kind: "rectangle".to_owned(),
            uuid: Some("tab-1".to_owned()),
            bounds_mm: None,
            points_mm: vec![
                (tab.0, tab.1),
                (tab.2, tab.1),
                (tab.2, tab.3),
                (tab.0, tab.3),
            ],
            closed: true,
            circle_mm: None,
            arc_mm: None,
            bezier_mm: None,
            stroke_width_mm: 0.0,
        });
        facts.text_boxes.push(TextBoxFact {
            uuid: "title-1".to_owned(),
            text: "Power input".to_owned(),
            bounds_mm: title,
        });
        facts
    }

    /// Creates a group with its frame, title tab, and title text box explicitly linked by UUID.
    ///
    /// # Returns
    ///
    /// A group intent containing one complete frame-title contract.
    fn title_group() -> GroupIntent {
        GroupIntent {
            frame_uuid: Some("frame-1".to_owned()),
            frame_title_tab_uuid: Some("tab-1".to_owned()),
            frame_title_text_box_uuid: Some("title-1".to_owned()),
            ..GroupIntent::default()
        }
    }

    /// Accepts a title tab attached to a frame with its text box fully contained.
    #[test]
    fn accepts_attached_tab_and_contained_title() {
        let facts = title_facts(
            (10.0, 10.0, 30.0, 30.0),
            (14.0, 6.0, 20.0, 10.0),
            Some((15.0, 7.0, 19.0, 9.0)),
        );
        assert!(validate_group_title_layout("power", &title_group(), &facts).is_empty());
    }

    /// Rejects a title tab that is near the frame but does not share an edge.
    #[test]
    fn rejects_detached_title_tab() {
        let facts = title_facts(
            (10.0, 10.0, 30.0, 30.0),
            (14.0, 5.0, 20.0, 9.0),
            Some((15.0, 6.0, 19.0, 8.0)),
        );
        assert!(
            validate_group_title_layout("power", &title_group(), &facts)
                .iter()
                .any(|item| item.message.contains("does not share"))
        );
    }

    /// Rejects a title text box that extends beyond the tab's rectangular bounds.
    #[test]
    fn rejects_title_box_outside_tab() {
        let facts = title_facts(
            (10.0, 10.0, 30.0, 30.0),
            (14.0, 6.0, 20.0, 10.0),
            Some((15.0, 7.0, 21.0, 9.0)),
        );
        assert!(
            validate_group_title_layout("power", &title_group(), &facts)
                .iter()
                .any(|item| item.message.contains("extends outside"))
        );
    }

    /// Reports unresolved title geometry as unverified rather than treating it as valid.
    #[test]
    fn unresolved_title_box_geometry_is_unverified() {
        let facts = title_facts((10.0, 10.0, 30.0, 30.0), (14.0, 6.0, 20.0, 10.0), None);
        assert!(
            validate_group_title_layout("power", &title_group(), &facts)
                .iter()
                .any(|item| item.severity == Severity::Warning
                    && item.message.contains("unverified"))
        );
    }

    /// Requires the title-tab and title-text-box UUIDs to be provided together.
    #[test]
    fn title_tab_and_text_box_uuids_are_a_paired_contract() {
        let mut group = title_group();
        group.frame_title_text_box_uuid = None;
        let facts = title_facts(
            (10.0, 10.0, 30.0, 30.0),
            (14.0, 6.0, 20.0, 10.0),
            Some((15.0, 7.0, 19.0, 9.0)),
        );
        assert!(
            validate_group_title_layout("power", &group, &facts)
                .iter()
                .any(|item| item.message.contains("together"))
        );
    }

    /// Accepts frames whose Euclidean edge gap exactly equals the effective minimum.
    #[test]
    fn accepts_frame_gap_at_the_threshold() {
        let declared = [
            DeclaredFrame {
                group_id: "first",
                uuid: "frame-a",
                bounds: (0.0, 0.0, 10.0, 10.0),
                minimum_gap_mm: 5.0,
            },
            DeclaredFrame {
                group_id: "second",
                uuid: "frame-b",
                bounds: (13.0, 14.0, 20.0, 20.0),
                minimum_gap_mm: 5.0,
            },
        ];
        assert!(compare_declared_frame_gaps(&declared).is_empty());
        assert!((rectangle_gap(declared[0].bounds, declared[1].bounds) - 5.0).abs() < 0.000001);
    }

    /// Rejects distinct frames whose diagonal Euclidean gap is below the configured minimum.
    #[test]
    fn rejects_frame_pair_below_minimum_euclidean_gap() {
        let declared = [
            DeclaredFrame {
                group_id: "first",
                uuid: "frame-a",
                bounds: (0.0, 0.0, 10.0, 10.0),
                minimum_gap_mm: 5.0,
            },
            DeclaredFrame {
                group_id: "second",
                uuid: "frame-b",
                bounds: (13.0, 14.0, 20.0, 20.0),
                minimum_gap_mm: 5.1,
            },
        ];
        let diagnostics = compare_declared_frame_gaps(&declared);
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("minimum is 5.10 mm"));
    }

    /// Applies one group's minimum gap to every other profile-declared frame.
    #[test]
    fn configured_frame_gap_applies_to_peer_without_its_own_threshold() {
        let mut profile: LintProfile =
            serde_json::from_str(r#"{"schema_version":1}"#).expect("valid profile");
        profile.groups.insert(
            "first".to_owned(),
            GroupIntent {
                frame_uuid: Some("frame-1".to_owned()),
                minimum_frame_gap_mm: Some(5.0),
                ..GroupIntent::default()
            },
        );
        profile.groups.insert(
            "second".to_owned(),
            GroupIntent {
                frame_uuid: Some("frame-2".to_owned()),
                ..GroupIntent::default()
            },
        );
        let mut facts = title_facts(
            (0.0, 0.0, 10.0, 10.0),
            (2.0, -4.0, 8.0, 0.0),
            Some((3.0, -3.0, 7.0, -1.0)),
        );
        facts.frames.push(SchematicFrameFact {
            uuid: "frame-2".to_owned(),
            bounds_mm: (12.0, 0.0, 20.0, 10.0),
        });

        let diagnostics = validate_minimum_frame_gaps(&profile, &facts);
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("minimum is 5.00 mm"));
    }

    /// Accepts groups intentionally referencing one shared frame UUID.
    #[test]
    fn shared_frame_uuid_is_not_treated_as_two_distinct_frames() {
        let mut profile: LintProfile =
            serde_json::from_str(r#"{"schema_version":1}"#).expect("valid profile");
        profile.groups.insert(
            "first".to_owned(),
            GroupIntent {
                frame_uuid: Some("frame-1".to_owned()),
                minimum_frame_gap_mm: Some(5.0),
                ..GroupIntent::default()
            },
        );
        profile.groups.insert(
            "second".to_owned(),
            GroupIntent {
                frame_uuid: Some("frame-1".to_owned()),
                minimum_frame_gap_mm: Some(6.0),
                ..GroupIntent::default()
            },
        );
        let facts = title_facts(
            (10.0, 10.0, 30.0, 30.0),
            (14.0, 6.0, 20.0, 10.0),
            Some((15.0, 7.0, 19.0, 9.0)),
        );
        assert!(validate_minimum_frame_gaps(&profile, &facts).is_empty());
    }
}
