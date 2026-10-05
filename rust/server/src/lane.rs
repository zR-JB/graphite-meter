//! An admitted request's time bounds, the one place a lane's ending is decided, and a connection's admitted work.

use crate::{auth::AuthLease, limits::Hold, lock};
use graphite_meter_proto::lane::{IDLE_BOUND, LaneEnding};
use std::{
    future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::time::{Instant, sleep_until};
use tokio_util::sync::CancellationToken;

const RUNNING: u8 = u8::MAX;

/// An admitted request, WebSocket bus or WebTransport session; clones share it, and the last one releases its
/// admission and its work on the connection.
#[derive(Debug, Clone)]
pub struct Lane(Arc<State>);

#[derive(Debug)]
struct State {
    start: Instant,
    deadline: Instant,
    /// Nanoseconds from `start` to the peer's last movement.
    moved: AtomicU64,
    /// The index of the decided ending in `LaneEnding::ALL`, or `RUNNING`.
    ending: AtomicU8,
    decided: CancellationToken,
    shutdown: CancellationToken,
    auth: Option<AuthLease>,
    _hold: Hold,
    _work: WorkGuard,
}

impl Lane {
    /// A lane holding `hold` until its `lifetime` ends, on `shutdown` or on revocation of the request's sign-in.
    pub(crate) fn start(
        hold: Hold,
        lifetime: Duration,
        work: &Work,
        shutdown: &CancellationToken,
        auth: Option<&AuthLease>,
    ) -> Self {
        let start = Instant::now();
        Self(Arc::new(State {
            start,
            deadline: start + lifetime,
            moved: AtomicU64::new(0),
            ending: AtomicU8::new(RUNNING),
            decided: CancellationToken::new(),
            shutdown: shutdown.clone(),
            auth: auth.cloned(),
            _hold: hold,
            _work: work.start(),
        }))
    }

    /// Records traffic from the peer, which restarts the idle bound.
    pub fn moved(&self) {
        let nanos = u64::try_from(self.0.start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.0.moved.store(nanos, Ordering::Relaxed);
    }

    pub fn ending(&self) -> Option<LaneEnding> {
        LaneEnding::ALL
            .get(usize::from(self.0.ending.load(Ordering::Acquire)))
            .copied()
    }

    /// The first cause to end the lane: revocation, shutdown, its lifetime, `IDLE_BOUND` without movement, or
    /// `finish`. Every caller sees the same ending; causes due at once rank in that order.
    pub async fn ended(&self) -> LaneEnding {
        let state = &self.0;
        let revoked = async {
            match &state.auth {
                Some(auth) => auth.ended().await,
                None => future::pending().await,
            }
        };
        let cause = tokio::select! {
            biased;
            () = state.decided.cancelled() => LaneEnding::Finished,
            () = revoked => LaneEnding::Revoked,
            () = state.shutdown.cancelled() => LaneEnding::Shutdown,
            () = sleep_until(state.deadline) => LaneEnding::Lifetime,
            () = self.idle() => LaneEnding::Idle,
        };
        self.decide(cause)
    }

    /// The ending revocation, shutdown or the lifetime calls for by now, decided without waiting: a check cheap
    /// enough for every chunk of a transfer, which then needs `ended` only while it waits.
    pub fn due(&self) -> Option<LaneEnding> {
        if let Some(ending) = self.ending() {
            return Some(ending);
        }
        let (state, now) = (&self.0, Instant::now());
        let cause = if state.auth.as_ref().is_some_and(|auth| auth.is_ended(now)) {
            LaneEnding::Revoked
        } else if state.shutdown.is_cancelled() {
            LaneEnding::Shutdown
        } else if now >= state.deadline {
            LaneEnding::Lifetime
        } else {
            return None;
        };
        Some(self.decide(cause))
    }

    /// Ends the lane as finished unless another cause came first, and returns the ending that won.
    pub fn finish(&self) -> LaneEnding {
        self.decide(LaneEnding::Finished)
    }

    async fn idle(&self) {
        loop {
            let moved = Duration::from_nanos(self.0.moved.load(Ordering::Relaxed));
            let due = self.0.start + moved + IDLE_BOUND;
            if Instant::now() >= due {
                return;
            }
            sleep_until(due).await;
        }
    }

    fn decide(&self, cause: LaneEnding) -> LaneEnding {
        let index = LaneEnding::ALL
            .iter()
            .position(|ending| *ending == cause)
            .expect("every ending is listed");
        let index = u8::try_from(index).expect("five endings");
        match self
            .0
            .ending
            .compare_exchange(RUNNING, index, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => {
                self.0.decided.cancel();
                cause
            }
            Err(decided) => LaneEnding::ALL[usize::from(decided)],
        }
    }
}

/// The admitted work running on one connection; its lifecycle reads when the last of it ended.
#[derive(Debug, Clone)]
pub struct Work(Arc<Mutex<WorkState>>);

#[derive(Debug)]
struct WorkState {
    running: usize,
    idle_since: Instant,
}

impl Default for Work {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(WorkState { running: 0, idle_since: Instant::now() })))
    }
}

