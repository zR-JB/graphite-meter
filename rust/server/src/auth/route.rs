//! The authentication controller's routes, as Go's `Service.Mount` registers them.
use crate::config::AuthMode;
use http::Method;

/// Go mounts `/login` and this subtree for the controller; a path in it that no route claims is not found.
const SUBTREE: &str = "/auth/";

/// Who may reach a route without a session.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Public {
    Never,
    Always,
    /// In a mode that signs in with a password.
    Password,
    /// In a mode that signs in with the OIDC provider.
    Oidc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthRoute {
    Login,
    Password,
    OidcStart,
    OidcCallback,
    Session,
    Logout,
    CliPage,
    CliApprove,
    CliToken,
    BrowserPage,
    BrowserApprove,
    BrowserToken,
}

impl AuthRoute {
    pub const ALL: [Self; 12] = [
        Self::Login,
        Self::Password,
        Self::OidcStart,
        Self::OidcCallback,
        Self::Session,
        Self::Logout,
        Self::CliPage,
        Self::CliApprove,
        Self::CliToken,
        Self::BrowserPage,
        Self::BrowserApprove,
        Self::BrowserToken,
    ];

    const fn row(self) -> (&'static str, &'static str, Public) {
        match self {
            Self::Login => ("GET", "/login", Public::Always),
            Self::Password => ("POST", "/auth/password", Public::Password),
            Self::OidcStart => ("POST", "/auth/oidc/start", Public::Oidc),
            Self::OidcCallback => ("GET", "/auth/oidc/callback", Public::Oidc),
            Self::Session => ("GET", "/auth/session", Public::Never),
            Self::Logout => ("POST", "/auth/logout", Public::Never),
            Self::CliPage => ("GET", "/auth/cli", Public::Always),
            Self::CliApprove => ("POST", "/auth/cli/approve", Public::Never),
            Self::CliToken => ("POST", "/auth/cli/token", Public::Always),
            Self::BrowserPage => ("GET", "/auth/browser", Public::Always),
            Self::BrowserApprove => ("POST", "/auth/browser/approve", Public::Never),
            Self::BrowserToken => ("POST", "/auth/browser/token", Public::Always),
        }
    }

    pub const fn method(self) -> &'static str {
        self.row().0
    }

    pub const fn path(self) -> &'static str {
        self.row().1
    }

    /// Whether a request reaches the route without a session under `mode`, as Go's `isPublicAuthRoute`.
    pub fn public(self, mode: AuthMode) -> bool {
        match self.row().2 {
            Public::Never => false,
            Public::Always => true,
            Public::Password => mode.password(),
            Public::Oidc => mode.oidc(),
        }
    }

    /// The route that `method` and the entire `path` name, as Go's method patterns match them.
    pub fn lookup(method: &Method, path: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|route| route.path() == path && route.method() == method.as_str())
    }
}

/// Whether `path` belongs to the controller: `/login` and everything under `/auth/`.
pub fn claims(path: &str) -> bool {
    path == AuthRoute::Login.path() || path.starts_with(SUBTREE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_match_their_method_and_whole_path_and_stay_in_the_subtree() {
        for route in AuthRoute::ALL {
            let method = Method::from_bytes(route.method().as_bytes()).unwrap();
            assert_eq!(AuthRoute::lookup(&method, route.path()), Some(route));
            assert!(claims(route.path()));
            let other = if method == Method::GET {
                Method::POST
            } else {
                Method::GET
            };
            assert_eq!(AuthRoute::lookup(&other, route.path()), None);
            assert_eq!(AuthRoute::lookup(&method, &format!("{}/", route.path())), None);
        }
        assert!(claims("/auth/unknown") && !claims("/login/") && !claims("/auth"));
        let public = |mode| {
            AuthRoute::ALL
                .into_iter()
                .filter(|route| route.public(mode))
                .map(AuthRoute::path)
                .collect::<Vec<_>>()
        };
        let always = [
            "/login",
            "/auth/cli",
            "/auth/cli/token",
            "/auth/browser",
            "/auth/browser/token",
        ];
        assert_eq!(public(AuthMode::Off), always);
        assert_eq!(
            public(AuthMode::Password),
            [
                "/login",
                "/auth/password",
                "/auth/cli",
                "/auth/cli/token",
                "/auth/browser",
                "/auth/browser/token"
            ]
        );
        assert_eq!(
            public(AuthMode::Hybrid),
            [
                "/login",
                "/auth/password",
                "/auth/oidc/start",
                "/auth/oidc/callback",
                "/auth/cli",
                "/auth/cli/token",
                "/auth/browser",
                "/auth/browser/token"
            ]
        );
    }
}
