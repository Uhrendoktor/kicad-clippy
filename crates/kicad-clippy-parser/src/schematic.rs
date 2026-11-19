use std::{
    fs, io,
    path::{Path, PathBuf},
};

use kicad_clippy_core::{ByteSpan, SourceFile};
use kiutils_sexpr::{Atom, CstDocument, Node, ParseError, parse_one};
use thiserror::Error;

/// A parsed schematic retaining both its original source and lossless CST.
#[derive(Debug)]
pub struct SchematicDocument {
    /// Original path and text buffer used to resolve source locations.
    source: SourceFile,
    /// Lossless parsed S-expression tree, including source spans for every node.
    cst: CstDocument,
}

impl SchematicDocument {
    /// Reads a UTF-8 schematic file and delegates parsing while retaining the file path for diagnostics.
    ///
    /// # Arguments
    ///
    /// * `path` — Path associated with the source text.
    ///
    /// # Returns
    ///
    /// The parsed schematic document, or a read, syntax, root-kind, or lossless-source error.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read, the S-expression is invalid, the root is not `kicad_sch`, or the source text cannot be preserved losslessly.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, SchematicParseError> {
        let path = path.as_ref().to_path_buf();
        let text = fs::read_to_string(&path).map_err(|source| SchematicParseError::Io {
            path: path.clone(),
            source,
        })?;

        Self::from_text(path, text)
    }

    /// Parses one S-expression, requires a `kicad_sch` root, preserves the exact source text, and decodes quoted atom values.
    ///
    /// # Arguments
    ///
    /// * `path` — Path associated with the source text.
    /// * `text` — Original UTF-8 source text; its byte positions are used by `ByteSpan`.
    ///
    /// # Returns
    ///
    /// The parsed schematic retaining both the original source and the decoded concrete syntax tree, or a parsing/validation error.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read, the S-expression is invalid, the root is not `kicad_sch`, or the source text cannot be preserved losslessly.
    pub fn from_text(
        path: impl Into<PathBuf>,
        text: impl Into<String>,
    ) -> Result<Self, SchematicParseError> {
        let path = path.into();
        let text = text.into();
        let mut cst = parse_one(&text).map_err(|source| SchematicParseError::Syntax {
            path: path.clone(),
            source,
        })?;

        let actual_root = root_name(&cst).unwrap_or_default();
        if actual_root != "kicad_sch" {
            return Err(SchematicParseError::WrongRoot {
                path,
                actual: actual_root.to_owned(),
            });
        }

        if cst.to_lossless_string() != text {
            return Err(SchematicParseError::LosslessInvariant { path });
        }

        // The CST keeps its original raw buffer and byte spans, but the parser's
        // quoted Atom payload only removes the escape backslash. Decode from the
        // original token here so all downstream consumers see semantic text.
        unescape_quoted_atoms(&mut cst.nodes, &text);

        Ok(Self {
            source: SourceFile::new(path, text),
            cst,
        })
    }

    /// Returns the source path and original text buffer associated with this parsed schematic.
    ///
    /// # Returns
    ///
    /// The source file object containing the original path and text.
    #[must_use]
    pub const fn source(&self) -> &SourceFile {
        &self.source
    }

    /// Returns the lossless concrete syntax tree for the parsed schematic.
    ///
    /// # Returns
    ///
    /// The lossless parsed S-expression document.
    #[must_use]
    pub const fn cst(&self) -> &CstDocument {
        &self.cst
    }

    /// Returns the byte span of the top-level schematic expression when the document has a list root.
    ///
    /// # Returns
    ///
    /// The top-level expression's byte span, or `None` when no list root exists.
    #[must_use]
    pub fn root_span(&self) -> Option<ByteSpan> {
        match self.cst.nodes.first() {
            Some(Node::List { span, .. }) => Some(ByteSpan::new(span.start, span.end)),
            _ => None,
        }
    }
}
/// Walks the syntax tree and decodes quoted atom values from their original source tokens without changing the retained lossless source.
///
/// # Arguments
///
/// * `nodes` — S-expression nodes whose quoted atom payloads should be decoded in place.
/// * `source` — Original source text used to decode quoted S-expression tokens by byte span.
///
/// # Returns
///
/// No return value; quoted atom payloads in `nodes` are updated in place.
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
/// Returns the symbol naming the root S-expression when the first syntax-tree node is a well-formed list.
///
/// # Arguments
///
/// * `document` — Parsed schematic document whose source path or syntax tree is requested.
///
/// # Returns
///
/// The root symbol name, or `None` when the syntax tree has no valid list root.
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

