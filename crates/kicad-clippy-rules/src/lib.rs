#![forbid(unsafe_code)]

//! Built-in schematic rules, project profiles, and the extensible rule-pass API.

use std::collections::{BTreeMap, btree_map::Entry};

use kicad_clippy_core::Diagnostic;
use kicad_clippy_parser::SchematicDocument;
use thiserror::Error;

mod generic;
mod metadata;

pub use generic::{
    ComponentFact, RuleRun, SchematicFacts, SchematicFieldFact, SchematicFrameFact,
    SchematicGroupFact, SchematicLabelFact, SchematicPinFact, SchematicPrimitiveFact,
    SchematicWireFact, SchematicTextFact, TextBoxFact, physical_wire_component_ids,
    run_builtin_rules,
};
pub use metadata::{
    CommonPinNetGroup, ComponentIntent, ComponentKind, ComponentPurpose, ComponentRole,
    DecouplingIntent, DirectWirePair, GroupEvidence, GroupIntent, LintProfile, MetadataError,
    ProfileMode, SeparateWirePair,
};

/// Immutable data shared by all rule passes for one schematic.
pub struct RuleContext<'a> {
    document: &'a SchematicDocument,
    facts: &'a SchematicFacts,
    segments: &'a [generic::Segment],
    junctions: &'a [generic::Point],
}

impl<'a> RuleContext<'a> {
    /// Returns the lossless parsed schematic.
    #[must_use]