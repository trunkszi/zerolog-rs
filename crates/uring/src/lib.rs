#![deny(clippy::unwrap_used, clippy::expect_used)]
//! Caller-driven logging on a shared io_uring proactor.
pub mod rotation;
pub use rotation::{Rotation, RotationStep};

pub mod sink;
pub use sink::{
    LifecycleFsm, OwnedConsumer, ProactorLogSink, SinkConfig, TimerFsm, WakeFsm, WriteRotateFsm,
};

pub mod worker;
pub use worker::{UringConsumerThread, WorkerBackend, WorkerConfig};

#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod test_support;
