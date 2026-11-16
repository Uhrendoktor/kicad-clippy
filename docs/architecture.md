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

Source spans are half-open UTF-8 byte ranges `[start, end)`. The source mapper converts valid byte offsets to one-based line and Unicode-scalar column positions and rejects offsets that split a UTF-8 code point.

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

`RuleDescriptor` is metadata, `RuleRegistry` rejects duplicate IDs, and neither is the implementation of a rule. The current built-in checks are executed by the rules crate. The next engine increment is to centralize the executable rule catalogue, typed options and phase dependencies rather than coupling a rule to a stringly-typed dispatcher.

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

Derived geometry, connectivity, hierarchy and symbol indexes should be built once and shared by related rules. Spatial indexes should be introduced where profiling demonstrates an all-pairs bottleneck. A spatial index is a broad-phase candidate filter, not a replacement for exact KiCad-specific predicates.

## Design constraints

1. Native electrical semantics stay native; avoid duplicate or divergent implementations of KiCad ERC.
2. Preserve source fidelity and the original source spans.
3. Treat built-in rule IDs and diagnostic JSON codes as stable public contracts.
4. Keep source byte offsets separate from physical millimetre coordinates.
5. Rules inspect immutable facts; edits belong to a separate planner.
6. Safe fixes require preconditions, reparsing and rerunning ERC and relevant rules.
7. The CLI is the primary interface; future editor integration should consume the shared diagnostic model.
