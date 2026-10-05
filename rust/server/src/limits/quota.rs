//! Concurrency or weight counted against a total and per-client shares, held until dropped.

use super::budget::Budget;
use crate::{
    lock,
    peer::{ClientKey, ClientKeys},
};
use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex},
};

/// A limit clones share.
#[derive(Clone)]
pub struct Quota(Arc<Inner>);

struct Inner {
    total: Total,
    /// What a client's narrowest key may hold; each wider key holds twice the one before it.
    share: usize,
    /// A key many clients share, which bounds nothing.
    exempt: Option<ClientKey>,
    state: Mutex<State>,
}

enum Total {
    Fixed(usize),
    /// Half the budget past its reservations.
    ClientWindows(Budget),
}

#[derive(Default)]
struct State {
    used: usize,
    peak: usize,
    held: HashMap<ClientKey, usize>,
    refused_total: u64,
    refused_client: u64,
}

/// Why a quota refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// A client's share is full.
    Client,
    Total,
}

impl Refusal {
    /// `429` for a full client share, `503` for a full total; either answer carries `Retry-After: 1`.
    pub const fn status(self) -> u16 {
        match self {
            Self::Client => 429,
            Self::Total => 503,
        }
    }
}

/// Counters for the admission log.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QuotaUsage {
    pub active: usize,
    pub peak: usize,
    pub refused_total: u64,
    pub refused_client: u64,
}

impl Quota {
    pub fn new(total: usize, share: usize) -> Self {
        Self::with(Total::Fixed(total), share, None)
    }

    /// Receive-window credit in bytes: all clients together within half the budget past its reservations.
    pub fn client_windows(budget: Budget, share: usize, exempt: Option<ClientKey>) -> Self {
        Self::with(Total::ClientWindows(budget), share, exempt)
    }

    fn with(total: Total, share: usize, exempt: Option<ClientKey>) -> Self {
        Self(Arc::new(Inner { total, share, exempt, state: Mutex::default() }))
    }

    /// Charges `weight` to the total and to every key at once, or refuses without charging.
    pub fn acquire(&self, keys: &ClientKeys, weight: usize) -> Result<Hold, Refusal> {
        self.acquire_inner(keys, weight, None)
    }

    /// Charges this quota and `outer`'s total, checking this client share, then `outer`'s total, then this total.
    pub fn acquire_within(&self, keys: &ClientKeys, weight: usize, outer: &Quota) -> Result<Hold, Refusal> {
        self.acquire_inner(keys, weight, Some(outer))
    }

    fn acquire_inner(&self, keys: &ClientKeys, weight: usize, outer: Option<&Quota>) -> Result<Hold, Refusal> {
        let total = self.0.total();
        let mut state = lock(&self.0.state);
        if self.0.keys(keys).enumerate().any(|(index, key)| {
            let held = state.held.get(&key).copied().unwrap_or(0);
            held.saturating_add(weight) > self.0.share.saturating_mul(1 << index)
        }) {
            state.refused_client += 1;
            return Err(Refusal::Client);
        }
        // The outer quota is only ever locked after this one.
        let outer = outer
            .map(|outer| outer.acquire(&ClientKeys::Exempt, weight))
            .transpose()?;
        if state.used.saturating_add(weight) > total {
            state.refused_total += 1;
            return Err(Refusal::Total);
        }
        state.used += weight;
        state.peak = state.peak.max(state.used);
        for key in self.0.keys(keys) {
            *state.held.entry(key).or_default() += weight;
        }
        let linked = outer.map(Box::new);
        Ok(Hold { quota: self.clone(), keys: keys.clone(), weight, linked })
    }

    /// Whether any of the keys holds part of this quota.
    pub fn holds_any(&self, keys: &ClientKeys) -> bool {
        let state = lock(&self.0.state);
        self.0.keys(keys).any(|key| state.held.contains_key(&key))
    }

    pub fn usage(&self) -> QuotaUsage {
        let state = lock(&self.0.state);
        let State { used, peak, refused_total, refused_client, .. } = *state;
        QuotaUsage { active: used, peak, refused_total, refused_client }
    }
}

