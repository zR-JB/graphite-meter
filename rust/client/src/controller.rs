//! One operation at a time, a path check or a run: each waits for the one it replaces, a stopped run ends within a
//! 5 s grace, and a server asking for sign-in gets it where the operator can approve it.
use crate::{
    config::Config,
    events::{Event, Events, SignInEnd, SignInPrompt},
    model::{Failure, Outcome},
    net::{
        Client,
        approval::{self, Approval, Unapproved},
    },
    run::{
        coordinator,
        prepare::{Prepared, prepare},
    },
};
use graphite_meter_net::Pool;
use graphite_meter_proto::{origin::Origin, reason::FailureReason};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// How long a stopped operation may take to finish before it is dropped.
const GRACE: Duration = Duration::from_secs(5);
/// Why a run without the interface does not sign in.
pub const SIGN_IN: &str = "Sign-in required; run graphite-meter-client in a terminal to sign in.";
const REJECTED: &str = "server rejected the approved credential; verify its authentication configuration";

pub enum Command {
    Check(Config),
    Run(Config),
    Stop,
}

/// What operations hand on: the last check for a run to reuse, and each signed-in server's grant.
#[derive(Default)]
struct State {
    prepared: Option<Prepared>,
    grants: HashMap<Origin, String>,
}

struct Operation {
    run: bool,
    config: Config,
    /// The unfinished check a run replaced, which a stop starts again.
    replaced: Option<Config>,
    token: CancellationToken,
    stop: CancellationToken,
    task: JoinHandle<State>,
}

/// Runs the operations commands ask for and sends their events.
pub struct Controller {
    /// Whether an operator can approve sign-ins.
    interactive: bool,
    runtimes: Arc<Pool>,
    events: Events,
    current: Option<Operation>,
    idle: State,
}

impl Controller {
    pub fn new(interactive: bool, runtimes: Arc<Pool>, events: Events) -> Self {
        Self {
            interactive,
            runtimes,
            events,
            current: None,
            idle: State::default(),
        }
    }

    pub fn command(&mut self, command: Command) {
        match command {
            Command::Check(config) => self.launch(config, false),
            Command::Run(config) => self.launch(config, true),
            Command::Stop => {
                let Some(current) = &self.current else { return };
                current.stop.cancel();
                if let Some(config) = current.replaced.clone() {
                    self.launch(config, false);
                }
            }
        }
    }

    /// Waits until no operation is left.
    pub async fn settled(&mut self) {
        if let Some(current) = self.current.take() {
            self.idle = current.task.await.unwrap_or_default();
        }
    }

    /// Cancels the current operation and starts one for `config` once it ended.
    fn launch(&mut self, config: Config, run: bool) {
        let previous = self.current.take();
        let unfinished = previous
            .as_ref()
            .filter(|previous| !previous.run && !previous.task.is_finished());
        let replaced = unfinished.filter(|_| run).map(|check| check.config.clone());
        if let Some(previous) = &previous {
            previous.token.cancel();
        }
        let token = CancellationToken::new();
        let work = Work {
            events: self.events.clone(),
            runtimes: self.runtimes.clone(),
            interactive: self.interactive,
            stop: token.child_token(),
        };
        let (stop, idle, settings) = (work.stop.clone(), std::mem::take(&mut self.idle), config.clone());
        let task = tokio::spawn(async move {
            let state = match previous {
                Some(previous) => previous.task.await.unwrap_or_default(),
                None => idle,
            };
            work.operate(settings, run, state).await
        });
        self.current = Some(Operation { run, config, replaced, token, stop, task });
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        if let Some(current) = &self.current {
            current.token.cancel();
        }
    }
}

/// One operation's view of the controller.
struct Work {
    events: Events,
    runtimes: Arc<Pool>,
    interactive: bool,
    stop: CancellationToken,
}

impl Work {
    async fn operate(self, config: Config, run: bool, mut state: State) -> State {
        let started = Instant::now();
        let work = async {
            match run {
                true => self.run(&config, &mut state).await,
                false => self.check(&config, &mut state).await,
            }
        };
        let overdue = async {
            self.stop.cancelled().await;
            tokio::time::sleep(GRACE).await;
        };
        tokio::select! {
            () = work => {}
            () = overdue => if run {
                let (outcome, elapsed) = (Outcome::Stopped, started.elapsed());
                self.events.send(Event::RunFinished { outcome, error: None, elapsed });
            }
        }
        state
    }

