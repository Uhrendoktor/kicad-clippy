#![forbid(unsafe_code)]

//! Built-in schematic rules, project profiles, and the extensible rule-pass API.

use std::collections::{BTreeMap, btree_map::Entry};

use kicad_clippy_core::Diagnostic;
use kicad_clippy_parser::SchematicDocument;
use thiserror::Error;
/// Contains the generic implementation and supporting types.
mod generic;
/// Contains the metadata implementation and supporting types.
mod metadata;

pub use generic::{
    ComponentFact, RuleRun, SchematicArcFact, SchematicFacts, SchematicFieldFact,
    SchematicFrameFact, SchematicGraphicFact, SchematicGroupFact, SchematicImageFact,
    SchematicLabelFact, SchematicPinFact, SchematicPrimitiveFact, SchematicSymbolTextFact,
    SchematicTextFact, SchematicWireFact, TextBoxFact, physical_wire_component_ids,
    physical_wire_component_key, run_builtin_rules,
};
pub use metadata::{
    CommonPinNetGroup, ComponentIntent, ComponentKind, ComponentPurpose, ComponentRole,
    DecouplingIntent, DirectWirePair, GroupEvidence, GroupIntent, LintProfile, MetadataError,
    PhysicalWireGroup, ProfileMode, SeparateWirePair,
};

/// Immutable data shared by all rule passes for one schematic.
pub struct RuleContext<'a> {
    /// Stores the `document` value.
    document: &'a SchematicDocument,
    /// Stores the `facts` value.
    facts: &'a SchematicFacts,
    /// Stores the `segments` value.
    segments: &'a [generic::Segment],
    /// Stores the `junctions` value.
    junctions: &'a [generic::Point],
    /// Stores the `default_wire_stroke_width_mm` value.
    default_wire_stroke_width_mm: Option<f64>,
    /// Stores the `project_wire_stroke_widths_mm` value.
    project_wire_stroke_widths_mm: &'a BTreeMap<usize, f64>,
    /// Stores the `default_schematic_line_stroke_width_mm` value.
    default_schematic_line_stroke_width_mm: Option<f64>,
}

impl<'a> RuleContext<'a> {
    /// Returns the lossless parsed schematic.
    /// # Returns
    ///
    /// The value produced by this operation.
    ///
    #[must_use]
    pub const fn document(&self) -> &'a SchematicDocument {
        self.document
    }

    /// Returns semantic and geometric facts collected once for this run.
    /// # Returns
    ///
    /// The value produced by this operation.
    ///
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
    /// # Returns
    ///
    /// The value produced by this operation.
    fn name(&self) -> &'static str;

    /// Analyze an immutable context and return normalized findings.
    /// # Arguments
    ///
    /// * `context` — Input `context` used by this operation.
    ///
    /// # Returns
    ///
    /// The value produced by this operation.
    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic>;
}

/// Error returned when a pass name is registered more than once.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("rule pass {0:?} is already registered")]
pub struct RuleEngineError(
    /// Name of the pass that was already registered.
    pub String,
);

/// Runs registered rule passes over one shared schematic context.
#[derive(Default)]
pub struct RuleEngine {
    /// Stores the `passes` value.
    /// Stores the `passes` value.
    passes: BTreeMap<&'static str, Box<dyn RulePass>>,
}

impl RuleEngine {
    /// Creates an empty engine for application- or plugin-provided rule passes.
    /// # Returns
    ///
    /// The value produced by this operation.
    ///
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an engine with all built-in schematic rule passes registered.
    /// # Returns
    ///
    /// The value produced by this operation.
    ///
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
    /// # Arguments
    ///
    /// * `pass` — Input `pass` used by this operation.
    ///
    /// # Returns
    ///
    /// The value produced by this operation.
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
    /// # Arguments
    ///
    /// * `document` — Input `document` used by this operation.
    ///
    /// # Returns
    ///
    /// The value produced by this operation.
    ///
    #[must_use]
    pub fn run(&self, document: &SchematicDocument) -> RuleRun {
        self.run_with_default_wire_stroke_width(document, None)
    }

    /// Executes all passes with a project-derived default SCH013 wire stroke width.
    ///
    /// The caller must supply a value only when it can establish that the
    /// project's effective Default netclass width applies to the schematic.
    /// # Arguments
    ///
    /// * `document` — Input `document` used by this operation.
    /// * `default_wire_stroke_width_mm` — Input `default wire stroke width mm` used by this operation.
    ///
    /// # Returns
    ///
    /// The value produced by this operation.
    ///
    #[must_use]
    pub fn run_with_default_wire_stroke_width(
        &self,
        document: &SchematicDocument,
        default_wire_stroke_width_mm: Option<f64>,
    ) -> RuleRun {
        self.run_with_project_stroke_widths(document, default_wire_stroke_width_mm, None)
    }

