#![forbid(unsafe_code)]

//! Built-in schematic rules and project component-intent contracts.

mod generic;
mod metadata;

pub use generic::{RuleRun, run_builtin_rules};
pub use metadata::{ComponentIntent, ComponentKind, ComponentRole, DecouplingIntent, LintProfile, MetadataError, ProfileMode};
