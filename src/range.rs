#[derive(Clone, Copy)]
pub(crate) struct ByteRange {
    start: u64,
    end: u64,
}

pub(crate) enum ParsedRange {
    Selected(ByteRange),
    UnsupportedUnit,
    Invalid,
}

impl ByteRange {
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

    pub(crate) fn start(self) -> u64 {
        self.start
    }

    pub(crate) fn end(self) -> u64 {
        self.end
    }

    pub(crate) fn length(self) -> u64 {
        self.end - self.start + 1
    }
}
