//! Checks uniqueness of UUIDs within the currently loaded schematic document.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use crate::core::{BuiltinRuleId, Diagnostic, Severity};
use kiutils_sexpr::{Atom, Node};

use crate::rules::{RuleContext, RulePass};

/// A rule pass that detects duplicate object UUIDs in one parsed schematic document.
pub struct DuplicateUuidPass;

impl RulePass for DuplicateUuidPass {
    /// Returns the stable SCH024 rule-pass name.
    ///
    /// # Returns
    ///
    /// The static identifier used to register this pass.
    fn name(&self) -> &'static str {
        "sch024.document-uuid-uniqueness"
    }

    /// Reports UUID values assigned to more than one object in the loaded schematic document.
    ///
    /// # Arguments
    ///
    /// * `context` — Shared parsed schematic document and normalized facts for this lint invocation.
    ///
    /// # Returns
    ///
    /// One SCH024 error for each UUID used by more than one object in this document.
    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let mut occurrences = BTreeMap::<String, Vec<String>>::new();
        let mut visited_paths = BTreeSet::<PathBuf>::new();
        let mut diagnostics = Vec::new();

        collect_hierarchy_uuids(
            context.document(),
            &mut visited_paths,
            &mut occurrences,
            &mut diagnostics,
        );

        diagnostics.extend(occurrences.into_iter().filter_map(|(uuid, objects)| {
            (objects.len() > 1).then(|| {
                Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch024,
                    Severity::Error,
                    format!(
                        "UUID {uuid:?} is used by {} objects across the schematic hierarchy: {}",
                        objects.len(),
                        objects.join(", ")
                    ),
                )
            })
        }));
        diagnostics
    }
}

/// Traverses a path-based schematic root and each distinct referenced child sheet.
///
/// In-memory documents whose associated path is not a file retain the original
/// single-document behavior. Child files are resolved relative to their parent
/// schematic, and canonical paths prevent repeated loading and hierarchy cycles.
///
/// # Arguments
///
/// * `document` — Parsed root or child schematic whose UUIDs and child-sheet references are inspected.
/// * `visited_paths` — Canonical schematic paths already visited during this hierarchy traversal.
/// * `occurrences` — UUID-to-object descriptions accumulated across all readable documents.
/// * `diagnostics` — SCH024 warnings for child sheets that cannot be inspected.
///
/// # Returns
///
/// No value; UUID occurrences and hierarchy-load warnings are appended to the supplied collections.
fn collect_hierarchy_uuids(
    document: &SchematicDocument,
    visited_paths: &mut BTreeSet<PathBuf>,
    occurrences: &mut BTreeMap<String, Vec<String>>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let source_path = document.source().path();
    let path_key = fs::canonicalize(source_path).unwrap_or_else(|_| source_path.to_path_buf());
    if !visited_paths.insert(path_key) {
        return;
    }

    let source_label = source_path.display().to_string();
    let Some(root) = document.cst().nodes.first() else {
        return;
    };
    collect_uuid_occurrences(root, &source_label, occurrences);

    // A from_text document is often used by library callers and unit tests.
    // Do not resolve serialized Sheetfile references unless the source exists.
    if !source_path.is_file() {
        return;
    }

    let mut child_files = BTreeSet::<PathBuf>::new();
    collect_child_sheet_files(root, &mut child_files);
    let parent_directory = source_path.parent().unwrap_or(Path::new("."));

    for child_file in child_files {
        if child_file.is_absolute() {
            diagnostics.push(child_sheet_warning(
                &child_file,
                "absolute Sheetfile references are not followed",
            ));
            continue;
        }

        let referenced_path = parent_directory.join(&child_file);
        let canonical_path = match fs::canonicalize(&referenced_path) {
            Ok(path) => path,
            Err(error) => {
                diagnostics.push(child_sheet_warning(&referenced_path, &error.to_string()));
                continue;
            }
        };
        if visited_paths.contains(&canonical_path) {
            continue;
        }

        match SchematicDocument::from_path(&canonical_path) {
            Ok(child_document) => collect_hierarchy_uuids(
                &child_document,
                visited_paths,
                occurrences,
                diagnostics,
            ),
            Err(error) => diagnostics.push(child_sheet_warning(
                &canonical_path,
                &error.to_string(),
            )),
        }
    }
}

