# Changelog

Notable changes are documented here. This project is in early development and has not published a stable release.

## Unreleased

- Added typed built-in rule identifiers for SCH001–SCH022 while preserving serialized diagnostic codes.
- Added a deterministic in-process RulePass engine with duplicate-name rejection and one immutable shared schematic context per run.
- Split built-in checks into geometry, label/topology, and annotation passes; shared wire segments and junctions are collected once.
- Documented linter architecture research, diagnostic/geometry library candidates, and the staged extension roadmap.

- Initialized the standalone Rust workspace and `kicad-clippy` CLI.
- Added lossless KiCad schematic parsing and source mapping.
- Added shared diagnostic and rule metadata contracts.
- Added native KiCad ERC execution and JSON report normalization.
- Added strict Rust/Clippy quality gates and project documentation.
