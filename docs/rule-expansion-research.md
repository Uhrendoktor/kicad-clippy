# Rule expansion research and implementation backlog

**Status:** living design backlog for the Rust implementation merged in PR #27 and follow-on work in PR #28.
**Integration baseline:** PR #27 was merged into `main` as squash commit `9fb78ebe9924c7d557c5ca0b1a400a12761283cc` on 2026-11-19. PR #28 is rebased directly onto `main`; the candidates below remain future work unless explicitly marked implemented.  
**Scope distinction:** the current parser and rules analyse .kicad_sch schematics. PCB-specific checks below are researched candidates, not implemented PCB functionality. A PCB parser/model and board-level fixtures are prerequisites.

## 1. Design principles

1. **Complement KiCad; do not replace it.** Native ERC owns electrical pin conflicts, missing drivers, native unconnected-pin semantics and other electrical checks. Native PCB DRC owns manufacturing/electrical constraints such as copper clearances, widths, holes, mask constraints, thermal-relief spokes, differential-pair constraints, permitted footprint layers/orientations and board connectivity.
2. **Lint declared intent, not guesses.** Circuit-specific rules must be opt-in through profiles or design-policy metadata. A TVS, fuse, ground partition, pull-up or protection path is not universally required merely because a component or net name looks familiar.
3. **Use authoritative geometry and connectivity.** Pin locations come from embedded symbol definitions and transforms; logical pin-to-net assertions use KiCad's exported netlist; PCB path rules use actual pad/track/via/zone geometry. Do not infer electrical connectivity from equal label strings or component origins.
4. **Separate physical, logical and presentation contracts.** A continuous drawn wire, a label-joined net, and a visually clear layout are different properties and should produce different findings.
5. **Make thresholds explainable.** Distances, current limits, copper weights, voltage/creepage classes, thermal assumptions and allowed exceptions must be explicit project/board inputs, with their source recorded. Do not hard-code one datasheet's recommendation as a universal rule.
6. **Require positive and negative fixtures.** A new rule is implemented only after it has a documented analysis model, a counterexample/negative test, an acceptance/positive test, and verification against KiCad where topology or electrical behavior can change.

## 2. Work started in this follow-up

### 2.1 SCH009: overlapping declared functional frames

Implemented in this PR: compare the rectangles referenced by different profile groups via frame_uuid and report an SCH009 error when their interiors overlap. Edge contact is allowed. Arbitrary schematic rectangles are not treated as group frames.

Why it belongs here: the original kicad-blocks checklist explicitly requires functional frames not to overlap. The check must use explicitly declared frame UUIDs so decorative or unrelated rectangles are not misclassified.

### 2.2 SCH015: required no-connect intent

Implemented in this PR as an opt-in component attribute:

**components.<reference>.attributes.required_no_connect_pins**

The value is a comma-separated list of pin numbers, for example:

~~~json
{
  "components": {
    "U1": {
      "kind": "integrated_circuit",
      "role": "controller",
      "function": "Power delivery controller",
      "rationale": "Owns USB-PD negotiation",
      "attributes": {
        "required_no_connect_pins": "3, 7"
      }
    }
  }
}
~~~

Each listed pin must resolve to an embedded placed-symbol pin and have a no-connect marker at its transformed coordinate. If pin geometry cannot be resolved, the check emits a warning that the contract is unverified instead of claiming the marker is absent.

Electrical boundary: the validator resolves the effective embedded library pin electrical type, including a selected alternate, before requiring a marker. It rejects declarations for `unconnected` and `no_connect` pin types because those types already denote a non-connectable pin. This prevents redundant requirements within the explicit profile contract; automatically scanning every existing marker for redundant placement remains future work. Native ERC remains authoritative for electrical pin semantics.

## 3. Existing rule refactoring decisions

### 3.1 SCH013: split by obstacle family, not by primitive

SCH013's obstacle-family implementation has been extracted into internal modules for annotations, electrical graphics, sheet graphics and pins, while retaining SCH013 as the single public aggregate rule ID. The module boundary keeps distinct geometry and endpoint exceptions isolated without multiplying public identifiers. The obstacle families remain:

