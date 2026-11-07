#![deny(clippy::unwrap_used, clippy::expect_used)]
//! Serialized namespace updates retaining both paths and the current descriptor.
use std::{
    ffi::CString,
    fs::OpenOptions,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    },
    path::Path,
    rc::Rc,
};

use liburing::backend::RawSqe;
use zerolog_core::{ConsumerCore, FrozenSlab, ZeroLogError};

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationStep {
    Idle,
    Sync,
    Unlink,
    Rename(u32),
    RenameBase,
    Open,
}

pub struct Rotation {
    current: Rc<OwnedFd>,
    paths: Vec<CString>,
    max_size: u64,
    max_files: u32,
    durable: bool,
    size: u64,
    step: RotationStep,
    reopen_pending: bool,
}
impl Rotation {
    pub fn from_fd(fd: OwnedFd) -> Self {
        Self {
            current: Rc::new(fd),
            paths: Vec::new(),
            max_size: u64::MAX,
            max_files: 0,
            durable: false,
            size: 0,
            step: RotationStep::Idle,
            reopen_pending: false,
        }
    }
    pub fn open(
        path: &Path,
        max_size: u64,
        max_files: u32,
        durable: bool,
    ) -> Result<Self, ZeroLogError> {
        if max_size == 0 || max_files == 0 {
            return Err(ZeroLogError::InvalidConfig("rotation limits must be positive"));
        }
        let path = std::path::absolute(path).map_err(io_error)?;
        let mut paths = Vec::with_capacity(max_files as usize + 1);
        paths.push(
            CString::new(path.as_os_str().as_bytes())
                .map_err(|_| ZeroLogError::InvalidConfig("NUL in path"))?,
        );
        for index in 1..=max_files {
            let mut bytes = path.as_os_str().as_bytes().to_vec();
            bytes.extend_from_slice(format!(".{index}").as_bytes());
            paths
                .push(CString::new(bytes).map_err(|_| ZeroLogError::InvalidConfig("NUL in path"))?);
        }
        let file = OpenOptions::new().create(true).append(true).open(&path).map_err(io_error)?;
        let size = file.metadata().map_err(io_error)?.len();
        Ok(Self {
            current: Rc::new(file.into()),
            paths,
            max_size,
            max_files,
            durable,
            size,
            step: RotationStep::Idle,
            reopen_pending: false,
        })
    }
    pub fn current_fd(&self) -> Rc<OwnedFd> {
        Rc::clone(&self.current)
    }
    pub fn current_size(&self) -> u64 {
        self.size
    }
    pub fn step(&self) -> RotationStep {
        self.step
    }
    pub fn reopen_pending(&self) -> bool {
        self.reopen_pending
    }
    pub fn record_written(&mut self, count: u32) {
        self.size = self.size.saturating_add(count as u64);
    }
    pub fn write_all_sync(&mut self, mut bytes: &[u8]) -> Result<(), ZeroLogError> {
        self.write_sync_progress(&mut bytes)
    }
    fn write_sync_progress(&mut self, bytes: &mut &[u8]) -> Result<(), ZeroLogError> {
        let fd = self.current.as_raw_fd();
        self.write_sync_with(bytes, |bytes| {
            // SAFETY: the descriptor is owned and the slice lives through this synchronous call.
            let count = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
            if count < 0 {
                Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO))
            } else {
                Ok(count as usize)
            }
        })
    }
    fn write_sync_with(
        &mut self,
        bytes: &mut &[u8],
        mut write: impl FnMut(&[u8]) -> Result<usize, i32>,
    ) -> Result<(), ZeroLogError> {
        while !bytes.is_empty() {
            match write(bytes) {
                Ok(0) => return Err(ZeroLogError::Write(libc::EIO)),
                Ok(count) => {
                    *bytes = &bytes[count..];
                    self.size = self.size.saturating_add(count as u64);
                }
                Err(libc::EINTR) => continue,
                Err(errno) => return Err(ZeroLogError::Write(errno)),
            }
        }
        Ok(())
    }
    pub(crate) fn write_slab_sync<C: ConsumerCore>(
        &mut self,
        core: &mut C,
        slab: FrozenSlab,
    ) -> Result<(), ZeroLogError> {
        let mut remaining = slab.active_slice();
        let result = self.write_sync_progress(&mut remaining);
        let written = slab.len() - remaining.len() as u32;
        // A failed write may have committed a prefix; retry only its unwritten suffix.
        if let Some(suffix) = core.complete_output(slab, written)
            && let Err(error) = result
        {
            core.write_failed(suffix, error);
        }
        result
    }
    pub fn begin(&mut self) -> bool {
        if self.step != RotationStep::Idle {
            return true;
        }
        if self.reopen_pending {
            self.step = RotationStep::Open;
        } else if self.max_files > 0 && self.size >= self.max_size {
            self.step = if self.durable { RotationStep::Sync } else { RotationStep::Unlink };
        }
        self.step != RotationStep::Idle
    }
    /// Applies a terminal namespace CQE. Successful OPENAT descriptors use `opened`.
    pub fn complete_step(&mut self, result: i32) -> Result<(), ZeroLogError> {
        if result < 0
            && !(result == -libc::ENOENT
                && matches!(self.step, RotationStep::Unlink | RotationStep::Rename(_)))
        {
            return Err(ZeroLogError::Rotation {
                step: self.step_code(),
                errno: result.saturating_neg(),
            });
        }
        self.step = match self.step {
            RotationStep::Sync => RotationStep::Unlink,
            RotationStep::Unlink if self.max_files > 1 => RotationStep::Rename(self.max_files - 1),
            RotationStep::Unlink | RotationStep::Rename(1) => RotationStep::RenameBase,
            RotationStep::Rename(index) if index > 1 => RotationStep::Rename(index - 1),
            RotationStep::RenameBase => {
                self.reopen_pending = true;
                RotationStep::Open
            }
            _ => return Err(ZeroLogError::InvalidConfig("invalid rotation completion")),
        };
        Ok(())
    }
    /// Transfers ownership of the successful OPENAT result exactly once.
    pub fn opened(&mut self, fd: OwnedFd) -> Result<(), ZeroLogError> {
        if self.step != RotationStep::Open {
            return Err(ZeroLogError::InvalidConfig("unexpected open completion"));
        }
        self.current = Rc::new(fd);
        self.size = 0;
        self.reopen_pending = false;
        self.step = RotationStep::Idle;
        Ok(())
    }
    pub fn rotate_sync(&mut self) -> Result<(), ZeroLogError> {
        self.rotate_sync_with(Self::sync_step)
    }
    fn rotate_sync_with(
        &mut self,
        mut execute: impl FnMut(&Self) -> i32,
    ) -> Result<(), ZeroLogError> {
        while self.begin() {
            let result = execute(self);
            if result == -libc::EINTR {
                continue;
            }
            if self.step == RotationStep::Open && result >= 0 {
                // SAFETY: successful openat transfers this new descriptor exactly once.
                self.opened(unsafe { OwnedFd::from_raw_fd(result) })?;
            } else {
                self.complete_step(result)?;
            }
        }
        Ok(())
    }
    fn sync_step(&self) -> i32 {
        // SAFETY: this owner retains its descriptor and immutable NUL-terminated paths
        // throughout these synchronous calls; no async operation may use this rotation.
        let result = unsafe {
            match self.step {
                RotationStep::Sync => libc::fsync(self.current.as_raw_fd()),
                RotationStep::Unlink => {
                    libc::unlinkat(libc::AT_FDCWD, self.paths[self.max_files as usize].as_ptr(), 0)
                }
                RotationStep::Rename(index) => libc::renameat(
                    libc::AT_FDCWD,
                    self.paths[index as usize].as_ptr(),
                    libc::AT_FDCWD,
                    self.paths[index as usize + 1].as_ptr(),
                ),
                RotationStep::RenameBase => libc::renameat(
                    libc::AT_FDCWD,
                    self.paths[0].as_ptr(),
                    libc::AT_FDCWD,
                    self.paths[1].as_ptr(),
                ),
                RotationStep::Open => libc::openat(
                    libc::AT_FDCWD,
                    self.paths[0].as_ptr(),
                    libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND | libc::O_CLOEXEC,
                    0o644,
                ),
                RotationStep::Idle => return 0,
            }
        };
        if result < 0 {
            -std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO)
        } else {
            result
        }
    }
    fn step_code(&self) -> u8 {
        match self.step {
            RotationStep::Idle => 0,
            RotationStep::Sync => 1,
            RotationStep::Unlink => 2,
            RotationStep::Rename(_) => 3,
            RotationStep::RenameBase => 4,
            RotationStep::Open => 5,
        }
    }
    /// # Safety
    /// Retain this owner and every operand until the target terminal CQE, including staging/cancel.
    /// `sqe` must point to initialized, exclusive, unpublished SQE storage.
    pub unsafe fn prepare_step(&self, sqe: *mut RawSqe) {
        use liburing::ops::{fs::*, prep_fsync};
        // SAFETY: the caller retains immutable path allocations and the current fd.
        unsafe {
            match self.step {
                RotationStep::Sync => prep_fsync(sqe, self.current.as_raw_fd(), 0),
                RotationStep::Unlink => prep_unlinkat(
                    sqe,
                    libc::AT_FDCWD,
                    self.paths[self.max_files as usize].as_ptr(),
                    0,
                ),
                RotationStep::Rename(index) => prep_renameat(
                    sqe,
                    libc::AT_FDCWD,
                    self.paths[index as usize].as_ptr(),
                    libc::AT_FDCWD,
                    self.paths[index as usize + 1].as_ptr(),
                    0,
                ),
                RotationStep::RenameBase => prep_renameat(
                    sqe,
                    libc::AT_FDCWD,
                    self.paths[0].as_ptr(),
                    libc::AT_FDCWD,
                    self.paths[1].as_ptr(),
                    0,
                ),
                RotationStep::Open => prep_openat(
                    sqe,
                    libc::AT_FDCWD,
                    self.paths[0].as_ptr(),
                    libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND | libc::O_CLOEXEC,
                    0o644,
                ),
                RotationStep::Idle => liburing::ops::prep_nop(sqe),
            }
        }
    }
}
fn io_error(error: std::io::Error) -> ZeroLogError {
    ZeroLogError::Write(error.raw_os_error().unwrap_or(libc::EIO))
}

#[cfg(test)]
#[path = "../tests/unit/rotation.rs"]
mod tests;
