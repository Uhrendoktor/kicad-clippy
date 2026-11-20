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
            "cluster": "U1.VREG_1V2",
            "max_target_distance_mm": 4.0
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
| Connector | interface_standard attribute and at least one expected pin mapping through `connections.pin.N` or `pin_nets.N` |

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

Physical wire groups express multi-terminal physical topology without requiring a shared label or exported netlist. Each configured pin must resolve to an embedded placed-symbol pin, be attached to a wire, and belong to the same continuous physical wire-only component. Matching labels do not join separate components.

    {
      "physical_wire_groups": [
        {
          "name": "SPI_MISO",
          "pins": ["U1.12", "R5.2", "J3.4"]
        }
      ]
    }

The group name is a profile identifier used in diagnostics, not an electrical label. This check verifies only physical wire connectivity; it does not assert the exported logical net name or electrical compatibility. Use `common_pin_groups` when a named label/net contract is also required, and `direct_wire_pairs` for an individual pin pair.

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

For each pin, SCH004 requires a resolved embedded pin coordinate attached to a physical wire and a matching label on that pin's physical wire component. `wire_connected` additionally requires all listed pins to share one wire-only component. `single_label` requires exactly one matching label across the resolved components. SCH004 checks only physical wire topology and label placement; it does not use the exported netlist. Label names never create graph edges, so separately wired islands with matching labels do not satisfy a wire-connected group.

These are opt-in profile contracts. `direct_wire_pairs` require the named pin pair to be attached to and joined by one physical wire-only component; a local or hierarchical label cannot substitute for that path. `common_pin_groups` require each listed pin to have a physical wire attachment and the expected label on its wire component. Set `wire_connected` to require all pins in the group to share one physical wire-only component, and `single_label` to require exactly one matching label across the resolved components. `separate_wire_pairs` assert the opposite physical condition: two resolved pins must not occupy the same wire-only component. This contract does not require either pin to be wired; it only rejects a shared physical wire island. Unsupported embedded pin transforms and pins that cannot be resolved are reported as unresolved rather than guessed.

The logical options are enforced by SCH014, not SCH004. Set `common_pin_groups[].netlist_connected` to additionally require all listed pins to resolve to one exported KiCad net, or set `separate_wire_pairs[].netlist_separate` to require two pins to resolve to different exported nets. These checks catch label-mediated logical joins between separate physical wire islands. If requested through the profile API without an exported netlist, SCH014 emits a warning.

Example: require two endpoints to remain physically and logically separate:

    {
      "schema_version": 1,
      "separate_wire_pairs": [
        { "first": "U1.2", "second": "J1.1", "netlist_separate": true }
      ]
    }

Without `netlist_separate`, the rule only forbids a shared physical wire island; separate islands carrying the same label still satisfy physical separation. Enabling the option also checks the exported KiCad netlist and rejects a label-mediated connection that places both pins on the same logical net.

## Component role vocabulary

| Kind | Common roles | Additional intent worth recording |
| --- | --- | --- |
| Capacitor | decoupling, bulk, filter, timing, coupling, snubber, reservoir, safety | Decouplers identify exact IC pin, capacitor terminal mapping, return net and cluster. Bulk capacitors identify rail, polarity and voltage margin. |
| Resistor | pull_up, pull_down, series, current_limit, current_sense, divider, feedback, termination, gate_bias, discharge, bleeder | Record nodes/pins served, topology, expected range, tolerance, power rating and safety role. |
| Diode | esd, clamp, flyback, reverse_polarity, rectifier, led, series_drop | Record protected node/interface, orientation, clamp target and rating assumptions. |
| Inductor / ferrite | power_filter, emi_filter, energy_storage, common_mode_choke | Record input/output nets, current rating, saturation and impedance/frequency target. |
| Transistor / FET | load_switch, power_switch, level_shift, amplifier, gate_driver | Record gate/base control source, switched path, polarity, body-diode orientation and expected state. |
| Connector | power_input, power_output, data_interface, programming, test, debug | Record interface standard, pin mapping, direction, mating connector and boundary ownership. |
| Test point | test_point | Record the critical net and explicit component pin mapped to the physical probe pad. |
| IC | controller, regulator, driver, sensor, protection, logic | Record datasheet/source, power/ground pins, required support circuitry and exposed interfaces. |

