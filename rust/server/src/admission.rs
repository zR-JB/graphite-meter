//! Leases (`api/discovery.md`): admission a client takes once, before it measures, which its work then presents.

use crate::{
    auth::{AuthLease, Via, digest, random},
    limits::{Full, Hold, Quotas},
    lock,
    peer::ClientKeys,
};
use graphite_meter_proto::{
    lease::{LEASE_HEADER, LEASE_PARAMETER, UNUSED_BOUND},
    refusal::Refusal,
};
use http::{HeaderValue, Request};
use std::{
    collections::HashMap,
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::{
    sync::Notify,
    time::{Instant, sleep_until},
};
use tokio_util::sync::CancellationToken;

/// What a lease holds: measurements, of them WebTransport transfer sessions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Grant {
    pub measurements: usize,
    pub sessions: usize,
}

#[derive(Clone, Default)]
pub struct Leases(Arc<Mutex<HashMap<[u8; 32], Arc<Lease>>>>);

pub struct Lease {
    id: u64,
    digest: [u8; 32],
    /// The creator's keys, which the lease's holds and any work past its grant charge.
    keys: ClientKeys,
    auth: Option<AuthLease>,
    origin: Option<HeaderValue>,
    state: Mutex<State>,
    changed: Notify,
    ended: CancellationToken,
}

#[derive(Default)]
struct State {
    grant: Grant,
    /// Used of the grant: its slots taken.
    used: Grant,
    /// Holds of the grant's measurements, then of its sessions apart from their measurements.
    measurements: Vec<Hold>,
    sessions: Vec<Hold>,
    /// Work presenting the lease, past its grant too.
    live: usize,
    unused_since: Option<Instant>,
}

impl fmt::Debug for Lease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("Lease").field(&self.id).finish()
    }
}

/// A lease's share held by one admitted request: a slot of its grant, or a hold past it.
#[derive(Debug)]
pub struct Slot {
    lease: Arc<Lease>,
    session: bool,
    granted: bool,
    _past: Option<Hold>,
}

/// What an admitted request holds until its lane ends.
#[derive(Debug)]
pub enum Admission {
    Quota(Hold),
    Lease(Slot),
}

impl From<Hold> for Admission {
    fn from(hold: Hold) -> Self {
        Self::Quota(hold)
    }
}

/// The token a request presents: the field, else `lease=` in its URL.
pub fn presented<B>(request: &Request<B>) -> Option<String> {
    let field = crate::peer::single(request.headers(), &LEASE_HEADER).map(str::to_owned);
    field.or_else(|| crate::app::query::get(request.uri().query(), LEASE_PARAMETER))
}

impl Leases {
    /// A new lease for `keys`, holding `grant` and bound to the sign-in and the `Origin` that asked; it ends unused,
    /// with its sign-in, or on `shutdown`.
    pub fn create(
        &self,
        quotas: &Quotas,
        keys: ClientKeys,
        auth: Option<&AuthLease>,
        origin: Option<&HeaderValue>,
        grant: Grant,
        shutdown: &CancellationToken,
    ) -> Result<(String, Arc<Lease>), Refusal> {
        static IDS: AtomicU64 = AtomicU64::new(0);
        let token = format!("gml_{}", random::<32>());
        let lease = Arc::new(Lease {
            id: IDS.fetch_add(1, Ordering::Relaxed),
            digest: digest(&token),
            keys,
            // Its work presents it in place of the cookie or grant, as a bearer would.
            auth: auth.map(|auth| auth.presented(Via::Bearer(auth.browser().cloned()))),
            origin: origin.cloned(),
            state: Mutex::default(),
            changed: Notify::new(),
            ended: CancellationToken::new(),
        });
        lease.resize(quotas, grant)?;
        lock(&self.0).insert(lease.digest, lease.clone());
        let (leases, watched, shutdown) = (self.clone(), lease.clone(), shutdown.clone());
        tokio::spawn(async move {
            let signed_out = async {
                match &watched.auth {
                    Some(auth) => auth.ended().await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                () = watched.ended.cancelled() => {}
                () = watched.unused() => {}
                () = signed_out => {}
                () = shutdown.cancelled() => {}
            }
            leases.end(&watched);
        });
        Ok((token, lease))
    }

    /// The lease `token` names, none when a request from another `Origin` presents it.
    pub fn find(&self, token: &str, origin: Option<&HeaderValue>) -> Option<Arc<Lease>> {
        let lease = lock(&self.0).get(&digest(token)).cloned()?;
        (origin.is_none() || origin == lease.origin.as_ref()).then_some(lease)
    }

    /// Ends `lease`: nothing presents it again, and it releases what its live work does not hold.
    pub fn end(&self, lease: &Lease) {
        lock(&self.0).remove(&lease.digest);
        lease.ended.cancel();
        let mut state = lock(&lease.state);
        state.grant = Grant::default();
        state.fit();
    }
}

impl Lease {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn keys(&self) -> &ClientKeys {
        &self.keys
    }