    async fn check(&self, config: &Config, state: &mut State) {
        self.events.send(Event::Checking { run: false });
        match self.prepare(config, state).await {
            Some(Ok(prepared)) => {
                self.events.send(Event::Prepared(prepared.servers.clone().into()));
                state.prepared = Some(prepared);
            }
            Some(Err(failure)) => self.events.send(Event::CheckFailed(failure)),
            None => {}
        }
    }

    /// Runs `config` over its check's paths while they are fresh, else over new ones.
    async fn run(&self, config: &Config, state: &mut State) {
        let started = Instant::now();
        self.events.send(Event::Checking { run: true });
        let reused = state
            .prepared
            .take()
            .filter(|prepared| prepared.reusable(&config.key(), started));
        let prepared = match reused {
            Some(prepared) => Some(Ok(prepared)),
            None => self.prepare(config, state).await,
        };
        let (outcome, error) = match prepared.filter(|_| !self.stop.is_cancelled()) {
            Some(Ok(prepared)) => {
                self.events.send(Event::Prepared(prepared.servers.clone().into()));
                coordinator::run(&prepared, config, &self.events, self.stop.clone()).await;
                return;
            }
            Some(Err(failure)) => (Outcome::Failed, Some(failure)),
            None => (Outcome::Stopped, None),
        };
        self.events
            .send(Event::RunFinished { outcome, error, elapsed: started.elapsed() });
    }

    /// Prepares `config` with the grants kept, signing in while a server asks and an operator can approve; none once
    /// stopped.
    async fn prepare(&self, config: &Config, state: &mut State) -> Option<Result<Prepared, Failure>> {
        let mut approved = Vec::new();
        loop {
            let client = Client::new(config.insecure, self.runtimes.clone());
            for (origin, grant) in &state.grants {
                client.grant(origin, grant);
            }
            let mut prepared = self.stop.run_until_cancelled(prepare(config, client.clone())).await?;
            let Some((origin, issuer)) = asking(&prepared, &config.url) else {
                return Some(prepared);
            };
            let refusal = match approval::refusal(&origin, config.insecure) {
                _ if !self.interactive => Some(Failure::new(FailureReason::SignInRequired, SIGN_IN)),
                Some(text) => Some(Failure::new(FailureReason::PreparationFailed, text)),
                None => approved
                    .contains(&origin)
                    .then(|| Failure::new(FailureReason::SignInRequired, REJECTED)),
            };
            if let Some(refusal) = refusal {
                refuse(&mut prepared, &refusal);
                return Some(prepared);
            }
            match self.sign_in(&origin, issuer, &client).await? {
                Ok(grant) => state.grants.insert(origin.clone(), grant),
                Err(failure) => return Some(Err(failure)),
            };
            approved.push(origin);
        }
    }

    /// The grant of the operator's approval at `origin`, or why none came; none once stopped.
    async fn sign_in(&self, origin: &Origin, issuer: String, client: &Client) -> Option<Result<String, Failure>> {
        let approval = Approval::new(origin);
        let (url, code, deadline) = (approval.url.clone(), approval.code.clone(), approval.deadline);
        self.events
            .send(Event::SignIn(SignInPrompt { issuer, url, code, deadline }));
        let granted = self.stop.run_until_cancelled(approval.grant(client)).await;
        let end = match &granted {
            None => SignInEnd::Cancelled,
            Some(Ok(_)) => SignInEnd::Approved,
            Some(Err(Unapproved::Expired)) => SignInEnd::Expired,
            Some(Err(Unapproved::Failed(_))) => SignInEnd::Failed,
        };
        self.events.send(Event::SignInEnded(end));
        Some(granted?.map_err(|unapproved| unapproved.failure()))
    }
}

/// The first origin asking for sign-in, the catalogue's or a server's, with the name a prompt gives it.
fn asking(prepared: &Result<Prepared, Failure>, url: &Origin) -> Option<(Origin, String)> {
    let asks = |failure: &Failure| failure.reason == FailureReason::SignInRequired;
    match prepared {
        Err(failure) => asks(failure).then(|| (url.clone(), url.to_string())),
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
            let failed = prepared
                .servers
                .iter_mut()
                .filter_map(|server| server.path.as_mut().err());
            failed.for_each(replace);
        }
    }
}
