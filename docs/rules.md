# Built-in rules

The historical checker defines stable IDs SCH001–SCH020. The Rust migration preserves those IDs and separates reusable sheet-level checks from project-profile contracts.

| ID | Historical rule | Rust status |
| --- | --- | --- |
| SCH001 | Wire geometry | Implemented: endpoint count, orthogonal segments, zero-length wires, collinear overlap, crossings and unmarked T-junctions |
| SCH002 | Label topology | Implemented: physical wire-island graph, local-label reuse across distinct islands, duplicate-label detection on one island, and hierarchical twig termination |
| SCH003 | Label/wire glyph overlap | Implemented: estimated visible glyph bounds and strict wire-interior intersection |
| SCH004 | Physical net connectivity | Implemented partial: profile-declared direct pin pairs and common-pin groups; verifies pin attachment, physical wire-only connectivity, expected labels, optional one-label cardinality, and scoped forbidden labels |
| SCH005 | Pin orientation | Implemented: checks connected wire axes against embedded pin geometry for selected unit/convert, right-angle rotation, and X/Y mirroring |
| SCH006 | Component spacing | Implemented when group profiles declare `minimum_member_origin_spacing_mm`: enforces Euclidean spacing between placed symbol origins; not a body-clearance check |
| SCH007 | Group metadata | Implemented when profiles provide `kicad_group_uuid`: resolves native KiCad group and verifies placed-symbol membership against declared component references |
| SCH008 | Annotation text-box geometry | Implemented: required geometry/font/margins, sizing, overlap and wire penetration |
| SCH009 | Group containment | Implemented opt-in: checks group member symbol origins, resolved pin coordinates, estimated visible property-field bounds, and the group description text-box bounds against its referenced top-level KiCad rectangle; symbol-body, label, wire and other annotation containment remain pending |
| SCH010 | Symbol field clearance | Implemented partial: estimated visible-field bounds are checked against other fields, own/other embedded symbol bodies, wires, pins, labels and annotation boxes; exact font rendering and maximum field-to-body spacing remain pending |
| SCH011 | Block annotation structure | Implemented: multiline text boxes require a visible border |
| SCH012 | Frame content clearance | Implemented partial: profile-defined inset checks for group member symbol origins, resolved embedded pin coordinates, estimated visible property glyph bounds and description text-box bounds relative to a referenced rectangle; labels, wires and other frame contents remain pending |
| SCH013 | Wire obstacle clearance | Implemented partial: checks wires against selected-unit embedded rectangles, polylines/polygons, circles and sampled three-point circular arcs (target sagitta <= 0.01 mm) and cubic Béziers (target flatness <= 0.01 mm; each curve capped at 4096 segments), plus intermediate pins, visible Reference/Value text and annotation boxes; geometric checks remain approximations |
| SCH014 | Shared pin/net contracts | Implemented for profile-declared component pins and connector `pin.N` mappings against exported KiCad netlist membership |
| SCH015 | Required no-connect markers | Implemented: verifies each explicit no-connect marker coincides with an embedded placed-symbol pin; incomplete/non-cardinal pin models are conservatively skipped |
| SCH016 | Wire compactness | Implemented: per-group total length of wire segments wholly inside the referenced frame divided by a positive `wire_length_budget_mm`; over-budget results are warnings |
| SCH017 | Circuit documentation | Implemented: group membership, description text-box UUID and document/page/figure or reviewed screenshot evidence |
| SCH018 | Label text collision | Implemented: estimated glyph bounds against other labels, visible symbol fields and annotation boxes |
| SCH019 | Label-kind consistency | Implemented |
| SCH020 | Component function/rationale documentation | Implemented for metadata-declared components |

## Rule boundaries

KiCad native ERC remains the electrical authority. Rust rules must not duplicate pin-type conflicts, undriven power pins, or native unconnected-pin analysis. Rules requiring exact pin-to-net facts must consume KiCad's exported netlist, not infer connectivity from equal label strings.

SCH004 is an opt-in physical contract for profile-declared `direct_wire_pairs` and `common_pin_groups`. It verifies resolved pin anchors are attached to wire segments; direct pairs and groups with `wire_connected` must belong to the same wire-only component. Common-pin groups also require the expected label on each pin component and can require exactly one matching label. With `netlist_connected`, they additionally require every listed pin to resolve to the same exported KiCad net. Labels do not create physical graph edges. It does not infer all physical-net intent automatically or replace native ERC.

SCH004’s physical graph follows SCH001’s explicit-junction contract: an endpoint touching the interior of another segment is not joined to that segment unless a junction marker is present. End-to-end wire segments remain connected without an additional junction marker.

SCH005 intentionally checks only connected-wire axis alignment; KiCad native ERC remains responsible for electrical pin compatibility and unconnected-pin findings. Pin layouts that cannot be resolved from the embedded symbol definition or use non-cardinal symbol rotation are skipped rather than guessed.

SCH015 checks only no-connect marker placement against embedded pin coordinates; it skips the rule if any placed symbol pin model is incomplete or unsupported. KiCad native ERC remains responsible for electrical unconnected-pin analysis.

SCH009 checks that group member symbol origins and resolved pin points, estimated visible symbol-property field rectangles, and the configured group description text-box rectangle are contained by the referenced frame. It does not yet check embedded symbol-body outlines, labels, wires or other annotation objects. SCH012 separately applies a configured positive inset to the member origins, pins, estimated field rectangles and description text box.

SCH010 estimates visible symbol-field text bounds from the stored field position, font size, angle and justification. It checks field-to-field and field-to-annotation-box overlap, field-to-wire and field-to-pin collisions, label-glyph collisions, and intersections with the field's own or another symbol's embedded outline. A Value field fully contained within a closed symbol primitive is allowed. The bounds are estimates rather than KiCad's exact rendered glyph geometry; the historical maximum-distance-to-body rule and exact gaps are not implemented.

A rule ID is not considered ported merely because it appears in the catalogue. A rule becomes implemented only when it has positive and negative tests and a documented analysis model.