/// Errors raised while loading or parsing a `KiCad` schematic.
#[derive(Debug, Error)]
pub enum SchematicParseError {
    /// The source file could not be read as UTF-8 text.
    #[error("could not read schematic {path}: {source}")]
    Io {
        /// Path being read.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// The S-expression parser rejected the input.
    #[error("invalid S-expression in {path}: {source}")]
    Syntax {
        /// Path being parsed.
        path: PathBuf,
        /// Underlying parser error.
        #[source]
        source: ParseError,
    },
    /// The root expression is not a `KiCad` schematic document.
    #[error("expected a kicad_sch root in {path}, found {actual:?}")]
    WrongRoot {
        /// Path being parsed.
        path: PathBuf,
        /// Actual root identifier, or an empty string when absent.
        actual: String,
    },
    /// The parser did not retain the exact input text.
    #[error("lossless source invariant failed while parsing {path}")]
    LosslessInvariant {
        /// Path being parsed.
        path: PathBuf,
    },
}
#[cfg(test)]
/// Unit tests for schematic-root validation, escape decoding, and lossless source retention.
mod tests {
    use std::error::Error;

    use super::SchematicDocument;
    use kicad_clippy_core::ByteSpan;
    /// Minimal valid schematic S-expression used by the parser tests.
    const SCHEMATIC: &str =
        "(kicad_sch (version 20250101) (generator eeschema) (uuid \"root-id\"))";
    /// Verifies that parses only a kicad schematic root behaves as required.
    ///
    /// # Returns
    ///
    /// The requested operation's result, or an error describing the failure.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read, the S-expression is invalid, the root is not `kicad_sch`, or the source text cannot be preserved losslessly.
    #[test]
    fn parses_only_a_kicad_schematic_root() -> Result<(), Box<dyn Error>> {
        let document = SchematicDocument::from_text("fixture.kicad_sch", SCHEMATIC)?;
        let root_span = document.root_span();

        assert_eq!(document.cst().to_lossless_string(), SCHEMATIC);
        assert_eq!(root_span, Some(ByteSpan::new(0, SCHEMATIC.len())));
        assert_eq!(
            document.source().path().to_string_lossy(),
            "fixture.kicad_sch"
        );
        if let Some(span) = root_span {
            assert_eq!(document.source().slice(span), Some(SCHEMATIC));
        }
        Ok(())
    }
    /// Verifies that unescapes quoted atoms and preserves lossless source behaves as required.
    ///
    /// # Returns
    ///
    /// The requested operation's result, or an error describing the failure.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read, the S-expression is invalid, the root is not `kicad_sch`, or the source text cannot be preserved losslessly.
    #[test]
    fn unescapes_quoted_atoms_and_preserves_lossless_source() -> Result<(), Box<dyn Error>> {
        use kiutils_sexpr::{Atom, Node};

        let text = r#"(kicad_sch (title "first\nsecond") (literal "slash\\nsequence") (quote "say \"ok\""))"#;
        let document = SchematicDocument::from_text("quoted.kicad_sch", text)?;

        assert_eq!(document.cst().to_lossless_string(), text);
        let Node::List { items, .. } = &document.cst().nodes[0] else {
            return Err("expected schematic root list".into());
        };
        let quoted_value = |index: usize| {
            let Node::List { items, .. } = &items[index] else {
                return None;
            };
            let Node::Atom {
                atom: Atom::Quoted(value),
                ..
            } = items.get(1)?
            else {
                return None;
            };
            Some(value.as_str())
        };

        assert_eq!(quoted_value(1), Some("first\nsecond"));
        assert_eq!(quoted_value(2), Some("slash\\nsequence"));
        assert_eq!(quoted_value(3), Some("say \"ok\""));
        Ok(())
    }
    /// Verifies that rejects non schematic roots behaves as required.
    #[test]
    fn rejects_non_schematic_roots() {
        let result =
            SchematicDocument::from_text("board.kicad_pcb", "(kicad_pcb (version 20250101))");

        assert!(result.is_err());
    }
    /// Verifies that rejects malformed sexpressions behaves as required.
    #[test]
    fn rejects_malformed_sexpressions() {
        let result = SchematicDocument::from_text("broken.kicad_sch", "(kicad_sch (version");

        assert!(result.is_err());
    }
}
