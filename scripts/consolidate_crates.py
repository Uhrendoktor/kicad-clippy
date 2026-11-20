#!/usr/bin/env python3
"""One-time consolidation of the five Rust packages into one crate with modules."""
from __future__ import annotations

from pathlib import Path
import re
import shutil

ROOT = Path(__file__).resolve().parents[1]
SOURCE_CRATES = {
    "core": ROOT / "crates/kicad-clippy-core/src",
    "parser": ROOT / "crates/kicad-clippy-parser/src",
    "erc": ROOT / "crates/kicad-clippy-erc/src",
    "rules": ROOT / "crates/kicad-clippy-rules/src",
}
CLI_SOURCE = ROOT / "crates/kicad-clippy-cli/src/main.rs"
SOURCE_ROOT = ROOT / "src"

if SOURCE_ROOT.exists():
    raise SystemExit("Refusing to overwrite an existing src/ directory.")

SOURCE_ROOT.mkdir()
for module, source in SOURCE_CRATES.items():
    destination = SOURCE_ROOT / module
    shutil.copytree(source, destination)
    (destination / "lib.rs").rename(destination / "mod.rs")

shutil.copy2(CLI_SOURCE, SOURCE_ROOT / "cli.rs")

# Preserve each old crate's public module boundary while making cross-boundary
# references explicit within the new single crate.
crate_paths = {
    "kicad_clippy_core::": "crate::core::",
    "kicad_clippy_parser::": "crate::parser::",
    "kicad_clippy_erc::": "crate::erc::",
    "kicad_clippy_rules::": "crate::rules::",
}
for source in SOURCE_ROOT.rglob("*.rs"):
    content = source.read_text(encoding="utf-8")
    for old, new in crate_paths.items():
        content = content.replace(old, new)
    if source.is_relative_to(SOURCE_ROOT / "core"):
        content = re.sub(
            r"\\bcrate::(?!core::|parser::|erc::|rules::)",
            "crate::core::",
            content,
        )
    elif source.is_relative_to(SOURCE_ROOT / "rules"):
        content = re.sub(
            r"\\bcrate::(?!core::|parser::|erc::|rules::)",
            "crate::rules::",
            content,
        )
    if source.name == "cli.rs":
        matches = list(re.finditer(r"(?m)^fn main\s*\(", content))
        if len(matches) != 1:
            raise SystemExit(f"Expected one CLI main function, found {len(matches)}.")
        position = matches[0].start()
        documentation = (
            "/// Runs the command-line application and returns its process exit status.\n"
            "///\n"
            "/// # Returns\n"
            "///\n"
            "/// ExitCode::SUCCESS when linting completes without active errors, or a failure code otherwise.\n"
        )
        content = content[:position] + documentation + content[position:]
        content = re.sub(r"(?m)^fn main\s*\(", "pub fn run(", content, count=1)
    source.write_text(content, encoding="utf-8")

(SOURCE_ROOT / "lib.rs").write_text(
    """#![forbid(unsafe_code)]

//! KiCad schematic linting, including lossless parsing, native ERC integration,
//! built-in rules, project profiles, diagnostics, and the command-line front end.

/// Shared source locations, diagnostics, rule identifiers, and rule policy contracts.
pub mod core;
/// Native KiCad ERC execution and flattened-netlist adapters.
pub mod erc;
/// Lossless KiCad schematic and PCB parsing.
pub mod parser;
/// Built-in schematic rules, rule-pass orchestration, and project-profile checks.
pub mod rules;
/// Command-line argument parsing, lint orchestration, and output rendering.
mod cli;

pub use cli::run;
""",
    encoding="utf-8",
)
(SOURCE_ROOT / "main.rs").write_text(
    """#![forbid(unsafe_code)]

//! Executable entry point for the kicad-clippy command.

/// Starts the command-line application and returns its process exit status.
///
/// # Returns
///
/// The exit status returned by the shared command-line implementation.
fn main() -> std::process::ExitCode {
    kicad_clippy::run()
}
""",
    encoding="utf-8",
)

(ROOT / "Cargo.toml").write_text(
    """[package]
name = "kicad-clippy"
description = "Clippy-inspired linting front end for KiCad schematics."
version = "0.1.0"
edition = "2024"
license = "MIT"
repository = "https://github.com/Uhrendoktor/kicad-clippy"
rust-version = "1.85"
keywords = ["kicad", "electronics", "schematic", "linting"]
categories = ["development-tools", "command-line-utilities"]

[dependencies]
clap = { version = "4.5", features = ["derive"] }
geo = { version = "0.31", default-features = false }
kiutils_sexpr = "0.1.1"
kurbo = "0.13.1"
miette = { version = "7.6.0", default-features = false }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
tempfile = "3.23"
thiserror = "2.0"
toml = "1.2.1"
unescaper = "0.2.0"
vector-text-core = "0.1.0"
vector-text-newstroke = "0.1.1"

[lints.rust]
unsafe_code = "forbid"
warnings = "deny"
missing_docs = "deny"
unused_must_use = "deny"

[lints.clippy]
all = { level = "deny", priority = -1 }
pedantic = { level = "deny", priority = -1 }
nursery = { level = "deny", priority = -1 }
cargo = { level = "deny", priority = -1 }
missing_docs_in_private_items = "deny"
unwrap_used = "deny"
expect_used = "deny"
panic = "deny"
todo = "deny"
unimplemented = "deny"
dbg_macro = "deny"
""",
    encoding="utf-8",
)

shutil.rmtree(ROOT / "crates")

