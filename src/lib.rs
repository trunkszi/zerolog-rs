//! Re-exports from `ferrilog_core` and `ferrilog_macros`, plus convenience
//! level-specific logging macros.

extern crate self as ferrilog;

pub use ferrilog_core::*;
pub use ferrilog_macros::*;

/// Logs a message at the **Debug** level.
///
/// ```ignore
/// logd!("value={}", x);
/// ```
#[macro_export]
macro_rules! logd {
    ($($arguments:tt)*) => {
        $crate::log!($crate::Level::DBG, $($arguments)*)
    };
}

/// Logs a message at the **Info** level.
///
/// ```ignore
/// logi!("server started on port {}", port);
/// ```
#[macro_export]
macro_rules! logi {
    ($($arguments:tt)*) => {
        $crate::log!($crate::Level::INF, $($arguments)*)
    };
}

/// Logs a message at the **Warning** level.
///
/// ```ignore
/// logw!("connection timeout: {} ms", elapsed);
/// ```
#[macro_export]
macro_rules! logw {
    ($($arguments:tt)*) => {
        $crate::log!($crate::Level::WRN, $($arguments)*)
    };
}

/// Logs a message at the **Error** level.
///
/// ```ignore
/// loge!("failed to open file: {}", path);
/// ```
#[macro_export]
macro_rules! loge {
    ($($arguments:tt)*) => {
        $crate::log!($crate::Level::ERR, $($arguments)*)
    };
}
