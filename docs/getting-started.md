# Getting started

This guide builds and runs the current `kicad-clippy` CLI against a KiCad schematic.

## Prerequisites

Install:

- Rust stable with Cargo, rustfmt, and Clippy. The project uses Rust 2024 edition and declares Rust 1.85 as its minimum supported version.
- A KiCad development build whose `kicad-cli` provides `sch erc` with JSON output. The standalone CI smoke test currently uses the KiCad development nightly; older distribution-packaged CLI builds may only provide schematic export.
- Git to clone the repository.

Confirm the tools are available:

```sh
rustc --version
cargo --version
kicad-cli version
```

On installations where the KiCad executable is named `kicad-cli-nightly`, use that command instead.

## Build the executable

```sh
git clone https://github.com/Uhrendoktor/kicad-clippy.git
cd kicad-clippy
cargo build --workspace --locked
```

The binary is created at `target/debug/kicad-clippy` (or `target/release/kicad-clippy` for a release build).

For a local installation:

```sh
cargo install --path crates/kicad-clippy-cli --locked
```

## Run a schematic check

```sh
kicad-clippy lint path/to/design.kicad_sch
```

Native ERC runs by default. The CLI first checks that the input is a valid KiCad schematic S-expression document, then invokes the KiCad CLI and normalizes its JSON report.

To select the KiCad executable explicitly:

```sh
kicad-clippy lint path/to/design.kicad_sch --kicad-cli /path/to/kicad-cli
```

Or set `KICAD_CLI` in the environment:

```sh
KICAD_CLI=/path/to/kicad-cli kicad-clippy lint path/to/design.kicad_sch
```

## Choose an output format

Human-readable text is the default:

```sh
kicad-clippy lint path/to/design.kicad_sch
```

For automation, select JSON:

```sh
kicad-clippy lint path/to/design.kicad_sch --format json
```

The JSON response includes a schema version, tool version, schematic path, parser status, KiCad version, normalized diagnostics, and a severity summary.

## Parser-only mode

To validate the S-expression and source handling without launching KiCad:

```sh
kicad-clippy lint path/to/design.kicad_sch --skip-erc
```

This mode is useful while developing parser code or on machines without KiCad. It does not run electrical checks and must not be used as an electrical sign-off.

## Exit status

The command returns a successful exit status when parsing succeeds and no active ERC errors are present. It returns a failure status for active ERC errors or when it cannot parse the schematic, start KiCad, or read/normalize the report.

Excluded ERC findings remain in the output for visibility but are not counted as active errors. Warnings and notes are also retained in the output.

## Current scope

This release is the parser and native ERC foundation. It does not yet execute custom SCH001–SCH020 checks, read project profiles, emit SARIF, propose edits, or integrate with the KiCad GUI. See [roadmap](roadmap.md) before interpreting a clean result as a complete schematic-style audit.
