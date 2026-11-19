#![forbid(unsafe_code)]

//! Lossless `KiCad` schematic parsing over the kiutils S-expression CST.
/// Parses `KiCad` schematic S-expressions while retaining the original source text and byte spans for diagnostics.
mod schematic;

pub use schematic::{SchematicDocument, SchematicParseError};
