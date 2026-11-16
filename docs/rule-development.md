# Rule development

## Built-in rule catalogue

The built-in rule implementation lives in `kicad-clippy-rules`. The shared rule descriptors are exposed through `kicad-clippy-core`.

List all registered rules:

```sh
kicad-clippy rules
kicad-clippy rules --format json
```

The catalogue keeps stable IDs SCH001–SCH020 from the original Python linter and adds SCH021–SCH025. Rule IDs are public identifiers and must not be recycled. Native KiCad ERC findings use their separate `KICAD/ERC/<type>` namespace.

See [rules.md](rules.md) for the full list and default levels.

## Rule levels and presets

| Level | Meaning |
| --- | --- |
| `allow` | Do not emit this rule's diagnostics. |
| `warn` | Emit a warning. |
| `deny` | Emit an error that fails the lint run. |

The `recommended` preset uses descriptor defaults. The `strict` preset promotes warnings to errors and makes SCH024 component metadata mandatory for all detected capacitors. Explicit per-rule level overrides take precedence. The `generic` preset skips project-profile rules.

## Design profiles

Profiles are versioned JSON documents. Their schema is represented by typed Rust structures in `config.rs` and rejects unrecognized keys.

A design profile can declare:

- Functional groups with titles, members, description phrases, and component purpose/rationale requirements.
- Direct physical pin-to-pin wire pairs, common-net contracts and explicit no-connect requirements.
- Ground pin orientations, adjacency, component/property distance constraints and per-group wire-length budgets.
- DNP-approved components and title-block fields.
- Shared datasheet/page/figure evidence.
- Typed component-intent metadata.

## Typed component metadata

Each metadata record has a stable component reference, component kind, role, function, and rationale. The role is a typed enum rather than an arbitrary free-form string. Type-specific metadata can add terminal references, associated pins, return nets, related components and protected nets.

The first automated component-intent check supports:

- Type classification against the placed symbol reference and embedded library identifier.
- Role/type compatibility.
- Non-empty function and engineering rationale.
- Strict-mode metadata presence for every detected capacitor.
- Required target and terminal metadata for decoupling capacitors.

For each decoupling capacitor, declare:

- `associated_pin`: the exact IC pin it serves, in `REFERENCE.NUMBER` form.
- `connection_pin`: the capacitor library terminal number wired to that IC pin.
- `return_pin`: the capacitor terminal that returns to the common reference net.
- `return_net`: expected label/net name (normally `GND`).
- Optional `cluster`: a shared identifier for review/documentation.
- Optional `max_distance_mm`: override for terminal-to-target-pin distance.

At profile level, set `decoupling_max_pin_distance_mm` and `decoupling_max_cluster_spacing_mm`. The linter then verifies that the target IC pin and capacitor connection terminal share a physical wire island (labels do not bridge a disconnected wire island), that the return terminal is wired and carries the declared net label, that all decouplers for one target pin are close to one another, and that they share one physical return island.

This is a schematic intent check. It cannot prove PCB-side capacitor placement, return-current path quality, loop inductance, effective capacitance under bias, voltage derating, ESR/ESL, or PDN impedance.

Other typed roles are included for resistors, diodes, inductors, transistors, ICs, connectors and test points. Their specific circuit checks should be added only with explicit pin/terminal contracts and positive/negative fixtures. For example, a resistor role can state the nets it connects; a protection diode can identify the nets it protects; a back-to-back switch can name its paired transistor. Avoid inferring these details from component references alone.

## Writing a new rule

Each rule needs:

- Stable ID and name.
- Category, summary and engineering explanation.
- Default severity justified by impact and subjectivity.
- Typed, validated configuration options with documented defaults.
- A positive fixture that produces a finding.
- A negative fixture proving valid input passes.
- Boundary cases for hierarchy, symbol rotation, tolerances and source mapping.
- Diagnostic assertions for code, severity, explanation, source span and physical location.
- Regression fixtures for every discovered false positive or false negative.

Prefer sharing semantic indexes and geometry analyses rather than recomputing them independently in each rule. Rules must not mutate the parsed model.

## Fixes and future extensions

Fix proposals remain a separate layer. A fix must state applicability and preconditions, preserve connectivity, and be verified by a fresh KiCad CLI export/ERC run. Geometry fixes must never change coordinates blindly without understanding pin topology and frame constraints.

Third-party dynamic rule plugins are not implemented. Keep the built-in rule API small and versioned before adding native plugin loading or sandboxing.
