#![forbid(unsafe_code)]

//! Lossless `KiCad` schematic parsing over the kiutils S-expression CST.
/// Parses `KiCad` schematic S-expressions while retaining the original source text and byte spans for diagnostics.
mod board;
mod schematic;

pub use board::{BoardDocument, BoardParseError};
pub use schematic::{SchematicDocument, SchematicParseError};
