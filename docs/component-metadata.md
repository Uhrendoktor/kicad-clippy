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

- connections: named electrical terminals/nodes mapped to a net name or a reference/pin such as U1.21. Connector keys named `pin.N` also declare the expected net for connector pin N.
- pin_nets: explicit component pin number to expected KiCad net name mappings for any component family.
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

These contracts make metadata extensible by component family and variant. SCH021 validates role-specific declaration completeness. SCH014 verifies each `pin_nets` and connector `pin.N` mapping against the exported KiCad netlist, reporting mismatched nets and pins absent from the export. A profile supplied to the CLI triggers netlist export; calling the profile API without a netlist produces a warning when pin-to-net contracts are present.

## Physical direct-wire contracts

SCH014 checks logical net membership in KiCad's exported netlist. Some design requirements are stricter: two pins must be joined by visible wire segments, and a label must not be used as a substitute for that physical connection. Profiles can declare these pairs separately:

    {
      "schema_version": 1,
      "direct_wire_pairs": [
        { "first": "U1.21", "second": "C1.1" },
        { "first": "T4.3", "second": "T1.2" },
        { "first": "T1.2", "second": "R10.1" }
      ],
      "forbidden_direct_wire_labels": ["FET_MID"]
    }

SCH004 resolves each `REF.PIN` against the embedded symbol definition and its placed transform, verifies that both pin anchors lie on wire segments, then checks that both pins belong to the same physical wire component. Wire segments are split at endpoints, pin coordinates, explicit junctions and label anchors; only wire segments create connectivity edges. An endpoint that touches the interior of another wire does not join that wire unless an explicit junction marker is present, consistent with SCH001. Matching local or hierarchical label names do not join separate islands. Labels anchored directly on a declared direct-wire pin are reported, and configured forbidden names are checked only on the physical wire components covered by those contracts.

Common-pin groups can express the complementary case where several pins must carry one named net:

    {
      "common_pin_groups": [
        {
          "name": "VBUS",
          "pins": ["J1.1", "U1.4", "R8.1"],
          "wire_connected": true,
          "netlist_connected": true,
          "single_label": true
        }
      ]
    }

For each pin, SCH004 requires a resolved embedded pin coordinate attached to a physical wire and a matching label on that pin's physical wire component. `wire_connected` additionally requires all listed pins to share one wire-only component. `single_label` requires exactly one matching label across the resolved components. Set `netlist_connected` to additionally require all listed pins to resolve to the same net in the KiCad-exported netlist; this is separate from `wire_connected`, which checks physical wire-only topology. When `netlist_connected` is requested through the profile API without a netlist, SCH004 emits a warning that the logical check was not performed. Label names never create graph edges, so separately wired islands with matching labels do not satisfy a wire-connected group.

These are opt-in profile contracts. `direct_wire_pairs` require the named pin pair to be attached to and joined by one physical wire-only component; a local or hierarchical label cannot substitute for that path. `common_pin_groups` require each listed pin to have a physical wire attachment and the expected label on its wire component. Set `wire_connected` to require all pins in the group to share one physical wire-only component, and `single_label` to require exactly one matching label across the resolved components. These checks do not infer all intended connections automatically or replace KiCad ERC. Unsupported embedded pin transforms and pins that cannot be resolved are reported as unresolved rather than guessed.

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
- SCH014: explicit component pin-to-net contracts against exported KiCad netlist membership, including connector `pin.N` maps.
- SCH022: decoupling metadata completeness, valid target-pin notation, distinct capacitor terminals, and consistent return net and cluster for capacitors assigned to the same IC pin.

The netlist adapter verifies actual target-pin and return-terminal net membership for decoupling capacitors. SCH004 separately checks profile-declared direct-wire pairs and common-pin groups against the physical wire graph built from transformed embedded-symbol pin coordinates. The graph never uses matching label names as connectivity edges. Automatic inference of every physical connection, physical proximity constraints, and shared-copper geometry analysis remain pending.

## Functional group documentation

Profiles can also declare functional groups. Each group identifies its component members, the members requiring documentation, the UUID of its rendered description text box, and evidence. It can also set `kicad_group_uuid` to bind the profile contract to a native KiCad group object, `frame_uuid` to reference a drawn schematic rectangle that encloses group members, `minimum_frame_clearance_mm` to require a minimum inset from the frame edges for each member symbol origin and the group description text box, and `minimum_member_origin_spacing_mm` to declare a minimum Euclidean distance between member symbol origins, and `wire_length_budget_mm` to cap the total length of wire segments wholly contained by that frame. For each documented component, the function and rationale phrases must appear in that text box.

    {
      "groups": {
        "vbus-monitor": {
          "members": ["R1", "R4"],
          "documentation_members": ["R1", "R4"],
          "description_uuid": "00000000-0000-0000-0000-000000000000",
          "kicad_group_uuid": "11111111-1111-4111-8111-111111111111",
          "frame_uuid": "22222222-2222-4222-8222-222222222222",
          "wire_length_budget_mm": 95.0,
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

A group is valid when it cites a document/page/figure or a reviewed screenshot asset, its members exist in the schematic, its description text box UUID resolves, and all component-purpose phrases are present. When `kicad_group_uuid` is set, SCH007 also resolves the native KiCad group and verifies that its placed-symbol members match the profile's component references in both directions. The native group check is opt-in so existing version-1 profiles without this field remain valid. When `frame_uuid` is set, SCH009 resolves the top-level KiCad rectangle with that UUID and checks that each declared member's symbol origin is inside the frame. This does not yet test the symbol body, pins, fields, labels, wires or notes against the rectangle edges. When `minimum_frame_clearance_mm` is set, SCH012 checks the inset of member symbol origins, resolved embedded pin coordinates, estimated visible symbol-property glyph bounds and the description text-box bounds from the rectangle edges; it does not yet check every group object. SCH006 separately checks minimum pairwise origin spacing. SCH016 uses the same frame to sum the lengths of wire segments whose two endpoints both lie inside it, then divides by `wire_length_budget_mm`. A score above 1.0 emits a warning with the actual total, budget, and normalized score. Segments crossing the frame edge are excluded; the budget must be finite and positive. This ports the original SCH017/SCH020 group-documentation intent while adding native grouping and frame-origin contracts.

## Netlist-backed decoupling verification

When a project profile is supplied, kicad-clippy exports a flattened netlist through KiCad CLI and checks:

1. The capacitor target terminal is on the same exported net as the declared target IC pin.
2. The capacitor return terminal is on the declared return net.
3. Decouplers for one target pin agree on their return net and cluster identifier.

This proves logical pin-to-net membership. It does not yet prove that the connection is a direct wire rather than a label-mediated connection, nor does it prove physical proximity; those require wire-island and transformed symbol-pin geometry checks.
