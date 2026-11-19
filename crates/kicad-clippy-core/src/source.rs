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
    /// # Arguments
    ///
    /// * `start` — Input `start` used by this operation.
    /// * `end` — Input `end` used by this operation.
    ///
    /// # Returns
    ///
    /// The value produced by this operation.
    ///
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// Returns whether the range is empty.
    /// # Returns
    ///
    /// The value produced by this operation.
    ///
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }
}

/// Original source text and its file name.
///
/// Byte spans remain the canonical source location contract. Terminal line and
/// column rendering is delegated to the diagnostic renderer rather than
/// maintaining a second line index here.
#[derive(Clone, Debug)]
pub struct SourceFile {
    /// Stores the `path` value.
    path: PathBuf,
    /// Stores the `text` value.
    text: Arc<str>,
}

impl SourceFile {
    /// Retains the original source without constructing a duplicate line index.
    /// # Arguments
    ///
    /// * `path` — Input `path` used by this operation.
    /// * `text` — Input `text` used by this operation.
    ///
    /// # Returns
    ///
    /// The value produced by this operation.
    ///
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, text: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            text: Arc::from(text.into()),
        }
    }

    /// Returns the source path supplied by the caller.
    /// # Returns
    ///
    /// The value produced by this operation.
    ///
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the original UTF-8 source text.
    /// # Returns
    ///
    /// The value produced by this operation.
    ///
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Clones the shared source buffer without copying its contents.
    /// # Returns
    ///
    /// The value produced by this operation.
    ///
    #[must_use]
    pub fn shared_text(&self) -> Arc<str> {
        Arc::clone(&self.text)
    }

    /// Returns the source slice covered by a valid byte span.
    /// # Arguments
    ///
    /// * `span` — Input `span` used by this operation.
    ///
    /// # Returns
    ///
    /// The value produced by this operation.
    ///
    #[must_use]
    pub fn slice(&self, span: ByteSpan) -> Option<&str> {
        self.text.get(span.start..span.end)
    }
}
#[cfg(test)]
/// Contains the tests implementation and supporting types.
mod tests {
    use super::{ByteSpan, SourceFile};
    /// Processes slices only valid half open utf8 ranges using the supplied values and schematic data.
    ///
    #[test]
    fn slices_only_valid_half_open_utf8_ranges() {
        let source = SourceFile::new("slice.kicad_sch", "café schematic");

        assert_eq!(source.slice(ByteSpan::new(0, 5)), Some("café"));
        assert_eq!(source.slice(ByteSpan::new(4, 8)), None);
        assert_eq!(source.slice(ByteSpan::new(9, 3)), None);
        assert_eq!(source.slice(ByteSpan::new(0, 99)), None);
    }
}
