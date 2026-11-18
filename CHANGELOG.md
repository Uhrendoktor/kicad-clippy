# Changelog

Notable changes are documented here. This project is in early development and has not published a stable release.

## Unreleased

- Added SCH013 crossings of both no-connect marker X centerlines using KiCad's fixed 48-mil marker size; no settings-derived marker pen width is guessed.

- Added an explicit `--project` input for SCH013 to resolve zero/omitted wire widths from the KiCad Default netclass when project assignment maps/patterns are absent, null, empty or Default-only and no schematic netclass overrides are present; non-Default mappings remain conservative.
- Added SCH013 hierarchical sheet frame/body obstacles with local exceptions for wires terminating at matching sheet-pin anchors; wires continuing into the sheet body remain checked.
- Corrected SCH013 embedded bitmap obstacles to use the serialized rotation and actual four-corner PNG/JPEG footprint instead of the axis-aligned bounding box; image dimensions still follow the existing version-aware PPI and scale rules.
- Added SCH013 stroked bus-line and wire-to-bus entry obstacles; only wire endpoints matching a bus-entry anchor receive a local contact exception, and direct wire/bus crossings remain diagnosed.
- Added SCH013 full ellipse and elliptical-arc stroke obstacles using adaptive rotated paths; closed ellipse bodies retain interior checks, while open elliptical arcs do not block their interiors.
- Added SCH013 table border and enabled separator strokes as vector obstacles, respecting disabled strokes and merged-cell spans; table strokes support consistent cardinal rotations, while inconsistent/non-cardinal layouts and non-cardinal cell text are conservatively skipped; table-cell glyph bounds also support 180° text rotation.

- Replaced handwritten segment intersection, wire-pair enumeration, point/segment distance, and polygon/rectangle predicates with GeoRust `geo` algorithms; retained KiCad-specific tolerances, pin exemptions and explicit-junction rules.
- Replaced handwritten cubic Bézier subdivision and manual arc sampling with Kurbo adaptive path flattening and affine transforms.
- Replaced the string-keyed wire adjacency graph and handwritten connected-component traversal with `petgraph::UnionFind`.
- Replaced guessed per-character text widths with actual KiCad NewStroke glyph advances; removed the extra approximate-width inflation from visible text bounds.
- Replaced the duplicate source line index and UTF-8 line/column mapper with miette source-span rendering in the CLI; the stable JSON `Diagnostic` and `ByteSpan` contracts remain unchanged.
- Added parser-boundary decoding of KiCad quoted-string escapes with `unescaper`, preserving lossless source bytes while removing source-text plumbing from rule helpers.
- Removed 86 lines from the source layer by delegating line/column rendering to `miette` and range validation to `str::get`. Across the four audited files (`generic.rs`, `source.rs`, CLI `main.rs`, parser `schematic.rs`), the current migration is +37 lines / +4,270 bytes net because the parser and CLI adapters plus regression tests offset the deleted mapper and geometry algorithms; see the measured before/after table in `docs/architecture-research.md`.
- Added typed built-in rule identifiers for SCH001–SCH022 while preserving serialized diagnostic codes.
- Added a deterministic in-process RulePass engine with duplicate-name rejection and one immutable shared schematic context per run.
- Split built-in checks into geometry, label/topology, and annotation passes; shared wire segments and junctions are collected once.
- Documented adopted and deferred library choices, semantic boundaries, and the code-reduction roadmap.

- Initialized the standalone Rust workspace and `kicad-clippy` CLI.
- Added lossless KiCad schematic parsing and source mapping.
- Added shared diagnostic and rule metadata contracts.
- Added native KiCad ERC execution and JSON report normalization.
- Added strict Rust/Clippy quality gates and project documentation.
