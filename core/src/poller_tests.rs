#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::AtomicUsize;

use super::*;

fn wait_until(mut ready: impl FnMut() -> bool) {
    for _ in 0..10_000 {
        if ready() {
            return;
        }
        thread::yield_now();
    }
    panic!("worker failed to reach the expected state");
}

#[test]
fn worker_observes_running_and_handoff() {
    let state = Arc::new(PollerState::default());
    let (calls, flushes) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let begin = |state: &Arc<PollerState>| {
        let (worker_state, calls, flushes) = (state.clone(), calls.clone(), flushes.clone());
        let cb = move |flush: bool| {
            assert!(worker_state.is_running());
            if flush {
                flushes.fetch_add(1, AtomicOrdering::Relaxed);
            } else {
                calls.fetch_add(1, AtomicOrdering::Release);
            }
            Ok(())
        };
        state.start_configured(1_000_000, cb, None, || Ok(())).unwrap()
    };
    let first = begin(&state);
    wait_until(|| calls.load(AtomicOrdering::Acquire) > 0);
    assert!(state.configure::<()>(|| panic!("active worker")).is_err());
    let second = begin(&state);
    assert!(second > first && flushes.load(AtomicOrdering::Relaxed) == 0);
    assert!(!state.stop_generation(first, || panic!("stale generation")).unwrap());
    assert!(state.is_running());
    let finished = AtomicBool::new(false);
    let finish_cb = || {
        assert!(!state.is_running());
        finished.store(true, AtomicOrdering::Relaxed);
        Ok(())
    };
    assert!(state.stop_generation(second, finish_cb).unwrap());
    assert!(finished.load(AtomicOrdering::Relaxed));
    assert_eq!(flushes.load(AtomicOrdering::Relaxed), 1);
    state.configure(|| Ok(())).unwrap();
    state.stop().unwrap();
}

struct Dropped(Arc<AtomicUsize>);
impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.fetch_add(1, AtomicOrdering::Release);
    }
}

#[test]
fn config_error_joins_waiting_worker() {
    let state = PollerState::default();
    state.start(1_000_000, |_| Ok(())).unwrap();
    let generation = state.lock_lifecycle().generation;
    for unwind in [false, true] {
        let (dropped, calls) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (token, worker_calls) = (Dropped(dropped.clone()), calls.clone());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let cb = move |_| {
                let _keep_alive = &token;
                worker_calls.fetch_add(1, AtomicOrdering::Relaxed);
                Ok(())
            };
            let cfg = || {
                if unwind {
                    panic!("configuration unwind");
                }
                Err(io::Error::other("configuration failure"))
            };
            state.start_configured(1_000_000, cb, None, cfg)
        }));
        if unwind {
            assert!(result.is_err());
        } else {
            assert_eq!(result.unwrap().unwrap_err().to_string(), "configuration failure");
        }
        assert_eq!(dropped.load(AtomicOrdering::Acquire), 1);
        assert_eq!(calls.load(AtomicOrdering::Relaxed), 0);
        assert!(state.is_running());
        let lifecycle = state.lock_lifecycle();
        assert_eq!(lifecycle.generation, generation);
        assert!(lifecycle.control.is_some() && !lifecycle.stopping);
    }
    state.stop().unwrap();
    state.configure(|| Ok(())).unwrap();
}

struct ExclusiveUse<'a>(&'a AtomicUsize);
impl<'a> ExclusiveUse<'a> {
    fn enter(active: &'a AtomicUsize) -> Self {
        assert_eq!(active.fetch_add(1, AtomicOrdering::AcqRel), 0);
        Self(active)
    }
}
impl Drop for ExclusiveUse<'_> {
    fn drop(&mut self) {
        assert_eq!(self.0.fetch_sub(1, AtomicOrdering::Release), 1);
    }
}

#[test]
fn concurrent_lifecycle_ops_serialize() {
    let (state, active) = (Arc::new(PollerState::default()), Arc::new(AtomicUsize::new(0)));
    let mut threads = Vec::new();
    for _ in 0..2 {
        let (state, active) = (state.clone(), active.clone());
        threads.push(thread::spawn(move || {
            for _ in 0..if cfg!(miri) { 2 } else { 8 } {
                let worker_active = active.clone();
                let cb = move |_| {
                    let _use = ExclusiveUse::enter(&worker_active);
                    thread::yield_now();
                    Ok(())
                };
                state.start(1_000_000, cb).unwrap();
                let _ = state.configure(|| {
                    let _use = ExclusiveUse::enter(&active);
                    thread::yield_now();
                    Ok(())
                });
                state.stop().unwrap();
            }
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    state.stop().unwrap();
    assert!(!state.is_running() && active.load(AtomicOrdering::Relaxed) == 0);
    let lifecycle = state.lock_lifecycle();
    assert!(lifecycle.control.is_none() && !lifecycle.stopping);
}

#[test]
fn worker_panic_allows_restart() {
    let state = PollerState::default();
    let entered = Arc::new(AtomicBool::new(false));
    let worker_entered = entered.clone();
    let cb = move |_| {
        worker_entered.store(true, AtomicOrdering::Release);
        panic!("worker failure");
    };
    state.start(1_000_000, cb).unwrap();
    wait_until(|| entered.load(AtomicOrdering::Acquire));
    assert!(state.stop().is_err());
    assert!(!state.is_running() && !state.lock_lifecycle().stopping);
    state.configure(|| Ok(())).unwrap();
    state.start(1_000_000, |_| Ok(())).unwrap();
    state.stop().unwrap();
}

#[test]
fn external_lease_excludes_concurrent_poller_start() {
    let state = Arc::new(PollerState::default());
    let (lease_state, poller_state) = (state.clone(), state.clone());
    let iterations = if cfg!(miri) { 4 } else { 40 };
    let t_lease = thread::spawn(move || {
        for _ in 0..iterations {
            if let Ok(lease) = lease_state.try_acquire_external_lease() {
                assert!(!lease_state.is_running());
                assert!(lease_state.has_external_lease());
                assert!(lease_state.start(1_000_000, |_| Ok(())).is_err());
                thread::yield_now();
                assert!(!lease_state.is_running());
                lease_state.release_external_lease();
                drop(lease);
            }
            thread::yield_now();
        }
    });
    let t_poller = thread::spawn(move || {
        for _ in 0..iterations {
            if poller_state.start(1_000_000, |_| Ok(())).is_ok() {
                assert!(!poller_state.has_external_lease());
                assert!(matches!(
                    poller_state.try_acquire_external_lease(),
                    Err(ZeroLogError::ConsumerBusy)
                ));
                thread::yield_now();
                poller_state.stop().unwrap();
            }
            thread::yield_now();
        }
    });
    t_lease.join().unwrap();
    t_poller.join().unwrap();
    state.stop().unwrap();
    assert!(!state.is_running() && !state.has_external_lease());
}
