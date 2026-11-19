#![forbid(unsafe_code)]

//! Shared data contracts for the kicad-clippy schematic linter.
/// Contains the diagnostic implementation and supporting types.
pub mod diagnostic;
/// Contains the rule id implementation and supporting types.
pub mod rule_id;
/// Contains the rules implementation and supporting types.
pub mod rules;
/// Contains the source implementation and supporting types.
pub mod source;

pub use diagnostic::{Diagnostic, RelatedLocation, SchematicLocation, Severity};
pub use rule_id::{BuiltinRuleId, UnknownBuiltinRuleId};
pub use rules::{RuleDescriptor, RuleLevel, RuleRegistry, RuleRegistryError};
pub use source::{ByteSpan, SourceFile};
