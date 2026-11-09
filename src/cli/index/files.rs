//! File discovery and vector batch insertion, shared by generation replay.
use super::*;

/// Collect all files to index based on configuration patterns
pub(crate) fn collect_files(repo_root: &Path, config: &Config) -> Result<Vec<PathBuf>> {
    collect_files_strict(repo_root, config, false)
}

pub(super) fn collect_files_strict(
    repo_root: &Path,
    config: &Config,
    strict: bool,
) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    let mut include_globs: Vec<String> = config.index.include.clone();
    if config.index.multimodal {
        // Opt-in: also walk multimodal file types (PDFs) so the extractor can
        // turn them into searchable text. Kept separate from the default
        // include list so toggling the flag is the only knob users need.
        for ext in crate::index::multimodal::MULTIMODAL_EXTENSIONS {
            include_globs.push(format!("**/*.{ext}"));
        }
    }
    if config.index.documents {
        // Same opt-in shape as multimodal: toggling the flag is the only
        // knob, no include-pattern editing required.
        for ext in crate::index::documents::DOCUMENT_EXTENSIONS {
            include_globs.push(format!("**/*.{ext}"));
        }
    }
    if strict {
        for pattern in include_globs.iter().chain(config.index.exclude.iter()) {
            glob::Pattern::new(pattern)?;
        }
    }
    let include_patterns: Vec<glob::Pattern> = include_globs
        .iter()
        .filter_map(|p| glob::Pattern::new(p).ok())
        .collect();

    let exclude_patterns: Vec<glob::Pattern> = config
        .index
        .exclude
        .iter()
        .filter_map(|p| glob::Pattern::new(p).ok())
        .collect();

    let mut builder = WalkBuilder::new(repo_root);
    builder
        .hidden(true)
        .git_ignore(config.index.use_gitignore)
        .git_global(config.index.use_gitignore)
        .git_exclude(config.index.use_gitignore);

    for entry in builder.build() {
        let entry = match entry {
            Ok(e) => e,
            Err(e) if strict => return Err(e.into()),
            Err(_) => continue,
        };

        if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(true) {
            continue;
        }

        let path = entry.path();
        let rel_path = path
            .strip_prefix(repo_root)
            .unwrap_or(path)
            .to_string_lossy();

        let excluded = exclude_patterns.iter().any(|p| p.matches(&rel_path));
        if excluded {
            continue;
        }

        let included = include_patterns.iter().any(|p| p.matches(&rel_path));

        if included {
            files.push(path.to_path_buf());
        }
    }

    Ok(files)
}

/// Process a batch of files: generate embeddings and store in LanceDB.
///
/// Collects all chunks across files into one big embedding batch to minimize
/// ONNX session overhead, then splits results back to per-file for storage.
pub(super) async fn process_batch(
    results: &mut Vec<FileIndexResult>,
    vector_store: &mut VectorStore,
    embed: &Embedder,
    repo: &str,
    profile: &mut ProfileStats,
    metadata_store: &MetadataStore,
    file_timestamps: &HashMap<String, i64>,
) -> Result<(usize, usize)> {
    if results.is_empty() {
        return Ok((0, 0));
    }

    let fallback_ts = chrono::Utc::now().timestamp();

    // Collect all embed texts across files into one batch
    let mut all_texts: Vec<String> = Vec::new();
    let mut file_chunk_counts: Vec<usize> = Vec::new();

    for result in results.iter() {
        let texts: Vec<String> = result
            .contexts
            .iter()
            .zip(result.chunks.iter())
            .map(|(ctx, chunk)| {
                ctx.as_ref()
                    .cloned()
                    .unwrap_or_else(|| chunk.content.clone())
            })
            .collect();
        file_chunk_counts.push(texts.len());
        all_texts.extend(texts);
    }

    let all_refs: Vec<&str> = all_texts.iter().map(|s| s.as_str()).collect();

    // Embed all chunks in one batch
    let t_embed = Instant::now();
    let (all_embeddings, embed_timing) = embed
        .embed_batch_timed(&all_refs)
        .await
        .context("Failed to generate embeddings")?;
    profile.embed_ms += t_embed.elapsed().as_millis();
    profile.embed_tokenize_ms += embed_timing.tokenize_ms;
    profile.embed_inference_ms += embed_timing.inference_ms;
    profile.embed_pooling_ms += embed_timing.pooling_ms;
    profile.total_chunks_embedded += all_refs.len();
    profile.total_batches += 1;

    // Accumulate all chunks, embeddings, contexts, per-chunk file hashes,
    // and per-chunk indexed_at timestamps for a single bulk Lance insert.
    let mut all_chunks: Vec<Chunk> = Vec::new();
    let mut all_contexts: Vec<Option<String>> = Vec::new();
    let mut all_hashes: Vec<String> = Vec::new();
    let mut all_indexed_at: Vec<String> = Vec::new();
    let mut batch_hashes = Vec::new();
    let mut indexed_count = 0;
    let mut chunks_count = 0;

    for (result, &chunk_count) in results.drain(..).zip(file_chunk_counts.iter()) {
        let ts = file_timestamps
            .get(&result.path)
            .copied()
            .unwrap_or(fallback_ts)
            .to_string();
        for _ in 0..chunk_count {
            all_hashes.push(result.hash.clone());
            all_indexed_at.push(ts.clone());
        }
        batch_hashes.push((result.path.clone(), result.hash.clone()));
        all_chunks.extend(result.chunks);
        all_contexts.extend(result.contexts);
        chunks_count += chunk_count;
        indexed_count += 1;
    }

    let t_ins = Instant::now();
    let hash_refs: Vec<&str> = all_hashes.iter().map(|s| s.as_str()).collect();
    let ts_refs: Vec<&str> = all_indexed_at.iter().map(|s| s.as_str()).collect();
    super::batch::persist(
        vector_store,
        metadata_store,
        repo,
        super::batch::Batch {
            chunks: &all_chunks,
            embeddings: &all_embeddings,
            contexts: &all_contexts,
            hashes: &hash_refs,
            timestamps: &ts_refs,
            file_hashes: &batch_hashes,
        },
    )
    .await?;
    profile.insert_ms += t_ins.elapsed().as_millis();

    Ok((indexed_count, chunks_count))
}
