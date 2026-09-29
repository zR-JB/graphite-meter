//! The server's buffer budget and the plan that sizes it. QUIC and HTTP/2 connection state, QUIC endpoint buffers
//! and the download block draw on one limit, `GM_MAX_BUFFER_BYTES`. Validation checks that it covers every
//! connection's floor, and each listener checks again with what it binds.
use crate::{
    admission::Limits,
    config::{Config, ConfigError, NativeKind},
    quic_shard,
    sync::lock,
    timeouts::IDLE_BOUND,
};
use graphite_meter_core::wire::MAX_WEBTRANSPORT_STREAMS;
use graphite_meter_http3 as http3;
use noq::SharedBudget;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

/// The random block every download repeats, leased once at startup.
pub(crate) const DOWNLOAD_BLOCK_BYTES: usize = 256 * 1024;

/// An HTTP/2 connection's TLS records and deframer, frame reads, write buffer, HPACK and default window.
const H2_TRANSPORT_BYTES: usize = 512 * 1024;
/// The h2 stream state the connection's share of the budget covers past its transport.
pub(crate) const H2_STATE_BYTES: usize = 1024 * 1024;
/// What an HTTP/2 connection holds from accept to close.
pub(crate) const H2_FLOOR_BYTES: usize = H2_TRANSPORT_BYTES + H2_STATE_BYTES;

/// Go's h3ControlStreams: request streams past a client's admission shares, for its control requests.
const QUIC_CONTROL_STREAMS: usize = 4;
/// Go's browserH3UniStreams, the lane cap and wtLaneCreditHeadroom: credit past the cap resets an excess lane.
const QUIC_UNI_STREAMS: u32 = (3 + MAX_WEBTRANSPORT_STREAMS + 4) as u32;
const QUIC_DATAGRAM_BUFFER_BYTES: usize = 64 * 1024;
/// Go's autotuning ceilings, and one maximal 64 KiB HTTP/3 frame of credit until an upload is admitted.
const QUIC_STREAM_RECEIVE_WINDOW: u32 = 32 * 1024 * 1024;
pub(crate) const QUIC_RECEIVE_WINDOW: u32 = 48 * 1024 * 1024;
pub(crate) const QUIC_RECEIVE_WINDOW_FLOOR: u32 = 64 * 1024;
/// The receive credit a funded QUIC connection reserves past its floor.
pub(crate) const QUIC_CREDIT_BYTES: usize = (QUIC_RECEIVE_WINDOW - QUIC_RECEIVE_WINDOW_FLOOR) as usize;
/// A QUIC connection's first send window, and the least its tuning returns to.
pub(crate) const QUIC_MIN_SEND_WINDOW: u64 = 2 * 1024 * 1024;
/// Handshake packets an endpoint buffers for one incoming connection, and for all of them.
pub(crate) const QUIC_INCOMING_BYTES: u64 = 64 * 1024;
pub(crate) const QUIC_INCOMING_TOTAL_BYTES: u64 = 4 * 1024 * 1024;

/// A QUIC connection's request streams: a client's admission shares and the control streams, when that fits a
/// QUIC stream count.
pub(crate) fn max_requests(limits: &Limits) -> Option<u32> {
    let streams = limits.operations_per_client.checked_add(limits.sessions_per_client)?;
    u32::try_from(streams.checked_add(QUIC_CONTROL_STREAMS)?).ok()
}

/// The transport of every QUIC connection, whose floor the plan counts.
pub(crate) fn quic_transport(limits: &Limits) -> Result<noq::TransportConfig, ConfigError> {
    let mut transport = noq::TransportConfig::default();
    let requests = max_requests(limits).ok_or("per-client stream budgets exceed the QUIC stream limit")?;
    transport.max_concurrent_bidi_streams(requests.into());
    transport.max_concurrent_uni_streams(QUIC_UNI_STREAMS.into());
    transport.stream_receive_window(QUIC_STREAM_RECEIVE_WINDOW.into());
    transport.receive_window(QUIC_RECEIVE_WINDOW_FLOOR.into());
    transport.send_window(QUIC_MIN_SEND_WINDOW);
    transport.datagram_receive_buffer_size(Some(QUIC_DATAGRAM_BUFFER_BYTES));
    transport.datagram_send_buffer_size(QUIC_DATAGRAM_BUFFER_BYTES);
    transport.max_idle_timeout(Some(IDLE_BOUND.try_into()?));
    Ok(transport)
}

/// Held from accept until Noq drops the connection: the TLS handshake and the HTTP/3 layer's fixed state.
pub(crate) fn connection_floor(handshake_bytes: usize) -> usize {
    handshake_bytes.saturating_add(http3::CONNECTION_BYTES)
}

/// Noq precharges its own floor when it creates a connection, so only validation counts it.
pub(crate) fn noq_floor(limits: &Limits) -> Result<usize, ConfigError> {
    Ok(quic_transport(limits)?.connection_floor_bytes())
}

