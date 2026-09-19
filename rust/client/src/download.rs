//! Owned download lanes. Only received bytes contribute to measurement.
use crate::{Error, transport::Transport};
use graphite_meter_core::route::Route;
use http::Method;
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
    time::timeout,
};

pub struct Download {
    bytes: Arc<AtomicU64>,
    tasks: JoinSet<Result<(), Error>>,
}

impl Download {
    pub async fn start(
        transport: Arc<Transport>,
        lanes: usize,
        duration: Duration,
        cancel: watch::Receiver<bool>,
    ) -> Result<Self, Error> {
        if !(1..=128).contains(&lanes) {
            return Err("invalid download lane count".into());
        }
        let bytes = Arc::new(AtomicU64::new(0));
        let mut owner = Self {
            bytes,
            tasks: JoinSet::new(),
        };
        let (ready, mut received) = mpsc::channel(lanes);
        for lane in 0..lanes {
            let transport = transport.clone();
            let bytes = owner.bytes.clone();
            let ready = ready.clone();
            let mut cancel = cancel.clone();
            owner.tasks.spawn(async move {
                let transfer = async {
                    let lane = lane.to_string();
                    let mut announced = false;
                    loop {
                        const LIMIT: u64 = 64 * 1024 * 1024 * 1024;
                        let mut body = transport
                            .receive(
                                Method::GET,
                                Route::Download,
                                &[("bytes", "68719476736"), ("lane", &lane)],
                                LIMIT,
                                duration,
                            )
                            .await?;
                        let mut received = 0_u64;
                        while let Some(chunk) = body.chunk().await? {
                            bytes.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                            received += chunk.len() as u64;
                            if !announced && !chunk.is_empty() {
                                ready
                                    .send(())
                                    .await
                                    .map_err(|_| "download readiness receiver closed")?;
                                announced = true;
                            }
                        }
                        if received != LIMIT {
                            return Err("download ended before its declared byte count".into());
                        }
                    }
                };
                tokio::select! {
                    result = transfer => result,
                    _ = cancel.wait_for(|value| *value) => Ok(()),
                }
            });
        }
        drop(ready);
        timeout(Duration::from_secs(10), async {
            for _ in 0..lanes {
                tokio::select! {
                    value = received.recv() => value.ok_or("download ended before readiness")?,
                    task = owner.tasks.join_next() => {
                        task.ok_or("no download lanes")???;
                        return Err::<(), Error>("download cancelled before readiness".into());
                    }
                }
            }
            Ok(())
        })
        .await??;
        Ok(owner)
    }

    pub fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    pub fn health(&mut self) -> Result<(), Error> {
        if let Some(task) = self.tasks.try_join_next() {
            task??;
            return Err("download lane ended before stage boundary".into());
        }
        Ok(())
    }

    pub async fn stop(mut self) {
        self.tasks.shutdown().await;
    }
}
