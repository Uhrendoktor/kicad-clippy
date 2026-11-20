//! Additional profile-driven checks that complement native ERC.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::core::{BuiltinRuleId, Diagnostic, SchematicLocation, Severity};
use crate::parser::SchematicDocument;
use crate::rules::{HALF_FACTOR, LintProfile, SchematicFacts};
use kiutils_sexpr::{Atom, Node};

/// Coordinates within this distance are treated as the same schematic anchor.
const GEOMETRY_TOLERANCE_MM: f64 = 0.001;
/// Component metadata key containing comma-separated pin numbers that require explicit no-connect flags.
const REQUIRED_NO_CONNECT_PINS_ATTRIBUTE: &str = "required_no_connect_pins";

/// Runs supplemental opt-in profile checks using normalized schematic facts.
///
/// # Arguments
///
/// * `profile` — Project-level contracts that opt into extra presentation and connectivity checks.
/// * `facts` — Normalized schematic objects, frames, pins, and markers collected once for this lint run.
///
/// # Returns
///
/// Diagnostics for every violated profile extension; an empty vector means all declared extensions passed.
pub fn validate_profile_extensions(
    profile: &LintProfile,
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    validate_profile_extensions_inner(profile, facts, None)
}

/// Runs supplemental profile checks with optional source-file access for hierarchy-wide policy.
///
/// # Arguments
///
/// * `profile` — Project-level contracts that opt into extra presentation and connectivity checks.
/// * `facts` — Normalized schematic objects collected for the root sheet.
/// * `document` — Parsed source document, required only by hierarchy-wide label policy.
///
/// # Returns
///
/// Diagnostics for violated profile extensions and unverified hierarchy evidence.
pub fn validate_profile_extensions_with_document(
    profile: &LintProfile,
    facts: &SchematicFacts,
    document: &SchematicDocument,
) -> Vec<Diagnostic> {
    validate_profile_extensions_inner(profile, facts, Some(document))
}

/// Runs all supplemental profile checks with optional path-based evidence for project-wide scope.
///
/// # Arguments
///
/// * `profile` — Project-level contracts that opt into extra presentation and connectivity checks.
/// * `facts` — Normalized schematic objects collected for the root sheet.
/// * `document` — Optional parsed root document used by hierarchy-wide label policies.
///
/// # Returns
///
/// Diagnostics for every violated profile extension and every unverified hierarchy boundary.
fn validate_profile_extensions_inner(
    profile: &LintProfile,
    facts: &SchematicFacts,
    document: Option<&SchematicDocument>,
) -> Vec<Diagnostic> {
    let mut diagnostics = validate_declared_frame_overlaps(profile, facts);
    diagnostics.extend(validate_required_no_connect_pins(profile, facts));
    diagnostics.extend(validate_required_title_block_fields(profile, facts));
    diagnostics.extend(validate_label_scope_policy_with_document(
        profile, facts, document,
    ));
    diagnostics
}

/// Enforces explicit project policy for global-label usage and required label scopes.
///
/// This check evaluates only labels present in the currently loaded schematic document.
/// It does not resolve hierarchical sheet pins across child files or replace `KiCad`'s
/// native global/hierarchical connectivity and ERC validation.
///
/// # Arguments
///
/// * `profile` — Project profile containing the optional label-scope policy.
/// * `facts` — Normalized label names and native `KiCad` label-kind facts from the loaded sheet.
///
/// # Returns
///
/// SCH027 configuration errors, global-label allow/deny findings, missing required labels,
/// and required-scope mismatches on the currently loaded sheet.
fn validate_label_scope_policy(profile: &LintProfile, facts: &SchematicFacts) -> Vec<Diagnostic> {
    let Some(policy) = profile.label_scope_policy.as_ref() else {
        return Vec::new();
    };
    let configuration = validate_label_scope_configuration(policy);
    if !configuration.is_empty() {
        return configuration;
    }
    if policy.project_wide {
        return validate_project_wide_label_scope_policy(policy, None);
    }

    let mut diagnostics = validate_global_label_policy(policy, facts);
    diagnostics.extend(validate_required_label_scopes(policy, facts));
    diagnostics
}

/// Applies current-sheet or project-hierarchy label policy using the requested verification scope.
///
/// # Arguments
///
/// * `profile` — Project profile containing the optional label-scope policy.
/// * `facts` — Normalized label objects from the root schematic.
/// * `document` — Parsed root schematic when its filesystem path is available for hierarchy traversal.
///
/// # Returns
///
/// SCH027 configuration, scope, global-label, or unverified hierarchy diagnostics.
fn validate_label_scope_policy_with_document(
    profile: &LintProfile,
    facts: &SchematicFacts,
    document: Option<&SchematicDocument>,
) -> Vec<Diagnostic> {
    let Some(policy) = profile.label_scope_policy.as_ref() else {
        return Vec::new();
    };
    if !policy.project_wide {
        return validate_label_scope_policy(profile, facts);
    }
    let configuration = validate_label_scope_configuration(policy);
    if !configuration.is_empty() {
        return configuration;
    }
    validate_project_wide_label_scope_policy(policy, document)
}

/// Validates label-scope names and rejects contradictory policy declarations.
///
/// # Arguments
///
/// * `policy` — Typed global-label allow/deny sets and required name-to-scope declarations.
///
/// # Returns
///
/// SCH027 configuration errors; an empty vector means the label policy is internally consistent.
fn validate_label_scope_configuration(policy: &super::LabelScopePolicy) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let valid_name = |name: &str| !name.trim().is_empty() && name.trim() == name;

    if let Some(allowed) = &policy.allowed_global_labels {
        for name in allowed {
            if !valid_name(name) {
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch027,
                    Severity::Error,
                    format!("label_scope_policy.allowed_global_labels contains invalid label name {name:?}"),
                ));
            }
        }
    }
    for name in &policy.forbidden_global_labels {
        if !valid_name(name) {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch027,
                Severity::Error,
                format!("label_scope_policy.forbidden_global_labels contains invalid label name {name:?}"),
            ));
        }
    }
    for (name, scope) in &policy.required_label_scopes {
        if !valid_name(name) {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch027,
                Severity::Error,
                format!(
                    "label_scope_policy.required_label_scopes contains invalid label name {name:?}"
                ),
            ));
        }
        if *scope == super::LabelScopeKind::Global && policy.forbidden_global_labels.contains(name)
        {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch027,
                Severity::Error,
                format!("label {name:?} is required to be global but is also listed in forbidden_global_labels"),
            ));
        }
        if *scope == super::LabelScopeKind::Global
            && policy
                .allowed_global_labels
                .as_ref()
                .is_some_and(|allowed| !allowed.contains(name))
        {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch027,
                Severity::Error,
                format!("label {name:?} is required to be global but is absent from allowed_global_labels"),
            ));
        }
    }
    if let Some(allowed) = &policy.allowed_global_labels {
        for name in allowed.intersection(&policy.forbidden_global_labels) {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch027,
                Severity::Error,
                format!("global label {name:?} is listed in both allowed_global_labels and forbidden_global_labels"),
            ));
        }
    }
    diagnostics
}

