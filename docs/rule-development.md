# Rule development

This document describes the rule metadata available today and the constraints for the future executable rule API.

## Current rule metadata

The `kicad-clippy-core` crate currently exposes `RuleDescriptor`, `RuleLevel`, `RuleRegistry`, and `RuleRegistryError`.

A descriptor records a stable ID and explanatory metadata:

```rust
use kicad_clippy_core::{RuleDescriptor, RuleLevel, RuleRegistry, RuleRegistryError};

fn register_builtin_rules(
    registry: &mut RuleRegistry,
) -> Result<(), RuleRegistryError> {
    let descriptor = RuleDescriptor::new(
        "SCH001",
        "wire geometry",
        "layout",
        "Keep schematic wires orthogonal.",
        "Orthogonal routing is easier to inspect and maintain.",
        RuleLevel::Deny,
    );

    registry.register(descriptor)
}
```

The registry is deterministic and rejects duplicate identifiers. Levels are:

| Level | Intended behavior |
| --- | --- |
| `Allow` | Disable a rule. |
| `Warn` | Report a non-blocking finding. |
| `Deny` | Report an error that fails a lint run. |

The current CLI does not yet load or execute descriptors, and it does not expose a `rules` command. The example above demonstrates metadata registration only.

## Stable identifiers

Use a stable, uppercase identifier for built-in schematic rules. The legacy project rules use the `SCHnnn` namespace. Native KiCad findings use `KICAD/ERC/<type>`.

Once published, a rule ID should not be reassigned to a different check. If a rule is retired, document that status rather than recycling its ID.

## Required rule documentation

Each future built-in rule should have:

- A stable ID and concise name.
- A category such as geometry, labels, annotation, documentation, profile, or project contract.
- A one-sentence summary.
- An explanation of the engineering reason for the rule.
- A default level appropriate to its impact and subjectivity.
- Optional help text and a documentation link.
- Rule-specific options with validated types and documented defaults.

Prefer objective correctness and layout invariants as errors. Keep subjective style or documentation preferences as warnings by default until a project profile chooses otherwise.

## Rule implementation contract (planned)

The execution trait and public context API have not been implemented yet. The intended contract is:

1. Rules receive immutable semantic schematic context.
2. Shared analyses and spatial/connectivity indexes are computed once and reused.
3. Rules emit normalized diagnostics through a common reporter.
4. Rules do not mutate the parsed model.
5. Any source edits are proposed by a separate fix planner with an applicability level and preconditions.

Do not create a parallel rule registry or invent an unofficial plugin interface before the public API is designed and versioned.

## Tests required for each rule

When the rule engine lands, each rule should include:

- A positive fixture that triggers the diagnostic.
- A negative fixture proving that valid input is accepted.
- Boundary cases for coordinate tolerances, hierarchy, and source mapping where relevant.
- A diagnostic snapshot covering code, severity, message, explanation, and location.
- An expected fixed-output fixture for machine-applicable fixes.
- A regression fixture for every discovered false positive or false negative.

Tests must not rely solely on a diagnostic count. Assert the affected objects and location data.

## Contributing a new rule

Until the execution API is available, a new rule proposal should include its ID, purpose, positive/negative schematic fixtures, default level rationale, and any required semantic/geometry analysis. Keep the proposed check separate from native ERC unless it represents a genuine additional design-quality constraint.
