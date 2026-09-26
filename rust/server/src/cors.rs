//! Browser response visibility, separate from request authentication.

use crate::route;
use graphite_meter_core::{origin::canonical_origin, route::Route};
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
        let origin = match self {
            Self::Public => HeaderValue::from_static("*"),
            Self::Cookie(origin) | Self::Bearer(origin) => origin.clone(),
        };
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
        headers.insert("timing-allow-origin", origin);

        // Replacing a policy must not retain a broader previous credential grant.
        headers.remove(header::ACCESS_CONTROL_ALLOW_CREDENTIALS);
        headers.remove(header::ACCESS_CONTROL_EXPOSE_HEADERS);
        match self {
            Self::Public => {}
            Self::Cookie(_) => {
                headers.insert(
                    header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
                    HeaderValue::from_static("true"),
                );
                headers.insert(
                    header::ACCESS_CONTROL_EXPOSE_HEADERS,
                    HeaderValue::from_static("Graphite-Meter-Auth, Graphite-Meter-Auth-URL"),
                );
            }
            Self::Bearer(_) => {
                headers.insert(
                    header::ACCESS_CONTROL_EXPOSE_HEADERS,
                    HeaderValue::from_static(
                        "Graphite-Meter-Auth, Graphite-Meter-Auth-URL, Graphite-Meter-Browser-Auth",
                    ),
                );
            }
        }
        if !matches!(self, Self::Public) {
            headers.append(header::VARY, HeaderValue::from_static("Origin"));
        }
    }

    pub fn apply_measurement(self, headers: &mut HeaderMap) {
        self.apply_response(headers);
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, POST, DELETE, OPTIONS"),
        );
        let allowed_headers = match self {
            Self::Public => "*",
            Self::Cookie(_) => "Authorization, Content-Type, X-CSRF-Token",
            Self::Bearer(_) => "Authorization, Content-Type",
        };
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static(allowed_headers),
        );
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
    let method = unique(request, header::ACCESS_CONTROL_REQUEST_METHOD)?
        .to_str()
        .ok()?;
    if !route::spec(route).allows_cors_method(method) {
        return None;
    }
    let origin = unique(request, header::ORIGIN)?;
    let same_origin = origin == public_origin;
    if !same_origin {
        let raw = origin.to_str().ok()?;
        if route == Route::Servers
            || !raw.starts_with("https://")
            || canonical_origin(raw).ok()?.as_str() != raw
        {
            return None;
        }
    }

    let requested_headers = match unique(request, header::ACCESS_CONTROL_REQUEST_HEADERS) {
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

fn unique(headers: &HeaderMap, name: header::HeaderName) -> Option<&HeaderValue> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?;
    values.next().is_none().then_some(value)
}