/// Checks every global label against the optional allow-list and explicit deny-list.
///
/// # Arguments
///
/// * `policy` — Validated policy containing global-label restrictions.
/// * `facts` — Label facts collected from the currently loaded schematic.
///
/// # Returns
///
/// SCH027 findings for global labels that are forbidden or missing from a configured allow-list.
fn validate_global_label_policy(
    policy: &super::LabelScopePolicy,
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for label in facts
        .labels
        .iter()
        .filter(|label| label.kind == "global_label")
    {
        if policy.forbidden_global_labels.contains(&label.name) {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch027,
                Severity::Error,
                format!(
                    "global label {:?} is forbidden by label_scope_policy on the loaded sheet",
                    label.name
                ),
            ));
        } else if policy
            .allowed_global_labels
            .as_ref()
            .is_some_and(|allowed| !allowed.contains(&label.name))
        {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch027,
                Severity::Error,
                format!(
                    "global label {:?} is not listed in allowed_global_labels",
                    label.name
                ),
            ));
        }
    }
    diagnostics
}

/// Checks that every named label exists on the loaded sheet and uses its required scope.
///
/// # Arguments
///
/// * `policy` — Label-scope settings whose global allow/deny sets and required name-to-scope entries passed configuration validation.
/// * `facts` — Native local, global and hierarchical label facts from the loaded sheet.
///
/// # Returns
///
/// SCH027 findings for missing required label names and labels whose actual scope differs from policy.
fn validate_required_label_scopes(
    policy: &super::LabelScopePolicy,
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for (name, expected_scope) in &policy.required_label_scopes {
        let mut found = false;
        for label in facts.labels.iter().filter(|label| label.name == *name) {
            found = true;
            let actual_scope = match label.kind.as_str() {
                "label" => Some(super::LabelScopeKind::Local),
                "global_label" => Some(super::LabelScopeKind::Global),
                "hierarchical_label" => Some(super::LabelScopeKind::Hierarchical),
                _ => None,
            };
            if actual_scope != Some(*expected_scope) {
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch027,
                    Severity::Error,
                    format!(
                        "label {name:?} has scope {actual_scope:?}, but label_scope_policy requires {expected_scope:?} on the loaded sheet"
                    ),
                ));
            }
        }
        if !found {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch027,
                Severity::Error,
                format!("required label {name:?} with scope {expected_scope:?} is missing from the loaded sheet"),
            ));
        }
    }
    diagnostics
}

/// Native `KiCad` title-block field keys that SCH023 can require on a loaded sheet.
const SUPPORTED_TITLE_BLOCK_FIELDS: [&str; 13] = [
    "title", "date", "rev", "company", "comment1", "comment2", "comment3", "comment4", "comment5",
    "comment6", "comment7", "comment8", "comment9",
];

/// One label occurrence with the source sheet from which its scope was read.
#[derive(Clone, Debug)]
struct ProjectLabelOccurrence {
    /// Serialized label name.
    name: String,
    /// Native `KiCad` label kind.
    kind: String,
    /// Source schematic file containing the label.
    source: PathBuf,
}

/// Traverses the root schematic and its referenced child sheets, collecting labels once per canonical file.
///
/// # Arguments
///
/// * `document` — Already parsed root schematic whose retained path anchors project traversal.
/// * `path` — Canonical filesystem path of the document being visited.
/// * `visited` — Canonical file paths already loaded, used for cycle and repeated-reference protection.
/// * `occurrences` — Accumulated label occurrences with source paths.
/// * `diagnostics` — Unverified warnings for missing, unreadable, or invalid child sheets.
///
/// # Returns
///
/// No value; readable sheet labels and hierarchy-verification warnings are appended to the supplied collections.
fn collect_project_label_occurrences(
    document: &SchematicDocument,
    path: &Path,
    visited: &mut BTreeSet<PathBuf>,
    occurrences: &mut Vec<ProjectLabelOccurrence>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if !visited.insert(path.to_path_buf()) {
        return;
    }

    occurrences.extend(
        SchematicFacts::from_document(document)
            .labels
            .into_iter()
            .map(|label| ProjectLabelOccurrence {
                name: label.name,
                kind: label.kind,
                source: path.to_path_buf(),
            }),
    );

    let mut sheetfiles = BTreeSet::new();
    if let Some(root) = document.cst().nodes.first() {
        collect_project_sheetfile_values(root, &mut sheetfiles);
    }

    for sheetfile in sheetfiles {
        let candidate = Path::new(&sheetfile);
        let child_path = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            path.parent()
                .unwrap_or_else(|| Path::new("."))
                .join(candidate)
        };
        let child_path = match fs::canonicalize(&child_path) {
            Ok(path) => path,
            Err(error) => {
                diagnostics.push(project_label_hierarchy_warning(
                    path,
                    &format!("referenced child sheet {sheetfile:?} could not be resolved: {error}"),
                ));
                continue;
            }
        };
        if visited.contains(&child_path) {
            continue;
        }
        match SchematicDocument::from_path(&child_path) {
            Ok(child_document) => collect_project_label_occurrences(
                &child_document,
                &child_path,
                visited,
                occurrences,
                diagnostics,
            ),
            Err(error) => diagnostics.push(project_label_hierarchy_warning(
                &child_path,
                &format!("referenced child sheet could not be parsed: {error}"),
            )),
        }
    }
}

/// Collects native `Sheetfile` property values from all nested sheet objects in a schematic CST.
///
/// # Arguments
///
/// * `node` — Current S-expression node being recursively examined.
/// * `sheetfiles` — Set receiving each distinct non-empty referenced child filename.
///
/// # Returns
///
/// No value; discovered sheet paths are appended to the deterministic set.
fn collect_project_sheetfile_values(node: &Node, sheetfiles: &mut BTreeSet<String>) {
    let Node::List { items, .. } = node else {
        return;
    };
    if items.first().and_then(project_atom_value) == Some("sheet") {
        for property in project_children(node, "property") {
            let Some(property_items) = project_list_items(property) else {
                continue;
            };
            if property_items.get(1).and_then(project_atom_value) == Some("Sheetfile") {
                if let Some(value) = property_items.get(2).and_then(project_atom_value) {
                    if !value.trim().is_empty() {
                        sheetfiles.insert(value.to_owned());
                    }
                }
            }
        }
    }
    for child in items.iter().skip(1) {
        collect_project_sheetfile_values(child, sheetfiles);
    }
}

