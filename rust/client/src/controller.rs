//! One operation owns publication; replacements start only after its task joins.
use crate::{
    Error,
    config::Config,
    model::{AuthPrompt, Phase, Snapshot},
    net::{Http, authentication_required},
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
    let mut controller = Controller::with_snapshots(&config, snapshots, true)?;
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
    let mut controller = Controller::new(&config)?;
    let mut events = controller.start(config, None)?;
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

pub struct Controller {
    snapshots: watch::Sender<Snapshot>,
    operations: JoinSet<Result<Option<runner::PreparedRun>, Error>>,
    prepared: Option<runner::PreparedRun>,
    cancel: Option<watch::Sender<bool>>,
    pending: Option<Work>,
    running: bool,
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
    pub fn new(config: &Config) -> Result<Self, Error> {
        let (snapshots, _) = watch::channel(Snapshot::default());
        Self::with_snapshots(config, snapshots, false)
    }

    pub fn events(&self) -> watch::Receiver<Snapshot> {
        self.snapshots.subscribe()
    }

    pub async fn prepare(&mut self, config: Config) -> Result<runner::PreparedRun, Error> {
        self.replace(Work::Verify(config))?;
        self.wait().await?;
        self.prepared.take().ok_or_else(|| {
            self.snapshots
                .borrow()
                .error
                .clone()
                .unwrap_or_else(|| "preparation stopped".into())
                .into()
        })
    }

    pub fn authorize(&self, origin: &str, login_url: &str) -> Result<crate::net::PendingAuthorization, Error> {
        self.http.begin_authorization(origin, login_url)
    }

    pub async fn poll_authorization(&self, pending: crate::net::PendingAuthorization) -> Result<(), Error> {
        self.http.poll_authorization(pending).await
    }

    pub fn start(
        &mut self,
        config: Config,
        prepared: Option<runner::PreparedRun>,
    ) -> Result<watch::Receiver<Snapshot>, Error> {
        self.prepared = prepared;
        self.replace(Work::Run(config))?;
        Ok(self.events())
    }

    pub async fn finish(&mut self) -> Result<Snapshot, Error> {
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

    fn with_snapshots(config: &Config, snapshots: watch::Sender<Snapshot>, interactive: bool) -> Result<Self, Error> {
        Ok(Self {
            snapshots,
            operations: JoinSet::new(),
            prepared: None,
            cancel: None,
            pending: None,
            running: false,
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
            status: "Preparing selected servers".into(),
            ..Snapshot::default()
        });
        let (cancel, cancelled) = watch::channel(false);
        self.cancel = Some(cancel);
        self.cancelling = false;
        self.cancel_deadline = None;
        let http = self.http.clone();
        let snapshots = self.snapshots.clone();
        let prepared = if matches!(work, Work::Run(_)) {
            self.prepared.take()
        } else {
            None
        };
        let interactive = self.interactive;
        self.operations
            .spawn(async move { execute(work, http, snapshots, cancelled, prepared, interactive).await });
        Ok(())
    }
    fn replace(&mut self, work: Work) -> Result<(), Error> {
        if self.operations.is_empty() {
            self.launch(work)
        } else {
            self.pending = Some(work);
            self.request_cancel();
            Ok(())
        }
    }
    pub fn cancel(&mut self) {
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
            let running = self.running;
            self.snapshots.send_modify(|snapshot| {
                if running && matches!(snapshot.phase, Phase::Preparing | Phase::Warmup | Phase::Measuring) {
                    snapshot.status = "Stopping the test…".into();
                }
                snapshot.auth = None;
            });
        }
    }
    fn finished(
        &mut self,
        result: Result<Result<Option<runner::PreparedRun>, Error>, tokio::task::JoinError>,
    ) -> Result<(), Error> {
        self.cancel = None;
        self.cancel_deadline = None;
        let running = self.running;
        if self.cancelling {
            self.snapshots.send_modify(|snapshot| {
                if matches!(
                    snapshot.phase,
                    Phase::Checking | Phase::Preparing | Phase::Warmup | Phase::Measuring
                ) {
                    (snapshot.phase, snapshot.status) = if running {
                        (Phase::Cancelled, "Stopped".into())
                    } else {
                        (Phase::Setup, "Recheck needed".into())
                    };
                    snapshot.error = None;
                }
                snapshot.auth = None;
            });
        } else {
            let result = result
                .map_err(|error| Box::new(error) as Error)
                .and_then(|result| result);
            match result {
                Ok(prepared) => self.prepared = prepared,
                Err(error) => {
                    let signed_out = authentication_required(error.as_ref()).is_some();
                    self.snapshots.send_modify(|snapshot| {
                        (snapshot.phase, snapshot.status) =
                            if snapshot.results.iter().any(|result| result.elapsed > Duration::ZERO) {
                                (Phase::Incomplete, "Incomplete".into())
                            } else {
                                (Phase::Failed, "Failed".into())
                            };
                        let text = crate::failure::text(error.as_ref());
                        let started = !snapshot.participants.is_empty() || !snapshot.results.is_empty();
                        snapshot.error = Some(if !running || started {
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
        if running {
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
        if self.browser.is_some() {
            return;
        }
        let Some(prompt) = self.snapshots.borrow().auth.clone() else {
            return;
        };
        // The URL came from validated PKCE setup, never a shell command.
        match browser(&prompt.browser_url) {
            Ok(child) => {
                self.browser = Some(child);
                self.browser_deadline = Some(Instant::now() + Duration::from_secs(5));
            }
            Err(error) => self.snapshots.send_modify(|snapshot| {
                snapshot.status = format!("Could not open browser: {error}; copy the displayed URL")
            }),
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
            Work::Run(config) => runner::run_prepared(
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
                _ = cancelled(&mut cancel) => return Ok(None),
                result = runner::prepare_run(config, &http, &snapshots) => result.map(|prepared| {
                    snapshots.send_modify(|snapshot| { snapshot.phase = Phase::Setup; snapshot.status = "Selected servers verified".into(); });
                    Some(prepared)
                }),
            },
        };
        let error = match result {
            Ok(prepared) => return Ok(prepared),
            Err(error) => error,
        };
        let measured = snapshots
            .borrow()
            .results
            .iter()
            .any(|result| result.elapsed > Duration::ZERO);
        if measured && matches!(work, Work::Run(_)) {
            return Err(error);
        }
        let Some(required) = authentication_required(error.as_ref()).filter(|_| interactive) else {
            return Err(error);
        };
        let origin = required.origin.clone();
        let login = required.login_url.clone();
        if !approvals.insert(origin.clone()) {
            return Err("server rejected the approved credential; verify its authentication configuration".into());
        }
        let pending = http.begin_authorization(&origin, &login)?;
        snapshots.send_modify(|snapshot| {
            if matches!(work, Work::Run(_)) {
                snapshot.phase = Phase::Preparing;
            }
            snapshot.error = None;
            snapshot.status = "Approve this client in your browser".into();
            snapshot.auth = Some(AuthPrompt {
                deadline: pending.deadline,
                origin: origin.clone(),
                browser_url: pending.browser_url.clone(),
                code: pending.code.clone(),
            });
        });
        tokio::select! {
            biased;
            _ = cancelled(&mut cancel) => return Ok(None),
            result = http.poll_authorization(pending) => result?,
        }
        snapshots.send_modify(|snapshot| {
            snapshot.auth = None;
            snapshot.status = "Approved; retrying selected operation".into();
        });
    }
}
async fn cancelled(cancel: &mut watch::Receiver<bool>) {
    loop {
        if *cancel.borrow_and_update() {
            return;
        }
        if cancel.changed().await.is_err() {
            return;
        }
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
                status: if phase == Phase::Complete {
                    "Complete"
                } else {
                    "Stopped"
                }
                .into(),
                ..Snapshot::default()
            });
            let mut controller = Controller::with_snapshots(&Config::default(), snapshots.clone(), true).unwrap();
            controller.running = true;
            let (cancel, mut cancelled_signal) = watch::channel(false);
            controller.cancel = Some(cancel);
            let (joined, completed) = tokio::sync::oneshot::channel();
            controller.operations.spawn(async move {
                cancelled(&mut cancelled_signal).await;
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
            assert_eq!(
                controller.snapshots.borrow().status,
                if phase == Phase::Complete {
                    "Complete"
                } else {
                    "Stopped"
                }
            );
            assert_eq!(controller.snapshots.borrow().latest.up_bps, Some(42.0));
        }
    }
}
