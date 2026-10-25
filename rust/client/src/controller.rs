//! One operation at a time, each awaiting the one it replaces for a bounded time; sign-in waits on the operator.
use crate::{
    config::Config,
    events::{Event, Events, SignInEnd, SignInPrompt},
    model::{Failure, Outcome},
    net::{
        Client, Fault,
        approval::{self, Approval, Unapproved},
    },
    run::{
        coordinator,
        prepare::{Prepared, prepare},
    },
};
use futures_util::FutureExt;
use graphite_meter_net::{Pool, Proxy, Verify};
use graphite_meter_proto::{origin::Origin, reason::FailureReason};
use std::{
    collections::HashMap,
    panic::AssertUnwindSafe,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// How long a stopped operation may take to finish before it is dropped.
const GRACE: Duration = Duration::from_secs(5);
/// Why a run without the interface does not sign in.
pub const SIGN_IN: &str = "Sign-in required; run graphite-meter-client in a terminal to sign in.";
const REJECTED: &str = "the server refused the approved sign-in; check its sign-in configuration";
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Check(Config),
    Run(Config),
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Check,
    Run,
}

/// What operations hand on: the last check for a run to reuse, and each signed-in server's grant.
#[derive(Default)]
struct State {
    prepared: Option<Prepared>,
    grants: HashMap<Origin, String>,
}

struct Operation {
    kind: Kind,
    config: Config,
    /// The unfinished check a run replaced, which a stop starts again.
    replaced: Option<Config>,
    token: CancellationToken,
    stop: CancellationToken,
    task: JoinHandle<State>,
}

pub struct Controller {
    /// What each operation starts from; its stop token is replaced.
    work: Work,
    current: Option<Operation>,
    idle: State,
}

impl Controller {
    pub fn new(interactive: bool, runtimes: Arc<Pool>, events: Events) -> Self {
        let work = Work {
            events,
            runtimes,
            interactive,
            outbound: None,
            stop: CancellationToken::new(),
        };
        Self {
            work,
            current: None,
            idle: State::default(),
        }
    }

    /// The same controller connecting through `proxy` and verifying as `verify` says, unless a config is insecure.
    pub fn with_outbound(mut self, verify: Verify, proxy: Proxy) -> Self {
        self.work.outbound = Some((verify, proxy));
        self
    }

    pub fn command(&mut self, command: Command) {
        match command {
            Command::Check(config) => self.launch(config, Kind::Check),
            Command::Run(config) => self.launch(config, Kind::Run),
            Command::Stop => {
                let Some(current) = &self.current else { return };
                current.stop.cancel();
                if let Some(config) = current.replaced.clone() {
                    self.launch(config, Kind::Check);
                }
            }
        }
    }

    pub async fn settled(&mut self) {
        if let Some(current) = self.current.take() {
            self.idle = current.task.await.unwrap_or_default();
        }
    }

    /// Cancels the current operation and starts one for `config` once it ended.
    fn launch(&mut self, config: Config, kind: Kind) {
        let previous = self.current.take();
        let unfinished = previous
            .as_ref()
            .filter(|check| check.kind == Kind::Check && !check.task.is_finished());
        let replaced = unfinished
            .filter(|_| kind == Kind::Run)
            .map(|check| check.config.clone());
        if let Some(previous) = &previous {
            previous.token.cancel();
        }
        let token = CancellationToken::new();
        let work = Work {
            stop: token.child_token(),
            ..self.work.clone()
        };
        let (stop, idle, settings) = (
            work.stop.clone(),
            std::mem::take(&mut self.idle),
            config.clone(),
        );
        let task = tokio::spawn(async move {
            let state = match previous {
                Some(previous) => previous.task.await.unwrap_or_default(),
                None => idle,
            };
            work.operate(settings, kind, state).await
        });
        self.current = Some(Operation {
            kind,
            config,
            replaced,
            token,
            stop,
            task,
        });
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        if let Some(current) = &self.current {
            current.token.cancel();
        }
    }
}

#[derive(Clone)]
struct Work {
    events: Events,
    runtimes: Arc<Pool>,
    interactive: bool,
    /// The proxies and verification of checked clients in place of the environment's.
    outbound: Option<(Verify, Proxy)>,
    stop: CancellationToken,
}

