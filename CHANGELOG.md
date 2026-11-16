# Changelog

Notable changes are documented here. This project is in early development and has not published a stable release.

## Unreleased

- Replaced handwritten segment intersection, wire-pair enumeration, point/segment distance, and polygon/rectangle predicates with GeoRust `geo` algorithms; retained KiCad-specific tolerances, pin exemptions and explicit-junction rules.
- Replaced handwritten cubic Bézier subdivision and manual arc sampling with Kurbo adaptive path flattening.
- Replaced the duplicate source line index and UTF-8 line/column mapper with miette source-span rendering in the CLI; the stable JSON `Diagnostic` and `ByteSpan` contracts remain unchanged.
- Added parser-boundary decoding of KiCad quoted-string escapes with `unescaper`, preserving lossless source bytes while removing source-text plumbing from rule helpers.
- Reduced four directly affected implementation files by a net 128 lines and 3,271 bytes compared with the pre-library-refactor branch state.
- Added typed built-in rule identifiers for SCH001–SCH022 while preserving serialized diagnostic codes.
- Added a deterministic in-process RulePass engine with duplicate-name rejection and one immutable shared schematic context per run.
- Split built-in checks into geometry, label/topology, and annotation passes; shared wire segments and junctions are collected once.
- Documented adopted and deferred library choices, semantic boundaries, and the code-reduction roadmap.

- Initialized the standalone Rust workspace and `kicad-clippy` CLI.
- Added lossless KiCad schematic parsing and source mapping.
- Added shared diagnostic and rule metadata contracts.
- Added native KiCad ERC execution and JSON report normalization.
- Added strict Rust/Clippy quality gates and project documentation.
