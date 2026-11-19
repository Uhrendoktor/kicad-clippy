//! Checks `SCH019` that reused label text has a consistent label kind.

use kicad_clippy_core::BuiltinRuleId;
use kicad_clippy_core::Diagnostic;
use kicad_clippy_core::Severity;
use kiutils_sexpr::Node;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use super::collect_labels;

/// Detects identical label text used with inconsistent local/global/hierarchical label kinds.
///
/// # Arguments
///
/// * `root` — Root S-expression node of the parsed schematic.
///
/// # Returns
///
/// SCH019 diagnostics for identical label text reused with conflicting local, global, or hierarchical label kinds.
fn check_label_kind_consistency(root: &Node) -> Vec<Diagnostic> {
    let mut label_kinds: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (kind, label, _point, _span) in collect_labels(root) {
        label_kinds.entry(label).or_default().insert(kind);
    }
    label_kinds
        .into_iter()
        .filter_map(|(label, kinds)| {
            (kinds.len() > 1).then(|| {
                Diagnostic::for_builtin_rule(
                    BuiltinRuleId::Sch019,
                    Severity::Error,
                    format!(
                        "label {label:?} mixes types: {}; use one scope consistently",
                        kinds.into_iter().collect::<Vec<_>>().join(", ")
                    ),
                )
            })
        })
        .collect()
}
