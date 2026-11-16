# Scope and roadmap

This page separates implemented functionality from the remaining migration work. A successful lint run is not a complete schematic-style or electrical validation pass.

## Implemented foundation

- Four-crate Rust workspace with a strict inherited lint policy and locked dependencies.
- Lossless KiCad S-expression parser integration, UTF-8 byte spans, and line/column mapping.
- Shared diagnostic model with severity, source spans, and schematic coordinates.
- Rule descriptor metadata, levels, deterministic registry, and a built-in rule runner.
- KiCad CLI native ERC adapter, per-sheet JSON normalization, and flattened netlist export with component-pin-to-net lookup.
- Text and JSON CLI output.
- CI for formatting, strict Clippy, workspace tests, documentation tests, release build, and a native KiCad ERC/netlist smoke test.

## Current rule coverage

The exact status and limitations for every rule are maintained in [rules.md](rules.md). Current coverage includes:

- Generic schematic rules: SCH001–SCH003, SCH005, SCH008, SCH010 (partial), SCH011, SCH015, SCH018, and SCH019.
- Project-profile rules: SCH006, SCH007, SCH009, SCH012 (partial), SCH014, SCH016, SCH017, SCH020, SCH021, and SCH022.
- SCH004 has partial profile-driven support for direct-wire pin pairs and common-pin groups with optional wire-only connectivity and single-label cardinality; automatic inference of all intended physical nets remains pending.
- SCH013 has partial wire-obstacle coverage for embedded rectangles, polylines/polygons, circles, three-point circular arcs sampled to a target sagitta of 0.01 mm, and cubic Bézier curves adaptively subdivided to a target flatness of 0.01 mm (each curve capped at 4096 segments), plus visible Reference/Value fields, intermediate pins and annotation text boxes. All primitive intersections remain polyline/bounds approximations.

Some migrated rules intentionally implement only a subset of their historical requirements. In particular, current SCH009 checks member symbol origins against a referenced rectangle, SCH010 checks visible-field overlap against embedded symbol outlines, wires, pins, labels and annotation boxes using estimated text bounds, SCH012 checks configured inset for member origins, resolved embedded pin coordinates and the description text box, and SCH015 checks the placement of explicit no-connect markers. These do not yet prove full symbol, pin, field, label, wire, or note containment/clearance.

## Project profiles

Version-1 JSON profiles currently support:

- Component intent, role-specific connection contracts, explicit pin-to-net expectations, and decoupling terminal-to-target-pin verification through the KiCad-exported netlist.
- Functional-group members, documentation text-box UUIDs, evidence references, optional native KiCad group UUID binding, and optional schematic rectangle UUID binding.
- Minimum member-origin spacing, minimum frame inset, and a per-group wire-length budget measured as the sum of segments wholly within the referenced frame.

The profile format is deliberately explicit; project-specific expectations are not imported from a schematic generator. A broader JSON Schema and a separate general rule-policy configuration are still planned.

## Next: physical connectivity and symbol geometry

1. Extend the current physical-wire graph and SCH004 profile contracts to infer or declare broader project-specific physical connectivity beyond explicit pin pairs and common-pin groups. Labels must never create physical connectivity edges.
2. Extend SCH013 beyond its current selected-unit/cardinal-transform scope and consider exact analytic arc intersections if the bounded polyline approximation proves insufficient.
3. Extend frame and field rules beyond symbol origins and estimated text boxes: check actual pin positions, visible field extents, labels, wire segments, and annotation bounds against frames and obstacles.
4. Improve multi-unit, alternate-convert, mirrored, and non-cardinal symbol transform coverage, with conservative skips for geometry that cannot be resolved.

## Rule configuration and extensibility

- Add a project rule-policy file such as `kicad-clippy.toml` with explicit `allow`, `warn`, and `deny` levels and named presets.
- Define a stable public rule trait and immutable rule-analysis context around the existing descriptor/registry model.
- Add rule explain/list commands and schema validation for configuration.
- Consider sandboxed WASM rule packs only after the in-process API and semantic model are stable.

## Diagnostics and output

- Add SARIF 2.1.0 output.
- Version and publish the JSON diagnostic schema, including stable rule identifiers and source-linked spans.
- Add snapshot and compatibility tests for serialized diagnostics and native ERC normalization.

## Fixes and layout proposals

- Introduce structured edits with applicability classifications.
- Add a non-overlapping edit planner with source-hash and object-UUID preconditions.
- Verify proposed fixes by reparsing, rerunning native ERC and relevant rules, and checking that logical and physical connectivity has not changed.
- Add reviewable layout proposals for group anchors, relative offsets, pin-relative placement, frames, spacing, and obstacle clearance.

Safe automatic application must be restricted to machine-applicable edits. Layout changes that could affect connectivity require an explicit reviewable diff and a verification pass.

## Editor integration

- Treat KiCad editor integration as a later UI adapter over the shared diagnostic model.
- Use KiCad IPC only when the required Eeschema API surface is available and validated against a supported KiCad release.

## Explicit non-goals

- Replacing KiCad's electrical ERC engine.
- Forking KiCad or modifying its native ERC catalogue.
- Mutating schematics during rule execution.
- Shipping an editor plugin before the CLI, physical geometry model, and diagnostic contracts are stable.
