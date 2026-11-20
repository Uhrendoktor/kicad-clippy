#![forbid(unsafe_code)]

//! KiCad schematic linting, including lossless parsing, native ERC integration,
//! built-in rules, project profiles, diagnostics, and the command-line front end.

/// Command-line argument parsing, lint orchestration, and output rendering.
mod cli;
/// Shared source locations, diagnostics, rule identifiers, and rule policy contracts.
pub mod core;
/// Native KiCad ERC execution and flattened-netlist adapters.
pub mod erc;
/// Lossless KiCad schematic and PCB parsing.
pub mod parser;
/// Built-in schematic rules, rule-pass orchestration, and project-profile checks.
pub mod rules;

pub use cli::run;
