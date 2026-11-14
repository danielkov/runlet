use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

/// Cooperative, host-owned request to stop one run.
///
/// Clones share one flag. Pass a token to [`crate::RunOptions::cancellation`],
/// keep a clone, and call [`cancel`](Self::cancel) from any thread. The
/// executor then stops creating work: no further tool is dispatched, retry
/// backoff sleeps end early, boundaries neither retry nor catch, and the run
/// returns `RL6110 RUN_CANCELLED`.
///
/// A handler that is already running is not interrupted. It receives the same
/// token as [`crate::ToolContext::cancellation`] and should poll
/// [`is_cancelled`](Self::is_cancelled) or await [`cancelled`](Self::cancelled)
/// to abandon long work early.
#[derive(Clone, Default)]
pub struct CancellationToken {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    cancelled: AtomicBool,
    // Guards the waker list and serializes `cancel` against sleepers and
    // pollers, so a waiter that observed "not cancelled" under the lock is
    // always notified.
    wakers: Mutex<Vec<Waker>>,
    changed: Condvar,
}

impl CancellationToken {
    /// Creates a token that has not been cancelled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation. Idempotent; never blocks on handlers.
    pub fn cancel(&self) {
        let wakers = {
            let mut wakers = self.inner.wakers.lock().unwrap();
            self.inner.cancelled.store(true, Ordering::SeqCst);
            self.inner.changed.notify_all();
            std::mem::take(&mut *wakers)
        };
        for waker in wakers {
            waker.wake();
        }
    }

    /// Returns whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    /// Completes once cancellation has been requested. Executor-agnostic.
    pub fn cancelled(&self) -> Cancelled<'_> {
        Cancelled { token: self }
    }

    /// Sleeps for `duration` or until cancelled; returns whether cancelled.
    pub(crate) fn sleep(&self, duration: Duration) -> bool {
        let deadline = Instant::now() + duration;
        let mut guard = self.inner.wakers.lock().unwrap();
        loop {
            if self.is_cancelled() {
                return true;
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            if remaining.is_zero() {
                return false;
            }
            guard = self.inner.changed.wait_timeout(guard, remaining).unwrap().0;
        }
    }
}

impl std::fmt::Debug for CancellationToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancellationToken")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

/// Future returned by [`CancellationToken::cancelled`].
#[derive(Debug)]
pub struct Cancelled<'a> {
    token: &'a CancellationToken,
}

impl Future for Cancelled<'_> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let mut wakers = self.token.inner.wakers.lock().unwrap();
        if self.token.is_cancelled() {
            return Poll::Ready(());
        }
        if !wakers.iter().any(|waker| waker.will_wake(cx.waker())) {
            wakers.push(cx.waker().clone());
        }
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleep_returns_early_when_cancelled() {
        let token = CancellationToken::new();
        let canceller = token.clone();
        let started = Instant::now();
        std::thread::scope(|scope| {
            scope.spawn(move || {
                std::thread::sleep(Duration::from_millis(20));
                canceller.cancel();
            });
            assert!(token.sleep(Duration::from_secs(30)));
        });
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(!CancellationToken::new().sleep(Duration::from_millis(1)));
    }
}
