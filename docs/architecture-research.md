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
| Typed KiCad schematic model | [`kiutils_kicad 0.3.0`](https://docs.rs/kiutils_kicad/0.3.0/kiutils_kicad/) | Evaluated; deferred | Exposes typed wires, junctions, labels, sheets, symbols and symbol properties over a lossless CST. Current gaps block direct replacement: `text_box` is only counted (no geometry/text model), groups and embedded library-symbol pin geometry are not in the schematic AST, item-level source spans are not exposed on typed facts, and `SchematicFile::read` is path-only (no `&str`/CST constructor). Adding it now would require temp-file parsing or duplicate fallback extraction and still leave rule-critical models hand-written. Revisit when the crate exposes a text/CST entry point, source-node spans or handles, and typed text-box/group/embedded-symbol-pin coverage. |
| Older generated KiCad parser | [`kicad_parse_gen 7.0.2`](https://docs.rs/kicad_parse_gen/7.0.2/kicad_parse_gen/) | Evaluated; rejected | It has schematic parsing and string helpers, but the latest published release is dated 2018-01-29 and targets an older KiCad format generation. It is not a safe substitute for current KiCad 9/10 syntax or the lossless unknown-node contract. |
| KiCad quoted-string escapes | [`unescaper`](https://docs.rs/unescaper/0.2.0/unescaper/) | Adopted | Decodes KiCad quoted-string escapes (control characters, quotes, backslashes and numeric forms) from the original token slice. The parser adapter preserves `CstDocument::raw` and byte spans; rule passes consume decoded `Atom::Quoted` values without carrying source text through helper calls. |
| Terminal diagnostics and source excerpts | [`miette`](https://docs.rs/miette/7.6.0/miette/) | Adopted | Renders named source files, labeled byte spans, severity, diagnostic codes, help and source excerpts. The CLI adapts the project's existing `Diagnostic` to miette; JSON remains the stable interchange schema. |
| General planar geometry | [`geo`](https://docs.rs/geo/0.31.0/geo/) | Adopted | Replaces handwritten segment-intersection classification, point-to-segment distance, point-in-polygon, and rectangle/polygon predicates. Its Bentley–Ottmann sweep replaces the nested all-pairs wire-intersection scan. |
| Curves and affine transforms | [`kurbo`](https://docs.rs/kurbo/0.13.1/kurbo/) | Adopted | Replaces hand-written recursive cubic Bézier subdivision, manual arc sampling, and duplicated point/vector rotation and mirror arithmetic with adaptive path flattening and affine transforms. |
| Physical wire connected components | [`petgraph::UnionFind`](https://docs.rs/petgraph/0.8.3/petgraph/unionfind/struct.UnionFind.html) | Adopted | Replaces the string-keyed adjacency graph and handwritten depth-first connected-component traversal with indexed disjoint sets. Segment splitting remains KiCad-specific; graph equivalence classes are delegated. |
| Spatial indexing | [`rstar`](https://docs.rs/rstar/0.12.2/rstar/) | Not used directly | Geo's sweep-line algorithm already addresses the current all-wire intersection scan. Do not add a second index until profiling shows a remaining broad-phase bottleneck in a different workload. |
| Robust geometric predicates | [`robust`](https://docs.rs/robust/1.2.0/robust/) | Not used directly | Geo already supplies the predicates used by the current rules. Reconsider only if fixtures expose specific numerical-degeneracy cases that require a separate robust-predicate layer. |
| Alternative source diagnostic renderer | [`codespan-reporting`](https://docs.rs/codespan-reporting/latest/codespan_reporting/) | Not used | It overlaps with miette's source-map and span-rendering responsibilities. Keeping both would create competing source/diagnostic abstractions. |

### Important semantic boundary

The libraries own generic algorithms; the project owns KiCad meaning. The application still defines the coordinate unit (millimetres), tolerance policy, explicit-junction semantics, pin-anchor exceptions, wire-island semantics, rule IDs, diagnostic messages and source-to-schematic context. A generic geometry library must not decide whether two KiCad wires are electrically connected.

Geo's rectangle/polygon operations are used with explicit project tolerance expansion where prior rules used an epsilon. Curve flattening uses a project-selected 0.01 mm tolerance. Those values are policy, not library defaults.


### Why the larger typed parser is deferred

The typed `kiutils_kicad` crate is the best future route to eliminate broad CST traversal, because it already shares the `kiutils_sexpr` lossless syntax tree. It is not yet a complete replacement for this linter's rule context: a number of SCH rules rely on exact source-node spans, text-box bounds, named groups/frames, and pins from embedded library symbols. Those structures are either absent from the current typed schematic AST or are represented only by counts. It also requires a filesystem path to parse, whereas this project deliberately supports `SchematicDocument::from_text`. Do not add a parallel parser or create temporary files merely to consume the partial AST. The preferred next step is to use the upstream typed model once it provides a text/CST constructor, source-span associations, and those missing schematic types; then migrate one fact family at a time with differential tests against the existing lossless CST fixtures.

## Code reduction achieved

Against the feature-branch state before this simplification pass (`90b19a296472eb955482e6ba2ceae94a7b46a0fd`), the four directly affected implementation files changed as follows:

| File | Before | After | Net change |
| --- | ---: | ---: | ---: |
| `generic.rs` | 3,523 lines / 115,651 bytes | 3,360 / 110,410 | -163 lines |
| `source.rs` | 177 lines / 4,911 bytes | 104 / 2,829 | -73 lines |
| CLI `main.rs` | 331 lines / 9,695 bytes | 375 / 11,380 | +44 lines |
| Parser `schematic.rs` | 183 lines / 5,318 bytes | 240 / 7,432 | +57 lines |
| **Combined** | **4,214 lines / 135,575 bytes** | **4,079 / 132,051** | **-135 lines / -3,524 bytes** |

Across all four directly affected implementation files, the net reduction is 135 source lines (3.2%) and 3,524 bytes (2.6%). The parser adapter and CLI grew because they now establish the correct quoted-string and terminal-diagnostic boundaries; this is outweighed by removing duplicated generic geometry, source indexing, source-to-rule plumbing and manual curve/transform algorithms. The comparison is a file-size delta for these four files, not a claim about all repository code or runtime performance.

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
3. Inspect text-width estimation and terminal formatting for mature typography/rendering libraries before writing more glyph/layout code.
4. Evaluate source parsing and diagnostic conversions by counting code removed, dependencies added, and maintenance responsibilities eliminated.
5. Keep only adapters where coordinate conversion or KiCad-specific semantics require them; do not create a wrapper layer that reproduces a library's API.

## Explicit non-goals

- Replacing KiCad native ERC or inferring electrical correctness from geometry alone.
- Replacing the lossless parser without a demonstrated correctness or maintenance benefit.
- Introducing a dynamic plugin ABI before the in-process rule API is stable.
- Adding libraries merely to increase the dependency count.
- Replacing KiCad-specific connectivity semantics with generic geometric intersection.
