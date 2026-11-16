# Getting started

## Prerequisites

Install:

- Rust stable with Cargo, rustfmt and Clippy. The project uses Rust 2024 edition and requires Rust 1.85 or later.
- KiCad CLI with schematic ERC and JSON report support when native ERC is required. The custom rule engine itself can run without KiCad.
- Git.

Confirm the tools:

```sh
rustc --version
cargo --version
kicad-cli version
```

## Build

```sh
git clone https://github.com/Uhrendoktor/kicad-clippy.git
cd kicad-clippy
cargo build --workspace --locked
cargo install --path crates/kicad-clippy-cli --locked
```

## Lint a schematic

Run generic layout rules and native ERC:

```sh
kicad-clippy lint path/to/design.kicad_sch
```

Run the custom rules without starting KiCad:

```sh
kicad-clippy lint path/to/design.kicad_sch --skip-erc
```

This is useful for rule development, but it is not an electrical validation substitute.

## Add a project profile

A profile declares project-specific contracts without hard-coding the project into the rule implementation. It can define functional groups, pin-to-pin physical-wire requirements, common nets, no-connects, component spacing, title-block fields, documentation evidence, and component-intent metadata.

```sh
kicad-clippy lint path/to/design.kicad_sch --config project/kicad-clippy.json
```

The repository includes an example profile at `examples/stusb4500/kicad-clippy.json`. It uses strict mode, retains the migrated STUSB4500 group and net contracts, and declares component roles. Every capacitor is classified (decoupling, filter, bypass or bulk); each decoupling capacitor additionally declares the IC pin it serves, the capacitor terminal connected to that pin, and its return terminal/net.

Strict mode requires metadata for each detected capacitor. It then checks decoupling records against the schematic's embedded symbol pins and physical wire networks. It does not infer electrical topology from reference designators alone.

## Rules and presets

List the catalogue:

```sh
kicad-clippy rules
kicad-clippy rules --format json
```

Choose a preset in the JSON profile:

- `generic`: reusable geometry and presentation rules only.
- `recommended`: rule defaults; profile contracts run when a profile is supplied.
- `strict`: warning rules are errors and component metadata is required for capacitors.

Override a rule level with `allow`, `warn`, or `deny`:

```json
{
  "schema_version": 1,
  "preset": "recommended",
  "rules": {
    "SCH006": "deny",
    "SCH016": "allow"
  }
}
```

## Output

Text is the default. Use `--format json` for structured output. Diagnostics carry the stable rule code, effective severity, reason, help, source path, optional byte span, and physical location in millimetres.

The command exits unsuccessfully when active errors are reported or when parsing, KiCad invocation, or report processing fails.

## Limitations

The first Rust rule milestone does not yet include machine-applicable fixes, SARIF, full project-wide hierarchy traversal, or GUI markers. Schematic-distance metrics are based on 2D drawing coordinates and should not be treated as PCB placement, copper-loop inductance, or electrical performance validation.
