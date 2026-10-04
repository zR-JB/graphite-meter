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
    task::JoinHandle,
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Purpose {
    Check,
    Run,
    /// The cancelled check is preparing the queued run until that run can launch.
    PreparingRun,
}

struct Operation {
    task: JoinHandle<Result<Option<runner::PreparedRun>, Error>>,
    cancel: watch::Sender<bool>,
    purpose: Purpose,
    pending: Option<Work>,
    started: Instant,
    stopping: Option<Instant>,
}

impl Operation {
    fn new<F>(purpose: Purpose, run: impl FnOnce(watch::Receiver<bool>) -> F) -> Self
    where
        F: Future<Output = Result<Option<runner::PreparedRun>, Error>> + Send + 'static,
    {
        let (cancel, cancelled) = watch::channel(false);
        Self {
            task: tokio::spawn(run(cancelled)),
            cancel,
            purpose,
            pending: None,
            started: Instant::now(),
            stopping: None,
        }
    }

    fn cancel(&mut self) {
        self.cancel.send_replace(true);
        self.stopping.get_or_insert(Instant::now() + CANCEL_GRACE);
    }
}

impl Drop for Operation {
    fn drop(&mut self) {
        self.task.abort();
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
            let handled = tokio::select! {
                result = &mut terminal => break result,
                Some(command) = incoming.recv() => match command {
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
                },
                result = controller.step() => result,
            };
            if let Err(error) = handled {
                break Err(error);
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
        return result;
    }
    controller.cancel();
    controller.finish().await
}

struct Controller {
    snapshots: watch::Sender<Snapshot>,
    operation: Option<Operation>,
    prepared: Option<runner::PreparedRun>,
    finished: Option<Snapshot>,
    http: Http,
    config: Config,
    interactive: bool,
}
impl Controller {
    async fn finish(&mut self) -> Result<Snapshot, Error> {
        self.wait().await?;
        Ok(self.snapshots.borrow().clone())
    }

    async fn wait(&mut self) -> Result<(), Error> {
        while self.operation.is_some() {
            self.step().await?;
        }
        Ok(())
    }

    /// Handles the operation's completion, or aborts it once its stop is overdue; pending without one.
    async fn step(&mut self) -> Result<(), Error> {
        let Some(operation) = &mut self.operation else {
            return std::future::pending().await;
        };
        let stopping = operation.stopping;
        tokio::select! {
            result = &mut operation.task => self.finished(result),
            () = deadline(stopping) => {
                self.abort();
                Ok(())
            }
        }
    }

    fn new(config: &Config, snapshots: watch::Sender<Snapshot>, interactive: bool) -> Result<Self, Error> {
        Ok(Self {
            snapshots,
            operation: None,
            prepared: None,
            finished: None,
            http: Http::new(config.insecure)?,
            config: config.clone(),
            interactive,
        })
    }
    fn launch(&mut self, work: Work) -> Result<(), Error> {
        assert!(self.operation.is_none());
        if self.config.insecure != work.config().insecure {
            self.http = Http::new(work.config().insecure)?;
        }
        self.config = work.config().clone();
        let running = matches!(work, Work::Run(_));
        if !running {
            self.prepared = None;
        }
        let fresh = self.prepared.as_ref().is_some_and(|run| run.fresh_for(work.config()));
        let servers = if fresh { self.snapshots.borrow().servers.clone() } else { Vec::new() };
        self.snapshots.send_replace(Snapshot {
            servers,
            phase: if running { Phase::Preparing } else { Phase::Checking },
            ..Snapshot::default()
        });
        // Grants carry over; connections belong to this check or run alone.
        let (http, snapshots) = (self.http.fresh(), self.snapshots.clone());
        let (prepared, interactive) = (self.prepared.take(), self.interactive);
        let purpose = if running { Purpose::Run } else { Purpose::Check };
        self.operation = Some(Operation::new(purpose, move |cancelled| {
            execute(work, http, snapshots, cancelled, prepared, interactive)
        }));
        Ok(())
    }
    fn replace(&mut self, work: Work) -> Result<(), Error> {
        let Some(operation) = &mut self.operation else {
            return self.launch(work);
        };
        if matches!(work, Work::Run(_)) && operation.purpose == Purpose::Check {
            // As in Go, the run starts at once: stopping the replaced check is its
            // preparation, and stopping the run before launch reads as stopped.
            operation.purpose = Purpose::PreparingRun;
            operation.started = Instant::now();
            self.snapshots
                .send_replace(Snapshot { phase: Phase::Preparing, ..Snapshot::default() });
        }
        operation.pending = Some(work);
        self.request_cancel();
        Ok(())
    }
    fn cancel(&mut self) {
        if let Some(operation) = &mut self.operation {
            operation.pending = None;
        }
        self.request_cancel();
    }
    fn request_cancel(&mut self) {
        if let Some(operation) = &mut self.operation {
            operation.cancel();
            self.snapshots.send_modify(|snapshot| snapshot.auth = None);
        }
    }

