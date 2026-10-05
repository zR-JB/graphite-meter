//! When a multiplexed connection goes away and closes, as its streams, admitted work and receive credit allow; the
//! HTTP/2 and QUIC drivers share it.

use super::accept::SHUTDOWN_GRACE;
use crate::lane::Work;
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio::time::{Instant, Sleep, sleep, sleep_until};

/// A connection without streams, or one holding receive credit without admitted work, goes away after this long.
pub const IDLE: Duration = Duration::from_secs(15);

/// What a connection's driver does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Send GOAWAY: no new requests.
    GoAway,
    /// Close the connection.
    Close,
}

/// How admitted work that outlived the GOAWAY grace ends it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grace {
    /// The connection closes a fresh grace after that work ends, as HTTP/2 does.
    Fresh,
    /// The connection closes as soon as that work ends, as QUIC does.
    Once,
}

/// One connection's lifecycle, which its driver polls with what it observes.
#[derive(Debug)]
pub struct Lifecycle {
    work: Work,
    grace: Grace,
    stopping: bool,
    /// The start of the admitted work's idle period last observed; `None` while some runs.
    idle_since: Option<Instant>,
    /// When a credited connection without admitted work goes away.
    stale: Option<Pin<Box<Sleep>>>,
    /// When a connection without streams goes away.
    quiet: Option<Pin<Box<Sleep>>>,
    /// When a connection that went away closes.
    closing: Option<Pin<Box<Sleep>>>,
}

impl Lifecycle {
    /// The lifecycle of a connection whose admitted work is `work`.
    pub fn new(work: Work, grace: Grace) -> Self {
        Self {
            work,
            grace,
            stopping: false,
            idle_since: None,
            stale: None,
            quiet: None,
            closing: None,
        }
    }

    /// The server stops: the connection goes away and closes after the grace, whatever runs on it.
    pub fn stop(&mut self) {
        self.stopping = true;
    }

    /// The next event, given whether the connection raised its receive window and whether a request stream lives.
    /// Unadmitted requests count only as streams, so they never hold a credited connection open.
    pub fn poll(&mut self, cx: &mut Context<'_>, credited: bool, streams: bool) -> Poll<Event> {
        self.observe(credited);
        let stale = self
            .stale
            .as_mut()
            .is_some_and(|stale| stale.as_mut().poll(cx).is_ready());
        let quiet = self.quiet(cx, streams);
        if self.closing.is_none() && (self.stopping || stale || quiet) {
            self.closing = Some(Box::pin(sleep(SHUTDOWN_GRACE)));
            return Poll::Ready(Event::GoAway);
        }
        let closable = self.idle_since.is_some() || self.stopping;
        match &mut self.closing {
            Some(closing) if closable => closing.as_mut().poll(cx).map(|()| Event::Close),
            _ => Poll::Pending,
        }
    }

    /// Restarts the stale bound when the admitted work's idle period changes, and the grace when work that outlived
    /// it ends on a connection with fresh graces.
    fn observe(&mut self, credited: bool) {
        let since = self.work.idle_since();
        if since == self.idle_since {
            return;
        }
        self.idle_since = since;
        self.stale = since
            .filter(|_| credited)
            .map(|since| Box::pin(sleep_until(since + IDLE)));
        if let (Grace::Fresh, false, Some(since), Some(closing)) = (self.grace, self.stopping, since, &mut self.closing)
        {
            closing.as_mut().reset(since + SHUTDOWN_GRACE);
        }
    }

