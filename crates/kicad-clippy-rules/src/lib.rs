#![forbid(unsafe_code)]

//! Built-in schematic rules, project profiles, and the extensible rule-pass API.

use std::collections::{BTreeMap, btree_map::Entry};

use kicad_clippy_core::Diagnostic;
use kicad_clippy_parser::SchematicDocument;
use thiserror::Error;

/// Dimensionless multiplier representing one half.
pub(crate) const HALF_FACTOR: f64 = 0.5;
/// Factor used when a dimension applies symmetrically to both sides of an object.
pub(crate) const TWO_SIDED_FACTOR: f64 = 2.0;
/// Extracts shared schematic facts and orchestrates the dedicated geometry, connectivity, layout, and obstacle rule modules.
mod generic;
/// Defines the shared project-profile schema and orchestrates its dedicated connectivity, group, component-intent, and decoupling validators.
mod metadata;

pub use generic::{
    ComponentFact, MILLIMETRES_PER_MIL, RuleRun, SchematicArcFact, SchematicFacts,
    SchematicFieldFact, SchematicFrameFact, SchematicGraphicFact, SchematicGroupFact,
    SchematicImageFact, SchematicLabelFact, SchematicPinFact, SchematicPrimitiveFact,
    SchematicSymbolTextFact, SchematicTextFact, SchematicWireFact, TextBoxFact,
    physical_wire_component_ids, physical_wire_component_key, run_builtin_rules,
};
pub use metadata::{
    CommonPinNetGroup, ComponentIntent, ComponentKind, ComponentPurpose, ComponentRole,
    DecouplingIntent, DirectWirePair, GroupEvidence, GroupIntent, LintProfile, MetadataError,
    PhysicalWireGroup, ProfileMode, SeparateWirePair,
};

/// Immutable data shared by all rule passes for one schematic.
pub struct RuleContext<'a> {
    /// Original lossless schematic document shared with every pass, including its syntax tree and source byte spans.
    document: &'a SchematicDocument,
    /// Normalized component, pin, wire, label, graphic, and annotation facts extracted for this run.
    facts: &'a SchematicFacts,
    /// Straight wire segments with source spans, reused by topology and obstacle checks.
    segments: &'a [generic::Segment],
    /// Explicit sheet-space junction coordinates used to distinguish connected crossings from unmarked crossings.
    junctions: &'a [generic::Point],
    /// Effective Default netclass wire width in millimetres, when the project resolver establishes it.
    default_wire_stroke_width_mm: Option<f64>,
    /// Per-wire effective widths in millimetres, keyed by each wire's source byte offset.
    project_wire_stroke_widths_mm: &'a BTreeMap<usize, f64>,
    /// Project default schematic line width in millimetres, used for strokes whose width is inherited.
    default_schematic_line_stroke_width_mm: Option<f64>,
}

impl<'a> RuleContext<'a> {
    /// Returns the lossless parsed schematic shared with this rule pass.
    ///
    /// # Returns
    ///
    /// The original `SchematicDocument`, including the syntax tree and source spans.
    #[must_use]
    pub const fn document(&self) -> &'a SchematicDocument {
        self.document
    }

    /// Returns the normalized facts collected once for the current schematic analysis.
    ///
    /// # Returns
    ///
    /// Placed components, pins, wires, labels, frames, graphics, images, and their resolved geometry.
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
    /// Returns the stable, unique name used to register and order this rule pass.
    ///
    /// # Returns
    ///
    /// A static identifier. The engine rejects duplicate names and executes passes in lexicographic name order.
    fn name(&self) -> &'static str;

    /// Runs this pass against the shared immutable context and returns findings produced by its checks.
    ///
    /// # Arguments
    ///
    /// * `context` — Per-run schematic facts, source document, wire geometry, and project-derived stroke widths.
    ///
    /// # Returns
    ///
    /// Diagnostics emitted by this pass. An empty vector means that this pass found no reportable issue.
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
    /// Rule implementations keyed by stable pass name; the ordered map preserves deterministic execution order.
    passes: BTreeMap<&'static str, Box<dyn RulePass>>,
}

impl RuleEngine {
    /// Creates an empty rule engine for built-in or externally supplied rule passes.
    ///
    /// # Returns
    ///
    /// A rule engine with no registered passes.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a rule engine containing the built-in geometry, label-topology, and annotation passes.
    ///
    /// # Returns
    ///
    /// A ready-to-run engine with all built-in passes registered under their stable names.
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

    /// Registers an in-process rule pass under its stable name. The engine rejects duplicate names and uses the name to preserve deterministic execution order.
    ///
    /// # Arguments
    ///
    /// * `pass` — Rule-pass implementation that the engine will own and execute with its other registered passes.
    ///
    /// # Returns
    ///
    /// `Ok(())` when the pass is inserted.
    ///
    /// # Errors
    ///
    /// Returns `RuleEngineError` when another pass already uses the same name.
    pub fn register_pass(&mut self, pass: impl RulePass + 'static) -> Result<(), RuleEngineError> {
        match self.passes.entry(pass.name()) {
            Entry::Vacant(entry) => {
                entry.insert(Box::new(pass));
                Ok(())
            }
            Entry::Occupied(entry) => Err(RuleEngineError((*entry.key()).to_owned())),
        }
    }

    /// Executes all registered passes for the supplied schematic using one shared rule context.
    ///
    /// # Arguments
    ///
    /// * `document` — Parsed schematic whose lossless source tree and sheet geometry are analyzed.
    ///
    /// # Returns
    ///
    /// A `RuleRun` containing diagnostics and counts of the analyzed wires and placed components.
    #[must_use]
    pub fn run(&self, document: &SchematicDocument) -> RuleRun {
        self.run_with_default_wire_stroke_width(document, None)
    }

