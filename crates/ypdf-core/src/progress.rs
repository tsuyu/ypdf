//! Progress reporting.
//!
//! Workers own a [`ProgressReporter`] and call [`ProgressReporter::advance`];
//! the GUI or CLI owns the receiving end. Emission is throttled, so a tight
//! per-object loop can report freely without flooding the channel or forcing a
//! repaint every iteration.

use std::borrow::Cow;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use serde::Serialize;

use crate::op::OperationId;

/// Default gap between emitted events: ~30 per second.
const DEFAULT_INTERVAL: Duration = Duration::from_millis(33);

/// Channel depth. Deep enough to absorb a GUI frame, shallow enough that a
/// stalled consumer never lets events pile up unboundedly.
const CHANNEL_DEPTH: usize = 64;

/// A single progress observation.
#[derive(Clone, Debug, Serialize)]
pub struct Progress {
    /// The operation this belongs to.
    pub op: OperationId,
    /// What is happening right now, e.g. `"rendering page 12"`.
    pub stage: Cow<'static, str>,
    /// Units completed.
    pub done: u64,
    /// Total units, when known up front.
    pub total: Option<u64>,
    /// Time since the operation started.
    pub elapsed: Duration,
    /// Set on the final event.
    pub finished: bool,
}

impl Progress {
    /// Completion in `0.0..=1.0`, when the total is known.
    #[must_use]
    pub fn fraction(&self) -> Option<f32> {
        let total = self.total?;
        if total == 0 {
            return Some(1.0);
        }
        #[expect(clippy::cast_precision_loss, reason = "display only")]
        Some((self.done as f32 / total as f32).clamp(0.0, 1.0))
    }

    /// Linear-extrapolation ETA, when the total is known and work has started.
    #[must_use]
    pub fn eta(&self) -> Option<Duration> {
        let total = self.total?;
        if self.done == 0 || self.done >= total {
            return None;
        }
        let per_unit = self.elapsed.as_secs_f64() / self.done as f64;
        let remaining = (total - self.done) as f64 * per_unit;
        Duration::try_from_secs_f64(remaining).ok()
    }
}

/// Create a progress channel sized for one operation.
#[must_use]
pub fn channel() -> (Sender<Progress>, Receiver<Progress>) {
    bounded(CHANNEL_DEPTH)
}

/// Sending half, held by whichever thread does the work.
#[derive(Debug)]
pub struct ProgressReporter {
    op: OperationId,
    tx: Option<Sender<Progress>>,
    stage: Cow<'static, str>,
    done: u64,
    total: Option<u64>,
    start: Instant,
    last_emit: Option<Instant>,
    interval: Duration,
}

impl ProgressReporter {
    /// A reporter that publishes to `tx`.
    #[must_use]
    pub fn new(op: OperationId, tx: Sender<Progress>) -> Self {
        Self {
            op,
            tx: Some(tx),
            stage: Cow::Borrowed(""),
            done: 0,
            total: None,
            start: Instant::now(),
            last_emit: None,
            interval: DEFAULT_INTERVAL,
        }
    }

    /// A reporter that discards everything, for callers that do not care.
    #[must_use]
    pub fn silent() -> Self {
        Self {
            op: OperationId::new(),
            tx: None,
            stage: Cow::Borrowed(""),
            done: 0,
            total: None,
            start: Instant::now(),
            last_emit: None,
            interval: DEFAULT_INTERVAL,
        }
    }

    /// Declare the total unit count, enabling percentage and ETA.
    #[must_use]
    pub fn with_total(mut self, total: u64) -> Self {
        self.total = Some(total);
        self
    }

    /// Override the throttle interval.
    #[must_use]
    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    /// The operation being reported on.
    #[must_use]
    pub const fn op(&self) -> OperationId {
        self.op
    }

    /// Change the stage label. Emits immediately: stage changes are rare and
    /// are the events a user most wants to see.
    pub fn set_stage(&mut self, stage: impl Into<Cow<'static, str>>) {
        self.stage = stage.into();
        self.emit(true, false);
    }

    /// Add to the completed count.
    pub fn advance(&mut self, units: u64) {
        self.done = self.done.saturating_add(units);
        self.emit(false, false);
    }

    /// Set the completed count directly.
    pub fn set_done(&mut self, done: u64) {
        self.done = done;
        self.emit(false, false);
    }

    /// Emit a final event. Always sent, never throttled.
    pub fn finish(mut self) {
        if let Some(total) = self.total {
            self.done = total;
        }
        self.emit(true, true);
        self.tx = None;
    }

    /// Wall-clock time since the reporter was created.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    fn emit(&mut self, force: bool, finished: bool) {
        let Some(tx) = &self.tx else { return };

        let now = Instant::now();
        if !force
            && let Some(last) = self.last_emit
            && now.duration_since(last) < self.interval
        {
            return;
        }

        let event = Progress {
            op: self.op,
            stage: self.stage.clone(),
            done: self.done,
            total: self.total,
            elapsed: now.duration_since(self.start),
            finished,
        };

        // A full channel means the consumer is behind; dropping an intermediate
        // sample is correct — the next one supersedes it. Never block a worker
        // on the GUI. A disconnected consumer stops reporting entirely.
        match tx.try_send(event) {
            Ok(()) => self.last_emit = Some(now),
            Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => self.tx = None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_fraction_and_finishes_at_total() {
        let (tx, rx) = channel();
        let mut r = ProgressReporter::new(OperationId::new(), tx).with_total(10);
        r.set_stage("merging");
        r.advance(5);
        r.finish();

        let events: Vec<_> = rx.into_iter().collect();
        let last = events.last().expect("at least one event");
        assert!(last.finished);
        assert_eq!(last.done, 10);
        assert_eq!(last.fraction(), Some(1.0));
    }

    #[test]
    fn throttles_intermediate_events() {
        let (tx, rx) = channel();
        let mut r = ProgressReporter::new(OperationId::new(), tx)
            .with_total(1000)
            .with_interval(Duration::from_secs(60));
        for _ in 0..500 {
            r.advance(1);
        }
        r.finish();
        // One throttled first sample, plus the forced final event.
        assert!(rx.into_iter().count() <= 2);
    }

    #[test]
    fn silent_reporter_does_not_panic() {
        let mut r = ProgressReporter::silent().with_total(3);
        r.advance(1);
        r.finish();
    }

    #[test]
    fn eta_is_none_without_a_total() {
        let p = Progress {
            op: OperationId::new(),
            stage: "x".into(),
            done: 1,
            total: None,
            elapsed: Duration::from_secs(1),
            finished: false,
        };
        assert!(p.eta().is_none());
        assert!(p.fraction().is_none());
    }
}
