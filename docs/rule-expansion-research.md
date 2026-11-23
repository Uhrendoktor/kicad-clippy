# Rule expansion research and implementation backlog

**Status:** living design backlog for PR #27/#28 and follow-on implementation through PR #31.
**Integration baseline:** PR #27 was merged into `main` as squash commit `9fb78ebe9924c7d557c5ca0b1a400a12761283cc` on 2026-11-19. PR #28 is rebased directly onto `main`; the candidates below remain future work unless explicitly marked implemented.  
**Scope distinction:** the current implementation parses both `.kicad_sch` and `.kicad_pcb` S-expressions. SCH023–SCH025, PCB001 and the first PCB009 test-point exposure contract are implemented as described below. PCB002–PCB008 and PCB010–PCB012 remain research candidates; full PCB path/zone/stackup analysis still requires a copper connectivity model and dedicated board fixtures.

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
| Frame title/tab geometry | Tab attached to frame, title inside tab, title not clipped, frame-to-frame gap | SCH028 checks explicit frame/tab/title UUIDs, rectangular edge attachment, title-box containment and optional minimum Euclidean frame gap; exact glyph clipping remains unverified | Implemented (partial text geometry) |
| Grid alignment | Symbol origins, pins, labels, junctions and wire endpoints on project grid | SCH026 uses explicit spacing/origin/tolerance, symbol-origin and label-anchor defaults, opt-in pins/junctions/wire endpoints, and narrowly scoped exemptions | Implemented (opt-in project contract) |
| Hierarchical UUID uniqueness | Duplicate object UUIDs across all sheets and hierarchy paths | SCH024 traverses a path-based root schematic and follows distinct relative or absolute `Sheetfile` paths; canonical paths prevent cycles/repeated loads and missing or invalid children are reported as unverified warnings. Project-variable expansion remains future work | Implemented (path-based hierarchy) |
| Label-scope policy | Forbid/allow global labels; require named labels to use local/global/hierarchical scope on the loaded sheet | SCH027 implements global allow/deny lists and required named-label scopes on the loaded sheet. Parent/child sheet-boundary checks and project-wide aggregation remain future work; do not reimplement native name resolution | Done (partial) |
| Required no-connect intent | Explicit markers for profile-declared intentionally unused pins | Implemented by SCH015 using transformed embedded-pin coordinates and effective library pin type, including selected alternates | Done |
| Redundant no-connect flags | Report explicit markers on pins whose effective library type is `unconnected` or `no_connect`, whether or not the pin appears in a profile | Implemented by SCH015 for all resolved placed pin facts, including selected units/alternates; ambiguous/unresolved pin geometry is not guessed | Done (resolved pins) |
| Field-to-body maximum gap | Per-reference Reference/Value gap from transformed symbol body | Implemented by SCH010 with per-component/per-field thresholds and estimated glyph rectangles; curve-based body outlines remain unverified | Done (partial geometry) |
| Functional-group spacing | Body clearance, alignment, adjacency and subgroup nesting | SCH006 implements minimum body clearance and axis alignment, maximum pairwise body-gap limits for cluster compactness, and conservative flattened circular-arc/cubic-Bézier body outlines; adjacency, subgroup nesting, and exact non-flattened curve-distance geometry remain future work | P1 (partial) |
| Title-block metadata | Required title, revision, company, date and comment fields | Implemented by opt-in SCH023 `required_title_block_fields` for native title/date/rev/company/comment1–comment9 fields on the current sheet; sheet number/count, project variables and hierarchy-wide policies remain future work | P2 (partial) |
| Component population/DNP | Optional parts retain real populated value and use DNP property rather than NC/DNP text in Value | Approved BOM/variant metadata and schematic DNP flag | P2 |
| Decoupling coverage | Require every explicitly declared IC supply pin to have at least one decoupling capacitor contract | Implemented by SCH022 with `attributes.required_decoupling_pins`; exported netlist verifies each required target pin exists | Done (opt-in contract) |
| Decoupler placement | Maximum capacitor-to-target-pin distance in the schematic and maximum target-pad-to-capacitor-pad distance on the PCB; cluster compactness/return-loop quality | SCH022 checks optional `max_target_distance_mm` using transformed schematic pin anchors. PCB001 checks optional `max_target_pad_distance_mm` using transformed footprint pad centers and board net assignments. Copper path length, ground-via/zone connectivity and return-loop analysis remain future work | P1 (partial) |
| Bulk-capacitor contract | Rail served, polarity, placement region and minimum voltage margin | Component ratings, rail voltage/tolerance/transient policy | P2 |
| Pull-up/pull-down topology | Required pull-ups for declared open-drain interfaces; divider endpoints and bias requirements | Interface role and expected voltage/current ranges | P2 |
| LED current limiting | Required series current-limiting path for declared indicator LEDs | LED Vf/current, supply range and resistor value/tolerance/power | P2 |
| Protection path | Declared fuse, reverse-polarity, TVS/clamp, flyback or discharge path exists and has correct orientation | Role metadata, topology path and component ratings | P2 |
| Connector interface completeness | Required pin set is mapped; direction, standard, mating interface and sheet-boundary ownership are consistent | SCH021 checks opt-in `attributes.required_pins` against expected `connections.pin.N`/`pin_nets.N` mappings; direction/mating/boundary contracts remain future work | P1 (partial) |
| Test-point coverage | Required power/reset/programming/measurement nets have accessible test points | SCH025 checks profile-declared test-point component pin mappings and exported netlist assignments for each `required_test_point_nets` entry; PCB009 verifies the placed board pad uses the expected net and includes copper plus solder-mask layers. Physical probe approach, fixture reach, and enclosure access remain future work | P2 (partial) |
| Oscillator support network | Crystal pins, load capacitors and target MCU pins are mapped and documented | Device-specific oscillator topology and load-capacitor calculation | P2 |
| Analog DC return path | Declared high-impedance/analog node has a valid bias/DC return path | Circuit role and analysis of conductive paths; avoid guessing from labels | P2 |

