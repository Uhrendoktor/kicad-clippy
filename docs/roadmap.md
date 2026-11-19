# Scope and roadmap

This page separates implemented functionality from the remaining migration work. A successful lint run is not a complete schematic-style or electrical validation pass.

## Implemented foundation

- Four-crate Rust workspace with a strict inherited lint policy and locked dependencies.
- Lossless KiCad S-expression parser integration, UTF-8 byte spans, and terminal line/column rendering delegated to `miette`.
- Shared diagnostic model with severity, source spans, and schematic coordinates.
- Typed built-in rule IDs, a complete 22-entry built-in descriptor catalogue, deterministic registry, and a deterministic `RulePass` engine with shared immutable analysis facts. An opt-in TOML policy applies named `default`, `strict`, or `advisory` presets and per-rule `allow`, `warn`, or `deny` overrides; no policy file preserves existing rule severities.
- KiCad CLI native ERC adapter, per-sheet JSON normalization, and flattened netlist export with component-pin-to-net lookup.
- Text and JSON CLI output.
- CI for formatting, strict Clippy, workspace tests, documentation tests, release build, and a native KiCad ERC/netlist smoke test.

## Current rule coverage

The exact status and limitations for every rule are maintained in [rules.md](rules.md). Current coverage includes:

- Generic schematic rules: SCH001–SCH003, SCH005, SCH008, SCH010 (partial), SCH011, SCH015, SCH018, and SCH019.
- Project-profile rules: SCH006, SCH007, SCH009, SCH012 (partial), SCH014, SCH016, SCH017, SCH020, SCH021, and SCH022. SCH009 also rejects overlapping interiors of distinct profile-declared frames; shared edges and reused frame UUIDs are allowed.
- SCH004 has partial profile-driven support for direct-wire pin pairs, common-pin groups and separate-wire pairs, with optional wire-only connectivity/separation, single-label cardinality and exported-netlist equality/separation checks; automatic inference of all intended physical nets remains pending.
- SCH013 has partial wire-obstacle coverage for selected-unit embedded primitives and top-level vector graphics (rectangles, polylines/polygons, circles, three-point circular arcs, cubic Béziers, ellipses and elliptical arcs), using finite symbol rotations and X/Y mirrors, analytic circular-arc and cubic Bézier centerline intersections, top-level and embedded-symbol straight-path/Bézier stroke proximity, explicit nonzero wire/graphic combined stroke envelopes, conservative 0.01 mm flattened circular/elliptical-arc stroke checks, adaptive rotated ellipse paths with closed-body semantics, embedded library-symbol text/text-box bounds, free-standing top-level plain-text glyph bounds, and orientation-aware PNG/JPEG bitmap quadrilaterals; it also checks stroked bus lines and bus-entry strokes with local matching bus-entry endpoint exceptions, no-connect X marker strokes at KiCad's fixed 48-mil size, including the project schematic default pen width when `--project` supplies it (without-project callers remain centerline-only), hierarchical sheet frames with local sheet-pin endpoint exceptions, enabled table borders/separators, table-cell glyphs, visible Reference/Value fields, intermediate pins and top-level annotation text boxes. Embedded-symbol pin-endpoint contact is locally excluded without exempting the rest of the wire, while circles retain existing body-interior semantics and their stroke rings expand by explicitly declared wire half-width. A wire width of 0 or an omitted width resolves per physical wire component when the exported pin-to-net mapping identifies exactly one logical net. The project resolver combines exact assignments, exact/wildcard netclass patterns and committed net-chain classes, applies netclass priority, and inherits the Default width when an assigned class omits it. Regex/bus-member patterns and schematic directive/rule-area/netclass-label assignments, as well as ambiguous or unmapped physical components, remain conservative/unknown. Free-standing schematic text is checked with estimated glyph bounds; label glyphs are intentionally excluded because their anchors may terminate wires. SCH009/SCH012 containment and other flattened-curve consumers still use 0.01 mm sampled paths, image obstacles use header-derived dimensions and their stored center/orientation, table border/separator geometry supports consistent 0°/90°/180°/270° cell rotations after validating every serialized cell origin against KiCad's rotated grid; inconsistent and non-cardinal layouts are conservatively skipped. Table-cell glyph bounds separately support 0°/90°/180°/270° cardinal text rotations; non-cardinal cell text rotations remain skipped.

