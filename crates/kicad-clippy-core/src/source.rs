use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::{Deserialize, Serialize};

/// A half-open byte range in the original UTF-8 source.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct ByteSpan {
    /// First byte included in the span.
    pub start: usize,
    /// First byte after the span.
    pub end: usize,
}

impl ByteSpan {
    /// Creates a byte range. Validate it against a source before slicing.
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// Returns whether the range is empty.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// Returns whether this range is ordered, in bounds, and on UTF-8 boundaries.
    #[must_use]
    pub fn is_valid_for(self, source: &str) -> bool {
        self.start <= self.end
            && self.end <= source.len()
            && source.is_char_boundary(self.start)
            && source.is_char_boundary(self.end)
    }
}

/// Original source text and its file name.
///
/// Byte spans remain the canonical source location contract. Terminal line and
/// column rendering is delegated to the diagnostic renderer rather than
/// maintaining a second line index here.
#[derive(Clone, Debug)]
pub struct SourceFile {
    path: PathBuf,
    text: Arc<str>,
}

impl SourceFile {
    /// Retains the original source without constructing a duplicate line index.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, text: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            text: Arc::from(text.into()),
        }
    }

    /// Returns the source path supplied by the caller.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the original UTF-8 source text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Clones the shared source buffer without copying its contents.
    #[must_use]
    pub fn shared_text(&self) -> Arc<str> {
        Arc::clone(&self.text)
    }

    /// Returns the source slice covered by a valid byte span.
    #[must_use]
    pub fn slice(&self, span: ByteSpan) -> Option<&str> {
        if !span.is_valid_for(&self.text) {
            return None;
        }

        self.text.get(span.start..span.end)
    }
}

#[cfg(test)]
mod tests {
    use super::{ByteSpan, SourceFile};

    #[test]
    fn slices_only_valid_half_open_utf8_ranges() {
        let source = SourceFile::new("slice.kicad_sch", "café schematic");

        assert_eq!(source.slice(ByteSpan::new(0, 5)), Some("café"));
        assert_eq!(source.slice(ByteSpan::new(4, 8)), None);
        assert_eq!(source.slice(ByteSpan::new(9, 3)), None);
        assert_eq!(source.slice(ByteSpan::new(0, 99)), None);
    }
}
