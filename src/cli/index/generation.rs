//! Durable producer checkpoints. A wrapper timestamp is never reset proof.
use anyhow::{bail, Context, Result};
use fs4::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::storage::{MetadataStore, VectorStore};

/// Hash semantic configuration, independent of HashMap iteration or JSON's
/// preserve_order feature. Arrays retain their semantically significant order.
pub(super) fn identity(input: &impl Serialize) -> Result<String> {
    let mut value = serde_json::to_value(input)?;
    value.sort_all_objects();
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(&value)?)))
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(super) enum Phase {
    ResetPending,
    Ready,
    Complete,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Record {
    pub schema: u32,
    pub id: String,
    pub repo: String,
    pub identity: String,
    pub phase: Phase,
    pub completed_at: Option<i64>,
    pub supersedes: Option<String>,
    stages: BTreeSet<String>,
    reset_sources: BTreeSet<String>,
}

/// Serializes index/watch writers across the shared model and auxiliary stores.
/// It is an OS advisory lock, released by the kernel even after SIGKILL.
pub(crate) struct WriterLock(File);

impl WriterLock {
    pub(crate) fn acquire(home: &Path) -> Result<Self> {
        Self::open_lock(home, true)
    }

    pub(crate) fn ordinary(home: &Path) -> Result<Self> {
        Self::open_lock(home, false)
    }

    fn open_lock(home: &Path, exclusive: bool) -> Result<Self> {
        let dir = home.join(".bobbin/generations");
        std::fs::create_dir_all(&dir)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("writer.lock"))?;
        if exclusive {
            FileExt::try_lock_exclusive(&file)
        } else {
            FileExt::try_lock_shared(&file)
        }
        .context("index generation writer is busy")?;
        Ok(Self(file))
    }

    pub(crate) fn refuse_pending(home: &Path) -> Result<()> {
        for entry in std::fs::read_dir(home.join(".bobbin/generations"))? {
            let path = entry?.path();
            if path.extension().is_some_and(|x| x == "json") {
                let record = read(&path)?;
                if record.phase != Phase::Complete {
                    bail!(
                        "pending generation {}; use explicit --resume-generation",
                        record.id
                    );
                }
            }
        }
        Ok(())
    }
}

impl Drop for WriterLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

pub(super) struct Generation {
    path: PathBuf,
    pub record: Record,
}

fn record_path(home: &Path, repo: &str) -> PathBuf {
    home.join(".bobbin/generations")
        .join(format!("{:x}.json", Sha256::digest(repo.as_bytes())))
}

fn read(path: &Path) -> Result<Record> {
    let record: Record = serde_json::from_slice(&std::fs::read(path)?)?;
    if record.schema != 1 {
        bail!("unsupported index generation schema");
    }
    Ok(record)
}

impl Generation {
    /// Caller holds WriterLock for the entire invocation, including all stages.
    pub(super) fn open(
        home: &Path,
        repo: &str,
        identity: String,
        start: Option<&str>,
        resume: Option<&str>,
    ) -> Result<Option<Self>> {
        let path = record_path(home, repo);
        if start.is_none() && resume.is_none() {
            WriterLock::refuse_pending(home)?;
            return Ok(None);
        }
        let id = start.or(resume).context("generation id required")?;
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        {
            bail!("generation id must be 1..128 ASCII letters, digits, -, _ or .");
        }
        let mut supersedes = None;
        if path.exists() {
            let record = read(&path)?;
            if resume.is_some() {
                if record.id != id || record.repo != repo || record.identity != identity {
                    bail!("generation identity mismatch; explicit new generation required");
                }
                return Ok(Some(Self { path, record }));
            }
            if record.id == id {
                bail!("generation already exists; use explicit --resume-generation");
            }
            // An explicit, different start ID is a new-generation disposition,
            // never a resume. Persist its ResetPending before touching stores.
            supersedes = Some(record.id);
        } else if resume.is_some() {
            bail!("generation does not exist");
        }
        for entry in std::fs::read_dir(home.join(".bobbin/generations"))? {
            let other = entry?.path();
            if other != path
                && other.extension().is_some_and(|x| x == "json")
                && read(&other)?.phase != Phase::Complete
            {
                bail!("another repository has a pending generation");
            }
        }
        let generation = Self {
            path,
            record: Record {
                schema: 1,
                id: id.to_string(),
                repo: repo.to_string(),
                identity,
                phase: Phase::ResetPending,
                completed_at: None,
                supersedes,
                stages: BTreeSet::new(),
                reset_sources: BTreeSet::new(),
            },
        };
        generation.persist()?;
        Ok(Some(generation))
    }