/// Creates a hierarchy-specific warning that leaves project-wide label verification explicitly incomplete.
///
/// # Arguments
///
/// * `source` — Schematic path whose hierarchy reference could not be verified.
/// * `reason` — Human-readable explanation of the unreadable or unresolved hierarchy member.
///
/// # Returns
///
/// A non-blocking SCH027 warning associated with the affected source path.
fn project_label_hierarchy_warning(source: &Path, reason: &str) -> Diagnostic {
    let mut diagnostic = Diagnostic::for_builtin_rule(
        BuiltinRuleId::Sch027,
        Severity::Warning,
        format!("project-wide label-scope verification is incomplete: {reason}"),
    );
    diagnostic.source = Some(source.to_path_buf());
    diagnostic
}

/// Validates global-label restrictions and required scopes across every readable project sheet.
///
/// # Arguments
///
/// * `policy` — Validated label policy configured for project-wide operation.
/// * `document` — Parsed root schematic with a real filesystem path and readable referenced child sheets.
///
/// # Returns
///
/// SCH027 findings for project-wide label restrictions, required-scope violations, and incomplete hierarchy evidence.
fn validate_project_wide_label_scope_policy(
    policy: &super::LabelScopePolicy,
    document: Option<&SchematicDocument>,
) -> Vec<Diagnostic> {
    let Some(document) = document else {
        return vec![project_label_hierarchy_warning(
            Path::new("<in-memory schematic>"),
            "project_wide requires a path-based root schematic",
        )];
    };
    let root_path = document.source().path();
    let canonical_root = match fs::canonicalize(root_path) {
        Ok(path) => path,
        Err(error) => {
            return vec![project_label_hierarchy_warning(
                root_path,
                &format!("root schematic path could not be resolved: {error}"),
            )];
        }
    };

    let mut occurrences = Vec::new();
    let mut diagnostics = Vec::new();
    collect_project_label_occurrences(
        document,
        &canonical_root,
        &mut BTreeSet::new(),
        &mut occurrences,
        &mut diagnostics,
    );
    let hierarchy_complete = !diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Warning);
    diagnostics.extend(validate_project_global_labels(policy, &occurrences));
    diagnostics.extend(validate_project_required_label_scopes(
        policy,
        &canonical_root,
        &occurrences,
        hierarchy_complete,
    ));
    diagnostics
}

/// Applies the global-label allow-list and deny-list to all readable project sheets.
///
/// # Arguments
///
/// * `policy` — Validated project-wide global-label allow/deny policy.
/// * `occurrences` — Label objects collected from the root schematic and readable children.
///
/// # Returns
///
/// SCH027 errors for every global-label occurrence that violates the declared allow/deny policy.
fn validate_project_global_labels(
    policy: &super::LabelScopePolicy,
    occurrences: &[ProjectLabelOccurrence],
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for occurrence in occurrences
        .iter()
        .filter(|occurrence| occurrence.kind == "global_label")
    {
        let violation = if policy.forbidden_global_labels.contains(&occurrence.name) {
            Some(format!(
                "global label {:?} is forbidden by project-wide label_scope_policy",
                occurrence.name
            ))
        } else if policy
            .allowed_global_labels
            .as_ref()
            .is_some_and(|allowed| !allowed.contains(&occurrence.name))
        {
            Some(format!(
                "global label {:?} is not listed in project-wide allowed_global_labels",
                occurrence.name
            ))
        } else {
            None
        };
        if let Some(message) = violation {
            let mut diagnostic =
                Diagnostic::for_builtin_rule(BuiltinRuleId::Sch027, Severity::Error, message);
            diagnostic.source = Some(occurrence.source.clone());
            diagnostics.push(diagnostic);
        }
    }
    diagnostics
}

/// Checks required label names and scopes over all readable project-label occurrences.
///
/// # Arguments
///
/// * `policy` — Validated project-wide required-label-scope declarations.
/// * `root_path` — Canonical root file used to locate labels missing from the hierarchy.
/// * `occurrences` — Label objects collected from every readable root/child schematic file.
/// * `hierarchy_complete` — Whether every referenced child sheet could be resolved and parsed.
///
/// # Returns
///
/// SCH027 scope errors and missing-label errors, or warnings when unreadable child sheets prevent a complete absence check.
fn validate_project_required_label_scopes(
    policy: &super::LabelScopePolicy,
    root_path: &Path,
    occurrences: &[ProjectLabelOccurrence],
    hierarchy_complete: bool,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for (name, expected_scope) in &policy.required_label_scopes {
        let matching: Vec<_> = occurrences
            .iter()
            .filter(|occurrence| occurrence.name == *name)
            .collect();
        if matching.is_empty() {
            let severity = if hierarchy_complete {
                Severity::Error
            } else {
                Severity::Warning
            };
            let message = if hierarchy_complete {
                format!(
                    "required label {name:?} with scope {expected_scope:?} is missing from the project hierarchy"
                )
            } else {
                format!(
                    "required label {name:?} with scope {expected_scope:?} is unverified because part of the project hierarchy could not be read"
                )
            };
            let mut diagnostic =
                Diagnostic::for_builtin_rule(BuiltinRuleId::Sch027, severity, message);
            diagnostic.source = Some(root_path.to_path_buf());
            diagnostics.push(diagnostic);
            continue;
        }
        diagnostics.extend(matching.into_iter().filter_map(|occurrence| {
            let actual_scope = match occurrence.kind.as_str() {
                "label" => Some(super::LabelScopeKind::Local),
                "global_label" => Some(super::LabelScopeKind::Global),
                "hierarchical_label" => Some(super::LabelScopeKind::Hierarchical),
                _ => None,
            };
            (actual_scope != Some(*expected_scope)).then(|| {
                let mut diagnostic = Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch027,
                    Severity::Error,
                    format!(
                        "label {name:?} in project sheet has scope {actual_scope:?}, but project-wide label_scope_policy requires {expected_scope:?}"
                    ),
                );
                diagnostic.source = Some(occurrence.source.clone());
                diagnostic
            })
        }));
    }
    diagnostics
}

/// Returns the symbolic or quoted value of an S-expression atom.
///
/// # Arguments
///
/// * `node` — S-expression node that may contain a scalar atom.
///
/// # Returns
///
/// The contained symbol or quoted string, or `None` for a nested list.
fn project_atom_value(node: &Node) -> Option<&str> {
    let Node::Atom { atom, .. } = node else {
        return None;
    };
    match atom {
        Atom::Symbol(value) | Atom::Quoted(value) => Some(value),
    }
}

/// Iterates over direct child lists with the requested symbolic head.
///
/// # Arguments
///
/// * `node` — Parent S-expression whose immediate list children are searched.
/// * `name` — Symbolic list-head name identifying the requested expressions.
///
/// # Returns
///
/// An iterator over the direct child lists matching `name`.
fn project_children<'a>(node: &'a Node, name: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
    project_list_items(node)
        .into_iter()
        .flatten()
        .skip(1)
        .filter(move |child| project_head(child) == Some(name))
}

