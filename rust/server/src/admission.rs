//! Shared operation budgets. A permit lives until the operation has fully stopped.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
};

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
pub enum Class {
    Request,
    Session,
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

pub(crate) fn recover<'a, T>(counts: &'a Mutex<T>, name: &str) -> MutexGuard<'a, T> {
    counts.lock().unwrap_or_else(|poisoned| {
        crate::log!("[gm:admission] {name} counts recovered after a panic and may be inaccurate");
        counts.clear_poison();
        poisoned.into_inner()
    })
}

#[derive(Default)]
struct Counts {
    active: usize,
    sessions: usize,
    requests_by_client: HashMap<String, usize>,
    sessions_by_client: HashMap<String, usize>,
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
    class: Class,
    clients: Vec<String>,
}

impl Admission {
    pub fn new(limits: Limits) -> Self {
        Self(Arc::new(Inner {
            limits,
            counts: Mutex::new(Counts::default()),
        }))
    }

    /// Call after resolving the request or session client key. Unmetered routes
    /// and CORS preflight do not acquire a permit.
    pub fn acquire(&self, class: Class, client: &str) -> Result<Permit, Refusal> {
        self.acquire_keys(class, &[client.to_owned()])
    }

    pub fn acquire_keys(&self, class: Class, keys: &[String]) -> Result<Permit, Refusal> {
        let mut counts = recover(&self.0.counts, "operation");
        let (clients, limit) = match class {
            Class::Request => (&counts.requests_by_client, self.0.limits.operations_per_client),
            Class::Session => (&counts.sessions_by_client, self.0.limits.sessions_per_client),
        };
        // Match Go's refusal precedence: client exhaustion wins over global exhaustion.
        if crate::client_address::share_full(keys, limit, |key| clients.get(key).copied().unwrap_or(0)) {
            return Err(Refusal::ClientFull);
        }
        if counts.active >= self.0.limits.operations {
            return Err(Refusal::GlobalFull);
        }
        if class == Class::Session && counts.sessions >= self.0.limits.sessions {
            return Err(Refusal::SessionsFull);
        }
        let clients = keys.to_vec();
        counts.active += 1;
        if class == Class::Session {
            counts.sessions += 1;
        }
        let held = match class {
            Class::Request => &mut counts.requests_by_client,
            Class::Session => &mut counts.sessions_by_client,
        };
        for client in &clients {
            *held.entry(client.clone()).or_default() += 1;
        }
        Ok(Permit {
            admission: self.clone(),
            class,
            clients,
        })
    }

    pub fn load(&self) -> (usize, usize) {
        (recover(&self.0.counts, "operation").active, self.0.limits.operations)
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut counts = recover(&self.admission.0.counts, "operation");
        counts.active -= 1;
        let clients = match self.class {
            Class::Request => &mut counts.requests_by_client,
            Class::Session => {
                counts.sessions -= 1;
                &mut counts.sessions_by_client
            }
        };
        for client in &self.clients {
            let count = clients.get_mut(client).expect("permit owns a client slot");
            *count -= 1;
            if *count == 0 {
                clients.remove(client);
            }
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
        let request = admission.acquire(Class::Request, "a").unwrap();
        let session = admission.acquire(Class::Session, "a").unwrap();
        assert_eq!(admission.acquire(Class::Request, "a").err(), Some(Refusal::ClientFull));
        assert_eq!(admission.acquire(Class::Request, "b").err(), Some(Refusal::GlobalFull));
        drop(request);
        assert_eq!(
            admission.acquire(Class::Session, "b").err(),
            Some(Refusal::SessionsFull)
        );
        drop(session);
        assert_eq!(admission.load(), (0, 2));
        assert!(admission.acquire(Class::Session, "a").is_ok());
    }

    #[tokio::test]
    async fn cancellation_releases_owned_budget() {
        let admission = Admission::new(Limits {
            operations: 1,
            ..Limits::default()
        });
        let permit = admission.acquire(Class::Request, "a").unwrap();
        let task = tokio::spawn(async move {
            let _permit = permit;
            std::future::pending::<()>().await;
        });
        assert_eq!(admission.load().0, 1);
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(admission.load().0, 0);
        assert!(admission.acquire(Class::Request, "a").is_ok());
    }
}