| Proposed rule family | Included objects | Distinct reason |
|---|---|---|
| Wire–symbol clearance | Embedded symbol bodies/primitives, intermediate pins, visible symbol fields/properties | Pin-termination exceptions and symbol transforms |
| Wire–annotation clearance | Free-standing text, embedded symbol text, text boxes and visible fields | Glyph estimates, visibility and text-box geometry |
| Wire–sheet-graphics clearance | Sheet rectangles/polylines/polygons/circles/arcs/Béziers/ellipses, images, hierarchical sheet frames and tables | Graphic stroke widths, frame endpoints, bitmap footprints and table-cell spans |
| Wire–electrical-graphics clearance | Buses, bus entries and no-connect marker strokes | Electrical-graphics endpoint exceptions and fixed marker dimensions |

Keep one stable parent identifier or provide compatibility aliases when diagnostic codes are split. Do not create separate rules for every primitive shape. The objective is independent configuration and clear remediation, not a larger catalogue.

Current status and remaining guardrails:
- [x] Extract obstacle-family implementations into internal modules while retaining the SCH013 diagnostic identifier.
- [x] Add the TOML policy layer with `default`, `strict` and `advisory` presets plus per-rule `allow`, `warn` and `deny` levels.
- [x] Preserve SCH013 as the aggregate compatibility identifier; no child rule IDs have been introduced.
- Continue adding family-specific positive/negative cases and diagnostic-parity regressions whenever an obstacle family changes.
- Introduce public child identifiers only if independent configuration/remediation proves valuable and a schema-versioned transition can preserve the aggregate SCH013 behavior.

### 3.2 SCH004 and SCH014: physical versus logical connectivity

The separation is implemented. SCH004 owns profile-declared physical wire topology: direct joins, common physical wire groups, physical wire groups, required physical separation, expected labels and label-cardinality/forbidden-label policy. It does not consume exported netlist facts. SCH014 owns logical pin-to-net contracts from KiCad's exported netlist, including component/connector pin mappings and expected commonality/separation. Avoid duplicating the same exported-netlist assertion in both rules.

A label can join separate physical islands in KiCad's logical netlist; it must not be treated as proof that visible wire continuity exists. KiCad's schematic documentation explicitly states that same-named labels connect according to their scope and that wire endpoints, rather than arbitrary crossings, establish direct wire connections.

### 3.3 SCH010 and SCH006: placement constraints

- Implemented SCH010 per-component/per-field maximum gaps from estimated Reference/Value glyph rectangles to selected-unit transformed body geometry. Per-reference settings provide documented exceptions (for example, a long value placed deliberately on a clear side); exact renderer glyph extents remain future work.
- Implemented SCH006 body-to-body clearance and horizontal/vertical member-origin alignment in addition to the existing symbol-origin spacing contract. Adjacency/cluster compactness and reference-specific spacing remain future work. Keep electrical/mechanical courtyard constraints in PCB DRC.
- Keep text bounds explicitly approximate until the KiCad renderer can provide exact glyph extents; do not describe estimated bounds as exact font rendering. Arc/Bézier symbol body outlines remain unverified until a suitable transformed curve-distance model is added.

## 4. Schematic rules that merit their own checks

These are candidates for separate identifiers or separately configurable child rules. They are not all committed scope; prioritize by project need and available evidence.

