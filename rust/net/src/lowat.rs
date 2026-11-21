//! Adaptive `SO_RCVLOWAT` batching shared by plain HTTP/1.1 bulk receive paths on Linux.

use nix::{libc, setsockopt_impl, sockopt_impl, sys::socket::setsockopt};
use std::{future::Future, io, pin::Pin, task::Context, time::Duration};
use tokio::{
    net::TcpStream,
    time::{Instant, Sleep},
};

sockopt_impl!(
    ReadLowWater,
    SetOnly,
    libc::SOL_SOCKET,
    libc::SO_RCVLOWAT,
    libc::c_int
);

pub const READ_LOWAT_MIN: u64 = 4 << 10;
pub const READ_LOWAT_MAX: u64 = 16 << 10;

/// Adaptive Linux `SO_RCVLOWAT` state machine: stays at 1 B unless steady small wakeups justify batching.
#[derive(Default)]
pub enum BulkRead {
    Sampling {
        started: Instant,
        bytes: u64,
        burst: u64,
        short_waits: u8,
        total_waits: u32,
    },
    Batch {
        last: Instant,
        lowat: u64,
    },
    #[default]
    Ordinary,
}

impl BulkRead {
    const SMALL_BURST_BYTES: u64 = 4 << 10;
    const SAMPLE_BYTES: u64 = 32 << 10;
    const SAMPLE_WAITS: u8 = 12;
    const SAMPLE_TIME: Duration = Duration::from_millis(25);
    /// Target kernel batching interval: well below `WAIT` so steady transfers stay batched without adding delay.
    const TARGET_WAIT_US: u128 = 2_000;
    /// A short burst of data must not stall the sender or progress checkpoints.
    pub const WAIT: Duration = Duration::from_millis(5);

    pub fn sample() -> Self {
        Self::Sampling {
            started: Instant::now(),
            bytes: 0,
            burst: 0,
            short_waits: 0,
            total_waits: 0,
        }
    }

    pub fn is_batched(&self) -> bool {
        matches!(self, Self::Batch { .. })
    }

    pub fn lowat(&self) -> u64 {
        match *self {
            Self::Batch { lowat, .. } => lowat,
            _ => READ_LOWAT_MIN,
        }
    }

    pub fn finish(&mut self, socket: &TcpStream) -> io::Result<()> {
        if matches!(self, Self::Batch { .. }) {
            setsockopt(socket, ReadLowWater, &1)?;
        }
        *self = Self::Ordinary;
        Ok(())
    }

    pub fn on_read(&mut self, moved: usize) {
        match self {
            Self::Sampling { bytes, burst, .. } => {
                *bytes += moved as u64;
                *burst += moved as u64;
            }
            Self::Batch { last, .. } => *last = Instant::now(),
            Self::Ordinary => {}
        }
    }

    pub fn on_would_block(&mut self, socket: &TcpStream, remaining: u64) {
        if let Self::Sampling {
            started,
            bytes,
            burst,
            short_waits,
            total_waits,
        } = self
        {
            *total_waits = total_waits.saturating_add(1);
            if (1..=Self::SMALL_BURST_BYTES).contains(&std::mem::take(burst)) {
                *short_waits = short_waits.saturating_add(1);
            }
            if *bytes >= Self::SAMPLE_BYTES {
                let now = Instant::now();
                let elapsed = now.saturating_duration_since(*started);
                let avg_small = *bytes <= u64::from(*total_waits) * Self::SMALL_BURST_BYTES;
                if *short_waits >= Self::SAMPLE_WAITS && avg_small && elapsed <= Self::SAMPLE_TIME {
                    let elapsed_us = elapsed.as_micros().max(1);
                    let target = ((*bytes as u128 * Self::TARGET_WAIT_US) / elapsed_us)
                        .min(READ_LOWAT_MAX as u128) as u64;
                    let cap = remaining.saturating_sub(1).min(READ_LOWAT_MAX);
                    let lowat = target.min(cap);
                    if lowat >= READ_LOWAT_MIN
                        && remaining > lowat
                        && setsockopt(socket, ReadLowWater, &(lowat as libc::c_int)).is_ok()
                    {
                        *self = Self::Batch { last: now, lowat };
                        return;
                    }
                }
                *started = now;
                *bytes = 0;
                *short_waits = 0;
                *total_waits = 0;
            }
        }
    }

    pub fn reset_after_timeout(&mut self, socket: &TcpStream) {
        let _ = setsockopt(socket, ReadLowWater, &1);
        *self = Self::sample();
    }

    pub fn observe(
        &mut self,
        socket: &TcpStream,
        cx: &mut Context<'_>,
        mut deadline: Pin<&mut Sleep>,
        moved: usize,
        pending: bool,
        remaining: u64,
    ) -> io::Result<()> {
        match self {
            Self::Sampling { .. } => {
                if moved != 0 {
                    self.on_read(moved);
                    return Ok(());
                }
                if !pending {
                    return Ok(());
                }
                self.on_would_block(socket, remaining);
                if let Self::Batch { last, .. } = *self {
                    deadline.as_mut().reset(last + Self::WAIT);
                    if deadline.as_mut().poll(cx).is_ready() {
                        cx.waker().wake_by_ref();
                    }
                }
            }
            Self::Batch { last, .. } if moved != 0 => *last = Instant::now(),
            Self::Batch { last, .. } if pending => {
                if deadline.as_mut().poll(cx).is_pending() {
                    return Ok(());
                }
                deadline.as_mut().reset(*last + Self::WAIT);
                if deadline.as_mut().poll(cx).is_ready() {
                    setsockopt(socket, ReadLowWater, &1)?;
                    *self = Self::sample();
                    cx.waker().wake_by_ref();
                }
            }
            _ => {}
        }
        Ok(())
    }
}
