#![forbid(unsafe_code)]

//! Lossless `KiCad` schematic parsing over the kiutils S-expression CST.
/// Parses `KiCad` schematic S-expressions while retaining the original source text and byte spans for diagnostics.
mod board;
/// Lossless S-expression parsing and source retention for `KiCad` schematic documents.
mod schematic;

pub use board::{BoardDocument, BoardParseError};
pub use schematic::{SchematicDocument, SchematicParseError};
