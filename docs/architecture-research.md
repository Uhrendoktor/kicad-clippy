# Linter architecture research

This document records the design evidence used to evolve `kicad-clippy`. The goal is to reuse mature infrastructure where it fits while preserving KiCad-specific electrical and geometric semantics.

## Findings from established lint systems

### Rust compiler and Clippy

The Rust compiler separates a lint's declaration (stable name, default level and documentation) from the pass that detects it. A lint may be emitted by zero, one or several passes; there is deliberately no one-to-one relationship between a lint ID and an implementation. Registration is centralized in a `LintStore`, duplicate names are rejected, registration completes before analysis, and the store is then shared immutably. Passes are grouped by the phase and information they need (syntax, resolved structures, type information, MIR); checks that need information produced elsewhere can run at that point rather than forcing every lint into one visitor.

Sources:
- [Rust Compiler Development Guide: LintStore](https://rustc-dev-guide.rust-lang.org/diagnostics/lintstore.html)
- [Rust Compiler Development Guide: Errors and lints](https://rustc-dev-guide.rust-lang.org/diagnostics.html)
- [Rust compiler lint crate](https://doc.rust-lang.org/nightly/nightly-rustc/rustc_lint/index.html)

Implications:
1. Keep rule identity and metadata independent from the check implementation.
2. Allow one analysis pass to emit diagnostics for multiple rule IDs, and allow a rule to consume several shared analyses.
3. Register built-ins in one place, reject duplicates, then freeze the registry before running checks.
4. Schedule checks according to required facts: parsed syntax, resolved symbol geometry, physical wire topology, native netlist, or profile metadata.
5. Do not equate a rule ID with a Rust module or a single function.

Clippy's in-process compiler integration is not a suitable plugin ABI for this standalone application. Reusing the architecture is useful; depending on rustc-private crates is not.

## Existing Rust libraries

| Concern | Library | Adopted use / decision |
| --- | --- | --- |
| Lossless S-expression parsing and source spans | [`kiutils_sexpr`](https://docs.rs/kiutils_sexpr/0.1.1/) | Already supplies a lossless CST with byte spans and round-trip validation. Keep it; replacing it with a generic CST would add grammar work without removing the parser boundary. |
| Diagnostic source snippets and terminal rendering | [`miette`](https://docs.rs/miette/7.6.0/miette/) | Adopted in the CLI for diagnostic codes, severity, source labels, source excerpts and help text. The JSON `Diagnostic` remains the stable interchange contract; the adapter stays at the CLI boundary. |
| Alternative file/span diagnostics | [`codespan-reporting`](https://docs.rs/codespan-reporting/latest/codespan_reporting/) | Not added. `miette` already supplies the needed terminal rendering; maintaining two source-map/rendering systems would duplicate responsibilities. Reconsider only if multi-file editor/source-map requirements exceed miette's model. |
| Planar geometry algorithms | [`geo`](https://docs.rs/geo/0.31.0/geo/) | Adopted for line intersections, Bentley–Ottmann sweep enumeration, point-to-segment distance, point-in-polygon, and rectangle/primitive predicates. Application code retains KiCad's millimetre tolerance policy, pin exemptions, explicit junction semantics and diagnostic wording. |
| Curves and affine transforms | [`kurbo`](https://docs.rs/kurbo/0.13.1/kurbo/) | Adopted for adaptive Bézier/arc flattening and symbol-local affine transforms. KiCad's three-point arc interpretation remains an adapter that constructs a curve; curve subdivision and matrix math are delegated. |
| Spatial candidate indexing | [`rstar`](https://docs.rs/rstar/0.12.2/rstar/) | Not directly adopted. Profile the remaining clearance scans first; add an R*-tree only where it measurably replaces repeated all-pairs candidate enumeration. It must remain a broad phase followed by exact rule-specific predicates. |
| Robust predicates | [`robust`](https://docs.rs/robust/1.2.0/robust/) | Not added directly. The adopted geometry stack already has robust-predicate support in its dependency graph; a second numerical-geometry implementation would need an identified uncovered case. |

The library choices above are candidates, not blanket replacements. In particular, geospatial defaults and epsilon rules must not silently change the project's millimetre coordinate model or KiCad's explicit-junction connectivity contract.


## Substitution results and code-reduction rule

The library substitutions are deliberately limited to generic algorithms. In the reviewed PR baseline, `generic.rs` was 3,523 lines / 115,651 bytes. After the `geo` and `kurbo` substitutions, the same module was 3,434 lines / 113,959 bytes: a net reduction of 89 lines and 1,692 bytes, including added regression coverage. `SourceFile` also dropped from roughly 4.9 KB to 2.8 KB by removing its duplicate line-start index; `miette` now computes display locations from the retained byte spans and shared source buffer.

This is an initial measurable reduction, not a claim that the remaining rules are fully library-backed. Keep application-specific code when it encodes KiCad semantics; prefer a library when the code is a general-purpose algorithm (curve flattening, affine transforms, polygon predicates, source rendering). Do not add dependencies solely to hide a short, stable KiCad-specific predicate.

## Target architecture

Use a staged, immutable analysis pipeline:

```text
source + CLI rule policy + optional project profile
                  |
                  v
       lossless CST + source map
                  |
                  v
    typed schematic model / UUID index
                  |
        +---------+----------+
        |                    |
        v                    v
 geometry analysis      connectivity analysis
        |                    |
        +---------+----------+
                  |
          shared RuleContext
                  |
       phase-specific rule passes
                  |
          normalized findings
                  |
      text / JSON / SARIF renderers
```

### Responsibilities

- **Parser:** preserve original source, CST nodes, spans and parse errors.
- **Semantic model:** resolve sheet hierarchy, placed symbols, embedded library symbols, UUIDs and typed object references once.
- **Geometry analysis:** expose normalized points, segments, bounds, arcs and curves in millimetres; centralize tolerances and intersection semantics.
- **Connectivity analysis:** distinguish physical wire-only components from KiCad netlist membership and label semantics. Never let labels create physical edges.
- **Rule engine:** own rule registration, effective levels, typed options, pass dependencies and deterministic execution.
- **Profiles:** hold project-specific intent and contracts; keep generic rule policy separate from component metadata.
- **Diagnostics:** use stable typed built-in IDs, retain external ERC codes, spans, sheet paths, UUIDs and physical locations.
- **Renderers:** consume normalized diagnostics; terminal formatting must not alter diagnostic meaning.
- **Fix planner:** propose structured, preconditioned edits separately from read-only analysis; verify by reparsing and rerunning relevant checks.

## Rule IDs and extensibility

Built-in rules are a closed, compile-time set and should be referenced through `BuiltinRuleId`, not repeated string literals. Their string forms remain stable external codes for JSON, documentation and integrations. Native ERC codes and future third-party rule namespaces are open-ended external identifiers and should remain a separate representation.

A built-in rule ID is not a plugin entry point. An initial in-process RulePass API over immutable shared facts is now implemented; external Rust crates can register passes programmatically. Before treating it as a stable third-party contract, version the trait/context, formalize typed options and analysis dependencies, and define diagnostic compatibility policy. Third-party dynamic libraries or WASM rule packs should wait until those contracts and resource limits are stable.

Configuration should use structured rule keys and typed per-rule option structs, with a small set of named presets. Unknown rule IDs, invalid option values, duplicate declarations and conflicting overrides should fail with actionable configuration errors rather than being silently ignored.

## Migration sequence

1. Establish typed built-in identifiers and one metadata catalogue.
2. Continue extracting reusable schematic facts, geometry primitives, tolerance policy and wire topology from the remaining large rule implementation module.
3. Build shared indexes once per schematic; use spatial indexes only where profiling demonstrates an all-pairs bottleneck.
4. Extend the initial geometry, label and annotation RulePass implementations into semantic, connectivity and project-contract phases over an immutable RuleContext.
5. Add separate generic rule-policy configuration and validated per-rule options.
6. Add source-labeled terminal rendering and SARIF as adapters over the existing diagnostic contract.
7. Add verified fix proposals only after edit preconditions and post-fix connectivity checks exist.
8. Consider external rule packs only after the in-process API is stable and documented.

## Explicit non-goals

- Replacing KiCad native ERC or inferring electrical correctness from geometry alone.
- Replacing the existing lossless parser without a demonstrated correctness or maintenance benefit.
- Introducing a dynamic plugin ABI before the rule API is stable.
- Replacing every custom geometry predicate with a geospatial library without checking coordinate, topology and tolerance semantics.