/// Collects every referenced child schematic path from native `sheet` objects.
///
/// # Arguments
///
/// * `node` — Current S-expression node to inspect recursively.
/// * `child_files` — Ordered set that receives non-empty `Sheetfile` property values.
///
/// # Returns
///
/// No value; each `Sheetfile` property found on a hierarchical sheet is added to the set.
fn collect_child_sheet_files(node: &Node, child_files: &mut BTreeSet<PathBuf>) {
    let Node::List { items, .. } = node else {
        return;
    };
    let is_sheet = matches!(
        items.first(),
        Some(Node::Atom {
            atom: Atom::Symbol(name),
            ..
        }) if name == "sheet"
    );
    if is_sheet {
        for child in items.iter().skip(1) {
            let Node::List {
                items: property_items,
                ..
            } = child
            else {
                continue;
            };
            let is_sheet_file_property = matches!(
                property_items.first(),
                Some(Node::Atom {
                    atom: Atom::Symbol(name),
                    ..
                }) if name == "property"
            ) && property_items.get(1).and_then(atom_text) == Some("Sheetfile");
            if is_sheet_file_property {
                if let Some(file_name) = property_items.get(2).and_then(atom_text) {
                    if !file_name.trim().is_empty() {
                        child_files.insert(PathBuf::from(file_name));
                    }
                }
            }
        }
    }

    for child in items.iter().skip(1) {
        collect_child_sheet_files(child, child_files);
    }
}

/// Creates a non-blocking finding when a referenced child schematic cannot be inspected.
///
/// # Arguments
///
/// * `path` — Referenced child schematic path that could not be loaded.
/// * `reason` — Filesystem or parser reason that prevented the hierarchy check.
///
/// # Returns
///
/// An SCH024 warning identifying the child sheet whose UUID uniqueness remains unverified.
fn child_sheet_warning(path: &Path, reason: &str) -> Diagnostic {
    let mut diagnostic = Diagnostic::for_builtin_rule(
        BuiltinRuleId::Sch024,
        Severity::Warning,
        format!(
            "could not verify hierarchical UUID uniqueness in child schematic {:?}: {reason}",
            path.display()
        ),
    );
    diagnostic.source = Some(path.to_path_buf());
    diagnostic
}

/// Walks an S-expression tree and records each direct object UUID with its owning file and object kind.
///
/// # Arguments
///
/// * `node` — Current syntax-tree node to inspect recursively.
/// * `source_path` — Human-readable source path of the schematic document owning the object.
/// * `occurrences` — UUID-to-object map accumulated across the loaded schematic hierarchy.
///
/// # Returns
///
/// No value; every UUID found in `node` and its descendants is appended to the map.
fn collect_uuid_occurrences(
    node: &Node,
    source_path: &str,
    occurrences: &mut BTreeMap<String, Vec<String>>,
) {
    let Node::List { items, .. } = node else {
        return;
    };
    let Some(Atom::Symbol(kind)) = items.first().and_then(|head| match head {
        Node::Atom { atom, .. } => Some(atom),
        Node::List { .. } => None,
    }) else {
        return;
    };
    for child in items.iter().skip(1) {
        if let Node::List {
            items: child_items, ..
        } = child
        {
            let is_uuid = matches!(
                child_items.first(),
                Some(Node::Atom {
                    atom: Atom::Symbol(name),
                    ..
                }) if name == "uuid"
            );
            if is_uuid {
                if let Some(value) = child_items.get(1).and_then(atom_text) {
                    if !value.trim().is_empty() {
                        occurrences
                            .entry(value.to_owned())
                            .or_default()
                            .push(format!("{source_path}: {kind}"));
                    }
                }
            }
            collect_uuid_occurrences(child, source_path, occurrences);
        }
    }
}

/// Reads a symbol or quoted atom's decoded text.
///
/// # Arguments
///
/// * `node` — S-expression atom from an object's UUID or other serialized identifier field.
///
/// # Returns
///
/// The decoded atom content, or `None` for a list node.
fn atom_text(node: &Node) -> Option<&str> {
    let Node::Atom { atom, .. } = node else {
        return None;
    };
    match atom {
        Atom::Symbol(value) | Atom::Quoted(value) => Some(value),
    }
}

