use std::{
    collections::VecDeque,
    fs::File,
    os::fd::{FromRawFd, OwnedFd},
};

use zerolog_core::{ConsumerCore, DrainBudget, DrainFlags, DrainReport, FrozenSlab, ZeroLogError};
#[derive(Default)]
pub(crate) struct Core {
    pub pending: VecDeque<FrozenSlab>,
    pub incoming: Option<Vec<u8>>,
    pub frozen: bool,
    pub completed: u32,
    pub drains: u32,
}
impl ConsumerCore for Core {
    fn drain(&mut self, now: i64, _: DrainBudget) -> DrainReport {
        self.drains += 1;
        if let Some(bytes) = self.incoming.take() {
            self.pending.push_back(slab(&bytes));
        }
        let mut flags = DrainFlags::new(0);
        if !self.pending.is_empty() {
            flags |= DrainFlags::OUTPUT_READY;
        }
        DrainReport { next_deadline_ns: now + 1_000_000, flags }
    }
    fn freeze_output(&mut self) -> Option<FrozenSlab> {
        let slab = self.pending.pop_front()?;
        self.frozen = true;
        Some(slab)
    }
    fn complete_output(&mut self, mut slab: FrozenSlab, written: u32) -> Option<FrozenSlab> {
        self.completed += written;
        if slab.advance(written) {
            self.frozen = false;
            None
        } else {
            Some(slab)
        }
    }
    fn write_failed(&mut self, slab: FrozenSlab, _: ZeroLogError) {
        self.frozen = false;
        self.pending.push_front(slab);
    }
    fn has_unflushed(&self) -> bool {
        self.frozen || !self.pending.is_empty()
    }
    fn flush_sync(&mut self) -> Result<(), ZeroLogError> {
        panic!("uring output must not use the legacy writer")
    }
}
pub(crate) fn slab(bytes: &[u8]) -> FrozenSlab {
    FrozenSlab::new(bytes.into(), bytes.len() as u32)
}
pub(crate) fn pipe() -> std::io::Result<(File, OwnedFd)> {
    let mut fds = [-1; 2];
    // SAFETY: pipe2 initializes two distinct owned descriptors.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: each successful descriptor is adopted exactly once.
    Ok(unsafe { (File::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}
