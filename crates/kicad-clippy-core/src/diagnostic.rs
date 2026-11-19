use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{rule_id::BuiltinRuleId, source::ByteSpan};

/// Diagnostic severity used by native ERC and custom rules.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// A blocking finding.
    Error,
    /// A non-blocking finding.
    Warning,
    /// Informational output.
    Note,
}

impl Severity {
    /// Returns the lowercase severity token used by text and serialized output.
    ///
    /// # Returns
    ///
    /// The stable lowercase token: `error`, `warning`, or `note`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Note => "note",
        }
    }
}

/// A schematic coordinate in millimetres, independent of source byte offsets.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct SchematicLocation {
    /// X coordinate in millimetres.
    pub x_mm: f64,
    /// Y coordinate in millimetres.
    pub y_mm: f64,
}

/// An affected item related to a diagnostic.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RelatedLocation {
    /// Human-readable description supplied by `KiCad` or a rule.
    pub description: String,
    /// `KiCad` item UUID, when available.
    pub uuid: Option<String>,
    /// Physical schematic location, when available.
    pub location: Option<SchematicLocation>,
}

/// A normalized lint or ERC finding.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Diagnostic {
    /// Stable rule identifier.
    pub code: String,
    /// Severity after the rule's configured level is applied.
    pub severity: Severity,
    /// One-line finding summary.
    pub message: String,
    /// Longer explanation of why the finding matters.
    pub explanation: Option<String>,
    /// Suggested next action.
    pub help: Option<String>,
    /// Source file associated with the finding.
    pub source: Option<PathBuf>,
    /// Source byte span, when the finding maps to text.
    pub span: Option<ByteSpan>,
    /// `KiCad` sheet path for hierarchical designs.
    pub sheet_path: Option<String>,
    /// Physical schematic location in millimetres.
    pub schematic_location: Option<SchematicLocation>,
    /// Additional affected items.
    pub related: Vec<RelatedLocation>,
    /// Whether `KiCad` marked the finding as excluded.
    pub excluded: bool,
}

impl Diagnostic {
    /// Creates a diagnostic with the supplied code, severity, and message; source mapping and optional context start unset.
    ///
    /// # Arguments
    ///
    /// * `code` — Stable diagnostic code, such as a native ERC identifier or a built-in `SCH` rule ID.
    /// * `severity` — Severity to assign to the finding.
    /// * `message` — Concise message shown to the user for this finding.
    ///
    /// # Returns
    ///
    /// A diagnostic with empty source location, explanation, help, related-item list, and exclusion state.
    #[must_use]
    pub fn new(code: impl Into<String>, severity: Severity, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            severity,
            message: message.into(),
            explanation: None,
            help: None,
            source: None,
            span: None,
            sheet_path: None,
            schematic_location: None,
            related: Vec::new(),
            excluded: false,
        }
    }

    /// Creates a diagnostic using a typed built-in rule ID, avoiding a duplicated string literal at the call site.
    ///
    /// # Arguments
    ///
    /// * `rule` — Built-in rule identifier; its stable string code is stored in the diagnostic.
    /// * `severity` — Severity to assign to the finding.
    /// * `message` — Concise message shown to the user for this finding.
    ///
    /// # Returns
    ///
    /// A diagnostic whose `code` field is the canonical code for `rule`.
    #[must_use]
    pub fn for_builtin_rule(
        rule: BuiltinRuleId,
        severity: Severity,
        message: impl Into<String>,
    ) -> Self {
        Self::new(rule, severity, message)
    }

    /// Determines whether this finding should make the lint run fail; excluded errors are inactive.
    ///
    /// # Returns
    ///
    /// `true` only when severity is `Error` and the diagnostic is not excluded.
    #[must_use]
    pub const fn is_active_error(&self) -> bool {
        matches!(self.severity, Severity::Error) && !self.excluded
    }
}
#[cfg(test)]
/// Unit tests for diagnostic severity, exclusion handling, and built-in rule codes.
mod tests {
    use super::{Diagnostic, Severity};
    use crate::BuiltinRuleId;
    /// Confirms excluded error diagnostics are retained but do not count as active errors.
    #[test]
    fn excluded_errors_do_not_fail_a_run() {
        let mut diagnostic = Diagnostic::new("ERC/EXCLUDED", Severity::Error, "ignored");
        diagnostic.excluded = true;

        assert!(!diagnostic.is_active_error());
    }
    /// Confirms a diagnostic created from `BuiltinRuleId` stores the corresponding canonical `SCHnnn` code.
    #[test]
    fn builtin_diagnostic_keeps_the_stable_external_code() {
        let diagnostic = Diagnostic::for_builtin_rule(
            BuiltinRuleId::Sch001,
            Severity::Error,
            "wire is diagonal",
        );
        assert_eq!(diagnostic.code, "SCH001");
    }
}
