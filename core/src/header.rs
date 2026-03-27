use std::io;

use crate::{Level, number_format};

pub const DEFAULT_HEADER_PATTERN: &str = "[{date} {time}.{ns}] {level} [{thread}] {location} - ";
pub const DEFAULT_HEADER_FIXED_LEN: usize = 42;
pub const LEVEL_THREAD_LOCATION_HEADER_PATTERN: &str = "{level} [{thread}] {location} - ";
pub const LEVEL_THREAD_LOCATION_HEADER_FIXED_LEN: usize = 10;

const NANOSECONDS_PER_SECOND: i64 = 1_000_000_000;
const SECONDS_PER_DAY: i64 = 86_400;
const NANOSECONDS_PER_DAY: i64 = SECONDS_PER_DAY * NANOSECONDS_PER_SECOND;
const DATE_WIDTH: usize = 10;
const TIME_WIDTH: usize = 8;
const NANOSECOND_WIDTH: usize = 9;

#[derive(Debug, Clone)]
pub struct HeaderDateCache {
    cached_date: [u8; DATE_WIDTH],
    cached_midnight_nanoseconds: i64,
}

impl Default for HeaderDateCache {
    fn default() -> Self {
        Self::new()
    }
}

impl HeaderDateCache {
    pub const fn new() -> Self {
        Self { cached_date: [b'0'; DATE_WIDTH], cached_midnight_nanoseconds: i64::MIN }
    }

    #[inline]
    fn ensure_date(&mut self, nanoseconds: i64) {
        let midnight_nanoseconds =
            nanoseconds.div_euclid(NANOSECONDS_PER_DAY) * NANOSECONDS_PER_DAY;
        if midnight_nanoseconds == self.cached_midnight_nanoseconds {
            return;
        }

        self.cached_midnight_nanoseconds = midnight_nanoseconds;
        let days_since_epoch = midnight_nanoseconds.div_euclid(NANOSECONDS_PER_DAY);
        let (year, month, day) = civil_from_days(days_since_epoch);
        write_four_digits(&mut self.cached_date[0..4], year as u32);
        self.cached_date[4] = b'-';
        write_two_digits(&mut self.cached_date[5..7], month);
        self.cached_date[7] = b'-';
        write_two_digits(&mut self.cached_date[8..10], day);
    }

    #[inline]
    fn bytes(&self) -> &[u8; DATE_WIDTH] {
        &self.cached_date
    }

    #[inline]
    pub fn append_date(&mut self, output: &mut Vec<u8>, nanoseconds: i64) {
        self.ensure_date(nanoseconds);
        output.extend_from_slice(self.bytes());
    }
}

pub type StaticHeaderWriteFn = fn(&mut HeaderDateCache, &mut Vec<u8>, i64, Level, &[u8], &str);

#[derive(Debug, Clone)]
pub struct StaticHeader {
    date_cache: HeaderDateCache,
    write: StaticHeaderWriteFn,
}

impl StaticHeader {
    pub const fn new(write: StaticHeaderWriteFn) -> Self {
        Self { date_cache: HeaderDateCache::new(), write }
    }

    #[inline]
    pub fn write_header(
        &mut self,
        output: &mut Vec<u8>,
        nanoseconds: i64,
        level: Level,
        thread_name: &[u8],
        location: &str,
    ) {
        (self.write)(&mut self.date_cache, output, nanoseconds, level, thread_name, location);
    }
}

/// Specialized writer for the built-in default header pattern.
#[derive(Debug, Clone)]
pub struct DefaultHeader {
    date_cache: HeaderDateCache,
}

impl Default for DefaultHeader {
    fn default() -> Self {
        Self::new()
    }
}

impl DefaultHeader {
    pub const fn new() -> Self {
        Self { date_cache: HeaderDateCache::new() }
    }

    #[inline]
    pub fn write_header(
        &mut self,
        output: &mut Vec<u8>,
        nanoseconds: i64,
        level: Level,
        thread_name: &[u8],
        location: &str,
    ) {
        self.date_cache.ensure_date(nanoseconds);
        output.reserve(DEFAULT_HEADER_FIXED_LEN + thread_name.len() + location.len());
        output.push(b'[');
        output.extend_from_slice(self.date_cache.bytes());
        output.push(b' ');
        append_header_time(output, nanoseconds);
        output.push(b'.');
        append_header_nanoseconds(output, nanoseconds);
        output.extend_from_slice(b"] ");
        output.extend_from_slice(level.as_bytes());
        output.extend_from_slice(b" [");
        output.extend_from_slice(thread_name);
        output.extend_from_slice(b"] ");
        output.extend_from_slice(location.as_bytes());
        output.extend_from_slice(b" - ");
    }
}

/// Specialized writer for a concise static header with no date/time fields.
#[derive(Debug, Clone, Copy, Default)]
pub struct LevelThreadLocationHeader;

impl LevelThreadLocationHeader {
    pub const fn new() -> Self {
        Self
    }