| Candidate | Suggested scope | Configuration/evidence required | Priority |
|---|---|---|---|
| Functional-frame overlap | Overlap between explicitly declared group frames | Group frame_uuid; implemented under SCH009 in this PR | Done here |
| Frame title/tab geometry | Tab attached to frame, title inside tab, title text not clipped, frame-to-frame gap | Explicit frame/title UUIDs and renderer-aware bounds | P1 |
| Grid alignment | Symbol origins, pins, labels, junctions and wire endpoints on project grid | Grid spacing and per-object exceptions; native ERC already reports some off-grid pin/wire ends | P1 |
| Hierarchical UUID uniqueness | Duplicate object UUIDs across all sheets and hierarchy paths | Complete hierarchical traversal; deterministic duplicate locations | P1 |
| Label-scope policy | Forbid/allow global labels by scope; require hierarchical labels at declared sheet boundaries; detect unintended cross-sheet scope | Profile allowlist/denylist and hierarchy model; do not reimplement native name resolution | P1 |
| Required no-connect intent | Explicit markers for profile-declared intentionally unused pins | Implemented by SCH015 using transformed embedded-pin coordinates and effective library pin type, including selected alternates | Done |
| Redundant no-connect flags | Report explicit markers on pins whose effective library type is `unconnected` or `no_connect`, whether or not the pin appears in a profile | Implemented by SCH015 for all resolved placed pin facts, including selected units/alternates; ambiguous/unresolved pin geometry is not guessed | Done (resolved pins) |
| Field-to-body maximum gap | Per-reference Reference/Value gap from transformed symbol body | Implemented by SCH010 with per-component/per-field thresholds and estimated glyph rectangles; curve-based body outlines remain unverified | Done (partial geometry) |
| Functional-group spacing | Body clearance, alignment, adjacency and subgroup nesting | SCH006 implements body clearance and axis alignment; adjacency, cluster compactness and subgroup nesting remain future work | P1 (partial) |
| Title-block metadata | Required title, revision, company, date, sheet number/count and project fields | Project policy and variable expansion rules | P2 |
| Component population/DNP | Optional parts retain real populated value and use DNP property rather than NC/DNP text in Value | Approved BOM/variant metadata and schematic DNP flag | P2 |
| Decoupling coverage | Require every explicitly declared IC supply pin to have at least one decoupling capacitor contract | Implemented by SCH022 with `attributes.required_decoupling_pins`; exported netlist verifies each required target pin exists | Done (opt-in contract) |
| Decoupler placement | Maximum capacitor-to-target-pin distance in the schematic and cluster compactness/PCB return-loop quality | SCH022 checks optional `max_target_distance_mm` using transformed symbol-pin anchors. Capacitor cluster compactness beyond target distance and PCB pad/copper return-loop analysis remain future work | P1 (partial) |
| Bulk-capacitor contract | Rail served, polarity, placement region and minimum voltage margin | Component ratings, rail voltage/tolerance/transient policy | P2 |
| Pull-up/pull-down topology | Required pull-ups for declared open-drain interfaces; divider endpoints and bias requirements | Interface role and expected voltage/current ranges | P2 |
| LED current limiting | Required series current-limiting path for declared indicator LEDs | LED Vf/current, supply range and resistor value/tolerance/power | P2 |
| Protection path | Declared fuse, reverse-polarity, TVS/clamp, flyback or discharge path exists and has correct orientation | Role metadata, topology path and component ratings | P2 |
| Connector interface completeness | Required pin set is mapped; direction, standard, mating interface and sheet-boundary ownership are consistent | SCH021 checks opt-in `attributes.required_pins` against expected `connections.pin.N`/`pin_nets.N` mappings; direction/mating/boundary contracts remain future work | P1 (partial) |
| Test-point coverage | Required power/reset/programming/measurement nets have accessible test points | Critical-net list and project test strategy | P2 |
| Oscillator support network | Crystal pins, load capacitors and target MCU pins are mapped and documented | Device-specific oscillator topology and load-capacitor calculation | P2 |
| Analog DC return path | Declared high-impedance/analog node has a valid bias/DC return path | Circuit role and analysis of conductive paths; avoid guessing from labels | P2 |

Native ERC overlap must be reviewed before implementation. For example, do not add a generic missing-driver, pin-type-conflict, dangling-wire, or ordinary label-connectivity rule already owned by ERC. A project-specific rule may check a stronger design intent, but must not rephrase an ERC finding as a new linter check.

## 5. PCB rules that merit their own checks

KiCad PCB DRC already covers ordinary electrical/manufacturing constraints and includes configurable high-speed constraints. The custom PCB rule family should check circuit intent and physical relationships that generic DRC cannot know from the board alone.

