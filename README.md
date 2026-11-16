# kicad-clippy

[![CI](https://github.com/Uhrendoktor/kicad-clippy/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/Uhrendoktor/kicad-clippy/actions/workflows/ci.yml)

A Rust-based linter for KiCad schematics. It combines reusable schematic geometry and presentation rules, declarative design-intent profiles, and KiCad's native Electrical Rules Checker (ERC). Native ERC remains authoritative for electrical-rule findings; custom rules add layout, annotation, metadata, and design-intent checks that ERC cannot infer.

## Current implementation

The Rust engine preserves the legacy rule IDs SCH001–SCH020 and adds SCH021–SCH025:

- Lossless KiCad S-expression parsing with UTF-8 source spans.
- Generic wire, label, annotation, symbol-field, and geometry checks.
- Declarative project profiles for physical-net contracts, functional groups, field spacing, DNP population, documentation, and source evidence.
- Typed component-intent metadata for capacitors, resistors, diodes, inductors, transistors, ICs, connectors and test points.
- Strict-mode requirements for capacitor role metadata, including a target pin for every decoupling capacitor.
- Decoupling checks for physical connection to the declared IC pin, schematic proximity, adjacency of capacitors serving the same pin, and a shared physical return net.
- Duplicate UUID and title-block checks.
- Native KiCad ERC execution and JSON normalization.
- Text and JSON diagnostics with rule explanations, source paths, byte spans, and physical schematic coordinates.

The project is in early development. Auto-fix proposals, SARIF, broader hierarchy-aware semantic analysis, and KiCad GUI/editor integration remain future work.

## Requirements

- Rust stable (edition 2024; minimum Rust 1.85).
- KiCad CLI with `sch erc` and JSON report support. The native-ERC CI job uses KiCad development nightly.
- A KiCad schematic file (`.kicad_sch`).

The custom-rule engine can run without KiCad by using `--skip-erc`.

## Build

```sh
git clone https://github.com/Uhrendoktor/kicad-clippy.git
cd kicad-clippy
cargo build --workspace --locked
cargo install --path crates/kicad-clippy-cli --locked
```

## Usage

Run the built-in generic rules and native KiCad ERC:

```sh
kicad-clippy lint path/to/design.kicad_sch
```

Run custom rules without launching KiCad ERC:

```sh
kicad-clippy lint path/to/design.kicad_sch --skip-erc
```

Load a design profile:

```sh
kicad-clippy lint path/to/design.kicad_sch --config project/kicad-clippy.json
```

List custom rules:

```sh
kicad-clippy rules
kicad-clippy rules --format json
```

Choose text or JSON output:

```sh
kicad-clippy lint path/to/design.kicad_sch --format json
```

The executable can be selected with `--kicad-cli PATH` or the `KICAD_CLI` environment variable. Without either, the CLI searches for `kicad-cli-nightly` and then `kicad-cli` on `PATH`.

A custom-rule error fails the command. Excluded native ERC findings remain visible but do not count as active errors.

## Presets and profiles

Three presets are supported:

| Preset | Behavior |
| --- | --- |
| `generic` | Reusable schematic geometry and presentation rules only; no project profile is loaded. |
| `recommended` | Rule defaults, with project-specific contracts enabled by a supplied profile. |
| `strict` | Promotes warnings to errors and requires typed metadata for every detected capacitor unless SCH024 is explicitly overridden. |

Levels can be changed per rule with `allow`, `warn`, or `deny`. JSON schema version 1 is demonstrated in [the STUSB4500 profile](examples/stusb4500/kicad-clippy.json). See [rules](docs/rules.md) and [design guidance](docs/design-guidance.md).

## Workspace layout

| Crate | Responsibility |
| --- | --- |
| `kicad-clippy-core` | Source spans, diagnostics, severities, rule descriptors, and registry. |
| `kicad-clippy-parser` | Lossless KiCad schematic parsing. |
| `kicad-clippy-erc` | KiCad CLI ERC execution and report normalization. |
| `kicad-clippy-rules` | Schematic model, rule catalogue, configuration, and profile execution. |
| `kicad-clippy` | Command-line interface and output formatting. |

## Quality gates

The workspace forbids unsafe Rust and denies compiler warnings, Clippy `all`, and Clippy `pedantic`.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo test --workspace --doc --locked
```

## Documentation

- [Getting started](docs/getting-started.md)
- [Architecture](docs/architecture.md)
- [Built-in rules](docs/rules.md)
- [Historical design guidance and further research](docs/design-guidance.md)
- [Rule development](docs/rule-development.md)
- [Scope and roadmap](docs/roadmap.md)
- [Contributing](CONTRIBUTING.md)

## Relationship to KiCad

KiCad ERC continues to check pin-type conflicts, undriven power pins, unconnected pins, and other electrical conditions supported by KiCad. The custom rules do not attempt to recreate ERC. They validate declared design intent such as decoupler-to-pin relationships, documentation, component roles, and layout conventions. See the [KiCad ERC reference](https://docs.kicad.org/10.0/en/eeschema/eeschema.html).

A future Eeschema integration may visualize findings, but editor integration is not included in this CLI milestone.

## License

MIT. See [LICENSE](LICENSE).
