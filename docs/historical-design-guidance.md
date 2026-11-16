# Historical schematic design guidance

This document consolidates engineering constraints and review feedback recorded during the original schematic-linter and STUSB4500 reference development, including kicad-blocks PRs #20 and #49.

## Layout and presentation invariants

- Organize the page as a clear left-to-right functional flow.
- Use compact functional frames with attached title tabs; keep consistent padding and place intended member components, fields, pins, labels, wires and notes inside the frame.
- Keep local wiring short and orthogonal. Use named hierarchical ports for genuine block boundaries instead of long cross-module wire loops.
- Keep ordinary internal nets local. Do not promote all common net names to global labels.
- Put label text perpendicular to a pin/wire corridor when along-wire placement would collide with pin names or visible wire paths.
- Keep reference/value fields near their actual symbol bodies, without straddling bodies, pins, wires, labels or note boxes.
- Use native KiCad title-block fields rather than drawing a second oversized sheet-title banner.
- Use compact, separately titled subgroups for distinct functional stages such as clamp/filter and sense-divider circuits.
- Component documentation must describe electrical function and design rationale, not merely repeat the component group name.

## Electrical and topology invariants

- A net name is not proof of physical wire continuity. Checks requiring physical connections must inspect wire segments and pin endpoints; native exported netlists are the authority for final pin-to-net topology.
- Do not infer symbol pin locations from component origins or rectangular bounding boxes. Use embedded symbol pin definitions and apply symbol transforms.
- A wire crossing or touching a symbol body, pin, visible property, label glyph or annotation box is a geometric obstacle unless it terminates at the intended pin.
- A shared node that matters to the design should be visibly wired, not represented only by repeated detached labels.
- Preserve intended electrical boundaries between hierarchical sheets. The STUSB4500 review explicitly required U1.22 to remain VSYS, distinct from GND.
- Keep unused pins explicitly marked with no-connects when required by the design contract; do not add a redundant marker to a library pin already hidden/NC.
- Validate any geometry/topology-changing fix with a KiCad-exported netlist. A Python geometry test passed while a rail relocation physically shorted FET_MID to VBUS; this is a known regression class.

## Component-intent metadata

Component metadata should identify family and functional role. Role-specific contracts should be machine-checkable:

- Decoupling capacitors: target IC reference/pin, capacitor terminal connected to that pin, return terminal/net, cluster identifier, and optional capacitance/voltage constraints.
- Bulk capacitors: rail served, polarity, minimum voltage margin, and placement region.
- Resistors: pull-up/down target, divider endpoints, current-sense nodes, series-protection interface, discharge path, gate-bias network, value/tolerance/power requirements.
- Diodes: protected interface/net, cathode/anode intent, clamp/flyback/reverse-polarity role, and rating assumptions.
- Inductors/ferrites: filtered nets, current/saturation rating, impedance target and frequency band.
- Transistors/FETs: gate/base control, switched terminals, expected polarity, body-diode direction and default state.
- Connectors: interface standard, pin mapping, direction, mating connector and sheet-boundary ownership.
- ICs: datasheet evidence, power/ground pins, required support circuitry and exposed interfaces.

## Additional candidate rules to investigate

These are candidates, not silently enabled universal requirements:

- Duplicate UUIDs across the complete hierarchical schematic.
- Configurable grid alignment of symbol origins, pins, labels, junctions and wire endpoints.
- Title-block completeness and project metadata consistency.
- Required decoupling coverage per IC power pin, with value/rating constraints from component intent.
- Decoupler placement radius and same-pin cluster compactness, measured from transformed pin coordinates.
- Explicit polarity and voltage-rating checks for polarized capacitors and protection devices.
- Connector pin-map completeness and interface-boundary checks.
- Test-point coverage for important power, reset, programming and measurement nets.
- Functional-group completeness and evidence/source traceability.
- Detect local net labels that accidentally substitute for a physical local connection where visible wire is required.
- Require I²C/open-drain pull-ups on nets whose declared interface role requires them.
- Require a current-limiting path for LED indicators, with the resistor path and expected current documented.
- Check for a DC bias/return path on declared analog nodes that otherwise contain only capacitors and high-impedance/connector terminals.
- Check declared power-input protection paths (fuse, reverse-polarity protection, TVS/clamp) as a project contract, not as a universal rule.
- Verify that declared IC supply pins have decoupling metadata and that each required supply domain has at least one decoupler.

Introduce candidates with configurable profiles and positive/negative fixtures. Avoid duplicating native ERC checks such as pin-type conflicts or unconnected pins.
