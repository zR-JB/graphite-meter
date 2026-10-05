//! One server's share of a stage: its download lanes, upload session and prober; dropping it ends them.
use super::{
    engine::{Probe, Sample, StagePlan, stagger},
    prepare::ServerPath,
    probe::Prober,
    upload::UploadSession,
};
use crate::{
    config::Config,
    measure::aggregate::{Reading, Receiver},
    model::{Dir, Direction, Failure, LaneHealth},
    net::{Client, Lanes, Work, topology},
};
use graphite_meter_proto::catalog::ServerId;
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};
use tokio::task::JoinHandle;
use tokio_util::sync::{CancellationToken, DropGuard};

pub struct Participant {
    server: ServerId,
    down: Option<Lanes>,
    up: Option<UploadSession>,
    prober: Option<Prober>,
    /// The upload session finishing after the window closed.
    finishing: Option<JoinHandle<()>>,
    _departs: DropGuard,
}

impl Participant {
    /// Starts `server`'s work for `plan` under `token`; `replaced` is its upload replacement for the run.
    pub async fn open(
        client: &Client,
        server: &ServerPath,
        plan: &StagePlan,
        config: &Config,
        replaced: Arc<AtomicBool>,
        token: CancellationToken,
    ) -> Result<Self, Failure> {
        let departs = token.clone().drop_guard();
        let paths = server.path.as_ref().map_err(Clone::clone)?;
        let member = plan.members.iter().find(|member| member.server == server.id);
        let warmup = member.map_or(Duration::ZERO, |member| member.warmup);
        let path = &paths.throughput;
        let lanes = config.lanes(path.protocol, path.transport);
        let plans = topology(path, plan.stage, lanes);
        let spacing = |direction| stagger(warmup, lanes[direction]);
        let prober = plan
            .latency
            .zip(paths.latency.clone())
            .map(|(cadence, latency)| Prober::spawn(client.clone(), latency, plan.stage, cadence, token.child_token()));
        let down = plan.stage.moves(Direction::Down).then(|| {
            Lanes::start(client, plans.clone(), Work::Download, spacing(Direction::Down), token.child_token())
        });
        let up = match plan.stage.moves(Direction::Up) {
            true => {
                let child = token.child_token();
                let session = UploadSession::open(client, path, plans, spacing(Direction::Up), replaced, child);
                Some(session.await.map_err(|fault| fault.failure())?)
            }
            false => None,
        };
        Ok(Self {
            server: server.id.clone(),
            down,
            up,
            prober,
            finishing: None,
            _departs: departs,
        })
    }

    pub fn server(&self) -> &ServerId {
        &self.server
    }

    /// Its local counters and lane health now; checkpoints come apart.
    pub fn local(&mut self) -> Sample {
        let reading = Reading {
            server: self.server.clone(),
            down: self.down.as_ref().map(Lanes::bytes),
            up: None,
            fed: self.up.as_ref().and_then(UploadSession::fed),
        };
        let ready = self.down.as_ref().is_none_or(Lanes::ready) && self.up.as_ref().is_none_or(UploadSession::ready);
        let lanes = Dir {
            down: self.down.as_mut().map_or(LaneHealth::Ok, Lanes::health),
            up: self.up.as_mut().map_or(LaneHealth::Ok, UploadSession::health),
        };
        Sample { reading, ready, missed: None, lanes }
    }

    /// A fresh receiver checkpoint within `budget`, when it uploads.
    pub async fn checkpoint(&self, budget: Duration) -> Option<Result<Receiver, Failure>> {
        let up = self.up.as_ref()?;
        Some(up.checkpoint(budget).await.map_err(|fault| fault.failure()))
    }

    /// Moves what its prober observed into `into`.
    pub fn probes(&mut self, into: &mut Vec<(ServerId, Probe)>) {
        let mut probes = Vec::new();
        if let Some(prober) = &mut self.prober {
            prober.drain(&mut probes);
        }
        into.extend(probes.into_iter().map(|probe| (self.server.clone(), probe)));
    }

    /// The measured window opened and ends at `end`.
    pub fn opened(&self, end: Instant) {
        if let Some(prober) = &self.prober {
            prober.open(end);
        }
    }

    /// The window closed: downloads stop, the upload session finishes within `budget` and the prober drains.
    pub fn close(&mut self, budget: Duration) {
        self.down = None;
        if let Some(prober) = &self.prober {
            prober.close();
        }
        if let Some(up) = self.up.take() {
            self.finishing = Some(tokio::spawn(up.finish(budget)));
        }
    }

    /// Its latency population failed: probing ends.
    pub fn stop_probing(&mut self) {
        self.prober = None;
    }

    /// Leaves the stage: its work ends and its upload receiver is asked to finalize.
    pub fn depart(mut self) {
        if let Some(up) = self.up.take() {
            up.depart();
        }
    }

    /// Closes it if the window did not, and waits up to `budget` for its upload session to finish.
    pub async fn finish(mut self, budget: Duration) {
        self.close(budget);
        if let Some(finishing) = self.finishing.take() {
            let _ = tokio::time::timeout(budget, finishing).await;
        }
    }
}
