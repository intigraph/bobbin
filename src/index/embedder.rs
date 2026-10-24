use anyhow::{Context, Result};
use ort::ep::{ExecutionProvider, CUDA};
use ort::session::Session;
use ort::value::Tensor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokenizers::Tokenizer;

use std::time::Instant;

use crate::config::{EmbeddingBackend, EmbeddingConfig};

mod tokenize;

mod runtime;
pub(crate) use runtime::{auto_resolve_gpu_dylib, auto_resolve_ort_dylib};

/// Sub-phase timing breakdown for embedding operations.
#[derive(Default, Debug, Clone)]
pub struct EmbedTiming {
    pub tokenize_ms: u128,
    pub inference_ms: u128,
    pub pooling_ms: u128,
}

// ── Built-in model registry ──────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct ModelConfig {
    pub name: String,
    pub repo: String,
    pub dim: usize,
    pub max_seq: usize,
    pub onnx_path: String,
}

impl ModelConfig {
    pub fn get(name: &str) -> Result<Self> {
        match name {
            "all-MiniLM-L6-v2" => Ok(Self {
                name: name.to_string(),
                repo: "Xenova/all-MiniLM-L6-v2".to_string(),
                dim: 384,
                max_seq: 256,
                onnx_path: "onnx/model.onnx".to_string(),
            }),
            "bge-small-en-v1.5" => Ok(Self {
                name: name.to_string(),
                repo: "Xenova/bge-small-en-v1.5".to_string(),
                dim: 384,
                max_seq: 512,
                onnx_path: "onnx/model.onnx".to_string(),
            }),
            "gte-small" => Ok(Self {
                name: name.to_string(),
                repo: "Xenova/gte-small".to_string(),
                dim: 384,
                max_seq: 512,
                onnx_path: "onnx/model.onnx".to_string(),
            }),
            _ => anyhow::bail!(
                "Unknown built-in model: {}. Built-in models: all-MiniLM-L6-v2, bge-small-en-v1.5, gte-small. \
                 For custom ONNX models, set [embedding.custom_model] in config.",
                name
            ),
        }
    }

    /// Check if a model name refers to a built-in model
    pub fn is_builtin(name: &str) -> bool {
        matches!(name, "all-MiniLM-L6-v2" | "bge-small-en-v1.5" | "gte-small")
    }
}

// ── Embedder (unified facade) ────────────────────────────────────────

/// Hard ceiling on texts handed to the model in ONE inference call.
///
/// This is a MEMORY bound, not a throughput tuning knob. One `embed_batch(N)`
/// builds N sequences' worth of ONNX Runtime internal attention activations, and
/// ORT's arena allocator never returns that memory to the OS — so peak RSS is
/// set by the largest single batch the process ever ran, permanently, and no
/// cgroup limit stays correct as the corpus grows.
///
/// Measured on this model class (all-MiniLM, seq 256, single-threaded): roughly
/// 7.4 MB retained PER TEXT, linear in batch size and never reclaimed. 2048
/// texts in one call cost about +15 GB that later small calls did not shrink;
/// the same 2048 in chunks of 32 cost about +492 MB and plateaued flat.
///
/// It is enforced HERE rather than at the call sites because the call sites keep
/// getting it wrong: the commit, bead and archive paths each collected an
/// entire corpus into one `embed_batch`, and the commit one even carried the
/// comment "Embed commit messages in batches" while doing the opposite. Bounding
/// the chokepoint makes every present and future caller safe by construction.
const MAX_EMBED_BATCH: usize = 32;

/// Generates embeddings using either local ONNX models or OpenAI-compatible APIs.
#[derive(Clone)]
pub struct Embedder {
    backend: EmbedderBackend,
}

#[derive(Clone)]
enum EmbedderBackend {
    Onnx(Arc<OnnxEmbedder>),
    Api(Arc<ApiEmbedder>),
}

