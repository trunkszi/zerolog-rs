#![feature(min_specialization, thread_local, core_intrinsics)]
#![allow(internal_features)]

pub mod builder;
pub mod encode;
pub mod header;
pub mod logger;
pub mod number_format;
pub mod output;
pub mod poller;
pub mod spsc_queue;
pub mod thread_buffer;
pub mod timestamp_counter;

// User-facing re-exports
pub use builder::{FlashLogBuilder, FlashLogGuard, builder};
// Proc-macro internals -- not part of the public API surface.
#[doc(hidden)]
pub use encode::{
    ENCODED_SIZE_DYNAMIC, EncodeBinary, EncodeConstBinaryFormat, EncodeConstDefaultFormat,
    EncodeConstFloatFormat, EncodeConstLowerHexFormat, EncodeConstOctalFormat,
    EncodeConstUpperHexFormat, EncodeLowerHex, EncodeOctal, EncodeUpperHex, FORMAT_ALIGN_CENTER,
    FORMAT_ALIGN_LEFT, FORMAT_ALIGN_NONE, FORMAT_ALIGN_RIGHT, FORMAT_PRECISION_NONE, VecWriter,
    append_display_const_default, append_display_with_spec,
};
pub use encode::{Encode, EncodeFast};
#[doc(hidden)]
pub use header::{
    HeaderDateCache, StaticHeader, StaticHeaderWriteFn, append_header_nanoseconds,
    append_header_time,
};
#[doc(hidden)]
pub use logger::{DecodeFn, StaticInfo, check_level, set_static_header};
pub use logger::{
    QueueFullCallback, QueueFullPolicy, flush_on, poll, queue_full_count, reset_queue_full_count,
    set_flush_buffer_size, set_flush_delay, set_header_pattern, set_level, set_log_file,
    set_log_file_with_rotation, set_output_writer, set_queue_full_callback, set_queue_full_policy,
    start_polling_thread, start_polling_thread_on_core, stop_polling_thread,
};
pub use output::RotationConfig;
#[doc(hidden)]
pub use spsc_queue::{
    PAYLOAD_ARGS_OFFSET, PAYLOAD_HEADER_SIZE, PAYLOAD_INFO_OFFSET, PAYLOAD_TIMESTAMP_OFFSET,
};
#[doc(hidden)]
pub use thread_buffer::alloc_message;
pub use thread_buffer::set_thread_name;
#[doc(hidden)]
pub use timestamp_counter::TimestampCounter;

/// Log severity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Level {
    /// Debug level.
    DBG = 0,
    /// Info level.
    INF = 1,
    /// Warning level.
    WRN = 2,
    /// Error level.
    ERR = 3,
    /// Logging disabled.
    OFF = 4,
}

impl Level {
    /// Returns the level name as a byte slice (e.g., `b"INF"`).
    #[inline]
    pub fn as_bytes(&self) -> &'static [u8] {
        match self {
            Level::DBG => b"DBG",
            Level::INF => b"INF",
            Level::WRN => b"WRN",
            Level::ERR => b"ERR",
            Level::OFF => b"OFF",
        }
    }
}

/// Converts a `u8` to a [`Level`]. Values outside the valid range map to [`Level::OFF`].
#[inline]
pub fn level_from_u8(value: u8) -> Level {
    match value {
        0 => Level::DBG,
        1 => Level::INF,
        2 => Level::WRN,
        3 => Level::ERR,
        _ => Level::OFF,
    }
}

/// Shorthand constant for [`Level::DBG`].
pub const DBG: Level = Level::DBG;
/// Shorthand constant for [`Level::INF`].
pub const INF: Level = Level::INF;
/// Shorthand constant for [`Level::WRN`].
pub const WRN: Level = Level::WRN;
/// Shorthand constant for [`Level::ERR`].
pub const ERR: Level = Level::ERR;
/// Shorthand constant for [`Level::OFF`].
pub const OFF: Level = Level::OFF;

/// Alias for [`Level::DBG`].
pub const DEBUG: Level = Level::DBG;
/// Alias for [`Level::INF`].
pub const INFO: Level = Level::INF;
/// Alias for [`Level::WRN`].
pub const WARN: Level = Level::WRN;
/// Alias for [`Level::ERR`].
pub const ERROR: Level = Level::ERR;
