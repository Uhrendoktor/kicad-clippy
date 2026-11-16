# Linter architecture research

This document records architectural research and the library choices actually adopted by `kicad-clippy`. The objective is to delete generic infrastructure from this repository and delegate it to maintained, purpose-built crates, while retaining only KiCad-specific rules and contracts.

## Established linter architecture

### Rust compiler and Clippy

The Rust compiler separates a lint's declaration (stable name, default level and documentation) from the pass that detects it. A lint may be emitted by zero, one or several passes; there is deliberately no one-to-one relationship between a lint ID and an implementation. Registration is centralized in a `LintStore`, duplicate names are rejected, registration completes before analysis, and the store is then shared immutably. Passes are grouped by the phase and information they need (syntax, resolved structures, type information, MIR).

Sources:
- [Rust Compiler Development Guide: LintStore](https://rustc-dev-guide.rust-lang.org/diagnostics/lintstore.html)
- [Rust Compiler Development Guide: Errors and lints](https://rustc-dev-guide.rust-lang.org/diagnostics.html)
- [Rust compiler lint crate](https://doc.rust-lang.org/nightly/nightly-rustc/rustc_lint/index.html)

The useful architectural implications are to keep rule identity separate from implementations, share analysis results, reject duplicate registrations, and schedule checks according to the facts they require. The standalone application should reuse these ideas, not depend on `rustc_private` or attempt to load Clippy's compiler plugins.

## Reuse decisions

The following table distinguishes adopted libraries from alternatives that remain unadopted. A dependency is not a success by itself: it should replace code, reduce maintenance surface, or provide a demonstrably better algorithm.

| Concern | Library | Status | What it replaces / why it fits |
| --- | --- | --- | --- |
| Lossless KiCad S-expression parsing | [`kiutils_sexpr`](https://crates.io/crates/kiutils_sexpr) | Adopted | Provides the CST and original byte spans. Replacing it with another parser would duplicate grammar and source-preservation work. |
| Terminal diagnostics and source excerpts | [`miette`](https://docs.rs/miette/7.6.0/miette/) | Adopted | Renders named source files, labeled byte spans, severity, diagnostic codes, help and source excerpts. The CLI adapts the project's existing `Diagnostic` to miette; JSON remains the stable interchange schema. |
| General planar geometry | [`geo`](https://docs.rs/geo/0.31.0/geo/) | Adopted | Replaces handwritten segment-intersection classification, point-to-segment distance, point-in-polygon, and rectangle/polygon predicates. Its Bentley–Ottmann sweep replaces the nested all-pairs wire-intersection scan. |
| Curve flattening | [`kurbo`](https://docs.rs/kurbo/0.13.1/kurbo/) | Adopted | Replaces hand-written recursive cubic Bézier subdivision and manual arc sampling with adaptive path flattening at a schematic-space tolerance. |
| Spatial indexing | [`rstar`](https://docs.rs/rstar/0.12.2/rstar/) | Not used directly | Geo's sweep-line algorithm already addresses the current all-wire intersection scan. Do not add a second index until profiling shows a remaining broad-phase bottleneck in a different workload. |
| Robust geometric predicates | [`robust`](https://docs.rs/robust/1.2.0/robust/) | Not used directly | Geo already supplies the predicates used by the current rules. Reconsider only if fixtures expose specific numerical-degeneracy cases that require a separate robust-predicate layer. |
| Alternative source diagnostic renderer | [`codespan-reporting`](https://docs.rs/codespan-reporting/latest/codespan_reporting/) | Not used | It overlaps with miette's source-map and span-rendering responsibilities. Keeping both would create competing source/diagnostic abstractions. |

### Important semantic boundary

The libraries own generic algorithms; the project owns KiCad meaning. The application still defines the coordinate unit (millimetres), tolerance policy, explicit-junction semantics, pin-anchor exceptions, wire-island semantics, rule IDs, diagnostic messages and source-to-schematic context. A generic geometry library must not decide whether two KiCad wires are electrically connected.

Geo's rectangle/polygon operations are used with explicit project tolerance expansion where prior rules used an epsilon. Curve flattening uses a project-selected 0.01 mm tolerance. Those values are policy, not library defaults.

## Code reduction achieved

Against the feature-branch state before this simplification pass (`90b19a296472eb955482e6ba2ceae94a7b46a0fd`), the three directly refactored implementation files changed as follows:

| File | Before | After | Net change |
| --- | ---: | ---: | ---: |
| `generic.rs` | 3,523 lines | 3,419 lines | -104 |
| `source.rs` | 177 lines | 104 lines | -73 |
| CLI `main.rs` | 331 lines | 375 lines | +44 |
| **Combined** | **4,031 lines** | **3,898 lines** | **-133** |

This is a net reduction of 133 source lines across the geometry kernel, source mapping, and terminal renderer. The CLI adapter grew because it now passes structured diagnostics to miette, but that is outweighed by deleting the duplicate source line-index and line/column implementation and by moving generic geometry and curve algorithms into their libraries. The comparison is a file line-count delta, not a claim about all repository code or runtime performance.

## Target architecture

```text
KiCad source
    |
    v
kiutils_sexpr lossless CST + byte spans
    |
    v
typed schematic facts and KiCad-specific context
    |
    +------ geo: intersections, distances, polygons, rectangles
    |
    +------ kurbo: adaptive curve flattening
    |
    v
phase-specific RulePass implementations
    |
    v
stable Diagnostic contract
    |
    +------ JSON: serde serialization (unchanged schema)
    |
    +------ miette: labeled source diagnostics in the CLI
```

### Ownership rules

- **Parser:** preserve original source and the lossless CST; rely on CST-provided spans.
- **Source file:** retain the path and shared original text. Do not maintain a second line-start index solely for terminal presentation.
- **Geometry adapter:** convert KiCad primitives to library types and apply KiCad-specific tolerances and exemptions. Do not reimplement general intersections, distances or containment.
- **Curves:** retain only the KiCad start/mid/end interpretation needed to construct an arc; delegate path flattening to kurbo.
- **Connectivity:** keep explicit junctions, pin anchors, label scope and physical wire islands as application logic.
- **Rule engine:** keep typed IDs, metadata, registration, pass dependencies and deterministic execution.
- **Diagnostics:** preserve the project's stable JSON contract; adapt it to miette only at the CLI rendering boundary.
- **Profiles:** hold project-specific intent and contracts, not generic geometry or terminal formatting.
- **Fix planner:** remain separate from read-only analysis and require preconditions plus post-edit validation.

## Rule IDs and extensibility

Built-in rules are a closed, compile-time set and are referenced through `BuiltinRuleId`. Their string forms remain stable external codes for JSON, documentation and integrations. Native ERC codes and future third-party rule namespaces remain open-ended external identifiers.

The in-process `RulePass` API over immutable shared facts is implemented. External Rust crates can register passes programmatically. Before treating it as a stable third-party contract, version the trait/context, formalize typed options and analysis dependencies, and define diagnostic compatibility policy. Dynamic libraries or WASM rule packs should wait until those contracts and resource limits are stable.

Configuration should use structured rule keys and typed per-rule options. Unknown rule IDs, invalid option values, duplicate declarations and conflicting overrides should fail with actionable configuration errors rather than being silently ignored.

## Next code-reduction opportunities

1. Continue splitting `generic.rs` by domain, but move reusable primitives and predicates into libraries instead of extracting private helper modules that merely relocate the same code.
2. Profile label/field/text collision checks. Where a library's sweep or R-tree can remove an actual all-pairs scan, use it as a candidate generator and preserve exact KiCad predicates.
3. Inspect text-width estimation and terminal formatting for mature typography/rendering libraries before writing more glyph/layout code.
4. Evaluate source parsing and diagnostic conversions by counting code removed, dependencies added, and maintenance responsibilities eliminated.
5. Keep only adapters where coordinate conversion or KiCad-specific semantics require them; do not create a wrapper layer that reproduces a library's API.

## Explicit non-goals

- Replacing KiCad native ERC or inferring electrical correctness from geometry alone.
- Replacing the lossless parser without a demonstrated correctness or maintenance benefit.
- Introducing a dynamic plugin ABI before the in-process rule API is stable.
- Adding libraries merely to increase the dependency count.
- Replacing KiCad-specific connectivity semantics with generic geometric intersection.