impl Embedder {
    /// Load an ONNX embedding model from the cache directory (backward-compatible)
    pub fn load(cache_dir: &Path, model_name: &str) -> Result<Self> {
        let onnx = OnnxEmbedder::load_builtin(cache_dir, model_name, false)?;
        Ok(Self {
            backend: EmbedderBackend::Onnx(Arc::new(onnx)),
        })
    }

    /// Create an embedder from full embedding config
    pub fn from_config(config: &EmbeddingConfig, cache_dir: &Path) -> Result<Self> {
        let use_gpu = config.use_gpu();
        match config.backend {
            EmbeddingBackend::Onnx => {
                let onnx = if let Some(ref custom) = config.custom_model {
                    OnnxEmbedder::load_custom(
                        Path::new(&custom.model_path),
                        Path::new(&custom.tokenizer_path),
                        config.dimensions.unwrap_or(384),
                        custom.max_seq_len.unwrap_or(512),
                        use_gpu,
                    )?
                } else {
                    OnnxEmbedder::load_builtin(cache_dir, &config.model, use_gpu)?
                };
                Ok(Self {
                    backend: EmbedderBackend::Onnx(Arc::new(onnx)),
                })
            }
            EmbeddingBackend::OpenaiApi => {
                let api_config = config
                    .api
                    .as_ref()
                    .context("OpenAI API config required when backend = \"openai-api\". Set [embedding.api] in config.")?;
                let dimensions = config.dimensions.context(
                    "Embedding dimensions required for API backend. Set embedding.dimensions in config.",
                )?;
                let api = ApiEmbedder::new(
                    api_config.url.clone(),
                    api_config.resolve_api_key(),
                    config.model.clone(),
                    dimensions,
                );
                Ok(Self {
                    backend: EmbedderBackend::Api(Arc::new(api)),
                })
            }
        }
    }

    /// Generate embeddings for a batch of texts
    pub async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(MAX_EMBED_BATCH) {
            let vecs = match &self.backend {
                EmbedderBackend::Onnx(onnx) => onnx.embed_batch(chunk).map(|(vecs, _)| vecs)?,
                EmbedderBackend::Api(api) => api.embed_batch(chunk).await?,
            };
            out.extend(vecs);
        }
        Ok(out)
    }

    /// Generate embeddings with sub-phase timing breakdown
    pub async fn embed_batch_timed(&self, texts: &[&str]) -> Result<(Vec<Vec<f32>>, EmbedTiming)> {
        let mut out = Vec::with_capacity(texts.len());
        let mut total = EmbedTiming::default();
        for chunk in texts.chunks(MAX_EMBED_BATCH) {
            let (vecs, t) = match &self.backend {
                EmbedderBackend::Onnx(onnx) => onnx.embed_batch(chunk)?,
                EmbedderBackend::Api(api) => {
                    (api.embed_batch(chunk).await?, EmbedTiming::default())
                }
            };
            // Sub-phase timings ACCUMULATE across chunks, so the reported total
            // still covers the whole request rather than just its last slice.
            total.tokenize_ms += t.tokenize_ms;
            total.inference_ms += t.inference_ms;
            total.pooling_ms += t.pooling_ms;
            out.extend(vecs);
        }
        Ok((out, total))
    }

    /// Generate embedding for a single text
    pub async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.embed_batch(&[text])
            .await?
            .into_iter()
            .next()
            .context("embed_batch returned empty results for single input")
    }

    /// Synchronous embed for backward compatibility (ONNX only)
    pub fn embed_sync(&self, text: &str) -> Result<Vec<f32>> {
        match &self.backend {
            EmbedderBackend::Onnx(onnx) => onnx
                .embed_batch(&[text])
                .map(|(vecs, _)| vecs)?
                .into_iter()
                .next()
                .context("embed_batch returned empty results for single input"),
            EmbedderBackend::Api(_) => {
                anyhow::bail!("Synchronous embed not supported for API backend; use embed().await")
            }
        }
    }

    /// Synchronous embed_batch for backward compatibility (ONNX only)
    pub fn embed_batch_sync(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        match &self.backend {
            EmbedderBackend::Onnx(onnx) => {
                // Bounded for the same reason as `embed_batch` — see MAX_EMBED_BATCH.
                let mut out = Vec::with_capacity(texts.len());
                for chunk in texts.chunks(MAX_EMBED_BATCH) {
                    out.extend(onnx.embed_batch(chunk).map(|(vecs, _)| vecs)?);
                }
                Ok(out)
            }
            EmbedderBackend::Api(_) => {
                anyhow::bail!(
                    "Synchronous embed_batch not supported for API backend; use embed_batch().await"
                )
            }
        }
    }

    /// Get the embedding dimension
    pub fn dimension(&self) -> usize {
        match &self.backend {
            EmbedderBackend::Onnx(onnx) => onnx.dim,
            EmbedderBackend::Api(api) => api.dimensions,
        }
    }

    /// Maximum input sequence length (tokens) the model accepts before it
    /// silently truncates. `None` for API backends whose window we don't track.
    /// Used by the chunker to keep chunks within the embedder window.
    pub fn max_seq(&self) -> Option<usize> {
        match &self.backend {
            EmbedderBackend::Onnx(onnx) => Some(onnx.max_seq),
            EmbedderBackend::Api(_) => None,
        }
    }

    /// Get the model name
    pub fn model_name(&self) -> &str {
        match &self.backend {
            EmbedderBackend::Onnx(onnx) => &onnx.model_name,
            EmbedderBackend::Api(api) => &api.model,
        }
    }

    /// Get the backend type as a string
    pub fn backend_type(&self) -> &'static str {
        match &self.backend {
            EmbedderBackend::Onnx(_) => "onnx",
            EmbedderBackend::Api(_) => "openai-api",
        }
    }

    /// Returns true if this embedder is using GPU acceleration
    pub fn is_gpu(&self) -> bool {
        match &self.backend {
            EmbedderBackend::Onnx(onnx) => onnx.gpu_active,
            EmbedderBackend::Api(_) => false,
        }
    }
}

