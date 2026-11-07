#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::{
    cell::UnsafeCell,
    io,
    ops::{Deref, DerefMut},
    ptr::NonNull,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering as AtomicOrdering},
    },
    thread::{self, JoinHandle, ThreadId},
};

use uarch::prelude::*;

pub use crate::contract::{HeapNode, RecordCursor};
use crate::{
    contract::{
        ConsumerLease, DOORBELL, DOORBELL_PARKED_BIT, DOORBELL_URGENT_BIT, REGISTERED_SLOTS,
        futex_wait_doorbell_timeout, ring_doorbell,
    },
    error::ZeroLogError,
    spsc_queue::{
        HEADER_SIZE, MessageHeader, PAYLOAD_INFO_OFFSET, PAYLOAD_TIMESTAMP_OFFSET, SpscVarQueue,
    },
    thread_buffer::{MAX_THREADS, THREAD_BUFFER_REGISTRY, ThreadBuffer},
};

const WORKER_WAIT_START: u8 = 0;
const WORKER_ABORT: u8 = 1;
const WORKER_RUNNING: u8 = 2;
const WORKER_STOP_AND_FLUSH: u8 = 3;
const WORKER_HANDOFF: u8 = 4;

struct PollerControl {
    stop_mode: Arc<AtomicU8>,
    handle: JoinHandle<io::Result<()>>,
}

struct PendingWorker {
    control: Option<PollerControl>,
}

impl PendingWorker {
    fn commit(mut self) -> PollerControl {
        let Some(control) = self.control.take() else {
            unreachable!("pending worker is committed only once");
        };
        control
    }
}

impl Drop for PendingWorker {
    fn drop(&mut self) {
        if let Some(control) = self.control.take() {
            control.stop_mode.store(WORKER_ABORT, AtomicOrdering::Release);
            ring_doorbell(true);
            control.handle.thread().unpark();
            let _ = control.handle.join();
        }
    }
}

#[derive(Default)]
struct PollerLifecycle {
    control: Option<PollerControl>,
    generation: u64,
    stopping: bool,
    worker_id: Option<ThreadId>,
}

/// Serializes background-worker publication, configuration and joined shutdown.
pub struct PollerState {
    lifecycle: UnsafeCell<PollerLifecycle>,
    lock: AtomicBool,
    running: AtomicBool,
    external_lease_active: AtomicBool,
}

// SAFETY: `lifecycle` is accessed exclusively while holding `LifecycleGuard`,
// which synchronizes via `lock` with `Acquire`/`Release` ordering.
unsafe impl Sync for PollerState {}

struct LifecycleGuard<'a> {
    state: &'a PollerState,
}

impl Deref for LifecycleGuard<'_> {
    type Target = PollerLifecycle;
    fn deref(&self) -> &Self::Target {
        // SAFETY: `self.state.lock` is held exclusively for the lifetime of this guard.
        unsafe { &*self.state.lifecycle.get() }
    }
}

impl DerefMut for LifecycleGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: `self.state.lock` is held exclusively for the lifetime of this guard.
        unsafe { &mut *self.state.lifecycle.get() }
    }
}

impl Drop for LifecycleGuard<'_> {
    fn drop(&mut self) {
        self.state.lock.store(false, AtomicOrdering::Release);
    }
}

impl Default for PollerState {
    fn default() -> Self {
        Self {
            lifecycle: UnsafeCell::new(PollerLifecycle::default()),
            lock: AtomicBool::new(false),
            running: AtomicBool::new(false),
            external_lease_active: AtomicBool::new(false),
        }
    }
}

/// Consumer-private snapshot of a registered producer slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotSnapshot {
    pub buffer_ptr: NonNull<ThreadBuffer>,
    pub slot_epoch: u32,
}

