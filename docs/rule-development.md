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

`RuleDescriptor` records the stable ID and explanatory metadata: name, category, summary, explanation, default level, optional documentation URL and tags. `builtin_rule_registry()` in `core` populates a deterministic descriptor for every value in `BuiltinRuleId::ALL` (SCH001–SCH022); it is the canonical built-in metadata catalogue. The completeness test ensures that adding a typed built-in ID without a descriptor fails validation. `RuleRegistry` is deterministic and rejects duplicate IDs.

When a policy file is supplied, `default_level` provides the baseline for the `default` preset. The `strict` and `advisory` presets replace that baseline, and explicit per-rule levels take precedence. Without a policy file, descriptor metadata does not change runtime diagnostics; the pre-existing rule-emitted severities remain in force. See [rule policy](rule-policy.md) for the TOML schema and CLI behavior.

A rule declaration is not its implementation. A check can emit more than one diagnostic, several checks can emit the same rule ID, and a single analysis pass can contribute findings to multiple rules. Keep metadata and execution separate.

Levels are:

| Level | Behavior |
| --- | --- |
| `Allow` | Disable a rule. |
| `Warn` | Emit a non-blocking finding. |
| `Deny` | Emit an error that fails a lint run. |

## Execution model

The in-process rule engine is implemented. `RuleEngine` collects schematic facts, wire segments and junctions once, builds an immutable `RuleContext`, then executes registered `RulePass` implementations in stable name order. Duplicate pass names are rejected. The built-in checks currently run as geometry/pin-placement, label/topology, and annotation/field passes. A pass may orchestrate several rule modules and emit diagnostics for several rule IDs. The built-in source layout nevertheless keeps each rule's implementation in a dedicated file named for its stable identifier.

The current shared context exposes the parsed document, schematic facts, segments and junctions. The longer-term analysis phases remain:

1. **Syntax:** facts from the lossless CST and source byte spans.
2. **Semantic:** resolved symbols, UUIDs, hierarchy, fields and embedded library definitions.
3. **Geometry:** library-backed primitives and predicates, with KiCad-specific tolerances and pin anchors.
4. **Connectivity:** explicit-junction physical wire components and native netlist membership.
5. **Project contracts:** profile-declared component intent, groups, frames and evidence.

Rules receive immutable context and return normalized diagnostics. They do not mutate the schematic. Configuration selects effective levels and typed options without changing rule implementations. The detailed rationale and library evaluation are in [architecture research](architecture-research.md).

## External extensions

The in-process `RulePass` extension point is available to Rust crates that can link to `rules`. It is not yet a versioned third-party compatibility contract: the trait/context, options, analysis dependencies, diagnostic compatibility policy and resource limits still need formalization. Dynamic libraries or WASM rule packs remain deferred until those contracts are stable.

## Source layout

Every built-in identifier has a dedicated implementation file. Schematic geometry and label checks live in `src/rules/rules/schNNN.rs`; profile-backed connectivity, group, component-intent, and decoupling checks live in `src/rules/profile_rules/schNNN.rs`. The shared `generic.rs` module owns normalized schematic facts and common geometry helpers, while `metadata.rs` owns the profile data model and validation orchestration. Keep rule-specific diagnostics and helpers in the matching `schNNN.rs` module, and move a helper to the shared parent only when more than one rule depends on it.

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