/// Thread-safe wrapper for the embedder
pub type SharedEmbedder = Arc<Embedder>;

// ── ONNX Backend ─────────────────────────────────────────────────────

struct OnnxEmbedder {
    session: Mutex<Session>,
    tokenizer: Tokenizer,
    model_name: String,
    dim: usize,
    max_seq: usize,
    gpu_active: bool,
}

impl OnnxEmbedder {
    /// Load a built-in model from the cache directory
    fn load_builtin(cache_dir: &Path, model_name: &str, use_gpu: bool) -> Result<Self> {
        let config = ModelConfig::get(model_name)?;
        let model_dir = cache_dir.join(&config.name);
        let model_path = model_dir.join(&config.onnx_path);
        let tokenizer_path = model_dir.join("tokenizer.json");

        if !model_path.exists() || !tokenizer_path.exists() {
            anyhow::bail!(
                "Model {} not found. Run `bobbin init` or `bobbin index` to download it.",
                model_name
            );
        }

        Self::load_from_files(
            &model_path,
            &tokenizer_path,
            config.dim,
            config.max_seq,
            &config.name,
            use_gpu,
        )
    }

    /// Load a custom ONNX model from specified paths
    fn load_custom(
        model_path: &Path,
        tokenizer_path: &Path,
        dim: usize,
        max_seq: usize,
        use_gpu: bool,
    ) -> Result<Self> {
        if !model_path.exists() {
            anyhow::bail!("Custom ONNX model not found at: {}", model_path.display());
        }
        if !tokenizer_path.exists() {
            anyhow::bail!("Tokenizer not found at: {}", tokenizer_path.display());
        }

        let name = model_path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "custom".to_string());

