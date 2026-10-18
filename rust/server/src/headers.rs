//! Header names the http crate lacks, built at compile time so that no request parses them.

use http::HeaderName;

pub const X_REAL_IP: HeaderName = HeaderName::from_static("x-real-ip");
pub const X_FORWARDED_FOR: HeaderName = HeaderName::from_static("x-forwarded-for");
pub const X_FORWARDED_PROTO: HeaderName = HeaderName::from_static("x-forwarded-proto");
pub const X_FORWARDED_HOST: HeaderName = HeaderName::from_static("x-forwarded-host");
pub const SEC_FETCH_SITE: HeaderName = HeaderName::from_static("sec-fetch-site");
pub const SEC_FETCH_MODE: HeaderName = HeaderName::from_static("sec-fetch-mode");
pub const SEC_FETCH_DEST: HeaderName = HeaderName::from_static("sec-fetch-dest");
pub const X_CSRF_TOKEN: HeaderName = HeaderName::from_static("x-csrf-token");
pub const TIMING_ALLOW_ORIGIN: HeaderName = HeaderName::from_static("timing-allow-origin");
pub const PERMISSIONS_POLICY: HeaderName = HeaderName::from_static("permissions-policy");
pub const X_ROBOTS_TAG: HeaderName = HeaderName::from_static("x-robots-tag");
pub const X_ACCEL_BUFFERING: HeaderName = HeaderName::from_static("x-accel-buffering");
pub const GRAPHITE_METER_BROWSER_AUTH: HeaderName =
    HeaderName::from_static("graphite-meter-browser-auth");
pub const GRAPHITE_METER_AUTH_URL: HeaderName = HeaderName::from_static("graphite-meter-auth-url");
