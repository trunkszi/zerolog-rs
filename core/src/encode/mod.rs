pub mod format;
pub mod primitives;
pub mod strings;
pub mod traits;

pub const ENCODED_SIZE_DYNAMIC: usize = usize::MAX;

pub const FORMAT_ALIGN_NONE: u8 = 0;
pub const FORMAT_ALIGN_LEFT: u8 = 1;
pub const FORMAT_ALIGN_RIGHT: u8 = 2;
pub const FORMAT_ALIGN_CENTER: u8 = 3;
pub const FORMAT_PRECISION_NONE: usize = usize::MAX;

/// Trait that must be implemented by all types that can be written to the log queue.
///
/// Built-in types are implemented by the library; custom types can derive it
/// via `#[derive(Encode)]`.
///
/// ## Three-layer type path
///
/// - **Layer 1 (Fast Primitive)**: i32/u64/f64/bool, etc. -- dedicated number_format
/// - **Layer 2 (Fast String)**: &str/String -- length-prefixed encoding
/// - **Layer 3 (General Display)**: user-defined types -- Display fallback
pub trait Encode {
    /// Compile-time hint for encoded byte width. Dynamic-width types keep the sentinel value.
    const ENCODED_SIZE_HINT: usize = ENCODED_SIZE_DYNAMIC;

    /// Returns the number of bytes needed to encode this value into the queue.
    fn encoded_size(&self) -> usize;

    /// Encodes the value at `buffer + offset` (hot path).
    ///
    /// # Safety
    /// `buffer + offset` must have sufficient space (guaranteed by `alloc_message`).
    unsafe fn encode(&self, buffer: *mut u8, offset: &mut usize);

    /// Encodes the value at a fixed byte offset from `buffer`.
    ///
    /// The default implementation forwards to [`Encode::encode`] using a local
    /// offset variable. Fixed-width types can override this to perform a direct
    /// write at a compile-time-known payload position.
    ///
    /// # Safety
    /// `buffer + offset` must have sufficient space for the encoded value.
    #[inline(always)]
    unsafe fn encode_at_offset(&self, buffer: *mut u8, offset: usize) {
        let mut offset = offset;
        unsafe {
            <Self as Encode>::encode(self, buffer, &mut offset);
        }
    }

    /// Decodes from `buffer + offset` and appends the formatted result to `output` (cold path).
    ///
    /// `format_spec` is a format specifier such as `".4"` or `"#x"`;
    /// an empty string means the default format.
    ///
    /// # Safety
    /// `buffer + offset` must point to valid encoded data.
    unsafe fn decode_and_format(
        buffer: *const u8,
        offset: &mut usize,
        output: &mut Vec<u8>,
        format_spec: &str,
    );

    /// Decodes from `buffer + offset` and appends the formatted result.
    ///
    /// The default implementation forwards to [`Encode::decode_and_format`]
    /// using a local mutable offset. Fixed-width types can override this to
    /// perform a direct read at a compile-time-known payload position.
    ///
    /// # Safety
    /// `buffer + offset` must point to valid encoded data.
    #[inline(always)]
    unsafe fn decode_and_format_at_offset(
        buffer: *const u8,
        offset: usize,
        output: &mut Vec<u8>,
        format_spec: &str,
    ) {
        let mut offset = offset;
        unsafe {
            <Self as Encode>::decode_and_format(buffer, &mut offset, output, format_spec);
        }
    }
}

/// Extreme-performance encoding path.
///
/// Currently selected explicitly by `log_fast!()` / `prewarm_fast!()`;
/// only supports an empty format specifier by default.
///
/// # Safety
///
/// Implementors must guarantee that `encode_fast` writes exactly
/// `encoded_size_fast()` bytes starting at `buffer + *offset`, and that
/// `decode_and_append_fast` reads the same number of bytes from a buffer
/// previously written by `encode_fast`.
pub unsafe trait EncodeFast: Encode {
    /// Returns the encoded byte size using the fast path (defaults to [`Encode::encoded_size`]).
    fn encoded_size_fast(&self) -> usize {
        <Self as Encode>::encoded_size(self)
    }

    /// Encodes the value using the fast path (defaults to [`Encode::encode`]).
    ///
    /// # Safety
    /// Same requirements as [`Encode::encode`].
    unsafe fn encode_fast(&self, buffer: *mut u8, offset: &mut usize) {
        unsafe {
            <Self as Encode>::encode(self, buffer, offset);
        }
    }

    /// Encodes the value using the fast path at a fixed byte offset.
    ///
    /// The default implementation forwards to [`Encode::encode_at_offset`],
    /// which lets fixed-width types benefit from compile-time layout
    /// specialization without duplicating logic.
    ///
    /// # Safety
    /// Same requirements as [`Encode::encode_at_offset`].
    #[inline(always)]
    unsafe fn encode_fast_at_offset(&self, buffer: *mut u8, offset: usize) {
        unsafe {
            <Self as Encode>::encode_at_offset(self, buffer, offset);
        }
    }

    /// Decodes and appends using the fast path with an empty format specifier
    /// (defaults to [`Encode::decode_and_format`]).
    ///
    /// # Safety
    /// Same requirements as [`Encode::decode_and_format`].
    unsafe fn decode_and_append_fast(buffer: *const u8, offset: &mut usize, output: &mut Vec<u8>) {
        unsafe {
            <Self as Encode>::decode_and_format(buffer, offset, output, "");
        }
    }

    /// Decodes and appends using the fast path from `buffer + offset`.
    ///
    /// The default implementation forwards to [`EncodeFast::decode_and_append_fast`]
    /// using a local mutable offset.
    ///
    /// # Safety
    /// Same requirements as [`EncodeFast::decode_and_append_fast`].
    #[inline(always)]
    unsafe fn decode_and_append_fast_at_offset(
        buffer: *const u8,
        offset: usize,
        output: &mut Vec<u8>,
    ) {
        let mut offset = offset;
        unsafe {
            <Self as EncodeFast>::decode_and_append_fast(buffer, &mut offset, output);
        }
    }
}

// Re-export everything that was previously public from this module.
pub use format::{VecWriter, append_display_const_default, append_display_with_spec};
pub use traits::{
    EncodeBinary, EncodeConstBinaryFormat, EncodeConstDefaultFormat, EncodeConstFloatFormat,
    EncodeConstLowerHexFormat, EncodeConstOctalFormat, EncodeConstUpperHexFormat, EncodeLowerHex,
    EncodeOctal, EncodeUpperHex,
};
