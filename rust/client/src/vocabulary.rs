use std::time::Duration;

#[derive(Clone, Copy)]
pub struct Term {
    pub label: &'static str,
    pub explanation: &'static str,
}

pub const MISSING: &str = "—";

pub const CADENCES: [(&str, &str, Duration); 4] = [
    ("reply-driven", "Reply-driven", Duration::ZERO),
    ("fast", "Fast (80 ms)", Duration::from_millis(80)),
    ("medium", "Medium (250 ms)", Duration::from_millis(250)),
    ("slow", "Slow (600 ms)", Duration::from_millis(600)),
];

pub const START: Term = Term {
    label: "Start test",
    explanation: "Measure the selected stages on the checked server paths.",
};
pub const ADVANCED: Term = Term {
    label: "Advanced",
    explanation: "Show origin, stream, timing and TLS settings.",
};
pub const URL: Term = Term {
    label: "Catalogue URL",
    explanation: "The operator address used to load the server catalogue.",
};
pub const SERVERS: Term = Term {
    label: "Test servers",
    explanation: "Measure up to four servers together; the first one's latency is the result.",
};
pub const THROUGHPUT_ORIGIN: Term = Term {
    label: "Throughput origin",
    explanation: "The advertised server address used for data transfers.",
};
pub const PROTOCOL: Term = Term {
    label: "HTTP protocol",
    explanation: "The HTTP version used for fetch streams.",
};
pub const THROUGHPUT_TRANSPORT: Term = Term {
    label: "Throughput transport",
    explanation: "The connection used to carry download and upload data.",
};
pub const LATENCY_ORIGIN: Term = Term {
    label: "Latency origin",
    explanation: "The advertised server address used for latency probes.",
};
pub const LATENCY_TRANSPORT: Term = Term {
    label: "Latency transport",
    explanation: "The connection used to send probes and receive replies.",
};
pub const LATENCY: Term = Term {
    label: "Latency",
    explanation: "Measure round-trip time without transfer traffic.",
};
pub const DOWNLOAD: Term = Term {
    label: "Download",
    explanation: "Measure data received from the selected servers each second.",
};
pub const UPLOAD: Term = Term {
    label: "Upload",
    explanation: "Measure data received by the selected servers each second.",
};
pub const BIDIRECTIONAL: Term = Term {
    label: "Bidirectional",
    explanation: "Measure download and upload at the same time.",
};
pub const WARMUP: Term = Term {
    label: "Warmup (seconds)",
    explanation: "Wait before measurement so connections can settle.",
};
pub const STREAMS: Term = Term {
    label: "Streams (0 = automatic)",
    explanation: "Parallel transfer lanes per server and direction; zero chooses automatically.",
};
pub const AUTO_STREAMS: Term = Term {
    label: "Automatic stream ceiling",
    explanation: "The maximum automatically chosen HTTP/1.1 lanes per direction.",
};
pub const PING_INTERVAL: Term = Term {
    label: "Idle latency cadence",
    explanation: "Send idle probes after replies or at the selected fixed interval.",
};
pub const LOADED_PING_INTERVAL: Term = Term {
    label: "Loaded latency cadence",
    explanation: "Send probes during transfers after replies or at the selected fixed interval.",
};
pub const LOADED_LATENCY: Term = Term {
    label: "Loaded latency",
    explanation: "Measure round-trip time while data transfers run.",
};
pub const INSECURE: Term = Term {
    label: "Skip TLS verification",
    explanation: "Skip certificate checks; authenticated connections still require verified TLS.",
};
pub const MEDIAN: Term = Term {
    label: "Median",
    explanation: "Half of replied probes took no longer than this round-trip time.",
};
pub const ADDED: Term = Term {
    label: "Added",
    explanation: "Loaded median minus the same server’s idle median; negative values are preserved.",
};
pub const P95: Term = Term {
    label: "p95",
    explanation: "Ninety-five percent of replied probes took no longer than this time.",
};
pub const JITTER: Term = Term {
    label: "Jitter",
    explanation: "Mean absolute change between consecutive replied probes.",
};
pub const PROBE_TIMEOUTS: Term = Term {
    label: "Probe timeouts",
    explanation: "Timed-out probes divided by replied and timed-out probes.",
};
pub const PEAK: Term = Term {
    label: "Peak",
    explanation: "The fastest receiver window of at least 500 milliseconds.",
};
pub const BYTES: Term = Term {
    label: "Received bytes",
    explanation: "Data confirmed by the receiver, including unscored windows.",
};
pub const RECEIVER_ELAPSED: Term = Term {
    label: "Receiver elapsed",
    explanation: "Receiver time covered by the accepted measurement windows.",
};
pub const SAMPLES: Term = Term {
    label: "Samples",
    explanation: "Accepted receiver accounting windows contributing to the result.",
};

pub const VALUES: [Term; 12] = [
    DOWNLOAD,
    UPLOAD,
    LATENCY,
    MEDIAN,
    ADDED,
    P95,
    JITTER,
    PROBE_TIMEOUTS,
    PEAK,
    BYTES,
    RECEIVER_ELAPSED,
    SAMPLES,
];

pub fn protocol(value: Option<graphite_meter_core::discovery::Protocol>) -> Term {
    use graphite_meter_core::discovery::Protocol;
    match value {
        None => Term {
            label: "Automatic",
            explanation: "Choose an advertised HTTP path after checking it.",
        },
        Some(Protocol::Http1) => Term {
            label: "HTTP/1.1",
            explanation: "Use separate HTTP connections for parallel transfer lanes.",
        },
        Some(Protocol::Http2) => Term {
            label: "HTTP/2",
            explanation: "Share one HTTP connection across parallel transfer streams.",
        },
        Some(Protocol::Http3) => Term {
            label: "HTTP/3",
            explanation: "Carry HTTP streams over QUIC.",
        },
        Some(Protocol::Negotiated) => Term {
            label: "Negotiated",
            explanation: "Use the HTTP version selected by the connection handshake.",
        },
    }
}

pub fn throughput_transport(value: Option<graphite_meter_core::discovery::ThroughputTransport>) -> Term {
    use graphite_meter_core::discovery::ThroughputTransport;
    match value {
        None => Term {
            label: "Automatic",
            explanation: "Try an advertised WebTransport path, then use fetch streams when needed.",
        },
        Some(ThroughputTransport::FetchStream) => Term {
            label: "Fetch streams",
            explanation: "Transfer data through streaming HTTP requests and responses.",
        },
        Some(_) => Term {
            label: "WebTransport streams",
            explanation: "Transfer data through continuous reliable QUIC streams.",
        },
    }
}

pub fn latency_transport(value: Option<graphite_meter_core::discovery::LatencyTransport>) -> Term {
    use graphite_meter_core::discovery::LatencyTransport;
    match value {
        None => Term {
            label: "Automatic",
            explanation: "Try an advertised WebTransport path, then use WebSocket when needed.",
        },
        Some(LatencyTransport::WebSocket) => Term {
            label: "WebSocket",
            explanation: "Send latency probes through an ordered reliable connection.",
        },
        Some(LatencyTransport::WebTransport) => Term {
            label: "WebTransport datagrams",
            explanation: "Send latency probes as QUIC datagrams that can arrive out of order or be lost.",
        },
    }
}
