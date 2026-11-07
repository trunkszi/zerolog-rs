#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::io;

use uarch::prelude::{cold_path, cold_sink, hot_inline, likely, unlikely};

use crate::{Level, number_format};

pub const DEFAULT_HEADER_PATTERN: &str = "[{date} {time}.{ns}] {level} [{thread}] {location} - ";
pub const DEFAULT_HEADER_FIXED_LEN: usize = 42;
pub const LEVEL_THREAD_LOCATION_HEADER_PATTERN: &str = "{level} [{thread}] {location} - ";
pub const LEVEL_THREAD_LOCATION_HEADER_FIXED_LEN: usize = 10;

const NANOSECONDS_PER_SECOND: i64 = 1_000_000_000;
const NANOSECONDS_PER_SECOND_U: u64 = 1_000_000_000;
const SECONDS_PER_DAY: i64 = 86_400;
const SECONDS_PER_DAY_U: u64 = 86_400;
const DATE_WIDTH: usize = 10;
const TIME_WIDTH: usize = 8;
const NANOSECOND_WIDTH: usize = 9;

#[derive(Debug, Clone)]
pub struct HeaderDateCache {
    cached_second_epoch: i64,
    cached_days: i64,
    cached_date: [u8; DATE_WIDTH],
    cached_hms: [u8; TIME_WIDTH],
}

impl Default for HeaderDateCache {
    fn default() -> Self {
        Self::new()
    }
}

impl HeaderDateCache {
    pub const fn new() -> Self {
        Self {
            cached_second_epoch: i64::MIN,
            cached_days: i64::MIN,
            cached_date: [b'0'; DATE_WIDTH],
            cached_hms: *b"00:00:00",
        }
    }

    /// Returns the nanosecond residue and refreshes the civil-time cache.
    ///
    /// A hit copies the stored `YYYY-MM-DD` and `HH:MM:SS` bytes. A miss, and
    /// every pre-1970 stamp, rebuilds those bytes with Euclidean division so
    /// the residue stays in `0..1_000_000_000`.
    #[hot_inline]
    fn prepare(&mut self, nanoseconds: i64) -> u64 {
        if unlikely(nanoseconds < 0) {
            cold_path();
            return self.prepare_negative(nanoseconds);
        }
        let ticks = nanoseconds as u64;
        let total_seconds = (ticks / NANOSECONDS_PER_SECOND_U) as i64;
        let nanos = ticks % NANOSECONDS_PER_SECOND_U;
        if likely(total_seconds == self.cached_second_epoch) {
            return nanos;
        }
        cold_path();
        self.store_second(total_seconds);
        nanos
    }

    #[cold_sink]
    fn prepare_negative(&mut self, nanoseconds: i64) -> u64 {
        let total_seconds = nanoseconds.div_euclid(NANOSECONDS_PER_SECOND);
        if unlikely(total_seconds != self.cached_second_epoch) {
            self.store_second(total_seconds);
        }
        nanoseconds.rem_euclid(NANOSECONDS_PER_SECOND) as u64
    }

    #[cold_sink]
    fn store_second(&mut self, total_seconds: i64) {
        let days = total_seconds.div_euclid(SECONDS_PER_DAY);
        if days != self.cached_days {
            self.cached_days = days;
            let (year, month, day) = civil_from_days(days);
            write_four_digits(&mut self.cached_date[0..4], year as u32);
            self.cached_date[4] = b'-';
            write_two_digits(&mut self.cached_date[5..7], month);
            self.cached_date[7] = b'-';
            write_two_digits(&mut self.cached_date[8..10], day);
        }
        let second_of_day = total_seconds.rem_euclid(SECONDS_PER_DAY) as u32;
        write_hms(&mut self.cached_hms, second_of_day);
        self.cached_second_epoch = total_seconds;
    }

    #[hot_inline]
    pub fn append_date(&mut self, output: &mut Vec<u8>, nanoseconds: i64) {
        let _nanos = self.prepare(nanoseconds);
        output.extend_from_slice(&self.cached_date);
    }

    #[doc(hidden)]
    #[hot_inline]
    pub fn append_time(&mut self, nanoseconds: i64, out: &mut Vec<u8>) {
        let _nanos = self.prepare(nanoseconds);
        out.extend_from_slice(&self.cached_hms);
    }