Native ERC overlap must be reviewed before implementation. For example, do not add a generic missing-driver, pin-type-conflict, dangling-wire, or ordinary label-connectivity rule already owned by ERC. A project-specific rule may check a stronger design intent, but must not rephrase an ERC finding as a new linter check.

## 5. PCB rules that merit their own checks

PCB001 implements the first decoupling pad-center contract, and PCB009 implements the first test-point pad-exposure contract. PCB002–PCB008 and PCB010–PCB012 remain candidates. KiCad PCB DRC already covers ordinary electrical/manufacturing constraints and includes configurable high-speed constraints. The custom PCB rule family should check circuit intent and physical relationships that generic DRC cannot know from the board alone.

| Candidate | Proposed rule family | What it should check | Required metadata / caveats | Priority |
|---|---|---|---|---|
| Decoupling placement and return loop | PCB001 | Implemented first slice: maximum center-to-center distance between target IC supply pad and capacitor target pad, target/return pad net checks, and optional schematic-netlist parity | `max_target_pad_distance_mm`, target pin, capacitor target/return terminals and return-net metadata. Track/arc/via/zone connectivity, pad-edge distance, nearby ground vias, copper path length and return-loop quality remain future work | P1 (partial) |
| Switching power-stage hot loop | PCB002 | Input capacitor/MOSFET/rectifier/inductor/output-capacitor placement and high-di/dt loop length/area; switch-node copper exposure and proximity to sensitive signals | Topology role map, switching-node nets, device-specific layout guidance; thresholds must come from controller datasheet/app note | P1 |
| Kelvin/current-sense routing | PCB003 | Sense traces originate at resistor pad centers, run as a differential pair, avoid carrying load current and stay away from switching nodes | Sense-resistor reference/pads, sense pin pair, current direction and noise-sensitive net roles | P1 |
| Oscillator/crystal layout | PCB004 | Crystal and load capacitors near oscillator pins; short load-cap return; separation from fast clocks/switching traces; reference-ground continuity below/around oscillator where applicable | Device-specific oscillator topology and datasheet/app note; guard rings are not universal | P2 |
| High-speed interface intent | PCB005 | Connector-to-ESD-to-transceiver ordering, pair continuity through protection, avoid stubs and unintended layer transitions, and ensure impedance/stackup evidence exists | Interface type (USB, Ethernet, LVDS, etc.), topology, stackup and loss/impedance targets; delegate pair width/gap/skew/length to KiCad DRC where supported | P1 |
| Ground/return-path integrity | PCB006 | Fast/sensitive signals do not cross gaps in their reference plane; critical return path remains continuous around layer transitions; declared analog return strategy is preserved | Net/layer intent and plane-region mapping; never universally require split AGND/DGND planes | P1 |
| Power-distribution/thermal capacity | PCB007 | Declared current paths do not neck down below project limits; copper area/via arrays/thermal paths meet component and board assumptions | Continuous current, peak current, copper thickness, layer count, temperature rise and manufacturer/IPC guidance; requires a validated model, not a generic width guess | P2 |
| Protection-device placement | PCB008 | TVS/ESD parts close to entry connector; low-inductance path to reference return; protected route proceeds from device to protected circuitry; fuse/reverse-protection ordering | Interface and source-protection topology; device manufacturer layout guide | P1 |
| Testability/accessibility | PCB009 | Programming/debug interfaces are reachable; connector orientation/board-edge clearance and full probe/fixture access meet project policy | Test-point pad presence/net/exposure is implemented by PCB009; fixture geometry, probe approach/clearance, connector edge offsets and enclosure constraints remain future work | P2 (partial) |
| RF/antenna keepout | PCB010 | Antenna copper/trace keepout and component/ground restrictions; matching network proximity and feed topology | Antenna vendor reference layout, board stackup and exact 3D keepout; vendor-specific and never generic | P2 |
| Creepage/safety insulation | PCB011 | Surface creepage across board edges, slots, coatings, pollution category and altitude assumptions where planar DRC clearance is insufficient | Applicable safety standard, working/impulse voltage, material group, pollution degree, coating and manufacturing process; never infer a universal distance | P2 |
| Assembly/board-edge intent | PCB012 | Declared connector-to-edge offsets, mechanical keepouts, tooling/fiducial presence, component access and enclosure constraints | Mechanical drawing, assembly process, enclosure model; allow intentional exceptions | P2 |

