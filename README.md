# kicad-clippy

[![CI](https://github.com/Uhrendoktor/kicad-clippy/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/Uhrendoktor/kicad-clippy/actions/workflows/ci.yml)

A Rust-based linter for KiCad schematic files. `kicad-clippy` combines repository-specific schematic quality rules with KiCad's native Electrical Rules Checker (ERC). Native ERC remains the authority for electrical-rule findings; custom rules add layout, annotation, documentation, and project-profile checks.

> **Project status: early development.** The Rust implementation now runs the migrated schematic rules (SCH001, SCH002, SCH019) and supports project component-intent profiles (SCH020–SCH022). The remaining historical rules are tracked explicitly in the rule status table. See [scope and roadmap](docs/roadmap.md).

## What it does today

- Parses a `.kicad_sch` file as a lossless S-expression document and checks its root type.
- Runs native ERC using `kicad-cli sch erc --format json --severity-all`.
- Normalizes ERC violations from the per-sheet JSON report, preserving severity, exclusion state, sheet paths, affected item UUIDs, and physical coordinates.
- Runs built-in geometry and label rules with source spans and schematic coordinates.\n- Loads versioned JSON project profiles and validates strict component metadata, including pin-specific decoupling intent.\n- Exposes shared UTF-8 byte spans, line/column mapping, diagnostic types, rule descriptors, levels, and a deterministic rule registry.
- Emits human-readable or JSON diagnostics with an exit code based on active ERC errors.

The full SCH001–SCH020 catalogue is not yet ported. Rules that need transformed symbol geometry, hierarchical netlists, or group frames remain pending. SARIF and verified fixes are also pending.

## Requirements

- Rust stable (edition 2024; minimum Rust 1.85).
- A KiCad CLI build with `sch erc` and JSON report support. The native-ERC CI test uses KiCad development nightly; older system packages may not include the schematic ERC subcommand.
- A KiCad schematic file ending in `.kicad_sch`.

The parser-only mode can be used without KiCad installed.

## Build

Clone the repository and build from its root:

```sh
git clone https://github.com/Uhrendoktor/kicad-clippy.git
cd kicad-clippy
cargo build --workspace --locked
```

Install the CLI locally:

```sh
cargo install --path crates/kicad-clippy-cli --locked
```

## Usage

Run native ERC and display diagnostics:

```sh
kicad-clippy lint path/to/design.kicad_sch
```

Select the KiCad CLI executable explicitly:

```sh
kicad-clippy lint path/to/design.kicad_sch --kicad-cli /path/to/kicad-cli
```

The executable can also be selected with the `KICAD_CLI` environment variable. Without an explicit path, the CLI searches for `kicad-cli-nightly` and then `kicad-cli` on `PATH`.

Emit structured JSON:

```sh
kicad-clippy lint path/to/design.kicad_sch --format json
```

Run the generic schematic checks without invoking ERC:\n\n```sh\nkicad-clippy lint path/to/design.kicad_sch --skip-erc\n```\n\nLoad project component-intent metadata, and require complete metadata with strict mode:\n\n```sh\nkicad-clippy lint path/to/design.kicad_sch --profile kicad-clippy.json --strict\n```\n\nSee [component metadata](docs/component-metadata.md), [rule status](docs/rules.md), and the [historical design guidance](docs/historical-design-guidance.md).\n\nCheck S-expression syntax without invoking ERC:

```sh
kicad-clippy lint path/to/design.kicad_sch --skip-erc
```

`--skip-erc` is intended for parser development and environments without KiCad. It is not an electrical validation substitute. The command exits non-zero when active ERC errors are present or when parsing/CLI/report processing fails. Excluded ERC findings remain in output but do not count as active errors.

Run the CLI directly from the workspace while developing:

```sh
cargo run -p kicad-clippy -- lint path/to/design.kicad_sch
```

## Workspace layout

| Crate | Responsibility |
| --- | --- |
| `kicad-clippy-core` | Source spans, diagnostics, severities, rule descriptors, and registry. |
| `kicad-clippy-parser` | Lossless KiCad schematic S-expression parsing. |
| `kicad-clippy-erc` | KiCad CLI ERC execution and JSON report normalization. |\n| `kicad-clippy-rules` | Built-in schematic geometry/label rules and project component-intent validation. |
| `kicad-clippy` | Command-line interface and output formatting. |

See [architecture](docs/architecture.md) for the data flow and design boundaries.

## Development quality gates

Every Rust crate inherits the workspace lint policy. Unsafe Rust is forbidden; Rust warnings, Clippy `all`, and Clippy `pedantic` are denied. Tests run with all workspace targets and features.

Run the same checks locally:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo test --workspace --doc --locked
```

See [contributing](CONTRIBUTING.md) for the full workflow and [rule development](docs/rule-development.md) for guidance on extending the rule system.

## Documentation

- [Getting started](docs/getting-started.md)
- [Architecture](docs/architecture.md)
- [Rule development](docs/rule-development.md)
- [Built-in rule status](docs/rules.md)
- [Component intent metadata](docs/component-metadata.md)
- [Historical design guidance](docs/historical-design-guidance.md)
- [Scope and roadmap](docs/roadmap.md)
- [Contributing](CONTRIBUTING.md)
- [Changelog](CHANGELOG.md)

## Relationship to KiCad

KiCad's native ERC remains responsible for electrical semantics such as pin-type conflicts, undriven power pins, and unconnected pins. `kicad-clippy` invokes that engine and adds repository-defined design-quality checks as those rules are ported. It does not patch KiCad, replace ERC, or register custom checks inside KiCad's native ERC catalogue.

A future Eeschema integration may visualize `kicad-clippy` findings, but editor integration is not part of the current CLI implementation.

## License

This project is licensed under the MIT License. See [LICENSE](LICENSE).