#[cfg(test)]
/// Tests duplicate-UUID detection on a loaded schematic document.
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::BuiltinRuleId;
    use crate::parser::SchematicDocument;
    use crate::rules::RuleEngine;

    /// Runs the built-in engine for a parsed schematic and filters its SCH024 findings.
    ///
    /// # Arguments
    ///
    /// * `source` — Complete S-expression source for the schematic test fixture.
    ///
    /// # Returns
    ///
    /// Diagnostics emitted under SCH024 for the supplied schematic document.
    fn findings(source: &str) -> Vec<crate::core::Diagnostic> {
        let document = SchematicDocument::from_text("duplicate-uuid.kicad_sch", source)
            .expect("fixture parses");
        RuleEngine::with_builtins()
            .run(&document)
            .diagnostics
            .into_iter()
            .filter(|item| item.code == BuiltinRuleId::Sch024.as_str())
            .collect()
    }

    /// Reports one duplicate UUID even when it is attached to different object kinds.
    #[test]
    fn duplicate_uuids_are_reported_across_object_kinds() {
        let diagnostics = findings(
            r#"(kicad_sch
                (uuid "root")
                (symbol (lib_id "Device:R") (at 10 10) (uuid "duplicate"))
                (rectangle (start 1 1) (end 5 5) (uuid "duplicate"))
            )"#,
        );
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, BuiltinRuleId::Sch024.as_str());
        assert!(diagnostics[0].message.contains("symbol"));
        assert!(diagnostics[0].message.contains("rectangle"));
    }

    /// Accepts a document where every object UUID is unique.
    #[test]
    fn unique_uuids_are_accepted() {
        let diagnostics = findings(
            r#"(kicad_sch
                (uuid "root")
                (symbol (lib_id "Device:R") (uuid "symbol-1"))
                (rectangle (start 1 1) (end 5 5) (uuid "rectangle-1"))
            )"#,
        );
        assert_eq!(diagnostics, Vec::new());
    }

    /// Finds repeated identifiers in nested objects, not only at the top level.
    #[test]
    fn nested_duplicate_uuids_are_reported() {
        let diagnostics = findings(
            r#"(kicad_sch
                (symbol
                    (uuid "placed-symbol")
                    (property "Reference" "U1" (uuid "property-id"))
                    (property "Value" "IC" (uuid "property-id"))
                )
            )"#,
        );
        assert!(diagnostics.iter().any(|item| {
            item.message.contains("property-id") && item.message.contains("2 objects")
        }));
    }

    /// Finds duplicate UUIDs shared by a root schematic, child sheet, and grandchild sheet.
    #[test]
    fn duplicate_uuids_are_reported_across_the_hierarchical_sheet_tree() {
        use std::fs;

        let directory = tempfile::tempdir().expect("temporary project directory is created");
        let root_path = directory.path().join("root.kicad_sch");
        let child_path = directory.path().join("child.kicad_sch");
        let grandchild_path = directory.path().join("grandchild.kicad_sch");

        fs::write(
            &root_path,
            r#"(kicad_sch
                (uuid "root-uuid")
                (symbol (uuid "shared-across-sheets"))
                (sheet (uuid "child-sheet")
                    (property "Sheetname" "Child")
                    (property "Sheetfile" "child.kicad_sch"))
            )"#,
        )
        .expect("root schematic is written");
        fs::write(
            &child_path,
            r#"(kicad_sch
                (uuid "child-root-uuid")
                (sheet (uuid "grandchild-sheet")
                    (property "Sheetname" "Grandchild")
                    (property "Sheetfile" "grandchild.kicad_sch"))
            )"#,
        )
        .expect("child schematic is written");
        fs::write(
            &grandchild_path,
            r#"(kicad_sch
                (uuid "grandchild-root-uuid")
                (rectangle (uuid "shared-across-sheets"))
            )"#,
        )
        .expect("grandchild schematic is written");

        let document = SchematicDocument::from_path(&root_path)
            .expect("root schematic parses");
        let diagnostics = RuleEngine::with_builtins()
            .run(&document)
            .diagnostics
            .into_iter()
            .filter(|item| item.code == BuiltinRuleId::Sch024.as_str())
            .collect::<Vec<_>>();

        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("shared-across-sheets"));
        assert!(diagnostics[0].message.contains("root.kicad_sch"));
        assert!(diagnostics[0].message.contains("grandchild.kicad_sch"));
        assert!(diagnostics[0].message.contains("symbol"));
        assert!(diagnostics[0].message.contains("rectangle"));
    }

    /// Reports a warning instead of claiming complete hierarchy coverage when a child sheet is missing.
    #[test]
    fn missing_child_sheet_is_reported_as_unverified() {
        use std::fs;

        let directory = tempfile::tempdir().expect("temporary project directory is created");
        let root_path = directory.path().join("root.kicad_sch");
        fs::write(
            &root_path,
            r#"(kicad_sch
                (uuid "root-uuid")
                (sheet (uuid "child-sheet")
                    (property "Sheetname" "Missing")
                    (property "Sheetfile" "missing.kicad_sch"))
            )"#,
        )
        .expect("root schematic is written");

        let document = SchematicDocument::from_path(&root_path)
            .expect("root schematic parses");
        let diagnostics = RuleEngine::with_builtins()
            .run(&document)
            .diagnostics
            .into_iter()
            .filter(|item| item.code == BuiltinRuleId::Sch024.as_str())
            .collect::<Vec<_>>();

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, crate::core::Severity::Warning);
        assert!(diagnostics[0].message.contains("missing.kicad_sch"));
        assert!(diagnostics[0].message.contains("could not verify"));
    }

}
