//! The settings table: one row per setting drives its environment variable, its flag and its usage line.

use super::{ListenerKind, catalog};
use graphite_meter_proto::{
    catalog::ServerCatalog,
    discovery::DEFAULT_STAGE_LIMIT,
    duration,
    flag::{self, Flag, Kind, Parsed},
};
use ipnet::IpNet;
use std::{collections::BTreeSet, ffi::OsString, io::Write, time::Duration};

/// The native endpoints discovery advertises: `all` or the named ones, `none` being the empty set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Advertised {
    All,
    Only(BTreeSet<ListenerKind>),
}

/// `GM_TRUSTED_PROXIES`: CIDRs whose peers may name their client.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct TrustedProxies(pub Vec<IpNet>);

/// A setting's value type: how trimmed text sets it and how a usage line shows it.
trait Value {
    const KIND: Kind = Kind::Value;

    fn set(&mut self, text: &str) -> Result<(), String>;

    /// The value as Go's flag shows it, empty for the type's zero value but for booleans.
    fn show(&self) -> String;
}

impl Value for String {
    fn set(&mut self, text: &str) -> Result<(), String> {
        text.clone_into(self);
        Ok(())
    }

    fn show(&self) -> String {
        self.clone()
    }
}

impl Value for Vec<String> {
    fn set(&mut self, text: &str) -> Result<(), String> {
        *self = split_list(text);
        Ok(())
    }

    fn show(&self) -> String {
        match self.is_empty() {
            true => String::new(),
            false => format!("[{}]", self.join(" ")),
        }
    }
}

impl Value for usize {
    /// A negative number reads as zero, which validation refuses by name.
    fn set(&mut self, text: &str) -> Result<(), String> {
        if !text.is_empty() {
            let number: i64 = text.parse().map_err(|_| "must be an integer")?;
            *self = usize::try_from(number).unwrap_or(0);
        }
        Ok(())
    }

    fn show(&self) -> String {
        match self {
            0 => String::new(),
            number => number.to_string(),
        }
    }
}

impl Value for bool {
    const KIND: Kind = Kind::Bool;

    fn set(&mut self, text: &str) -> Result<(), String> {
        if !text.is_empty() {
            *self = flag::parse_bool(&text.to_lowercase()).ok_or("must be true/false or 1/0")?;
        }
        Ok(())
    }

    fn show(&self) -> String {
        self.to_string()
    }
}

impl Value for Duration {
    /// A negative duration reads as zero, which validation refuses by name.
    fn set(&mut self, text: &str) -> Result<(), String> {
        if !text.is_empty() {
            let nanos = duration::parse(text).map_err(|error| error.to_string())?;
            *self = Duration::from_nanos(u64::try_from(nanos).unwrap_or(0));
        }
        Ok(())
    }

    fn show(&self) -> String {
        match self.is_zero() {
            true => String::new(),
            false => duration::format(*self),
        }
    }
}

impl Value for Advertised {
    fn set(&mut self, text: &str) -> Result<(), String> {
        *self = match text {
            "" => return Ok(()),
            "all" => Self::All,
            "none" => Self::Only(BTreeSet::new()),
            names => Self::Only(
                split_list(names)
                    .iter()
                    .map(|name| ListenerKind::from_name(name).ok_or_else(|| format!("unknown endpoint {name:?}")))
                    .collect::<Result<_, _>>()?,
            ),
        };
        Ok(())
    }

    fn show(&self) -> String {
        match self {
            Self::All => "all".into(),
            Self::Only(kinds) if kinds.is_empty() => "none".into(),
            Self::Only(kinds) => {
                let mut names: Vec<_> = kinds.iter().map(|kind| kind.name()).collect();
                names.sort_unstable();
                names.join(",")
            }
        }
    }
}

impl Value for TrustedProxies {
    fn set(&mut self, text: &str) -> Result<(), String> {
        self.0 = split_list(text)
            .iter()
            .map(|cidr| {
                let prefix = prefix(cidr).map_err(|error| format!("{cidr:?}: netip.ParsePrefix({cidr:?}): {error}"))?;
                match prefix.prefix_len() {
                    0 => Err(format!("{cidr:?} trusts every address; list the proxy's actual CIDR instead")),
                    _ => Ok(prefix.trunc()),
                }
            })
            .collect::<Result<_, _>>()?;
        Ok(())
    }