    /// Whether the connection has gone `IDLE` without a stream.
    fn quiet(&mut self, cx: &mut Context<'_>, streams: bool) -> bool {
        if streams {
            self.quiet = None;
            return false;
        }
        let quiet = self.quiet.get_or_insert_with(|| Box::pin(sleep(IDLE)));
        quiet.as_mut().poll(cx).is_ready()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::poll_fn;
    use tokio::time::advance;

    async fn poll(lifecycle: &mut Lifecycle, credited: bool, streams: bool) -> Poll<Event> {
        poll_fn(|cx| Poll::Ready(lifecycle.poll(cx, credited, streams))).await
    }

    const MILLI: Duration = Duration::from_millis(1);

    #[tokio::test(start_paused = true)]
    async fn a_credited_connection_goes_away_after_its_admitted_work_and_closes_after_the_grace() {
        for grace in [Grace::Fresh, Grace::Once] {
            let work = Work::default();
            let mut lifecycle = Lifecycle::new(work.clone(), grace);
            advance(IDLE * 2).await;
            assert!(poll(&mut lifecycle, false, true).await.is_pending(), "no credit, no idle rule");
            drop(work.start());
            assert!(poll(&mut lifecycle, true, true).await.is_pending());
            advance(IDLE - MILLI).await;
            assert!(poll(&mut lifecycle, true, true).await.is_pending(), "unadmitted streams do not count");
            advance(MILLI).await;
            assert_eq!(poll(&mut lifecycle, true, true).await, Poll::Ready(Event::GoAway));
            advance(SHUTDOWN_GRACE - MILLI).await;
            assert!(poll(&mut lifecycle, true, true).await.is_pending());
            advance(MILLI).await;
            assert_eq!(
                poll(&mut lifecycle, true, true).await,
                Poll::Ready(Event::Close),
                "unadmitted streams get 5 s"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn admitted_work_racing_the_goaway_runs_on_and_then_gets_its_grace() {
        for grace in [Grace::Fresh, Grace::Once] {
            let work = Work::default();
            let mut lifecycle = Lifecycle::new(work.clone(), grace);
            drop(work.start());
            assert!(poll(&mut lifecycle, true, true).await.is_pending());
            advance(IDLE).await;
            assert_eq!(poll(&mut lifecycle, true, true).await, Poll::Ready(Event::GoAway));
            let racing = work.start();
            advance(SHUTDOWN_GRACE * 4).await;
            assert!(poll(&mut lifecycle, true, true).await.is_pending(), "running work is not cut");
            drop(racing);
            if grace == Grace::Fresh {
                assert!(poll(&mut lifecycle, true, true).await.is_pending());
                advance(SHUTDOWN_GRACE - MILLI).await;
                assert!(poll(&mut lifecycle, true, true).await.is_pending());
                advance(MILLI).await;
            }
            assert_eq!(poll(&mut lifecycle, true, true).await, Poll::Ready(Event::Close));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_connection_without_streams_goes_away_after_fifteen_seconds() {
        let mut lifecycle = Lifecycle::new(Work::default(), Grace::Fresh);
        assert!(poll(&mut lifecycle, false, false).await.is_pending());
        advance(IDLE - MILLI).await;
        assert!(poll(&mut lifecycle, false, true).await.is_pending(), "a stream restarts the bound");
        assert!(poll(&mut lifecycle, false, false).await.is_pending());
        advance(IDLE - MILLI).await;
        assert!(poll(&mut lifecycle, false, false).await.is_pending());
        advance(MILLI).await;
        assert_eq!(poll(&mut lifecycle, false, false).await, Poll::Ready(Event::GoAway));
        advance(SHUTDOWN_GRACE).await;
        assert_eq!(poll(&mut lifecycle, false, false).await, Poll::Ready(Event::Close));
    }

    #[tokio::test(start_paused = true)]
    async fn a_stopping_connection_closes_after_the_grace_whatever_runs() {
        for grace in [Grace::Fresh, Grace::Once] {
            let work = Work::default();
            let mut lifecycle = Lifecycle::new(work.clone(), grace);
            let _running = work.start();
            assert!(poll(&mut lifecycle, true, true).await.is_pending());
            lifecycle.stop();
            assert_eq!(poll(&mut lifecycle, true, true).await, Poll::Ready(Event::GoAway));
            advance(SHUTDOWN_GRACE - MILLI).await;
            assert!(poll(&mut lifecycle, true, true).await.is_pending());
            advance(MILLI).await;
            assert_eq!(poll(&mut lifecycle, true, true).await, Poll::Ready(Event::Close));
        }
    }
}
