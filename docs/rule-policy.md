# Rule policy

The CLI can apply an optional project policy to built-in schematic rules:

```sh
kicad-clippy lint design.kicad_sch --policy kicad-clippy.toml
```

A policy is a TOML file. It selects a named baseline and may override individual built-in rule levels:

```toml
preset = "default" # default, strict, or advisory

[rules]
SCH013 = "warn"
SCH015 = "deny"
SCH019 = "allow"
```

## Named presets

| Preset | Effective level for unspecified rules |
| --- | --- |
| `default` | Uses each rule's `RuleDescriptor.default_level` (currently deny except SCH016, which warns). |
| `strict` | Deny every built-in rule. |
| `advisory` | Warn for every built-in rule. |

A value in `[rules]` takes precedence over the selected preset:

- `allow` suppresses findings from that built-in rule.
- `warn` retains findings as warnings, so they do not fail the command.
- `deny` reports findings as errors, so active findings fail the command.

Only stable built-in IDs from SCH001 through SCH022 are accepted. A misspelled or unknown rule ID is an error rather than a silently ignored setting. Unknown TOML fields are also rejected.

## Scope and compatibility

The policy is opt-in. Without `--policy`, existing rule-emitted severities and CLI exit behavior remain unchanged.

The policy is applied after native ERC, built-in schematic rules, and project-profile checks have produced diagnostics. It changes only diagnostics whose codes exist in the built-in descriptor registry. Native `KICAD/ERC/...` findings are preserved with the severity and exclusion state reported by KiCad.

When a policy file is supplied, its selected preset applies to all built-in findings, including warnings emitted by a rule whose effective level is `deny`. This makes the chosen policy authoritative at the rule level. Profile validation and native ERC still run; `allow` affects reporting, not parsing, netlist export, or the underlying check execution.

The descriptor's default level is the baseline for the `default` preset. A named preset is a starting point, not a replacement for the per-rule map. There is not yet a schema for typed per-rule options or third-party rules.
