//! One operation owns publication; replacements start only after its task joins.
use crate::{
    Error,
    config::Config,
    failure::{Failure, sign_in},
    model::{AuthPrompt, Phase, Snapshot},
    net::Http,
    runner,
    ui::{self, Command},
};
use std::{collections::HashSet, process::Stdio, time::Duration};
use tokio::{
    process::Child,
    sync::{mpsc, watch},
    task::JoinSet,
    time::Instant,
};

const CANCEL_GRACE: Duration = Duration::from_secs(5);
const SIGN_IN: &str = "Sign-in required; run graphite-meter-client in a terminal to sign in.";
const SIGN_IN_EXPIRED: &str = "Sign-in expired. Press v to request a new code.";
#[derive(Clone)]
enum Work {
    Run(Config),
    Verify(Config),
}
impl Work {
    fn config(&self) -> &Config {
        match self {
            Self::Run(c) | Self::Verify(c) => c,
        }
    }
}

pub async fn run(config: Config, interrupts: mpsc::Receiver<()>) -> Result<(Option<Snapshot>, ui::Exit), Error> {
    let (snapshots, receiver) = watch::channel(Snapshot::default());
    let (commands, mut incoming) = mpsc::channel(8);
    let mut controller = Controller::new(&config, snapshots, true)?;
    controller.launch(Work::Verify(config.clone()))?;
    let result = {
        let terminal = ui::run(config, receiver, commands, interrupts);
        tokio::pin!(terminal);
        loop {
            tokio::select! {
                result = &mut terminal => break result,
                Some(command) = incoming.recv() => {
                    let replaced = match command {
                        Command::Run(config) => controller.replace(Work::Run(config)),
                        Command::Verify(config) => controller.replace(Work::Verify(config)),
                        Command::Cancel => {
                            controller.cancel();
                            Ok(())
                        }
                        Command::OpenBrowser => {
                            controller.open_browser();
                            Ok(())
                        }
                    };
                    if let Err(error) = replaced {
                        break Err(error);
                    }
                }
                Some(result) = controller.operations.join_next() => {
                    if let Err(error) = controller.finished(result) {
                        break Err(error);
                    }
                }
                _ = deadline(controller.cancel_deadline) => {
                    controller.operations.abort_all();
                    controller.cancel_deadline = None;
                }
                _ = deadline(controller.browser_deadline) => {
                    controller.close_browser().await;
                }
            }
        }
    }; // Drop the UI and restore the terminal before shutdown/reporting.
    controller.stop().await;
    Ok((controller.finished, result?))
}

pub async fn run_once(config: Config, mut interrupts: mpsc::Receiver<()>) -> Result<Snapshot, Error> {
    let mut controller = Controller::new(&config, watch::channel(Snapshot::default()).0, false)?;
    controller.launch(Work::Run(config))?;
    let mut events = controller.snapshots.subscribe();
    let result = {
        let finished = controller.finish();
        tokio::pin!(finished);
        let mut stage = None;
        loop {
            tokio::select! {
                result = &mut finished => break Some(result),
                Some(()) = interrupts.recv() => break None,
                result = events.changed() => {
                    if result.is_err() { continue; }
                    let snapshot = events.borrow_and_update();
                    if snapshot.phase == Phase::Measuring && stage != snapshot.stage {
                        stage = snapshot.stage;
                        if let Some(stage) = stage { eprintln!("{}…", stage.name()); }
                    }
                }
            }
        }
    };
    if let Some(result) = result {
        result
    } else {
        controller.cancel();
        controller.finish().await
    }
}

struct Controller {
    snapshots: watch::Sender<Snapshot>,
    operations: JoinSet<Result<Option<runner::PreparedRun>, Error>>,
    prepared: Option<runner::PreparedRun>,
    cancel: Option<watch::Sender<bool>>,
    pending: Option<Work>,
    running: bool,
    /// A run replaced the path check that is ending: that check's end is the run's preparation.
    replaced_check: bool,
    finished: Option<Snapshot>,
    cancelling: bool,
    cancel_deadline: Option<Instant>,
    started: Instant,
    http: Http,
    config: Config,
    interactive: bool,
    browser: Option<Child>,
    browser_deadline: Option<Instant>,
}
impl Controller {
    async fn finish(&mut self) -> Result<Snapshot, Error> {
        self.wait().await?;
        Ok(self.snapshots.borrow().clone())
    }

