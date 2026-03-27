//! Procedural macros for the `ferrilog` crate.
//!
//! Provides the [`log!`] macro, level-specific convenience macros
//! (`logd!`, `logi!`, `logw!`, `loge!`), and the [`Encode`](derive@Encode)
//! derive macro.

mod derive_encode;
mod format_string;
mod log_macro;

use proc_macro::TokenStream;
use syn::{
    DeriveInput, Expr, LitStr, Result, Token,
    parse::{Parse, ParseStream},
    parse_macro_input,
};

/// Parsed input for the `log!` family of macros.
///
/// Expects a level expression, a format string literal, and zero or more
/// argument expressions separated by commas.
pub(crate) struct LogInput {
    pub(crate) level: Expr,
    pub(crate) format_string: LitStr,
    pub(crate) args: Vec<Expr>,
}

pub(crate) struct HeaderPatternInput {
    pub(crate) pattern: LitStr,
}

impl Parse for LogInput {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let level = input.parse()?;
        input.parse::<Token![,]>()?;
        let format_string = input.parse()?;
        let mut args = Vec::new();

        while input.peek(Token![,]) {
            input.parse::<Token![,]>()?;
            args.push(input.parse()?);
        }

        Ok(Self { level, format_string, args })
    }
}

impl Parse for HeaderPatternInput {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        Ok(Self { pattern: input.parse()? })
    }
}

/// Emits a log entry.
///
/// Automatically selects the optimal encoding path at compile time:
/// - **Fast path** (`EncodeFast`): when all placeholders are bare `{}` with
///   no format specifiers.
/// - **Normal path** (`Encode`): when any placeholder has a format specifier
///   (e.g. `{:x}`, `{:.2}`, `{:>10}`).
///
/// # Syntax
///
/// ```ignore
/// log!(Level::INF, "key={}", value);
/// log!(Level::INF, "hex={:x} pad={:>5}", num, other);
/// ```
#[proc_macro]
pub fn log(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as LogInput);
    log_macro::expand_log_auto(input).into()
}

/// Installs a compile-time-generated static header writer for a literal pattern.
#[proc_macro]
pub fn set_header_pattern_static(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as HeaderPatternInput);
    log_macro::expand_set_header_pattern_static(input).into()
}

/// Derives the [`Encode`] trait for a type.
///
/// The type must be `Copy + 'static + Display`.
#[proc_macro_derive(Encode)]
pub fn derive_encode(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    derive_encode::expand_derive_encode(input).into()
}

// ---------------------------------------------------------------------------
// Legacy / internal macros — hidden from user documentation
// ---------------------------------------------------------------------------

/// Pre-allocates the calling thread's log buffer.
///
/// Since `StaticInfo` is initialized lazily via `OnceLock`, there is no
/// site registration to perform. This macro simply ensures the thread's
/// SPSC buffer is allocated before the first `log!()` call.
#[doc(hidden)]
#[proc_macro]
pub fn prewarm(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as LogInput);
    log_macro::expand_prewarm(input).into()
}

/// Explicit fast-path variant of `log!`. Prefer `log!()` which auto-detects.
#[doc(hidden)]
#[proc_macro]
pub fn log_fast(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as LogInput);
    log_macro::expand_log_auto(input).into()
}

/// Explicit fast-path variant of `prewarm!`. Prefer `prewarm!()`.
#[doc(hidden)]
#[proc_macro]
pub fn prewarm_fast(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as LogInput);
    log_macro::expand_prewarm(input).into()
}
