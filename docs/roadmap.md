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
- SCH013 has partial wire-obstacle coverage for selected-unit embedded primitives and top-level vector graphics (rectangles, polylines/polygons, circles, three-point arcs and cubic Béziers), using finite symbol rotations and X/Y mirrors, analytic circular-arc and cubic Bézier centerline intersections, top-level and embedded-symbol straight-path/Bézier stroke proximity, conservative 0.01 mm flattened-arc stroke checks, embedded library-symbol text/text-box bounds, and resolved PNG/JPEG bitmap rectangles; it also checks visible Reference/Value fields, intermediate pins and top-level annotation text boxes. Embedded-symbol pin-endpoint contact is locally excluded without exempting the rest of the wire, while circles retain existing body-interior semantics and now also check the declared stroke ring. SCH009/SCH012 containment and other flattened-curve consumers still use 0.01 mm sampled paths, and image obstacles use header-derived bounds.

Some migrated rules intentionally implement only a subset of their historical requirements. SCH009 now checks member origins/pins/fields and symbol primitives, wire segments connected to member pins or intersecting the configured frame, frame-overlapping labels and top-level plain text, the description and overlapping text boxes, top-level vector graphics, and embedded PNG/JPEG image bounds. SCH012 applies its configured inset to those same object families. Arc/Bézier geometry is sampled, text extents are estimates, and image bounds follow embedded PPI plus KiCad's stored scale/version rules; unknown image encodings and other non-text-box annotation types remain incomplete. Finite non-cardinal embedded pin and primitive transforms are supported; SCH005 pin-axis checks remain cardinal-only, while SCH015 validates no-connect markers against transformed pin coordinates. SCH010 still uses estimated text bounds.

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

1. SCH004 now supports explicit multi-terminal `physical_wire_groups` independently of named `common_pin_groups`. Continue extending the physical-wire contract for richer path/topology constraints where pin groups alone are insufficient; any inference must use physical wire geometry, never label equality.
2. SCH013 uses analytic segment/circular-arc intersections reconstructed from the three KiCad control points and directed midpoint-selected sweep, plus Kurbo's analytic cubic Bézier/path-segment intersections from the four original control points. Top-level and embedded-symbol straight-path/Bézier stroke clearance compare wire segments against half the declared graphic stroke width with Kurbo minimum-distance; circular arcs use their existing 0.01 mm flattened paths with a conservative margin. For embedded symbols, only the wire portion within the matching pin endpoint's local stroke-clearance radius is exempt, and the remaining wire is still checked; circles retain existing body-interior semantics. Combined wire-plus-graphic stroke envelopes and remaining unsupported obstacle classes remain pending; wire stroke width is not yet part of the shared segment fact, so the combined envelope must wait for an authoritative wire-width source rather than assume one.
3. Extend SCH009/SCH012 beyond the current vector graphics and PNG/JPEG bitmap bounds to cover unsupported image encodings and remaining annotation types; improve exact body-curve coverage.
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