    fn abort(&mut self) {
        if let Some(operation) = &mut self.operation {
            operation.task.abort();
            operation.stopping = None;
        }
    }
    fn finished(
        &mut self,
        result: Result<Result<Option<runner::PreparedRun>, Error>, tokio::task::JoinError>,
    ) -> Result<(), Error> {
        let mut operation = self
            .operation
            .take()
            .expect("completion belongs to the owned operation");
        let running = operation.purpose != Purpose::Check;
        // Unless it was stopped before launch, the run that replaced the check goes on.
        let preparing = operation.purpose == Purpose::PreparingRun && operation.pending.is_some();
        if *operation.cancel.borrow() {
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
                        snapshot.phase = if snapshot.measured() { Phase::Incomplete } else { Phase::Failed };
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
                    if running && signed_out && self.interactive && operation.pending.is_none() {
                        operation.pending = Some(Work::Verify(self.config.clone()));
                    }
                }
            }
        }
        if running && !preparing {
            let duration = operation.started.elapsed();
            self.snapshots.send_modify(|snapshot| snapshot.duration = duration);
            self.finished = Some(self.snapshots.borrow().clone());
        }
        let pending = operation.pending.take();
        drop(operation);
        pending.map_or(Ok(()), |work| self.launch(work))
    }
    fn open_browser(&self) {
        let Some(prompt) = self.snapshots.borrow().auth.clone() else {
            return;
        };
        // As Go's, every press starts a launcher, which is reaped and never stopped: the browser owns
        // the outcome. The URL came from validated PKCE setup, never a shell command.
        if let Ok(mut child) = browser(&prompt.browser_url) {
            tokio::spawn(async move { child.wait().await });
        }
    }
    async fn stop(&mut self) {
        self.cancel();
        let _ = self.wait().await;
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
            Work::Run(config) => {
                let run = runner::run(config.clone(), http.clone(), snapshots.clone(), cancel.clone(), prepared.take());
                run.await.map(|()| None)
            }
            Work::Verify(config) => tokio::select! {
                biased;
                _ = cancel.wait_for(|cancelled| *cancelled) => return Ok(None),
                result = runner::prepare_run(config, &http, &snapshots) => result.map(|prepared| {
                    snapshots.send_modify(|snapshot| snapshot.phase = Phase::Setup);
                    Some(prepared)
                }),
            },
        };
        let Err(error) = result else {
            return result;
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
        .spawn()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An interactive controller of `config` whose snapshot starts in `phase`.
    fn controller(config: &Config, phase: Phase) -> Controller {
        let _ = crate::crypto::provider().install_default();
        let snapshots = watch::channel(Snapshot { phase, ..Snapshot::default() });
        Controller::new(config, snapshots.0, true).unwrap()
    }

    /// A controller whose path check runs until it is cancelled.
    fn checking() -> Controller {
        let mut controller = controller(&Config::default(), Phase::Checking);
        controller.operation = Some(Operation::new(Purpose::Check, |mut cancelled| async move {
            let _ = cancelled.wait_for(|cancelled| *cancelled).await;
            Ok(None)
        }));
        controller
    }

    #[tokio::test]
    async fn dropping_the_controller_aborts_its_owned_task() {
        let mut controller = checking();
        controller.stop().await;
        let (held, released) = tokio::sync::oneshot::channel::<()>();
        controller.operation = Some(Operation::new(Purpose::Check, |_| async move {
            let _held = held;
            std::future::pending().await
        }));
        drop(controller);
        let released = tokio::time::timeout(Duration::from_secs(1), released).await;
        assert!(released.unwrap().is_err());
    }

    #[tokio::test]
    async fn shutdown_joins_operation_before_returning_its_partial_result() {
        for phase in [Phase::Measuring, Phase::Cancelled, Phase::Complete] {
            let mut controller = controller(&Config::default(), phase);
            let snapshots = controller.snapshots.clone();
            let (joined, completed) = tokio::sync::oneshot::channel();
            controller.operation = Some(Operation::new(Purpose::Run, |mut cancelled_signal| async move {
                let _ = cancelled_signal.wait_for(|cancelled| *cancelled).await;
                snapshots.send_modify(|snapshot| snapshot.latest.up_bps = Some(42.0));
                let _ = joined.send(());
                Ok(None)
            }));
            controller.stop().await;
            completed.await.unwrap();
            assert!(controller.operation.is_none());
            let expected = if phase == Phase::Complete { Phase::Complete } else { Phase::Cancelled };
            assert_eq!(controller.snapshots.borrow().phase, expected);
            assert_eq!(controller.snapshots.borrow().latest.up_bps, Some(42.0));
        }
    }

    #[tokio::test]
    async fn a_run_replacing_a_path_check_starts_at_once() {
        let mut controller = checking();
        controller.replace(Work::Run(Config::default())).unwrap();
        assert_eq!(controller.snapshots.borrow().phase, Phase::Preparing);
        // The check ending is the run's preparation, not a stopped run.
        controller.step().await.unwrap();
        assert!(controller.finished.is_none());
        assert!(controller.operation.is_some(), "the run launched");
        controller.stop().await;

        let mut controller = checking();
        controller.replace(Work::Run(Config::default())).unwrap();
        controller.cancel();
        controller.wait().await.unwrap();
        assert_eq!(controller.snapshots.borrow().phase, Phase::Cancelled);
        let finished = controller.finished.as_ref().map(|run| run.phase);
        assert_eq!(finished, Some(Phase::Cancelled));
    }

    #[tokio::test]
    async fn a_sign_in_that_expires_after_measuring_checks_the_servers_again() {
        let config = Config { url: "http://127.0.0.1:1".into(), ..Config::default() };
        let mut controller = controller(&config, Phase::default());
        let snapshots = controller.snapshots.clone();
        controller.operation = Some(Operation::new(Purpose::Run, |_| async move {
            let measured = crate::model::StageResult { elapsed: Duration::from_secs(1), ..Default::default() };
            snapshots.send_modify(|snapshot| snapshot.results.push(measured));
            Err(Box::new(Failure::SignIn {
                origin: "https://meter.test".into(),
                login_url: "https://meter.test/login".into(),
            }) as Error)
        }));
        controller.step().await.unwrap();
        let finished = controller.finished.as_ref().map(|run| run.phase);
        assert_eq!(finished, Some(Phase::Incomplete));
        assert_eq!(controller.snapshots.borrow().phase, Phase::Checking);
        controller.stop().await;
    }

    /// Sign-in asked without a login page, as both servers end a revoked upload lane
    /// (go/internal/endpoint/upload.go:65-66), ends the run and checks the servers again, as Go's
    /// client prepares again (run.go:275-278, 306-310); no empty login page is opened.
    #[tokio::test]
    async fn a_sign_in_without_a_login_page_checks_the_servers_again() -> Result<(), Error> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (listener, url) = crate::fixtures::listener().await?;
        let config = Config { url, ..Config::default() };
        let server = tokio::spawn(async move {
            for _ in 0..8 {
                let (mut stream, _) = listener.accept().await?;
                let mut request = [0_u8; 4096];
                let count = stream.read(&mut request).await?;
                let answer = if request[..count].starts_with(b"GET /servers ") {
                    let body = r#"{"defaultSelection":["self"],"servers":[{"id":"self","url":".","name":"self"}]}"#;
                    crate::fixtures::ok(body)
                } else {
                    "HTTP/1.1 403 Forbidden\r\nGraphite-Meter-Auth: required\r\nContent-Length: 0\r\n\r\n".into()
                };
                stream.write_all(answer.as_bytes()).await?;
            }
            Ok::<_, Error>(())
        });
        let mut controller = controller(&config, Phase::default());
        controller.launch(Work::Run(config))?;
        controller.step().await?;
        let phase = controller.snapshots.borrow().phase;
        controller.stop().await;
        server.abort();
        assert_eq!(phase, Phase::Checking);
        Ok(())
    }

    #[tokio::test]
    async fn an_expired_approval_asks_for_a_new_code_like_go() {
        for purpose in [Purpose::Check, Purpose::Run] {
            let mut controller = controller(&Config::default(), Phase::default());
            controller.operation =
                Some(Operation::new(purpose, |_| async { Err(Box::new(Failure::ApprovalExpired) as Error) }));
            controller.step().await.unwrap();
            assert_eq!(controller.snapshots.borrow().error.as_deref(), Some(SIGN_IN_EXPIRED));
        }
    }
}
