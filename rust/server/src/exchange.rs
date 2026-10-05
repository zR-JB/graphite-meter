//! A request's time bound before admission, and what its transport sees of whether and for whom it was admitted.

use crate::{
    auth::AuthLease,
    lane::{Lane, Work},
    limits::Hold,
    peer::ClientKeys,
};
use std::{
    future::{self, Future, poll_fn},
    pin::pin,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::time::{Instant, sleep_until};
use tokio_util::sync::CancellationToken;

/// Every exchange has this long in all, request and reply, until it is admitted.
pub const EXCHANGE_BOUND: Duration = Duration::from_secs(15);

/// A request before admission. Its bound never extends its connection's idle period.
#[derive(Debug)]
pub struct Exchange {
    deadline: Instant,
    admitted: Arc<OnceLock<ClientKeys>>,
}

impl Exchange {
    pub fn start() -> Self {
        Self::until(Instant::now() + EXCHANGE_BOUND)
    }

    /// An exchange that began before its request was parsed, such as at the first byte of an HTTP/1 head.
    pub fn until(deadline: Instant) -> Self {
        Self { deadline, admitted: Arc::default() }
    }

    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// What the transport keeps of the exchange it hands to the app.
    pub fn watch(&self) -> Watch {
        Watch { deadline: self.deadline, admitted: self.admitted.clone() }
    }

    /// Admits the request of `keys` as a lane holding `hold`, whose `lifetime` replaces the exchange bound. It
    /// ends on `shutdown`, and on revocation of the request's sign-in.
    pub fn admit(
        self,
        keys: ClientKeys,
        hold: Hold,
        lifetime: Duration,
        work: &Work,
        shutdown: &CancellationToken,
        auth: Option<&AuthLease>,
    ) -> Lane {
        let _ = self.admitted.set(keys);
        Lane::start(hold, lifetime, work, shutdown, auth)
    }
}

/// A transport's view of an exchange: its deadline, and whether and for whom it was admitted.
#[derive(Debug, Clone)]
pub struct Watch {
    deadline: Instant,
    admitted: Arc<OnceLock<ClientKeys>>,
}

impl Watch {
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// The admitted client's keys, which also fund its connection's receive window; `None` before admission.
    pub fn admitted(&self) -> Option<&ClientKeys> {
        self.admitted.get()
    }

    /// `served`, counted as admitted work on `work` from the poll that sees the exchange admitted until it ends.
    pub async fn counted<T>(&self, served: impl Future<Output = T>, work: &Work) -> T {
        let mut served = pin!(served);
        let mut guard = None;
        poll_fn(|cx| {
            let polled = served.as_mut().poll(cx);
            if guard.is_none() && self.admitted().is_some() {
                guard = Some(work.start());
            }
            polled
        })
        .await
    }

    /// Completes at the deadline unless the exchange was admitted by then; a lane bounds it after admission.
    pub async fn expired(&self) {
        sleep_until(self.deadline).await;
        if self.admitted().is_some() {
            future::pending::<()>().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::Quota;

    #[tokio::test(start_paused = true)]
    async fn an_exchange_expires_after_fifteen_seconds_unless_admitted() {
        let start = Instant::now();
        let exchange = Exchange::start();
        let watch = exchange.watch();
        watch.expired().await;
        assert_eq!((start.elapsed(), watch.admitted()), (EXCHANGE_BOUND, None));
        let exchange = Exchange::start();
        let watch = exchange.watch();
        let hold = Quota::new(10, 10).acquire(&ClientKeys::Exempt, 1).unwrap();
        let keys = ClientKeys::address("192.0.2.1".parse().unwrap());
        let lifetime = Duration::from_secs(3600);
        let _lane = exchange.admit(keys.clone(), hold, lifetime, &Work::default(), &CancellationToken::new(), None);
        assert_eq!(watch.admitted(), Some(&keys));
        let expired = tokio::time::timeout(EXCHANGE_BOUND * 2, watch.expired()).await;
        assert!(expired.is_err(), "an admitted exchange never expires");
    }
}