    /// Executes all passes with per-wire netclass stroke widths from a resolved project/netlist.
    ///
    /// Only wire source offsets present in the map receive a project-derived width.
    /// # Arguments
    ///
    /// * `document` — Input `document` used by this operation.
    /// * `project_wire_stroke_widths_mm` — Input `project wire stroke widths mm` used by this operation.
    /// * `default_schematic_line_stroke_width_mm` — Input `default schematic line stroke width mm` used by this operation.
    ///
    /// # Returns
    ///
    /// The value produced by this operation.
    ///
    #[must_use]
    pub fn run_with_wire_stroke_widths(
        &self,
        document: &SchematicDocument,
        project_wire_stroke_widths_mm: &BTreeMap<usize, f64>,
        default_schematic_line_stroke_width_mm: Option<f64>,
    ) -> RuleRun {
        self.run_with_project_and_wire_stroke_widths(
            document,
            None,
            project_wire_stroke_widths_mm,
            default_schematic_line_stroke_width_mm,
        )
    }

    /// Executes all passes with authoritative project-derived SCH013 stroke widths.
    ///
    /// The schematic line width supplies the pen around fixed-size no-connect markers.
    /// It remains unknown when the project settings do not provide a positive value.
    /// # Arguments
    ///
    /// * `document` — Input `document` used by this operation.
    /// * `default_wire_stroke_width_mm` — Input `default wire stroke width mm` used by this operation.
    /// * `default_schematic_line_stroke_width_mm` — Input `default schematic line stroke width mm` used by this operation.
    ///
    /// # Returns
    ///
    /// The value produced by this operation.
    ///
    #[must_use]
    pub fn run_with_project_stroke_widths(
        &self,
        document: &SchematicDocument,
        default_wire_stroke_width_mm: Option<f64>,
        default_schematic_line_stroke_width_mm: Option<f64>,
    ) -> RuleRun {
        self.run_with_project_and_wire_stroke_widths(
            document,
            default_wire_stroke_width_mm,
            &BTreeMap::new(),
            default_schematic_line_stroke_width_mm,
        )
    }
    /// Processes run with project and wire stroke widths using the supplied values and schematic data.
    ///
    /// # Arguments
    ///
    /// * `document` — Input `document` used by this operation.
    /// * `default_wire_stroke_width_mm` — Input `default wire stroke width mm` used by this operation.
    /// * `project_wire_stroke_widths_mm` — Input `project wire stroke widths mm` used by this operation.
    /// * `default_schematic_line_stroke_width_mm` — Input `default schematic line stroke width mm` used by this operation.
    ///
    /// # Returns
    ///
    /// The value produced by this operation.
    fn run_with_project_and_wire_stroke_widths(
        &self,
        document: &SchematicDocument,
        default_wire_stroke_width_mm: Option<f64>,
        project_wire_stroke_widths_mm: &BTreeMap<usize, f64>,
        default_schematic_line_stroke_width_mm: Option<f64>,
    ) -> RuleRun {
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
            default_wire_stroke_width_mm: default_wire_stroke_width_mm
                .filter(|width| width.is_finite() && *width > 0.0),
            project_wire_stroke_widths_mm,
            default_schematic_line_stroke_width_mm: default_schematic_line_stroke_width_mm
                .filter(|width| width.is_finite() && *width > 0.0),
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
    /// # Returns
    ///
    /// The value produced by this operation.
    ///
    #[must_use]
    pub fn len(&self) -> usize {
        self.passes.len()
    }

    /// Returns whether no passes are registered.
    /// # Returns
    ///
    /// The value produced by this operation.
    ///
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.passes.is_empty()
    }
}
/// Contains the tests implementation and supporting types.

#[cfg(test)]
mod tests {
    use std::error::Error;

    use kicad_clippy_core::{Diagnostic, Severity};
    use kicad_clippy_parser::SchematicDocument;

    use super::{RuleContext, RuleEngine, RuleEngineError, RulePass};
    /// Stores externalexamplepass data used by this crate.
    struct ExternalExamplePass;

    impl RulePass for ExternalExamplePass {
        /// Processes name using the supplied values and schematic data.
        ///
        /// # Returns
        ///
        /// The value produced by this operation.
        fn name(&self) -> &'static str {
            "example.external"
        }
        /// Processes run using the supplied values and schematic data.
        ///
        /// # Arguments
        ///
        /// * `context` — Input `context` used by this operation.
        ///
        /// # Returns
        ///
        /// The value produced by this operation.
        fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
            let message = format!("analyzed {} component(s)", context.facts().components.len());
            vec![Diagnostic::new("EXAMPLE/EXTERNAL", Severity::Note, message)]
        }
    }
    /// Processes external passes share context and run deterministically using the supplied values and schematic data.
    ///
    /// # Returns
    ///
    /// The value produced by this operation.
    ///
    /// # Errors
    ///
    /// Returns an error when the operation cannot complete with the supplied input.
    ///

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
    /// Processes duplicate pass names are rejected using the supplied values and schematic data.
    ///
    /// # Returns
    ///
    /// The value produced by this operation.
    ///
    /// # Errors
    ///
    /// Returns an error when the operation cannot complete with the supplied input.
    ///

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
    /// Processes built in engine registers and runs its passes using the supplied values and schematic data.
    ///
    /// # Returns
    ///
    /// The value produced by this operation.
    ///
    /// # Errors
    ///
    /// Returns an error when the operation cannot complete with the supplied input.
    ///

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
