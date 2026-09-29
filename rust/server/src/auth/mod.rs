//! Authentication state shared by all listeners. HTTP policy lives above this layer.
mod approval;
mod grant;
pub mod http;
mod jwt;
mod logging;
mod oidc;
pub mod pages;
pub mod password_login;
pub mod policy;
pub mod rate;
pub mod reason;
pub mod route;
mod session;
mod ticket;

pub use session::{SESSION_LIFETIME, Session, SessionError, SessionLease, SessionStore};

pub use grant::{AuthLease, secure_browser_origin};
pub use route::AuthRoute;
pub use ticket::{Ticket, TicketError};

pub use approval::{ApprovalError, ApprovalKind, ApprovalView, Exchange, ExchangeError, valid_challenge};