for relative in (
    "scripts/check-rust-docs.py",
    "scripts/check-rust-magic-numbers.py",
):
    path = ROOT / relative
    content = path.read_text(encoding="utf-8")
    content = content.replace('SOURCE_ROOT = ROOT / "crates"', 'SOURCE_ROOT = ROOT / "src"')
    path.write_text(content, encoding="utf-8")

path_replacements = {
    "crates/kicad-clippy-core/src/lib.rs": "src/core/mod.rs",
    "crates/kicad-clippy-core/src/": "src/core/",
    "crates/kicad-clippy-parser/src/lib.rs": "src/parser/mod.rs",
    "crates/kicad-clippy-parser/src/": "src/parser/",
    "crates/kicad-clippy-erc/src/lib.rs": "src/erc/mod.rs",
    "crates/kicad-clippy-erc/src/": "src/erc/",
    "crates/kicad-clippy-rules/src/lib.rs": "src/rules/mod.rs",
    "crates/kicad-clippy-rules/src/": "src/rules/",
    "crates/kicad-clippy-cli/src/main.rs": "src/cli.rs",
    "crates/kicad-clippy-cli/src/": "src/",
}
name_replacements = {
    "kicad-clippy-core": "core",
    "kicad-clippy-parser": "parser",
    "kicad-clippy-erc": "erc",
    "kicad-clippy-rules": "rules",
    "core crate": "core module",
    "parser crate": "parser module",
    "ERC crate": "erc module",
    "erc crate": "erc module",
    "rules crate": "rules module",
    "CLI crate": "cli module",
    "workspace crates": "crate modules",
}
for path in [ROOT / "README.md", ROOT / "CONTRIBUTING.md", *sorted((ROOT / "docs").glob("*.md"))]:
    content = path.read_text(encoding="utf-8")
    for old, new in path_replacements.items():
        content = content.replace(old, new)
    for old, new in name_replacements.items():
        content = content.replace(old, new)
    path.write_text(content, encoding="utf-8")

readme = ROOT / "README.md"
content = readme.read_text(encoding="utf-8")
content = content.replace("cargo build --workspace --locked", "cargo build --locked")
content = re.sub(
    r"## Workspace layout\n.*?(?=\n## )",
    """## Code layout

The project is one Rust package. Its library keeps the implementation organized
into focused modules, and the binary delegates to the shared CLI implementation.

| Module | Responsibility |
| --- | --- |
| core | Source spans, diagnostics, typed built-in rule IDs, and rule metadata. |
| parser | Lossless KiCad schematic and PCB S-expression parsing. |
| erc | KiCad CLI ERC and flattened-netlist adapters. |
| rules | Built-in schematic rules and project-profile contracts. |
| cli | Command-line parsing, lint orchestration, and output formatting. |
""",
    content,
    count=1,
    flags=re.DOTALL,
)
content = content.replace(
    "cargo clippy --workspace --all-targets --all-features --locked -- -D warnings",
    "cargo clippy --all-targets --all-features --locked -- -D warnings",
)
content = content.replace("cargo test --workspace --all-features --locked", "cargo test --all-features --locked")
content = content.replace("cargo test --workspace --doc --locked", "cargo test --doc --locked")
readme.write_text(content, encoding="utf-8")

contributing = ROOT / "CONTRIBUTING.md"
content = contributing.read_text(encoding="utf-8")
content = content.replace("cargo build --workspace --locked", "cargo build --locked")
content = content.replace(
    "cargo clippy --workspace --all-targets --all-features --locked -- -D warnings",
    "cargo clippy --all-targets --all-features --locked -- -D warnings",
)
content = content.replace("cargo test --workspace --all-features --locked", "cargo test --all-features --locked")
content = content.replace("cargo test --workspace --doc --locked", "cargo test --doc --locked")
content = content.replace(
    "All workspace crates inherit the lint policy from the root manifest.",
    "The crate applies one strict lint policy to the library modules and binary.",
)
contributing.write_text(content, encoding="utf-8")

architecture = ROOT / "docs/architecture.md"
content = architecture.read_text(encoding="utf-8")
start = content.index("## Current workspace")
end = content.index("## Parser and source locations")
module_section = """## Single-crate module structure

    .kicad_sch
       |
       v
    parser ---------------- lossless S-expression CST + original source
       |
       +------------------- core
       |                    source spans, diagnostics, rule IDs, policy
       |
       +------------------- erc
       |                    native KiCad CLI + netlist adapters
       |
       +------------------- rules
       |                    schematic checks + profile contracts
       |
       v
    cli ------------------- orchestration + text / JSON output

| Module | Responsibility |
| --- | --- |
| core | Shared source mapping, diagnostics, severity, typed built-in rule IDs, rule metadata, and deterministic registry. |
| parser | Loads and parses a KiCad schematic while retaining original text and CST spans. |
| erc | Invokes KiCad CLI ERC, exports netlists, and normalizes native reports. |
| rules | Built-in schematic checks, extracted schematic facts, and project-profile contracts. |
| cli | CLI arguments, orchestration, text output, JSON output, and process exit status. |

All five responsibilities compile into the single kicad-clippy package. Internal module paths make their dependency boundaries explicit without publishing separate Cargo packages.

"""
content = content[:start] + module_section + content[end:]
architecture.write_text(content, encoding="utf-8")

# The temporary CI hook is restored separately through the GitHub contents API.

Path(__file__).unlink()
print("Consolidated core, parser, ERC, rules, and CLI into the kicad-clippy crate.")
