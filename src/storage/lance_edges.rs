//! Bounded edge deletion, retaining the existing single-file API.
use super::*;

const EDGE_CLEAR_BATCH_SIZE: usize = 100;

impl VectorStore {
    /// Clear chunk edges for a specific file, optionally scoped to one repo
    pub async fn clear_file_chunk_edges(&self, file_path: &str, repo: Option<&str>) -> Result<()> {
        let table = match &self.chunk_edges_table {
            Some(t) => t,
            None => return Ok(()),
        };

        let mut filter = format!("file_path = '{}'", file_path.replace('\'', "''"));
        if let Some(r) = repo {
            filter.push_str(&format!(" AND repo = '{}'", r.replace('\'', "''")));
        }
        retry_on_conflict!(table, table.delete(&filter))
            .context("Failed to clear file chunk edges")?;

        Ok(())
    }

    /// Clear selected files' edges in bounded batches, optionally within one repo.
    ///
    /// An empty selection or absent edge table is a no-op. Each batch retains
    /// the normal conflict retry; an unrecoverable error stops before later
    /// batches or the caller's replacement inserts. This is not an atomic
    /// transaction across batches, just as single-file deletion was not.
    pub async fn clear_files_chunk_edges(
        &self,
        file_paths: &[String],
        repo: Option<&str>,
    ) -> Result<()> {
        let Some(table) = &self.chunk_edges_table else {
            return Ok(());
        };
        for files in file_paths.chunks(EDGE_CLEAR_BATCH_SIZE) {
            let values = files
                .iter()
                .map(|file| format!("'{}'", file.replace('\'', "''")))
                .collect::<Vec<_>>()
                .join(",");
            let mut filter = format!("file_path IN ({values})");
            if let Some(repo) = repo {
                filter.push_str(&format!(" AND repo = '{}'", repo.replace('\'', "''")));
            }
            retry_on_conflict!(table, table.delete(&filter))
                .context("Failed to clear file chunk edges batch")?;
        }
        Ok(())
    }
}
