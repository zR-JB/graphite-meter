//! When a multiplexed connection goes away and closes, as its admitted work and receive credit allow.
use crate::timeouts::{CONTROL, SHUTDOWN_GRACE};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};
use tokio::time::{Instant, Sleep};

/// Admitted operations on one connection; leftover receive credit is reclaimed the control bound after the last.
#[derive(Clone)]
pub(super) struct AdmittedWork(Arc<Mutex<WorkState>>);

struct WorkState {
    running: usize,
    idle_since: Instant,
}

impl AdmittedWork {
    pub(super) fn new() -> Self {
        Self(Arc::new(Mutex::new(WorkState {
            running: 0,
            idle_since: Instant::now(),
        })))
    }

    pub(super) fn admit(&self) -> Admitted {
        self.0.lock().expect("admitted work poisoned").running += 1;
        Admitted(self.clone())
    }

    pub(super) fn idle_since(&self) -> Option<Instant> {
        let work = self.0.lock().expect("admitted work poisoned");
        (work.running == 0).then_some(work.idle_since)
    }
}

pub(super) struct Admitted(AdmittedWork);

impl Drop for Admitted {
    fn drop(&mut self) {
        let mut work = self.0.0.lock().expect("admitted work poisoned");
        work.running -= 1;
        if work.running == 0 {
            work.idle_since = Instant::now();
        }
    }
}

/// What a connection's lifecycle asks of it.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Event {
    /// Send the peer away: no new requests.
    GoAway,
    /// Close the connection.
    Close,
}

/// Receive credit granted to a connection stays reserved while the peer may still fill its window. Once a credited
/// connection's admitted work has been idle for the control bound, the connection goes away (GOAWAY), and it closes
/// after the shutdown grace. Admitted work that raced the GOAWAY runs on: the connection closes only while its work
/// is idle, or once it is stopping.
pub(super) struct ConnectionLifecycle {
    work: AdmittedWork,
    /// Work that outlived the GOAWAY gets the grace again once it ends, as HTTP/2 does; otherwise, as for QUIC, the
    /// connection closes as soon as that work ends past the grace.
    fresh_grace: bool,
    stopping: bool,
    /// The start of the idle period last observed, or `None` while admitted work runs.
    idle_since: Option<Instant>,
    /// When the credit left over from the current idle period is reclaimed.
    stale: Option<Pin<Box<Sleep>>>,
    /// When a connection that went away closes.
    closing: Option<Pin<Box<Sleep>>>,
}

impl ConnectionLifecycle {
    pub(super) fn new(work: AdmittedWork, fresh_grace: bool) -> Self {
        Self {
            work,
            fresh_grace,
            stopping: false,
            idle_since: None,
            stale: None,
            closing: None,
        }
    }

    /// Observes the admitted work, and whether its current idle period left the connection's receive credit unused
    /// for the control bound. `credited` says whether the connection holds credit, when an idle period starts.
    pub(super) fn poll_stale(&mut self, cx: &mut Context<'_>, credited: impl FnOnce() -> bool) -> bool {
        let since = self.work.idle_since();
        if since != self.idle_since {
            self.idle_since = since;
            let credited = credited();
            self.stale = since
                .filter(|_| credited)
                .map(|since| Box::pin(tokio::time::sleep_until(since + CONTROL)));
            if self.fresh_grace
                && !self.stopping
                && let (Some(since), Some(closing)) = (since, &mut self.closing)
            {
                closing.as_mut().reset(since + SHUTDOWN_GRACE);
            }
        }
        self.stale
            .as_mut()
            .is_some_and(|stale| stale.as_mut().poll(cx).is_ready())
    }

    /// The server stops: the connection closes after the grace, whether or not its work is idle.
    pub(super) fn stop(&mut self) {
        self.stopping = true;
    }

    pub(super) fn stopping(&self) -> bool {
        self.stopping
    }

    /// Starts the grace before the connection closes: `true` the first time, when the caller sends the GOAWAY.
    pub(super) fn go_away(&mut self) -> bool {
        let first = self.closing.is_none();
        if first {
            self.closing = Some(Box::pin(tokio::time::sleep(SHUTDOWN_GRACE)));
        }
        first
    }

    pub(super) fn going_away(&self) -> bool {
        self.closing.is_some()
    }