    #[doc(hidden)]
    #[hot_inline]
    pub fn append_nanoseconds(&mut self, nanoseconds: i64, out: &mut Vec<u8>) {
        append_nanosecond_digits(out, self.prepare(nanoseconds));
    }
}

/// Appends a span id to `output`, using `-` for the root/no-span id.
#[inline]
pub fn append_span_id(output: &mut Vec<u8>, span_id: u64) {
    if span_id == 0 {
        output.push(b'-');
        return;
    }

    let mut buffer = [0u8; 20];
    let len = number_format::format_u64(&mut buffer, span_id);
    output.extend_from_slice(&buffer[..len]);
}

pub type StaticHeaderWriteFn = fn(&mut HeaderDateCache, &mut Vec<u8>, i64, Level, &[u8], &str, u64);

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
        span_id: u64,
    ) {
        (self.write)(
            &mut self.date_cache,
            output,
            nanoseconds,
            level,
            thread_name,
            location,
            span_id,
        );
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
        _span_id: u64,
    ) {
        let nanos = self.date_cache.prepare(nanoseconds);
        output.reserve(DEFAULT_HEADER_FIXED_LEN + thread_name.len() + location.len());
        output.push(b'[');
        output.extend_from_slice(&self.date_cache.cached_date);
        output.push(b' ');
        output.extend_from_slice(&self.date_cache.cached_hms);
        output.push(b'.');
        append_nanosecond_digits(output, nanos);
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
        _span_id: u64,
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
    /// The current span id (`-` when no span is active).
    SpanId,
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
        match Self::parse_pattern(DEFAULT_HEADER_PATTERN) {
            Ok(header) => header,
            Err(error) => panic!("default header pattern must be valid: {error}"),
        }
    }

    /// Parses a header pattern string into a `TokenizedHeader`.
    ///
    /// Supported placeholders: `{date}`, `{time}`, `{ns}`, `{level}`, `{span}`, `{thread}`,
    /// `{location}`. Use `{{` and `}}` for literal braces.
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
        span_id: u64,
    ) {
        output.reserve(self.pattern_buffer.len() + thread_name.len() + location.len() + 52);

        for index in 0..self.tokens.len() {
            match self.tokens[index] {
                HeaderToken::Literal { start, end } => {
                    output.extend_from_slice(&self.pattern_buffer[start as usize..end as usize]);
                }
                HeaderToken::Date => self.date_cache.append_date(output, nanoseconds),
                HeaderToken::Time => self.date_cache.append_time(nanoseconds, output),
                HeaderToken::Nanosecond => self.date_cache.append_nanoseconds(nanoseconds, output),
                HeaderToken::Level => output.extend_from_slice(level.as_bytes()),
                HeaderToken::SpanId => append_span_id(output, span_id),
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
        "span" => Ok(HeaderToken::SpanId),
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

fn write_hms(buffer: &mut [u8], second_of_day: u32) {
    let hour = second_of_day / 3_600;
    let minute = (second_of_day % 3_600) / 60;
    let second = second_of_day % 60;
    write_two_digits(&mut buffer[0..2], hour);
    buffer[2] = b':';
    write_two_digits(&mut buffer[3..5], minute);
    buffer[5] = b':';
    write_two_digits(&mut buffer[6..8], second);
}

#[inline]
pub fn append_header_time(output: &mut Vec<u8>, nanoseconds: i64) {
    let mut buffer = [0u8; TIME_WIDTH];
    let second_of_day = if likely(nanoseconds >= 0) {
        let total_seconds = (nanoseconds as u64) / NANOSECONDS_PER_SECOND_U;
        (total_seconds % SECONDS_PER_DAY_U) as u32
    } else {
        cold_path();
        negative_second_of_day(nanoseconds)
    };
    write_hms(&mut buffer, second_of_day);
    output.extend_from_slice(&buffer);
}

#[cold_sink]
fn negative_second_of_day(nanoseconds: i64) -> u32 {
    nanoseconds.div_euclid(NANOSECONDS_PER_SECOND).rem_euclid(SECONDS_PER_DAY) as u32
}

#[hot_inline]
fn append_nanosecond_digits(output: &mut Vec<u8>, nanos: u64) {
    let mut buffer = [0u8; NANOSECOND_WIDTH];
    number_format::format_fixed_width::<NANOSECOND_WIDTH>(&mut buffer, nanos);
    output.extend_from_slice(&buffer);
}

#[hot_inline]
pub fn append_header_nanoseconds(output: &mut Vec<u8>, nanoseconds: i64) {
    let nanos = if likely(nanoseconds >= 0) {
        (nanoseconds as u64) % NANOSECONDS_PER_SECOND_U
    } else {
        cold_path();
        negative_nanosecond_of_second(nanoseconds)
    };
    append_nanosecond_digits(output, nanos);
}

#[cold_sink]
fn negative_nanosecond_of_second(nanoseconds: i64) -> u64 {
    nanoseconds.rem_euclid(NANOSECONDS_PER_SECOND) as u64
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
#[allow(clippy::unwrap_used, clippy::expect_used)]
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
            0,
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
            0,
        );
        specialized.write_header(
            &mut specialized_output,
            timestamp,
            Level::INF,
            b"worker-1",
            "src/main.rs:10",
            0,
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
            0,
        );
        specialized.write_header(
            &mut specialized_output,
            timestamp,
            Level::INF,
            b"worker-1",
            "src/main.rs:10",
            0,
        );

        assert_eq!(specialized_output, tokenized_output);
    }

    #[test]
    fn span_header_token_renders_dash_for_root_and_decimal_for_span() {
        let mut header = TokenizedHeader::parse_pattern("{level}|span={span}|").unwrap();
        let mut root = Vec::new();
        header.write_header(&mut root, 0, Level::TRC, b"worker-1", "src/main.rs:10", 0);
        assert_eq!(String::from_utf8(root).unwrap(), "TRC|span=-|");

        let mut span = Vec::new();
        header.write_header(&mut span, 0, Level::TRC, b"worker-1", "src/main.rs:10", 42);
        assert_eq!(String::from_utf8(span).unwrap(), "TRC|span=42|");
    }

    #[test]
    fn cached_clock_matches_civil_time() {
        let mut header = TokenizedHeader::parse_pattern("{date} {time}.{ns}").unwrap();
        let samples: &[(i64, &str)] = &[
            (0, "1970-01-01 00:00:00.000000000"),
            (999_999_999, "1970-01-01 00:00:00.999999999"),
            (86_399_000_000_000, "1970-01-01 23:59:59.000000000"),
            (86_400_000_000_000, "1970-01-02 00:00:00.000000000"),
            (-1, "1969-12-31 23:59:59.999999999"),
            (-86_400_000_000_000, "1969-12-31 00:00:00.000000000"),
            (-57_974_400_000_000_001, "1968-02-29 23:59:59.999999999"),
            (1_582_979_696_000_000_007, "2020-02-29 12:34:56.000000007"),
            (1_583_020_800_000_000_000, "2020-03-01 00:00:00.000000000"),
            (1_744_000_123_456_789_123, "2025-04-07 04:28:43.456789123"),
            (1_744_000_123_456_789_124, "2025-04-07 04:28:43.456789124"),
            (1_744_000_124_000_000_001, "2025-04-07 04:28:44.000000001"),
        ];
        for (nanoseconds, expected) in samples {
            let mut output = Vec::new();
            header.write_header(&mut output, *nanoseconds, Level::INF, b"", "", 0);
            assert_eq!(output, expected.as_bytes(), "ns={nanoseconds}");
        }
    }

    #[test]
    fn pre_epoch_fields_stay_euclidean() {
        let mut time = Vec::new();
        append_header_time(&mut time, -1);
        assert_eq!(time, b"23:59:59");
        let mut nanos = Vec::new();
        append_header_nanoseconds(&mut nanos, -1);
        assert_eq!(nanos, b"999999999");
        let mut midnight = Vec::new();
        append_header_time(&mut midnight, -86_400_000_000_000);
        assert_eq!(midnight, b"00:00:00");
    }

    #[test]
    fn negative_header_matches_tokenized() {
        let mut tokenized = TokenizedHeader::default_pattern();
        let mut specialized = DefaultHeader::new();
        let mut tokenized_output = Vec::new();
        let mut specialized_output = Vec::new();
        let timestamp = -1;

        tokenized.write_header(
            &mut tokenized_output,
            timestamp,
            Level::INF,
            b"worker-1",
            "src/main.rs:10",
            0,
        );
        specialized.write_header(
            &mut specialized_output,
            timestamp,
            Level::INF,
            b"worker-1",
            "src/main.rs:10",
            0,
        );

        assert_eq!(specialized_output, tokenized_output);
    }
}
