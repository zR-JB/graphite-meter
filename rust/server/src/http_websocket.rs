//! Upgrade ownership stays with the accepting HTTP/1 connection task.

use super::*;
use crate::websocket::{self, CloseReason};

pub(super) struct Upgrade {
    handshake: hyper::upgrade::OnUpgrade,
    deadline: tokio::time::Instant,
    _permit: Permit,
    lease: Option<AuthLease>,
}

impl Upgrade {
    pub(super) async fn run(self) {
        let stream = tokio::select! {
            biased;
            _ = lease_ended(self.lease.clone()) => return,
            result = tokio::time::timeout_at(self.deadline, self.handshake) => {
                match result {
                    Ok(Ok(stream)) => stream,
                    _ => return,
                }
            },
        };
        websocket::serve_ping(TokioIo::new(stream), async {
            tokio::select! {
                biased;
                _ = lease_ended(self.lease) => CloseReason::AuthenticationRequired,
                _ = tokio::time::sleep_until(self.deadline) => CloseReason::Finished,
            }
        })
        .await;
    }
}

impl HttpServer {
    pub(super) fn upgrade_websocket<B>(
        &self,
        mut request: Request<B>,
        owner: &Owner,
        lease: Option<AuthLease>,
        pending: &Mutex<Option<Upgrade>>,
    ) -> Response<ResponseBody> {
        if let Some(response) = self.validate_request(&request) {
            return response;
        }
        let permit = match self.admission.acquire(Class::Request, owner.budget_key()) {
            Ok(permit) => permit,
            Err(refusal) => {
                let mut response =
                    text_response(StatusCode::from_u16(refusal.status()).expect("known status"));
                response
                    .headers_mut()
                    .insert(header::RETRY_AFTER, http::HeaderValue::from_static("1"));
                return response;
            }
        };
        let approved_origin = lease.as_ref().and_then(|lease| {
            lease.browser_origin().or_else(|| {
                (!lease.is_bearer()).then(|| {
                    self.auth
                        .as_ref()
                        .expect("cookie auth enabled")
                        .policy()
                        .public_origin()
                })
            })
        });
        let response = websocket::handshake(&request, approved_origin);
        if response.status() == StatusCode::SWITCHING_PROTOCOLS {
            *pending.lock().expect("WebSocket upgrade poisoned") = Some(Upgrade {
                handshake: hyper::upgrade::on(&mut request),
                deadline: tokio::time::Instant::now() + self.config.max_operation_duration,
                _permit: permit,
                lease,
            });
        }
        response.map(|()| ResponseBody::empty())
    }
}
