//! Streaming HTTP operations shared by measurement lanes and control requests.
//! The connection owns its H3 driver; response bodies borrow that owner.
use crate::{
    Error,
    net::Http,
    quic::{Http3Client, Http3Stream, RequestLimits},
};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use graphite_meter_core::{
    discovery::Protocol, origin::canonical_origin, route::Route, wire::decode_json,
};
use http::{Method, Request};
use serde::de::DeserializeOwned;
use std::time::Duration;
use tokio::time::{Instant, timeout_at};

pub struct Transport {
    http: Http,
    origin: String,
    protocol: Protocol,
    insecure: bool,
    h3: Option<Http3Client>,
}

impl Transport {
    pub async fn connect(
        http: Http,
        origin: &str,
        protocol: Protocol,
        insecure: bool,
    ) -> Result<Self, Error> {
        let origin = canonical_origin(origin)?;
        let h3 = if protocol == Protocol::Http3 {
            Some(Http3Client::connect(&origin.parse()?, insecure, Duration::from_secs(10)).await?)
        } else {
            None
        };
        Ok(Self {
            http,
            origin,
            protocol,
            insecure,
            h3,
        })
    }

    pub async fn webtransport(
        &self,
        route: Route,
        query: &[(&str, &str)],
    ) -> Result<crate::webtransport::Session, Error> {
        crate::webtransport::Session::dial(
            &self.http,
            &self.url(route, query)?,
            self.insecure,
            Duration::from_secs(10),
        )
        .await
    }

    pub fn url(&self, route: Route, query: &[(&str, &str)]) -> Result<String, Error> {
        let mut url = url::Url::parse(&format!("{}{}", self.origin, route.path()))?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query.iter().copied());
        }
        Ok(url.into())
    }

    pub async fn receive(
        &self,
        method: Method,
        route: Route,
        query: &[(&str, &str)],
        limit: u64,
        duration: Duration,
    ) -> Result<Body<'_>, Error> {
        let target = self.url(route, query)?;
        let deadline = Instant::now()
            .checked_add(duration)
            .ok_or("request duration is too large")?;
        let inner = timeout_at(deadline, async {
            if let Some(h3) = &self.h3 {
                let mut request = Request::builder().method(method).uri(&target);
                if let Some(auth) = self.http.authorization(&target) {
                    request = request.header(http::header::AUTHORIZATION, auth);
                }
                let mut stream = h3
                    .open(
                        request.body(())?,
                        RequestLimits {
                            timeout: duration,
                            max_send_bytes: 0,
                            max_receive_bytes: limit,
                        },
                    )
                    .await?;
                stream.finish().await?;
                let response = stream.response().await?;
                self.http
                    .check_status(&target, response.status(), response.headers())?;
                Ok::<_, Error>(BodyInner::H3(Box::new(stream)))
            } else {
                Ok(BodyInner::Http(
                    self.http.request(method, &target, self.protocol).await?,
                ))
            }
        })
        .await??;
        Ok(Body {
            inner,
            deadline,
            remaining: limit,
        })
    }

    /// Stream a finite request without materializing its body. No sender counts
    /// escape this API: upload measurements must use the receiver's counters.
    pub async fn send<S>(
        &self,
        route: Route,
        query: &[(&str, &str)],
        body: S,
        length: u64,
        duration: Duration,
    ) -> Result<(), Error>
    where
        S: Stream<Item = Result<Bytes, Error>> + Send + 'static,
    {
        let target = self.url(route, query)?;
        let deadline = Instant::now()
            .checked_add(duration)
            .ok_or("request duration is too large")?;
        timeout_at(deadline, async {
            if let Some(h3) = &self.h3 {
                let mut request = Request::builder()
                    .method(Method::POST)
                    .uri(&target)
                    .header(http::header::CONTENT_TYPE, "application/octet-stream")
                    .header(http::header::CONTENT_LENGTH, length);
                if let Some(auth) = self.http.authorization(&target) {
                    request = request.header(http::header::AUTHORIZATION, auth);
                }
                let mut request = h3
                    .open(
                        request.body(())?,
                        RequestLimits {
                            timeout: duration,
                            max_send_bytes: length,
                            max_receive_bytes: 64 * 1024,
                        },
                    )
                    .await?;
                futures_util::pin_mut!(body);
                let mut sent = 0_u64;
                while let Some(chunk) = body.next().await {
                    let chunk = chunk?;
                    sent = sent
                        .checked_add(chunk.len() as u64)
                        .filter(|sent| *sent <= length)
                        .ok_or("request body exceeds content length")?;
                    request.send_data(chunk).await?;
                }
                if sent != length {
                    return Err("request body shorter than content length".into());
                }
                request.finish().await?;
                let response = request.response().await?;
                self.http
                    .check_status(&target, response.status(), response.headers())?;
                request.recv_body().await?;
            } else {
                let response = self
                    .http
                    .builder(Method::POST, &target, self.protocol)?
                    .header(http::header::CONTENT_TYPE, "application/octet-stream")
                    .header(http::header::CONTENT_LENGTH, length)
                    .body(reqwest::Body::wrap_stream(body))
                    .send()
                    .await
                    .map_err(reqwest::Error::without_url)?;
                let response = self.http.check_response(response)?;
                crate::net::bounded_body(response).await?;
            }
            Ok(())
        })
        .await?
    }

    pub async fn json<T: DeserializeOwned>(
        &self,
        method: Method,
        route: Route,
        query: &[(&str, &str)],
    ) -> Result<T, Error> {
        let mut body = self
            .receive(method, route, query, 64 * 1024, Duration::from_secs(10))
            .await?;
        let mut bytes = Vec::new();
        while let Some(chunk) = body.chunk().await? {
            bytes.extend_from_slice(&chunk);
        }
        Ok(decode_json(&bytes)?)
    }
}

enum BodyInner<'a> {
    Http(reqwest::Response),
    H3(Box<Http3Stream<'a>>),
}

pub struct Body<'a> {
    inner: BodyInner<'a>,
    deadline: Instant,
    remaining: u64,
}

impl Body<'_> {
    pub async fn chunk(&mut self) -> Result<Option<Bytes>, Error> {
        let chunk = timeout_at(self.deadline, async {
            match &mut self.inner {
                BodyInner::Http(response) => Ok(response
                    .chunk()
                    .await
                    .map_err(reqwest::Error::without_url)?),
                BodyInner::H3(stream) => stream.recv_data().await,
            }
        })
        .await??;
        if let Some(chunk) = &chunk {
            self.remaining = self
                .remaining
                .checked_sub(chunk.len() as u64)
                .ok_or("response exceeds byte limit")?;
        }
        Ok(chunk)
    }
}