    fn show(&self) -> String {
        String::new()
    }
}

/// Go's `netip.ParsePrefix`: no sign or leading zero in the length.
fn prefix(cidr: &str) -> Result<IpNet, String> {
    let (address, bits) = cidr.rsplit_once('/').ok_or("no '/'")?;
    let address = address
        .parse()
        .map_err(|_| format!("ParseAddr({address:?}): unable to parse IP"))?;
    let canonical = bits.len() == 1 || bits.starts_with(|c: char| matches!(c, '1'..='9'));
    let bits = Some(bits)
        .filter(|bits| canonical && bits.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|bits| bits.parse::<u8>().ok())
        .ok_or_else(|| format!("bad bits after slash: {bits:?}"))?;
    IpNet::new(address, bits).map_err(|_| "prefix length out of range".into())
}

/// Comma-separated entries, trimmed, empty ones dropped.
fn split_list(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_owned)
        .collect()
}

/// One setting: its variable, its flag (empty for an environment-only secret) and its usage text.
struct Setting {
    env: &'static str,
    flag: &'static str,
    usage: &'static str,
    kind: Kind,
    set: fn(&mut Settings, &str) -> Result<(), String>,
    show: fn(&Settings) -> String,
}

/// Declares `Settings`, its defaults and `SETTINGS` from one row per setting.
macro_rules! settings {
    ($($field:ident: $type:ty = $default:expr, $env:literal, $flag:literal, $usage:literal;)+) => {
        /// Settings as given, before validation.
        pub(super) struct Settings {
            $(pub $field: $type,)+
            pub catalog: ServerCatalog,
            /// The variables set by the environment or a flag, also to their defaults.
            pub given: BTreeSet<&'static str>,
        }

        impl Default for Settings {
            fn default() -> Self {
                Self { $($field: $default,)+ catalog: ServerCatalog::singleton(), given: BTreeSet::new() }
            }
        }

        const SETTINGS: &[Setting] = &[$(Setting {
            env: $env,
            flag: $flag,
            usage: $usage,
            kind: <$type as Value>::KIND,
            set: |settings, text| {
                settings.given.insert($env);
                Value::set(&mut settings.$field, text.trim())
            },
            show: |settings| Value::show(&settings.$field),
        }),+];
    };
}

