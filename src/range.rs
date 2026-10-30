//! Framework-independent parsing for a single HTTP byte range.
//!
//! Keeping this module free of Axum, Hyper, filesystem, and socket types makes
//! the range rules reusable by a future storage layer and easy to test in
//! isolation.

/// A selected inclusive byte interval within a representation.
#[derive(Clone, Copy)]
pub(crate) struct ByteRange {
    /// Zero-based offset of the first selected byte.
    start: u64,
    /// Zero-based offset of the last selected byte, inclusive.
    end: u64,
}

/// The application-level outcomes of parsing a `Range` header.
pub(crate) enum ParsedRange {
    /// A valid, satisfiable range that can be served.
    Selected(ByteRange),
    /// A syntactically recognizable range whose unit is not `bytes`.
    ///
    /// The HTTP adapter ignores unsupported units and serves the complete
    /// representation.
    UnsupportedUnit,
    /// A malformed, multiple, empty, or unsatisfiable byte range.
    Invalid,
}

impl ByteRange {
    /// Parses one range value against a representation of `total_length` bytes.
    ///
    /// Supported forms are `bytes=START-END`, `bytes=START-`, and
    /// `bytes=-SUFFIX_LENGTH`. Explicit end offsets beyond the representation
    /// are clamped to its final byte. Multiple ranges are intentionally
    /// unsupported.
    pub(crate) fn parse(value: &str, total_length: u64) -> ParsedRange {
        let Some((unit, value)) = value.split_once('=') else {
            return ParsedRange::Invalid;
        };
        if !unit.eq_ignore_ascii_case("bytes") {
            return ParsedRange::UnsupportedUnit;
        }
        if value.contains(',') {
            return ParsedRange::Invalid;
        }
        let Some((start, end)) = value.split_once('-') else {
            return ParsedRange::Invalid;
        };

        if start.is_empty() {
            let Ok(suffix_length) = end.parse::<u64>() else {
                return ParsedRange::Invalid;
            };
            if suffix_length == 0 || total_length == 0 {
                return ParsedRange::Invalid;
            }
            let start = total_length.saturating_sub(suffix_length);
            return ParsedRange::Selected(Self {
                start,
                end: total_length - 1,
            });
        }

        let Ok(start) = start.parse::<u64>() else {
            return ParsedRange::Invalid;
        };
        if start >= total_length {
            return ParsedRange::Invalid;
        }
        let end = if end.is_empty() {
            total_length - 1
        } else {
            let Ok(end) = end.parse::<u64>() else {
                return ParsedRange::Invalid;
            };
            end.min(total_length - 1)
        };
        if end < start {
            return ParsedRange::Invalid;
        }

        ParsedRange::Selected(Self { start, end })
    }

    /// Returns the inclusive starting offset.
    pub(crate) fn start(self) -> u64 {
        self.start
    }

    /// Returns the inclusive ending offset.
    pub(crate) fn end(self) -> u64 {
        self.end
    }

    /// Returns the number of selected bytes.
    ///
    /// Both endpoints are inclusive, hence the additional byte in the formula.
    pub(crate) fn length(self) -> u64 {
        self.end - self.start + 1
    }
}