## Implemented checks

- SCH020: every metadata-declared component needs a function and rationale.
- SCH021: strict-mode component-intent coverage, profile references to unknown components, and required connection fields for supported component/role pairs. Connector intent also requires an `interface_standard` attribute and at least one non-empty `connections.pin.N` or `pin_nets.N` mapping.
- SCH014: explicit component pin-to-net contracts against exported KiCad netlist membership, including connector `pin.N` maps.
- SCH022: decoupling metadata completeness, valid `REF.PIN` target notation, distinct capacitor target/return terminals, netlist-backed target and return membership, and consistent return-net/cluster declarations among capacitors serving the same target pin.

SCH021's supported connection requirements are intentionally narrow and explicit: bulk capacitors require `rail` and `return`; pull-up/pull-down resistors require `signal` and `rail`; divider resistors require `high`, `tap`, and `low`; series, current-limit, and current-sense resistors require `input` and `output`; gate-bias resistors require `gate`, `bias`, and `return`; discharge/bleeder resistors require `storage` and `return`; ESD/clamp diodes require `protected` and `return`; flyback diodes require `anode`, `cathode`, `switched_node`, and `return`; reverse-polarity diodes require `input` and `protected_output`; inductors and ferrite beads require `input` and `output`; and transistors require `control`, `switched_high`, and `switched_low`. Other kind/role combinations are not assigned inferred requirements. For connectors, SCH021 checks for a non-empty `interface_standard` and at least one expected pin mapping through `connections.pin.N` or `pin_nets.N`; SCH014 separately checks declared pin mappings against the exported netlist.

SCH022 requires each capacitor declared with role `decoupling` to provide `decoupling.target_pin`, `target_terminal`, `return_terminal`, `return_net`, and `cluster`. The target and return terminals must be different. When a netlist is supplied, the capacitor target terminal must share the target IC pin's net, and the return terminal must resolve to the declared return net. Capacitors assigned to the same target pin must agree on the return net; differing cluster identifiers are reported as a warning. These checks validate explicit declarations and do not infer a decoupling requirement for every IC supply pin.

A decoupling capacitor may additionally set `max_target_distance_mm`. SCH022 measures the Euclidean distance between the transformed target IC pin anchor and transformed capacitor target-terminal anchor in schematic space. Missing or ambiguous pin geometry produces an unverified warning, not an assumed distance. This is a schematic-placement check only: it does not measure PCB pad distance, copper-path length, or return-loop area.

For a board-level pad-center constraint, a decoupling contract may also set `max_target_pad_distance_mm`:

~~~json
{
  "decoupling": {
    "target_pin": "U1.21",
    "target_terminal": "1",
    "return_terminal": "2",
    "return_net": "GND",
    "cluster": "U1.VREG_1V2",
    "max_target_distance_mm": 4.0,
    "max_target_pad_distance_mm": 2.0
  }
}
~~~

Run the schematic lint with both `--profile project-profile.json` and `--board design.kicad_pcb`. PCB001 compares the actual transformed board-space center of the IC target pad with the capacitor target-terminal pad, including footprint rotation and front/back transforms. It verifies that the target IC pad and capacitor target pad use the same board net, that the capacitor return pad uses the declared `return_net`, and—when the profile CLI has exported a schematic netlist—that all three board pad net assignments agree with their schematic pin-to-net assignments. A missing/ambiguous pad is reported as unverified. This is a pad-center distance only—not routed path length, pad-edge clearance, via/plane connectivity, or return-loop analysis.

