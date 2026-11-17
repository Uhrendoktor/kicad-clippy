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
| Typed KiCad schematic model | [`kiutils_kicad 0.3.0`](https://docs.rs/kiutils_kicad/0.3.0/kiutils_kicad/) | Evaluated; deferred | This is the strongest candidate that shares the existing `kiutils_sexpr` CST and claims lossless round-trips/unknown-node preservation. Its current `SchematicAst` already types wires, buses, junctions, no-connects, labels, symbols/properties, sheets, sheet instances, text, rectangles, circles, arcs and rule areas. It still exposes only `text_box_count` (no text-box geometry/content), `lib_symbol_count` (no embedded library-symbol primitives/pin geometry), no schematic groups, and typed facts do not carry the source-node spans needed for precise rule diagnostics. `SchematicFile::read` is also path-only; there is no public in-memory/CST constructor. Adopting it now means a temp-file bridge or reparsing from disk, plus duplicate extraction for the missing rule-critical facts. Revisit after it exposes a shared-CST constructor, source-node handles/spans, and typed text-box, group and embedded-symbol geometry. |
| Typed KiCad parser with a separate AST | [`adom-inc/kicad_lib`](https://github.com/adom-inc/kicad_lib) | Evaluated; deferred | `kicad_format::SchematicFile` already models wires, junctions, no-connects, buses, labels, text boxes, texts, placed symbols, embedded library symbols and their shapes/pins. However, the project README explicitly says APIs/data structures are unstable and the crates are not published on crates.io. Its separate `kicad_sexpr::Sexpr` representation does not retain token byte spans or the exact raw source; its serializer normalizes layout. Using it would mean a second parser/AST and a new correlation layer to recover spans from the existing CST. It is a promising upstream candidate if it adds a shared `kiutils_sexpr` CST/source-span integration; it is not currently a safe drop-in for diagnostics that must point at original bytes. |\n| Netlist-first electronics CAD | [`cadlab 0.0.2`](https://docs.rs/cadlab/0.0.2/cadlab/) | Evaluated; not applicable | Its schematic module generates a label-style drawing from its own circuit/netlist model; its documentation says KiCad schematic files are not the import/source-of-truth path. The 0.0.2 release is explicitly pre-alpha and its default feature set pulls CLI, 3D, network, parallel and PNG stacks. It may be useful as a future independent schematic visualizer, but it cannot parse the existing arbitrary `.kicad_sch` being linted and would add substantial unrelated dependencies. |\n| Older generated KiCad parser | [`kicad_parse_gen 7.0.2`](https://docs.rs/kicad_parse_gen/7.0.2/kicad_parse_gen/) | Evaluated; rejected | It exposes a schematic module and string parser, but depends on the separate `symbolic_expressions` representation and older logging/CLI dependencies (`log 0.3`, `env_logger 0.4`, `shellexpand 1.0`). It does not offer this project's lossless CST byte-span/source contract. |
| Borrowed typed KiCad views | [`forge-model 0.1.0`](https://docs.rs/forge-model/0.1.0/forge_model/) | Evaluated; deferred | It parses from `&str`, preserves unknown CST content and has a small borrowed schematic API. The published `Schematic` accessors are currently `symbols()`, `sheets()`, `version()` and `emit()`; it does not expose wires, labels, junctions, text boxes, embedded library pins or item-to-source spans needed by the rules. Its 0.1.0 model is promising, but adopting it now would add a second CST and still leave most current extraction in place. Re-evaluate as the typed schematic surface grows. |
| Schematic-to-scene parser | [`eda-view 0.1.0`](https://docs.rs/eda-view/0.1.0/eda_view/) | Evaluated; deferred | Its schematic feature parses projects/files into a reified scene and is designed for rendering/viewers. That may be useful for future visual diagnostics, but the scene abstraction does not replace byte-span diagnostics or the linter's rule-specific embedded-symbol and text-box facts; it also brings a broader visualization-oriented crate. Do not add it only to avoid writing a CST adapter. |
| General KiCad parser | [`kicad-parser 0.0.10`](https://docs.rs/kicad-parser/0.0.10/kicad_parser/) | Evaluated; not applicable | The published crate exposes generic S-expression and PCB-file modules, but no schematic module in its public crate surface. It therefore cannot replace schematic parsing for this project. |
| Archived parser project | [`KiParse`](https://github.com/Atlantix-EDA/KiParse) | Evaluated; not adopted | The standalone repository was archived in December 2025 and moved into the Atlantix-EDA project family. It is not a maintained crates.io dependency with the required source-span and typed schematic contract. |
| KiCad quoted-string escapes | [`unescaper`](https://docs.rs/unescaper/0.2.0/unescaper/) | Adopted | Decodes KiCad quoted-string escapes (control characters, quotes, backslashes and numeric forms) from the original token slice. The parser adapter preserves `CstDocument::raw` and byte spans; rule passes consume decoded `Atom::Quoted` values without carrying source text through helper calls. |
| Terminal diagnostics and source excerpts | [`miette`](https://docs.rs/miette/7.6.0/miette/) | Adopted | Renders named source files, labeled byte spans, severity, diagnostic codes, help and source excerpts. The CLI adapts the project's existing `Diagnostic` to miette; JSON remains the stable interchange schema. |
| General planar geometry | [`geo`](https://docs.rs/geo/0.31.0/geo/) | Adopted | Replaces handwritten segment-intersection classification, point-to-segment distance, point-in-polygon, and rectangle/polygon predicates. Its Bentley–Ottmann sweep replaces the nested all-pairs wire-intersection scan. |
| Curves and affine transforms | [`kurbo`](https://docs.rs/kurbo/0.13.1/kurbo/) | Adopted | Replaces hand-written recursive cubic Bézier subdivision, manual arc sampling, and duplicated point/vector rotation and mirror arithmetic with adaptive path flattening and affine transforms. |
| KiCad stroke-font text metrics | [`vector-text-newstroke 0.1.1`](https://docs.rs/vector-text-newstroke/0.1.1/vector_text_newstroke/) | Adopted cautiously; low maturity | Supplies the NewStroke glyph data used by KiCad, avoiding the previous guessed per-character width/height buckets. This is a specialized dependency, not a mature general-purpose geometry crate: the 0.1.x API returns rendered points but has no measurement method, so the adapter measures advance with a trailing sentinel glyph and glyph height from point extents. It replaces the inaccurate 14-line character-width heuristic, but the adapter is longer than that heuristic; this is a correctness trade-off, not a raw line-count reduction. Keep it pinned and regression-tested; remove or replace it if upstream adds measurement or if a better-maintained exact NewStroke implementation appears. Multiline line spacing and text-box padding remain application policy. |
| Physical wire connected components | [`petgraph::UnionFind`](https://docs.rs/petgraph/0.8.3/petgraph/unionfind/struct.UnionFind.html) | Adopted | Replaces the string-keyed adjacency graph and handwritten depth-first connected-component traversal with indexed disjoint sets. Segment splitting remains KiCad-specific; graph equivalence classes are delegated. |
| Spatial indexing | [`rstar`](https://docs.rs/rstar/0.12.2/rstar/) | Not used directly | Geo's sweep-line algorithm already addresses the current all-wire intersection scan. Do not add a second index until profiling shows a remaining broad-phase bottleneck in a different workload. |
| Robust geometric predicates | [`robust`](https://docs.rs/robust/1.2.0/robust/) | Not used directly | Geo already supplies the predicates used by the current rules. Reconsider only if fixtures expose specific numerical-degeneracy cases that require a separate robust-predicate layer. |
| Alternative source diagnostic renderer | [`codespan-reporting`](https://docs.rs/codespan-reporting/latest/codespan_reporting/) | Not used | It overlaps with miette's source-map and span-rendering responsibilities. Keeping both would create competing source/diagnostic abstractions. |

### Important semantic boundary

The libraries own generic algorithms; the project owns KiCad meaning. The application still defines the coordinate unit (millimetres), tolerance policy, explicit-junction semantics, pin-anchor exceptions, wire-island semantics, rule IDs, diagnostic messages and source-to-schematic context. A generic geometry library must not decide whether two KiCad wires are electrically connected.

Geo's rectangle/polygon operations are used with explicit project tolerance expansion where prior rules used an epsilon. Curve flattening uses a project-selected 0.01 mm tolerance. Those values are policy, not library defaults.


### Why no second typed parser is adopted yet

`kiutils_kicad` is currently the closest architectural fit because it derives typed facts from the same `kiutils_sexpr` CST and preserves unknown nodes. Its current schematic AST still does not cover the full inputs required by these rules: text-box geometry/content, named groups/frames, embedded library pin geometry, and item-level source-span associations. Its public `SchematicFile::read` entry point is filesystem-path based, while this linter intentionally supports in-memory `SchematicDocument::from_text`.

`forge-model` is the most interesting newer alternative for in-memory typed views, but its published schematic accessors cover symbols, sheets and version rather than wires/labels/junctions/graphics/text boxes. `eda-view` has richer scene extraction but is primarily a parser-to-renderer/viewer pipeline; its scene is not a replacement for this linter's source-span contract. `kicad_parse_gen` uses a different S-expression stack and does not preserve the same raw-source span contract. The other current candidates either expose PCB only or have been archived.

Do not add a second parser, temp-file bridge, or parallel typed AST just to replace a small portion of CST traversal. The adoption threshold is explicit: a candidate should share/preserve the existing source representation, support in-memory parsing, expose all rule-critical fact families, and allow a differential migration in which entire hand-written extraction paths can be deleted. Revisit `kiutils_kicad`/ `forge-model` when those APIs mature; migrate one complete fact family at a time with positive, negative and round-trip differential fixtures.

## Code reduction achieved

Against the feature-branch baseline before the external-library migration (`90b19a296472eb955482e6ba2ceae94a7b46a0fd`), the four directly affected implementation files at the verified PR head were:

| File | Before | After | Net change |
| --- | ---: | ---: | ---: |
| `generic.rs` | 3,523 lines / 115,651 bytes | 3,548 / 118,643 | +25 lines |
| `source.rs` | 177 lines / 4,911 bytes | 104 / 2,829 | -73 lines |
| CLI `main.rs` | 331 lines / 9,695 bytes | 375 / 11,380 | +44 lines |
| Parser `schematic.rs` | 183 lines / 5,318 bytes | 240 / 7,432 | +57 lines |
| **Combined** | **4,214 lines / 135,575 bytes** | **4,267 / 140,284** | **+53 lines / +4,709 bytes** |

This is not yet a raw code-size win: the four files grew by 53 lines (1.26%) and 4,709 bytes (3.47%). The source mapper dropped 73 lines by delegating line/column indexing and source excerpts to `miette`; the CLI and parser adapters added 101 lines for source-aware rendering and correct KiCad quoted-string decoding; the large rules module grew by 25 lines net while replacing several handwritten algorithms and adding regression fixtures. The line-count comparison includes tests and adapters but excludes the extra dependency source pulled into Cargo's build graph. It does not measure maintenance burden, runtime performance, or the complexity removed from bespoke algorithms.

The important current win is algorithm ownership, not LOC: the repository no longer owns its own segment intersection classifier/sweep, polygon and rectangle predicates, point-to-segment distance, cubic Bezier subdivision, arc flattening, symbol affine transforms, or disjoint-set union-find implementation. However, the requested outcome is fewer lines and fewer moving parts too; this checkpoint has not achieved that yet. The next work should remove complete extraction/geometry algorithms rather than add more adapter layers. The current `generic.rs` is 3,548 lines; prioritize replacements only when the integration deletes an entire meaningful implementation path and preserves source spans and KiCad semantics.

## Target architecture

```text
KiCad source
    |
    v
kiutils_sexpr lossless CST + byte spans
    |  unescaper: semantic quoted-string decoding
    |
    v
typed schematic facts and KiCad-specific context
    |
    +------ geo: intersections, distances, polygons, rectangles
    |
    +------ kurbo: adaptive curve flattening
    +------ petgraph: disjoint-set wire components
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

- **Parser:** preserve original source and CST spans. Decode quoted atom payloads once from their original source slices through `unescaper`; do not carry source text through every rule helper.
- **Source file:** retain the path and shared original text. Do not maintain a second line-start index solely for terminal presentation.
- **Geometry adapter:** convert KiCad primitives to library types and apply KiCad-specific tolerances and exemptions. Do not reimplement general intersections, distances or containment.
- **Curves and transforms:** retain only the KiCad start/mid/end interpretation needed to construct an arc and the coordinate-frame convention; delegate path flattening and affine matrix operations to kurbo.
- **Connectivity:** use `petgraph::UnionFind` for the generic disjoint-set operation; keep explicit junctions, pin anchors, label scope, segment splitting and physical wire-island semantics as application logic.
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
3. Use NewStroke glyph geometry/metrics as the single source for both text width and height; avoid retaining guessed text dimensions alongside the renderer.
4. Evaluate source parsing and diagnostic conversions by counting code removed, dependencies added, and maintenance responsibilities eliminated.
5. Keep only adapters where coordinate conversion or KiCad-specific semantics require them; do not create a wrapper layer that reproduces a library's API.

## Explicit non-goals

- Replacing KiCad native ERC or inferring electrical correctness from geometry alone.
- Replacing the lossless parser without a demonstrated correctness or maintenance benefit.
- Introducing a dynamic plugin ABI before the in-process rule API is stable.
- Adding libraries merely to increase the dependency count.
- Replacing KiCad-specific connectivity semantics with generic geometric intersection.
