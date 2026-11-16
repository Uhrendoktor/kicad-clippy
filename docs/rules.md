# Built-in rules

The historical checker defines stable IDs SCH001–SCH020. The Rust migration preserves those IDs and separates reusable sheet-level checks from project-profile contracts.

| ID | Historical rule | Rust status |
| --- | --- | --- |
| SCH001 | Wire geometry | Implemented: endpoint count, orthogonal segments, zero-length wires, collinear overlap, crossings and unmarked T-junctions |
| SCH002 | Label topology | Implemented: physical wire-island graph, local-label reuse across distinct islands, duplicate-label detection on one island, and hierarchical twig termination |
| SCH003 | Label/wire glyph overlap | Implemented: estimated visible glyph bounds and strict wire-interior intersection |
| SCH004 | Physical net connectivity | Pending exported-netlist and physical wire-graph integration |
| SCH005 | Pin orientation | Implemented: checks connected wire axes against embedded pin geometry for selected unit/convert, right-angle rotation, and X/Y mirroring |
| SCH006 | Component spacing | Pending profile-driven spacing limits |
| SCH007 | Group metadata | Pending profile-driven group extraction |
| SCH008 | Annotation text-box geometry | Implemented: required geometry/font/margins, sizing, overlap and wire penetration |
| SCH009 | Group containment | Pending symbol/pin/field/frame geometry |
| SCH010 | Symbol field clearance | Pending actual embedded symbol primitives and text glyph geometry |
| SCH011 | Block annotation structure | Implemented: multiline text boxes require a visible border |
| SCH012 | Frame content clearance | Pending |
| SCH013 | Wire obstacle clearance | Pending embedded symbol primitive geometry |
| SCH014 | Shared pin/net contracts | Implemented for profile-declared component pins and connector `pin.N` mappings against exported KiCad netlist membership |
| SCH015 | Required no-connect markers | Implemented: verifies each explicit no-connect marker coincides with an embedded placed-symbol pin; incomplete/non-cardinal pin models are conservatively skipped |
| SCH016 | Wire compactness | Pending profile group/frame geometry |
| SCH017 | Circuit documentation | Implemented: group membership, description text-box UUID and document/page/figure or reviewed screenshot evidence |
| SCH018 | Label text collision | Implemented: estimated glyph bounds against other labels, visible symbol fields and annotation boxes |
| SCH019 | Label-kind consistency | Implemented |
| SCH020 | Component function/rationale documentation | Implemented for metadata-declared components |

## Rule boundaries

KiCad native ERC remains the electrical authority. Rust rules must not duplicate pin-type conflicts, undriven power pins, or native unconnected-pin analysis. Rules requiring exact pin-to-net facts must consume KiCad's exported netlist, not infer connectivity from equal label strings.

SCH005 intentionally checks only connected-wire axis alignment; KiCad native ERC remains responsible for electrical pin compatibility and unconnected-pin findings. Pin layouts that cannot be resolved from the embedded symbol definition or use non-cardinal symbol rotation are skipped rather than guessed.

SCH015 checks only no-connect marker placement against embedded pin coordinates; it skips the rule if any placed symbol pin model is incomplete or unsupported. KiCad native ERC remains responsible for electrical unconnected-pin analysis.

A rule ID is not considered ported merely because it appears in the catalogue. A rule becomes implemented only when it has positive and negative tests and a documented analysis model.
