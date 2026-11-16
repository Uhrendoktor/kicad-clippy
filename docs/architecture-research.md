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

| Concern | Candidate | Fit and recommendation |
| --- | --- | --- |
| Lossless S-expression parsing and source spans | Existing `kiutils_sexpr` | Already supplies a lossless CST with byte spans and round-trip validation. Keep it; replacing it with a generic CST would add grammar work without removing the current parser boundary. |
| Diagnostic protocol and source snippets | [miette](https://docs.rs/miette/latest/miette/) | Strong candidate for terminal rendering, labeled source spans, help text and diagnostic codes. Keep the project's JSON `Diagnostic` as the stable interchange contract; adapt it to miette at the CLI boundary rather than making a rendering crate own the schema. |
| File/span diagnostics | [codespan-reporting](https://docs.rs/codespan-reporting/latest/codespan_reporting/) | Good alternative when explicit multi-file source maps and byte-span labels are the central abstraction. Evaluate against miette before adding either; do not build two competing source registries. |
| Planar geometry algorithms | [geo](https://docs.rs/geo/latest/geo/) | Provides reusable line, polygon, containment, distance and intersection algorithms. Evaluate for generic primitives and broad-phase geometry operations. It does not encode KiCad's junction-marker, wire-island, pin-anchor or schematic-unit semantics. |
| Spatial candidate indexing | [rstar](https://docs.rs/rstar/latest/rstar/) | R*-tree with rectangle queries, nearest-neighbour and bulk loading. Strong candidate for avoiding repeated all-pairs scans in field/wire/primitive clearance checks. Use it only as a broad phase; retain exact rule-specific predicates after candidate retrieval. |
| Robust predicates | [robust](https://docs.rs/robust/latest/robust/) | Candidate for orientation and segment predicates where floating-point degeneracies matter. Integrate only with explicit tolerance policy and regression fixtures. |

The library choices above are candidates, not blanket replacements. In particular, geospatial defaults and epsilon rules must not silently change the project's millimetre coordinate model or KiCad's explicit-junction connectivity contract.

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

A built-in rule ID is not a plugin entry point. The first extensibility boundary should be an in-process, versioned rule interface over immutable shared facts and typed options. Third-party dynamic libraries or WASM rule packs should wait until the context API, configuration schema, diagnostic compatibility policy and resource limits are stable.

Configuration should use structured rule keys and typed per-rule option structs, with a small set of named presets. Unknown rule IDs, invalid option values, duplicate declarations and conflicting overrides should fail with actionable configuration errors rather than being silently ignored.

## Migration sequence

1. Establish typed built-in identifiers and one metadata catalogue.
2. Extract reusable schematic facts, geometry primitives, tolerance policy and wire topology from the monolithic rule module.
3. Build shared indexes once per schematic; use spatial indexes only where profiling demonstrates an all-pairs bottleneck.
4. Introduce phase-specific rule passes and an immutable `RuleContext`.
5. Add separate generic rule-policy configuration and validated per-rule options.
6. Add source-labeled terminal rendering and SARIF as adapters over the existing diagnostic contract.
7. Add verified fix proposals only after edit preconditions and post-fix connectivity checks exist.
8. Consider external rule packs only after the in-process API is stable and documented.

## Explicit non-goals

- Replacing KiCad native ERC or inferring electrical correctness from geometry alone.
- Replacing the existing lossless parser without a demonstrated correctness or maintenance benefit.
- Introducing a dynamic plugin ABI before the rule API is stable.
- Replacing every custom geometry predicate with a geospatial library without checking coordinate, topology and tolerance semantics.