        Self::load_from_files(model_path, tokenizer_path, dim, max_seq, &name, use_gpu)
    }

    fn load_from_files(
        model_path: &Path,
        tokenizer_path: &Path,
        dim: usize,
        max_seq: usize,
        name: &str,
        use_gpu: bool,
    ) -> Result<Self> {
        // Use BOBBIN_ONNX_THREADS env var, or default to half available cores (min 2)
        let num_threads: usize = std::env::var("BOBBIN_ONNX_THREADS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| {
                let cpus = std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(4);
                (cpus / 2).max(2)
            });

        // Auto-resolve ONNX Runtime library at well-known system paths.
        // The `load-dynamic` ort feature reads ORT_DYLIB_PATH at first use.
        auto_resolve_ort_dylib(use_gpu);
        crate::runtime_check::validate(model_path)?;

        let mut builder = Session::builder().map_err(|e| {
            let hint = if std::env::var("ORT_DYLIB_PATH").is_err() {
                "\n\nHint: ONNX Runtime library not found. Install it:\n  \
                     apt install libonnxruntime-dev   # Debian/Ubuntu\n  \
                     brew install onnxruntime          # macOS\n  \
                     pip install onnxruntime           # Python (sets up shared lib)\n\n\
                     Or set ORT_DYLIB_PATH=/path/to/libonnxruntime.so"
            } else {
                ""
            };
            anyhow::anyhow!("Failed to create ONNX session builder: {}{}", e, hint)
        })?;

        if use_gpu {
            crate::gpu_runtime::wait_for_hold();
        }

        // Register CUDA execution provider if GPU requested.
        // with_execution_providers silently falls back to CPU if CUDA is unavailable.
        let gpu_active = if use_gpu {
            builder = builder
                .with_execution_providers([CUDA::default().build().error_on_failure()])
                .map_err(|e| anyhow::anyhow!("Failed to register CUDA EP: {}", e))?;
            // Check if CUDA EP was actually registered (lib is now loaded)
            CUDA::default().is_available().unwrap_or(false)
        } else {
            false
        };

        let session = builder
            .with_intra_threads(num_threads)
            .map_err(|e| anyhow::anyhow!("Failed to set thread count: {}", e))?
            .commit_from_file(model_path)
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to load ONNX model from {}: {}",
                    model_path.file_name().unwrap_or_default().to_string_lossy(),
                    e
                )
            })?;

        if use_gpu {
            if gpu_active {
                eprintln!("ONNX session using CUDA GPU acceleration");
            } else {
                eprintln!(
                    "warning: GPU requested but CUDA execution provider not available, falling back to CPU"
                );
            }
        }

        let tokenizer = Tokenizer::from_file(tokenizer_path)
            .map_err(|e| anyhow::anyhow!("Failed to load tokenizer: {}", e))?;

        Ok(Self {
            session: Mutex::new(session),
            tokenizer,
            model_name: name.to_string(),
            dim,
            max_seq,
            gpu_active,
        })
    }

    fn embed_batch(&self, texts: &[&str]) -> Result<(Vec<Vec<f32>>, EmbedTiming)> {
        let mut timing = EmbedTiming::default();

        if texts.is_empty() {
            return Ok((vec![], timing));
        }

        let t_tok = Instant::now();
        let (input_ids, attention_mask, token_type_ids) =
            tokenize::model_inputs(&self.tokenizer, texts, self.max_seq)?;
        let (batch_size, max_len) = input_ids.dim();

        // Keep a flat copy of attention mask for mean pooling
        let attention_mask_vec: Vec<i64> = attention_mask.iter().cloned().collect();

        let input_ids_tensor = Tensor::from_array(input_ids)
            .map_err(|e| anyhow::anyhow!("Failed to create input_ids tensor: {}", e))?;
        let attention_mask_tensor = Tensor::from_array(attention_mask)
            .map_err(|e| anyhow::anyhow!("Failed to create attention_mask tensor: {}", e))?;
        let token_type_ids_tensor = Tensor::from_array(token_type_ids)
            .map_err(|e| anyhow::anyhow!("Failed to create token_type_ids tensor: {}", e))?;
        timing.tokenize_ms = t_tok.elapsed().as_millis();

        if self.gpu_active {
            crate::gpu_runtime::wait_for_hold();
        }
        let t_inf = Instant::now();
        let mut session = self
            .session
            .lock()
            .map_err(|e| anyhow::anyhow!("Session lock poisoned: {}", e))?;
        let outputs = session
            .run(ort::inputs![
                "input_ids" => input_ids_tensor,
                "attention_mask" => attention_mask_tensor,
                "token_type_ids" => token_type_ids_tensor,
            ])
            .map_err(|e| anyhow::anyhow!("ONNX inference failed: {}", e))?;
        timing.inference_ms = t_inf.elapsed().as_millis();

        let t_pool = Instant::now();
        // Get the last hidden state (batch_size, seq_len, hidden_dim)
        let (shape, data) = outputs["last_hidden_state"]
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow::anyhow!("Failed to extract output tensor: {}", e))?;

        let shape_dims: Vec<usize> = shape.iter().map(|&x| x as usize).collect();
        let seq_len = shape_dims[1];
        let hidden_dim = shape_dims[2];

        // Mean pooling with attention mask
        let mut embeddings = Vec::with_capacity(batch_size);

        for i in 0..batch_size {
            let mut sum = vec![0.0f32; hidden_dim];
            let mut count = 0.0f32;

            for j in 0..max_len.min(seq_len) {
                if attention_mask_vec[i * max_len + j] == 1 {
                    let offset = (i * seq_len + j) * hidden_dim;
                    for k in 0..hidden_dim {
                        sum[k] += data[offset + k];
                    }
                    count += 1.0;
                }
            }

            // Average and normalize
            if count > 0.0 {
                for v in &mut sum {
                    *v /= count;
                }
            }

            // L2 normalize
            let norm: f32 = sum.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 0.0 {
                for v in &mut sum {
                    *v /= norm;
                }
            }

            embeddings.push(sum);
        }
        timing.pooling_ms = t_pool.elapsed().as_millis();

        Ok((embeddings, timing))
    }
}

