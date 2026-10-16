//! The maintenance step of `bobbin index`: prune + compact, and when to run it.
//! Split from `index.rs` (at its size ceiling).
//!
//! `--maintenance` decides WHETHER an index run sweeps the store:
//!
//! * `always` (default): every run sweeps, as before. The scheduled full
//!   reindex is the only job that prunes, so it must not be skipped.
//! * `auto`: sweep only when the store's shared maintenance record says the
//!   last compaction is older than `--maintenance-interval-hours`, or on
//!   `--force`. For frequent incremental refreshes: a sweep rewrites the
//!   table, and a 15-minute refresh that compacted every time wrote ~5 GB per
//!   run to a shared SSD and stalled the databases beside it (aegis-91w).
//! * `never`: do not sweep from this run at all.

use anyhow::{Context, Result};
use clap::ValueEnum;

use crate::storage::{LockWait, MaintenanceOutcome, MaintenanceStatus};

/// How long the reindex waits for the store-wide maintenance lock before giving
/// up.
///
/// Sized against the CONTENDER, not against comfort: with the read path
/// throttled store-wide, the only thing that can hold this lock is one
/// opportunistic, memory-bounded compaction — not another full sweep. It is
/// also multiplied by the number of repos, because the nightly runs one `index`
/// per repo (27 here), so a "generous" budget is really 27x that in the
/// pathological case. Two minutes is long enough to outlast a bounded
/// compaction and short enough that a genuinely stuck lock costs a bounded
/// amount of nightly — and every expiry now says so out loud.
const DEFAULT_MAINTENANCE_LOCK_WAIT_SECS: u64 = 120;

/// Environment override for [`DEFAULT_MAINTENANCE_LOCK_WAIT_SECS`], so a host
/// whose reindex unit has a tighter `TimeoutStartSec` can shorten the wait
/// without a rebuild. `0` restores the old skip-on-contention behaviour.
const MAINTENANCE_LOCK_WAIT_ENV: &str = "BOBBIN_MAINTENANCE_LOCK_WAIT_SECS";

pub(super) fn maintenance_lock_wait() -> LockWait {
    let secs = std::env::var(MAINTENANCE_LOCK_WAIT_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_MAINTENANCE_LOCK_WAIT_SECS);
    if secs == 0 {
        LockWait::NoWait
    } else {
        LockWait::UpTo(std::time::Duration::from_secs(secs))
    }
}

/// When an index run prunes and compacts the store. See the module docs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum MaintenanceMode {
    #[default]
    Always,
    Auto,
    Never,
}

/// What this run decided to do about maintenance, before touching the lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Decision {
    Run,
    /// `auto`: the last compaction is younger than the interval.
    SkipRecent {
        age_secs: u64,
    },
    /// `never`.
    SkipDisabled,
}

/// Decide from the store's shared maintenance record. Pure, so it is tested
/// without a store. A missing record, or one stamped in the future (a clock
/// that moved), counts as stale: skipping on bad data would let the store
/// grow unswept, which is the failure the nightly sweep exists to prevent.
pub(super) fn decide(
    mode: MaintenanceMode,
    force: bool,
    status: MaintenanceStatus,
    now_unix: u64,
    interval_secs: u64,
) -> Decision {
    match mode {
        MaintenanceMode::Always => Decision::Run,
        MaintenanceMode::Never => Decision::SkipDisabled,
        MaintenanceMode::Auto if force => Decision::Run,
        MaintenanceMode::Auto => match status.last_compact_unix {
            Some(last) if last <= now_unix && now_unix - last < interval_secs => {
                Decision::SkipRecent {
                    age_secs: now_unix - last,
                }
            }
            _ => Decision::Run,
        },
    }
}

/// What the maintenance step of a reindex actually did, so the run can say so.
///
/// The interesting state is a lock-starved sweep — maintenance that did nothing
/// while the command still exits 0. That was previously indistinguishable from
/// a sweep that reclaimed gigabytes, and the difference went unnoticed for
/// months because nothing could report it. A deliberate skip (`auto`/`never`)
/// is labelled separately, so it can never be mistaken for either.
#[derive(Debug)]
pub(super) struct MaintenanceReport {
    outcome: Result<MaintenanceOutcome, Decision>,
}

impl MaintenanceReport {
    /// Turn a completed maintenance call into its operator-facing report.
    ///
    /// A real maintenance error must fail the index command. Swallowing it here
    /// made systemd report success while every scheduled run failed compaction.
    /// Lock contention is different: it is represented explicitly by
    /// `SkippedLockHeld` and remains a reportable, non-error outcome.
    pub(super) fn from_result(r: anyhow::Result<MaintenanceOutcome>) -> anyhow::Result<Self> {
        Ok(Self {
            outcome: Ok(r.context("Lance maintenance failed")?),
        })
    }

