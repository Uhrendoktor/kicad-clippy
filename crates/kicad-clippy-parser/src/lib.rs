#![forbid(unsafe_code)]

//! Lossless `KiCad` schematic parsing over the kiutils S-expression CST.
/// Contains the schematic implementation and supporting types.
mod schematic;

pub use schematic::{SchematicDocument, SchematicParseError};