// ── OpenAI-compatible API Backend ────────────────────────────────────

struct ApiEmbedder {
    url: String,
    api_key: Option<String>,
    model: String,
    dimensions: usize,
    client: reqwest::Client,
}

impl ApiEmbedder {
    fn new(url: String, api_key: Option<String>, model: String, dimensions: usize) -> Self {
        Self {
            url,
            api_key,
            model,
            dimensions,
            client: reqwest::Client::new(),
        }
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }

        let input: Vec<String> = texts.iter().map(|t| t.to_string()).collect();

        let body = serde_json::json!({
            "model": self.model,
            "input": input,
        });

        let mut request = self.client.post(&self.url).json(&body);

        if let Some(ref key) = self.api_key {
            request = request.bearer_auth(key);
        }

        let response = request
            .send()
            .await
            .with_context(|| format!("Failed to call embedding API at {}", self.url))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "Embedding API returned HTTP {}: {}",
                status,
                body.chars().take(500).collect::<String>()
            );
        }

        let resp: EmbeddingApiResponse = response
            .json()
            .await
            .context("Failed to parse embedding API response")?;

        // Sort by index to maintain input order
        let mut data = resp.data;
        data.sort_by_key(|d| d.index);

        let embeddings: Vec<Vec<f32>> = data.into_iter().map(|d| d.embedding).collect();

        // Validate dimensions
        if let Some(first) = embeddings.first() {
            if first.len() != self.dimensions {
                anyhow::bail!(
                    "API returned embeddings with dimension {} but config specifies {}",
                    first.len(),
                    self.dimensions
                );
            }
        }

        Ok(embeddings)
    }
}

#[derive(serde::Deserialize)]
struct EmbeddingApiResponse {
    data: Vec<EmbeddingData>,
}

