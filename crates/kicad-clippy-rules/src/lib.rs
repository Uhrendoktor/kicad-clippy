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
    SchematicWireFact, TextBoxFact, physical_wire_component_ids, run_builtin_rules,
};
pub use metadata::{
    CommonPinNetGroup, ComponentIntent, ComponentKind, ComponentPurpose, ComponentRole,
    DecouplingIntent, DirectWirePair, GroupEvidence, GroupIntent, LintProfile, MetadataError,
    ProfileMode,
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
    pub const fn document(&self) -> &'a SchematicDocument {
        self.document
    }

    /// Returns semantic and geometric facts collected once for this run.
    #[must_use]
    pub const fn facts(&self) -> &'a SchematicFacts {
        self.facts
    }
}

/// A deterministic analysis pass that can emit diagnostics for one or more rules.
///
/// Rule passes are distinct from rule IDs: a pass may emit findings for several
/// rules, and a rule may be implemented by several passes.
pub trait RulePass: Send + Sync {
    /// Unique, stable pass name used for deterministic registration and execution.
    fn name(&self) -> &'static str;

    /// Analyze an immutable context and return normalized findings.
    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic>;
}

/// Error returned when a pass name is registered more than once.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("rule pass {0:?} is already registered")]
pub struct RuleEngineError(pub String);

/// Runs registered rule passes over one shared schematic context.
#[derive(Default)]
pub struct RuleEngine {
    passes: BTreeMap<&'static str, Box<dyn RulePass>>,
}

impl RuleEngine {
    /// Creates an empty engine for application- or plugin-provided rule passes.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an engine with all built-in schematic rule passes registered.
    #[must_use]
    pub fn with_builtins() -> Self {
        let mut engine = Self::new();
        for pass in [
            Box::new(generic::BuiltinGeometryPass) as Box<dyn RulePass>,
            Box::new(generic::BuiltinLabelPass),
            Box::new(generic::BuiltinAnnotationPass),
        ] {
            engine.passes.insert(pass.name(), pass);
        }
        engine
    }

    /// Registers an in-process rule pass.
    ///
    /// External rule crates can implement `RulePass` and register their pass
    /// without changing the built-in rule catalogue.
    ///
    /// # Errors
    ///
    /// Returns `RuleEngineError` if a pass with the same name is already registered.
    pub fn register_pass(&mut self, pass: impl RulePass + 'static) -> Result<(), RuleEngineError> {
        match self.passes.entry(pass.name()) {
            Entry::Vacant(entry) => {
                entry.insert(Box::new(pass));
                Ok(())
            }
            Entry::Occupied(entry) => Err(RuleEngineError((*entry.key()).to_owned())),
        }
    }

    /// Executes all registered passes in stable name order.
    #[must_use]
    pub fn run(&self, document: &SchematicDocument) -> RuleRun {
        let Some(root) = document.cst().nodes.first() else {
            return RuleRun::default();
        };
        let segments = generic::collect_segments(root);
        let junctions = generic::collect_points(root, "junction");
        let facts = SchematicFacts::from_document_with_geometry(document, &segments, &junctions);
        let context = RuleContext {
            document,
            facts: &facts,
            segments: &segments,
            junctions: &junctions,
        };
        let diagnostics = self
            .passes
            .values()
            .flat_map(|pass| pass.run(&context))
            .collect();

        RuleRun {
            diagnostics,
            wires: segments.len(),
            components: facts.components.len(),
        }
    }

    /// Returns the number of registered passes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.passes.len()
    }

    /// Returns whether no passes are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.passes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use kicad_clippy_core::{Diagnostic, Severity};
    use kicad_clippy_parser::SchematicDocument;

    use super::{RuleContext, RuleEngine, RuleEngineError, RulePass};

    struct ExternalExamplePass;

    impl RulePass for ExternalExamplePass {
        fn name(&self) -> &'static str {
            "example.external"
        }

        fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
            let message = format!("analyzed {} component(s)", context.facts().components.len());
            vec![Diagnostic::new("EXAMPLE/EXTERNAL", Severity::Note, message)]
        }
    }

    #[test]
    fn external_passes_share_context_and_run_deterministically() -> Result<(), Box<dyn Error>> {
        let document = SchematicDocument::from_text("fixture.kicad_sch", "(kicad_sch)")?;
        let mut engine = RuleEngine::new();
        engine.register_pass(ExternalExamplePass)?;
        assert_eq!(engine.len(), 1);

        let result = engine.run(&document);
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].code, "EXAMPLE/EXTERNAL");
        assert!(result.diagnostics[0].message.contains("0 component(s)"));
        Ok(())
    }

    #[test]
    fn duplicate_pass_names_are_rejected() -> Result<(), Box<dyn Error>> {
        let mut engine = RuleEngine::new();
        engine.register_pass(ExternalExamplePass)?;
        assert_eq!(
            engine.register_pass(ExternalExamplePass),
            Err(RuleEngineError("example.external".to_owned()))
        );
        Ok(())
    }

    #[test]
    fn built_in_engine_registers_and_runs_its_passes() -> Result<(), Box<dyn Error>> {
        let document = SchematicDocument::from_text("fixture.kicad_sch", "(kicad_sch)")?;
        let engine = RuleEngine::with_builtins();
        assert_eq!(engine.len(), 3);
        assert!(
            !engine
                .run(&document)
                .diagnostics
                .iter()
                .any(|item| item.code.starts_with("SCH"))
        );
        Ok(())
    }
}
