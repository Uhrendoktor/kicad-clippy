# Architecture

`kicad-clippy` is a standalone Rust schematic linter. It uses Clippy's separation of rule metadata, implementation, registration, configuration, diagnostics, and tests as an architectural reference; it does not embed Clippy or depend on rustc-private APIs.

## Current workspace

```text
.kicad_sch
   |
   v
kicad-clippy-parser ---- lossless S-expression CST + source text
   |
   v
kicad-clippy-core ------ source spans, line mapping, diagnostics,
   |                    severities, rule descriptors, registry
   |
   +-------------------- kicad-clippy-erc
   |                         |
   |                         v
   |                    kicad-cli sch erc --format json
   |                         |
   +-------------------------+
                 |
                 v
          kicad-clippy CLI
          text / JSON output
```

| Crate | Responsibility |
| --- | --- |
| `kicad-clippy-core` | Shared data types for source mapping, diagnostics, severity, rule metadata, and deterministic registry. |
| `kicad-clippy-parser` | Loads and parses a KiCad schematic while retaining the original text and CST spans. |
| `kicad-clippy-erc` | Invokes KiCad CLI ERC and normalizes the per-sheet JSON report. |
| `kicad-clippy` | CLI arguments, orchestration, text output, JSON output, and process exit status. |

## Parser and source locations

The parser uses the `kiutils_sexpr` lossless concrete-syntax-tree parser. The `SchematicDocument` retains a `SourceFile` and the CST. The original text is not canonicalized or rewritten.

Source spans are half-open UTF-8 byte ranges `[start, end)`. The source mapper converts valid byte offsets to one-based line and Unicode-scalar column positions. It rejects offsets that split a UTF-8 code point.

This model intentionally distinguishes two coordinate systems:

- A source span identifies text in a `.kicad_sch` file.
- A `SchematicLocation` identifies a physical schematic point in millimetres.

Native ERC reports generally provide a schematic coordinate and KiCad item UUID, not a precise source-text range. Its diagnostics therefore keep source spans optional rather than inventing a byte mapping.

## Native ERC boundary

KiCad remains the authority for electrical-rule semantics. The adapter runs:

```sh
kicad-cli sch erc --format json --severity-all --output erc-report.json design.kicad_sch
```

The report is read from the per-sheet `sheets[].violations[]` structure. The adapter retains the violation type, description, severity, excluded flag, sheet path, affected item UUIDs, and physical positions. It also retains ignored-check metadata and report-level version/date/unit information.

If KiCad exits non-zero but writes a valid report, the report is still parsed. The linter's exit status is derived from normalized active error diagnostics. If the process fails without producing a report, the adapter returns a process error.

`kicad-clippy` does not patch KiCad or register new checks in KiCad's native ERC catalogue. Custom layout, annotation, documentation, and project-profile rules remain a separate linter responsibility.

## Diagnostics

A `Diagnostic` includes:

- Stable diagnostic code and severity.
- One-line message, optional explanation, and optional help.
- Optional source path and byte span.
- Optional hierarchical sheet path.
- Optional physical schematic coordinate.
- Related affected items, including UUID and description.
- Exclusion state.

Diagnostics are serializable so future renderers can consume the same model. The current CLI exposes text and JSON output. SARIF and editor/LSP presentation are future work.

## Rule metadata and execution

The current core crate defines:

- `RuleDescriptor`: stable ID, name, category, summary, explanation, default level, documentation URL, and tags.
- `RuleLevel`: `allow`, `warn`, or `deny`.
- `RuleRegistry`: deterministic metadata registry that rejects duplicate rule IDs.

This is metadata infrastructure, not yet a complete runtime rule engine. There is no public rule trait, profile loader, rule option schema, third-party extension ABI, or active custom-rule catalogue in the current executable.

## Planned analysis pipeline

The architecture for the full linter extends the current foundation:

```text
schematic + kicad-clippy.toml + project profile
                 |
                 v
      lossless CST + source map
                 |
                 v
    typed semantic model + UUID/sheet hierarchy
                 |
          +------+------+
          |             |
          v             v
  custom rule engine  native KiCad ERC
          |             |
          +------+------+
                 |
                 v
       normalized diagnostics
                 |
        +--------+--------+
        |        |        |
        v        v        v
     terminal  JSON/SARIF  fix planner
                           |
                           v
                  verified edit proposal
                           |
                           v
                 reparse + ERC + relint
```

Derived geometry, connectivity, hierarchy, and symbol indexes should be built once and shared by related rules. Rule configuration should select levels/options without forking rule implementations.

## Design constraints

1. **Native electrical semantics stay native.** Avoid duplicate or divergent implementations of KiCad ERC.
2. **Preserve source fidelity.** Keep the original S-expression text and source spans available for diagnostics and future edits.
3. **Stable identity.** Rule IDs and diagnostic codes are public contracts once released.
4. **Separate source and drawing coordinates.** Byte spans and millimetre positions are not interchangeable.
5. **Read-only analysis first.** Rules should inspect immutable schematic context; edits belong to a separate planner.
6. **Safe fixes require verification.** Future automatic edits must check preconditions, avoid overlapping edits, reparse, and rerun ERC and relevant rules.
7. **The CLI is the primary interface.** A future KiCad plugin should visualize the same diagnostic model rather than implement a second checker.
