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
    use graphite_meter_proto::{reason::FailureReason, route::Route};
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
        let failed = |at| Attempt { started: at, moved: false };
        let mut at = start;
        for expected in [300, 600, 1200] {
            assert_eq!(retry.after(busy(None), failed(at), at).unwrap(), ms(expected));
            at += ms(expected);
        }
        let standing = retry.after(busy(None), failed(at), at).unwrap_err();
        assert_eq!(standing.reason(), FailureReason::ServerBusy);
        let mut retry = Retry::default();
        assert_eq!(retry.after(busy(Some(1)), failed(start), start).unwrap(), ms(1000));
        assert_eq!(retry.after(busy(Some(9)), failed(start), start).unwrap(), BUSY_CAP);
    }

    #[test]
    fn an_instant_failure_pauses_500_ms_a_slow_one_none_and_moving_bytes_restarts_the_window() {
        let (start, mut retry) = (Instant::now(), Retry::default());
        let mut after = |fault, started, moved, now| {
            let attempt = Attempt { started: start + ms(started), moved };
            retry.after(fault, attempt, start + ms(now))
        };
        let lost = || Fault::Lost("reset".into());
        assert_eq!(after(lost(), 0, false, 10).unwrap(), QUICK_FAILURE);
        assert_eq!(after(lost(), 500, false, 1500).unwrap(), Duration::ZERO);
        assert_eq!(after(lost(), 1500, true, 2500).unwrap(), Duration::ZERO, "moving bytes is not failing");
        assert!(after(lost(), 2500, false, 4000).is_ok(), "the window starts at the next failing attempt");
        assert!(after(lost(), 4000, false, 4500).is_err());
        assert!(
            after(Fault::Malformed("frame".into()), 4500, false, 4500).is_err(),
            "a final fault stands"
        );
    }
}
