//! Lossless `KiCad` PCB parsing over the kiutils S-expression CST.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use kicad_clippy_core::SourceFile;
use kiutils_sexpr::{Atom, CstDocument, Node, ParseError, parse_one};
use thiserror::Error;

/// A parsed PCB retaining its original source and lossless CST.
#[derive(Debug)]
pub struct BoardDocument {
    /// Original path and text buffer used to resolve source locations.
    source: SourceFile,
    /// Lossless parsed S-expression tree, including source spans for every node.
    cst: CstDocument,
}

impl BoardDocument {
    /// Reads a UTF-8 `.kicad_pcb` file and retains its path for diagnostics.
    ///
    /// # Arguments
    ///
    /// * `path` — Path to the PCB source file.
    ///
    /// # Returns
    ///
    /// The parsed board document, or a read, syntax, root-kind, or source-retention error.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the file cannot be read, a syntax error for malformed S-expressions,
    /// a root error when the expression is not `kicad_pcb`, or a lossless-source error when bytes differ.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, BoardParseError> {
        let path = path.as_ref().to_path_buf();
        let text = fs::read_to_string(&path).map_err(|source| BoardParseError::Io {
            path: path.clone(),
            source,
        })?;
        Self::from_text(path, text)
    }

    /// Parses one S-expression and requires a `kicad_pcb` root.
    ///
    /// # Arguments
    ///
    /// * `path` — Logical source path retained for diagnostics and callers.
    /// * `text` — Complete PCB source text to parse.
    ///
    /// # Returns
    ///
    /// A lossless parsed PCB document.
    ///
    /// # Errors
    ///
    /// Returns a syntax error for malformed S-expressions, a root error for a non-`kicad_pcb` root,
    /// or a lossless-source error if the parser changes the original text.
    pub fn from_text(
        path: impl Into<PathBuf>,
        text: impl Into<String>,
    ) -> Result<Self, BoardParseError> {
        let path = path.into();
        let text = text.into();
        let mut cst = parse_one(&text).map_err(|source| BoardParseError::Syntax {
            path: path.clone(),
            source,
        })?;
        let actual_root = root_name(&cst).unwrap_or_default();
        if actual_root != "kicad_pcb" {
            return Err(BoardParseError::WrongRoot {
                path,
                actual: actual_root.to_owned(),
            });
        }
        if cst.to_lossless_string() != text {
            return Err(BoardParseError::LosslessInvariant { path });
        }

        // Decode string values from their exact quoted source tokens while retaining the original CST spans.
        unescape_quoted_atoms(&mut cst.nodes, &text);
        Ok(Self {
            source: SourceFile::new(path, text),
            cst,
        })
    }

    /// Returns the original PCB source and path used by this document.
    ///
    /// # Returns
    ///
    /// The source file retained during parsing.
    #[must_use]
    pub const fn source(&self) -> &SourceFile {
        &self.source
    }

    /// Returns the decoded, lossless PCB syntax tree.
    ///
    /// # Returns
    ///
    /// The concrete syntax tree for the complete `kicad_pcb` source.
    #[must_use]
    pub const fn cst(&self) -> &CstDocument {
        &self.cst
    }
}

/// Errors raised while loading or parsing a `KiCad` PCB.
#[derive(Debug, Error)]
pub enum BoardParseError {
    /// The source file could not be read as UTF-8 text.
    #[error("could not read PCB {path}: {source}")]
    Io {
        /// Path being read.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// The S-expression parser rejected the input.
    #[error("invalid S-expression in PCB {path}: {source}")]
    Syntax {
        /// Path being parsed.
        path: PathBuf,
        /// Underlying parser error.
        #[source]
        source: ParseError,
    },
    /// The root expression is not a `KiCad` PCB document.
    #[error("expected a kicad_pcb root in {path}, found {actual:?}")]
    WrongRoot {
        /// Path being parsed.
        path: PathBuf,
        /// Actual root identifier, or an empty string when absent.
        actual: String,
    },
    /// The parser did not retain the exact input text.
    #[error("lossless source invariant failed while parsing PCB {path}")]
    LosslessInvariant {
        /// Path being parsed.
        path: PathBuf,
    },
}

/// Returns the symbol naming the root S-expression.
fn root_name(document: &CstDocument) -> Option<&str> {
    let Node::List { items, .. } = document.nodes.first()? else {
        return None;
    };
    let Node::Atom {
        atom: Atom::Symbol(name),
        ..
    } = items.first()?
    else {
        return None;
    };
    Some(name)
}

/// Decodes quoted atom values from their exact source tokens while keeping CST spans stable.
///
/// # Arguments
///
/// * `nodes` — Mutable syntax-tree nodes whose quoted values must be decoded.
/// * `source` — Original source text used to decode quoted token contents by byte span.
///
/// # Returns
///
/// No value; quoted atom payloads are updated in place.
fn unescape_quoted_atoms(nodes: &mut [Node], source: &str) {
    for node in nodes {
        match node {
            Node::List { items, .. } => unescape_quoted_atoms(items, source),
            Node::Atom {
                atom: Atom::Quoted(value),
                span,
            } => {
                if let Some(content) = source
                    .get(span.start..span.end)
                    .and_then(|token| token.strip_prefix('"'))
                    .and_then(|token| token.strip_suffix('"'))
                {
                    *value = unescaper::unescape_lossy(content);
                }
            }
            Node::Atom { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BoardDocument, BoardParseError};

    /// Parses a minimal board and preserves the original serialized source.
    #[test]
    fn parses_kicad_pcb_root_losslessly() {
        let source = r#"(kicad_pcb (version 20250101) (generator "pcbnew"))"#;
        let board = BoardDocument::from_text("minimal.kicad_pcb", source).expect("board parses");
        assert_eq!(board.cst().to_lossless_string(), source);
        assert_eq!(board.source().path().to_string_lossy(), "minimal.kicad_pcb");
    }

    /// Rejects a schematic document passed to the PCB parser.
    #[test]
    fn rejects_non_board_root() {
        let error = BoardDocument::from_text("wrong.kicad_pcb", "(kicad_sch)").unwrap_err();
        assert!(matches!(error, BoardParseError::WrongRoot { .. }));
    }

    /// Decodes escaped string values without changing the original lossless source.
    #[test]
    fn decodes_quoted_values_and_retains_source() {
        let source = r#"(kicad_pcb (property "comment" "line\nnext"))"#;
        let board = BoardDocument::from_text("escaped.kicad_pcb", source).expect("board parses");
        assert_eq!(board.cst().to_lossless_string(), source);
        let text = format!("{:?}", board.cst().nodes);
        assert!(text.contains("line\\nnext"));
    }
}