impl HeapNode {
    #[hot_inline]
    pub const fn raw_timestamp(self) -> i64 {
        self.raw_timestamp
    }
    #[hot_inline]
    pub const fn cursor(self) -> RecordCursor {
        self.cursor
    }
    #[hot_inline]
    pub const fn slot(self) -> u8 {
        self.cursor.get(RecordCursor::SLOT)
    }
    #[hot_inline]
    pub const fn slot_epoch(self) -> u32 {
        self.cursor.get(RecordCursor::SLOT_EPOCH)
    }
    #[hot_inline]
    pub const fn block_index(self) -> u32 {
        self.cursor.get(RecordCursor::BLOCK_INDEX)
    }
    #[hot_inline]
    pub const fn precedes(self, other: Self) -> bool {
        if self.raw_timestamp != other.raw_timestamp {
            self.raw_timestamp < other.raw_timestamp
        } else {
            self.cursor.raw() < other.cursor.raw()
        }
    }
}

const EMPTY_HEAP_NODE: HeapNode = HeapNode { raw_timestamp: 0, cursor: RecordCursor::new(0) };

/// Fixed-capacity value-oriented min-heap merging published queue fronts by raw timestamp.
pub struct ReadyMessageHeap {
    nodes: [HeapNode; MAX_THREADS],
    slots: [Option<SlotSnapshot>; MAX_THREADS],
    active_slots: [Bitmap64; 4],
    len: usize,
    slot_bound: usize,
    next_epoch: u32,
}

/// Alias for [`ReadyMessageHeap`].
pub type MergeHeap = ReadyMessageHeap;

impl Default for ReadyMessageHeap {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadyMessageHeap {
    pub const fn new() -> Self {
        Self {
            nodes: [EMPTY_HEAP_NODE; MAX_THREADS],
            slots: [None; MAX_THREADS],
            active_slots: [Bitmap64::EMPTY; 4],
            len: 0,
            slot_bound: 0,
            next_epoch: 1,
        }
    }

    #[hot_inline]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
    #[hot_inline]
    pub const fn len(&self) -> usize {
        self.len
    }
    #[hot_inline]
    pub const fn has_slot(&self, slot: u8) -> bool {
        self.active_slots[(slot >> 6) as usize].test((slot & 63) as u32)
    }

    #[hot_inline]
    pub fn resolve_slot(&self, cursor: RecordCursor) -> Option<NonNull<ThreadBuffer>> {
        let snapshot = self.slots[cursor.get(RecordCursor::SLOT) as usize]?;
        (snapshot.slot_epoch == cursor.get(RecordCursor::SLOT_EPOCH)).then_some(snapshot.buffer_ptr)
    }

    /// # Safety
    /// The caller must hold exclusive consumer access to the resolved buffer.
    #[hot_inline]
    pub unsafe fn resolve_front(
        &self,
        cursor: RecordCursor,
    ) -> Option<(NonNull<ThreadBuffer>, *const MessageHeader)> {
        let buffer_ptr = self.resolve_slot(cursor)?;
        // SAFETY: caller holds exclusive consumer access to the live buffer.
        let header =
            unsafe { SpscVarQueue::consumer_front(&raw const (*buffer_ptr.as_ptr()).queue)? };
        Some((buffer_ptr, header))
    }

    /// # Safety
    /// Every non-null pointer must reference a live `ThreadBuffer` under exclusive consumer access.
    pub unsafe fn from_buffers(buffers: &[*mut ThreadBuffer]) -> Self {
        let mut heap = Self::new();
        // SAFETY: the caller provides live buffers and exclusive consumer access.
        unsafe { heap.clear_and_fill(buffers) };
        heap
    }

