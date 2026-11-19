//! Checks `SCH019` that reused label text has a consistent label kind.

use super::collect_labels;
use kicad_clippy_core::BuiltinRuleId;
use kicad_clippy_core::Diagnostic;
use kicad_clippy_core::Severity;
use kiutils_sexpr::Node;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

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

/// Concrete implementation of built-in rule 019.
pub(super) struct Sch019;
impl<'a> crate::Rule<crate::RuleContext<'a>> for Sch019 {
    /// Returns the stable 019 identifier.
    ///
    /// # Returns
    ///
    /// The stable SCH019 identifier used to associate diagnostics with this rule.
    fn id(&self) -> BuiltinRuleId {
        BuiltinRuleId::Sch019
    }
    /// Evaluates consistent label kinds for identical label text.
    ///
    /// # Arguments
    ///
    /// * `context` — the parsed schematic containing local, global, and hierarchical labels.
    ///
    /// # Returns
    ///
    /// Diagnostics for consistent label kinds for identical label text.
    fn check(&self, context: &crate::RuleContext<'a>) -> Vec<Diagnostic> {
        let Some(root) = context.document().cst().nodes.first() else {
            return Vec::new();
        };
        check_label_kind_consistency(root)
    }
}