impl Work {
    pub fn start(&self) -> WorkGuard {
        lock(&self.0).running += 1;
        WorkGuard(self.clone())
    }

    /// When the last admitted work ended, or the connection began; `None` while some runs.
    pub fn idle_since(&self) -> Option<Instant> {
        let state = lock(&self.0);
        (state.running == 0).then_some(state.idle_since)
    }
}

/// One piece of admitted work, ended when dropped.
#[derive(Debug)]
pub struct WorkGuard(Work);

impl Drop for WorkGuard {
    fn drop(&mut self) {
        let mut state = lock(&self.0.0);
        state.running -= 1;
        if state.running == 0 {
            state.idle_since = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        auth::{NewLogin, Store},
        exchange::Exchange,
        limits::Quota,
        peer::ClientKeys,
    };

    struct Fixture {
        quota: Quota,
        work: Work,
        shutdown: CancellationToken,
        store: Store,
        login: NewLogin,
    }

    impl Fixture {
        fn new() -> Self {
            let store = Store::default();
            let login = store.sign_in("p", "P", "local").unwrap();
            Self {
                quota: Quota::new(10, 10),
                work: Work::default(),
                shutdown: CancellationToken::new(),
                store,
                login,
            }
        }

        fn lane(&self, lifetime: Duration) -> Lane {
            let hold = self.quota.acquire(&ClientKeys::Exempt, 1).unwrap();
            let auth = self.store.cookie(&self.login.token).unwrap();
            Exchange::start().admit(ClientKeys::Exempt, hold, lifetime, &self.work, &self.shutdown, Some(&auth))
        }

        fn revoke(&self) {
            assert!(self.store.sign_out(self.login.key, false));
        }
    }

    const LONG: Duration = Duration::from_secs(3600);

    /// The lane's ending and how long after admission it came.
    async fn ending(lane: &Lane) -> (LaneEnding, Duration) {
        let start = Instant::now();
        let ending = lane.ended().await;
        (ending, start.elapsed())
    }

    #[tokio::test(start_paused = true)]
    async fn a_lane_ends_idle_exactly_thirty_seconds_after_its_last_movement() {
        let fixture = Fixture::new();
        assert_eq!(ending(&fixture.lane(LONG)).await, (LaneEnding::Idle, IDLE_BOUND));
        let lane = fixture.lane(LONG);
        let mover = lane.clone();
        tokio::spawn(async move {
            for _ in 0..3 {
                tokio::time::sleep(Duration::from_secs(20)).await;
                mover.moved();
            }
        });
        assert_eq!(ending(&lane).await, (LaneEnding::Idle, Duration::from_secs(90)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_due_cause_is_decided_without_waiting() {
        let fixture = Fixture::new();
        let lane = fixture.lane(Duration::from_secs(10));
        assert_eq!(lane.due(), None);
        tokio::time::advance(Duration::from_secs(10)).await;
        assert_eq!((lane.due(), lane.ending()), (Some(LaneEnding::Lifetime), Some(LaneEnding::Lifetime)));
        let lane = fixture.lane(LONG);
        fixture.shutdown.cancel();
        fixture.revoke();
        assert_eq!(lane.due(), Some(LaneEnding::Revoked));
        assert_eq!((lane.finish(), lane.ended().await), (LaneEnding::Revoked, LaneEnding::Revoked));
        let fixture = Fixture::new();
        let lane = fixture.lane(LONG);
        assert_eq!(lane.finish(), LaneEnding::Finished);
        fixture.shutdown.cancel();
        assert_eq!(lane.due(), Some(LaneEnding::Finished), "a decided ending stays");
    }

    #[tokio::test(start_paused = true)]
    async fn a_moving_lane_ends_at_its_lifetime() {
        let fixture = Fixture::new();
        let lane = fixture.lane(Duration::from_secs(45));
        let mover = lane.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;
                mover.moved();
            }
        });
        assert_eq!(ending(&lane).await, (LaneEnding::Lifetime, Duration::from_secs(45)));
    }

    #[tokio::test(start_paused = true)]
    async fn revocation_and_shutdown_end_a_lane_at_once() {
        let fixture = Fixture::new();
        let lane = fixture.lane(LONG);
        tokio::time::sleep(Duration::from_secs(5)).await;
        fixture.revoke();
        assert_eq!(ending(&lane).await, (LaneEnding::Revoked, Duration::ZERO));
        let fixture = Fixture::new();
        let lane = fixture.lane(LONG);
        fixture.shutdown.cancel();
        assert_eq!(ending(&lane).await, (LaneEnding::Shutdown, Duration::ZERO));
        let anonymous = Exchange::start().admit(
            ClientKeys::Exempt,
            fixture.quota.acquire(&ClientKeys::Exempt, 1).unwrap(),
            LONG,
            &fixture.work,
            &CancellationToken::new(),
            None,
        );
        assert_eq!(ending(&anonymous).await.0, LaneEnding::Idle, "a lane without sign-in is never revoked");
    }

    #[tokio::test(start_paused = true)]
    async fn the_first_cause_wins_and_every_observer_sees_it() {
        let fixture = Fixture::new();
        let lane = fixture.lane(LONG);
        let observer = tokio::spawn({
            let lane = lane.clone();
            async move { lane.ended().await }
        });
        assert_eq!(lane.finish(), LaneEnding::Finished);
        fixture.shutdown.cancel();
        assert_eq!(observer.await.unwrap(), LaneEnding::Finished);
        assert_eq!(
            (lane.ended().await, lane.finish(), lane.ending()),
            (LaneEnding::Finished, LaneEnding::Finished, Some(LaneEnding::Finished))
        );

        let fixture = Fixture::new();
        let lane = fixture.lane(IDLE_BOUND);
        assert_eq!(
            ending(&lane).await,
            (LaneEnding::Lifetime, IDLE_BOUND),
            "a lifetime due with the idle bound wins"
        );
        fixture.revoke();
        assert_eq!((lane.finish(), lane.ended().await), (LaneEnding::Lifetime, LaneEnding::Lifetime));

        let fixture = Fixture::new();
        let lane = fixture.lane(LONG);
        fixture.revoke();
        fixture.shutdown.cancel();
        assert_eq!(lane.ended().await, LaneEnding::Revoked, "revocation outranks a shutdown due at once");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn racing_causes_decide_one_ending() {
        for _ in 0..100 {
            let fixture = Fixture::new();
            let lane = fixture.lane(LONG);
            let observers: Vec<_> = (0..4)
                .map(|_| {
                    let lane = lane.clone();
                    tokio::spawn(async move { lane.ended().await })
                })
                .collect();
            let finisher = tokio::spawn({
                let lane = lane.clone();
                async move { lane.finish() }
            });
            fixture.shutdown.cancel();
            let decided = finisher.await.unwrap();
            for observer in observers {
                assert_eq!(observer.await.unwrap(), decided);
            }
            assert!(matches!(decided, LaneEnding::Finished | LaneEnding::Shutdown));
            assert_eq!(lane.ending(), Some(decided));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_the_last_clone_releases_admission_and_work() {
        let fixture = Fixture::new();
        let start = Instant::now();
        assert_eq!(fixture.work.idle_since(), Some(start));
        let (first, second) = (fixture.lane(LONG), fixture.lane(LONG));
        let clone = first.clone();
        assert_eq!((fixture.quota.usage().active, fixture.work.idle_since()), (2, None));
        tokio::time::sleep(Duration::from_secs(3)).await;
        drop((first, second));
        assert_eq!(fixture.work.idle_since(), None, "a clone keeps the lane");
        tokio::time::sleep(Duration::from_secs(2)).await;
        drop(clone);
        assert_eq!(fixture.quota.usage().active, 0);
        assert_eq!(fixture.work.idle_since(), Some(start + Duration::from_secs(5)));
    }
}