    async fn wait(&mut self) -> Result<(), Error> {
        while !self.operations.is_empty() {
            tokio::select! {
                Some(result) = self.operations.join_next() => self.finished(result)?,
                _ = deadline(self.cancel_deadline) => { self.operations.abort_all(); self.cancel_deadline = None; },
            }
        }
        Ok(())
    }

    fn new(config: &Config, snapshots: watch::Sender<Snapshot>, interactive: bool) -> Result<Self, Error> {
        Ok(Self {
            snapshots,
            operations: JoinSet::new(),
            prepared: None,
            cancel: None,
            pending: None,
            running: false,
            replaced_check: false,
            finished: None,
            cancelling: false,
            cancel_deadline: None,
            started: Instant::now(),
            http: Http::new(config.insecure)?,
            config: config.clone(),
            interactive,
            browser: None,
            browser_deadline: None,
        })
    }
    fn launch(&mut self, work: Work) -> Result<(), Error> {
        assert!(self.operations.is_empty());
        if self.config.insecure != work.config().insecure {
            self.http = Http::new(work.config().insecure)?;
        }
        self.config = work.config().clone();
        self.running = matches!(work, Work::Run(_));
        self.started = Instant::now();
        if !self.running {
            self.prepared = None;
        }
        let servers = if self
            .prepared
            .as_ref()
            .is_some_and(|prepared| prepared.fresh_for(work.config()))
        {
            self.snapshots.borrow().servers.clone()
        } else {
            Vec::new()
        };
        self.snapshots.send_replace(Snapshot {
            servers,
            phase: if self.running {
                Phase::Preparing
            } else {
                Phase::Checking
            },
            ..Snapshot::default()
        });
        let (cancel, cancelled) = watch::channel(false);
        self.cancel = Some(cancel);
        self.cancelling = false;
        self.cancel_deadline = None;
        // Grants carry over; connections belong to this check or run alone.
        let http = self.http.fresh();
        let snapshots = self.snapshots.clone();
        let prepared = self.prepared.take();
        let interactive = self.interactive;
        self.operations
            .spawn(async move { execute(work, http, snapshots, cancelled, prepared, interactive).await });
        Ok(())
    }
    fn replace(&mut self, work: Work) -> Result<(), Error> {
        if self.operations.is_empty() {
            self.launch(work)
        } else {
            if matches!(work, Work::Run(_)) && !self.running {
                // As in Go, the run starts at once: stopping the replaced check is its
                // preparation, and stopping the run before launch reads as stopped.
                self.running = true;
                self.replaced_check = true;
                self.started = Instant::now();
                self.snapshots.send_replace(Snapshot {
                    phase: Phase::Preparing,
                    ..Snapshot::default()
                });
            }
            self.pending = Some(work);
            self.request_cancel();
            Ok(())
        }
    }
    fn cancel(&mut self) {
        self.pending = None;
        self.request_cancel();
    }
    fn request_cancel(&mut self) {
        if let Some(cancel) = &self.cancel {
            let _ = cancel.send(true);
        }
        if !self.operations.is_empty() {
            self.cancelling = true;
            self.cancel_deadline.get_or_insert(Instant::now() + CANCEL_GRACE);
            self.snapshots.send_modify(|snapshot| snapshot.auth = None);
        }
    }
    fn finished(
        &mut self,
        result: Result<Result<Option<runner::PreparedRun>, Error>, tokio::task::JoinError>,
    ) -> Result<(), Error> {
        self.cancel = None;
        self.cancel_deadline = None;
        let running = self.running;
        // Unless it was stopped before launch, the run that replaced the check goes on.
        let preparing = std::mem::take(&mut self.replaced_check) && self.pending.is_some();
        if self.cancelling {
            self.snapshots.send_modify(|snapshot| {
                if snapshot.phase.busy() && !preparing {
                    snapshot.phase = if running { Phase::Cancelled } else { Phase::Setup };
                    snapshot.error = None;
                }
                snapshot.auth = None;
            });
        } else {
            match result.map_err(Error::from).and_then(|result| result) {
                Ok(prepared) => self.prepared = prepared,
                Err(error) => {
                    let signed_out = sign_in(error.as_ref()).is_some();
                    let expired = matches!(error.downcast_ref(), Some(Failure::ApprovalExpired));
                    self.snapshots.send_modify(|snapshot| {
                        snapshot.phase = if snapshot.measured() {
                            Phase::Incomplete
                        } else {
                            Phase::Failed
                        };
                        let text = crate::failure::text(error.as_ref());
                        snapshot.error = Some(if expired {
                            SIGN_IN_EXPIRED.into()
                        } else if !running || snapshot.started() {
                            text
                        } else if signed_out {
                            SIGN_IN.into()
                        } else {
                            format!("Test could not start: {text}")
                        });
                        snapshot.auth = None;
                    });
                    if running && signed_out && self.interactive {
                        self.pending.get_or_insert_with(|| Work::Verify(self.config.clone()));
                    }
                }
            }
        }
        self.cancelling = false;
        if running && !preparing {
            let duration = self.started.elapsed();
            self.snapshots.send_modify(|snapshot| snapshot.duration = duration);
            self.finished = Some(self.snapshots.borrow().clone());
        }
        if let Some(work) = self.pending.take() {
            self.launch(work)?;
        }
        Ok(())
    }
    fn open_browser(&mut self) {
        // Go opens the page on every press; only a launcher that is still starting is waited for.
        if self
            .browser
            .as_mut()
            .is_some_and(|child| !matches!(child.try_wait(), Ok(Some(_))))
        {
            return;
        }
        let Some(prompt) = self.snapshots.borrow().auth.clone() else {
            return;
        };
        // The URL came from validated PKCE setup, never a shell command.
        if let Ok(child) = browser(&prompt.browser_url) {
            self.browser = Some(child);
            self.browser_deadline = Some(Instant::now() + Duration::from_secs(5));
        }
    }
    async fn close_browser(&mut self) {
        self.browser_deadline = None;
        if let Some(mut child) = self.browser.take() {
            if !matches!(child.try_wait(), Ok(Some(_))) {
                let _ = child.kill().await;
            }
            let _ = child.wait().await;
        }
    }
    async fn stop(&mut self) {
        self.cancel();
        let _ = self.wait().await;
        self.close_browser().await;
    }
}

