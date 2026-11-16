# Rule development

This document describes the built-in rule identity and the intended execution contract.

## Built-in identifiers

Use `BuiltinRuleId` for built-in rules. It is a closed enum covering SCH001–SCH022, provides stable string conversion for output, and rejects unknown built-in IDs when parsing. For example:

```rust
use kicad_clippy_core::{BuiltinRuleId, Diagnostic, Severity};

let finding = Diagnostic::for_builtin_rule(
    BuiltinRuleId::Sch001,
    Severity::Error,
    "wire is not orthogonal",
);
assert_eq!(finding.code, "SCH001");
```

Use the open-ended string namespace only for external diagnostics such as `KICAD/ERC/<type>` and future third-party rule IDs. A string code in serialized JSON is a compatibility boundary; the Rust implementation should use typed built-in IDs.

## Metadata and registration

`RuleDescriptor` records the stable ID and explanatory metadata: name, category, summary, explanation, default level, optional documentation URL and tags. `RuleRegistry` is deterministic and rejects duplicate IDs.

A rule declaration is not its implementation. A check can emit more than one diagnostic, several checks can emit the same rule ID, and a single analysis pass can contribute findings to multiple rules. Keep metadata and execution separate.

Levels are:

| Level | Behavior |
| --- | --- |
| `Allow` | Disable a rule. |
| `Warn` | Emit a non-blocking finding. |
| `Deny` | Emit an error that fails a lint run. |

## Execution model

The intended runtime has one immutable `RuleContext` containing the parsed document, semantic facts, geometry indexes, physical connectivity graph, optional native netlist, and validated project profile. Rules declare which analysis products they require. The engine computes each shared analysis once and schedules rules in deterministic phase order.

Suggested phases:

1. **Syntax:** facts available directly from the lossless CST and source spans.
2. **Semantic:** resolved symbols, UUIDs, hierarchy, fields and embedded library definitions.
3. **Geometry:** normalized primitives, bounds, pin anchors and spatial candidate indexes.
4. **Connectivity:** explicit-junction physical wire components and native netlist membership.
5. **Project contracts:** profile-declared component intent, groups, frames and evidence.

Rules receive immutable context and return normalized diagnostics. They do not mutate the schematic. Configuration selects effective levels and typed options without changing rule implementations. The detailed rationale and library evaluation are in [architecture research](architecture-research.md).

## External extensions

Do not introduce dynamic libraries or WASM rule packs yet. First stabilize the in-process trait, context, option schema, diagnostic compatibility policy, and resource limits. Third-party extensions should be an optional later layer over that stable API, not a dependency of built-in checks.

## Tests required for each rule

Each rule should include:

- A positive fixture that triggers the diagnostic.
- A negative fixture proving valid input is accepted.
- Boundary cases for coordinate tolerances, hierarchy and source mapping where relevant.
- A diagnostic assertion covering code, severity, message, explanation and location.
- Regression fixtures for every discovered false positive or false negative.

Tests must not rely solely on diagnostic counts. Assert affected objects and location data.

## Contributing a new rule

A new rule proposal should include its stable ID, purpose, default level rationale, positive/negative schematic fixtures, required analysis products, and limitations. Keep electrical semantics in native ERC unless the rule represents a distinct design-quality contract.
