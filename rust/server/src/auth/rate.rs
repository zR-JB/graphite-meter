//! Sign-in budgets over a one-minute window: attempts per client address, and ceilings all clients share.

use crate::{
    lock, log,
    peer::{ClientKey, ClientKeys},
};
use std::{
    collections::{HashMap, VecDeque},
    fmt,
    sync::Mutex,
    time::Duration,
};
use tokio::time::Instant;

const WINDOW: Duration = Duration::from_secs(60);
/// An address table tracks at most this many keys.
const MAX_KEYS: usize = 2048;

/// Attempts per client: `limit` for its narrowest key, twice that for each wider one.
pub(super) struct Attempts {
    name: &'static str,
    limit: usize,
    table: Mutex<Table>,
}

#[derive(Default)]
struct Table {
    keys: HashMap<ClientKey, VecDeque<Instant>>,
    logged: Option<Instant>,
}

impl Attempts {
    pub fn new(name: &'static str, limit: usize) -> Self {
        Self { name, limit, table: Mutex::default() }
    }

    /// Records an attempt unless a key spent its share, the table is full or `ceiling` is engaged; a `known` device
    /// passes a full table.
    pub fn allow(&self, keys: &ClientKeys, known: bool, ceiling: Option<&Ceiling>) -> bool {
        let keys: Vec<_> = keys.iter().collect();
        let mut table = lock(&self.table);
        let now = Instant::now();
        let Table { keys: held, logged } = &mut *table;
        if held.len() + keys.len() > MAX_KEYS {
            held.retain(|_, times| {
                expire(times, now);
                !times.is_empty()
            });
        }
        for (wider, key) in keys.iter().enumerate() {
            let full = held.len() >= MAX_KEYS;
            match held.get_mut(key) {
                Some(times) => {
                    expire(times, now);
                    if times.len() >= self.limit << wider {
                        return false;
                    }
                }
                None if full && known => {}
                None if full => {
                    engage(logged, now, format_args!("{}-address", self.name));
                    return false;
                }
                None => {
                    held.insert(key.clone(), VecDeque::new());
                }
            }
        }
        if ceiling.is_some_and(Ceiling::engaged) {
            return false;
        }
        for key in &keys {
            if let Some(times) = held.get_mut(key) {
                times.push_back(now);
            }
        }
        true
    }
}

/// Events across all clients, such as wrong passwords; at `limit` in the window it refuses further attempts.
pub(super) struct Ceiling {
    name: &'static str,
    limit: usize,
    window: Mutex<(VecDeque<Instant>, Option<Instant>)>,
}

impl Ceiling {
    pub fn new(name: &'static str, limit: usize) -> Self {
        Self { name, limit, window: Mutex::default() }
    }

    pub fn note(&self) {
        let mut window = lock(&self.window);
        let now = Instant::now();
        expire(&mut window.0, now);
        window.0.push_back(now);
    }

    /// Whether the window holds `limit` events, logged once a minute while it does.
    fn engaged(&self) -> bool {
        let mut window = lock(&self.window);
        let now = Instant::now();
        let (times, logged) = &mut *window;
        expire(times, now);
        let engaged = times.len() >= self.limit;
        if engaged {
            engage(logged, now, format_args!("{}", self.name));
        }
        engaged
    }
}

fn expire(times: &mut VecDeque<Instant>, now: Instant) {
    while times.front().is_some_and(|&time| now.duration_since(time) >= WINDOW) {
        times.pop_front();
    }
}

/// Logs that the ceiling `name` engaged, at most once a minute.
fn engage(logged: &mut Option<Instant>, now: Instant, name: fmt::Arguments<'_>) {
    if logged.is_some_and(|at| now.duration_since(at) < WINDOW) {
        return;
    }
    *logged = Some(now);
    log!("[gm:auth] global {name} ceiling engaged; further attempts are refused until the window drains");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use tokio::time::advance;

    fn v4(last: u32) -> ClientKeys {
        ClientKeys::V4(Ipv4Addr::from(0xc000_0200 + last))
    }

    #[tokio::test(start_paused = true)]
    async fn an_ipv6_client_shares_its_wider_prefixes_at_twice_and_four_times_the_limit() {
        let attempts = Attempts::new("password-attempt", 5);
        let v6 = |text: &str| ClientKeys::address(text.parse().unwrap());
        let allowed = |keys: &ClientKeys| (0..30).filter(|_| attempts.allow(keys, false, None)).count();
        assert_eq!(allowed(&v6("2001:db8:0:1::1")), 5);
        assert_eq!(allowed(&v6("2001:db8:0:2::1")), 5, "another /64 in the /56");
        assert_eq!(allowed(&v6("2001:db8:0:3::1")), 0, "the /56 holds twice the limit");
        assert_eq!(allowed(&v6("2001:db8:0:100::1")) + allowed(&v6("2001:db8:0:101::1")), 10);
        assert_eq!(allowed(&v6("2001:db8:0:200::1")), 0, "the /48 holds four times the limit");
        advance(WINDOW).await;
        assert_eq!(allowed(&v6("2001:db8:0:1::1")), 5);
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_table_and_an_engaged_ceiling_refuse_all_but_a_known_device() {
        let attempts = Attempts::new("password-attempt", 5);
        for client in 0..MAX_KEYS as u32 {
            assert!(attempts.allow(&v4(client), false, None));
        }
        let newcomer = v4(MAX_KEYS as u32);
        assert!(!attempts.allow(&newcomer, false, None));
        assert!(attempts.allow(&newcomer, true, None), "a known device passes a full table");
        advance(WINDOW).await;
        let ceiling = Ceiling::new("password-attempt", 60);
        (0..60).for_each(|_| ceiling.note());
        assert!(!attempts.allow(&newcomer, false, Some(&ceiling)));
        assert!(attempts.allow(&newcomer, true, None));
        advance(WINDOW).await;
        assert!(attempts.allow(&newcomer, false, Some(&ceiling)));
    }
}
