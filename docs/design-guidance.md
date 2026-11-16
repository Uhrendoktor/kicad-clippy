# Historical design guidance and further rule research

This document records the original schematic-linter requirements recovered from KiCad Blocks pull-request discussions and the additional checks assessed for `kicad-clippy`.

## Recovered history

The most detailed original development history is [KiCad Blocks PR #20](https://github.com/Uhrendoktor/kicad-blocks/pull/20), which created the reusable SCH001–SCH019 rule set and recorded iterative style/topology feedback. [PR #49](https://github.com/Uhrendoktor/kicad-blocks/pull/49) added SCH020 component-purpose documentation and reference-specific field-clearance limits. [PR #16](https://github.com/Uhrendoktor/kicad-blocks/pull/16) records the A3 left-to-right layout conventions and additional layout regression checks.

The recovered requirements are:

1. **Functional hierarchy:** compact, unfilled module frames with attached title tabs and small notes; no oversized decorative title in the circuit field.
2. **Signal flow:** USB input/CC protection at the left, VBUS filtering and controller in the centre, power path/output to the right, with address straps close to the controller.
3. **Grid/alignment:** align related component rows and shared rails; keep consistent frame gaps and interior padding.
4. **Short local routing:** use orthogonal local wires. Use boundary ports or matching local labels for remote nets rather than long loops across functional blocks.
5. **Label scope:** local labels for internal disconnected nets, hierarchical labels only for actual sheet interfaces, and no global labels in the STUSB4500 child block. A name must not mix label kinds. Hierarchical labels terminate a short wire twig.
6. **Actual symbol geometry:** derive pin and obstacle coordinates from embedded library symbols. A wire may terminate on its intended pin, but must not cross another pin or symbol body.
7. **Readable fields:** keep references/values close to symbol geometry and out of wire corridors, pins, labels, annotations and adjacent symbol bodies. Allow a passive value deliberately placed wholly inside its body.
8. **Native annotation boxes:** use measured text boxes with consistent padding; multiline notes need visible outlines and should remain near their subject.
9. **Explicit groups:** all functional groups need a title, description, member list and frame. Components, pins, labels and notes should remain in the correct frame.
10. **Netlist-first electrical verification:** a style test is not connectivity proof. PR #20 records a routing iteration that accidentally placed FET_MID on VBUS even though Python tests passed. Each topology-changing edit required a fresh KiCad-exported netlist and critical pin-to-net assertions.
11. **STUSB4500-specific boundaries:** U1.22 is a separate VSYS hierarchical input, never GND. Regulator decouplers should be short physical connections. T4.3/T1.2/R10.1 form the common-source node. Parent-level MCU connectors and pull-ups do not belong inside the child implementation block.
12. **Population/documentation:** R51/R52 are optional 100 kΩ pull-ups and must use KiCad DNP rather than an "NC" value. SCH020 requires R1/R4/R2 function and rationale, not just reference designators.
13. **Visual review:** actual KiCad SVG and Eeschema inspection remain necessary after changes to geometry, orientation, text or frame placement.

The [STUSB4500 review checklist](https://github.com/Uhrendoktor/kicad-blocks/blob/main/examples/stusb4500/SCHEMATIC_STYLE_CHECKLIST.md) expands the original acceptance criteria. The Rust rules keep the generic checks separate from this design's profile.

## Additional checks implemented

### SCH021 — symbol-origin grid alignment

A project profile may specify a grid quantum such as 2.54 mm. The rule checks symbol origins for alignment. It is not a duplicate of KiCad ERC's own pin/wire connection-grid checks.

### SCH022 — unique object UUIDs

KiCad schematic objects use UUID identifiers. The file-format specification describes the UUID token as the globally unique identifier for the schematic and uses identifiers for symbol/sheet items. Duplicate IDs within a document are therefore a data-integrity risk. The check reports duplicates found in UUID tokens. The current implementation does not yet compare UUIDs across multiple files in a project.

### SCH023 — title-block completeness

A native title block and non-empty title make a drawing identifiable. Profiles may require additional title, date, revision, company or comment fields. The first default is warning-level to avoid turning a documentation preference into an unconditional electrical failure.

### SCH024/SCH025 — typed component intent and decoupling contracts

The user-requested metadata layer identifies the intended type and role of each component, with a function and engineering rationale. Strict mode requires every capacitor to be classified. A decoupling capacitor additionally declares the target IC pin, the capacitor terminal connected to it, its return terminal and the return-net name.

The decoupling check verifies that the named terminals physically share wire networks, that the decoupling terminal is within a configured schematic-distance threshold of the IC pin, that capacitors serving the same pin are arranged as a compact group, and that their return terminals share one physical island carrying the expected GND label.

This only checks schematic intent and visual topology. It does not establish physical PCB proximity, low-inductance routing or correct power-distribution-network behaviour.

## Further candidates

These candidates are useful, but should not become built-in stable rule IDs until their schemas and fixtures are well defined.

| Candidate | Suggested metadata | Why useful | Guardrail |
| --- | --- | --- | --- |
| Bulk-capacitor coverage per rail | Rail identity, required aggregate capacitance or expected capacitor references, rail scope | Catch a missing bulk reservoir or duplicated/incorrect rail assignment | Do not infer capacitance sufficiency without voltage, load and transient requirements. |
| Decoupling coverage per IC power pin | IC power-pin inventory, capacitor role, target pin, rail and return | Detect a declared power pin with no assigned decoupler | Must know which pins are power pins and which need local bypass from a reviewed IC contract/datasheet. |
| Resistor network role checks | Pull-up/down target, rail/net, resistance range, default state, optional population/variant | Detect missing or contradictory bias networks, default-state resistors and mis-populated straps | Electrical operating values need voltage, leakage, input thresholds and tolerance constraints. |
| Diode/protection topology | Device role, protected nets, anode/cathode terminal mapping, clamp direction | Check ESD/clamp devices are connected to the nets they are meant to protect and oriented as intended | Require explicit symbol pin mappings; do not guess polarity from graphical orientation. |
| MOSFET switch topology | Pair membership, drain/source/gate role mapping, common-source/common-drain node, control/bias network | Verify a back-to-back switch is wired as the declared topology | Use actual library pin numbers and device-specific topology; native ERC still handles pin electrical types. |
| Test-point coverage | Critical net list, approved test-point symbol IDs, accessibility or measurement intent | Ensure power rails and critical signals have test access | Profile must distinguish externally measurable nets from internal nodes and intentional no-test cases. |
| Differential-pair metadata | Pair name, P/N net names, polarity, layer/length/impedance intent | Keep high-speed pair names and constraints consistent | Schematic declaration cannot validate PCB impedance, skew or coupling. |
| Power/voltage/current constraints | Rail min/max voltage, expected current, source/sink role, protection/derating policy | Document and cross-check absolute-maximum and rating constraints | Needs trustworthy component data and design load assumptions; do not claim simulation/sign-off. |
| Footprint/part-number completeness | Manufacturer part number, footprint, value, variant and lifecycle attributes | Improve BOM and production readiness | Different lifecycle stages require different required fields; generic symbols may be intentional early on. |
| Hierarchy-wide consistency | Sheet path/page numbering, title blocks, interface contract and cross-sheet net ownership | Detect inconsistent multi-sheet design metadata | Requires project-level loading and hierarchy resolution, not one-file parsing. |

## Why KiCad ERC remains separate

KiCad's documentation explicitly notes that ERC can find many common issues but does not detect all design errors. It checks native connectivity and pin-type semantics including unconnected pins, input pins without drivers, power inputs without power-output sources, and output conflicts. The quality of results depends on the electrical types assigned to library pins.

The further custom checks should therefore not reproduce ERC's connection/pin-conflict catalogue. Instead they should evaluate declared engineering intent such as decoupler assignment, roles, topology and visual layout. References: [KiCad 10 Eeschema/ERC documentation](https://docs.kicad.org/10.0/en/eeschema/eeschema.html), [KiCad schematic file format](https://dev-docs.kicad.org/en/file-formats/sexpr-schematic/), [Altium schematic design review checklist](https://resources.altium.com/p/schematic-review-checklist), and [Texas Instruments AM62P schematic design guidelines](https://www.ti.com/lit/ug/spradn4b/spradn4b.pdf).
