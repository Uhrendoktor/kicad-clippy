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
    source: SourceFile,
    cst: CstDocument,
}

impl SchematicDocument {
    /// Reads and parses a schematic from disk.
    ///
    /// # Errors
    ///
    /// Returns [`SchematicParseError`] when the file cannot be read, parsed, or validated as a schematic.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, SchematicParseError> {
        let path = path.as_ref().to_path_buf();
        let text = fs::read_to_string(&path).map_err(|source| SchematicParseError::Io {
            path: path.clone(),
            source,
        })?;

        Self::from_text(path, text)
    }

    /// Parses schematic text while retaining the caller-supplied path for diagnostics.
    ///
    /// # Errors
    ///
    /// Returns [`SchematicParseError::Syntax`] for malformed input or [`SchematicParseError::WrongRoot`] for a non-schematic root.
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

    /// Returns the original source and its precomputed line index.
    #[must_use]
    pub const fn source(&self) -> &SourceFile {
        &self.source
    }

    /// Returns the lossless concrete syntax tree.
    #[must_use]
    pub const fn cst(&self) -> &CstDocument {
        &self.cst
    }

    /// Returns the byte span of the top-level schematic expression.
    #[must_use]
    pub fn root_span(&self) -> Option<ByteSpan> {
        match self.cst.nodes.first() {
            Some(Node::List { span, .. }) => Some(ByteSpan::new(span.start, span.end)),
            _ => None,
        }
    }
}

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
mod tests {
    use std::error::Error;

    use super::SchematicDocument;
    use kicad_clippy_core::ByteSpan;

    const SCHEMATIC: &str =
        "(kicad_sch (version 20250101) (generator eeschema) (uuid \"root-id\"))";

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

    #[test]
    fn rejects_non_schematic_roots() {
        let result =
            SchematicDocument::from_text("board.kicad_pcb", "(kicad_pcb (version 20250101))");

        assert!(result.is_err());
    }

    #[test]
    fn rejects_malformed_sexpressions() {
        let result = SchematicDocument::from_text("broken.kicad_sch", "(kicad_sch (version");

        assert!(result.is_err());
    }
}
