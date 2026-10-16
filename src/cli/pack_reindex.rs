//! Incremental catch-up after activating a portable index pack.

use anyhow::Result;
use std::path::PathBuf;

use super::{index, OutputConfig};

/// Run the normal hash- and watermark-based incremental index path after a
/// portable pack has been activated.
pub(super) async fn run(
    home: PathBuf,
    source: PathBuf,
    repo: String,
    output: OutputConfig,
) -> Result<()> {
    index::run(
        index::IndexArgs {
            incremental: true,
            force: false,
            force_publish: false,
            graph: None,
            repo: Some(repo),
            source: Some(source),
            include_beads: false,
            no_quipu_publish: false,
            include_globs: Vec::new(),
            skip_calibrate: true,
            // Unchanged: a pack catch-up sweeps like any other index run.
            maintenance: index::MaintenanceMode::Always,
            maintenance_interval_hours: 24,
            path: home,
        },
        output,
    )
    .await
}