#[derive(serde::Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
    index: usize,
}

// ── Dimension resolution ─────────────────────────────────────────────

/// Resolve the embedding dimension from the config
pub fn resolve_dimension(config: &EmbeddingConfig) -> Result<usize> {
    // Explicit dimensions always win
    if let Some(dim) = config.dimensions {
        return Ok(dim);
    }

    match config.backend {
        EmbeddingBackend::Onnx => {
            if config.custom_model.is_some() {
                anyhow::bail!(
                    "Embedding dimensions must be specified for custom ONNX models. \
                     Set embedding.dimensions in config."
                );
            }
            // Built-in model: look up the dimension
            let model_config = ModelConfig::get(&config.model)?;
            Ok(model_config.dim)
        }
        EmbeddingBackend::OpenaiApi => {
            anyhow::bail!(
                "Embedding dimensions must be specified for API backend. \
                 Set embedding.dimensions in config."
            );
        }
    }
}

// ── Model download ───────────────────────────────────────────────────

/// Download the embedding model if not present
pub async fn ensure_model(cache_dir: &Path, model_name: &str) -> Result<PathBuf> {
    // Only download for built-in models
    if !ModelConfig::is_builtin(model_name) {
        return Ok(cache_dir.to_path_buf());
    }

    let config = ModelConfig::get(model_name)?;
    let model_dir = cache_dir.join(&config.name);
    let model_path = model_dir.join(&config.onnx_path);
    let tokenizer_path = model_dir.join("tokenizer.json");

    if model_path.exists() && tokenizer_path.exists() {
        return Ok(model_dir);
    }

    std::fs::create_dir_all(&model_dir)
        .with_context(|| format!("Failed to create model directory: {}", model_dir.display()))?;

    if let Some(parent) = model_path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create model parent directory: {}",
                    parent.display()
                )
            })?;
        }
    }

    eprintln!("Downloading embedding model {}...", model_name);

    // Download model.onnx from HuggingFace
    let model_url = format!(
        "https://huggingface.co/{}/resolve/main/{}",
        config.repo, config.onnx_path
    );
    download_file(&model_url, &model_path).await?;

    // Download tokenizer.json
    let tokenizer_url = format!(
        "https://huggingface.co/{}/resolve/main/tokenizer.json",
        config.repo
    );
    download_file(&tokenizer_url, &tokenizer_path).await?;

    eprintln!("Model downloaded successfully to {}", model_dir.display());
    Ok(model_dir)
}

/// Check if the configured embedding needs model download (ONNX built-in only)
pub async fn ensure_model_for_config(
    cache_dir: &Path,
    config: &EmbeddingConfig,
) -> Result<PathBuf> {
    match config.backend {
        EmbeddingBackend::Onnx => {
            if config.custom_model.is_some() {
                // Custom model: no download needed, just verify paths
                Ok(cache_dir.to_path_buf())
            } else {
                ensure_model(cache_dir, &config.model).await
            }
        }
        EmbeddingBackend::OpenaiApi => {
            // API backend: no local model needed
            Ok(cache_dir.to_path_buf())
        }
    }
}

/// List all built-in model names
pub fn builtin_model_names() -> &'static [&'static str] {
    &["all-MiniLM-L6-v2", "bge-small-en-v1.5", "gte-small"]
}

