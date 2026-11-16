#![forbid(unsafe_code)]

//! Shared data contracts for the kicad-clippy schematic linter.

pub mod diagnostic;
pub mod rule_id;
pub mod rules;
pub mod source;

pub use diagnostic::{Diagnostic, RelatedLocation, SchematicLocation, Severity};
pub use rule_id::{BuiltinRuleId, UnknownBuiltinRuleId};
pub use rules::{RuleDescriptor, RuleLevel, RuleRegistry, RuleRegistryError};
pub use source::{ByteSpan, SourceFile};
