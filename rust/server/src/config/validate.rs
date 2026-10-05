//! Cross-field validation into a `Config`, in Go's order: catalogue, authentication, limits, listeners, origins.

use super::{
    Auth, Config, Lifetimes, Limits, Listener, ListenerKind, Methods, Oidc, PublicOrigins, Secret, TlsFiles,
    settings::{Advertised, Settings},
};
use graphite_meter_proto::{
    discovery::{Protocol, STAGE_LIMITS},
    origin::{BaseUrl, Origin, Scheme},
    text::{quote, safe},
};
use std::collections::HashMap;

pub(super) fn config(settings: Settings) -> Result<Config, String> {
    let mut catalog = settings.catalog.clone();
    catalog.servers[0].name.clone_from(&settings.name);
    catalog.servers[0].location.clone_from(&settings.location);
    catalog
        .validate()
        .map_err(|error| format!("GM_SERVER_NAME, GM_SERVER_LOCATION or the server catalogue: {error}"))?;
    let auth = auth(&settings)?;
    let (limits, lifetimes) = limits(&settings)?;
    let listeners = listeners(&settings)?;
    let public = origins(&settings)?;
    let tls = listeners
        .iter()
        .any(|listener| listener.kind != ListenerKind::H1)
        .then(|| TlsFiles {
            cert: settings.tls_cert.clone().into(),
            key: settings.tls_key.clone().into(),
        });
    Ok(Config {
        listeners,
        tls,
        public,
        result_history_default: settings.result_history_default,
        verbose: settings.verbose,
        trusted_proxies: settings.trusted_proxies.0,
        limits,
        max_buffer_bytes: settings.max_buffer_bytes,
        lifetimes,
        auth,
        catalog,
    })
}

fn limits(settings: &Settings) -> Result<(Limits, Lifetimes), String> {
    let operations = ("GM_MAX_ACTIVE_MEASUREMENTS", settings.max_active_measurements);
    let client_operations = ("GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT", settings.max_active_measurements_per_client);
    let sessions = ("GM_MAX_ACTIVE_SESSIONS", settings.max_active_sessions);
    let client_sessions = ("GM_MAX_SESSIONS_PER_CLIENT", settings.max_sessions_per_client);
    let connections = ("GM_MAX_CONNECTIONS", settings.max_connections);
    let client_connections = ("GM_MAX_CONNECTIONS_PER_CLIENT", settings.max_connections_per_client);
    let all = [operations, client_operations, sessions, client_sessions, connections, client_connections];
    if let Some((name, _)) = all.into_iter().find(|&(_, value)| value == 0) {
        return Err(format!("{name} must be greater than zero"));
    }
    for ((name, value), (outer, limit)) in [
        (client_operations, operations),
        (sessions, operations),
        (client_sessions, client_operations),
        (client_sessions, sessions),
        (client_connections, connections),
    ] {
        if value > limit {
            return Err(format!("{name} must not exceed {outer}"));
        }
    }
    let lifetimes = Lifetimes {
        operation: settings.max_operation_duration,
        session: settings.max_session_duration,
        stage: settings.max_stage_duration,
    };
    if lifetimes.operation.is_zero() {
        return Err("GM_MAX_OPERATION_DURATION must be greater than zero".into());
    }
    if lifetimes.session < lifetimes.operation {
        return Err("GM_MAX_SESSION_DURATION must be at least GM_MAX_OPERATION_DURATION".into());
    }
    if !STAGE_LIMITS.contains(&lifetimes.stage) {
        return Err("GM_MAX_STAGE_DURATION must be from 1s to 24h".into());
    }
    let limits = Limits {
        operations: operations.1,
        operations_per_client: client_operations.1,
        sessions: sessions.1,
        sessions_per_client: client_sessions.1,
        connections: connections.1,
        connections_per_client: client_connections.1,
    };
    Ok((limits, lifetimes))
}

/// Each listener's address and public origin settings as given.
fn natives(settings: &Settings) -> [(ListenerKind, &str, &str); 4] {
    [
        (ListenerKind::H1, &settings.h1_addr, &settings.h1_public_origin),
        (ListenerKind::H1Tls, &settings.h1_tls_addr, &settings.h1_tls_public_origin),
        (ListenerKind::H2, &settings.h2_addr, &settings.h2_public_origin),
        (ListenerKind::H3, &settings.h3_addr, &settings.h3_public_origin),
    ]
}

fn advertised(settings: &Settings, kind: ListenerKind, address: &str) -> bool {
    !address.is_empty()
        && match &settings.advertised {
            Advertised::All => true,
            Advertised::Only(kinds) => kinds.contains(&kind),
        }
}