    pub(super) fn status(home: &Path, repo: &str) -> Result<Record> {
        read(&record_path(home, repo))
    }

    fn persist(&self) -> Result<()> {
        // Both fsyncs matter: after a kill, seeing Ready must imply the rename
        // is durable. The shared writer lock owns this same-directory temp path.
        let temp = self.path.with_extension("pending");
        let mut file = File::create(&temp)?;
        file.write_all(&serde_json::to_vec_pretty(&self.record)?)?;
        file.sync_all()?;
        std::fs::rename(temp, &self.path)?;
        File::open(self.path.parent().context("generation directory missing")?)?.sync_all()?;
        Ok(())
    }

    pub(super) fn ready(&mut self) -> Result<()> {
        if self.record.phase != Phase::ResetPending {
            bail!("reset completion requires ResetPending");
        }
        self.record.phase = Phase::Ready;
        self.persist()
    }

    pub(super) fn needs(&self, stage: &str) -> bool {
        !self.record.stages.contains(stage)
    }

    pub(super) fn finish(&mut self, stage: &str) -> Result<()> {
        if self.record.phase != Phase::Ready {
            bail!("stage completion requires Ready");
        }
        self.record.stages.insert(stage.to_string());
        self.persist()
    }

    /// Reset an auxiliary source's vectors and hashes exactly once. Until the
    /// reset is durable, a retry repeats both clears before any source work.
    pub(super) async fn reset_source(
        &mut self,
        store: &MetadataStore,
        vectors: &VectorStore,
        key: &str,
    ) -> Result<()> {
        if !self.record.reset_sources.contains(key) {
            vectors.delete_by_repo(key).await?;
            store.clear_file_hashes(key)?;
            self.record.reset_sources.insert(key.to_string());
            self.persist()?;
        }
        Ok(())
    }

    pub(super) fn complete(&mut self) -> Result<()> {
        if self.record.phase != Phase::Ready {
            bail!("generation completion requires Ready");
        }
        self.record.phase = Phase::Complete;
        self.record.completed_at = Some(chrono::Utc::now().timestamp());
        self.persist()
    }
}

pub(super) fn needs(generation: &Option<Generation>, stage: &str) -> bool {
    generation.as_ref().is_none_or(|g| g.needs(stage))
}

pub(super) fn finish(generation: &mut Option<Generation>, stage: &str) -> Result<()> {
    if let Some(generation) = generation {
        generation.finish(stage)?;
    }
    Ok(())
}

impl Generation {
    pub(super) async fn reset(
        &mut self,
        metadata: &MetadataStore,
        vectors: &VectorStore,
        repo: &str,
    ) -> Result<()> {
        self.reset_observed(metadata, vectors, repo, |_| Ok(()))
            .await
    }

    async fn reset_observed(
        &mut self,
        metadata: &MetadataStore,
        vectors: &VectorStore,
        repo: &str,
        mut observe: impl FnMut(&str) -> Result<()>,
    ) -> Result<()> {
        if self.record.phase == Phase::ResetPending {
            observe("ResetPending")?;
            vectors.delete_by_repo(repo).await?;
            observe("AfterVectorClear")?;
            metadata.clear_file_hashes(repo)?;
            observe("AfterHashClear")?;
            self.ready()?;
        }
        observe("Ready")?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "generation_tests.rs"]
mod tests;