| Candidate | Proposed rule family | What it should check | Required metadata / caveats | Priority |
|---|---|---|---|---|
| Decoupling placement and return loop | PCB001 | Capacitor proximity to target IC supply pad; short supply/return path; nearby ground via/plane connection; cluster compactness; capacitor-side relationship | Target pin, capacitor terminal, return net, capacitance/rating and datasheet-derived distance; must evaluate actual footprint pads and routed copper, not component origins | P1 |
| Switching power-stage hot loop | PCB002 | Input capacitor/MOSFET/rectifier/inductor/output-capacitor placement and high-di/dt loop length/area; switch-node copper exposure and proximity to sensitive signals | Topology role map, switching-node nets, device-specific layout guidance; thresholds must come from controller datasheet/app note | P1 |
| Kelvin/current-sense routing | PCB003 | Sense traces originate at resistor pad centers, run as a differential pair, avoid carrying load current and stay away from switching nodes | Sense-resistor reference/pads, sense pin pair, current direction and noise-sensitive net roles | P1 |
| Oscillator/crystal layout | PCB004 | Crystal and load capacitors near oscillator pins; short load-cap return; separation from fast clocks/switching traces; reference-ground continuity below/around oscillator where applicable | Device-specific oscillator topology and datasheet/app note; guard rings are not universal | P2 |
| High-speed interface intent | PCB005 | Connector-to-ESD-to-transceiver ordering, pair continuity through protection, avoid stubs and unintended layer transitions, and ensure impedance/stackup evidence exists | Interface type (USB, Ethernet, LVDS, etc.), topology, stackup and loss/impedance targets; delegate pair width/gap/skew/length to KiCad DRC where supported | P1 |
| Ground/return-path integrity | PCB006 | Fast/sensitive signals do not cross gaps in their reference plane; critical return path remains continuous around layer transitions; declared analog return strategy is preserved | Net/layer intent and plane-region mapping; never universally require split AGND/DGND planes | P1 |
| Power-distribution/thermal capacity | PCB007 | Declared current paths do not neck down below project limits; copper area/via arrays/thermal paths meet component and board assumptions | Continuous current, peak current, copper thickness, layer count, temperature rise and manufacturer/IPC guidance; requires a validated model, not a generic width guess | P2 |
| Protection-device placement | PCB008 | TVS/ESD parts close to entry connector; low-inductance path to reference return; protected route proceeds from device to protected circuitry; fuse/reverse-protection ordering | Interface and source-protection topology; device manufacturer layout guide | P1 |
| Testability/accessibility | PCB009 | Declared test nets have accessible pads; programming/debug interfaces are reachable; connector orientation/board-edge clearance and probe access meet project policy | Test-point roles, probe/fixture geometry and enclosure/connector constraints | P2 |
| RF/antenna keepout | PCB010 | Antenna copper/trace keepout and component/ground restrictions; matching network proximity and feed topology | Antenna vendor reference layout, board stackup and exact 3D keepout; vendor-specific and never generic | P2 |
| Creepage/safety insulation | PCB011 | Surface creepage across board edges, slots, coatings, pollution category and altitude assumptions where planar DRC clearance is insufficient | Applicable safety standard, working/impulse voltage, material group, pollution degree, coating and manufacturing process; never infer a universal distance | P2 |
| Assembly/board-edge intent | PCB012 | Declared connector-to-edge offsets, mechanical keepouts, tooling/fiducial presence, component access and enclosure constraints | Mechanical drawing, assembly process, enclosure model; allow intentional exceptions | P2 |

### 5.1 PCB implementation dependencies

Do not begin PCB001–PCB012 until the board model can resolve:
- footprint reference, footprint UUID and 3D/board position/rotation/side;
- pad number, pad position/shape/layer/net and padstack/via details;
- track, arc, via and zone geometry with net/layer association;
- board outline, edge cuts, keepouts, stackup/materials and copper weights;
- schematic-to-board reference and pin/pad mapping;
- netclasses and effective custom DRC rules.

For path-based checks, a board graph over copper features is needed in addition to Euclidean component/pad distances. A nearby capacitor with a long routed return path is not a good decoupler. A geometrically short path can still be wrong if its reference plane is interrupted.

## 6. Explicitly do not duplicate native KiCad checks

Before adding a custom rule, query the supported KiCad version and identify the native owner.

| Topic | Native KiCad owner | Custom-linter boundary |
|---|---|---|
| Pin electrical conflicts, undriven inputs/power inputs, native unconnected pins | Schematic ERC | Only stronger profile-declared circuit intent |
| Wire/pin connectivity, label scope resolution and no-connect flag warnings | Schematic ERC/netlist | Visible-wire requirements, project label policy, missing markers in declared contracts |
| Copper clearance, track width, via/hole and board-outline constraints | PCB DRC | Device-specific topology/placement intent |
| Differential-pair gap, uncoupled length, skew, absolute length and routing width | PCB DRC/custom rule engine | Verify policy exists and is correctly applied; don't build a second generic length tuner |
| Footprint allowed layers/orientations and vias-under-SMD | PCB DRC | Only project-specific role constraints not representable in DRC |
| Solder-mask/paste clearances and thermal-relief spoke count | PCB DRC | Manufacturing-specific metadata/exception audit only |

For KiCad 10, the PCB Editor manual documents Electrical, Manufacturability, Highspeed design, Footprints and Custom constraint groups. It lists differential-pair length/skew/gap and absolute length constraints, footprint permitted layers/orientations, and other common constraints. Run native DRC with zones refilled and schematic/PCB parity checking enabled before fabrication.

## 7. Research sources

The following primary and vendor sources support the proposed design checks. Datasheet rules are device-specific evidence, not universal constants.

