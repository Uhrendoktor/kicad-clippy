# Architecture

`kicad-clippy` is a standalone Rust schematic linter. Its design follows the separation of lint declarations, registration, passes and diagnostics used by the Rust compiler/Clippy without embedding Clippy or depending on rustc-private APIs. The rationale and comparison of reusable libraries are recorded in [architecture research](architecture-research.md).

## Current workspace

```text
.kicad_sch
   |
   v
kicad-clippy-parser ---- lossless S-expression CST + source text
   |
   v
kicad-clippy-core ------ source spans, diagnostics, typed built-in IDs,
   |                    rule descriptors, deterministic registry
   |
   +-------------------- kicad-clippy-erc
   |                         |
   |                         v
   |                    kicad-cli sch erc --format json
   |                         |
   +-------------------------+
                 |
                 v
          kicad-clippy-rules
          generic schematic checks + profile contracts
                 |
                 v
          kicad-clippy CLI
          text / JSON output
```

| Crate | Responsibility |
| --- | --- |
| `kicad-clippy-core` | Shared source mapping, diagnostics, severity, typed built-in rule IDs, rule metadata and deterministic registry. |
| `kicad-clippy-parser` | Loads and parses a KiCad schematic while retaining original text and CST spans. |
| `kicad-clippy-erc` | Invokes KiCad CLI ERC, exports netlists and normalizes native reports. |
| `kicad-clippy-rules` | Built-in schematic checks, extracted schematic facts and project-profile contracts. |
| `kicad-clippy` | CLI arguments, orchestration, text output, JSON output and process exit status. |

## Parser and source locations

The parser uses `kiutils_sexpr` as a lossless concrete-syntax-tree parser. `SchematicDocument` retains a `SourceFile` and the CST. The original text is not canonicalized or rewritten. Parser nodes already carry UTF-8 byte spans, so a second generic CST/span framework would duplicate rather than simplify the current boundary.

The CST's `raw` buffer and token spans remain unchanged. At the `SchematicDocument` boundary, the parser adapter uses each quoted atom's original span to pass the token contents through `unescaper`, replacing the parser's quoted payload with the semantic KiCad string. This keeps exact source reproduction available through `CstDocument::to_lossless_string()` while ensuring rule passes see real newlines, quotes and literal backslashes. The rules layer no longer slices source text or threads a source argument through its geometry and annotation helpers just to decode strings.

Source spans are half-open UTF-8 byte ranges `[start, end)`. `ByteSpan` validates ordering, source bounds and UTF-8 boundaries. `SourceFile` retains the path and one shared copy of the original source text; it deliberately does not build a second line-start index. The CLI passes validated byte ranges to `miette`, which resolves line/column display and renders source labels. This removes a parallel source-location implementation while keeping byte offsets as the canonical contract.

This model intentionally distinguishes two coordinate systems:

- A source span identifies text in a `.kicad_sch` file.
- A `SchematicLocation` identifies a physical schematic point in millimetres.

Native ERC reports generally provide schematic coordinates and KiCad item UUIDs, not a precise source-text range. Its diagnostics therefore keep source spans optional rather than inventing a byte mapping.

## Native ERC boundary

KiCad remains the authority for electrical-rule semantics. The adapter invokes `kicad-cli sch erc`, retains native violation type, description, severity, excluded flag, sheet path, affected UUIDs and physical positions, and derives exit status from normalized active error diagnostics.

`kicad-clippy` does not patch KiCad or register new checks in KiCad's native ERC catalogue. Custom layout, annotation, documentation and project-profile rules remain a separate linter responsibility.

## Diagnostics and rule identity

A `Diagnostic` includes a stable code and severity, message, optional explanation/help, optional source path and byte span, optional sheet path and physical coordinate, related affected items, and exclusion state.

Built-in IDs are represented in Rust by `BuiltinRuleId` (SCH001–SCH022). Conversion to strings occurs at the diagnostic/serialization boundary, preserving the existing external JSON code. Open-ended native ERC identifiers remain strings because KiCad controls that namespace.

`RuleDescriptor` is metadata, `RuleRegistry` rejects duplicate IDs, and neither is the implementation of a rule. The rules crate now exposes a deterministic RuleEngine, an immutable RuleContext, and a RulePass extension trait. The engine collects SchematicFacts once, shares them with every registered pass, rejects duplicate pass names, and executes passes in stable name order. A pass can emit diagnostics for several rule IDs; a rule may be emitted by more than one pass. External Rust crates can implement RulePass and register it in-process. The CLI currently registers the three built-in passes; dynamic library and WASM loading are intentionally not part of this initial extension contract. The built-in checks are currently split into three deterministic passes: geometry/pin placement, physical-wire topology/labels, and annotation/field layout. Parsed source, schematic facts, wire segments and junction points are collected once and shared. Further extraction should separate semantic resolution, spatial indexing, physical connectivity and project-profile contracts as those shared analysis boundaries stabilize.


## Reused geometry and diagnostic libraries

The implementation delegates generic algorithms rather than maintaining parallel versions:

- `geo` supplies planar intersection predicates, sweep-line wire intersection enumeration, point/segment distance, polygon containment and rectangle intersection. The rule layer still applies KiCad-specific tolerances, explicit-junction handling and pin exceptions.
- `kurbo` supplies adaptive curve flattening for cubic Bézier and circular-arc paths plus affine matrix operations for placed-symbol rotation and mirroring. The rules layer retains KiCad's local-to-sheet coordinate convention and applies the symbol origin.
- `miette` supplies terminal diagnostic labels and source excerpts. `Diagnostic` remains the JSON-compatible project schema, and its adapter is confined to the CLI.
- `unescaper` decodes quoted S-expression text once at the parser boundary from original byte-span slices; the raw CST buffer and source spans are preserved.
- `petgraph::UnionFind` supplies the generic disjoint-set structure for physical wire components; the rules layer alone decides which schematic points are joined by KiCad wire/junction semantics.

Do not add a second source renderer or geometry kernel. Introduce spatial indexing only after profiling demonstrates a remaining candidate-scan bottleneck.

## Analysis pipeline

The intended full pipeline is:

```text
schematic + rule policy + optional project profile
                  |
                  v
       lossless CST + source map
                  |
                  v
    typed semantic model / UUID index
                  |
        +---------+----------+
        |                    |
        v                    v
 geometry analysis      connectivity analysis
        |                    |
        +---------+----------+
                  |
          immutable RuleContext
                  |
       phase-specific rule passes
                  |
          normalized diagnostics
                  |
       text / JSON / SARIF renderers
```

Derived geometry, connectivity, hierarchy and symbol indexes should be built once and shared by related rules. Physical wire connectivity uses `petgraph::UnionFind` after KiCad-specific segment splitting has determined the actual edges. Spatial indexes should be introduced where profiling demonstrates an all-pairs bottleneck. A spatial index is a broad-phase candidate filter, not a replacement for exact KiCad-specific predicates.

## Design constraints

1. Native electrical semantics stay native; avoid duplicate or divergent implementations of KiCad ERC.
2. Preserve source fidelity and the original source spans.
3. Treat built-in rule IDs and diagnostic JSON codes as stable public contracts.
4. Keep source byte offsets separate from physical millimetre coordinates.
5. Rules inspect immutable facts; edits belong to a separate planner.
6. Safe fixes require preconditions, reparsing and rerunning ERC and relevant rules.
7. The CLI is the primary interface; future editor integration should consume the shared diagnostic model.