### 5.1 PCB implementation dependencies

Do not begin the remaining PCB002–PCB008 and PCB010–PCB012 rules until the board model can resolve:
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

The following milestones are implemented in `main` through PR #28 and follow-up PRs #36/#37:

- Complete SCH001–SCH022 descriptor catalogue and opt-in TOML rule policy with named presets and per-rule overrides.
- SCH015 required no-connect profile intent, validated against transformed pin geometry and effective electrical type including selected alternates.
- SCH013 obstacle-family module extraction, retaining one aggregate public rule identifier.
- Separation of SCH004 physical wire topology from SCH014 exported logical pin-to-net assertions.
- SCH006 body-to-body clearance/alignment and SCH010 per-field maximum body-gap contracts.
- SCH009 independently declared frame-overlap detection, alongside the wider rule and profile documentation updates.
- Follow-up PR #36 implements SCH026 project-grid alignment, SCH027 loaded-sheet label-scope policy and SCH028 functional-frame title-tab geometry.
- Follow-up PR #37 extends SCH024 UUID uniqueness across referenced child-sheet hierarchies, including relative/absolute paths, cycle protection and explicit warnings for unresolved children.

The following PR #27/#28 cases have also been implemented in follow-ups: universal redundant no-connect-marker detection for resolved pins, opt-in required decoupling coverage per IC supply pin, opt-in schematic target-pin distance checks for decouplers, and opt-in required connector pin-map completeness. The contracts remain explicit and do not promote library-family guesses into universal rules.

Recommended future work, ordered by prerequisites:

1. Continue strengthening family-specific SCH013 tests and diagnostic-parity regressions. Extend effective netclass resolution to KiCad regex/bus-member syntax and schematic directive-label, label-property and rule-area assignments only where project semantics can be resolved without guessing.
2. Improve SCH009/SCH012 coverage for unsupported image encodings and remaining annotation types; add exact curve-based body geometry where a validated transform/distance model is available.
3. SCH006 now supports maximum pairwise body gaps for explicit cluster compactness and conservatively includes flattened circular-arc/cubic-Bézier body outlines with a documented tolerance margin. Extend it with adjacency and subgroup-nesting contracts; exact non-flattened curve-distance geometry remains future work. SCH005 axis checks now cover non-cardinal rotations where the transformed pin axis resolves.
4. Expand role-specific schematic contracts for still-uncovered intent such as pull-up/pull-down topology, LED current limiting, protection paths, oscillator support and bulk-capacitor requirements. SCH021 connector pin-map completeness, SCH025 test-point coverage and SCH022 decoupling contracts already cover their documented opt-in slices; direction/mating-interface and physical fixture-access evidence remain future work.
5. Extend the `.kicad_pcb` parser and board-fact model beyond pad layers/anchors; prioritize PCB003, PCB004 and PCB008 against annotated routing and board-outline fixtures. PCB002–PCB008 and PCB010–PCB012 remain unimplemented candidates except the documented partial PCB001/PCB009 slices.
6. Add board-layer/stackup/zone return-path analysis only after path connectivity and KiCad DRC integration are validated.
7. Add controlled project-variable expansion for hierarchical `Sheetfile` paths, using KiCad-compatible resolution and explicit warnings for unsupported or unresolved variables.

**Do not implement every candidate at once.** Each rule should arrive with a narrow contract, exact input facts, a documented false-positive policy, positive/negative fixtures and an explicit statement of what native ERC/DRC continues to own.


### Implemented PCB evidence boundary

The new `BoardDocument` parser retains the source text and S-expression spans for `.kicad_pcb` documents. PCB001 currently extracts placed F.Cu/B.Cu footprint pad anchors, applies the serialized footprint scale/rotation (KiCad already stores flipped pad-local coordinates and the adjusted footprint orientation), resolves each footprint reference and pad number, and checks the declared target/capacitor distance. PCB009 additionally reads each pad's layer list and verifies that required test-point pads include both copper and solder-mask layers. It also checks the capacitor return pad against the declared return net and, when available, compares target/return pad net names with the exported schematic netlist. It does not yet extract routed tracks, arcs, vias, filled zones, board stackup or continuous copper regions. PCB002–PCB008 and PCB010–PCB012 should not be described as implemented until their required board facts and positive/negative fixtures exist.


### Implemented test-point evidence boundary

SCH025 is profile-driven: every critical net listed in `required_test_point_nets` must map to at least one component declared with kind/role `test_point`, using an explicit `pin_nets` mapping. The exported schematic netlist confirms that the mapped pin belongs to the requested net. PCB009 resolves the same reference/pad number in the board, verifies the assigned net, and checks for copper plus solder-mask layers. This does not prove that a probe can physically reach the pad or that an automated fixture has sufficient approach clearance.


### Implemented project-grid boundary

SCH026 is an opt-in profile contract. It checks only selected anchor families, defaulting to placed symbol origins and label anchors. Pin anchors, junctions and wire endpoints are explicit opt-ins. Grid spacing, origin and tolerance are validated, and narrowly scoped reference/pin/label/coordinate exemptions are available. It neither changes KiCad's project/editor grid nor overrides native ERC findings.

### Implemented label-scope boundary

SCH027 applies global-label allow/deny lists and required label-name scope declarations to labels in the currently loaded schematic file. It does not aggregate project-wide labels, resolve hierarchical labels against parent-sheet pins, or replace KiCad's native label/net resolution.

### Implemented frame-title geometry boundary

SCH028 is opt-in per functional group. The profile links a frame rectangle, attached title tab and title text box by UUID. The rule requires a non-zero shared edge segment between tab and frame and checks that the serialized title-box rectangle lies within the tab. Optional minimum frame gaps use Euclidean rectangle-to-rectangle distance. It does not infer arbitrary group frames or simulate exact glyph rendering/clipping.


### Implemented hierarchical UUID boundary

SCH024 starts from the path-based schematic supplied to the CLI, reads each `Sheetfile` property on native `sheet` objects, resolves relative paths from the owning document, and follows absolute paths as written. It recursively checks each distinct canonical child path, preventing cycles or repeated file references from double-counting one document. Missing or invalid child sheets are warnings: the remaining hierarchy is checked, but the result explicitly states that uniqueness could not be verified for that child. In-memory documents without a corresponding file path retain local-document scope. Reused child files are loaded once because UUID identity is file-based rather than per-instance. Project-variable expansion in sheet filenames remains unsupported.