    /// A run that deliberately did not sweep (`Decision` other than `Run`).
    pub(super) fn skipped(decision: Decision) -> Self {
        Self {
            outcome: Err(decision),
        }
    }

    /// Print an unmissable stderr warning when the sweep was starved.
    ///
    /// stderr and not stdout, and unconditional: `--json` and `--quiet` are the
    /// modes the scheduled reindex actually runs in, and those are exactly the
    /// runs where a silent skip went unnoticed for months.
    pub(super) fn warn_if_starved(&self) {
        let Ok(MaintenanceOutcome::SkippedLockHeld { waited }) = self.outcome else {
            return;
        };
        let waited = waited.as_secs();
        eprintln!(
            "warning: MAINTENANCE SKIPPED — another process held the store maintenance \
             lock for the whole {waited}s wait. Nothing was pruned or compacted by this \
             run; the store did not shrink. Set {MAINTENANCE_LOCK_WAIT_ENV} higher, or \
             find the contender (fuser .maintenance.lock in the lance dir)."
        );
    }

    /// Machine-readable summary for `--json`, e.g. `"ran"`,
    /// `"skipped_lock_held"` or `"skipped_recent"`.
    pub(super) fn json_label(&self) -> String {
        match self.outcome {
            Ok(outcome) => outcome.label().to_string(),
            Err(Decision::SkipRecent { .. }) => "skipped_recent".to_string(),
            Err(Decision::SkipDisabled) => "skipped_disabled".to_string(),
            Err(Decision::Run) => "ran".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: u64 = 3600;
    const NOW: u64 = 1_792_161_137;

    fn compacted_at(t: u64) -> MaintenanceStatus {
        MaintenanceStatus {
            last_compact_unix: Some(t),
            last_prune_unix: Some(t),
        }
    }

    #[test]
    fn always_runs_whatever_the_record_says() {
        let fresh = compacted_at(NOW - 60);
        assert_eq!(
            decide(MaintenanceMode::Always, false, fresh, NOW, 24 * HOUR),
            Decision::Run
        );
    }

    #[test]
    fn auto_skips_a_recent_compaction_and_reports_its_age() {
        let d = decide(
            MaintenanceMode::Auto,
            false,
            compacted_at(NOW - 900),
            NOW,
            24 * HOUR,
        );
        assert_eq!(d, Decision::SkipRecent { age_secs: 900 });
        assert_eq!(MaintenanceReport::skipped(d).json_label(), "skipped_recent");
    }

    #[test]
    fn auto_runs_once_the_interval_has_passed() {
        let d = decide(
            MaintenanceMode::Auto,
            false,
            compacted_at(NOW - 24 * HOUR),
            NOW,
            24 * HOUR,
        );
        assert_eq!(d, Decision::Run);
    }

    #[test]
    fn auto_runs_when_the_store_was_never_compacted() {
        let d = decide(
            MaintenanceMode::Auto,
            false,
            MaintenanceStatus::default(),
            NOW,
            24 * HOUR,
        );
        assert_eq!(d, Decision::Run);
    }

    #[test]
    fn auto_treats_a_future_stamp_as_stale_not_fresh() {
        let d = decide(
            MaintenanceMode::Auto,
            false,
            compacted_at(NOW + HOUR),
            NOW,
            24 * HOUR,
        );
        assert_eq!(d, Decision::Run);
    }

    #[test]
    fn force_makes_auto_sweep_even_after_a_recent_compaction() {
        let d = decide(
            MaintenanceMode::Auto,
            true,
            compacted_at(NOW - 60),
            NOW,
            24 * HOUR,
        );
        assert_eq!(d, Decision::Run);
    }

    #[test]
    fn never_skips_even_on_force_and_says_so() {
        let d = decide(
            MaintenanceMode::Never,
            true,
            MaintenanceStatus::default(),
            NOW,
            24 * HOUR,
        );
        assert_eq!(d, Decision::SkipDisabled);
        assert_eq!(
            MaintenanceReport::skipped(d).json_label(),
            "skipped_disabled"
        );
    }

    #[test]
    fn a_deliberate_skip_is_never_reported_as_lock_starved() {
        // warn_if_starved prints only for a lock-starved sweep; a skip must not
        // read as one, or the alarm for the real failure loses its meaning.
        let report = MaintenanceReport::skipped(Decision::SkipRecent { age_secs: 1 });
        assert!(!matches!(
            report.outcome,
            Ok(MaintenanceOutcome::SkippedLockHeld { .. })
        ));
    }
}
