//! One operation owns publication; replacements start only after its task joins.
use crate::{
    Error,
    config::Config,
    model::{AuthPrompt, Phase, Snapshot},
    net::{AuthRequired, Http},
    runner,
    ui::{self, Command},
};
use std::{collections::HashSet, future::Future, process::Stdio, time::Duration};
use tokio::{
    process::Child,
    sync::{mpsc, watch},
    task::JoinSet,
    time::Instant,
};

const CANCEL_GRACE: Duration = Duration::from_secs(5);
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

pub async fn run(config: Config, shutdown: impl Future<Output = ()>) -> Result<Snapshot, Error> {
    let (snapshots, receiver) = watch::channel(Snapshot::default());
    let (commands, mut incoming) = mpsc::channel(8);
    let mut controller = Controller::new(&config, snapshots)?;
    controller.start(Work::Verify(config.clone()))?;
    let result = {
        let terminal = ui::run(config, receiver, commands);
        tokio::pin!(terminal, shutdown);
        loop {
            tokio::select! {
                result = &mut terminal => break result,
                _ = &mut shutdown => break Ok(()),
                command = incoming.recv() => {
                    match command {
                        Some(Command::Quit) | None => break Ok(()),
                        Some(Command::Run(config)) => {
                            if let Err(error) = controller.replace(Work::Run(config)) {
                                break Err(error);
                            }
                        }
                        Some(Command::Verify(config)) => {
                            if let Err(error) = controller.replace(Work::Verify(config)) {
                                break Err(error);
                            }
                        }
                        Some(Command::Cancel) => controller.cancel(),
                        Some(Command::OpenBrowser) => controller.open_browser(),
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
    result?;
    let final_snapshot = controller.snapshots.borrow().clone();
    Ok(final_snapshot)
}

struct Controller {
    snapshots: watch::Sender<Snapshot>,
    operations: JoinSet<Result<(), Error>>,
    cancel: Option<watch::Sender<bool>>,
    pending: Option<Work>,
    cancelling: bool,
    cancel_deadline: Option<Instant>,
    http: Http,
    insecure: bool,
    browser: Option<Child>,
    browser_deadline: Option<Instant>,
}
impl Controller {
    fn new(config: &Config, snapshots: watch::Sender<Snapshot>) -> Result<Self, Error> {
        Ok(Self {
            snapshots,
            operations: JoinSet::new(),
            cancel: None,
            pending: None,
            cancelling: false,
            cancel_deadline: None,
            http: Http::new(config.insecure)?,
            insecure: config.insecure,
            browser: None,
            browser_deadline: None,
        })
    }
    fn start(&mut self, work: Work) -> Result<(), Error> {
        assert!(self.operations.is_empty());
        if self.insecure != work.config().insecure {
            self.http = Http::new(work.config().insecure)?;
            self.insecure = work.config().insecure;
        }
        self.snapshots.send_replace(Snapshot {
            phase: Phase::Preparing,
            status: "Preparing selected servers".into(),
            ..Snapshot::default()
        });
        let (cancel, cancelled) = watch::channel(false);
        self.cancel = Some(cancel);
        self.cancelling = false;
        self.cancel_deadline = None;
        let http = self.http.clone();
        let snapshots = self.snapshots.clone();
        self.operations
            .spawn(async move { execute(work, http, snapshots, cancelled).await });
        Ok(())
    }
    fn replace(&mut self, work: Work) -> Result<(), Error> {
        if self.operations.is_empty() {
            self.start(work)
        } else {
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
            self.cancel_deadline
                .get_or_insert(Instant::now() + CANCEL_GRACE);
            self.snapshots.send_modify(|snapshot| {
                snapshot.status = "Cancelling; waiting for owned IO".into();
                snapshot.auth = None;
            });
        }
    }
    fn finished(
        &mut self,
        result: Result<Result<(), Error>, tokio::task::JoinError>,
    ) -> Result<(), Error> {
        self.cancel = None;
        self.cancel_deadline = None;
        if self.cancelling {
            self.snapshots.send_modify(|snapshot| {
                snapshot.phase = Phase::Cancelled;
                snapshot.status = "Cancelled".into();
                snapshot.auth = None;
                snapshot.error = None;
            });
        } else if let Err(error) = result
            .map_err(|error| Box::new(error) as Error)
            .and_then(|result| result)
        {
            self.snapshots.send_modify(|snapshot| {
                snapshot.phase = Phase::Failed;
                snapshot.status = "Failed".into();
                snapshot.error = Some(error.to_string());
                snapshot.auth = None;
            });
        }
        self.cancelling = false;
        if let Some(work) = self.pending.take() {
            self.start(work)?;
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
        self.pending = None;
        self.request_cancel();
        if tokio::time::timeout(CANCEL_GRACE, async {
            while self.operations.join_next().await.is_some() {}
        })
        .await
        .is_err()
        {
            self.operations.abort_all();
            while self.operations.join_next().await.is_some() {}
        }
        self.close_browser().await;
        if self.cancelling {
            self.snapshots.send_modify(|snapshot| {
                if matches!(
                    snapshot.phase,
                    Phase::Preparing | Phase::Warmup | Phase::Measuring
                ) {
                    snapshot.phase = Phase::Cancelled;
                    snapshot.status = "Cancelled".into();
                }
                snapshot.auth = None;
            });
        }
    }
}

async fn execute(
    work: Work,
    http: Http,
    snapshots: watch::Sender<Snapshot>,
    mut cancel: watch::Receiver<bool>,
) -> Result<(), Error> {
    let mut approvals = HashSet::new();
    loop {
        if *cancel.borrow() {
            return Ok(());
        }
        let result = match &work {
            Work::Run(config) => {
                runner::run(
                    config.clone(),
                    http.clone(),
                    snapshots.clone(),
                    cancel.clone(),
                )
                .await
            }
            Work::Verify(config) => tokio::select! {
                biased;
                _ = cancelled(&mut cancel) => return Ok(()),
                result = runner::verify(config, &http, &snapshots) => result,
            },
        };
        let error = match result {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };
        let Some(required) = authentication_required(error.as_ref()) else {
            return Err(error);
        };
        let origin = required.origin.clone();
        let login = required.login_url.clone();
        if !approvals.insert(origin.clone()) {
            return Err(
                "server rejected the approved credential; verify its authentication configuration"
                    .into(),
            );
        }
        let pending = http.begin_authorization(&origin, &login)?;
        snapshots.send_modify(|snapshot| {
            snapshot.phase = Phase::Preparing;
            snapshot.error = None;
            snapshot.status = "Approve this client in your browser".into();
            snapshot.auth = Some(AuthPrompt {
                origin: origin.clone(),
                browser_url: pending.browser_url.clone(),
                code: pending.code.clone(),
            });
        });
        tokio::select! {
            biased;
            _ = cancelled(&mut cancel) => return Ok(()),
            result = http.poll_authorization(pending) => result?,
        }
        snapshots.send_modify(|snapshot| {
            snapshot.auth = None;
            snapshot.status = "Approved; retrying selected operation".into();
        });
    }
}
fn authentication_required<'a>(
    mut error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a AuthRequired> {
    loop {
        if let Some(required) = error.downcast_ref::<AuthRequired>() {
            return Some(required);
        }
        error = error.source()?;
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
    if url::Url::parse(url)?.scheme() != "https" {
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
        let _ = rustls::crypto::ring::default_provider().install_default();
        let (snapshots, _) = watch::channel(Snapshot {
            phase: Phase::Measuring,
            ..Snapshot::default()
        });
        let mut controller = Controller::new(&Config::default(), snapshots.clone()).unwrap();
        let (cancel, mut cancelled_signal) = watch::channel(false);
        controller.cancel = Some(cancel);
        let (joined, completed) = tokio::sync::oneshot::channel();
        controller.operations.spawn(async move {
            cancelled(&mut cancelled_signal).await;
            snapshots.send_modify(|snapshot| snapshot.latest.up_bps = Some(42.0));
            let _ = joined.send(());
            Ok(())
        });
        controller.stop().await;
        completed.await.unwrap();
        assert!(controller.operations.is_empty());
        assert_eq!(controller.snapshots.borrow().phase, Phase::Cancelled);
        assert_eq!(controller.snapshots.borrow().latest.up_bps, Some(42.0));
    }
}