/// The largest datagram an endpoint reads into one receive segment.
pub(crate) fn packet_bytes(config: &noq::EndpointConfig) -> Option<usize> {
    usize::try_from(config.get_max_udp_payload_size().min(64 * 1024)).ok()
}

/// One of `shards` endpoints: its receive batch, the pending incoming packets its part of the incoming limits
/// admits, its forwarding queue and its kernel buffers.
pub(crate) fn endpoint_bytes(
    config: &noq::EndpointConfig,
    shards: usize,
    max_connections: usize,
    kernel_bytes: usize,
    receive_segments: usize,
) -> Option<usize> {
    let packet = packet_bytes(config)?;
    let receive = packet.checked_mul(receive_segments)?;
    let queue = if shards > 1 {
        quic_shard::queue_bytes(packet)?
    } else {
        0
    };
    receive
        .checked_mul(noq::udp::BATCH_SIZE)?
        .checked_add(receive.checked_mul(max_connections.div_ceil(shards).checked_add(1)?)?)?
        .checked_add((QUIC_INCOMING_TOTAL_BYTES as usize).div_ceil(shards))?
        .checked_add(queue)?
        .checked_add(kernel_bytes)
}

/// The configured budget, before any socket exists: one endpoint's buffers without its kernel's.
pub(crate) fn check_configured(config: &Config) -> Result<(), ConfigError> {
    let endpoint = if config.listener(NativeKind::H3).address.is_empty() {
        None
    } else {
        Some(
            endpoint_bytes(&noq::EndpointConfig::default(), 1, config.max_connections, 0, 1)
                .ok_or("QUIC endpoint buffer size overflow")?,
        )
    };
    check(config, config.max_buffer_bytes, 0, endpoint)
}

/// `limit` must cover every connection's floor, the QUIC endpoints' buffers when HTTP/3 runs, and the download block.
pub(crate) fn check(
    config: &Config,
    limit: usize,
    handshake_bytes: usize,
    quic_endpoint_bytes: Option<usize>,
) -> Result<(), ConfigError> {
    let quic = match quic_endpoint_bytes {
        Some(_) => connection_floor(handshake_bytes).saturating_add(noq_floor(&config.limits)?),
        None => 0,
    };
    let h2 = if config.listener(NativeKind::H2).address.is_empty() {
        0
    } else {
        H2_FLOOR_BYTES
    };
    let (floor, endpoint_bytes) = (quic.max(h2), quic_endpoint_bytes.unwrap_or(0));
    let minimum =
        floor as u128 * config.max_connections as u128 + endpoint_bytes as u128 + DOWNLOAD_BLOCK_BYTES as u128;
    if minimum > limit as u128 {
        return Err(format!(
            "GM_MAX_BUFFER_BYTES ({limit}) must be at least {minimum}: GM_MAX_CONNECTIONS ({}) connection floors \
             of {floor} bytes, {endpoint_bytes} bytes of QUIC endpoint buffers and the {DOWNLOAD_BLOCK_BYTES}-byte \
             download block",
            config.max_connections
        )
        .into());
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct MemoryBudget {
    pub(crate) limit: usize,
    used: AtomicUsize,
    held_back: AtomicBool,
}

impl MemoryBudget {
    pub(crate) fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit,
            used: AtomicUsize::new(0),
            held_back: AtomicBool::new(false),
        })
    }

    pub(crate) fn lease(self: &Arc<Self>, bytes: usize) -> Option<Lease> {
        self.try_charge(bytes).then(|| Lease {
            budget: self.clone(),
            bytes,
        })
    }

    #[cfg(test)]
    pub(crate) fn available(&self) -> usize {
        self.limit - self.used.load(Ordering::Relaxed)
    }

    pub(crate) fn under_pressure(&self) -> bool {
        self.used.load(Ordering::Relaxed) >= self.limit / 4
    }

    pub(crate) fn has_headroom(&self) -> bool {
        let used = self.used.load(Ordering::Relaxed);
        let headroom = used < self.limit / 4 * 3;
        // Reported recovery waits for five eighths, so usage hovering at the threshold cannot flood the log.
        let held_back = self.held_back.load(Ordering::Relaxed);
        let changed = if held_back {
            used < self.limit / 8 * 5
        } else {
            !headroom
        };
        if changed
            && self
                .held_back
                .compare_exchange(held_back, !held_back, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            crate::log!(
                "[gm:memory] window growth {}: {used} of {} buffer bytes in use",
                if held_back {
                    "resumed"
                } else {
                    "held back by memory pressure"
                },
                self.limit
            );
        }
        headroom
    }
}

impl SharedBudget for MemoryBudget {
    fn try_charge(&self, bytes: usize) -> bool {
        self.used
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                used.checked_add(bytes).filter(|&used| used <= self.limit)
            })
            .is_ok()
    }

    fn refund(&self, bytes: usize) {
        self.used.fetch_sub(bytes, Ordering::Relaxed);
    }
}