    #[inline]
    pub fn write_header(
        &mut self,
        output: &mut Vec<u8>,
        _nanoseconds: i64,
        level: Level,
        thread_name: &[u8],
        location: &str,
    ) {
        output.reserve(LEVEL_THREAD_LOCATION_HEADER_FIXED_LEN + thread_name.len() + location.len());
        output.extend_from_slice(level.as_bytes());
        output.extend_from_slice(b" [");
        output.extend_from_slice(thread_name);
        output.extend_from_slice(b"] ");
        output.extend_from_slice(location.as_bytes());
        output.extend_from_slice(b" - ");
    }
}

/// Represents a single token in a parsed header pattern.
#[derive(Debug, Clone, Copy)]
pub enum HeaderToken {
    /// A literal string segment stored at `[start..end]` in the pattern buffer.
    Literal { start: u32, end: u32 },
    /// The date field (YYYY-MM-DD).
    Date,
    /// The time field (HH:MM:SS).
    Time,
    /// The sub-second nanosecond field (9 digits).
    Nanosecond,
    /// The log level.
    Level,
    /// The thread name.
    ThreadName,
    /// The source location.
    Location,
}

/// A parsed and tokenized log header pattern, ready for efficient formatting.
#[derive(Debug, Clone)]
pub struct TokenizedHeader {
    pattern_buffer: Box<[u8]>,
    tokens: Vec<HeaderToken>,
    date_cache: HeaderDateCache,
}

impl TokenizedHeader {
    /// Creates a `TokenizedHeader` using the default pattern.
    pub fn default_pattern() -> Self {
        Self::parse_pattern(DEFAULT_HEADER_PATTERN).expect("default header pattern must be valid")
    }

    /// Parses a header pattern string into a `TokenizedHeader`.
    ///
    /// Supported placeholders: `{date}`, `{time}`, `{ns}`, `{level}`, `{thread}`, `{location}`.
    /// Use `{{` and `}}` for literal braces.
    pub fn parse_pattern(pattern: &str) -> io::Result<Self> {
        let mut pattern_buffer = Vec::new();
        let mut tokens = Vec::new();
        let bytes = pattern.as_bytes();
        let mut index = 0usize;
        let mut literal_start = 0usize;

        while index < bytes.len() {
            match bytes[index] {
                b'{' => {
                    if index + 1 < bytes.len() && bytes[index + 1] == b'{' {
                        index += 2;
                        continue;
                    }

                    if literal_start < index {
                        push_literal(
                            &mut pattern_buffer,
                            &mut tokens,
                            &pattern[literal_start..index],
                        )?;
                    }

                    let mut end = index + 1;
                    while end < bytes.len() && bytes[end] != b'}' {
                        end += 1;
                    }

                    if end >= bytes.len() {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "unclosed `{` in header pattern",
                        ));
                    }

                    let name = &pattern[index + 1..end];
                    tokens.push(parse_named_token(name)?);
                    index = end + 1;
                    literal_start = index;
                }
                b'}' => {
                    if index + 1 < bytes.len() && bytes[index + 1] == b'}' {
                        index += 2;
                    } else {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "stray `}` in header pattern",
                        ));
                    }
                }
                _ => index += 1,
            }
        }

        if literal_start < pattern.len() {
            push_literal(&mut pattern_buffer, &mut tokens, &pattern[literal_start..])?;
        }

        Ok(Self {
            pattern_buffer: pattern_buffer.into_boxed_slice(),
            tokens,
            date_cache: HeaderDateCache::new(),
        })
    }

    /// Writes the formatted header into the output buffer for a single log record.
    pub fn write_header(
        &mut self,
        output: &mut Vec<u8>,
        nanoseconds: i64,
        level: Level,
        thread_name: &[u8],
        location: &str,
    ) {
        output.reserve(self.pattern_buffer.len() + thread_name.len() + location.len() + 32);

        for index in 0..self.tokens.len() {
            match self.tokens[index] {
                HeaderToken::Literal { start, end } => {
                    output.extend_from_slice(&self.pattern_buffer[start as usize..end as usize]);
                }
                HeaderToken::Date => {
                    self.date_cache.append_date(output, nanoseconds);
                }
                HeaderToken::Time => append_header_time(output, nanoseconds),
                HeaderToken::Nanosecond => append_header_nanoseconds(output, nanoseconds),
                HeaderToken::Level => output.extend_from_slice(level.as_bytes()),
                HeaderToken::ThreadName => output.extend_from_slice(thread_name),
                HeaderToken::Location => output.extend_from_slice(location.as_bytes()),
            }
        }
    }
}

fn push_literal(
    pattern_buffer: &mut Vec<u8>,
    tokens: &mut Vec<HeaderToken>,
    literal: &str,
) -> io::Result<()> {
    if literal.is_empty() {
        return Ok(());
    }

    let start = pattern_buffer.len();
    pattern_buffer.extend_from_slice(unescape_braces(literal).as_bytes());
    let end = pattern_buffer.len();
    tokens.push(HeaderToken::Literal {
        start: u32::try_from(start).map_err(|_| io::Error::other("header literal too large"))?,
        end: u32::try_from(end).map_err(|_| io::Error::other("header literal too large"))?,
    });
    Ok(())
}

