//! Bound Tokio's blocking pool independently of its async worker pool.
use anyhow::{bail, Context, Result};
use std::ffi::OsString;
use tokio::runtime::{Builder, Runtime};

pub const BLOCKING_THREADS: &str = "BOBBIN_TOKIO_BLOCKING_THREADS";

fn blocking_limit(value: Option<OsString>) -> Result<Option<usize>> {
    let Some(value) = value else { return Ok(None) };
    let count = value.to_str().and_then(|v| v.parse::<usize>().ok());
    match count {
        Some(count @ 1..=512) => Ok(Some(count)),
        _ => bail!("{BLOCKING_THREADS} must be an integer from 1 through 512"),
    }
}

pub fn build(value: Option<OsString>) -> Result<Runtime> {
    let limit = blocking_limit(value)?;
    // Like #[tokio::main], retain TOKIO_WORKER_THREADS and all I/O/time drivers.
    let mut builder = Builder::new_multi_thread();
    builder.enable_all();
    if let Some(limit) = limit {
        builder.max_blocking_threads(limit);
    }
    builder.build().context("build Tokio runtime")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use std::time::Duration;

    #[test]
    fn blocking_budget_rejects_invalid_values_and_preserves_unset_default() {
        assert_eq!(blocking_limit(None).unwrap(), None);
        assert_eq!(blocking_limit(Some("2".into())).unwrap(), Some(2));
        for value in ["", "0", "-1", "513", "99999999999999999999999", "two"] {
            assert!(blocking_limit(Some(value.into())).is_err(), "{value}");
        }
    }

    fn pressure(limit: usize) -> usize {
        let runtime = build(Some(limit.to_string().into())).unwrap();
        let current = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        runtime.block_on(async {
            let mut tasks = Vec::new();
            for _ in 0..24 {
                let current = current.clone();
                let peak = peak.clone();
                tasks.push(tokio::task::spawn_blocking(move || {
                    let active = current.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(active, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(50));
                    current.fetch_sub(1, Ordering::SeqCst);
                    1
                }));
            }
            let mut completed = 0;
            for task in tasks {
                completed += task.await.unwrap();
            }
            assert_eq!(completed, 24, "the budget must queue work, not drop it");
        });
        assert_eq!(current.load(Ordering::SeqCst), 0);
        let observed = peak.load(Ordering::SeqCst);
        println!("blocking budget={limit}; peak active={observed}; completed=24");
        observed
    }

    #[test]
    fn blocking_budget_bounds_real_tasks_without_dropping_work() {
        assert!(
            pressure(8) > 2,
            "positive control must exceed the small budget"
        );
        assert!(pressure(2) <= 2);
    }
}
