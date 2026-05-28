use std::{
    cell::Cell,
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicU64, Ordering},
    task::{Context, Poll},
};

/// The root span id used when no span is currently entered.
pub const ROOT_SPAN_ID: u64 = 0;

const MAX_SPAN_DEPTH: usize = 64;

static NEXT_SPAN_ID: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static SPAN_STACK: Cell<SpanStack> = const { Cell::new(SpanStack::new()) };
}

/// Numeric identifier for a span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpanId(u64);

impl SpanId {
    /// The root span id used outside any entered span.
    pub const ROOT: Self = Self(ROOT_SPAN_ID);

    /// Creates a span id from its raw numeric representation.
    #[inline]
    pub const fn from_u64(value: u64) -> Self {
        Self(value)
    }

    /// Returns the raw numeric representation.
    #[inline]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

impl Default for SpanId {
    fn default() -> Self {
        Self::ROOT
    }
}

/// Static metadata associated with a span call site.
#[derive(Debug)]
pub struct StaticSpanInfo {
    name: &'static str,
    location: &'static str,
}

impl StaticSpanInfo {
    /// Creates static span metadata.
    #[inline]
    pub const fn new(name: &'static str, location: &'static str) -> Self {
        Self { name, location }
    }

    /// Span name.
    #[inline]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Source location where the span was created.
    #[inline]
    pub const fn location(&self) -> &'static str {
        self.location
    }
}

/// A lightweight span handle.
#[derive(Debug, Clone, Copy)]
pub struct Span {
    id: SpanId,
    parent_id: SpanId,
    info: &'static StaticSpanInfo,
}

impl Span {
    /// Creates a new span under the currently entered span.
    #[inline]
    pub fn new(info: &'static StaticSpanInfo) -> Self {
        let id = NEXT_SPAN_ID.fetch_add(1, Ordering::Relaxed);
        Self { id: SpanId::from_u64(id), parent_id: current_span_id(), info }
    }

    /// Returns this span's id.
    #[inline]
    pub const fn id(self) -> SpanId {
        self.id
    }

    /// Returns this span's parent id.
    #[inline]
    pub const fn parent_id(self) -> SpanId {
        self.parent_id
    }

    /// Returns this span's static name.
    #[inline]
    pub const fn name(self) -> &'static str {
        self.info.name()
    }

    /// Returns this span's creation location.
    #[inline]
    pub const fn location(self) -> &'static str {
        self.info.location()
    }

    /// Enters the span on the current thread until the returned guard is dropped.
    #[inline]
    pub fn enter(self) -> SpanGuard {
        push_span(self.id);
        SpanGuard { id: self.id }
    }

    /// Runs a closure inside this span.
    #[inline]
    pub fn in_scope<F, R>(self, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        let _guard = self.enter();
        f()
    }
}

/// RAII guard returned by [`Span::enter`].
#[derive(Debug)]
pub struct SpanGuard {
    id: SpanId,
}

impl Drop for SpanGuard {
    #[inline]
    fn drop(&mut self) {
        pop_span(self.id);
    }
}

/// Returns the current thread's active span id.
#[inline]
pub fn current_span_id() -> SpanId {
    SpanId::from_u64(current_span_id_u64())
}

/// Returns the current thread's active span id as a raw integer.
///
/// This is used by the logging macro after level filtering has already passed,
/// keeping the disabled-log hot path to a single level check.
#[inline]
pub fn current_span_id_u64() -> u64 {
    SPAN_STACK.with(|stack| stack.get().current())
}

#[inline]
fn push_span(id: SpanId) {
    SPAN_STACK.with(|cell| {
        let mut stack = cell.get();
        stack.push(id.as_u64());
        cell.set(stack);
    });
}

#[inline]
fn pop_span(id: SpanId) {
    SPAN_STACK.with(|cell| {
        let mut stack = cell.get();
        stack.pop(id.as_u64());
        cell.set(stack);
    });
}

#[derive(Clone, Copy)]
struct SpanStack {
    ids: [u64; MAX_SPAN_DEPTH],
    depth: u8,
    overflow_depth: u16,
}

impl SpanStack {
    const fn new() -> Self {
        Self { ids: [ROOT_SPAN_ID; MAX_SPAN_DEPTH], depth: 0, overflow_depth: 0 }
    }

    #[inline]
    fn current(self) -> u64 {
        if self.depth == 0 { ROOT_SPAN_ID } else { self.ids[self.depth as usize - 1] }
    }

    #[inline]
    fn push(&mut self, id: u64) {
        let depth = self.depth as usize;
        if depth < MAX_SPAN_DEPTH {
            self.ids[depth] = id;
            self.depth += 1;
        } else {
            self.overflow_depth = self.overflow_depth.saturating_add(1);
        }
    }

    #[inline]
    fn pop(&mut self, id: u64) {
        if self.overflow_depth > 0 {
            self.overflow_depth -= 1;
            return;
        }

        if self.depth == 0 {
            return;
        }

        let top = self.depth as usize - 1;
        if self.ids[top] == id {
            self.ids[top] = ROOT_SPAN_ID;
            self.depth -= 1;
            return;
        }

        debug_assert_eq!(self.ids[top], id, "ferrilog span guards must be dropped in LIFO order");

        if let Some(position) =
            self.ids[..self.depth as usize].iter().rposition(|candidate| *candidate == id)
        {
            for index in position..top {
                self.ids[index] = self.ids[index + 1];
            }
            self.ids[top] = ROOT_SPAN_ID;
            self.depth -= 1;
        }
    }
}

/// Extension trait for carrying a span across future polls.
pub trait Instrument: Sized {
    /// Wraps `self` so that `span` is entered for each `poll`.
    #[inline]
    fn instrument(self, span: Span) -> Instrumented<Self> {
        Instrumented { inner: self, span }
    }
}

impl<T> Instrument for T {}

/// Future wrapper returned by [`Instrument::instrument`].
#[derive(Debug)]
#[must_use = "futures do nothing unless polled"]
pub struct Instrumented<F> {
    inner: F,
    span: Span,
}

impl<F: Future> Future for Instrumented<F> {
    type Output = F::Output;

    #[inline]
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Safety: `inner` is never moved out of the pinned `Instrumented`.
        let this = unsafe { self.get_unchecked_mut() };
        let _guard = this.span.enter();
        // Safety: projecting the pin to `inner` is sound because `inner`
        // remains in place for the duration of this poll.
        unsafe { Pin::new_unchecked(&mut this.inner) }.poll(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_SPAN: StaticSpanInfo = StaticSpanInfo::new("test", "span.rs:1");

    #[test]
    fn enter_and_drop_restore_current_span() {
        assert_eq!(current_span_id(), SpanId::ROOT);
        let outer = Span::new(&TEST_SPAN);
        {
            let _outer_guard = outer.enter();
            assert_eq!(current_span_id(), outer.id());

            let inner = Span::new(&TEST_SPAN);
            {
                let _inner_guard = inner.enter();
                assert_eq!(current_span_id(), inner.id());
            }

            assert_eq!(current_span_id(), outer.id());
        }
        assert_eq!(current_span_id(), SpanId::ROOT);
    }

    #[test]
    fn span_records_parent_id() {
        let parent = Span::new(&TEST_SPAN);
        let _guard = parent.enter();
        let child = Span::new(&TEST_SPAN);
        assert_eq!(child.parent_id(), parent.id());
    }
}
