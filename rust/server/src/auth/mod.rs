//! Authentication state shared by all listeners. HTTP policy lives above this layer.
mod approval;
mod grant;
pub mod http;
mod oidc;
pub mod pages;
pub mod password_login;
pub mod policy;
pub mod rate;
mod session;
mod ticket;

pub use session::{SESSION_LIFETIME, Session, SessionError, SessionLease, SessionStore};

pub use grant::{AuthLease, GrantError, secure_browser_origin};
pub use ticket::{SocketKind, Ticket, TicketError};

pub use approval::{
    ApprovalError, ApprovalKind, ApprovalView, Exchange, ExchangeError, valid_challenge,
};
