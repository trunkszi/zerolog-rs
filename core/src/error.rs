#![deny(clippy::unwrap_used, clippy::expect_used)]
//! Finite errors shared by consumer and output adapters.

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ZeroLogError {
    #[error("[zerolog.consumer] consumer role already leased or busy")]
    ConsumerBusy,
    #[error("[zerolog.consumer] lease poisoned by abandoned kernel operations")]
    ConsumerPoisoned,
    #[error("[zerolog.consumer] worker cannot change its own lifecycle")]
    SelfStop,
    #[error("[zerolog.output] write failed with errno {0}")]
    Write(i32),
    #[error("[zerolog.output] write made no progress")]
    WriteZero,
    #[error("[zerolog.output] rotation step {step} failed with errno {errno}")]
    Rotation { step: u8, errno: i32 },
    #[error("[zerolog.uring] blocking target on a shared proactor")]
    BlockingTargetOnSharedProactor,
    #[error("[zerolog.uring] unsupported proactor mode: {0}")]
    UnsupportedMode(&'static str),
    #[error("[zerolog.uring] submission queue full")]
    SqRingFull,
    #[error("[zerolog.config] invalid configuration: {0}")]
    InvalidConfig(&'static str),
}

impl From<ZeroLogError> for std::io::Error {
    fn from(error: ZeroLogError) -> Self {
        use std::io::{Error, ErrorKind};
        match error {
            ZeroLogError::Write(errno) | ZeroLogError::Rotation { errno, .. } => {
                Error::from_raw_os_error(errno)
            }
            ZeroLogError::ConsumerBusy | ZeroLogError::SqRingFull => {
                Error::from(ErrorKind::WouldBlock)
            }
            ZeroLogError::WriteZero => Error::from(ErrorKind::WriteZero),
            ZeroLogError::InvalidConfig(_) => Error::from(ErrorKind::InvalidInput),
            ZeroLogError::BlockingTargetOnSharedProactor | ZeroLogError::UnsupportedMode(_) => {
                Error::from(ErrorKind::Unsupported)
            }
            ZeroLogError::ConsumerPoisoned | ZeroLogError::SelfStop => {
                Error::from(ErrorKind::Other)
            }
        }
    }
}

pub(crate) fn io_error_to_zerolog_error(error: std::io::Error) -> ZeroLogError {
    if error.kind() == std::io::ErrorKind::WriteZero {
        return ZeroLogError::WriteZero;
    }
    if let Some(errno) = error.raw_os_error() {
        return ZeroLogError::Write(errno);
    }
    match error.kind() {
        std::io::ErrorKind::WouldBlock => ZeroLogError::ConsumerBusy,
        std::io::ErrorKind::InvalidInput => {
            ZeroLogError::InvalidConfig("invalid I/O configuration")
        }
        std::io::ErrorKind::Unsupported => {
            ZeroLogError::UnsupportedMode("unsupported I/O operation")
        }
        _ => ZeroLogError::Write(libc::EIO),
    }
}