/// Returns a list node's items or `None` when the node is not a list.
///
/// # Arguments
///
/// * `node` — S-expression node whose list elements are requested.
///
/// # Returns
///
/// A borrowed list slice, or `None` for an atom.
fn project_list_items(node: &Node) -> Option<&[Node]> {
    let Node::List { items, .. } = node else {
        return None;
    };
    Some(items)
}

/// Returns a symbolic list head.
///
/// # Arguments
///
/// * `node` — S-expression node whose first item may identify the expression kind.
///
/// # Returns
///
/// The symbolic head, or `None` if the node is not a symbolic list.
fn project_head(node: &Node) -> Option<&str> {
    let Node::List { items, .. } = node else {
        return None;
    };
    items.first().and_then(project_atom_value)
}

/// Validates only the sheet title-block fields explicitly required by the project profile./// Validates only the sheet title-block fields explicitly required by the project profile.
///
/// # Arguments
///
/// * `profile` — Profile declaring the required title-block field keys.
/// * `facts` — Normalized title-block field values extracted from the current schematic document.
///
/// # Returns
///
/// SCH023 errors for unsupported keys, absent fields, and fields whose trimmed value is empty.
fn validate_required_title_block_fields(
    profile: &LintProfile,
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for field in &profile.required_title_block_fields {
        if !SUPPORTED_TITLE_BLOCK_FIELDS.contains(&field.as_str()) {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch023,
                Severity::Error,
                format!("required_title_block_fields contains unsupported field {field:?}"),
            ));
            continue;
        }
        match facts.title_block_fields.get(field) {
            Some(value) if !value.trim().is_empty() => {}
            Some(_) => diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch023,
                Severity::Error,
                format!("sheet title-block field {field:?} is present but empty"),
            )),
            None => diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch023,
                Severity::Error,
                format!("required sheet title-block field {field:?} is missing"),
            )),
        }
    }
    diagnostics
}

/// Rejects overlapping frames when two different profile groups claim them.
///
/// Only frames explicitly referenced by a group's `frame_uuid` are checked;
/// arbitrary schematic rectangles are not assumed to be group boundaries.
/// Edge contact is allowed because adjacent frames may share an alignment
/// line without occupying the same area. Two groups reusing the exact same
/// frame UUID are treated as sharing one frame, not as overlapping frames.
///
/// # Arguments
///
/// * `profile` — Functional-group declarations identifying which rectangles act as group frames.
/// * `facts` — Resolved schematic frame rectangles used for geometric intersection tests.
///
/// # Returns
///
/// SCH009 diagnostics for overlapping declared frame interiors; edge contact and shared UUIDs are accepted.
fn validate_declared_frame_overlaps(
    profile: &LintProfile,
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    let declared_frames: Vec<_> = profile
        .groups
        .iter()
        .filter_map(|(group_id, group)| {
            let frame_uuid = group.frame_uuid.as_deref()?;
            let frame = facts.frames.iter().find(|frame| frame.uuid == frame_uuid)?;
            Some((group_id.as_str(), frame_uuid, frame.bounds_mm))
        })
        .collect();

    let mut diagnostics = Vec::new();
    for (index, first) in declared_frames.iter().enumerate() {
        for second in declared_frames.iter().skip(index + 1) {
            let (first_group, first_uuid, first_bounds) = *first;
            let (second_group, second_uuid, second_bounds) = *second;
            if first_uuid == second_uuid {
                continue;
            }

            let (first_left, first_top, first_right, first_bottom) = first_bounds;
            let (second_left, second_top, second_right, second_bottom) = second_bounds;
            let left = first_left.max(second_left);
            let top = first_top.max(second_top);
            let right = first_right.min(second_right);
            let bottom = first_bottom.min(second_bottom);
            let overlap_width = right - left;
            let overlap_height = bottom - top;

            if overlap_width <= GEOMETRY_TOLERANCE_MM || overlap_height <= GEOMETRY_TOLERANCE_MM {
                continue;
            }

            let mut diagnostic = Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch009,
                Severity::Error,
                format!(
                    "functional frames for groups {first_group:?} ({first_uuid}) and {second_group:?} ({second_uuid}) overlap by {overlap_width:.2} x {overlap_height:.2} mm"
                ),
            );
            diagnostic.explanation = Some(
                "Functional frames are visual boundaries for separate circuit groups. Overlapping frames make group ownership and the intended page hierarchy ambiguous."
                    .to_owned(),
            );
            diagnostic.help = Some(
                "Move or resize the declared frames so their interiors no longer overlap; preserve any intentional shared boundary as edge contact."
                    .to_owned(),
            );
            diagnostic.schematic_location = Some(SchematicLocation {
                x_mm: (left + right) * HALF_FACTOR,
                y_mm: (top + bottom) * HALF_FACTOR,
            });
            diagnostics.push(diagnostic);
        }
    }

    diagnostics
}