/// Download a file from a URL to a local path
async fn download_file(url: &str, path: &Path) -> Result<()> {
    use tokio::io::AsyncWriteExt;

    let response = reqwest::get(url)
        .await
        .with_context(|| format!("Failed to download {}", url))?;

    if !response.status().is_success() {
        anyhow::bail!("Failed to download {}: HTTP {}", url, response.status());
    }

    let bytes = response
        .bytes()
        .await
        .with_context(|| format!("Failed to read response from {}", url))?;

    let mut file = tokio::fs::File::create(path)
        .await
        .with_context(|| format!("Failed to create file {}", path.display()))?;

    file.write_all(&bytes)
        .await
        .with_context(|| format!("Failed to write to {}", path.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ApiEmbeddingConfig, CustomModelConfig};

    #[test]
    fn test_model_config_builtin_models() {
        let minilm = ModelConfig::get("all-MiniLM-L6-v2").unwrap();
        assert_eq!(minilm.dim, 384);
        assert_eq!(minilm.max_seq, 256);

        let bge = ModelConfig::get("bge-small-en-v1.5").unwrap();
        assert_eq!(bge.dim, 384);
        assert_eq!(bge.max_seq, 512);

        let gte = ModelConfig::get("gte-small").unwrap();
        assert_eq!(gte.dim, 384);
        assert_eq!(gte.max_seq, 512);
    }

    #[test]
    fn test_model_config_unknown_model() {
        assert!(ModelConfig::get("nonexistent-model").is_err());
    }

    #[test]
    fn test_model_config_is_builtin() {
        assert!(ModelConfig::is_builtin("all-MiniLM-L6-v2"));
        assert!(ModelConfig::is_builtin("bge-small-en-v1.5"));
        assert!(ModelConfig::is_builtin("gte-small"));
        assert!(!ModelConfig::is_builtin("custom-model"));
        assert!(!ModelConfig::is_builtin(""));
    }

    #[test]
    fn test_builtin_model_names() {
        let names = builtin_model_names();
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"all-MiniLM-L6-v2"));
        assert!(names.contains(&"bge-small-en-v1.5"));
        assert!(names.contains(&"gte-small"));
    }

    #[test]
    fn test_resolve_dimension_builtin() {
        let config = EmbeddingConfig::default();
        assert_eq!(resolve_dimension(&config).unwrap(), 384);
    }

    #[test]
    fn test_resolve_dimension_explicit() {
        let config = EmbeddingConfig {
            dimensions: Some(768),
            ..Default::default()
        };
        assert_eq!(resolve_dimension(&config).unwrap(), 768);
    }

    #[test]
    fn test_resolve_dimension_api_requires_explicit() {
        let config = EmbeddingConfig {
            backend: EmbeddingBackend::OpenaiApi,
            model: "test".to_string(),
            dimensions: None,
            api: Some(ApiEmbeddingConfig {
                url: "http://localhost:8080".to_string(),
                api_key: None,
            }),
            ..Default::default()
        };
        assert!(resolve_dimension(&config).is_err());
    }

    #[test]
    fn test_resolve_dimension_custom_onnx_requires_explicit() {
        let config = EmbeddingConfig {
            model: "custom".to_string(),
            dimensions: None,
            custom_model: Some(CustomModelConfig {
                model_path: "/path/to/model.onnx".to_string(),
                tokenizer_path: "/path/to/tokenizer.json".to_string(),
                max_seq_len: None,
            }),
            ..Default::default()
        };
        assert!(resolve_dimension(&config).is_err());
    }

    #[test]
    fn test_resolve_dimension_api_with_explicit() {
        let config = EmbeddingConfig {
            backend: EmbeddingBackend::OpenaiApi,
            model: "test".to_string(),
            dimensions: Some(1536),
            api: Some(ApiEmbeddingConfig {
                url: "http://localhost:8080".to_string(),
                api_key: None,
            }),
            ..Default::default()
        };
        assert_eq!(resolve_dimension(&config).unwrap(), 1536);
    }

    #[test]
    fn test_embedder_from_config_api_missing_config() {
        let config = EmbeddingConfig {
            backend: EmbeddingBackend::OpenaiApi,
            model: "test".to_string(),
            dimensions: Some(768),
            api: None,
            ..Default::default()
        };
        let cache_dir = std::path::PathBuf::from("/tmp/nonexistent");
        assert!(Embedder::from_config(&config, &cache_dir).is_err());
    }

    #[test]
    fn test_embedder_from_config_api_missing_dimensions() {
        let config = EmbeddingConfig {
            backend: EmbeddingBackend::OpenaiApi,
            model: "test".to_string(),
            dimensions: None,
            api: Some(ApiEmbeddingConfig {
                url: "http://localhost:8080".to_string(),
                api_key: None,
            }),
            ..Default::default()
        };
        let cache_dir = std::path::PathBuf::from("/tmp/nonexistent");
        assert!(Embedder::from_config(&config, &cache_dir).is_err());
    }

    #[test]
    fn test_embedder_from_config_api_success() {
        let config = EmbeddingConfig {
            backend: EmbeddingBackend::OpenaiApi,
            model: "test-model".to_string(),
            dimensions: Some(768),
            api: Some(ApiEmbeddingConfig {
                url: "http://localhost:8080/v1/embeddings".to_string(),
                api_key: Some("test-key".to_string()),
            }),
            ..Default::default()
        };
        let cache_dir = std::path::PathBuf::from("/tmp/nonexistent");
        let embedder = Embedder::from_config(&config, &cache_dir).unwrap();
        assert_eq!(embedder.dimension(), 768);
        assert_eq!(embedder.model_name(), "test-model");
        assert_eq!(embedder.backend_type(), "openai-api");
    }

    /// The chokepoint bound, proven against a real backend rather than asserted
    /// about a constant.
    ///
    /// A mock OpenAI-compatible endpoint records the size of every batch the
    /// embedder actually sends. The commit / bead / archive index paths each
    /// hand `embed_batch` an entire corpus in one call, so if the slicing here
    /// regresses, the model receives thousands of sequences at once — and since
    /// ORT's arena never returns memory, that pins peak RSS for the life of the
    /// process and OOM-kills the nightly reindex. Measured cost of getting this
    /// wrong: roughly 7.4 MB retained per text, linear, unreclaimed.
    #[tokio::test]
    async fn embed_batch_slices_oversized_input_to_max_embed_batch() {
        use std::sync::{Arc, Mutex};

        const DIM: usize = 768;
        const TOTAL: usize = 3022; // the real aegis commit count that OOM'd

        let seen: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_srv = seen.clone();

        let app = axum::Router::new().route(
            "/v1/embeddings",
            axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                let seen = seen_srv.clone();
                async move {
                    let n = body["input"].as_array().map_or(0, Vec::len);
                    seen.lock().unwrap().push(n);
                    let data: Vec<serde_json::Value> = (0..n)
                        .map(|i| serde_json::json!({"index": i, "embedding": vec![0.1f32; DIM]}))
                        .collect();
                    axum::Json(serde_json::json!({"data": data}))
                }
            }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        let config = EmbeddingConfig {
            backend: EmbeddingBackend::OpenaiApi,
            model: "mock".to_string(),
            dimensions: Some(DIM),
            api: Some(ApiEmbeddingConfig {
                url: format!("http://{addr}/v1/embeddings"),
                api_key: None,
            }),
            ..Default::default()
        };
        let embedder =
            Embedder::from_config(&config, &std::path::PathBuf::from("/tmp/nonexistent")).unwrap();

        let texts: Vec<String> = (0..TOTAL).map(|i| format!("commit message {i}")).collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let out = embedder
            .embed_batch(&refs)
            .await
            .expect("embed should succeed");

        let batches = seen.lock().unwrap().clone();
        let largest = batches.iter().copied().max().unwrap_or(0);

        assert!(
            largest <= MAX_EMBED_BATCH,
            "embed_batch sent a batch of {largest} to the backend against a bound of \
             {MAX_EMBED_BATCH} ({} calls) — an entire corpus reached the model in one \
             inference, which is the nightly-reindex OOM",
            batches.len()
        );
        // Chunking must not lose, duplicate, or reorder work.
        assert_eq!(
            batches.iter().sum::<usize>(),
            TOTAL,
            "texts dropped or duplicated"
        );
        assert_eq!(
            out.len(),
            TOTAL,
            "returned vector count must match input count"
        );
    }
}
