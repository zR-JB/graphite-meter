//! The headers every answer gets last: cross-origin access, hardening, bootstrap Alt-Svc and connection hints.

use super::App;
use crate::transport::body::Body;
use http::{HeaderMap, HeaderValue, Response, Version, header};

/// Who may read an answer from another origin.
#[derive(Debug, Clone)]
pub enum Access {
    /// Anyone, without credentials: authentication is off.
    Public,
    /// The UI origin, with its session cookie.
    Cookie(HeaderValue),
    /// An approved browser origin, with a bearer grant.
    Bearer(HeaderValue),
}

impl Access {
    /// The headers of any answer, replacing those of a broader access.
    pub fn apply(&self, headers: &mut HeaderMap) {
        const PUBLIC: &str = "X-Graphite-Upload-Refusal, Retry-After";
        const AUTH: &str = "Graphite-Meter-Auth, Graphite-Meter-Auth-URL, X-Graphite-Upload-Refusal, Retry-After";
        const BEARER: &str = "Graphite-Meter-Auth, Graphite-Meter-Auth-URL, X-Graphite-Upload-Refusal, Retry-After, \
                              Graphite-Meter-Browser-Auth";
        let (origin, exposed) = match self {
            Self::Public => (HeaderValue::from_static("*"), PUBLIC),
            Self::Cookie(origin) => (origin.clone(), AUTH),
            Self::Bearer(origin) => (origin.clone(), BEARER),
        };
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
        headers.insert("timing-allow-origin", origin);
        headers.insert(header::ACCESS_CONTROL_EXPOSE_HEADERS, HeaderValue::from_static(exposed));
        headers.remove(header::ACCESS_CONTROL_ALLOW_CREDENTIALS);
        if let Self::Cookie(_) = self {
            headers.insert(header::ACCESS_CONTROL_ALLOW_CREDENTIALS, HeaderValue::from_static("true"));
        }
        if !matches!(self, Self::Public) {
            headers.append(header::VARY, HeaderValue::from_static("Origin"));
        }
    }

    /// The headers of a preflight's answer; with authentication off every route answer carries them.
    pub fn apply_measurement(&self, headers: &mut HeaderMap) {
        self.apply(headers);
        let allowed = match self {
            Self::Public => "*",
            Self::Cookie(_) => "Authorization, Content-Type, X-CSRF-Token",
            Self::Bearer(_) => "Authorization, Content-Type",
        };
        let methods = HeaderValue::from_static("GET, POST, DELETE, OPTIONS");
        headers.insert(header::ACCESS_CONTROL_ALLOW_METHODS, methods);
        headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static(allowed));
        headers.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("7200"));
    }
}

/// The hardening headers, and HSTS once a request is known to have arrived over TLS.
pub fn harden(headers: &mut HeaderMap, secure: bool) {
    headers.insert("referrer-policy", HeaderValue::from_static("same-origin"));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    let permissions = HeaderValue::from_static("camera=(), microphone=(), geolocation=()");
    headers.insert("permissions-policy", permissions);
    if secure {
        headers.insert(header::STRICT_TRANSPORT_SECURITY, HeaderValue::from_static("max-age=31536000"));
    }
}

/// Ends an HTTP/1 connection after this answer; later versions carry no connection headers.
pub fn close(headers: &mut HeaderMap, version: Version) {
    if version <= Version::HTTP_11 {
        headers.insert(header::CONNECTION, HeaderValue::from_static("close"));
    }
}

impl App {
    /// Applies a passed answer's headers: `access`, hardening once authenticated, and a bootstrap probe's HTTP/3 port.
    pub(super) fn finalize(
        &self,
        response: &mut Response<Body>,
        access: Option<&Access>,
        bootstrap: bool,
        version: Version,
    ) {
        let headers = response.headers_mut();
        match access {
            Some(Access::Public) => Access::Public.apply_measurement(headers),
            Some(access) => access.apply(headers),
            None => {}
        }
        if self.auth.enabled() {
            harden(headers, true);
        }
        if bootstrap && let Some(alt_svc) = &self.alt_svc {
            headers.insert(header::ALT_SVC, alt_svc.clone());
            close(headers, version);
        }
    }
}