/// Enforces profile-declared no-connect markers for selected placed-symbol pins.
///
/// The `required_no_connect_pins` attribute is a comma-separated list of pin
/// numbers. Each pin must resolve through the embedded symbol definition and
/// the marker must coincide with its transformed sheet coordinate. A pin whose
/// geometry cannot be resolved is reported as unverified rather than silently
/// treated as correctly marked.
///
/// # Arguments
///
/// * `profile` — Component intent declarations containing required no-connect pin numbers.
/// * `facts` — Embedded placed-symbol pin coordinates and actual no-connect marker locations.
///
/// # Returns
///
/// SCH015 diagnostics for missing, empty, or unverifiable required no-connect marker declarations.
fn validate_required_no_connect_pins(
    profile: &LintProfile,
    facts: &SchematicFacts,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    for (reference, intent) in &profile.components {
        let Some(raw_pins) = intent.attributes.get(REQUIRED_NO_CONNECT_PINS_ATTRIBUTE) else {
            continue;
        };

        let required_pins: std::collections::BTreeSet<_> = raw_pins
            .split(',')
            .map(str::trim)
            .filter(|pin| !pin.is_empty())
            .collect();

        if required_pins.is_empty() {
            diagnostics.push(Diagnostic::for_builtin_rule(
                BuiltinRuleId::Sch015,
                Severity::Error,
                format!(
                    "{reference} has an empty {REQUIRED_NO_CONNECT_PINS_ATTRIBUTE} declaration"
                ),
            ));
            continue;
        }

        for pin_number in required_pins {
            let pin_reference = format!("{reference}.{pin_number}");
            let Some(pin) = facts
                .pins
                .iter()
                .find(|pin| pin.reference == *reference && pin.number == pin_number)
            else {
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch015,
                    Severity::Warning,
                    format!(
                        "{pin_reference} is declared as requiring a no-connect marker, but its pin geometry could not be resolved; the requirement is unverified"
                    ),
                ));
                continue;
            };

            if let Some(electrical_type @ ("no_connect" | "unconnected")) =
                pin.electrical_type.as_deref()
            {
                diagnostics.push(Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch015,
                    Severity::Error,
                    format!(
                        "{pin_reference} has library electrical type {electrical_type:?} and must not be listed in {REQUIRED_NO_CONNECT_PINS_ATTRIBUTE}; that pin type already denotes no connection"
                    ),
                ));
                continue;
            }

            let marker_present = facts.no_connects.iter().any(|marker| {
                (marker.center_mm.0 - pin.position_mm.0)
                    .hypot(marker.center_mm.1 - pin.position_mm.1)
                    <= GEOMETRY_TOLERANCE_MM
            });

            if !marker_present {
                let mut diagnostic = Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch015,
                    Severity::Error,
                    format!(
                        "{pin_reference} is declared as intentionally unconnected but has no explicit no-connect marker"
                    ),
                );
                diagnostic.explanation = Some(
                    "The profile declares this pin as intentionally unused. An explicit marker makes that design decision visible and prevents an accidental omission from being mistaken for an unfinished connection."
                        .to_owned(),
                );
                diagnostic.help = Some(
                    "Place a KiCad no-connect flag at the transformed pin anchor, or remove the pin from the profile declaration if it is not intentionally unused."
                        .to_owned(),
                );
                diagnostic.schematic_location = Some(SchematicLocation {
                    x_mm: pin.position_mm.0,
                    y_mm: pin.position_mm.1,
                });
                diagnostics.push(diagnostic);
            }
        }
    }

    diagnostics
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::error::Error;

    use crate::core::BuiltinRuleId;
    use crate::parser::SchematicDocument;

    use crate::rules::{LintProfile, SchematicFacts};

    use super::validate_declared_frame_overlaps;

    /// Creates the smallest valid profile needed by the functional-frame unit tests.
    ///
    /// # Returns
    ///
    /// A version-one profile declaring two independent groups with distinct frame UUIDs.
    fn frame_profile() -> LintProfile {
        serde_json::from_str(
            r#"{
                "schema_version": 1,
                "groups": {
                    "input": {"description_uuid": "description-input", "frame_uuid": "frame-input"},
                    "controller": {"description_uuid": "description-controller", "frame_uuid": "frame-controller"}
                }
            }"#,
        )
        .expect("valid frame profile")
    }

    /// Confirms overlapping declared functional frames produce an SCH009 error at the overlap.
    ///
    /// # Returns
    ///
    /// Ok when the overlap is reported with both group identities and a schematic location.
    ///
    /// # Errors
    ///
    /// Returns an error if the schematic fixture cannot be parsed.
    #[test]
    fn overlapping_declared_group_frames_are_reported() -> Result<(), Box<dyn Error>> {
        let document = SchematicDocument::from_text(
            "overlapping-frames.kicad_sch",
            r#"(kicad_sch
                (rectangle (start 1 1) (end 10 10) (uuid "frame-input"))
                (rectangle (start 8 4) (end 14 12) (uuid "frame-controller"))
            )"#,
        )?;
        let facts = SchematicFacts::from_document(&document);
        let diagnostics = validate_declared_frame_overlaps(&frame_profile(), &facts);

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, BuiltinRuleId::Sch009.as_str());
        assert!(diagnostics[0].message.contains("input"));
        assert!(diagnostics[0].message.contains("controller"));
        assert!(diagnostics[0].schematic_location.is_some());
        Ok(())
    }

    /// Confirms adjacent declared frame edges are permitted and do not produce a false overlap.
    ///
    /// # Returns
    ///
    /// Ok when the touching-frame fixture produces no frame-overlap diagnostics.
    ///
    /// # Errors
    ///
    /// Returns an error if the schematic fixture cannot be parsed.
    #[test]
    fn touching_group_frame_edges_are_not_reported_as_overlap() -> Result<(), Box<dyn Error>> {
        let document = SchematicDocument::from_text(
            "touching-frames.kicad_sch",
            r#"(kicad_sch
                (rectangle (start 1 1) (end 10 10) (uuid "frame-input"))
                (rectangle (start 10 1) (end 18 10) (uuid "frame-controller"))
            )"#,
        )?;
        let facts = SchematicFacts::from_document(&document);

        assert_eq!(
            validate_declared_frame_overlaps(&frame_profile(), &facts).len(),
            0
        );
        Ok(())
    }

    /// Confirms two groups may intentionally refer to the same frame UUID without a self-overlap error.
    ///
    /// # Returns
    ///
    /// Ok when shared frame ownership produces no SCH009 overlap diagnostic.
    ///
    /// # Errors
    ///
    /// Returns an error if the schematic fixture cannot be parsed.
    #[test]
    fn two_groups_claiming_the_same_frame_do_not_self_overlap() -> Result<(), Box<dyn Error>> {
        let document = SchematicDocument::from_text(
            "shared-frame.kicad_sch",
            r#"(kicad_sch
                (rectangle (start 1 1) (end 10 10) (uuid "shared-frame"))
            )"#,
        )?;
        let mut profile = frame_profile();
        for group in profile.groups.values_mut() {
            group.frame_uuid = Some("shared-frame".to_owned());
        }
        let facts = SchematicFacts::from_document(&document);

        assert_eq!(validate_declared_frame_overlaps(&profile, &facts).len(), 0);
        Ok(())
    }

    /// Builds a one-pin symbol fixture with optional marker and effective electrical type.
    ///
    /// # Arguments
    ///
    /// * `with_marker` — Whether the fixture should contain the marker at the transformed pin coordinate.
    /// * `pin_type` — Base library electrical-type token for the pin.
    /// * `alternate_type` — Optional electrical type declared by an `NC` alternate.
    /// * `select_alternate` — Whether the placed instance selects the `NC` alternate.
    ///
    /// # Returns
    ///
    /// A parsed schematic containing the embedded one-pin symbol and the requested pin-type/marker state.
    ///
    /// # Errors
    ///
    /// Returns an error if the schematic fixture cannot be parsed.
    fn no_connect_document(
        with_marker: bool,
        pin_type: &str,
        alternate_type: Option<&str>,
        select_alternate: bool,
    ) -> Result<SchematicDocument, Box<dyn Error>> {
        let marker = if with_marker {
            r#"(no_connect (at 10 10) (uuid "required-nc"))"#
        } else {
            ""
        };
        let alternate_definition = alternate_type
            .map(|type_token| format!("(alternate \"NC\" {type_token} line)"))
            .unwrap_or_default();
        let instance_alternate = if select_alternate {
            r#"(pin "1" (alternate "NC") (uuid "pin-alternate"))"#
        } else {
            ""
        };
        let source = format!(
            r#"(kicad_sch
                (lib_symbols
                    (symbol "Test:OnePin"
                        (symbol "OnePin_1_1"
                            (pin {pin_type} line (at 0 0 0) (length 1.27)
                                (name "1") (number "1") {alternate_definition})
                        )
                    )
                )
                (symbol (lib_id "Test:OnePin") (at 10 10 0) (unit 1)
                    (property "Reference" "U1" (at 10 9 0)
                        (effects (font (size 1 1))))
                    {instance_alternate}
                )
                {marker}
            )"#
        );
        Ok(SchematicDocument::from_text(
            "required-no-connect.kicad_sch",
            &source,
        )?)
    }

    /// Creates component intent that requires an explicit no-connect marker for U1 pin 1.
    ///
    /// # Returns
    ///
    /// A valid version-one profile with the required no-connect pin attribute.
    fn no_connect_profile() -> LintProfile {
        serde_json::from_str(
            r#"{
                "schema_version": 1,
                "components": {
                    "U1": {
                        "kind": "integrated_circuit",
                        "role": "controller",
                        "function": "Control logic",
                        "rationale": "Fixture for required no-connect coverage",
                        "attributes": {"required_no_connect_pins": "1"}
                    }
                }
            }"#,
        )
        .expect("valid no-connect profile")
    }

    /// Confirms a profile-declared unused pin without a flag emits an SCH015 error.
    ///
    /// # Returns
    ///
    /// Ok when the missing marker is identified by its reference and pin number.
    ///
    /// # Errors
    ///
    /// Returns an error if the schematic fixture cannot be parsed or validated.
    #[test]
    fn declared_required_no_connect_pin_must_have_a_marker() -> Result<(), Box<dyn Error>> {
        let document = no_connect_document(false, "passive", None, false)?;
        let profile = no_connect_profile();
        let diagnostics = profile.validate_with_netlist(&document, None);

        assert!(diagnostics.iter().any(|item| {
            item.code == BuiltinRuleId::Sch015.as_str()
                && item.message.contains("U1.1")
                && item.message.contains("no explicit no-connect marker")
        }));
        Ok(())
    }

    /// Rejects profile no-connect declarations for pins whose base electrical type is already unconnected.
    ///
    /// # Returns
    ///
    /// Ok when an intrinsically unconnected library pin is rejected as a redundant declaration.
    ///
    /// # Errors
    ///
    /// Returns an error if the schematic fixture cannot be parsed or validated.
    #[test]
    fn declared_required_no_connect_pin_rejects_unconnected_library_type()
    -> Result<(), Box<dyn Error>> {
        let document = no_connect_document(false, "unconnected", None, false)?;
        let facts = SchematicFacts::from_document(&document);
        assert_eq!(
            facts.pins[0].electrical_type.as_deref(),
            Some("unconnected")
        );
        let diagnostics = no_connect_profile().validate_with_netlist(&document, None);
        assert!(diagnostics.iter().any(|item| {
            item.code == BuiltinRuleId::Sch015.as_str()
                && item.message.contains("U1.1")
                && item.message.contains("must not be listed")
                && item.message.contains("unconnected")
        }));
        Ok(())
    }

    /// Rejects a required no-connect declaration when the active alternate has no-connect electrical type.
    ///
    /// # Returns
    ///
    /// Ok when the selected alternate type, rather than the passive base type, controls SCH015 validation.
    ///
    /// # Errors
    ///
    /// Returns an error if the schematic fixture cannot be parsed or validated.
    #[test]
    fn declared_required_no_connect_pin_uses_active_alternate_electrical_type()
    -> Result<(), Box<dyn Error>> {
        let document = no_connect_document(false, "passive", Some("no_connect"), true)?;
        let facts = SchematicFacts::from_document(&document);
        assert_eq!(facts.pins[0].electrical_type.as_deref(), Some("no_connect"));
        let diagnostics = no_connect_profile().validate_with_netlist(&document, None);
        assert!(diagnostics.iter().any(|item| {
            item.code == BuiltinRuleId::Sch015.as_str()
                && item.message.contains("U1.1")
                && item.message.contains("must not be listed")
                && item.message.contains("no_connect")
        }));
        Ok(())
    }

    /// Confirms a marker at the transformed embedded pin anchor satisfies the profile contract.
    ///
    /// # Returns
    ///
    /// Ok when the valid marker does not produce an SCH015 missing-marker finding.
    ///
    /// # Errors
    ///
    /// Returns an error if the schematic fixture cannot be parsed or validated.
    #[test]
    fn declared_required_no_connect_pin_accepts_a_marker_at_the_transformed_pin()
    -> Result<(), Box<dyn Error>> {
        let document = no_connect_document(true, "passive", None, false)?;
        let profile = no_connect_profile();
        let diagnostics = profile.validate_with_netlist(&document, None);

        assert!(!diagnostics.iter().any(|item| {
            item.code == BuiltinRuleId::Sch015.as_str() && item.message.contains("U1.1")
        }));
        Ok(())
    }
}