    /// # Safety
    /// Every non-null pointer must reference a live `ThreadBuffer` under exclusive consumer access.
    pub unsafe fn clear_and_fill(&mut self, buffers: &[*mut ThreadBuffer]) {
        self.clear();
        let bound = buffers.len().min(MAX_THREADS);
        self.slot_bound = bound;
        for (index, &ptr) in buffers.iter().take(bound).enumerate() {
            if let Some(non_null) = NonNull::new(ptr) {
                let snapshot = self.bind_slot(index, non_null);
                // SAFETY: caller guarantees exclusive consumer access to live buffer.
                unsafe { self.push_front_for_slot(index as u8, snapshot) };
            } else {
                self.slots[index] = None;
            }
        }
    }

    /// # Safety
    /// The caller must hold exclusive consumer access to all registered buffers.
    pub unsafe fn fill_from_registry(&mut self) {
        let count = THREAD_BUFFER_REGISTRY.count().min(MAX_THREADS);
        self.slot_bound = self.slot_bound.max(count);
        for slot in 0..count {
            let Some(non_null) = NonNull::new(THREAD_BUFFER_REGISTRY.get(slot)) else {
                if !self.has_slot(slot as u8) {
                    self.slots[slot] = None;
                }
                continue;
            };
            let snapshot = self.bind_slot(slot, non_null);
            if !self.has_slot(slot as u8) {
                // SAFETY: caller holds exclusive consumer access to registered buffers.
                unsafe { self.push_front_for_slot(slot as u8, snapshot) };
            }
        }
    }

    fn bind_slot(&mut self, slot: usize, non_null: NonNull<ThreadBuffer>) -> SlotSnapshot {
        match self.slots[slot] {
            Some(existing) if existing.buffer_ptr == non_null => existing,
            _ => {
                self.next_epoch = self.next_epoch.wrapping_add(1).max(1);
                let snapshot = SlotSnapshot { buffer_ptr: non_null, slot_epoch: self.next_epoch };
                self.slots[slot] = Some(snapshot);
                snapshot
            }
        }
    }

    pub(crate) fn clear(&mut self) {
        self.len = 0;
        self.active_slots = [Bitmap64::EMPTY; 4];
    }

    pub(crate) fn clear_slot(&mut self, slot: usize) {
        if slot < MAX_THREADS {
            self.slots[slot] = None;
        }
    }

    pub(crate) fn reinsert(&mut self, node: HeapNode) {
        if !self.has_slot(node.slot()) && self.len < MAX_THREADS {
            self.push_node(node);
        }
    }

    #[hot_inline]
    pub fn pop(&mut self) -> Option<HeapNode> {
        if self.len == 0 {
            return None;
        }
        let min_node = self.nodes[0];
        let slot = min_node.slot();
        let word = (slot >> 6) as usize;
        self.active_slots[word] = self.active_slots[word].clear((slot & 63) as u32);
        self.len -= 1;
        if self.len > 0 {
            let last = self.nodes[self.len];
            self.sift_down(0, last);
        }
        Some(min_node)
    }

    /// # Safety
    /// The slot's `ThreadBuffer` must remain live under exclusive consumer access.
    #[hot_inline]
    pub unsafe fn refresh_cursor(&mut self, cursor: RecordCursor) {
        let slot = cursor.get(RecordCursor::SLOT);
        if let Some(snapshot) = self.slots[slot as usize]
            && snapshot.slot_epoch == cursor.get(RecordCursor::SLOT_EPOCH)
        {
            // SAFETY: caller holds exclusive consumer access to live slot buffer.
            unsafe { self.push_front_for_slot(slot, snapshot) };
        }
    }

    /// # Safety
    /// The pointer must be null or reference a live `ThreadBuffer` under exclusive consumer access.
    pub unsafe fn refresh_buffer(&mut self, buffer: *mut ThreadBuffer) {
        let Some(non_null) = NonNull::new(buffer) else { return };
        for slot in 0..self.slot_bound {
            if let Some(snapshot) = self.slots[slot]
                && snapshot.buffer_ptr == non_null
            {
                // SAFETY: caller holds exclusive consumer access to live buffer.
                unsafe { self.push_front_for_slot(slot as u8, snapshot) };
                return;
            }
        }
        if self.slot_bound < MAX_THREADS {
            let slot = self.slot_bound;
            self.slot_bound += 1;
            let snapshot = self.bind_slot(slot, non_null);
            // SAFETY: caller holds exclusive consumer access to live buffer.
            unsafe { self.push_front_for_slot(slot as u8, snapshot) };
        }
    }

