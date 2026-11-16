# Component intent metadata

A schematic's geometry and native netlist describe what is drawn and connected. They do not explain why a component exists or which pin it is intended to serve. Project profiles add that missing design intent.

Profiles are versioned JSON documents passed with --profile. The initial schema is version 1.

## Example: decoupling capacitor

    {
      "schema_version": 1,
      "mode": "strict",
      "components": {
        "C1": {
          "kind": "capacitor",
          "role": "decoupling",
          "function": "Local high-frequency bypass for U1 VREG_1V2",
          "rationale": "Minimize the supply-pin current loop and local return impedance",
          "evidence": "Controller datasheet, power-supply recommendations",
          "decoupling": {
            "target_pin": "U1.21",
            "target_terminal": "1",
            "return_terminal": "2",
            "return_net": "GND",
            "cluster": "U1.VREG_1V2"
          }
        }
      },
      "ignore_components": ["J1"]
    }

For a decoupler, metadata distinguishes the IC pin from the capacitor terminal: target_pin is the IC reference/pin (for example U1.21), while target_terminal and return_terminal identify the capacitor's own pins. This prevents confusing a net name with a component pin.

Strict mode requires metadata for every placed component except references explicitly listed in ignore_components. Metadata for references absent from the schematic is an error. Every declared component must include a useful function and rationale.

## Role-specific connection contracts

Besides the decoupling block, every component intent can carry two extensible maps:

- connections: named electrical terminals/nodes mapped to a net name or a reference/pin such as U1.21.
- attributes: role-specific facts such as interface_standard, orientation, expected_state, or a protected interface.

The first required contracts are:

| Component role | Required connection keys |
| --- | --- |
| Bulk capacitor | rail, return |
| Resistor pull_up / pull_down | signal, rail |
| Resistor divider | high, tap, low |
| Resistor series / current_limit / current_sense | input, output |
| Resistor gate_bias | gate, bias, return |
| Resistor discharge / bleeder | storage, return |
| Diode esd / clamp | protected, return |
| Diode flyback | anode, cathode, switched_node, return |
| Diode reverse_polarity | input, protected_output |
| Inductor / ferrite bead | input, output |
| Transistor / FET | control, switched_high, switched_low |
| Connector | interface_standard attribute and at least one pin.N mapping |

Example resistor metadata:

    {
      "kind": "resistor",
      "role": "pull_up",
      "function": "Pull SDA to the 3.3 V logic rail",
      "rationale": "I2C uses open-drain signalling",
      "connections": {
        "signal": "SDA",
        "rail": "3V3"
      },
      "attributes": {
        "nominal_value": "4.7k",
        "tolerance": "1%"
      }
    }

These contracts make the metadata extensible by component family and variant. They validate declared intent completeness; proving the actual pin-to-net mapping still requires the exported KiCad netlist.

## Component role vocabulary

| Kind | Common roles | Additional intent worth recording |
| --- | --- | --- |
| Capacitor | decoupling, bulk, filter, timing, coupling, snubber, reservoir, safety | Decouplers identify exact IC pin, capacitor terminal mapping, return net and cluster. Bulk capacitors identify rail, polarity and voltage margin. |
| Resistor | pull_up, pull_down, series, current_limit, current_sense, divider, feedback, termination, gate_bias, discharge, bleeder | Record nodes/pins served, topology, expected range, tolerance, power rating and safety role. |
| Diode | esd, clamp, flyback, reverse_polarity, rectifier, led, series_drop | Record protected node/interface, orientation, clamp target and rating assumptions. |
| Inductor / ferrite | power_filter, emi_filter, energy_storage, common_mode_choke | Record input/output nets, current rating, saturation and impedance/frequency target. |
| Transistor / FET | load_switch, power_switch, level_shift, amplifier, gate_driver | Record gate/base control source, switched path, polarity, body-diode orientation and expected state. |
| Connector | power_input, power_output, data_interface, programming, test, debug | Record interface standard, pin mapping, direction, mating connector and boundary ownership. |
| IC | controller, regulator, driver, sensor, protection, logic | Record datasheet/source, power/ground pins, required support circuitry and exposed interfaces. |

## Implemented checks

- SCH020: every metadata-declared component needs a function and rationale.
- SCH021: strict-mode completeness, unknown component references and capacitor role sanity.
- SCH022: decoupling metadata completeness, valid target-pin notation, distinct capacitor terminals, and consistent return net and cluster for capacitors assigned to the same IC pin.

The first metadata implementation validates intent declarations. It does not yet prove physical connectivity, pin-to-net membership, proximity or shared copper. Those checks must use the KiCad-exported netlist plus physical wire graph and transformed symbol-pin geometry; matching labels or metadata strings alone are not evidence of a physical connection.

## Functional group documentation

Profiles can also declare functional groups. Each group identifies its component members, the members requiring documentation, the UUID of its rendered description text box, and evidence. For each documented component, the function and rationale phrases must appear in that text box.

    {
      "groups": {
        "vbus-monitor": {
          "members": ["R1", "R4"],
          "documentation_members": ["R1", "R4"],
          "description_uuid": "00000000-0000-0000-0000-000000000000",
          "component_purposes": {
            "R1": {
              "function": "Series current limiter at VBUS_VS_DISCH",
              "rationale": "Limits current through the controller during discharge"
            }
          },
          "evidence": {
            "document": "Controller datasheet revision 8",
            "page": 26,
            "figure": "Figure 10"
          }
        }
      }
    }

A group is valid when it cites a document/page/figure or a reviewed screenshot asset, its members exist in the schematic, its description text box UUID resolves, and all component-purpose phrases are present. This ports the original SCH017/SCH020 group-documentation intent while keeping component intent reusable for other checks.

## Netlist-backed decoupling verification

When a project profile is supplied, kicad-clippy exports a flattened netlist through KiCad CLI and checks:

1. The capacitor target terminal is on the same exported net as the declared target IC pin.
2. The capacitor return terminal is on the declared return net.
3. Decouplers for one target pin agree on their return net and cluster identifier.

This proves logical pin-to-net membership. It does not yet prove that the connection is a direct wire rather than a label-mediated connection, nor does it prove physical proximity; those require wire-island and transformed symbol-pin geometry checks.