impl Work {
    async fn operate(self, config: Config, kind: Kind, mut state: State) -> State {
        let started = Instant::now();
        let work = async {
            match kind {
                Kind::Run => self.run(&config, &mut state).await,
                Kind::Check => self.check(&config, &mut state).await,
            }
        };
        let guarded = guarded(&self.events, kind, started, work);
        bounded(&self.events, kind, started, &self.stop, guarded).await;
        state
    }

    async fn check(&self, config: &Config, state: &mut State) {
        self.events.send(Event::Checking(Kind::Check));
        if let Some(previous) = state.prepared.take() {
            previous.end().await;
        }
        match self.prepare(config, state).await {
            Some(Ok(prepared)) => {
                self.events.send(prepared.event());
                state.prepared = Some(prepared);
            }
            Some(Err(failure)) => self.events.send(Event::CheckFailed(failure)),
            None => {}
        }
    }

    /// Runs `config` over its check's paths while they are fresh, else over new ones.
    async fn run(&self, config: &Config, state: &mut State) {
        let started = Instant::now();
        self.events.send(Event::Checking(Kind::Run));
        let key = config.key();
        let reused = match state.prepared.take() {
            Some(check) if check.reusable(&key, started) => Some(check),
            Some(stale) => {
                stale.end().await;
                None
            }
            None => None,
        };
        let prepared = match reused {
            Some(prepared) => Some(Ok(prepared)),
            None => self.prepare(config, state).await,
        };
        let (outcome, error) = match prepared.filter(|_| !self.stop.is_cancelled()) {
            Some(Ok(prepared)) => {
                self.events.send(prepared.event());
                coordinator::run(&prepared, config, &self.events, self.stop.clone()).await;
                prepared.end().await;
                return;
            }
            Some(Err(failure)) => (Outcome::Failed, Some(failure)),
            None => (Outcome::Stopped, None),
        };
        let elapsed = started.elapsed();
        self.events.send(Event::RunFinished {
            outcome,
            error,
            elapsed,
        });
    }

    /// Prepares `config` with kept grants, signing in while a server asks and an operator approves; none once stopped.
    async fn prepare(
        &self,
        config: &Config,
        state: &mut State,
    ) -> Option<Result<Prepared, Failure>> {
        let mut approved = Vec::new();
        loop {
            let client = match &self.outbound {
                Some((verify, proxy)) if !config.insecure => {
                    Client::with(verify.clone(), proxy.clone(), self.runtimes.clone())
                }
                _ => Client::new(config.insecure, self.runtimes.clone()),
            };
            for (origin, grant) in &state.grants {
                client.grant(origin, grant);
            }
            let mut prepared = self
                .stop
                .run_until_cancelled(prepare(config, client.clone()))
                .await?;
            let Some((origin, issuer)) = asking(&prepared, config.url.as_ref()) else {
                return Some(prepared);
            };
            let refusal = match approval::refusal(&origin, config.insecure) {
                _ if !self.interactive => {
                    Some(Failure::new(FailureReason::SignInRequired, SIGN_IN))
                }
                Some(text) => Some(Failure::new(FailureReason::SignInRequired, text)),
                None if approved.contains(&origin) => {
                    Some(Failure::new(FailureReason::SignInRequired, REJECTED))
                }
                None => None,
            };
            if let Some(refusal) = refusal {
                refuse(&mut prepared, &refusal);
                return Some(prepared);
            }
            match self.sign_in(&origin, issuer, &client).await? {
                Ok(grant) => state.grants.insert(origin.clone(), grant),
                Err(failure) => return Some(Err(failure)),
            };
            if let Ok(asked) = prepared {
                asked.end().await;
            }
            approved.push(origin);
        }
    }

    /// The grant of the operator's approval at `origin`, or why none came; none once stopped.
    async fn sign_in(
        &self,
        origin: &Origin,
        issuer: String,
        client: &Client,
    ) -> Option<Result<String, Failure>> {
        let approval = Approval::new(origin);
        let (url, code, deadline) = (
            approval.url.clone(),
            approval.code.clone(),
            approval.deadline,
        );
        let prompt = SignInPrompt {
            issuer,
            url,
            code,
            deadline,
        };
        self.events.send(Event::SignIn(prompt));
        let granted = self.stop.run_until_cancelled(approval.grant(client)).await;
        let end = match &granted {
            None => SignInEnd::Canceled,
            Some(Ok(_)) => SignInEnd::Approved,
            Some(Err(Unapproved::Expired)) => SignInEnd::Expired,
            Some(Err(Unapproved::Failed(_))) => SignInEnd::Failed,
        };
        self.events.send(Event::SignInEnded(end));
        Some(granted?.map_err(|unapproved| unapproved.failure()))
    }
}