    #[hot_inline]
    unsafe fn push_front_for_slot(&mut self, slot: u8, snapshot: SlotSnapshot) {
        if self.has_slot(slot) || self.len >= MAX_THREADS {
            return;
        }
        let buffer = snapshot.buffer_ptr.as_ptr();
        // SAFETY: callers maintain exclusive consumer ownership of live buffers.
        let Some(header) = (unsafe { SpscVarQueue::consumer_front(&raw const (*buffer).queue) })
        else {
            return;
        };
        // SAFETY: `queue` is at the start of `buffer` and `header` lies within `queue.storage`.
        let queue_base = (unsafe { &raw const (*buffer).queue }) as usize;
        let block_index = (((header as usize).wrapping_sub(queue_base)) / HEADER_SIZE) as u32;

        // SAFETY: consumer_front acquired publication; metadata remains live until consumer_pop.
        let raw_timestamp = unsafe {
            let payload = MessageHeader::payload_pointer_raw(header) as *const u8;
            uarch::mem::prefetch_read::<uarch::mem::L2, u8>(payload);
            let info_ptr = payload.add(PAYLOAD_INFO_OFFSET).cast::<*const u8>().read_unaligned();
            if !info_ptr.is_null() {
                uarch::mem::prefetch_read::<uarch::mem::L2, u8>(info_ptr);
            }
            payload.add(PAYLOAD_TIMESTAMP_OFFSET).cast::<i64>().read_unaligned()
        };

        let cursor = RecordCursor::new(0).patch(
            BitPatch::EMPTY
                .with_field(RecordCursor::BLOCK_INDEX, block_index as u64)
                .with_field(RecordCursor::SLOT, slot as u64)
                .with_field(RecordCursor::SLOT_EPOCH, snapshot.slot_epoch as u64),
        );
        self.push_node(HeapNode { raw_timestamp, cursor });
    }

    #[hot_inline]
    fn push_node(&mut self, node: HeapNode) {
        let slot = node.slot();
        let word = (slot >> 6) as usize;
        self.active_slots[word] = self.active_slots[word].set((slot & 63) as u32);
        let idx = self.len;
        self.len = idx + 1;
        self.sift_up(idx, node);
    }

    #[hot_inline]
    fn sift_up(&mut self, mut idx: usize, node: HeapNode) {
        while idx > 0 {
            let parent = (idx - 1) >> 1;
            let parent_node = self.nodes[parent];
            if !node.precedes(parent_node) {
                break;
            }
            self.nodes[idx] = parent_node;
            idx = parent;
        }
        self.nodes[idx] = node;
    }

    #[hot_inline]
    fn sift_down(&mut self, mut idx: usize, node: HeapNode) {
        let len = self.len;
        loop {
            let left = (idx << 1) + 1;
            if left >= len {
                break;
            }
            let right = left + 1;
            let child = if right < len && self.nodes[right].precedes(self.nodes[left]) {
                right
            } else {
                left
            };
            let child_node = self.nodes[child];
            if !child_node.precedes(node) {
                break;
            }
            self.nodes[idx] = child_node;
            idx = child;
        }
        self.nodes[idx] = node;
    }
}

fn pin_current_thread_to_core(core_id: usize) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use libc::{CPU_SET, CPU_ZERO, cpu_set_t, sched_setaffinity};
        // SAFETY: `cpu_set_t` is a plain-old-data bitmask on the caller's stack.
        unsafe {
            let mut cpuset = std::mem::zeroed::<cpu_set_t>();
            CPU_ZERO(&mut cpuset);
            CPU_SET(core_id, &mut cpuset);
            if sched_setaffinity(0, std::mem::size_of::<cpu_set_t>(), &cpuset) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = core_id;
        Ok(())
    }
}