Some migrated rules intentionally implement only a subset of their historical requirements. SCH009 now checks member origins/pins/fields and symbol primitives, wire segments connected to member pins or intersecting the configured frame, frame-overlapping labels and top-level plain text, the description and overlapping text boxes, top-level vector graphics, and embedded PNG/JPEG image bounds. SCH012 applies its configured inset to those same object families. Arc/Bézier geometry is sampled, text extents are estimates, and image bounds follow embedded PPI plus KiCad's stored scale/version rules; unknown image encodings and other non-text-box annotation types remain incomplete. Finite non-cardinal embedded pin and primitive transforms are supported; SCH005 pin-axis checks remain cardinal-only. SCH015 validates explicit no-connect marker placement against transformed pin coordinates and supports the profile `required_no_connect_pins` attribute; unresolved pin geometry is reported as unverified. The attribute must not list library pins with electrical type `unconnected`; automatic pin-type filtering remains future work. SCH010 still uses estimated text bounds.

## Project profiles

Version-1 JSON profiles currently support:

- Component intent, role-specific connection contracts, explicit pin-to-net expectations, and decoupling terminal-to-target-pin verification through the KiCad-exported netlist.
- Functional-group members, documentation text-box UUIDs, evidence references, optional native KiCad group UUID binding, and optional schematic rectangle UUID binding.
- Minimum member-origin spacing, minimum frame inset, and a per-group wire-length budget measured as the sum of segments wholly within the referenced frame.
- Explicit required no-connect pin numbers via the component `attributes.required_no_connect_pins` comma-separated string.

The profile format is deliberately explicit; project-specific expectations are not imported from a schematic generator. A broader JSON Schema and typed rule-policy options remain planned.

## Next: reduce remaining domain-specific extraction

- Continue replacing complete generic algorithms with mature libraries only when the adoption removes more implementation/maintenance surface than the adapter adds.
- Re-evaluate typed KiCad model crates only when they support in-memory parsing, source-span provenance, and the missing text-box/group/embedded-pin fact families. Avoid adding a second partial parser.
- Profile remaining collision scans before introducing an R-tree; do not add spatial indexing without a measured hot path.
- Keep the raw line/byte count honest: current substitutions have simplified algorithm ownership, but parser/CLI adapters and tests mean the measured four-file total has not yet decreased.

## Next: physical connectivity and symbol geometry

1. SCH004 now supports explicit multi-terminal `physical_wire_groups` independently of named `common_pin_groups`. Continue extending the physical-wire contract for richer path/topology constraints where pin groups alone are insufficient; any inference must use physical wire geometry, never label equality.
2. SCH013 uses analytic segment/circular-arc intersections reconstructed from the three KiCad control points and directed midpoint-selected sweep, plus Kurbo's analytic cubic Bézier/path-segment intersections from the four original control points. Top-level and embedded-symbol straight-path/Bézier stroke clearance compare wire segments against half the declared graphic stroke width with Kurbo minimum-distance; circular arcs use their existing 0.01 mm flattened paths with a conservative margin. For embedded symbols, only the wire portion within the matching pin endpoint's local stroke-clearance radius is exempt, and the remaining wire is still checked; circles retain existing body-interior semantics. Extend the effective per-wire netclass resolver to KiCad regex/bus-member pattern syntax and schematic directive-label, label-property and rule-area assignments; verify ambiguous physical-topology and netlist cases remain unknown. Remaining unsupported vector obstacle classes and non-cardinal/inconsistent table layouts also remain pending.
3. Extend SCH009/SCH012 beyond the current vector graphics and PNG/JPEG bitmap bounds to cover unsupported image encodings and remaining annotation types; improve exact body-curve coverage.
4. Broaden multi-unit, alternate-convert and mirrored transform coverage. Finite non-cardinal rotations now resolve pin and primitive coordinates for frame/obstacle rules; SCH005 axis checks remain cardinal-only, and unresolvable geometry must continue to be skipped conservatively.

## Rule configuration and extensibility

- Add typed per-rule options and schema versioning for the implemented `kicad-clippy.toml` policy file.
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