    /// Ready when a connection that went away closes: once the grace has passed, while its work is idle or it stops.
    pub(super) fn poll_close(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        match &mut self.closing {
            Some(closing) if self.idle_since.is_some() || self.stopping => closing.as_mut().poll(cx),
            _ => Poll::Pending,
        }
    }

    /// Both at once, for a connection whose stop the caller handles itself.
    pub(super) fn poll(&mut self, cx: &mut Context<'_>, credited: impl FnOnce() -> bool) -> Poll<Event> {
        if self.poll_stale(cx, credited) && self.go_away() {
            return Poll::Ready(Event::GoAway);
        }
        self.poll_close(cx).map(|()| Event::Close)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    async fn poll(lifecycle: &mut ConnectionLifecycle, credited: bool) -> Poll<Event> {
        std::future::poll_fn(|cx| Poll::Ready(lifecycle.poll(cx, || credited))).await
    }

    async fn advance(duration: Duration) {
        tokio::time::advance(duration).await;
    }

    #[tokio::test(start_paused = true)]
    async fn credit_left_unused_through_the_control_bound_sends_the_connection_away() {
        let work = AdmittedWork::new();
        let mut lifecycle = ConnectionLifecycle::new(work.clone(), true);
        // An idle connection that holds no credit has nothing to reclaim.
        assert!(poll(&mut lifecycle, false).await.is_pending());
        advance(CONTROL * 2).await;
        assert!(poll(&mut lifecycle, false).await.is_pending());

        drop(work.admit());
        assert!(poll(&mut lifecycle, true).await.is_pending());
        advance(CONTROL - Duration::from_millis(1)).await;
        assert!(poll(&mut lifecycle, true).await.is_pending());
        // New work before the bound starts the idle period over.
        drop(work.admit());
        advance(Duration::from_millis(1)).await;
        assert!(poll(&mut lifecycle, true).await.is_pending());
        advance(CONTROL - Duration::from_millis(2)).await;
        assert!(poll(&mut lifecycle, true).await.is_pending());
        advance(Duration::from_millis(1)).await;
        assert_eq!(poll(&mut lifecycle, true).await, Poll::Ready(Event::GoAway));
        // The connection closes once the grace has passed, and only then.
        advance(SHUTDOWN_GRACE - Duration::from_millis(1)).await;
        assert!(poll(&mut lifecycle, true).await.is_pending());
        advance(Duration::from_millis(1)).await;
        assert_eq!(poll(&mut lifecycle, true).await, Poll::Ready(Event::Close));
    }

    #[tokio::test(start_paused = true)]
    async fn work_that_raced_the_goaway_runs_on_and_then_closes_with_or_without_a_fresh_grace() {
        for fresh_grace in [true, false] {
            let work = AdmittedWork::new();
            let mut lifecycle = ConnectionLifecycle::new(work.clone(), fresh_grace);
            drop(work.admit());
            assert!(poll(&mut lifecycle, true).await.is_pending());
            advance(CONTROL).await;
            assert_eq!(poll(&mut lifecycle, true).await, Poll::Ready(Event::GoAway));
            let admitted = work.admit();
            advance(SHUTDOWN_GRACE * 2).await;
            assert!(poll(&mut lifecycle, true).await.is_pending(), "running work closed");
            drop(admitted);
            if fresh_grace {
                assert!(poll(&mut lifecycle, true).await.is_pending());
                advance(SHUTDOWN_GRACE - Duration::from_millis(1)).await;
                assert!(poll(&mut lifecycle, true).await.is_pending());
                advance(Duration::from_millis(1)).await;
            }
            assert_eq!(poll(&mut lifecycle, true).await, Poll::Ready(Event::Close));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_stopping_connection_closes_after_the_grace_while_its_work_runs() {
        let work = AdmittedWork::new();
        let mut lifecycle = ConnectionLifecycle::new(work.clone(), true);
        let _running = work.admit();
        assert!(poll(&mut lifecycle, true).await.is_pending());
        lifecycle.stop();
        assert!(lifecycle.go_away() && !lifecycle.go_away());
        advance(SHUTDOWN_GRACE - Duration::from_millis(1)).await;
        assert!(poll(&mut lifecycle, true).await.is_pending());
        advance(Duration::from_millis(1)).await;
        assert_eq!(poll(&mut lifecycle, true).await, Poll::Ready(Event::Close));
    }
}