impl h2::SharedBudget for MemoryBudget {
    fn try_charge(&self, bytes: usize) -> bool {
        SharedBudget::try_charge(self, bytes)
    }

    fn refund(&self, bytes: usize) {
        SharedBudget::refund(self, bytes);
    }
}

#[derive(Debug)]
pub(crate) struct Lease {
    pub(crate) budget: Arc<MemoryBudget>,
    pub(crate) bytes: usize,
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.budget.refund(self.bytes);
    }
}

/// The receive-window credit each client may hold across its connections, `share` for its narrowest key. As in
/// admission, each wider key (an IPv6 /56 and /48, or a login's principal) may hold twice the one before it. A key
/// that many clients share, the password operator's principal, bounds no claim. All claims together stay within half
/// of a `budget`, below the three quarters at which window growth is held back, so claims alone never hold it back.
#[derive(Debug)]
pub(crate) struct ClientCredit {
    share: usize,
    shared: Option<String>,
    limit: usize,
    held: Mutex<Held>,
}

#[derive(Debug, Default)]
struct Held {
    keys: HashMap<String, usize>,
    total: usize,
}

impl ClientCredit {
    pub(crate) fn new(share: usize, shared: Option<String>, budget: usize) -> Arc<Self> {
        Arc::new(Self {
            share,
            shared,
            limit: budget / 2,
            held: Mutex::default(),
        })
    }

    /// Charges `bytes` to every key of an admitted client, or to none if any would pass its share.
    pub(crate) fn claim(self: &Arc<Self>, keys: &[String], bytes: usize) -> Option<CreditClaim> {
        let keys = || keys.iter().filter(|&key| Some(key) != self.shared.as_ref());
        let mut held = lock(&self.held);
        let fits = held.total.saturating_add(bytes) <= self.limit
            && keys().enumerate().all(|(index, key)| {
                let share = self.share.saturating_mul(1 << index.min(usize::BITS as usize - 1));
                held.keys.get(key).copied().unwrap_or_default().saturating_add(bytes) <= share
            });
        if !fits {
            return None;
        }
        held.total += bytes;
        for key in keys() {
            *held.keys.entry(key.clone()).or_default() += bytes;
        }
        Some(CreditClaim {
            credit: self.clone(),
            keys: keys().cloned().collect(),
            bytes,
        })
    }
}

/// Released once, when the connection that holds it can no longer be sent the credit it covers.
#[derive(Debug)]
pub(crate) struct CreditClaim {
    credit: Arc<ClientCredit>,
    keys: Vec<String>,
    bytes: usize,
}

impl Drop for CreditClaim {
    fn drop(&mut self) {
        let mut held = lock(&self.credit.held);
        held.total -= self.bytes;
        for key in &self.keys {
            let bytes = held.keys.get_mut(key).expect("claimed keys are held");
            *bytes -= self.bytes;
            if *bytes == 0 {
                held.keys.remove(key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ClientCredit;

    #[test]
    fn a_client_share_doubles_for_wider_keys_and_a_shared_key_bounds_no_claim() {
        let credit = ClientCredit::new(1 << 20, Some("principal:shared".into()), usize::MAX);
        let claim =
            |address: &str| credit.claim(&crate::client_address::client_keys(address.parse().unwrap()), 1 << 20);
        let first = claim("2001:db8:1:1::1").unwrap();
        assert!(claim("2001:db8:1:1::2").is_none(), "its /64 holds the share");
        let mut held = vec![claim("2001:db8:1:2::1").unwrap()];
        assert!(claim("2001:db8:1:3::1").is_none(), "its /56 holds twice the share");
        held.push(claim("2001:db8:1:100::1").unwrap());
        held.push(claim("2001:db8:1:101::1").unwrap());
        assert!(
            claim("2001:db8:1:200::1").is_none(),
            "its /48 holds four times the share"
        );
        assert!(claim("2001:db8:2::1").is_some());
        drop(first);
        held.push(claim("2001:db8:1:1::2").unwrap());
        let logins = [
            ["login:a", "principal:p"],
            ["login:b", "principal:p"],
            ["login:c", "principal:p"],
        ]
        .map(|keys| keys.map(String::from));
        held.push(credit.claim(&logins[0], 1 << 20).unwrap());
        held.push(credit.claim(&logins[1], 1 << 20).unwrap());
        assert!(
            credit.claim(&logins[2], 1 << 20).is_none(),
            "a principal holds twice a login's share"
        );
        for login in ["login:d", "login:e", "login:f"] {
            let keys = [login, "principal:shared"].map(String::from);
            held.push(
                credit
                    .claim(&keys, 1 << 20)
                    .expect("the shared principal bounds no login"),
            );
        }
        drop(held);
        let held = credit.held.lock().unwrap();
        assert!(held.keys.is_empty() && held.total == 0);
    }
}
