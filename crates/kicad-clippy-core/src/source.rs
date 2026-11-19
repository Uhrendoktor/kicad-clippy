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
    /// Stores the caller's path and original text, sharing the text buffer through `Arc`.
    ///
    /// # Arguments
    ///
    /// * `start` — First byte offset included in the half-open source range.
    /// * `end` — First byte offset after the half-open source range.
    ///
    /// # Returns
    ///
    /// A byte span with the supplied start and end offsets.
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// Reports whether the span contains no bytes.
    ///
    /// # Returns
    ///
    /// `true` when `start` equals `end`.
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
    /// Filesystem path associated with the original source text.
    path: PathBuf,
    /// Reference-counted original UTF-8 buffer used to resolve byte spans.
    text: Arc<str>,
}

impl SourceFile {
    /// Stores the caller's path and original text, sharing the text buffer through `Arc`.
    ///
    /// # Arguments
    ///
    /// * `path` — Path associated with the source text.
    /// * `text` — Original UTF-8 source text; its byte positions are used by `ByteSpan`.
    ///
    /// # Returns
    ///
    /// A byte span with the supplied start and end offsets.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, text: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            text: Arc::from(text.into()),
        }
    }

    /// Returns the path associated with this source buffer.
    ///
    /// # Returns
    ///
    /// A borrowed path to the original source file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Borrows the original UTF-8 source text without allocating a copy.
    ///
    /// # Returns
    ///
    /// A borrowed view of the original UTF-8 text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Clones the reference-counted source buffer without duplicating its contents.
    ///
    /// # Returns
    ///
    /// A cloned `Arc<str>` pointing to the same source allocation.
    #[must_use]
    pub fn shared_text(&self) -> Arc<str> {
        Arc::clone(&self.text)
    }

    /// Returns the source substring only when both offsets are valid UTF-8 boundaries and the range lies within the source.
    ///
    /// # Arguments
    ///
    /// * `span` — Half-open byte range to slice from the original source.
    ///
    /// # Returns
    ///
    /// The requested substring when the span is in bounds and aligned to UTF-8 character boundaries; otherwise `None`.
    #[must_use]
    pub fn slice(&self, span: ByteSpan) -> Option<&str> {
        self.text.get(span.start..span.end)
    }
}
#[cfg(test)]
/// Unit tests for half-open byte spans and UTF-8-safe source slicing.
mod tests {
    use super::{ByteSpan, SourceFile};
    /// Verifies that slices only valid half open utf8 ranges behaves as required.
    #[test]
    fn slices_only_valid_half_open_utf8_ranges() {
        let source = SourceFile::new("slice.kicad_sch", "café schematic");

        assert_eq!(source.slice(ByteSpan::new(0, 5)), Some("café"));
        assert_eq!(source.slice(ByteSpan::new(4, 8)), None);
        assert_eq!(source.slice(ByteSpan::new(9, 3)), None);
        assert_eq!(source.slice(ByteSpan::new(0, 99)), None);
    }
}