    pub fn auth(&self) -> Option<&AuthLease> {
        self.auth.as_ref()
    }

    pub fn grant(&self) -> Grant {
        lock(&self.state).grant
    }

    /// Holds `grant` from now, charged to the creator's keys; a raise that does not fit changes nothing, nor does
    /// one that lost the race with the lease's end.
    pub fn resize(&self, quotas: &Quotas, grant: Grant) -> Result<(), Refusal> {
        let mut state = lock(&self.state);
        if self.ended.is_cancelled() {
            return Ok(());
        }
        let (measurements, sessions) = state.wanted(grant);
        let mut raised = Vec::new();
        for _ in state.sessions.len()..sessions {
            raised.push(quotas.session_beside_handler(&self.keys)?);
        }
        let mut more = Vec::new();
        for _ in state.measurements.len()..measurements {
            more.push(quotas.operation(&self.keys)?);
        }
        state.sessions.append(&mut raised);
        state.measurements.append(&mut more);
        state.grant = grant;
        state.fit();
        Ok(())
    }

    /// Admits one request against the grant, or past it against the creator's share; `session` for a WebTransport
    /// transfer session.
    pub fn admit(self: &Arc<Self>, quotas: &Quotas, session: bool) -> Result<Admission, Full> {
        let mut state = lock(&self.state);
        let State { grant, used, .. } = &mut *state;
        let granted =
            used.measurements < grant.measurements && (!session || used.sessions < grant.sessions);
        let past = match (granted, session) {
            (true, _) => None,
            (false, true) => Some(quotas.session(&self.keys)?),
            (false, false) => Some(quotas.operation(&self.keys)?),
        };
        if granted {
            used.measurements += 1;
            used.sessions += usize::from(session);
        }
        state.live += 1;
        state.unused_since = None;
        Ok(Admission::Lease(Slot {
            lease: self.clone(),
            session,
            granted,
            _past: past,
        }))
    }

    /// Completes once nothing presenting the lease has been live for `UNUSED_BOUND`.
    async fn unused(&self) {
        loop {
            let changed = self.changed.notified();
            let since = {
                let mut state = lock(&self.state);
                if state.live > 0 {
                    None
                } else {
                    Some(*state.unused_since.get_or_insert_with(Instant::now))
                }
            };
            match since {
                Some(since) if since.elapsed() >= UNUSED_BOUND => return,
                Some(since) => tokio::select! {
                    () = sleep_until(since + UNUSED_BOUND) => {}
                    () = changed => {}
                },
                None => changed.await,
            }
        }
    }
}

impl State {
    /// The holds that keep the grant and the slots in use: measurements, and sessions.
    fn wanted(&self, grant: Grant) -> (usize, usize) {
        (
            grant.measurements.max(self.used.measurements),
            grant.sessions.max(self.used.sessions),
        )
    }

    /// Releases holds no longer wanted, as after the grant was lowered or a slot ended.
    fn fit(&mut self) {
        let (measurements, sessions) = self.wanted(self.grant);
        self.measurements.truncate(measurements);
        self.sessions.truncate(sessions);
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut state = lock(&self.lease.state);
        if self.granted {
            state.used.measurements -= 1;
            state.used.sessions -= usize::from(self.session);
            state.fit();
        }
        state.live -= 1;
        drop(state);
        self.lease.changed.notify_waiters();
    }
}
