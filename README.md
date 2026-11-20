# kicad-clippy

[![CI](https://github.com/Uhrendoktor/kicad-clippy/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/Uhrendoktor/kicad-clippy/actions/workflows/ci.yml)

A Rust-based linter for KiCad schematic files. `kicad-clippy` combines repository-specific schematic quality rules with KiCad's native Electrical Rules Checker (ERC). Native ERC remains the authority for electrical-rule findings; custom rules add layout, annotation, documentation, and project-profile checks.

> **Project status: early development.** The current Rust implementation includes a lossless S-expression parser, shared source/diagnostic contracts, typed built-in rule identifiers, a deterministic extensible rule-pass engine, native ERC/netlist adapters, custom schematic rules SCH001–SCH028, board-level rules PCB001 and PCB009, project-profile checks, and a CLI. Some geometric and profile rules remain explicitly partial; see [rule coverage](docs/rules.md) and [scope and roadmap](docs/roadmap.md).

## What it does

- Parses `.kicad_sch` and `.kicad_pcb` files as lossless S-expression documents and checks their root types.
- Runs native ERC using `kicad-cli sch erc --format json --severity-all`.
- Exports the flattened netlist for profile-declared pin/net contracts.
- Runs built-in schematic rules, optional project-profile checks, decoupling placement checks, and required test-point pad exposure checks when a `.kicad_pcb` file is supplied.
- Applies an opt-in project grid contract to selected symbol, pin, label, junction and wire anchors.
- Enforces profile-declared global-label allow/deny lists and required label scopes on the loaded sheet.
- Checks UUID-linked functional-frame title tabs, title-box containment and optional frame spacing.
- Applies an optional TOML rule policy with `default`, `strict`, or `advisory` presets and per-rule `allow`, `warn`, or `deny` overrides.
- Checks overlaps between independently declared functional-group frames and enforces profile-declared no-connect pin markers.
- Checks duplicate UUIDs across a root schematic and its referenced child-sheet hierarchy when the root file is loaded from disk.
- Preserves source byte spans, physical coordinates, sheet paths and affected item UUIDs where available.
- Emits human-readable or JSON diagnostics with an exit code based on active errors.

## Requirements

- Rust stable (edition 2024; minimum Rust 1.85).
- KiCad CLI (`kicad-cli` or `kicad-cli-nightly`) for native ERC and profile netlist export.
- A KiCad schematic file ending in `.kicad_sch`.

Parser and built-in rule mode can run without KiCad using `--skip-erc`, provided no profile is supplied (profiles require native netlist export).

## Build

```sh
git clone https://github.com/Uhrendoktor/kicad-clippy.git
cd kicad-clippy
cargo build --locked
```

## Usage

```sh
kicad-clippy lint path/to/design.kicad_sch
kicad-clippy lint path/to/design.kicad_sch --format json
kicad-clippy lint path/to/design.kicad_sch --skip-erc
kicad-clippy lint path/to/design.kicad_sch --profile project-profile.json
kicad-clippy lint path/to/design.kicad_sch --profile project-profile.json --board path/to/design.kicad_pcb
kicad-clippy lint path/to/design.kicad_sch --policy kicad-clippy.toml
```

Select the KiCad CLI executable explicitly with `--kicad-cli PATH` or the `KICAD_CLI` environment variable. Without an explicit path, the CLI searches for `kicad-cli-nightly` and then `kicad-cli` on `PATH`.

## Code layout

The project is one Rust package. Its library keeps the implementation organized
into focused modules, and the binary delegates to the shared CLI implementation.

| Module | Responsibility |
| --- | --- |
| core | Source spans, diagnostics, typed built-in rule IDs, and rule metadata. |
| parser | Lossless KiCad schematic and PCB S-expression parsing. |
| erc | KiCad CLI ERC and flattened-netlist adapters. |
| rules | Built-in schematic rules and project-profile contracts. |
| cli | Command-line parsing, lint orchestration, and output formatting. |

## Development quality gates

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-features --locked
cargo test --doc --locked
```

See [contributing](CONTRIBUTING.md) for the development workflow.

## Relationship to KiCad

KiCad's native ERC remains responsible for electrical semantics such as pin-type conflicts, undriven power pins, and unconnected pins. `kicad-clippy` invokes that engine and adds independent design-quality checks. It does not patch KiCad, replace ERC, or register custom checks inside KiCad's native ERC catalogue.
