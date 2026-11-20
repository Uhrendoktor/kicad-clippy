//! Checks uniqueness of UUIDs within the currently loaded schematic document.

use std::collections::BTreeMap;

use kicad_clippy_core::{BuiltinRuleId, Diagnostic, Severity};
use kiutils_sexpr::{Atom, Node};

use crate::{RuleContext, RulePass};

/// A rule pass that detects duplicate object UUIDs in one parsed schematic document.
pub(crate) struct DuplicateUuidPass;

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
        if let Some(root) = context.document().cst().nodes.first() {
            collect_uuid_occurrences(root, &mut occurrences);
        }
        occurrences
            .into_iter()
            .filter_map(|(uuid, objects)| {
                (objects.len() > 1).then(|| {
                    Diagnostic::for_builtin_rule(
                        BuiltinRuleId::Sch024,
                        Severity::Error,
                        format!(
                            "UUID {uuid:?} is used by {} objects in this schematic document: {}",
                            objects.len(),
                            objects.join(", ")
                        ),
                    )
                })
            })
            .collect()
    }
}

/// Walks an S-expression tree and records each direct object UUID with its owning object kind.
///
/// # Arguments
///
/// * `node` — Current syntax-tree node to inspect recursively.
/// * `occurrences` — UUID-to-object-kind map accumulated during the traversal.
///
/// # Returns
///
/// No value; every UUID found in `node` and its descendants is appended to the map.
fn collect_uuid_occurrences(node: &Node, occurrences: &mut BTreeMap<String, Vec<String>>) {
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
        if let Node::List { items: child_items, .. } = child {
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
                            .push(kind.to_owned());
                    }
                }
            }
            collect_uuid_occurrences(child, occurrences);
        }
    }
}

/// Reads a symbol or quoted atom's decoded text.
///
/// # Arguments
///
/// * `node` — S-expression atom to decode.
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
    use super::DuplicateUuidPass;
    use crate::{RuleContext, RulePass};
    use kicad_clippy_core::BuiltinRuleId;
    use kicad_clippy_parser::SchematicDocument;
    use crate::{generic::SchematicFacts, generic::collect_segments, generic::collect_points};
    use std::collections::BTreeMap;

    /// Runs SCH024 for one parsed schematic fixture and returns its findings.
    ///
    /// # Arguments
    ///
    /// * `source` — Complete S-expression source for the schematic test fixture.
    ///
    /// # Returns
    ///
    /// The diagnostics emitted by SCH024 for the fixture.
    fn findings(source: &str) -> Vec<kicad_clippy_core::Diagnostic> {
        let document =
            SchematicDocument::from_text("duplicate-uuid.kicad_sch", source).expect("fixture parses");
        let root = document.cst().nodes.first().expect("root exists");
        let segments = collect_segments(root);
        let junctions = collect_points(root, "junction");
        let facts = SchematicFacts::from_document_with_geometry(&document, &segments, &junctions);
        let context = RuleContext {
            document: &document,
            facts: &facts,
            segments: &segments,
            junctions: &junctions,
            default_wire_stroke_width_mm: None,
            project_wire_stroke_widths_mm: &BTreeMap::new(),
            default_schematic_line_stroke_width_mm: None,
        };
        DuplicateUuidPass.run(&context)
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
        assert!(diagnostics.is_empty());
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
}
