//! Per-stage liveness heartbeats consumed by the /health endpoint.
//!
//! Each pipeline stage owns a `StageHeartbeat`; the stage updates `last_input_at`
//! when it receives a unit of work and `last_progress_at` when it produces output.
//! /health declares a stage healthy when the two are close in time (progress is
//! caught up to input within `STAGE_PROGRESS_MARGIN_SECS`) or when no input has
//! ever been received (legitimately idle).

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Margin within which last_progress_at must be of last_input_at. 5s absorbs
/// in-flight processing without flagging stages that are just busy.
const STAGE_PROGRESS_MARGIN_SECS: i64 = 5;

#[derive(Debug, Default)]
pub struct StageHeartbeat {
    last_input_at: AtomicI64,
    last_progress_at: AtomicI64,
}

impl StageHeartbeat {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Bump on each successfully received unit of input.
    pub fn record_input(&self) {
        self.last_input_at.store(now_unix(), Ordering::Relaxed);
    }

    /// Bump on each successfully produced output.
    pub fn record_progress(&self) {
        self.last_progress_at.store(now_unix(), Ordering::Relaxed);
    }

    /// Healthy iff never received input (idle) or progress is caught up with input.
    pub fn is_healthy(&self) -> bool {
        let t_input = self.last_input_at.load(Ordering::Relaxed);
        if t_input == 0 {
            return true;
        }
        let t_progress = self.last_progress_at.load(Ordering::Relaxed);
        t_progress >= t_input - STAGE_PROGRESS_MARGIN_SECS
    }
}

/// Top-level registry passed to the /health handler. Each field is `None` when
/// the corresponding stage isn't running (e.g. read-only mode skips all stages).
#[derive(Debug, Default, Clone)]
pub struct HeartbeatRegistry {
    pub dedup: Option<Arc<StageHeartbeat>>,
    pub sigverify: Option<Arc<StageHeartbeat>>,
    pub sequencer: Option<Arc<StageHeartbeat>>,
    pub executor: Option<Arc<StageHeartbeat>>,
    pub settler: Option<Arc<StageHeartbeat>>,
}

impl HeartbeatRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the name of the first unhealthy stage, or None if every running stage is healthy.
    pub fn first_unhealthy(&self) -> Option<&'static str> {
        for (name, hb) in [
            ("dedup", &self.dedup),
            ("sigverify", &self.sigverify),
            ("sequencer", &self.sequencer),
            ("executor", &self.executor),
            ("settler", &self.settler),
        ] {
            if let Some(hb) = hb {
                if !hb.is_healthy() {
                    return Some(name);
                }
            }
        }
        None
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