fn wait_doorbell_interval(observed: u32, worker_stop: &AtomicU8, interval_ns: u64) {
    let parked_word = observed | DOORBELL_PARKED_BIT;
    if DOORBELL
        .compare_exchange(observed, parked_word, AtomicOrdering::SeqCst, AtomicOrdering::Relaxed)
        .is_err()
    {
        return;
    }
    if worker_stop.load(AtomicOrdering::Acquire) == WORKER_RUNNING {
        #[cfg(all(target_os = "linux", not(miri)))]
        let _ = futex_wait_doorbell_timeout(parked_word, Some(interval_ns));
        #[cfg(any(not(target_os = "linux"), miri))]
        {
            let _ = futex_wait_doorbell_timeout;
            thread::park_timeout(std::time::Duration::from_nanos(interval_ns));
        }
    }
    DOORBELL.fetch_and(!DOORBELL_PARKED_BIT, AtomicOrdering::Release);
}

impl PollerState {
    pub fn is_running(&self) -> bool {
        self.running.load(AtomicOrdering::Acquire)
    }

    #[inline]
    pub(crate) fn has_external_lease(&self) -> bool {
        self.external_lease_active.load(AtomicOrdering::Acquire)
    }

    pub(crate) fn try_acquire_external_lease(&self) -> Result<ConsumerLease, ZeroLogError> {
        let state = self.stable().map_err(|_| ZeroLogError::ConsumerBusy)?;
        if state.control.is_some() || self.is_running() || self.has_external_lease() {
            return Err(ZeroLogError::ConsumerBusy);
        }
        let lease = ConsumerLease::try_acquire()?;
        self.external_lease_active.store(true, AtomicOrdering::Release);
        drop(state);
        Ok(lease)
    }

    pub(crate) fn release_external_lease(&self) {
        let _state = self.lock_lifecycle();
        self.external_lease_active.store(false, AtomicOrdering::Release);
    }

