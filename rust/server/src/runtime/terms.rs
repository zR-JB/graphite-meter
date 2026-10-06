//! What the buffer budget covers before binding: connection floors, QUIC endpoint buffers and the download block.

use crate::{
    config::{Config, ListenerKind},
    engine::download::BLOCK_BYTES,
    transport::{http2, quic},
};

/// Refuses a buffer budget below every connection's floor, the QUIC endpoint's pre-socket buffers and downloads.
pub fn check_budget(config: &Config) -> Result<(), String> {
    Terms::of(config)?.check(0, configured_endpoint(config)?)
}

/// The QUIC endpoint's buffers without its socket's, or none without HTTP/3.
pub(super) fn configured_endpoint(config: &Config) -> Result<usize, String> {
    match config.listener(ListenerKind::H3) {
        Some(_) => quic::endpoint_bytes(&noq::EndpointConfig::default(), 1, config.limits.connections, 0, 1)
            .ok_or_else(|| "QUIC endpoint buffer size overflow".into()),
        None => Ok(0),
    }
}

/// What the buffer budget must cover: every connection's floor, the QUIC endpoint's buffers and the download block.
#[derive(Debug, Clone, Copy)]
pub(super) struct Terms {
    limit: usize,
    connections: usize,
    h2: bool,
    /// noq's own floor per connection, with HTTP/3 enabled.
    pub(super) quic: Option<usize>,
}

impl Terms {
    pub(super) fn of(config: &Config) -> Result<Self, String> {
        Ok(Self {
            limit: config.max_buffer_bytes,
            connections: config.limits.connections,
            h2: config.listener(ListenerKind::H2).is_some(),
            quic: config
                .listener(ListenerKind::H3)
                .map(|_| quic::noq_floor(&config.limits))
                .transpose()?,
        })
    }

    /// Refuses a budget that a `handshake`-byte QUIC handshake and `endpoint`-byte endpoint buffers leave short.
    pub(super) fn check(&self, handshake: usize, endpoint: usize) -> Result<(), String> {
        let quic = self
            .quic
            .map_or(0, |noq| quic::floor_bytes(handshake).saturating_add(noq));
        let floor = quic.max(if self.h2 { http2::FLOOR_BYTES } else { 0 });
        let (limit, connections) = (self.limit, self.connections);
        let minimum = floor as u128 * connections as u128 + endpoint as u128 + BLOCK_BYTES as u128;
        if minimum > limit as u128 {
            return Err(format!(
                "GM_MAX_BUFFER_BYTES ({limit}) must be at least {minimum}: GM_MAX_CONNECTIONS ({connections}) \
                 connection floors of {floor} bytes, {endpoint} bytes of QUIC endpoint buffers and the \
                 {BLOCK_BYTES}-byte download block"
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{self, Loaded};
    use std::ffi::OsString;

    fn config(env: &[(&str, &str)]) -> Config {
        let lookup = |name: &str| env.iter().find(|(key, _)| *key == name).map(|(_, value)| value.into());
        match config::load(lookup, Vec::<OsString>::new(), &mut Vec::new()) {
            Ok(Loaded::Config(config)) => *config,
            other => panic!("{other:?}"),
        }
    }

    const TLS: [(&str, &str); 2] = [("GM_TLS_CERT", "/cert.pem"), ("GM_TLS_KEY", "/key.pem")];

    #[test]
    fn per_client_stream_budgets_must_fit_a_quic_stream_count() {
        let max = i64::MAX.to_string();
        let huge = [
            ("GM_H3_ADDR", ":7249"),
            ("GM_MAX_ACTIVE_MEASUREMENTS", &max),
            ("GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT", &max),
            ("GM_MAX_ACTIVE_SESSIONS", &max),
            ("GM_MAX_SESSIONS_PER_CLIENT", &max),
        ];
        let refused = check_budget(&config(&[&TLS[..], &huge[..]].concat()));
        assert_eq!(refused, Err("per-client stream budgets exceed the QUIC stream limit".into()));
        assert_eq!(check_budget(&config(&huge[1..])), Ok(()), "only HTTP/3 counts streams");
    }

    #[test]
    fn http3_adds_its_connection_floor_handshake_and_endpoint_buffers() {
        let env = [
            ("GM_H3_ADDR", ":7249"),
            ("GM_MAX_CONNECTIONS", "2"),
            ("GM_MAX_CONNECTIONS_PER_CLIENT", "2"),
        ];
        let config = config(&[&TLS[..], &env[..]].concat());
        assert_eq!(
            quic::noq_floor(&config.limits).unwrap() >> 10,
            481,
            "noq's stream floor at default limits"
        );
        let endpoint = configured_endpoint(&config).unwrap();
        let floor = quic::floor_bytes(0) + quic::noq_floor(&config.limits).unwrap();
        let terms = Terms {
            limit: 2 * floor + endpoint + BLOCK_BYTES,
            ..Terms::of(&config).unwrap()
        };
        assert_eq!(terms.check(0, endpoint), Ok(()));
        let refused = terms.check(1, endpoint).unwrap_err();
        let minimum = terms.limit + 2;
        let message = format!(
            "GM_MAX_BUFFER_BYTES ({}) must be at least {minimum}: GM_MAX_CONNECTIONS (2) connection floors of {} \
             bytes, {endpoint} bytes of QUIC endpoint buffers and the 262144-byte download block",
            terms.limit,
            floor + 1
        );
        assert_eq!(refused, message, "a handshake byte more on each connection");
        assert!(terms.check(0, endpoint + 1).is_err(), "the endpoint's socket buffers count");
    }

    #[test]
    fn noqs_stream_floor_at_per_client_limits_equal_to_the_totals() {
        let totals = [("GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT", "256"), ("GM_MAX_SESSIONS_PER_CLIENT", "64")];
        let floor = quic::noq_floor(&config(&totals).limits).unwrap();
        assert_eq!(floor >> 10, 978, "noq's stream floor for 324 request streams, without HTTP/3's state");
    }
}
