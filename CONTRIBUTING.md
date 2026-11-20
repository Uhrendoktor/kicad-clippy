# Contributing

Contributions are welcome for parser correctness, ERC handling, rule design, documentation, and tests.

## Before opening a pull request

For a substantial change, open an issue or explain the problem, constraints, and intended behavior in the pull request. Keep changes scoped to one implementation milestone.

Include:
- The behavior being added or corrected.
- Relevant design trade-offs.
- Tests and fixtures.
- Documentation updates for user-visible behavior or API changes.
- Validation output for all required quality gates.

## Development setup

Install Rust stable with Cargo, rustfmt, and Clippy. Install KiCad if the change touches native ERC execution.

```sh
cargo build --locked
```

## Required local checks

Run these commands from the repository root:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-features --locked
cargo test --doc --locked
```

The root manifest applies one strict lint policy to the library modules and binary. Do not add crate-local lint overrides merely to silence a warning. Fix the cause or document a narrowly scoped exception with a clear reason.

## Rust conventions

- Keep unsafe code forbidden.
- Prefer explicit error types and propagate errors with `Result`.
- Do not use `unwrap`, `expect`, `panic!`, `todo!`, or `unimplemented!` in production code.
- Document public APIs, including `# Errors` sections for fallible functions.
- Keep rule metadata separate from rule execution.
- Preserve source spans and original schematic text; do not canonicalize source as a side effect of linting.
- Keep physical schematic coordinates distinct from source byte offsets.

## Documentation and lint policy

- Every Rust function and method, including private helpers and tests, must have a doc comment.
- Document every named argument under `# Arguments`, every explicit non-unit return type under `# Returns`, and fallible functions under `# Errors`.
- Document structs, enums, traits, modules, constants, static values, enum variants, and named fields.
- Physical units, geometry tolerances, algorithm thresholds, file-format defaults, and conversion factors must be named constants. Do not repeat values such as `0.001` inline.
- The crate denies Clippy `all`, `pedantic`, `nursery`, and `cargo` groups. Restriction lints are selected individually.

## Tests

Add focused unit tests and fixtures for changed behavior. For parser and ERC changes, include malformed and boundary cases. For diagnostics, assert stable identifiers, severity, messages, sheet paths, UUIDs, coordinates, and exclusion handling rather than checking only result counts.

The CI workflow runs formatting, strict Clippy, unit tests, doc tests, and a native KiCad ERC smoke test. Changes to native ERC behavior should be tested with an actual supported `kicad-cli` whenever possible.

## Documentation

Update the README and relevant pages under `docs/` when commands, output, requirements, rule semantics, configuration, or supported behavior changes. State clearly whether a feature is implemented or planned.
