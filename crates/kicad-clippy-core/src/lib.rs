#![forbid(unsafe_code)]

//! Shared data contracts for the kicad-clippy schematic linter.
/// Defines the normalized severity, source location, and related-item structures shared by ERC and schematic rule findings.
pub mod diagnostic;
/// Defines stable `SCHnnn` identifiers and their canonical string conversion for built-in schematic checks.
pub mod rule_id;
/// Defines configurable rule metadata, the complete built-in descriptor catalogue, TOML-compatible rule policy contracts, severity levels, and the deterministic registry used by rule integrations.
pub mod rules;
/// Defines half-open source byte spans and the reference-counted original text buffer used to render diagnostics.
pub mod source;

pub use diagnostic::{Diagnostic, RelatedLocation, SchematicLocation, Severity};
pub use rule_id::{BuiltinRuleId, UnknownBuiltinRuleId};
pub use rules::{
    RuleDescriptor, RuleLevel, RulePolicy, RulePolicyError, RulePreset, RuleRegistry,
    RuleRegistryError, builtin_rule_registry,
};
pub use source::{ByteSpan, SourceFile};