/// Awaits `work` until it ends, or until the grace after `stop`; a run cut short ends as stopped.
async fn bounded(
    events: &Events,
    kind: Kind,
    started: Instant,
    stop: &CancellationToken,
    work: impl Future<Output = ()>,
) {
    let overdue = async {
        stop.cancelled().await;
        tokio::time::sleep(GRACE).await;
    };
    tokio::select! {
        () = work => {}
        () = overdue => if kind == Kind::Run {
            let (outcome, elapsed) = (Outcome::Stopped, started.elapsed());
            events.send(Event::RunFinished { outcome, error: None, elapsed });
        }
    }
}

/// Runs `work`, the operation begun at `started`; a panic in it ends the check or run as failed.
async fn guarded(events: &Events, kind: Kind, started: Instant, work: impl Future<Output = ()>) {
    if AssertUnwindSafe(work).catch_unwind().await.is_ok() {
        return;
    }
    let (failure, elapsed) = (Fault::Internal.failure(), started.elapsed());
    events.send(match kind {
        Kind::Run => Event::RunFinished {
            outcome: Outcome::Failed,
            error: Some(failure),
            elapsed,
        },
        Kind::Check => Event::CheckFailed(failure),
    });
}

/// The first origin asking for sign-in, the server's or a test server's, with the name a prompt gives it.
fn asking(prepared: &Result<Prepared, Failure>, url: Option<&Origin>) -> Option<(Origin, String)> {
    let asks = |failure: &Failure| failure.reason == FailureReason::SignInRequired;
    match prepared {
        Err(failure) => url
            .filter(|_| asks(failure))
            .map(|url| (url.clone(), url.to_string())),
        Ok(prepared) => prepared
            .servers
            .iter()
            .find(|server| server.path.as_ref().is_err_and(asks))
            .map(|server| (server.origin.clone(), server.name.clone())),
    }
}

/// Gives every sign-in failure of `prepared` the refusal's reason and text.
fn refuse(prepared: &mut Result<Prepared, Failure>, refusal: &Failure) {
    let replace = |failure: &mut Failure| {
        if failure.reason == FailureReason::SignInRequired {
            *failure = refusal.clone();
        }
    };
    match prepared {
        Err(failure) => replace(failure),
        Ok(prepared) => {
            let servers = prepared.servers.iter_mut();
            let failed = servers.filter_map(|server| server.path.as_mut().err());
            failed.for_each(replace);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::assert_matches;

    #[tokio::test]
    async fn an_operation_that_panics_ends_its_run_or_check_as_failed() {
        let (events, mut received) = Events::channel();
        for kind in [Kind::Run, Kind::Check] {
            guarded(&events, kind, Instant::now(), async {
                panic!("an operation's bug")
            })
            .await;
            let failure = match (kind, received.try_recv().unwrap()) {
                (
                    Kind::Run,
                    Event::RunFinished {
                        outcome: Outcome::Failed,
                        error: Some(failure),
                        ..
                    },
                ) => failure,
                (Kind::Check, Event::CheckFailed(failure)) => failure,
                (_, event) => panic!("{event:?}"),
            };
            assert_eq!(failure, Fault::Internal.failure());
        }
        guarded(&events, Kind::Run, Instant::now(), async {}).await;
        assert!(
            received.try_recv().is_err(),
            "a finished operation sends nothing more"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_stopped_run_that_hangs_ends_as_stopped_after_its_grace() {
        let (events, mut received) = Events::channel();
        for kind in [Kind::Check, Kind::Run] {
            let (stop, started) = (CancellationToken::new(), tokio::time::Instant::now());
            stop.cancel();
            bounded(&events, kind, Instant::now(), &stop, std::future::pending()).await;
            assert_eq!(started.elapsed(), GRACE);
            let ended = received.try_recv().ok();
            match kind {
                Kind::Check => assert_eq!(ended, None),
                Kind::Run => assert_matches!(
                    ended,
                    Some(Event::RunFinished {
                        outcome: Outcome::Stopped,
                        ..
                    })
                ),
            }
        }
        let stop = CancellationToken::new();
        bounded(&events, Kind::Run, Instant::now(), &stop, async {}).await;
        assert!(
            received.try_recv().is_err(),
            "a run that ends needs no grace"
        );
    }
}