#[cfg(test)]
/// Tests opt-in sheet title-block completeness contracts.
mod title_block_tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::validate_required_title_block_fields;
    use crate::parser::SchematicDocument;
    use crate::rules::{LintProfile, SchematicFacts};

    /// Parses a profile requiring the supplied title-block fields.
    ///
    /// # Arguments
    ///
    /// * `fields` — JSON array of title-block field keys required by the test profile.
    ///
    /// # Returns
    ///
    /// A valid profile with the supplied required title-block field set.
    fn profile(fields: &str) -> LintProfile {
        serde_json::from_str(&format!(
            r#"{{"schema_version":1,"required_title_block_fields":{fields}}}"#
        ))
        .expect("profile fixture parses")
    }

    /// Reports missing title and revision fields requested by the project.
    #[test]
    fn missing_required_title_block_fields_are_reported() {
        let schematic = SchematicDocument::from_text(
            "missing-title-block.kicad_sch",
            "(kicad_sch (title_block (company \"Acme\")))",
        )
        .expect("schematic fixture parses");
        let facts = SchematicFacts::from_document(&schematic);
        let diagnostics =
            validate_required_title_block_fields(&profile(r#"["title","rev"]"#), &facts);
        assert_eq!(diagnostics.len(), 2);
        assert!(diagnostics.iter().all(|item| item.code == "SCH023"));
        assert!(
            diagnostics
                .iter()
                .any(|item| item.message.contains("\"title\" is missing"))
        );
        assert!(
            diagnostics
                .iter()
                .any(|item| item.message.contains("\"rev\" is missing"))
        );
    }

    /// Accepts populated title-block values after trimming surrounding whitespace.
    #[test]
    fn populated_required_title_block_fields_are_accepted() {
        let schematic = SchematicDocument::from_text(
            "complete-title-block.kicad_sch",
            "(kicad_sch (title_block (title \"Power board\") (date \"2026-11-20\") (rev \"A\")))",
        )
        .expect("schematic fixture parses");
        let facts = SchematicFacts::from_document(&schematic);
        let diagnostics =
            validate_required_title_block_fields(&profile(r#"["title","date","rev"]"#), &facts);
        assert_eq!(diagnostics, Vec::new());
    }

    /// Rejects title-block fields that exist but contain only whitespace.
    #[test]
    fn blank_required_title_block_fields_are_rejected() {
        let schematic = SchematicDocument::from_text(
            "blank-title-block.kicad_sch",
            "(kicad_sch (title_block (title \"   \")))",
        )
        .expect("schematic fixture parses");
        let facts = SchematicFacts::from_document(&schematic);
        let diagnostics = validate_required_title_block_fields(&profile(r#"["title"]"#), &facts);
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("present but empty"));
    }

    /// Rejects unsupported title-block keys instead of treating them as valid empty fields.
    #[test]
    fn unsupported_title_block_fields_are_rejected() {
        let schematic =
            SchematicDocument::from_text("unsupported-title-field.kicad_sch", "(kicad_sch)")
                .expect("schematic fixture parses");
        let facts = SchematicFacts::from_document(&schematic);
        let diagnostics =
            validate_required_title_block_fields(&profile(r#"["sheet_number"]"#), &facts);
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("unsupported field"));
    }
}

#[cfg(test)]
/// Tests current-sheet label scope policy, including global allow/deny and required named scopes.
mod label_scope_policy_tests {
    #![allow(clippy::expect_used)]

    use super::validate_label_scope_policy;
    use crate::rules::{LabelScopeKind, LintProfile, SchematicFacts, SchematicLabelFact};

    /// Parses a version-one profile containing the supplied label-scope policy object.
    ///
    /// # Arguments
    ///
    /// * `policy` — JSON object for the optional `label_scope_policy` profile property.
    ///
    /// # Returns
    ///
    /// A profile with the supplied label-scope policy.
    fn profile(policy: &str) -> LintProfile {
        let source = format!(r#"{{"schema_version":1,"label_scope_policy":{policy}}}"#);
        serde_json::from_str(&source).expect("label-scope profile parses")
    }

    /// Builds normalized label facts from native `KiCad` label-kind names.
    ///
    /// # Arguments
    ///
    /// * `labels` — Pairs of serialized `KiCad` label kind and label name.
    ///
    /// # Returns
    ///
    /// Schematic facts containing those labels and no unrelated schematic objects.
    fn facts(labels: &[(&str, &str)]) -> SchematicFacts {
        SchematicFacts {
            labels: labels
                .iter()
                .map(|(kind, name)| SchematicLabelFact {
                    kind: (*kind).to_owned(),
                    name: (*name).to_owned(),
                    position_mm: (0.0, 0.0),
                    bounds_mm: None,
                })
                .collect(),
            ..SchematicFacts::default()
        }
    }

    /// Accepts global labels listed in the configured allow-list.
    #[test]
    fn allowed_global_labels_are_accepted() {
        let profile = profile(r#"{"allowed_global_labels":["VDD","GND"]}"#);
        let diagnostics = validate_label_scope_policy(
            &profile,
            &facts(&[("global_label", "VDD"), ("global_label", "GND")]),
        );
        assert_eq!(diagnostics, Vec::new());
    }

    /// An explicitly empty allow-list forbids every global label on the loaded sheet.
    #[test]
    fn empty_global_allow_list_forbids_all_global_labels() {
        let profile = profile(r#"{"allowed_global_labels":[]}"#);
        let diagnostics = validate_label_scope_policy(&profile, &facts(&[("global_label", "VDD")]));
        assert_eq!(diagnostics.len(), 1);
        assert!(
            diagnostics[0]
                .message
                .contains("not listed in allowed_global_labels")
        );
    }

    /// Rejects global labels on the explicit deny-list.
    #[test]
    fn forbidden_global_labels_are_rejected() {
        let profile = profile(r#"{"forbidden_global_labels":["RESET"]}"#);
        let diagnostics =
            validate_label_scope_policy(&profile, &facts(&[("global_label", "RESET")]));
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("is forbidden"));
    }

    /// Accepts required local, global and hierarchical label scopes.
    #[test]
    fn required_label_scopes_match_native_label_kinds() {
        let profile = profile(
            r#"{
                "required_label_scopes":{
                    "VDD":"local",
                    "RESET":"global",
                    "SDA":"hierarchical"
                }
            }"#,
        );
        let diagnostics = validate_label_scope_policy(
            &profile,
            &facts(&[
                ("label", "VDD"),
                ("global_label", "RESET"),
                ("hierarchical_label", "SDA"),
            ]),
        );
        assert_eq!(diagnostics, Vec::new());
    }

    /// Reports both missing required labels and labels using the wrong requested scope.
    #[test]
    fn missing_and_mismatched_required_label_scopes_are_reported() {
        let profile = profile(
            r#"{
                "required_label_scopes":{
                    "RESET":"hierarchical",
                    "VDD":"global",
                    "MISSING":"local"
                }
            }"#,
        );
        let diagnostics = validate_label_scope_policy(
            &profile,
            &facts(&[("global_label", "RESET"), ("label", "VDD")]),
        );
        assert_eq!(diagnostics.len(), 3);
        assert!(
            diagnostics
                .iter()
                .any(|item| item.message.contains("RESET") && item.message.contains("requires"))
        );
        assert!(
            diagnostics
                .iter()
                .any(|item| item.message.contains("VDD") && item.message.contains("requires"))
        );
        assert!(diagnostics.iter().any(|item| item.message.contains("MISSING") && item.message.contains("is missing")));
    }

    /// Rejects contradictory, empty and untrimmed policy names before evaluating sheet labels.
    #[test]
    fn invalid_and_contradictory_label_scope_configuration_is_rejected() {
        let profile = profile(
            r#"{
                "allowed_global_labels":["VDD"," RESET "],
                "forbidden_global_labels":["VDD"],
                "required_label_scopes":{"   ":"local","RESET":"global"}
            }"#,
        );
        let diagnostics = validate_label_scope_policy(&profile, &facts(&[]));
        assert!(diagnostics.len() >= 4);
        assert!(diagnostics.iter().all(|item| item.code == "SCH027"));
    }

    /// Leaves label scope unrestricted when the project omits the policy entirely.
    #[test]
    fn omitted_label_scope_policy_does_not_change_existing_schematics() {
        let profile: LintProfile =
            serde_json::from_str(r#"{"schema_version":1}"#).expect("plain profile parses");
        assert_eq!(
            validate_label_scope_policy(&profile, &facts(&[("global_label", "ANY_NAME")])),
            Vec::new()
        );
    }

    /// Applies global-label and required-scope policies to labels in referenced child sheets.
    #[test]
    fn project_wide_label_policy_checks_referenced_child_sheets() {
        use super::validate_label_scope_policy_with_document;
        use crate::parser::SchematicDocument;
        use std::fs;
        use tempfile::tempdir;

        let directory = tempdir().expect("temporary project directory is created");
        let root = directory.path().join("root.kicad_sch");
        let child = directory.path().join("child.kicad_sch");
        fs::write(
            &root,
            r#"(kicad_sch
                (sheet (property "Sheetfile" "child.kicad_sch"))
                (global_label "VDD" (at 1 1 0))
            )"#,
        )
        .expect("root sheet is written");
        fs::write(
            &child,
            r#"(kicad_sch
                (hierarchical_label "SDA" (at 2 2 0))
            )"#,
        )
        .expect("child sheet is written");
        let document = SchematicDocument::from_path(&root).expect("root schematic parses");
        let facts = SchematicFacts::from_document(&document);
        let profile = profile(
            r#"{
                "project_wide":true,
                "allowed_global_labels":["VDD"],
                "required_label_scopes":{"VDD":"global","SDA":"hierarchical"}
            }"#,
        );

        let diagnostics =
            validate_label_scope_policy_with_document(&profile, &facts, Some(&document));
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    /// Reports a forbidden global label with the source path of its nested sheet.
    #[test]
    fn project_wide_label_policy_reports_nested_global_label_source() {
        use super::validate_label_scope_policy_with_document;
        use crate::parser::SchematicDocument;
        use std::fs;
        use tempfile::tempdir;

        let directory = tempdir().expect("temporary project directory is created");
        let root = directory.path().join("root.kicad_sch");
        let child = directory.path().join("child.kicad_sch");
        let grandchild = directory.path().join("grandchild.kicad_sch");
        fs::write(
            &root,
            r#"(kicad_sch (sheet (property "Sheetfile" "child.kicad_sch")))"#,
        )
        .expect("root sheet is written");
        fs::write(
            &child,
            r#"(kicad_sch (sheet (property "Sheetfile" "grandchild.kicad_sch")))"#,
        )
        .expect("child sheet is written");
        fs::write(
            &grandchild,
            r#"(kicad_sch (global_label "DEBUG_INTERNAL" (at 3 3 0)))"#,
        )
        .expect("grandchild sheet is written");
        let document = SchematicDocument::from_path(&root).expect("root schematic parses");
        let facts = SchematicFacts::from_document(&document);
        let profile =
            profile(r#"{"project_wide":true,"forbidden_global_labels":["DEBUG_INTERNAL"]}"#);

        let diagnostics =
            validate_label_scope_policy_with_document(&profile, &facts, Some(&document));
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, crate::core::Severity::Error);
        assert!(
            diagnostics[0]
                .source
                .as_ref()
                .is_some_and(|path| path.ends_with("grandchild.kicad_sch"))
        );
    }

    /// Uses an unverified warning rather than a missing-label error when a referenced child cannot be read.
    #[test]
    fn project_wide_missing_child_does_not_create_false_required_label_error() {
        use super::validate_label_scope_policy_with_document;
        use crate::parser::SchematicDocument;
        use std::fs;
        use tempfile::tempdir;

        let directory = tempdir().expect("temporary project directory is created");
        let root = directory.path().join("root.kicad_sch");
        fs::write(
            &root,
            r#"(kicad_sch (sheet (property "Sheetfile" "missing.kicad_sch")))"#,
        )
        .expect("root sheet is written");
        let document = SchematicDocument::from_path(&root).expect("root schematic parses");
        let facts = SchematicFacts::from_document(&document);
        let profile =
            profile(r#"{"project_wide":true,"required_label_scopes":{"REQUIRED":"local"}}"#);

        let diagnostics =
            validate_label_scope_policy_with_document(&profile, &facts, Some(&document));
        assert!(diagnostics.iter().any(|item| {
            item.severity == crate::core::Severity::Warning
                && item.message.contains("could not be resolved")
        }));
        assert!(diagnostics.iter().any(|item| {
            item.severity == crate::core::Severity::Warning
                && item.message.contains("required label")
                && item.message.contains("unverified")
        }));
        assert!(!diagnostics.iter().any(|item| {
            item.severity == crate::core::Severity::Error && item.message.contains("REQUIRED")
        }));
    }

    /// Loads a referenced child once when multiple sheet objects reference it and the child points back to the root.
    #[test]
    fn project_wide_label_policy_handles_repeated_child_references_and_cycles() {
        use super::validate_label_scope_policy_with_document;
        use crate::parser::SchematicDocument;
        use std::fs;
        use tempfile::tempdir;

        let directory = tempdir().expect("temporary project directory is created");
        let root = directory.path().join("root.kicad_sch");
        let child = directory.path().join("child.kicad_sch");
        fs::write(
            &root,
            r#"(kicad_sch
                (sheet (property "Sheetfile" "child.kicad_sch"))
                (sheet (property "Sheetfile" "child.kicad_sch"))
            )"#,
        )
        .expect("root sheet is written");
        fs::write(
            &child,
            r#"(kicad_sch
                (sheet (property "Sheetfile" "root.kicad_sch"))
                (global_label "DEBUG_INTERNAL" (at 4 4 0))
            )"#,
        )
        .expect("child sheet is written");
        let document = SchematicDocument::from_path(&root).expect("root schematic parses");
        let facts = SchematicFacts::from_document(&document);
        let profile =
            profile(r#"{"project_wide":true,"forbidden_global_labels":["DEBUG_INTERNAL"]}"#);

        let diagnostics =
            validate_label_scope_policy_with_document(&profile, &facts, Some(&document));
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("DEBUG_INTERNAL"));
    }

    /// Reports that project-wide label policy cannot be verified for an in-memory root schematic.
    #[test]
    fn project_wide_label_policy_warns_for_in_memory_documents() {
        use super::validate_label_scope_policy_with_document;
        use crate::parser::SchematicDocument;

        let document = SchematicDocument::from_text(
            "in-memory.kicad_sch",
            r#"(kicad_sch (global_label "DEBUG_INTERNAL" (at 1 1 0)))"#,
        )
        .expect("in-memory schematic parses");
        let facts = SchematicFacts::from_document(&document);
        let profile = profile(r#"{"project_wide":true}"#);

        let diagnostics =
            validate_label_scope_policy_with_document(&profile, &facts, Some(&document));
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, crate::core::Severity::Warning);
        assert!(diagnostics[0].message.contains("path-based root schematic"));
    }

    /// Keeps scope comparisons tied to exact project-selected label kinds.
    #[test]
    fn label_scope_kind_values_round_trip_through_profile_deserialization() {
        let profile = profile(r#"{"required_label_scopes":{"RESET":"hierarchical"}}"#);
        assert_eq!(
            profile
                .label_scope_policy
                .as_ref()
                .and_then(|policy| policy.required_label_scopes.get("RESET")),
            Some(&LabelScopeKind::Hierarchical)
        );
    }
}