/// The enabled listeners; their public origins are checked with the other origins.
fn listeners(settings: &Settings) -> Result<Vec<Listener>, String> {
    let natives = natives(settings);
    if settings.h1_addr.is_empty() {
        return Err("GM_H1_ADDR must not be empty".into());
    }
    let tls = natives[1..].iter().any(|(_, address, _)| !address.is_empty());
    if tls && (settings.tls_cert.is_empty() || settings.tls_key.is_empty()) {
        return Err("GM_TLS_CERT and GM_TLS_KEY are required when a native TLS listener is enabled".into());
    }
    for (index, (kind, address, _)) in natives.iter().enumerate() {
        if let Some((other, ..)) = natives[index + 1..]
            .iter()
            .find(|(_, other, _)| !address.is_empty() && address == other)
        {
            return Err(format!("{}_ADDR and {}_ADDR must differ", kind.env(), other.env()));
        }
    }
    if let Advertised::Only(kinds) = &settings.advertised
        && let Some(kind) = kinds.iter().find(|kind| natives[**kind as usize].1.is_empty())
    {
        return Err(format!("GM_ADVERTISED_NATIVE_ENDPOINTS includes disabled endpoint {:?}", kind.name()));
    }
    let enabled = natives.into_iter().filter(|(_, address, _)| !address.is_empty());
    Ok(enabled
        .map(|(kind, address, public)| Listener {
            kind,
            address: address.into(),
            public_origin: Origin::parse(public).ok(),
            advertised: advertised(settings, kind, address),
        })
        .collect())
}

fn origins(settings: &Settings) -> Result<PublicOrigins, String> {
    let mut throughput = !settings.public_origins.is_empty() || !settings.public_throughput_origins.is_empty();
    let mut deterministic: HashMap<Origin, Protocol> = HashMap::new();
    for (kind, address, public) in natives(settings) {
        let origin = Origin::parse(public)
            .ok()
            .filter(|origin| origin.scheme == kind.scheme());
        if !public.is_empty() && origin.is_none() {
            let scheme = kind.scheme().name();
            return Err(format!("{}_PUBLIC_ORIGIN must be an origin with {scheme} scheme", kind.env()));
        }
        if !advertised(settings, kind, address) {
            continue;
        }
        throughput = true;
        if let Some(origin) = origin
            && deterministic
                .insert(origin, kind.protocol())
                .is_some_and(|other| other != kind.protocol())
        {
            return Err(format!(
                "native origin {} is advertised with multiple deterministic protocols",
                quote(public)
            ));
        }
    }
    let mut parsed: [Vec<BaseUrl>; 3] = Default::default();
    for ((env, list), parsed) in public_lists(settings).into_iter().zip(&mut parsed) {
        for text in list {
            let origin = match text.as_str() {
                "self" => BaseUrl::Served,
                text => BaseUrl::Origin(
                    Origin::parse(text).map_err(|_| format!("{env} contains invalid origin {}", quote(text)))?,
                ),
            };
            if let BaseUrl::Origin(origin) = &origin
                && env != "GM_PUBLIC_LATENCY_ORIGINS"
                && deterministic.contains_key(origin)
            {
                return Err(format!(
                    "origin {} cannot be both native deterministic and public negotiated",
                    quote(text)
                ));
            }
            parsed.push(origin);
        }
    }
    if !throughput {
        return Err("configuration advertises no throughput endpoint".into());
    }
    let [both, throughput, latency] = parsed;
    Ok(PublicOrigins { both, throughput, latency })
}