impl Inner {
    fn total(&self) -> usize {
        match &self.total {
            Total::Fixed(total) => *total,
            Total::ClientWindows(budget) => budget.client_windows(),
        }
    }

    fn keys<'a>(&'a self, keys: &'a ClientKeys) -> impl Iterator<Item = ClientKey> + 'a {
        keys.iter().filter(move |key| Some(key) != self.exempt.as_ref())
    }
}

/// A share of a quota, released when dropped: at the end of its work, on error and while a panic unwinds.
#[must_use]
pub struct Hold {
    quota: Quota,
    keys: ClientKeys,
    weight: usize,
    /// Released with this one.
    linked: Option<Box<Hold>>,
}

impl fmt::Debug for Hold {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { keys, weight, linked, .. } = self;
        formatter
            .debug_struct("Hold")
            .field("keys", keys)
            .field("weight", weight)
            .field("linked", linked)
            .finish()
    }
}

impl Hold {
    /// One hold releasing both.
    pub fn join(mut self, other: Hold) -> Hold {
        self.linked = Some(Box::new(match self.linked.take() {
            Some(linked) => linked.join(other),
            None => other,
        }));
        self
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        let inner = &self.quota.0;
        let mut state = lock(&inner.state);
        state.used -= self.weight;
        for key in inner.keys(&self.keys) {
            if let Some(held) = state.held.get_mut(&key) {
                *held -= self.weight;
                if *held == 0 {
                    state.held.remove(&key);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Holder;

    fn v6(address: &str) -> ClientKeys {
        ClientKeys::address(address.parse().unwrap())
    }

    #[test]
    fn ipv6_prefixes_share_once_twice_and_four_times() {
        let quota = Quota::new(100, 1);
        let first = quota.acquire(&v6("2001:db8:1:1::1"), 1).unwrap();
        assert_eq!(quota.acquire(&v6("2001:db8:1:1::2"), 1).err(), Some(Refusal::Client), "its /64 is full");
        let mut held = vec![quota.acquire(&v6("2001:db8:1:2::1"), 1).unwrap()];
        assert_eq!(quota.acquire(&v6("2001:db8:1:3::1"), 1).err(), Some(Refusal::Client), "its /56 holds two");
        held.push(quota.acquire(&v6("2001:db8:1:100::1"), 1).unwrap());
        held.push(quota.acquire(&v6("2001:db8:1:101::1"), 1).unwrap());
        assert_eq!(
            quota.acquire(&v6("2001:db8:1:200::1"), 1).err(),
            Some(Refusal::Client),
            "its /48 holds four"
        );
        assert!(quota.acquire(&v6("2001:db8:2::1"), 1).is_ok());
        drop(first);
        held.push(quota.acquire(&v6("2001:db8:1:1::2"), 1).unwrap());
        assert_eq!(quota.usage().refused_client, 3);
    }

    #[test]
    fn a_principal_shares_twice_unless_exempt() {
        let login = |id: &str, principal: &str| ClientKeys::Auth(Holder::Login(id.into()), principal.into());
        let quota = Quota::new(100, 1);
        let held = [quota.acquire(&login("a", "p"), 1).unwrap(), quota.acquire(&login("b", "p"), 1).unwrap()];
        assert_eq!(quota.acquire(&login("c", "p"), 1).err(), Some(Refusal::Client));
        drop(held);
        let operator = ClientKey::Principal("operator".into());
        let credit = Quota::client_windows(Budget::new(usize::MAX), 10, Some(operator));
        let logins: Vec<_> = ["a", "b", "c", "d"]
            .map(|id| {
                credit
                    .acquire(&login(id, "operator"), 10)
                    .expect("the operator principal bounds nothing")
            })
            .into();
        assert_eq!(
            credit.acquire(&login("a", "operator"), 1).err(),
            Some(Refusal::Client),
            "each login alone"
        );
        drop(logins);
        assert_eq!(credit.usage().active, 0);
    }

    #[test]
    fn weights_fill_shares_and_totals_by_their_last_byte() {
        let budget = Budget::new(1000);
        let credit = Quota::client_windows(budget.clone(), 300, None);
        let client = v6("2001:db8::1");
        let claim = credit.acquire(&client, 300).unwrap();
        assert_eq!(credit.acquire(&client, 1).err(), Some(Refusal::Client));
        let other = credit.acquire(&v6("2001:db9::1"), 200).unwrap();
        assert_eq!(
            credit.acquire(&v6("2001:dba::1"), 1).err(),
            Some(Refusal::Total),
            "half the budget is held"
        );
        let reserved = budget.reserve(200).unwrap();
        drop(other);
        assert_eq!(
            credit.acquire(&v6("2001:dba::1"), 101).err(),
            Some(Refusal::Total),
            "reservations shrink half"
        );
        drop((claim, reserved));
        assert_eq!(credit.usage().active, 0);
    }

    #[test]
    fn a_full_client_share_is_refused_before_a_full_total() {
        let quota = Quota::new(1, 1);
        let held = quota.acquire(&v6("2001:db8::1"), 1).unwrap();
        assert_eq!(quota.acquire(&v6("2001:db8::2"), 1).err(), Some(Refusal::Client));
        assert_eq!(quota.acquire(&v6("2001:db9::1"), 1).err(), Some(Refusal::Total));
        assert_eq!(quota.acquire(&ClientKeys::Exempt, 1).err(), Some(Refusal::Total));
        let usage = quota.usage();
        assert_eq!((usage.active, usage.peak, usage.refused_client, usage.refused_total), (1, 1, 1, 2));
        drop(held);
        assert_eq!(quota.usage().active, 0);
    }

    #[test]
    fn an_inner_quota_checks_its_share_then_the_outer_total_then_its_own() {
        let (outer, inner) = (Quota::new(2, 2), Quota::new(1, 1));
        let client = v6("2001:db8::1");
        let session = inner.acquire_within(&client, 1, &outer).unwrap();
        assert_eq!(outer.usage().active, 1, "the outer quota holds no keys for it");
        assert!(!outer.holds_any(&client));
        assert_eq!(inner.acquire_within(&client, 1, &outer).err(), Some(Refusal::Client));
        let operation = outer.acquire(&v6("2001:db9::1"), 1).unwrap();
        assert_eq!(inner.acquire_within(&v6("2001:dba::1"), 1, &outer).err(), Some(Refusal::Total));
        assert_eq!(outer.usage().refused_total, 1, "the outer total refused first");
        drop(operation);
        assert_eq!(inner.acquire_within(&v6("2001:dba::1"), 1, &outer).err(), Some(Refusal::Total));
        assert_eq!(
            (outer.usage().active, inner.usage().refused_total),
            (1, 1),
            "the outer charge was returned"
        );
        drop(session);
        assert_eq!((outer.usage().active, inner.usage().active), (0, 0));
    }

    #[test]
    fn holds_are_released_while_a_panic_unwinds() {
        let quota = Quota::new(10, 10);
        let other = Quota::new(10, 10);
        let (moved, joined) = (quota.clone(), other.clone());
        let panicked = std::thread::spawn(move || {
            let client = v6("2001:db8::1");
            let _hold = moved
                .acquire(&client, 3)
                .unwrap()
                .join(joined.acquire(&client, 2).unwrap());
            panic!("the work failed");
        })
        .join();
        assert!(panicked.is_err());
        assert_eq!((quota.usage().active, other.usage().active), (0, 0));
        assert!(!quota.holds_any(&v6("2001:db8::1")));
    }

    #[test]
    fn any_prefix_of_a_source_holding_a_share_counts() {
        let quota = Quota::new(10, 10);
        let held = quota.acquire(&v6("2001:db8:1:2::1"), 1).unwrap();
        for sibling in ["2001:db8:1:2::2", "2001:db8:1:ff::1", "2001:db8:1:ff00::1"] {
            assert!(quota.holds_any(&v6(sibling)), "{sibling}");
        }
        assert!(!quota.holds_any(&v6("2001:db8:2::1")) && !quota.holds_any(&ClientKeys::Exempt));
        drop(held);
        assert!(!quota.holds_any(&v6("2001:db8:1:2::1")));
    }
}