    /// Executes all registered passes with an optional project-derived Default netclass wire width.
    ///
    /// # Arguments
    ///
    /// * `document` — Parsed `KiCad` schematic whose placed symbols, wires, labels, and sheet graphics are passed to the registered rule implementations.
    /// * `default_wire_stroke_width_mm` — Positive finite Default netclass wire width in millimetres; `None` leaves the inherited width unknown.
    ///
    /// # Returns
    ///
    /// A `RuleRun` containing combined diagnostics and schematic object counts.
    #[must_use]
    pub fn run_with_default_wire_stroke_width(
        &self,
        document: &SchematicDocument,
        default_wire_stroke_width_mm: Option<f64>,
    ) -> RuleRun {
        self.run_with_project_stroke_widths(document, default_wire_stroke_width_mm, None)
    }

    /// Executes all registered passes with project-resolved stroke widths for individual wire objects.
    ///
    /// # Arguments
    ///
    /// * `document` — Parsed `KiCad` schematic whose source tree and sheet geometry are analyzed by the registered rule implementations.
    /// * `project_wire_stroke_widths_mm` — Map from each wire's source byte offset to its effective width in millimetres; unresolved wires are absent from the map.
    /// * `default_schematic_line_stroke_width_mm` — Optional positive finite project line width in millimetres for schematic strokes whose pen width is inherited.
    ///
    /// # Returns
    ///
    /// A `RuleRun` containing diagnostics and wire/component counts for this schematic.
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

    /// Executes all registered passes with project-derived default wire and schematic-line widths, without assigning per-wire overrides.
    ///
    /// # Arguments
    ///
    /// * `document` — Parsed `KiCad` schematic whose source tree and sheet geometry are analyzed by the registered rule implementations.
    /// * `default_wire_stroke_width_mm` — Optional effective Default netclass wire width in millimetres.
    /// * `default_schematic_line_stroke_width_mm` — Optional project schematic-line width in millimetres, used for graphic strokes whose width is inherited.
    ///
    /// # Returns
    ///
    /// A `RuleRun` containing diagnostics and counts derived from the parsed schematic.
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
    /// Builds one shared schematic context, applies only validated project stroke widths, and executes every registered pass in stable name order.
    ///
    /// # Arguments
    ///
    /// * `document` — Parsed schematic whose lossless syntax tree supplies source spans and placed-object definitions.
    /// * `default_wire_stroke_width_mm` — Optional effective Default netclass wire width in millimetres; non-finite and non-positive values are treated as unknown.
    /// * `project_wire_stroke_widths_mm` — Per-wire effective width map keyed by source byte offset. Include only wires with an unambiguous project, netlist, and physical-topology mapping.
    /// * `default_schematic_line_stroke_width_mm` — Optional positive finite schematic-line width in millimetres, used for the pen around fixed-size no-connect markers.
    ///
    /// # Returns
    ///
    /// A `RuleRun` containing all pass diagnostics and the number of wire segments and placed components analyzed.
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

    /// Returns the number of rule passes currently registered in the engine.
    ///
    /// # Returns
    ///
    /// The count of uniquely named pass implementations.
    #[must_use]
    pub fn len(&self) -> usize {
        self.passes.len()
    }

    /// Returns whether the engine has any registered rule passes.
    ///
    /// # Returns
    ///
    /// `true` when no pass is registered; otherwise `false`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.passes.is_empty()
    }
}
#[cfg(test)]
/// Exercises shared rule-context accessors, deterministic pass registration, and integration with externally supplied rule passes.
mod tests {
    use std::error::Error;

    use kicad_clippy_core::{Diagnostic, Severity};
    use kicad_clippy_parser::SchematicDocument;

    use super::{RuleContext, RuleEngine, RuleEngineError, RulePass};
    /// Test pass that emits one note containing the number of components visible in the shared rule context.
    struct ExternalExamplePass;

    impl RulePass for ExternalExamplePass {
        /// Returns the stable identifier used to register this example external pass.
        ///
        /// # Returns
        ///
        /// The unique static identifier `example.external` used by the rule engine for registration and deterministic pass ordering.
        fn name(&self) -> &'static str {
            "example.external"
        }
        /// Emits a note diagnostic reporting the number of placed components visible through the shared rule context.
        ///
        /// # Arguments
        ///
        /// * `context` — Immutable per-run context containing the normalized schematic component facts.
        ///
        /// # Returns
        ///
        /// A single informational diagnostic from the example external pass.
        fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
            let message = format!("analyzed {} component(s)", context.facts().components.len());
            vec![Diagnostic::new("EXAMPLE/EXTERNAL", Severity::Note, message)]
        }
    }
    /// Regression test: verifies that an external pass can be registered, receives the shared context, and runs in stable name order.
    ///
    /// # Returns
    ///
    /// `Ok(())` when registration succeeds and the emitted note reports the expected component count.
    ///
    /// # Errors
    ///
    /// Returns an error if the schematic fixture cannot be parsed or registration fails.
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
    /// Regression test: verifies that registering a second pass with the same stable name returns `RuleEngineError`.
    ///
    /// # Returns
    ///
    /// `Ok(())` when the duplicate-name error is returned as expected.
    ///
    /// # Errors
    ///
    /// Returns an error if the first registration or the fixture setup fails.
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
    /// Regression test: verifies that the built-in engine registers all three standard passes and can run them on an empty schematic without SCH diagnostics.
    ///
    /// # Returns
    ///
    /// `Ok(())` when all built-in passes are registered and the fixture produces no unexpected findings.
    ///
    /// # Errors
    ///
    /// Returns an error if the minimal schematic fixture cannot be parsed.
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