    fn lock_lifecycle(&self) -> LifecycleGuard<'_> {
        if self
            .lock
            .compare_exchange(false, true, AtomicOrdering::Acquire, AtomicOrdering::Relaxed)
            .is_err()
        {
            while self
                .lock
                .compare_exchange_weak(
                    false,
                    true,
                    AtomicOrdering::Acquire,
                    AtomicOrdering::Relaxed,
                )
                .is_err()
            {
                std::hint::spin_loop();
                thread::yield_now();
            }
        }
        LifecycleGuard { state: self }
    }

    fn stable(&self) -> io::Result<LifecycleGuard<'_>> {
        loop {
            let state = self.lock_lifecycle();
            if state.worker_id == Some(thread::current().id()) {
                return Err(io::Error::other("poller lifecycle cannot be changed by its worker"));
            }
            if !state.stopping {
                return Ok(state);
            }
            drop(state);
            std::hint::spin_loop();
            thread::yield_now();
        }
    }

    fn join_worker<'a>(&'a self, state: LifecycleGuard<'a>) -> io::Result<LifecycleGuard<'a>> {
        self.join_worker_with_mode(state, WORKER_STOP_AND_FLUSH)
    }

    fn join_worker_with_mode<'a>(
        &'a self,
        mut state: LifecycleGuard<'a>,
        stop_mode: u8,
    ) -> io::Result<LifecycleGuard<'a>> {
        let Some(control) = state.control.take() else { return Ok(state) };
        state.stopping = true;
        control.stop_mode.store(stop_mode, AtomicOrdering::Release);
        ring_doorbell(true);
        control.handle.thread().unpark();
        drop(state);
        let result = control
            .handle
            .join()
            .map_err(|_| io::Error::other("zerolog polling thread panicked"))
            .and_then(|res| res);
        let mut state = self.lock_lifecycle();
        state.stopping = false;
        state.worker_id = None;
        self.running.store(false, AtomicOrdering::Release);
        result?;
        Ok(state)
    }

    pub(crate) fn configure<T>(&self, apply: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        let state = self.stable()?;
        if state.control.is_some() {
            return Err(io::Error::other(
                "polling thread is running; stop it before configuration",
            ));
        }
        let result = apply();
        drop(state);
        result
    }

    pub fn start<F: Fn(bool) -> io::Result<()> + Send + 'static>(
        &self,
        interval_nanoseconds: i64,
        poll_fn: F,
    ) -> io::Result<()> {
        self.start_on_core(interval_nanoseconds, poll_fn, None)
    }

    pub fn start_on_core<F: Fn(bool) -> io::Result<()> + Send + 'static>(
        &self,
        interval_nanoseconds: i64,
        poll_fn: F,
        core_id: Option<usize>,
    ) -> io::Result<()> {
        self.start_configured(interval_nanoseconds, poll_fn, core_id, || Ok(())).map(|_| ())
    }

    pub(crate) fn start_configured<F: Fn(bool) -> io::Result<()> + Send + 'static>(
        &self,
        interval_nanoseconds: i64,
        poll_fn: F,
        core_id: Option<usize>,
        configure: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<u64> {
        let state = self.stable()?;
        if self.has_external_lease() {
            return Err(io::Error::from(ZeroLogError::ConsumerBusy));
        }
        let generation = state
            .generation
            .checked_add(1)
            .ok_or_else(|| io::Error::other("poller generation exhausted"))?;
        let stop_mode = Arc::new(AtomicU8::new(WORKER_WAIT_START));
        let worker_stop = Arc::clone(&stop_mode);
        let interval_ns = poll_interval_ns(interval_nanoseconds);
        let handle = thread::Builder::new().name("zerolog-poller".into()).spawn(move || {
            loop {
                match worker_stop.load(AtomicOrdering::Acquire) {
                    WORKER_WAIT_START => thread::park(),
                    WORKER_ABORT => return Ok(()),
                    _ => break,
                }
            }
            if let Some(core) = core_id {
                let _ = pin_current_thread_to_core(core);
            }
            while worker_stop.load(AtomicOrdering::Acquire) == WORKER_RUNNING {
                let clear_mask = !(DOORBELL_PARKED_BIT | DOORBELL_URGENT_BIT);
                let observed = DOORBELL.fetch_and(clear_mask, AtomicOrdering::SeqCst) & clear_mask;
                let _ = poll_fn(false);
                if worker_stop.load(AtomicOrdering::Acquire) != WORKER_RUNNING {
                    break;
                }
                wait_doorbell_interval(observed, &worker_stop, interval_ns);
            }
            if worker_stop.load(AtomicOrdering::Acquire) == WORKER_STOP_AND_FLUSH {
                poll_fn(true)
            } else {
                Ok(())
            }
        })?;
        let pending = PendingWorker { control: Some(PollerControl { stop_mode, handle }) };
        configure()?;
        let mut state = self.join_worker_with_mode(state, WORKER_HANDOFF)?;
        if self.has_external_lease() {
            return Err(io::Error::from(ZeroLogError::ConsumerBusy));
        }
        let control = pending.commit();
        state.worker_id = Some(control.handle.thread().id());
        state.generation = generation;
        // Publish installed state before permitting the worker's first callback.
        self.running.store(true, AtomicOrdering::Release);
        control.stop_mode.store(WORKER_RUNNING, AtomicOrdering::Release);
        control.handle.thread().unpark();
        state.control = Some(control);
        Ok(generation)
    }

    pub fn stop(&self) -> io::Result<()> {
        drop(self.join_worker(self.stable()?)?);
        Ok(())
    }

    pub(crate) fn stop_generation(
        &self,
        generation: u64,
        finish: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<bool> {
        let state = self.stable()?;
        if state.generation != generation || state.control.is_none() {
            return Ok(false);
        }
        let state = self.join_worker(state)?;
        let result = finish();
        drop(state);
        result.map(|_| true)
    }
}