settings! {
    h1_addr: String = ":7246".into(), "GM_H1_ADDR", "h1-addr", "clear HTTP/1.1 listen `address`";
    h1_tls_addr: String = String::new(), "GM_H1_TLS_ADDR", "h1-tls-addr",
        "HTTPS HTTP/1.1 listen `address`; empty disables it";
    h2_addr: String = String::new(), "GM_H2_ADDR", "h2-addr", "HTTP/2 TLS listen `address`; empty disables it";
    h3_addr: String = String::new(), "GM_H3_ADDR", "h3-addr",
        "HTTP/3 UDP and bootstrap TCP listen `address`; empty disables it";
    tls_cert: String = String::new(), "GM_TLS_CERT", "tls-cert", "TLS certificate PEM `path`";
    tls_key: String = String::new(), "GM_TLS_KEY", "tls-key", "TLS private key PEM `path`";
    h1_public_origin: String = String::new(), "GM_H1_PUBLIC_ORIGIN", "h1-public-origin",
        "public `origin` of the native clear HTTP/1.1 listener";
    h1_tls_public_origin: String = String::new(), "GM_H1_TLS_PUBLIC_ORIGIN", "h1-tls-public-origin",
        "public `origin` of the native HTTPS HTTP/1.1 listener";
    h2_public_origin: String = String::new(), "GM_H2_PUBLIC_ORIGIN", "h2-public-origin",
        "public `origin` of the native HTTP/2 listener";
    h3_public_origin: String = String::new(), "GM_H3_PUBLIC_ORIGIN", "h3-public-origin",
        "public `origin` of the native HTTP/3 listener";
    advertised: Advertised = Advertised::All, "GM_ADVERTISED_NATIVE_ENDPOINTS", "advertised-native-endpoints",
        "all, none, or comma-separated native endpoint `names`";
    public_origins: Vec<String> = Vec::new(), "GM_PUBLIC_ORIGINS", "public-origins",
        "comma-separated negotiated `origins` providing throughput and latency";
    public_throughput_origins: Vec<String> = Vec::new(), "GM_PUBLIC_THROUGHPUT_ORIGINS", "public-throughput-origins",
        "comma-separated negotiated throughput `origins`";
    public_latency_origins: Vec<String> = Vec::new(), "GM_PUBLIC_LATENCY_ORIGINS", "public-latency-origins",
        "comma-separated WebSocket latency `origins`";
    name: String = "graphite-meter".into(), "GM_SERVER_NAME", "name", "server `name` advertised in /preflight";
    location: String = String::new(), "GM_SERVER_LOCATION", "location", "server `location` label";
    result_history_default: bool = false, "GM_RESULT_HISTORY_DEFAULT", "result-history-default",
        "save completed browser results on this device by default";
    verbose: bool = false, "GM_VERBOSE", "verbose", "log per-second download/upload throughput";
    max_active_measurements: usize = 256, "GM_MAX_ACTIVE_MEASUREMENTS", "max-active-measurements",
        "maximum `number` of concurrent measurement handlers";
    max_active_measurements_per_client: usize = 32, "GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT",
        "max-active-measurements-per-client", "maximum `number` of concurrent measurement handlers per client";
    max_active_sessions: usize = 64, "GM_MAX_ACTIVE_SESSIONS", "max-active-sessions",
        "maximum `number` of concurrent WebTransport sessions, a share of the measurement pool";
    max_sessions_per_client: usize = 8, "GM_MAX_SESSIONS_PER_CLIENT", "max-sessions-per-client",
        "maximum `number` of concurrent WebTransport sessions per client";
    max_buffer_bytes: usize = 8 << 30, "GM_MAX_BUFFER_BYTES", "max-buffer-bytes",
        "`bytes` of connection buffers shared by QUIC and HTTP/2";
    max_connections: usize = 4096, "GM_MAX_CONNECTIONS", "max-connections",
        "maximum `number` of concurrent TCP and QUIC connections";
    max_connections_per_client: usize = 64, "GM_MAX_CONNECTIONS_PER_CLIENT", "max-connections-per-client",
        "maximum `number` of concurrent connections per direct client";
    max_operation_duration: Duration = Duration::from_secs(5 * 60), "GM_MAX_OPERATION_DURATION",
        "max-operation-duration", "maximum measurement operation `duration`";
    max_session_duration: Duration = Duration::from_secs(2 * 60 * 60), "GM_MAX_SESSION_DURATION",
        "max-session-duration", "maximum WebTransport session `duration`";
    max_stage_duration: Duration = DEFAULT_STAGE_LIMIT, "GM_MAX_STAGE_DURATION", "max-stage-duration",
        "longest stage `duration` clients may plan, 1s to 24h";
    trusted_proxies: TrustedProxies = TrustedProxies::default(), "GM_TRUSTED_PROXIES", "", "";
    auth_mode: String = "off".into(), "GM_AUTH_MODE", "auth-mode",
        "authentication `mode`: off, password, oidc, or hybrid";
    auth_public_url: String = String::new(), "GM_AUTH_PUBLIC_URL", "auth-public-url", "canonical HTTPS UI `origin`";
    auth_password_hash: String = String::new(), "GM_AUTH_PASSWORD_HASH", "", "";
    auth_password_hash_file: String = String::new(), "GM_AUTH_PASSWORD_HASH_FILE", "auth-password-hash-file",
        "`file` containing the operator Argon2id PHC hash";
    auth_oidc_issuer: String = String::new(), "GM_AUTH_OIDC_ISSUER", "auth-oidc-issuer", "OIDC issuer `URL`";
    auth_oidc_client_id: String = String::new(), "GM_AUTH_OIDC_CLIENT_ID", "auth-oidc-client-id", "OIDC client `ID`";
    auth_oidc_client_secret: String = String::new(), "GM_AUTH_OIDC_CLIENT_SECRET", "", "";
    auth_oidc_client_secret_file: String = String::new(), "GM_AUTH_OIDC_CLIENT_SECRET_FILE",
        "auth-oidc-client-secret-file", "`file` containing the OIDC client secret";
    auth_oidc_allowed_groups: Vec<String> = Vec::new(), "GM_AUTH_OIDC_ALLOWED_GROUPS", "auth-oidc-allowed-groups",
        "comma-separated case-sensitive OIDC `groups`";
    auth_oidc_provider_name: String = "Authelia".into(), "GM_AUTH_OIDC_PROVIDER_NAME", "auth-oidc-provider-name",
        "OIDC provider `label`";
}

