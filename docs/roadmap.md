# Scope and roadmap

This page describes what is implemented and what remains planned. A successful current lint run is not a complete schematic-style audit.

## Implemented foundation

- Rust workspace and strict inherited lint policy.
- Lossless KiCad S-expression parser integration.
- UTF-8 byte spans, line/column mapping, and shared source model.
- Diagnostic data model with optional source spans and schematic coordinates.
- Rule descriptor metadata, levels, and deterministic registry.
- KiCad CLI native ERC adapter and per-sheet JSON normalization.
- Text and JSON CLI output.
- Tests for source mapping, parser validation, rule registry, and ERC report normalization.
- CI for formatting, strict Clippy, unit tests, and documentation tests.

## Next: executable custom-rule engine

- Define the public rule trait and immutable rule-analysis context.
- Add `kicad-clippy.toml` parsing and validation.
- Implement presets and per-rule `allow` / `warn` / `deny` configuration.
- Add a diagnostic reporter and rule runner.
- Add generic, project-independent geometry, label, annotation, and documentation rules.

## Port the legacy rule catalogue

The KiCad Blocks schematic checker historically contains rules SCH001–SCH019, with SCH020 introduced separately in a review branch. The plan is to classify and migrate those checks into:

- Generic design-quality rules that work on any schematic.
- Declarative profile rules that describe project-specific expectations.
- Checks that should remain delegated to native KiCad ERC.

The migration must remove dependencies on the KiCad Blocks Python generator and its example-specific globals. Project design metadata should be loaded from an explicit profile, not imported from an example generator.

## Profiles and analysis model

- Build a typed schematic model with embedded library-symbol geometry, pins, wires, labels, junctions, text boxes, fields, and sheet hierarchy.
- Preserve KiCad UUIDs and the sheet instance path.
- Add shared geometry, connectivity, and spatial indexes.
- Define schema-validated project profiles for net contracts, component roles, circuit groups, documentation, and layout expectations.

## Diagnostics and output

- Add SARIF 2.1.0.
- Add source-linked spans for custom semantic findings.
- Add stable JSON schema versioning and snapshot tests.
- Add explain/list commands for rule discovery.

## Fixes and layout proposals

- Introduce structured edits with applicability classifications.
- Add a non-overlapping edit planner and source-hash/object-UUID preconditions.
- Verify proposed fixes by reparsing, rerunning native ERC, rerunning relevant rules, and checking net connectivity.
- Add layout proposal support for group anchors, relative offsets, pin-relative placement, frames, spacing, and obstacle clearances.

Safe automatic application must be restricted to machine-applicable fixes. Layout changes that can affect connectivity require an explicit reviewable diff and verification pass.

## Third-party rules and editor integration

- Start with a stable in-process Rust rule API.
- Consider sandboxed WASM rule packs only after the semantic model and versioned API are stable.
- Treat KiCad editor integration as a later UI adapter over the shared diagnostic model.
- Use KiCad IPC only when the required Eeschema API surface is available and validated against a supported KiCad release.

## Explicit non-goals

- Replacing KiCad's electrical ERC engine.
- Forking KiCad or modifying its native ERC catalogue.
- Mutating schematics during rule execution.
- Shipping an editor plugin before the CLI and diagnostic contracts stabilize.