async fn execute(
    work: Work,
    http: Http,
    snapshots: watch::Sender<Snapshot>,
    mut cancel: watch::Receiver<bool>,
    mut prepared: Option<runner::PreparedRun>,
    interactive: bool,
) -> Result<Option<runner::PreparedRun>, Error> {
    let mut approvals = HashSet::new();
    loop {
        if *cancel.borrow() {
            return Ok(None);
        }
        let result = match &work {
            Work::Run(config) => runner::run(
                config.clone(),
                http.clone(),
                snapshots.clone(),
                cancel.clone(),
                prepared.take(),
            )
            .await
            .map(|()| None),
            Work::Verify(config) => tokio::select! {
                biased;
                _ = cancel.wait_for(|cancelled| *cancelled) => return Ok(None),
                result = runner::prepare_run(config, &http, &snapshots) => result.map(|prepared| {
                    snapshots.send_modify(|snapshot| snapshot.phase = Phase::Setup);
                    Some(prepared)
                }),
            },
        };
        let error = match result {
            Ok(prepared) => return Ok(prepared),
            Err(error) => error,
        };
        if snapshots.borrow().measured() && matches!(work, Work::Run(_)) {
            return Err(error);
        }
        // Without a login page the run ends, and the check that follows finds it (finished).
        let Some((origin, login)) = sign_in(error.as_ref()).filter(|(_, login)| interactive && !login.is_empty())
        else {
            return Err(error);
        };
        let (origin, login) = (origin.to_owned(), login.to_owned());
        if !approvals.insert(origin.clone()) {
            return Err("server rejected the approved credential; verify its authentication configuration".into());
        }
        let pending = http.begin_authorization(&origin, &login)?;
        snapshots.send_modify(|snapshot| {
            if matches!(work, Work::Run(_)) {
                snapshot.phase = Phase::Preparing;
            }
            snapshot.error = None;
            snapshot.auth = Some(AuthPrompt {
                deadline: pending.deadline,
                origin: origin.clone(),
                browser_url: pending.browser_url.clone(),
                code: pending.code.clone(),
            });
        });
        tokio::select! {
            biased;
            _ = cancel.wait_for(|cancelled| *cancelled) => return Ok(None),
            result = http.poll_authorization(pending) => result?,
        }
        snapshots.send_modify(|snapshot| snapshot.auth = None);
    }
}
async fn deadline(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}
fn browser(url: &str) -> Result<Child, Error> {
    if graphite_meter_core::origin::split_url(url)?.0.scheme != "https" {
        return Err("approval browser URL must use HTTPS".into());
    }
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = tokio::process::Command::new("rundll32.exe");
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = tokio::process::Command::new("open");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let mut command = tokio::process::Command::new("xdg-open");
    Ok(command
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shutdown_joins_operation_before_returning_its_partial_result() {
        let _ = crate::crypto::provider().install_default();
        for phase in [Phase::Measuring, Phase::Cancelled, Phase::Complete] {
            let (snapshots, _) = watch::channel(Snapshot {
                phase,
                ..Snapshot::default()
            });
            let mut controller = Controller::new(&Config::default(), snapshots.clone(), true).unwrap();
            controller.running = true;
            let (cancel, mut cancelled_signal) = watch::channel(false);
            controller.cancel = Some(cancel);
            let (joined, completed) = tokio::sync::oneshot::channel();
            controller.operations.spawn(async move {
                let _ = cancelled_signal.wait_for(|cancelled| *cancelled).await;
                snapshots.send_modify(|snapshot| snapshot.latest.up_bps = Some(42.0));
                let _ = joined.send(());
                Ok(None)
            });
            controller.stop().await;
            completed.await.unwrap();
            assert!(controller.operations.is_empty());
            let expected = if phase == Phase::Complete {
                Phase::Complete
            } else {
                Phase::Cancelled
            };
            assert_eq!(controller.snapshots.borrow().phase, expected);
            assert_eq!(controller.snapshots.borrow().latest.up_bps, Some(42.0));
        }
    }

    /// A controller whose path check runs until it is cancelled.
    fn checking() -> Controller {
        let _ = crate::crypto::provider().install_default();
        let (snapshots, _) = watch::channel(Snapshot {
            phase: Phase::Checking,
            ..Snapshot::default()
        });
        let mut controller = Controller::new(&Config::default(), snapshots, true).unwrap();
        let (cancel, mut cancelled) = watch::channel(false);
        controller.cancel = Some(cancel);
        controller.operations.spawn(async move {
            let _ = cancelled.wait_for(|cancelled| *cancelled).await;
            Ok(None)
        });
        controller
    }

    #[tokio::test]
    async fn a_run_replacing_a_path_check_starts_at_once() {
        let mut controller = checking();
        controller.replace(Work::Run(Config::default())).unwrap();
        assert_eq!(controller.snapshots.borrow().phase, Phase::Preparing);
        // The check ending is the run's preparation, not a stopped run.
        let check = controller.operations.join_next().await.unwrap();
        controller.finished(check).unwrap();
        assert!(controller.finished.is_none());
        assert_eq!(controller.operations.len(), 1, "the run launched");
        controller.stop().await;

        let mut controller = checking();
        controller.replace(Work::Run(Config::default())).unwrap();
        controller.cancel();
        controller.wait().await.unwrap();
        assert_eq!(controller.snapshots.borrow().phase, Phase::Cancelled);
        assert_eq!(
            controller.finished.as_ref().map(|run| run.phase),
            Some(Phase::Cancelled)
        );
    }

    #[tokio::test]
    async fn a_sign_in_that_expires_after_measuring_checks_the_servers_again() {
        let _ = crate::crypto::provider().install_default();
        let config = Config {
            url: "http://127.0.0.1:1".into(),
            ..Config::default()
        };
        let (snapshots, _) = watch::channel(Snapshot::default());
        let mut controller = Controller::new(&config, snapshots.clone(), true).unwrap();
        controller.running = true;
        controller.operations.spawn(async move {
            snapshots.send_modify(|snapshot| {
                snapshot.results.push(crate::model::StageResult {
                    elapsed: Duration::from_secs(1),
                    ..Default::default()
                })
            });
            Err(Box::new(Failure::SignIn {
                origin: "https://meter.test".into(),
                login_url: "https://meter.test/login".into(),
            }) as Error)
        });
        let result = controller.operations.join_next().await.unwrap();
        controller.finished(result).unwrap();
        assert_eq!(
            controller.finished.as_ref().map(|run| run.phase),
            Some(Phase::Incomplete)
        );
        assert_eq!(controller.snapshots.borrow().phase, Phase::Checking);
        controller.stop().await;
    }

    /// Sign-in asked without a login page, as both servers end a revoked upload lane
    /// (go/internal/endpoint/upload.go:65-66), ends the run and checks the servers again, as Go's
    /// client prepares again (run.go:275-278, 306-310); no empty login page is opened.
    #[tokio::test]
    async fn a_sign_in_without_a_login_page_checks_the_servers_again() -> Result<(), Error> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let _ = crate::crypto::provider().install_default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let config = Config {
            url: format!("http://{}", listener.local_addr()?),
            ..Config::default()
        };
        let server = tokio::spawn(async move {
            for _ in 0..8 {
                let (mut stream, _) = listener.accept().await?;
                let mut request = [0_u8; 4096];
                let count = stream.read(&mut request).await?;
                let answer = if request[..count].starts_with(b"GET /servers ") {
                    let body = r#"{"defaultSelection":["self"],"servers":[{"id":"self","url":".","name":"self"}]}"#;
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}", body.len())
                } else {
                    "HTTP/1.1 403 Forbidden\r\nGraphite-Meter-Auth: required\r\nContent-Length: 0\r\n\r\n".into()
                };
                stream.write_all(answer.as_bytes()).await?;
            }
            Ok::<_, Error>(())
        });
        let (snapshots, _) = watch::channel(Snapshot::default());
        let mut controller = Controller::new(&config, snapshots, true)?;
        controller.launch(Work::Run(config))?;
        let result = controller.operations.join_next().await.ok_or("no run")?;
        controller.finished(result)?;
        let phase = controller.snapshots.borrow().phase;
        controller.stop().await;
        server.abort();
        assert_eq!(phase, Phase::Checking);
        Ok(())
    }

    #[tokio::test]
    async fn an_expired_approval_asks_for_a_new_code_like_go() {
        let _ = crate::crypto::provider().install_default();
        for running in [false, true] {
            let (snapshots, _) = watch::channel(Snapshot::default());
            let mut controller = Controller::new(&Config::default(), snapshots, true).unwrap();
            controller.running = running;
            controller
                .operations
                .spawn(async { Err(Box::new(Failure::ApprovalExpired) as Error) });
            let result = controller.operations.join_next().await.unwrap();
            controller.finished(result).unwrap();
            assert_eq!(controller.snapshots.borrow().error.as_deref(), Some(SIGN_IN_EXPIRED));
        }
    }

    /// Each check takes connections of its own: one an earlier check left idle, silent since as
    /// after a sleep or a network change, is never reused.
    #[tokio::test]
    async fn each_check_takes_connections_of_its_own() -> Result<(), Error> {
        use http_body_util::Full;
        use hyper::{body::Bytes, service::service_fn};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let _ = crate::crypto::provider().install_default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let epoch = Arc::new(AtomicUsize::new(0));
        let now = epoch.clone();
        let server = tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                // A connection from an earlier epoch accepts requests but never answers them.
                let (now, born) = (now.clone(), now.load(Ordering::SeqCst));
                let service = service_fn(move |request: http::Request<hyper::body::Incoming>| {
                    let silent = now.load(Ordering::SeqCst) != born;
                    async move {
                        if silent {
                            std::future::pending::<()>().await;
                        }
                        let body = match request.uri().path() {
                            "/servers" => serde_json::json!({
                                "defaultSelection": ["self"],
                                "servers": [{"id": "self", "url": ".", "name": "fixture"}]
                            }),
                            "/preflight" => serde_json::json!({
                                "generation": "fixture",
                                "capabilities": {
                                    "throughput": [{"baseUrl": ".", "transport": "fetch-stream", "protocol": "http1"}],
                                    "latency": []
                                }
                            }),
                            _ => serde_json::json!({
                                "clientIp": "127.0.0.1", "clientIpVersion": 4, "clientIpSource": "socket",
                                "protocolNegotiated": "http/1.1"
                            }),
                        };
                        Ok::<_, std::convert::Infallible>(http::Response::new(Full::new(Bytes::from(body.to_string()))))
                    }
                });
                tokio::spawn(
                    hyper::server::conn::http1::Builder::new()
                        .serve_connection(hyper_util::rt::TokioIo::new(socket), service),
                );
            }
        });
        let config = Config {
            url: origin,
            stages: vec![crate::model::Stage::Download],
            loaded_latency: false,
            ..Config::default()
        };
        let mut controller = Controller::new(&config, watch::channel(Snapshot::default()).0, false)?;
        let mut checks = Vec::new();
        for _ in 0..2 {
            controller.replace(Work::Verify(config.clone()))?;
            checks.push(tokio::time::timeout(Duration::from_secs(5), controller.wait()).await);
            epoch.fetch_add(1, Ordering::SeqCst);
        }
        server.abort();
        for check in checks {
            check.map_err(|_| "a check reused a connection an earlier check left idle")??;
        }
        assert_eq!(controller.snapshots.borrow().phase, Phase::Setup);
        Ok(())
    }

    #[tokio::test]
    async fn controller_reuses_fresh_paths_and_reprepares_changed_settings() -> Result<(), Error> {
        use crate::runner::prepare_tests::{FixtureMode, request_path, serve};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let _ = crate::crypto::provider().install_default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let catalogs = Arc::new(AtomicUsize::new(0));
        let count = catalogs.clone();
        let server_origin = origin.clone();
        let server = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let origin = server_origin.clone();
                let count = count.clone();
                tokio::spawn(async move {
                    if request_path(&stream).await.ok().flatten().as_deref() == Some("/servers") {
                        count.fetch_add(1, Ordering::SeqCst);
                    }
                    let _ = serve(stream, origin, FixtureMode::Negotiated).await;
                });
            }
        });
        let mut config = Config {
            url: origin,
            stages: vec![crate::model::Stage::Download],
            loaded_latency: false,
            warmup: Duration::ZERO,
            download_duration: Duration::from_secs(1),
            streams: 1,
            ..Config::default()
        };
        let mut controller = Controller::new(&config, watch::channel(Snapshot::default()).0, false)?;
        controller.replace(Work::Verify(config.clone()))?;
        controller.wait().await?;
        assert_eq!(catalogs.load(Ordering::SeqCst), 1);
        config.download_duration = Duration::from_secs(2);
        controller.replace(Work::Run(config.clone()))?;
        assert_eq!(controller.finish().await?.phase, Phase::Failed);
        assert_eq!(controller.snapshots.borrow().servers[0].name, "fixture");
        assert_eq!(catalogs.load(Ordering::SeqCst), 1);
        controller.replace(Work::Verify(config.clone()))?;
        controller.wait().await?;
        controller
            .prepared
            .as_mut()
            .ok_or("paths were not verified")?
            .verified_at -= Duration::from_secs(31);
        controller.replace(Work::Run(config.clone()))?;
        controller.finish().await?;
        assert_eq!(catalogs.load(Ordering::SeqCst), 3);
        controller.replace(Work::Verify(config.clone()))?;
        controller.wait().await?;
        config.throughput_protocol = Some(graphite_meter_core::discovery::Protocol::Http1);
        controller.replace(Work::Run(config))?;
        controller.finish().await?;
        assert_eq!(catalogs.load(Ordering::SeqCst), 5);
        server.abort();
        Ok(())
    }
}