/// The rows that have a flag, as the flag parser reads them.
fn flags() -> Vec<Flag<Settings>> {
    let rows = SETTINGS.iter().filter(|setting| !setting.flag.is_empty());
    rows.map(|setting| Flag {
        name: setting.flag,
        kind: setting.kind,
        usage: setting.usage,
        env: Some(setting.env),
        set: setting.set,
        show: setting.show,
    })
    .collect()
}

/// Loads the environment, then the flags over it; `None` when the usage was asked for. A flag error goes to `usage`
/// with the usage and precedes an environment error.
pub(super) fn load(
    env: &dyn Fn(&str) -> Option<OsString>,
    args: impl IntoIterator<Item = OsString>,
    usage: &mut dyn Write,
) -> Result<Option<Settings>, String> {
    let mut settings = Settings::default();
    let from_env = settings.load_env(env);
    let flags = flags();
    match flag::parse(&flags, &mut settings, args) {
        Ok(Parsed::Help) => {
            let _ = write_usage(usage, &flags);
            return Ok(None);
        }
        Ok(Parsed::Arguments(_)) => {}
        Err(error) => {
            let _ = writeln!(usage, "{error}").and_then(|()| write_usage(usage, &flags));
            return Err(error.0);
        }
    }
    from_env?;
    settings.cover_stage_limit();
    Ok(Some(settings))
}

impl Settings {
    /// Applies each variable present, stopping at the first refused one, then loads the catalogue.
    fn load_env(&mut self, env: &dyn Fn(&str) -> Option<OsString>) -> Result<(), String> {
        let text = |name: &str| match env(name) {
            Some(value) => value
                .into_string()
                .map(Some)
                .map_err(|_| format!("{name}: must be UTF-8")),
            None => Ok(None),
        };
        for setting in SETTINGS {
            if let Some(value) = text(setting.env)? {
                (setting.set)(self, &value).map_err(|error| format!("{}: {error}", setting.env))?;
            }
        }
        self.catalog = catalog::load(text("GM_SERVER_CATALOG")?, text("GM_SERVER_CATALOG_FILE")?)?;
        Ok(())
    }

    /// Lifetimes left unset grow to cover the stage limit plus a minute, and a session at least an operation.
    fn cover_stage_limit(&mut self) {
        if !self.given.contains("GM_MAX_OPERATION_DURATION") {
            let covering = self.max_stage_duration.saturating_add(Duration::from_secs(60));
            self.max_operation_duration = self.max_operation_duration.max(covering);
        }
        if !self.given.contains("GM_MAX_SESSION_DURATION") {
            self.max_session_duration = self.max_session_duration.max(self.max_operation_duration);
        }
    }

    /// Whether any authentication setting but the mode was given.
    pub fn auth_given(&self) -> bool {
        self.given
            .iter()
            .any(|env| env.starts_with("GM_AUTH_") && *env != "GM_AUTH_MODE")
    }
}

fn write_usage(usage: &mut dyn Write, flags: &[Flag<Settings>]) -> std::io::Result<()> {
    write!(
        usage,
        "Usage:\n  graphite-meter [flags]\n  graphite-meter version | --version\n  graphite-meter hash-password    \
         read a password twice on stdin, print its Argon2id hash\n  graphite-meter --legal           \
         print the reviewed project and dependency notices\n\nFlags:\n{}",
        flag::defaults(flags, &Settings::default())
    )
}
