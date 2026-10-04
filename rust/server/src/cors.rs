//! Browser response visibility, separate from request authentication.

use crate::{auth::secure_browser_origin, client_address::unique_header};
use graphite_meter_core::route::Route;
use http::{HeaderMap, HeaderValue, header};

/// The caller must authenticate an actual request before selecting its access.
/// A successful preflight only permits the browser to attempt that request.
#[derive(Clone, Copy, Debug)]
pub enum Access<'a> {
    Public,
    Cookie(&'a HeaderValue),
    Bearer(&'a HeaderValue),
}

impl Access<'_> {
    pub fn apply_response(self, headers: &mut HeaderMap) {
        let (origin, exposed) = match self {
            Self::Public => (HeaderValue::from_static("*"), "X-Graphite-Upload-Refusal, Retry-After"),
            Self::Cookie(origin) => (
                origin.clone(),
                "X-Graphite-Upload-Refusal, Retry-After, Graphite-Meter-Auth, Graphite-Meter-Auth-URL",
            ),
            Self::Bearer(origin) => (
                origin.clone(),
                "X-Graphite-Upload-Refusal, Retry-After, Graphite-Meter-Auth, Graphite-Meter-Auth-URL, \
                 Graphite-Meter-Browser-Auth",
            ),
        };
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
        headers.insert("timing-allow-origin", origin);

        // Replacing a policy must not retain a broader previous credential grant.
        headers.remove(header::ACCESS_CONTROL_ALLOW_CREDENTIALS);
        headers.remove(header::ACCESS_CONTROL_EXPOSE_HEADERS);
        if matches!(self, Self::Cookie(_)) {
            let credentials = HeaderValue::from_static("true");
            headers.insert(header::ACCESS_CONTROL_ALLOW_CREDENTIALS, credentials);
        }
        headers.insert(header::ACCESS_CONTROL_EXPOSE_HEADERS, HeaderValue::from_static(exposed));
        if !matches!(self, Self::Public) {
            headers.append(header::VARY, HeaderValue::from_static("Origin"));
        }
    }

    pub fn apply_measurement(self, headers: &mut HeaderMap) {
        self.apply_response(headers);
        headers.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("7200"));
        let methods = HeaderValue::from_static("GET, POST, DELETE, OPTIONS");
        headers.insert(header::ACCESS_CONTROL_ALLOW_METHODS, methods);
        let allowed = match self {
            Self::Public => "*",
            Self::Cookie(_) => "Authorization, Content-Type, X-CSRF-Token",
            Self::Bearer(_) => "Authorization, Content-Type",
        };
        headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static(allowed));
    }
}

/// Validate an authenticated deployment's OPTIONS request. `secure` must come
/// from the listener/trusted-proxy policy, not an unchecked forwarded header.
/// Missing routes, invalid headers and insecure origins all fail closed.
pub fn authenticated_preflight<'a>(
    public_origin: &'a HeaderValue,
    secure: bool,
    route: Option<Route>,
    request: &'a HeaderMap,
) -> Option<Access<'a>> {
    if !secure {
        return None;
    }
    let route = route?;
    let method = unique_header(request, header::ACCESS_CONTROL_REQUEST_METHOD)?
        .to_str()
        .ok()?;
    if !route.methods().contains(&method) {
        return None;
    }
    let origin = unique_header(request, header::ORIGIN)?;
    let same_origin = origin == public_origin;
    if !same_origin && (route == Route::Servers || !origin.to_str().is_ok_and(secure_browser_origin)) {
        return None;
    }

    let requested_headers = match unique_header(request, header::ACCESS_CONTROL_REQUEST_HEADERS) {
        Some(value) => value.to_str().ok()?,
        None if request.contains_key(header::ACCESS_CONTROL_REQUEST_HEADERS) => return None,
        None => "",
    };
    let mut has_authorization = false;
    for name in requested_headers.split(',').map(str::trim) {
        if name.eq_ignore_ascii_case("authorization") {
            has_authorization = true;
        } else if !(name.is_empty()
            || name.eq_ignore_ascii_case("content-type")
            || same_origin && name.eq_ignore_ascii_case("x-csrf-token"))
        {
            return None;
        }
    }
    if same_origin {
        Some(Access::Cookie(public_origin))
    } else if has_authorization {
        Some(Access::Bearer(origin))
    } else {
        None
    }
}