### KiCad documentation

- [KiCad 10 PCB Editor manual](https://docs.kicad.org/10.0/en/pcbnew/pcbnew.html) — native DRC, board setup constraints, high-speed constraints, footprint constraints, rule areas, stackup and fabrication checks.
- [KiCad 10 Schematic Editor manual](https://docs.kicad.org/10.0/en/eeschema/eeschema.html) — wire endpoint connection semantics, local/global/hierarchical label scope, no-connect flag versus electrical pin type unconnected, ERC findings, grids and hierarchy.

### Board-level design guidance

- [Analog Devices: Basic Guidelines for Layout Design of Mixed-Signal PCBs](https://www.analog.com/en/resources/analog-dialogue/articles/2022/09/02/02/40/what-are-the-basic-guidelines-for-layout-design-of-mixed-signal-pcbs.html) — functional block placement, connectors at board edges, decoupling capacitors/crystals near devices, short supply/return paths and the need to assess ground strategy from return-current behavior.
- [TI BQ25856-Q1 datasheet, PCB Layout Guidelines (Section 10)](https://www.ti.com/lit/ds/symlink/bq25856-q1.pdf) — current-sense Kelvin connections taken from pad centers, differential sensing traces away from switching nodes, short gate-drive loops and close bypass-capacitor placement.
- [TI LM5176 datasheet, Layout Guidelines (Section 10)](https://www.ti.com/lit/ds/snvsai1/snvsai1.pdf) — minimize buck/boost switching loop areas and place input filters, switches and current-sense components close together.
- [Microchip/Atmel AVR186: Best Practices for the PCB Layout of Oscillators](https://www.microchip.com/content/dam/mchp/documents/OTH/ApplicationNotes/ApplicationNotes/Atmel-8128-Best-Practices-for-the-PCB-Layout-of-Oscillators_ApplicationNote_AVR186.pdf) — keep crystal pins/load capacitors close, short local ground returns, avoid noisy/high-frequency traces, and use an oscillator-ground reference area where appropriate.
- [Analog Devices: Should digital and analog ground planes be separated?](https://www.analog.com/en/resources/faqs/faq_dds_digital_and_analog_gnd_planes.html) — cautions against mechanically splitting analog and digital grounds; low-impedance return planes and actual current paths matter.

## 8. Completed handoff milestones and future work

The following milestones are implemented in the current PR #28 branch:

- Complete SCH001–SCH022 descriptor catalogue and opt-in TOML rule policy with named presets and per-rule overrides.
- SCH015 required no-connect profile intent, validated against transformed pin geometry and effective electrical type including selected alternates.
- SCH013 obstacle-family module extraction, retaining one aggregate public rule identifier.
- Separation of SCH004 physical wire topology from SCH014 exported logical pin-to-net assertions.
- SCH006 body-to-body clearance/alignment and SCH010 per-field maximum body-gap contracts.
- SCH009 independently declared frame-overlap detection, alongside the wider rule and profile documentation updates.

The following PR #27/#28 cases have now been implemented in this follow-up: universal redundant no-connect-marker detection for resolved pins, opt-in required decoupling coverage per IC supply pin, opt-in schematic target-pin distance checks for decouplers, and opt-in required connector pin-map completeness. The contracts remain explicit and do not promote library-family guesses into universal rules.

Recommended future work, ordered by prerequisites:

1. Add frame title/tab geometry, hierarchical UUID uniqueness, configurable grid alignment, label-scope policy and title-block metadata contracts.
2. Continue strengthening family-specific SCH013 tests and diagnostic-parity regressions; extend the conservative netclass resolver only when KiCad project semantics can be resolved without guessing.
3. Improve SCH009/SCH012 coverage for unsupported image encodings and remaining annotation types; add exact curve-based body geometry where a validated transform/distance model is available.
4. Expand role-specific schematic contracts beyond the current SCH021/SCH022 coverage (pull-ups/dividers, LED current limiting, protection paths, connector completeness and test-point coverage).
5. Build the `.kicad_pcb` parser and board-fact model; prioritize PCB001, PCB003 and PCB008 against annotated board fixtures.
6. Add board-layer/stackup/zone return-path analysis only after path connectivity and KiCad DRC integration are validated.

**Do not implement every candidate at once.** Each rule should arrive with a narrow contract, exact input facts, a documented false-positive policy, positive/negative fixtures and an explicit statement of what native ERC/DRC continues to own.
