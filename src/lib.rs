//! Re-exports from `zerolog_core` and `zerolog_macros`, plus convenience
//! level-specific logging macros.

/// Raw-copy encodings may only transfer values safe to send to the consumer.
///
/// ```compile_fail
/// use std::cell::Cell;
/// #[derive(Clone, Copy, zerolog::Encode)]
/// struct NotSend(&'static Cell<u32>);
/// impl std::fmt::Display for NotSend {
///     fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
///         self.0.get().fmt(f)
///     }
/// }
/// fn require_encode<T: zerolog::Encode>() {}
/// require_encode::<NotSend>();
/// ```
extern crate self as zerolog;

pub use zerolog_core::*;
pub use zerolog_macros::*;

/// Logs a message at the **Trace** level.
///
/// ```ignore
/// trace!("poll state={}", state);
/// ```
#[macro_export]
macro_rules! trace {
    ($($arguments:tt)*) => {
        $crate::log!(@crate_path = $crate; $crate::Level::TRC, $($arguments)*)
    };
}

/// Logs a message at the **Debug** level.
///
/// ```ignore
/// debug!("value={}", x);
/// ```
#[macro_export]
macro_rules! debug {
    ($($arguments:tt)*) => {
        $crate::log!(@crate_path = $crate; $crate::Level::DBG, $($arguments)*)
    };
}

/// Logs a message at the **Info** level.
///
/// ```ignore
/// info!("server started on port {}", port);
/// ```
#[macro_export]
macro_rules! info {
    ($($arguments:tt)*) => {
        $crate::log!(@crate_path = $crate; $crate::Level::INF, $($arguments)*)
    };
}

/// Logs a message at the **Warning** level.
///
/// ```ignore
/// warn!("connection timeout: {} ms", elapsed);
/// ```
#[macro_export]
macro_rules! warn {
    ($($arguments:tt)*) => {
        $crate::log!(@crate_path = $crate; $crate::Level::WRN, $($arguments)*)
    };
}

/// Logs a message at the **Error** level.
///
/// ```ignore
/// error!("failed to open file: {}", path);
/// ```
#[macro_export]
macro_rules! error {
    ($($arguments:tt)*) => {
        $crate::log!(@crate_path = $crate; $crate::Level::ERR, $($arguments)*)
    };
}

/// Creates a lightweight span.
///
/// The span is not entered until [`Span::enter`](zerolog_core::Span::enter)
/// is called. Log records emitted while the span is entered carry the span id,
/// which can be rendered with the `{span}` header token.
///
/// ```ignore
/// let span = zerolog::span!("connection");
/// let _guard = span.enter();
/// trace!("poll");
/// ```
#[macro_export]
macro_rules! span {
    ($name:literal) => {{
        static INFO: $crate::StaticSpanInfo =
            $crate::StaticSpanInfo::new($name, concat!(file!(), ":", line!()));
        $crate::Span::new(&INFO)
    }};
}

/// Logs a structured event with optional `key => value` fields.
///
/// ```ignore
/// zerolog::event!(zerolog::Level::INF, "order submitted"; order_id => 42u64, qty => 10u32);
/// ```
#[macro_export]
macro_rules! event {
    ($($tt:tt)*) => {
        $crate::log!(@crate_path = $crate; $($tt)*)
    };
}
