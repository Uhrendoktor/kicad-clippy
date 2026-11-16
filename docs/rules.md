# Built-in schematic rules

The built-in catalogue preserves the historical IDs SCH001–SCH020. New IDs are appended and are not reused. Native KiCad ERC findings remain separate under `KICAD/ERC/<type>`.

## Generic rules

These rules run without a project profile.

| ID | Rule | Default | Purpose |
| --- | --- | --- | --- |
| SCH001 | Wire geometry | deny | No zero-length, diagonal, overlapping, crossing or ambiguously branched wire segments. |
| SCH002 | Label topology | deny | Local labels connect distinct physical wire islands; hierarchical labels terminate one wire twig. |
| SCH003 | Label/wire clearance | deny | Wires do not cross visible label glyphs. |
| SCH008 | Annotation layout | deny | Native text boxes, uniform padding, sufficient size, no overlap and no wire penetration. |
| SCH010 | Symbol-field clearance | deny | Visible fields clear symbol geometry, pins, wires, labels and annotations. Values may be inside passive bodies. |
| SCH011 | Block annotation structure | deny | Multiline notes require visible outlines. |
| SCH013 | Wire obstacle clearance | deny | Wires do not cross embedded symbol outlines, intermediate pins, fields or text boxes. |
| SCH018 | Label text collision | deny | Label glyphs do not overlap other labels, symbol fields or text boxes. |
| SCH019 | Label-kind consistency | deny | One net name uses one label type: local, hierarchical or global. |
| SCH022 | Unique object UUIDs | deny | UUIDs in the schematic are unique. |
| SCH023 | Title-block completeness | warn | A native title block and non-empty title are present; a profile may require more fields. |

## Profile rules

These rules run when a design profile is supplied and the selected preset is not `generic`.

| ID | Rule | Default | Profile data |
| --- | --- | --- | --- |
| SCH004 | Physical-net connectivity | deny | Pin pairs that must share a physical wire network without labels replacing that connection. |
| SCH005 | Pin orientation | deny | Ground pins that must face down and orientation of downward hierarchical GND ports. |
| SCH006 | Component spacing | warn | Component adjacency/alignment and selected property-to-symbol distances. |
| SCH007 | Group metadata | deny | Functional group title, description and frame. |
| SCH009 | Group containment | deny | Declared component membership, pin/field/text containment, frame overlap, primary spacing and approved DNP population. |
| SCH012 | Frame content clearance | deny | Minimum label and note inset from functional-frame borders. |
| SCH014 | Shared pin-net contracts | deny | Logical net membership, physical connection and expected label count. |
| SCH015 | Required no-connects | deny | Explicit no-connect markers at declared intentionally unused pins. |
| SCH016 | Wire compactness | warn | Per-group sum of internal wire lengths compared with a configured budget. |
| SCH017 | Circuit documentation | deny | Source evidence and identification of documented group members. |
| SCH020 | Component-purpose documentation | deny | A documented component's electrical function and rationale appear in its group note. |
| SCH021 | Symbol-grid alignment | warn | Symbol origins align to the profile grid; this does not replace KiCad ERC's pin/wire grid checks. |

## Component-intent rules

These rules use typed profile metadata.

| ID | Rule | Default | Purpose |
| --- | --- | --- | --- |
| SCH024 | Component intent metadata | warn | Validate component kind/role compatibility and non-empty function/rationale. In strict mode, every detected capacitor must have a metadata record. Decoupling capacitors must declare target pin and connection/return terminals. |
| SCH025 | Decoupling network contracts | deny | Verify target pin physical connectivity, terminal-to-target proximity, adjacency among decouplers for the same pin, and a shared physical return/GND island. |

### Typed component metadata

A metadata record contains:

- `reference`, `kind`, and typed `role`.
- A concise `function` describing what the part does.
- A `rationale` explaining why the part is present.
- Optional terminal/relationship fields for the checks appropriate to that role: `associated_pin`, `connection_pin`, `return_pin`, `return_net`, `cluster`, `related_components`, `from_net`, `to_net`, and `protected_nets`.

Supported kinds are capacitor, resistor, diode, inductor, transistor, integrated circuit, connector, test point and other. The role enum contains common roles such as decoupling, bulk, filter, pull-up, pull-down, series current limit, discharge, gate bias, clamp, ESD protection, flyback, high-side/low-side switch and back-to-back switch.

For a decoupling capacitor, pin references have the form `REFERENCE.NUMBER` and terminal numbers refer to the embedded KiCad library symbol pins, not package pad numbers. Example:

```json
{
  "reference": "C2",
  "kind": "capacitor",
  "role": "decoupling",
  "function": "VREG_2V7 regulator decoupling",
  "rationale": "provides local high-frequency bypass at the controller supply pin",
  "associated_pin": "U1.23",
  "connection_pin": "1",
  "return_pin": "2",
  "return_net": "GND",
  "cluster": "U1.23"
}
```

Profile-level `decoupling_max_pin_distance_mm` sets the maximum distance from the target IC pin to the capacitor terminal; a record-level `max_distance_mm` can override it. `decoupling_max_cluster_spacing_mm` sets the maximum distance between capacitor symbol origins in the same target-pin cluster.

The checker verifies physical wire connectivity. Same-name labels do not bridge disconnected schematic wire islands. Return pins in one cluster must share one physical island, and that island must carry the declared return-net label.

### Presets and overrides

- `generic`: generic rules only; skips project profile and metadata checks.
- `recommended`: built-in defaults. SCH024 warns about invalid metadata, but does not require every capacitor to be annotated unless the profile sets `require_component_metadata_for`.
- `strict`: promotes warning rules to errors and requires metadata for every detected capacitor by default.
- Explicit per-rule overrides in `rules` take precedence over the preset. Set `SCH024: "allow"` to disable the metadata rule or `SCH024: "deny"` to require it in recommended mode.

Use the example profile:

```sh
kicad-clippy lint path/to/design.kicad_sch --config examples/stusb4500/kicad-clippy.json
```

List metadata:

```sh
kicad-clippy rules
kicad-clippy rules --format json
```

## Engineering boundaries

These are schematic-level checks. A short schematic distance is not PCB placement verification, and a shared GND wire island does not prove a good PCB return-current path. Capacitor effective capacitance under DC bias, voltage derating, ESR/ESL, loop inductance, current capability and PDN impedance require part- and board-level analysis.

KiCad ERC remains authoritative for pin-type conflicts, undriven power inputs, unconnected pins and other native electrical conditions. Custom metadata rules are intended to express design intent ERC cannot infer. See [the KiCad ERC reference](https://docs.kicad.org/10.0/en/eeschema/eeschema.html).
