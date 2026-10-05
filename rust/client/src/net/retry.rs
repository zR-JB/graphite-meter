//! The one retry rule: work may fail without moving bytes for the redial window; busy answers back off and
//! quick failures pause.
use super::fault::{Class, Fault};
use std::time::{Duration, Instant};

/// How long work may fail without moving bytes before its fault stands.
pub const REDIAL_WINDOW: Duration = Duration::from_secs(2);
/// The pause after an attempt that failed sooner than this.
const QUICK_FAILURE: Duration = Duration::from_millis(500);
/// A busy answer's first wait, doubled up to the cap, which also bounds a server's `Retry-After`.
const BUSY_FIRST: Duration = Duration::from_millis(300);
const BUSY_CAP: Duration = Duration::from_millis(1200);

/// A failed attempt: when it started and whether it moved bytes first.
#[derive(Debug, Clone, Copy)]
pub struct Attempt {
    pub started: Instant,
    pub moved: bool,
}

/// One lane's or dial's retries; work that ends cleanly after moving bytes starts a new one.
#[derive(Debug, Default)]
pub struct Retry {
    failing_since: Option<Instant>,
    busy: Duration,
}

impl Retry {
    /// The pause before the next attempt, or the fault when it is final or work failed for the redial window.
    pub fn after(&mut self, fault: Fault, attempt: Attempt, now: Instant) -> Result<Duration, Fault> {
        let class = fault.class();
        if attempt.moved {
            self.failing_since = None;
        }
        let failing = (!attempt.moved).then(|| *self.failing_since.get_or_insert(attempt.started));
        if class == Class::Final || failing.is_some_and(|since| now.duration_since(since) >= REDIAL_WINDOW) {
            return Err(fault);
        }
        Ok(match class {
            Class::Busy(asked) => {
                self.busy = (self.busy * 2).clamp(BUSY_FIRST, BUSY_CAP);
                self.busy.max(asked).min(BUSY_CAP)
            }
            _ => {
                self.busy = Duration::ZERO;
                match now.duration_since(attempt.started) < QUICK_FAILURE {
                    true => QUICK_FAILURE,
                    false => Duration::ZERO,
                }
            }
        })
    }
}

/// `attempt`'s result once it succeeds or its fault stands.
pub async fn retrying<T, F: Future<Output = Result<T, Fault>>>(mut attempt: impl FnMut() -> F) -> Result<T, Fault> {
    let mut retry = Retry::default();
    loop {
        let started = Instant::now();
        let fault = match attempt().await {
            Ok(value) => return Ok(value),
            Err(fault) => fault,
        };
        let pause = retry.after(fault, Attempt { started, moved: false }, Instant::now())?;
        tokio::time::sleep(pause).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use graphite_meter_proto::{lane::LaneEnding, reason::FailureReason, route::Route};
    use http::StatusCode;

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    fn busy(seconds: Option<u64>) -> Fault {
        let retry_after = seconds.map(Duration::from_secs);
        Fault::Status {
            status: StatusCode::TOO_MANY_REQUESTS,
            from: Route::Upload,
            retry_after,
        }
    }

    #[test]
    fn busy_answers_wait_300_ms_doubling_to_1200_or_retry_after_then_stand_as_server_busy() {
        let (start, mut retry) = (Instant::now(), Retry::default());
        let mut at = start;
        for expected in [300, 600, 1200] {
            let attempt = Attempt { started: at, moved: false };
            assert_eq!(retry.after(busy(None), attempt, at).unwrap(), ms(expected));
            at += ms(expected);
        }
        let standing = retry.after(busy(None), Attempt { started: at, moved: false }, at);
        assert_eq!(standing.unwrap_err().reason(), FailureReason::ServerBusy);
        let mut retry = Retry::default();
        let fresh = Attempt { started: start, moved: false };
        assert_eq!(
            retry.after(busy(Some(1)), fresh, start).unwrap(),
            ms(1000),
            "Retry-After above the backoff"
        );
        assert_eq!(retry.after(busy(Some(9)), fresh, start).unwrap(), BUSY_CAP, "Retry-After under the cap");
    }

    #[test]
    fn an_instant_failure_pauses_500_ms_a_slow_one_none_and_moving_bytes_restarts_the_window() {
        let (start, mut retry) = (Instant::now(), Retry::default());
        let lost = || Fault::Lost("reset".into());
        let attempt = |started, moved| Attempt { started, moved };
        assert_eq!(retry.after(lost(), attempt(start, false), start + ms(10)).unwrap(), QUICK_FAILURE);
        assert_eq!(
            retry
                .after(lost(), attempt(start + ms(500), false), start + ms(1500))
                .unwrap(),
            Duration::ZERO
        );
        let moved = retry.after(lost(), attempt(start + ms(1500), true), start + ms(2500));
        assert_eq!(moved.unwrap(), Duration::ZERO, "a lane that moved bytes is not failing");
        let again = retry.after(lost(), attempt(start + ms(2500), false), start + ms(4000));
        assert!(again.is_ok(), "its window starts at its next failing attempt");
        assert!(
            retry
                .after(lost(), attempt(start + ms(4000), false), start + ms(4500))
                .is_err()
        );
    }

    #[test]
    fn endings_redial_and_final_faults_stand_at_once() {
        let (start, mut retry) = (Instant::now(), Retry::default());
        let attempt = Attempt { started: start, moved: false };
        assert_eq!(retry.after(Fault::Ended(LaneEnding::Idle), attempt, start).unwrap(), QUICK_FAILURE);
        assert_eq!(retry.after(Fault::Ended(LaneEnding::Shutdown), attempt, start).unwrap(), QUICK_FAILURE);
        assert!(retry.after(Fault::Ended(LaneEnding::Revoked), attempt, start).is_err());
        let origin = graphite_meter_proto::origin::Origin::parse("https://meter.example").unwrap();
        assert!(retry.after(Fault::SignIn(origin), attempt, start).is_err());
        assert!(retry.after(Fault::Malformed("frame".into()), attempt, start).is_err());
    }
}