fn public_lists(settings: &Settings) -> [(&'static str, &Vec<String>); 3] {
    [
        ("GM_PUBLIC_ORIGINS", &settings.public_origins),
        ("GM_PUBLIC_THROUGHPUT_ORIGINS", &settings.public_throughput_origins),
        ("GM_PUBLIC_LATENCY_ORIGINS", &settings.public_latency_origins),
    ]
}

fn auth(settings: &Settings) -> Result<Option<Auth>, String> {
    match settings.auth_mode.as_str() {
        "off" if settings.auth_given() => {
            return Err("authentication settings require GM_AUTH_MODE to be enabled".into());
        }
        "off" => return Ok(None),
        "password" | "oidc" | "hybrid" => {}
        _ => return Err("GM_AUTH_MODE must be off, password, oidc, or hybrid".into()),
    }
    let public = &settings.auth_public_url;
    let public_origin = Origin::parse(public)
        .ok()
        .filter(|origin| origin.scheme == Scheme::Https)
        .ok_or("GM_AUTH_PUBLIC_URL must be an HTTPS origin with no path, query, or fragment")?;
    if public.rsplit_once(':').is_some_and(|(_, port)| port == "443") {
        return Err("GM_AUTH_PUBLIC_URL must omit the default HTTPS port".into());
    }
    let methods = methods(settings)?;
    advertised_auth_origins(settings, &public_origin)?;
    Ok(Some(Auth { public_origin, methods }))
}

/// Go's secret checks, in its order: both exclusions, each method's sources, then the provider's settings.
fn methods(settings: &Settings) -> Result<Methods, String> {
    let password = secret(&settings.auth_password_hash, &settings.auth_password_hash_file);
    let client_secret = secret(&settings.auth_oidc_client_secret, &settings.auth_oidc_client_secret_file);
    let (wants_password, wants_oidc) = (settings.auth_mode != "oidc", settings.auth_mode != "password");
    let (issuer, client_id) = (&settings.auth_oidc_issuer, &settings.auth_oidc_client_id);
    let groups = &settings.auth_oidc_allowed_groups;
    let oidc_given = !issuer.is_empty() || !client_id.is_empty() || client_secret.is_some() || !groups.is_empty();
    let name = &settings.auth_oidc_provider_name;
    let failure = if !settings.auth_password_hash.is_empty() && !settings.auth_password_hash_file.is_empty() {
        "GM_AUTH_PASSWORD_HASH and GM_AUTH_PASSWORD_HASH_FILE are mutually exclusive"
    } else if !settings.auth_oidc_client_secret.is_empty() && !settings.auth_oidc_client_secret_file.is_empty() {
        "GM_AUTH_OIDC_CLIENT_SECRET and GM_AUTH_OIDC_CLIENT_SECRET_FILE are mutually exclusive"
    } else if wants_password && password.is_none() {
        "password authentication requires exactly one password hash source"
    } else if !wants_password && password.is_some() {
        "password hash configured while password authentication is disabled"
    } else if wants_oidc && (issuer.is_empty() || client_id.is_empty() || groups.is_empty() || client_secret.is_none())
    {
        "OIDC authentication requires issuer, client ID, one client secret source, and allowed groups"
    } else if !wants_oidc && oidc_given {
        "OIDC settings configured while OIDC authentication is disabled"
    } else if wants_oidc && !https_issuer(issuer) {
        "GM_AUTH_OIDC_ISSUER must be an HTTPS URL with no credentials, query, or fragment"
    } else if wants_oidc && name.trim().is_empty() {
        "GM_AUTH_OIDC_PROVIDER_NAME must not be empty"
    } else if name.len() > 64 || !name.chars().all(safe) {
        "GM_AUTH_OIDC_PROVIDER_NAME must be at most 64 bytes of UTF-8 without control characters"
    } else {
        let oidc = client_secret.map(|secret| Oidc {
            issuer: issuer.clone(),
            client_id: client_id.clone(),
            secret,
            allowed_groups: groups.clone(),
            provider_name: name.clone(),
        });
        return Ok(match (password, oidc) {
            (Some(password), Some(oidc)) => Methods::Hybrid(password, oidc),
            (Some(password), _) => Methods::Password(password),
            (None, Some(oidc)) => Methods::Oidc(oidc),
            (None, None) => unreachable!("an enabled mode has a method"),
        });
    };
    Err(failure.into())
}

fn secret(inline: &str, file: &str) -> Option<Secret> {
    match (inline, file) {
        ("", "") => None,
        ("", file) => Some(Secret::File(file.into())),
        (inline, _) => Some(Secret::Inline(inline.into())),
    }
}

/// An HTTPS URL with an ASCII host and path, and no credentials, query or fragment.
fn https_issuer(issuer: &str) -> bool {
    Origin::split(issuer).is_ok_and(|(origin, rest)| origin.scheme == Scheme::Https && !rest.contains('?'))
}

/// Under authentication clear HTTP/1.1 is not advertised, and advertised origins share the sign-in hostname.
fn advertised_auth_origins(settings: &Settings, public: &Origin) -> Result<(), String> {
    if advertised(settings, ListenerKind::H1, &settings.h1_addr) {
        return Err("clear HTTP/1.1 cannot be advertised when authentication is enabled".into());
    }
    let natives = natives(settings);
    let lists = public_lists(settings).into_iter();
    let mut given: Vec<(String, &str)> = lists
        .flat_map(|(env, list)| list.iter().map(move |text| (env.to_owned(), text.as_str())))
        .collect();
    given.extend(
        natives[1..]
            .iter()
            .map(|(kind, _, public)| (format!("{}_PUBLIC_ORIGIN", kind.env()), *public)),
    );
    for (env, text) in given
        .into_iter()
        .filter(|(_, text)| !text.is_empty() && *text != "self")
    {
        let same_host =
            Origin::split(text).is_ok_and(|(origin, _)| origin.scheme == Scheme::Https && origin.host == public.host);
        if !same_host {
            return Err(format!("{env} must use HTTPS and the canonical authentication hostname"));
        }
    }
    Ok(())
}
