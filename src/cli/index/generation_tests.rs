use super::super::batch::{self, Batch};
use super::*;
use crate::types::{Chunk, ChunkType};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn chunk(repo: &str, name: &str) -> Chunk {
    Chunk {
        id: format!("{repo}-{name}"),
        file_path: name.into(),
        chunk_type: ChunkType::Other,
        name: None,
        start_line: 1,
        end_line: 1,
        content: name.into(),
        language: "text".into(),
        tags: String::new(),
    }
}

fn boundary(home: &Path, point: &str) -> Result<()> {
    if std::env::var("BOBBIN_GENERATION_TEST_CUT").ok().as_deref() == Some(point) {
        std::fs::write(home.join("cut-reached"), point)?;
        // Parent sends an actual process kill after observing the boundary.
        loop {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    Ok(())
}

async fn drive(home: &Path, start: bool) -> Result<()> {
    let _lock = WriterLock::acquire(home)?;
    let mut g = Generation::open(
        home,
        "a",
        "fixture-config-root".into(),
        start.then_some("fixture"),
        (!start).then_some("fixture"),
    )?
    .unwrap();
    if g.record.phase == Phase::Complete {
        return Ok(());
    }
    let metadata = MetadataStore::open(&home.join("metadata.db"))?;
    let mut vectors = VectorStore::open_with_dim(&home.join("vectors"), 4).await?;
    g.reset_observed(&metadata, &vectors, "a", |p| boundary(home, p))
        .await?;
    for file in ["one", "two", "three"] {
        if metadata.get_file_hash("a", file)?.as_deref() == Some(file) {
            continue;
        }
        let chunks = [chunk("a", file)];
        let embeddings = [vec![0.0, 1.0, 0.0, 0.0]];
        let hashes = [(file.to_string(), file.to_string())];
        batch::persist_observed(
            &mut vectors,
            &metadata,
            "a",
            Batch {
                chunks: &chunks,
                embeddings: &embeddings,
                contexts: &[None],
                hashes: &[file],
                timestamps: &["1"],
                file_hashes: &hashes,
            },
            |p| {
                boundary(home, p)?;
                boundary(home, &format!("{p}-{file}"))
            },
        )
        .await?;
    }
    g.finish("files")?;
    boundary(home, "AfterFinalStage")?;
    g.complete()?;
    boundary(home, "BeforeAck")?;
    Ok(())
}

/// Invoked only as a subprocess by the boundary-kill controls below.
#[tokio::test]
#[ignore = "subprocess worker; parent owns its temp directory and kill"]
async fn crash_worker() {
    let home = PathBuf::from(std::env::var_os("BOBBIN_GENERATION_TEST_HOME").unwrap());
    drive(
        &home,
        std::env::var_os("BOBBIN_GENERATION_TEST_START").is_some(),
    )
    .await
    .unwrap();
}

fn kill_at(home: &Path, cut: &str, start: bool) {
    let marker = home.join("cut-reached");
    let _ = std::fs::remove_file(&marker);
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "cli::index::generation::tests::crash_worker",
            "--ignored",
            "--nocapture",
        ])
        .env("BOBBIN_GENERATION_TEST_HOME", home)
        .env("BOBBIN_GENERATION_TEST_CUT", cut)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    if start {
        command.env("BOBBIN_GENERATION_TEST_START", "1");
    } else {
        command.env_remove("BOBBIN_GENERATION_TEST_START");
    }
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !marker.exists() && Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("child exited before {cut}: {status}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let reached = marker.exists();
    child.kill().unwrap();
    let status = child.wait().unwrap();
    assert!(reached, "boundary {cut} was not reached");
    assert!(!status.success());
}

async fn fixture(home: &Path) {
    let metadata = MetadataStore::open(&home.join("metadata.db")).unwrap();
    let mut vectors = VectorStore::open_with_dim(&home.join("vectors"), 4)
        .await
        .unwrap();
    for repo in ["a", "b"] {
        vectors
            .insert(
                &[chunk(repo, "old")],
                &[vec![1.0, 0.0, 0.0, 0.0]],
                &[None],
                repo,
                "old",
                "0",
            )
            .await
            .unwrap();
        metadata.set_file_hash(repo, "old", "old").unwrap();
    }
}

async fn assert_consistent(home: &Path) {
    let metadata = MetadataStore::open(&home.join("metadata.db")).unwrap();
    let vectors = VectorStore::open_with_dim(&home.join("vectors"), 4)
        .await
        .unwrap();
    let hashes = metadata.get_all_indexed_files("a").unwrap();
    let rows = vectors
        .get_all_chunks_with_embeddings(Some("a"))
        .await
        .unwrap();
    let paths: BTreeSet<_> = rows.iter().map(|(c, _, _)| c.file_path.clone()).collect();
    let ids: BTreeSet<_> = rows.iter().map(|(c, _, _)| c.id.clone()).collect();
    assert_eq!(rows.len(), 3);
    assert_eq!(hashes.len(), rows.len());
    assert_eq!(ids.len(), rows.len(), "no duplicate vector IDs");
    assert_eq!(paths, hashes.into_iter().collect());
    let other = vectors
        .get_all_chunks_with_embeddings(Some("b"))
        .await
        .unwrap();
    assert_eq!(other.len(), 1);
    assert_eq!(other[0].0.id, "b-old");
    assert_eq!(other[0].1, vec![1.0, 0.0, 0.0, 0.0]);
    assert_eq!(
        metadata.get_file_hash("b", "old").unwrap().as_deref(),
        Some("old")
    );
    let complete = Generation::status(home, "a").unwrap();
    assert_eq!(complete.phase, Phase::Complete);
    assert!(complete.completed_at.is_some());
}

#[tokio::test]
async fn process_kills_at_each_boundary_resume_consistently() {
    for cut in [
        "ResetPending",
        "AfterVectorClear",
        "AfterHashClear",
        "Ready",
        "AfterBatchDelete",
        "AfterVectorInsert",
        "AfterHashCommit",
        "AfterFinalStage",
        "BeforeAck",
    ] {
        let home = tempfile::tempdir().unwrap();
        fixture(home.path()).await;
        kill_at(home.path(), cut, true);
        drive(home.path(), false).await.unwrap();
        assert_consistent(home.path()).await;
        let completed = Generation::status(home.path(), "a").unwrap().completed_at;
        drive(home.path(), false).await.unwrap();
        assert_eq!(
            Generation::status(home.path(), "a").unwrap().completed_at,
            completed
        );
        assert_consistent(home.path()).await;
    }
}

#[tokio::test]
async fn n_interrupted_invocations_preserve_committed_progress() {
    let home = tempfile::tempdir().unwrap();
    fixture(home.path()).await;
    for (i, file) in ["one", "two", "three"].iter().enumerate() {
        kill_at(home.path(), &format!("AfterHashCommit-{file}"), i == 0);
        let metadata = MetadataStore::open(&home.path().join("metadata.db")).unwrap();
        assert_eq!(metadata.get_all_indexed_files("a").unwrap().len(), i + 1);
    }
    drive(home.path(), false).await.unwrap();
    assert_consistent(home.path()).await;
}

#[test]
fn pending_identity_and_competing_writer_fail_closed() {
    let home = tempfile::tempdir().unwrap();
    let lock = WriterLock::acquire(home.path()).unwrap();
    assert!(WriterLock::acquire(home.path()).is_err());
    let mut g = Generation::open(
        home.path(),
        "a",
        "root-and-config".into(),
        Some("first"),
        None,
    )
    .unwrap()
    .unwrap();
    assert!(WriterLock::refuse_pending(home.path()).is_err());
    assert!(Generation::open(
        home.path(),
        "a",
        "changed-config".into(),
        None,
        Some("first")
    )
    .is_err());
    assert!(Generation::open(
        home.path(),
        "a",
        "root-and-config".into(),
        None,
        Some("other")
    )
    .is_err());
    assert!(Generation::open(
        home.path(),
        "b",
        "root-and-config".into(),
        Some("other"),
        None
    )
    .is_err());
    assert!(g.complete().is_err(), "ResetPending cannot complete");
    drop(lock);
    assert!(WriterLock::acquire(home.path()).is_ok());
}

#[test]
fn ordinary_writers_can_coexist_but_fence_generation_start() {
    let home = tempfile::tempdir().unwrap();
    let a = WriterLock::ordinary(home.path()).unwrap();
    let b = WriterLock::ordinary(home.path()).unwrap();
    assert!(WriterLock::acquire(home.path()).is_err());
    drop((a, b));
    assert!(WriterLock::acquire(home.path()).is_ok());
}
