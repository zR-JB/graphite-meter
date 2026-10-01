//! Shared operation budgets. A permit lives until the operation has fully stopped.
use crate::{client_address::Shares, sync::lock};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub operations: usize,
    pub operations_per_client: usize,
    pub sessions: usize,
    pub sessions_per_client: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            operations: 256,
            operations_per_client: 32,
            sessions: 64,
            sessions_per_client: 8,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    ClientFull,
    GlobalFull,
    SessionsFull,
}

impl Refusal {
    pub fn status(self) -> u16 {
        match self {
            Self::ClientFull => 429,
            Self::GlobalFull | Self::SessionsFull => 503,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub active: usize,
    pub peak: usize,
    pub refused_pool: u64,
    pub refused_client: u64,
    pub sessions: usize,
    pub sessions_refused_budget: u64,
    pub sessions_refused_client: u64,
}

#[derive(Default)]
struct Counts {
    stats: Stats,
    requests_by_client: Shares,
    sessions_by_client: Shares,
}

struct Inner {
    limits: Limits,
    counts: Mutex<Counts>,
}

#[derive(Clone)]
pub struct Admission(Arc<Inner>);

/// Moving a permit transfers ownership; dropping it releases exactly once.
#[must_use]
pub struct Permit {
    admission: Admission,
    session: bool,
    clients: Vec<String>,
}

impl Admission {
    pub fn new(limits: Limits) -> Self {
        Self(Arc::new(Inner {
            limits,
            counts: Mutex::new(Counts::default()),
        }))
    }

    /// Call after resolving the request or session client keys. Unmetered routes
    /// and CORS preflight do not acquire a permit.
    pub fn acquire(&self, session: bool, keys: &[String]) -> Result<Permit, Refusal> {
        let mut counts = lock(&self.0.counts);
        let Counts {
            stats,
            requests_by_client,
            sessions_by_client,
        } = &mut *counts;
        let (held, limit, refused) = if session {
            (
                sessions_by_client,
                self.0.limits.sessions_per_client,
                &mut stats.sessions_refused_client,
            )
        } else {
            (
                requests_by_client,
                self.0.limits.operations_per_client,
                &mut stats.refused_client,
            )
        };
        // Match Go's refusal precedence: client exhaustion wins over global exhaustion.
        if held.full(keys, limit) {
            *refused += 1;
            return Err(Refusal::ClientFull);
        }
        if stats.active >= self.0.limits.operations {
            stats.refused_pool += 1;
            return Err(Refusal::GlobalFull);
        }
        if session && stats.sessions >= self.0.limits.sessions {
            stats.sessions_refused_budget += 1;
            return Err(Refusal::SessionsFull);
        }
        stats.active += 1;
        stats.peak = stats.peak.max(stats.active);
        if session {
            stats.sessions += 1;
        }
        held.hold(keys);
        Ok(Permit {
            admission: self.clone(),
            session,
            clients: keys.to_vec(),
        })
    }

    pub fn load(&self) -> (usize, usize) {
        (self.stats().active, self.0.limits.operations)
    }

    pub fn stats(&self) -> Stats {
        lock(&self.0.counts).stats
    }
}

impl Permit {
    /// The client keys the permit was admitted under.
    pub fn clients(&self) -> &[String] {
        &self.clients
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut counts = lock(&self.admission.0.counts);
        counts.stats.active -= 1;
        if self.session {
            counts.stats.sessions -= 1;
            counts.sessions_by_client.release(&self.clients);
        } else {
            counts.requests_by_client.release(&self.clients);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_and_request_clients_are_separate_but_global_budget_is_shared() {
        let admission = Admission::new(Limits {
            operations: 2,
            operations_per_client: 1,
            sessions: 1,
            sessions_per_client: 1,
        });
        let request = admission.acquire(false, &["a".into()]).unwrap();
        let session = admission.acquire(true, &["a".into()]).unwrap();
        assert_eq!(admission.acquire(false, &["a".into()]).err(), Some(Refusal::ClientFull));
        assert_eq!(admission.acquire(false, &["b".into()]).err(), Some(Refusal::GlobalFull));
        drop(request);
        assert_eq!(
            admission.acquire(true, &["b".into()]).err(),
            Some(Refusal::SessionsFull)
        );
        assert_eq!(admission.acquire(true, &["a".into()]).err(), Some(Refusal::ClientFull));
        drop(session);
        assert_eq!(admission.load(), (0, 2));
        assert!(admission.acquire(true, &["a".into()]).is_ok());
        assert_eq!(
            admission.stats(),
            Stats {
                active: 0,
                peak: 2,
                refused_pool: 1,
                refused_client: 1,
                sessions: 0,
                sessions_refused_budget: 1,
                sessions_refused_client: 1,
            }
        );
    }
}