pub fn cleanup_finished_buffers() {
    crate::logger::cleanup_finished_buffers();
}

/// # Safety
/// The caller must hold the global logger consumer lock.
pub(crate) unsafe fn cleanup_finished_buffers_exclusive(heap: &mut ReadyMessageHeap) {
    for index in 0..THREAD_BUFFER_REGISTRY.count().min(MAX_THREADS) {
        if !heap.has_slot(index as u8) {
            // SAFETY: caller holds exclusive consumer access and slot has no node in heap.
            unsafe { try_reclaim_slot(index, heap) };
        }
    }
}

unsafe fn try_reclaim_slot(index: usize, heap: &mut ReadyMessageHeap) {
    let pointer = THREAD_BUFFER_REGISTRY.get(index);
    if pointer.is_null() {
        return;
    }
    // SAFETY: caller holds exclusive consumer/reclamation access to live initialized buffer.
    if !unsafe { (*pointer).should_deallocate.load(AtomicOrdering::Acquire) } {
        return;
    }
    // SAFETY: the same consumer permit protects this live queue cursor.
    let is_empty = unsafe { SpscVarQueue::consumer_front(&raw const (*pointer).queue).is_none() };
    if is_empty {
        heap.clear_slot(index);
        REGISTERED_SLOTS[index >> 6].fetch_and(!(1u64 << (index & 63)), AtomicOrdering::Release);
        THREAD_BUFFER_REGISTRY.clear_slot(index);
        // SAFETY: retirement was acquired and the drained buffer has no remaining accesses.
        unsafe { ThreadBuffer::free_on_heap(pointer) };
    }
}

fn poll_interval_ns(interval_nanoseconds: i64) -> u64 {
    if interval_nanoseconds <= 0 { 1_000_000 } else { interval_nanoseconds as u64 }
}

static MEMBARRIER_STATE: AtomicU8 = AtomicU8::new(0);

#[doc(hidden)]
pub fn set_membarrier_state_for_test(state: u8) {
    MEMBARRIER_STATE.store(state, AtomicOrdering::Release);
}

#[inline]
pub(crate) fn membarrier_state() -> u8 {
    MEMBARRIER_STATE.load(AtomicOrdering::Acquire)
}

#[cfg(all(target_os = "linux", not(miri)))]
pub(crate) fn membarrier_private_expedited() -> bool {
    const CMD_PRIVATE_EXPEDITED: libc::c_int = 8;
    const CMD_REGISTER_PRIVATE_EXPEDITED: libc::c_int = 16;
    let mut state = MEMBARRIER_STATE.load(AtomicOrdering::Acquire);
    if state == 0 {
        // SAFETY: `SYS_membarrier` registration command takes zero flags and cpu_id.
        let rc =
            unsafe { libc::syscall(libc::SYS_membarrier, CMD_REGISTER_PRIVATE_EXPEDITED, 0, 0) };
        state = if rc == 0 { 1 } else { 2 };
        MEMBARRIER_STATE.store(state, AtomicOrdering::Release);
    }
    // SAFETY: `SYS_membarrier` private expedited barrier synchronizes process threads.
    state == 1 && unsafe { libc::syscall(libc::SYS_membarrier, CMD_PRIVATE_EXPEDITED, 0, 0) == 0 }
}

#[cfg(any(not(target_os = "linux"), miri))]
pub(crate) fn membarrier_private_expedited() -> bool {
    MEMBARRIER_STATE.store(2, AtomicOrdering::Release);
    false
}

#[cfg(test)]
#[path = "poller_tests.rs"]
mod tests;
