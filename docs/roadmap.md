# Scope and roadmap

This page separates implemented functionality from the remaining migration work. A successful lint run is not a complete schematic-style or electrical validation pass.

## Implemented foundation

- Four-crate Rust workspace with a strict inherited lint policy and locked dependencies.
- Lossless KiCad S-expression parser integration, UTF-8 byte spans, and terminal line/column rendering delegated to `miette`.
- Shared diagnostic model with severity, source spans, and schematic coordinates.
- Typed built-in rule IDs, rule descriptor metadata, deterministic registry, and a deterministic `RulePass` engine with shared immutable analysis facts.
- KiCad CLI native ERC adapter, per-sheet JSON normalization, and flattened netlist export with component-pin-to-net lookup.
- Text and JSON CLI output.
- CI for formatting, strict Clippy, workspace tests, documentation tests, release build, and a native KiCad ERC/netlist smoke test.

## Current rule coverage

The exact status and limitations for every rule are maintained in [rules.md](rules.md). Current coverage includes:

- Generic schematic rules: SCH001–SCH003, SCH005, SCH008, SCH010 (partial), SCH011, SCH015, SCH018, and SCH019.
- Project-profile rules: SCH006, SCH007, SCH009, SCH012 (partial), SCH014, SCH016, SCH017, SCH020, SCH021, and SCH022.
- SCH004 has partial profile-driven support for direct-wire pin pairs, common-pin groups and separate-wire pairs, with optional wire-only connectivity/separation, single-label cardinality and exported-netlist equality/separation checks; automatic inference of all intended physical nets remains pending.
- SCH013 has partial wire-obstacle coverage for selected-unit embedded rectangles, polylines/polygons, circles, three-point circular arcs and cubic Bézier curves adaptively flattened through `kurbo` at a target tolerance of 0.01 mm, with finite symbol rotations and X/Y mirror transforms, plus visible Reference/Value fields, intermediate pins and annotation text boxes. All primitive intersections remain polyline/bounds approximations.

Some migrated rules intentionally implement only a subset of their historical requirements. SCH009 now checks group member symbol origins, resolved pins, estimated field bounds, description and overlapping text boxes, selected-unit symbol primitive outlines, member-pin-connected wire segments and the estimated visible glyph bounds of labels on those wire components. SCH012 applies its configured inset to those same object families. Arc/Bézier primitive geometry is sampled and text extents are estimates; free-standing labels and wires, non-text-box annotation types and unresolvable transforms remain incomplete. Finite non-cardinal embedded pin and primitive transforms are supported; SCH005 pin-axis checks remain cardinal-only, while SCH015 now verifies no-connect markers against transformed pin coordinates. SCH010 still uses estimated text bounds.

## Project profiles

Version-1 JSON profiles currently support:

- Component intent, role-specific connection contracts, explicit pin-to-net expectations, and decoupling terminal-to-target-pin verification through the KiCad-exported netlist.
- Functional-group members, documentation text-box UUIDs, evidence references, optional native KiCad group UUID binding, and optional schematic rectangle UUID binding.
- Minimum member-origin spacing, minimum frame inset, and a per-group wire-length budget measured as the sum of segments wholly within the referenced frame.

The profile format is deliberately explicit; project-specific expectations are not imported from a schematic generator. A broader JSON Schema and a separate general rule-policy configuration are still planned.

## Next: reduce remaining domain-specific extraction

- Continue replacing complete generic algorithms with mature libraries only when the adoption removes more implementation/maintenance surface than the adapter adds.
- Re-evaluate typed KiCad model crates only when they support in-memory parsing, source-span provenance, and the missing text-box/group/embedded-pin fact families. Avoid adding a second partial parser.
- Profile remaining collision scans before introducing an R-tree; do not add spatial indexing without a measured hot path.
- Keep the raw line/byte count honest: current substitutions have simplified algorithm ownership, but parser/CLI adapters and tests mean the measured four-file total has not yet decreased.

## Next: physical connectivity and symbol geometry

1. Extend the current physical-wire graph and SCH004 profile contracts to infer or declare broader project-specific physical connectivity beyond explicit pin pairs and common-pin groups. Labels must never create physical connectivity edges.
2. Extend SCH013 beyond its current selected-unit primitive coverage and consider exact analytic arc intersections if the bounded polyline approximation proves insufficient.
3. Extend SCH009/SCH012 beyond their current member-connected wire islands and text-box annotations to cover free-standing labels/wires, plain text and graphical/bitmap annotations; improve exact body-curve coverage.
4. Broaden multi-unit, alternate-convert and mirrored transform coverage. Finite non-cardinal rotations now resolve pin and primitive coordinates for frame/obstacle rules; SCH005 axis checks remain cardinal-only, and unresolvable geometry must continue to be skipped conservatively.

## Rule configuration and extensibility

- Add a project rule-policy file such as `kicad-clippy.toml` with explicit `allow`, `warn`, and `deny` levels and named presets.
- Extend the initial geometry, label and annotation passes with explicit semantic/connectivity phases; version the extension contract and add typed per-rule options.
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
