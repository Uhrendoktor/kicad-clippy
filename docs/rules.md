# Built-in rules

The historical checker defines stable IDs SCH001–SCH020. The Rust migration preserves those IDs and separates reusable sheet-level checks from project-profile contracts.

| ID | Historical rule | Rust status |
| --- | --- | --- |
| SCH001 | Wire geometry | Implemented: orthogonal segments, zero-length wires, crossings and unmarked T-junctions |
| SCH002 | Label topology | Initial port: local-label attachment/reuse and hierarchical twig termination; island graph refinement pending |
| SCH003 | Label/wire glyph overlap | Pending exact text-glyph bounds |
| SCH004 | Physical net connectivity | Pending exported-netlist and physical wire-graph integration |
| SCH005 | Pin orientation | Pending transformed embedded-pin geometry |
| SCH006 | Component spacing | Pending profile-driven spacing limits |
| SCH007 | Group metadata | Pending profile-driven group extraction |
| SCH008 | Annotation text-box geometry | Pending |
| SCH009 | Group containment | Pending symbol/pin/field/frame geometry |
| SCH010 | Symbol field clearance | Pending actual embedded symbol primitives and text glyph geometry |
| SCH011 | Block annotation structure | Pending; historical multiline-outline contract recorded |
| SCH012 | Frame content clearance | Pending |
| SCH013 | Wire obstacle clearance | Pending embedded symbol primitive geometry |
| SCH014 | Shared pin/net contracts | Pending KiCad-exported netlist mapping |
| SCH015 | Required no-connect markers | Pending transformed pin geometry |
| SCH016 | Wire compactness | Pending profile group/frame geometry |
| SCH017 | Circuit documentation | Group evidence pending; component rationale is enforced by SCH020 |
| SCH018 | Label text collision | Pending text metrics and field/annotation bounds |
| SCH019 | Label-kind consistency | Implemented |
| SCH020 | Component function/rationale documentation | Implemented for metadata-declared components |

## Rule boundaries

KiCad native ERC remains the electrical authority. Rust rules must not duplicate pin-type conflicts, undriven power pins, or native unconnected-pin analysis. Rules requiring exact pin-to-net facts must consume KiCad's exported netlist, not infer connectivity from equal label strings.

A rule ID is not considered ported merely because it appears in the catalogue. A rule becomes implemented only when it has positive and negative tests and a documented analysis model.