fn parse_named_token(name: &str) -> io::Result<HeaderToken> {
    match name {
        "date" => Ok(HeaderToken::Date),
        "time" => Ok(HeaderToken::Time),
        "ns" => Ok(HeaderToken::Nanosecond),
        "level" => Ok(HeaderToken::Level),
        "thread" => Ok(HeaderToken::ThreadName),
        "location" => Ok(HeaderToken::Location),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unknown header token: {{{name}}}"),
        )),
    }
}

fn unescape_braces(literal: &str) -> String {
    let mut output = String::with_capacity(literal.len());
    let mut chars = literal.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                output.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                output.push('}');
            }
            _ => output.push(ch),
        }
    }
    output
}

fn nanoseconds_to_time(nanoseconds: i64) -> (u32, u32, u32) {
    let total_seconds = nanoseconds.div_euclid(NANOSECONDS_PER_SECOND);
    let second_of_day = total_seconds.rem_euclid(SECONDS_PER_DAY) as u32;
    let hour = second_of_day / 3600;
    let minute = (second_of_day % 3600) / 60;
    let second = second_of_day % 60;
    (hour, minute, second)
}

#[inline]
pub fn append_header_time(output: &mut Vec<u8>, nanoseconds: i64) {
    let (hour, minute, second) = nanoseconds_to_time(nanoseconds);
    let mut buffer = [0u8; TIME_WIDTH];
    write_two_digits(&mut buffer[0..2], hour);
    buffer[2] = b':';
    write_two_digits(&mut buffer[3..5], minute);
    buffer[5] = b':';
    write_two_digits(&mut buffer[6..8], second);
    output.extend_from_slice(&buffer);
}

#[inline]
pub fn append_header_nanoseconds(output: &mut Vec<u8>, nanoseconds: i64) {
    let mut buffer = [0u8; NANOSECOND_WIDTH];
    let nanosecond_part = nanoseconds.rem_euclid(NANOSECONDS_PER_SECOND) as u64;
    number_format::format_fixed_width::<NANOSECOND_WIDTH>(&mut buffer, nanosecond_part);
    output.extend_from_slice(&buffer);
}

fn write_two_digits(buffer: &mut [u8], value: u32) {
    debug_assert_eq!(buffer.len(), 2);
    let index = (value as usize) << 1;
    buffer[0] = number_format::DIGIT_PAIRS[index];
    buffer[1] = number_format::DIGIT_PAIRS[index + 1];
}

fn write_four_digits(buffer: &mut [u8], value: u32) {
    debug_assert_eq!(buffer.len(), 4);
    let high = ((value / 100) as usize) << 1;
    let low = ((value % 100) as usize) << 1;
    buffer[0] = number_format::DIGIT_PAIRS[high];
    buffer[1] = number_format::DIGIT_PAIRS[high + 1];
    buffer[2] = number_format::DIGIT_PAIRS[low];
    buffer[3] = number_format::DIGIT_PAIRS[low + 1];
}

fn civil_from_days(days_since_epoch: i64) -> (i32, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if month <= 2 { 1 } else { 0 };
    (year as i32, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_write_header_tokens() {
        let mut header =
            TokenizedHeader::parse_pattern("{level}|{thread}|{location}|{date} {time}.{ns}|")
                .unwrap();
        let mut output = Vec::new();
        header.write_header(
            &mut output,
            1_744_000_123_456_789_123,
            Level::INF,
            b"worker-1",
            "src/main.rs:10",
        );
        let string = String::from_utf8(output).unwrap();
        assert!(string.contains("INF|worker-1|src/main.rs:10|"));
        assert!(string.contains(".456789123|"));
    }

    #[test]
    fn default_header_specialization_matches_tokenized_default_pattern() {
        let mut tokenized = TokenizedHeader::default_pattern();
        let mut specialized = DefaultHeader::new();
        let mut tokenized_output = Vec::new();
        let mut specialized_output = Vec::new();
        let timestamp = 1_744_000_123_456_789_123;

        tokenized.write_header(
            &mut tokenized_output,
            timestamp,
            Level::INF,
            b"worker-1",
            "src/main.rs:10",
        );
        specialized.write_header(
            &mut specialized_output,
            timestamp,
            Level::INF,
            b"worker-1",
            "src/main.rs:10",
        );

        assert_eq!(specialized_output, tokenized_output);
    }

    #[test]
    fn concise_header_specialization_matches_tokenized_pattern() {
        let mut tokenized =
            TokenizedHeader::parse_pattern(LEVEL_THREAD_LOCATION_HEADER_PATTERN).unwrap();
        let mut specialized = LevelThreadLocationHeader::new();
        let mut tokenized_output = Vec::new();
        let mut specialized_output = Vec::new();
        let timestamp = 1_744_000_123_456_789_123;

        tokenized.write_header(
            &mut tokenized_output,
            timestamp,
            Level::INF,
            b"worker-1",
            "src/main.rs:10",
        );
        specialized.write_header(
            &mut specialized_output,
            timestamp,
            Level::INF,
            b"worker-1",
            "src/main.rs:10",
        );

        assert_eq!(specialized_output, tokenized_output);
    }
}
