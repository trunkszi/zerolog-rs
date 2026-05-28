//! Re-exports from `ferrilog_core` and `ferrilog_macros`, plus convenience
//! level-specific logging macros.

extern crate self as ferrilog;

pub use ferrilog_core::*;
pub use ferrilog_macros::*;

/// Logs a message at the **Trace** level.
///
/// ```ignore
/// trace!("poll state={}", state);
/// ```
#[macro_export]
macro_rules! trace {
    ($($arguments:tt)*) => {
        $crate::log!($crate::Level::TRC, $($arguments)*)
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
        $crate::log!($crate::Level::DBG, $($arguments)*)
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
        $crate::log!($crate::Level::INF, $($arguments)*)
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
        $crate::log!($crate::Level::WRN, $($arguments)*)
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
        $crate::log!($crate::Level::ERR, $($arguments)*)
    };
}

/// Creates a lightweight span.
///
/// The span is not entered until [`Span::enter`](ferrilog_core::Span::enter)
/// is called. Log records emitted while the span is entered carry the span id,
/// which can be rendered with the `{span}` header token.
///
/// ```ignore
/// let span = ferrilog::span!("connection");
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