An IC may declare an opt-in comma-separated `attributes.required_decoupling_pins` list, such as `"1, 2, 7"`. SCH022 requires at least one capacitor decoupling contract targeting every listed `REF.PIN`. When a KiCad netlist is available, each listed IC pin must also exist in that netlist. This list is the project's explicit supply-pin contract; the linter does not guess that every pin with a particular library electrical type requires a capacitor.

The netlist adapter verifies actual target-pin and return-terminal net membership for decoupling capacitors. SCH004 separately checks profile-declared direct-wire pairs and common-pin groups against the physical wire graph built from transformed embedded-symbol pin coordinates. The graph never uses matching label names as connectivity edges. Automatic inference of every physical connection, physical proximity constraints, and shared-copper geometry analysis remain pending.

## Functional group documentation

Profiles can also declare functional groups. Each group identifies its component members, the members requiring documentation, the UUID of its rendered description text box, and evidence. It can also set `kicad_group_uuid` to bind the profile contract to a native KiCad group object, `frame_uuid` to reference a drawn schematic rectangle that encloses group members, `minimum_frame_clearance_mm` to require an inset from the frame edges, `minimum_member_origin_spacing_mm` to require Euclidean separation between member origins, `minimum_member_body_clearance_mm` to require a minimum edge-to-edge separation between transformed member outlines, `maximum_member_body_gap_mm` to bound the nearest-edge gap for every member pair and keep the group compact, `alignment_axis` with `alignment_tolerance_mm` to keep member origins in a horizontal row or vertical column, and `wire_length_budget_mm` to cap the total length of wire segments wholly contained by the frame. For each documented component, the function and rationale phrases must appear in that text box.

    {
      "groups": {
        "vbus-monitor": {
          "members": ["R1", "R4"],
          "documentation_members": ["R1", "R4"],
          "description_uuid": "00000000-0000-0000-0000-000000000000",
          "kicad_group_uuid": "11111111-1111-4111-8111-111111111111",
          "frame_uuid": "22222222-2222-4222-8222-222222222222",
          "minimum_member_origin_spacing_mm": 10.0,
          "minimum_member_body_clearance_mm": 1.5,
          "maximum_member_body_gap_mm": 8.0,
          "alignment_axis": "horizontal",
          "alignment_tolerance_mm": 0.5,
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

When `minimum_member_origin_spacing_mm` is configured, SCH006 compares every member-origin pair. When `minimum_member_body_clearance_mm` is configured, it compares selected-unit transformed body outlines, including each primitive's stroke half-width. Supported bodies include closed rectangles/polygons, circles, and open line paths. Missing outlines and bodies containing unsupported arc/Bézier curves produce an unverified warning rather than a guessed distance. The minimum body clearance must be finite and non-negative.

When `maximum_member_body_gap_mm` is set, SCH006 checks the nearest edge-to-edge distance between every pair of supported member bodies and reports an error when any pair exceeds the configured maximum. This is the cluster-compactness contract; it does not require the bodies to be separated, so use `minimum_member_body_clearance_mm` separately when a minimum gap is also required. The maximum must be finite and non-negative. Missing or curve-based member outlines produce an unverified warning rather than an inferred distance.

Set `alignment_axis` to `horizontal` to keep member-origin Y coordinates within `alignment_tolerance_mm`, or `vertical` to keep X coordinates within that tolerance. The axis and tolerance must be supplied together; the tolerance must be finite and non-negative.

A group is valid when it cites a document/page/figure or a reviewed screenshot asset, its members exist in the schematic, its description text box UUID resolves, and all component-purpose phrases are present. When `kicad_group_uuid` is set, SCH007 also resolves the native KiCad group and verifies that its placed-symbol members match the profile's component references in both directions. The native group check is opt-in so existing version-1 profiles without this field remain valid. When `frame_uuid` is set, SCH009 resolves the top-level KiCad rectangle with that UUID and checks each declared member's symbol origin, resolved embedded pin coordinates, estimated visible property-field glyph rectangles, description text-box bounds, selected-unit embedded symbol primitives, physical wire segments attached to member pins, visible glyph bounds for labels on those same wire components, and other text-box annotations that overlap the frame. Arc and Bézier primitive paths use the existing 0.01 mm flattened approximation with stroke half-width margin; label text bounds use estimated NewStroke glyph rectangles. Labels never create physical wire edges. Member-pin-connected wire islands are treated as group wiring, and any other wire segment that intersects the frame is also checked. Labels whose anchors/glyphs overlap the frame and top-level plain-text glyph bounds overlapping the frame are checked even when they are not attached to member pins; labels never create physical wire edges. Free-standing labels/wires outside the frame, unsupported raster image encodings, other non-text-box annotation types, and unresolvable symbol transforms remain outside the rule's coverage. Top-level vector graphics and embedded PNG/JPEG images are checked when their geometry can be resolved; bitmap bounds use the stored scale, image pixel dimensions and PPI/file-version compatibility rules. Finite non-cardinal rotations are applied to embedded pin and primitive coordinates; SCH005 still skips non-cardinal pin-axis checks. When `minimum_frame_clearance_mm` is set, SCH012 applies the configured inset to these same object families as well as member origins, resolved pins, estimated property glyphs and the group description text box. SCH006 separately checks minimum pairwise origin spacing. SCH016 uses the same frame to sum the lengths of wire segments whose two endpoints both lie inside it, then divides by `wire_length_budget_mm`. A score above 1.0 emits a warning with the actual total, budget, and normalized score. Segments crossing the frame edge are excluded; the budget must be finite and positive. This ports the original SCH017/SCH020 group-documentation intent while adding native grouping and frame-origin contracts.

## Component field-to-body spacing

A component can opt into maximum gaps for its visible Reference and/or Value fields by setting `max_field_body_gap_mm`:

~~~json
{
  "components": {
    "U1": {
      "kind": "integrated_circuit",
      "role": "controller",
      "function": "Control logic",
      "rationale": "Owns the system control function",
      "max_field_body_gap_mm": {
        "Reference": 4.0,
        "Value": 6.0
      }
    }
  }
}
~~~

Each value is a finite, non-negative distance in millimetres. SCH010 measures from the estimated visible glyph rectangle to the component's own selected-unit transformed body geometry; a glyph rectangle that intersects or lies inside the body has zero gap. The map is per component and per field, so a deliberately long Value field can have a different allowance from its Reference field. Only `Reference` and `Value` are supported. Missing body geometry or a body containing arc/Bézier primitives is reported as unverified; the rule does not estimate a curve outline from its control points. Glyph rectangles remain NewStroke-based estimates, not exact KiCad renderer bounds.

## Netlist-backed decoupling verification

When a project profile is supplied, kicad-clippy exports a flattened netlist through KiCad CLI and checks:

1. The capacitor target terminal is on the same exported net as the declared target IC pin.
2. The capacitor return terminal is on the declared return net.
3. Decouplers for one target pin agree on their return net and cluster identifier.

This proves logical pin-to-net membership. It does not yet prove that the connection is a direct wire rather than a label-mediated connection, nor does it prove physical proximity; those require wire-island and transformed symbol-pin geometry checks.

## Profile-declared required no-connect pins

A component may declare pins that are intentionally unused in the current design by adding a comma-separated `required_no_connect_pins` entry to its `attributes` map:

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

SCH015 resolves those pin numbers through the placed symbol's embedded library geometry and requires a no-connect flag at each transformed pin coordinate. It also resolves the effective electrical type from the selected library pin alternate when the placed instance overrides the base pin. Do not list pins whose effective library electrical type is `unconnected` or `no_connect`: those types already denote a non-connectable pin and are distinct from a schematic no-connect flag. If pin geometry cannot be resolved, the profile requirement is reported as unverified. Independently of any profile, SCH015 also scans every resolved placed pin and reports an explicit marker on a pin whose effective type is `unconnected` or `no_connect`. KiCad native ERC remains authoritative for the electrical meaning of both.

## Complete connector pin maps

A connector can declare its full expected pin set with the comma-separated `attributes.required_pins` field:

~~~json
{
  "components": {
    "J1": {
      "kind": "connector",
      "role": "power_input",
      "function": "External supply input",
      "rationale": "Connects the external supply to the board",
      "connections": {
        "pin.1": "VBUS",
        "pin.2": "GND",
        "pin.3": "SHIELD"
      },
      "attributes": {
        "interface_standard": "USB-C",
        "required_pins": "1, 2, 3"
      }
    }
  }
}
~~~

SCH021 verifies that every listed pin has an expected net mapping in `connections.pin.N` or `pin_nets.N`. SCH014 independently verifies those mappings against the exported KiCad netlist. This check is opt-in because unpopulated, reserved, shield, and intentionally unused connector contacts differ by interface and product variant.


## Required sheet title-block fields

A project may require selected native KiCad title-block fields by adding `required_title_block_fields` at the profile root:

~~~json
{
  "schema_version": 1,
  "required_title_block_fields": ["title", "rev", "company", "comment1"]
}
~~~

SCH023 supports `title`, `date`, `rev`, `company`, and `comment1` through `comment9`. Only the fields named by the profile are enforced; each must exist in the current sheet's `title_block` and contain non-whitespace text. This keeps the rule opt-in for projects that use their own title-block conventions. Sheet number/count and project-level metadata are not inferred by SCH023.

SCH024 reports duplicate UUID values across a path-based root schematic and each distinct child `.kicad_sch` file referenced by its `Sheetfile` properties. Relative paths resolve from the owning schematic file; absolute paths are followed as written. Canonical paths prevent cycles and repeated file loads, so a reused child file is counted once even when multiple sheet instances reference it. Missing or unreadable children produce an unverified warning rather than claiming complete hierarchy coverage. In-memory documents without an existing source path retain single-document behavior. Project-variable expansion in `Sheetfile` values is not performed by the linter; unresolved paths are reported as unverified.


## Required test-point coverage

A project can list critical nets that must have an intentional test point:

~~~json
{
  "schema_version": 1,
  "required_test_point_nets": ["VDD", "RESET", "SWDIO"],
  "components": {
    "TP1": {
      "kind": "test_point",
      "role": "test_point",
      "function": "Main logic-rail measurement access",
      "rationale": "Allows production verification of the 3.3 V rail",
      "pin_nets": {"1": "VDD"}
    },
    "TP2": {
      "kind": "test_point",
      "role": "test_point",
      "function": "Reset-line programming access",
      "rationale": "Supports reset-state and programming checks",
      "pin_nets": {"1": "RESET"}
    },
    "TP3": {
      "kind": "test_point",
      "role": "test_point",
      "function": "Debug data measurement access",
      "rationale": "Supports SWD programming and debug verification",
      "pin_nets": {"1": "SWDIO"}
    }
  }
}
~~~

SCH025 requires every name in `required_test_point_nets` to map to at least one declared component with kind and role `test_point`. Each such component uses `pin_nets` to identify the exact footprint/symbol pin. When the CLI exports a KiCad netlist, SCH025 verifies that the declared pin is actually assigned to the required net; profile-only validation without a netlist is reported as unverified.

PCB009 requires `--board design.kicad_pcb` and verifies that each declared reference/pad exists on the board, has the expected net, and has a pad layer list containing both copper and solder mask (including wildcard through-hole layer sets). This is a serialized layer-exposure check. It does not model the physical probe tip, surrounding pad clearance, fixture geometry, enclosure constraints, or whether a manufacturing fixture can reach the pad.


## Project grid alignment

A project may declare the schematic grid used for placement checks at the profile root:

~~~json
{
  "schema_version": 1,
  "grid_alignment": {
    "spacing_mm": 2.54,
    "origin_mm": [0.0, 0.0],
    "tolerance_mm": 0.01,
    "targets": ["symbol_origins", "label_anchors"],
    "exempt_components": ["U3"],
    "exempt_pins": ["J1.7"],
    "exempt_labels": ["global:TEST"],
    "exempt_coordinates_mm": [[25.4, 17.78]]
  }
}
~~~

SCH026 checks each selected coordinate against the nearest grid intersection independently on the X and Y axes. The maximum allowed deviation on either axis is `tolerance_mm`. The grid origin defaults to `[0, 0]`; the tolerance defaults to 0.001 mm. When `targets` is omitted, the rule checks placed symbol origins and label anchors only. Optional targets are `pin_anchors`, `junctions`, and `wire_endpoints`; these electrical geometry checks are not enabled by default because KiCad native ERC already reports some off-grid pin and wire conditions.

Exemptions are explicit and scoped: `exempt_components` contains symbol references, `exempt_pins` contains `REF.PIN` identifiers, `exempt_labels` contains `kind:name` selectors, and `exempt_coordinates_mm` contains exact sheet-space anchors. Exemptions do not disable checks for other objects. SCH026 reports the actual off-grid anchor and its nearest grid point. It does not read or modify the KiCad project grid setting; spacing and origin are project-profile values.


## Label scope policy

A project can explicitly constrain global labels and require selected label names to use a particular scope on the currently loaded sheet:

~~~json
{
  "schema_version": 1,
  "label_scope_policy": {
    "allowed_global_labels": ["GND", "VBUS"],
    "forbidden_global_labels": ["DEBUG_INTERNAL"],
    "required_label_scopes": {
      "RESET": "hierarchical",
      "SDA": "hierarchical",
      "VDD_SENSE": "local"
    }
  }
}
~~~

SCH027 treats an omitted `allowed_global_labels` as no allow-list restriction. An explicitly empty list permits no global labels. `forbidden_global_labels` always rejects the named global labels. `required_label_scopes` maps a label name to `local`, `global`, or `hierarchical`; every matching instance on the loaded sheet must use the requested scope, and a required label that is absent from that sheet is reported.

The rule checks the serialized label objects in the current `.kicad_sch` file only. It does not confirm that a hierarchical label is connected to a matching parent-sheet pin, aggregate global labels across other loaded project sheets, or reimplement KiCad's native label/net resolution. Native ERC and KiCad's exported netlist remain authoritative for actual connectivity.


## Functional-frame title tabs and spacing

A group can opt into a frame title tab by linking its UUID-backed objects:

~~~json
{
  "schema_version": 1,
  "groups": {
    "power-input": {
      "description_uuid": "group-description",
      "frame_uuid": "group-frame",
      "frame_title_tab_uuid": "group-title-tab",
      "frame_title_text_box_uuid": "group-title-text",
      "minimum_frame_gap_mm": 2.0
    }
  }
}
~~~

SCH028 verifies that the referenced frame and tab are unique rectangular objects, that the tab shares a non-zero straight edge segment with the frame, and that the referenced title text-box rectangle is contained by the tab. The title-tab and title-text-box UUIDs must be supplied together, and a tab requires frame_uuid. minimum_frame_gap_mm is optional and checks the Euclidean edge-to-edge distance between distinct group frames; when two groups declare different thresholds, the larger threshold applies. A group that intentionally shares the same frame UUID with another group is not treated as a separate frame.

The text-box check uses its serialized rectangle. It does not rasterize the final font or prove glyph-level clipping, nor does it infer group frames from unrelated rectangles. Unsupported or unresolved geometry is reported as unverified.
