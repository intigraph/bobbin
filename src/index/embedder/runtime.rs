use std::path::Path;

/// Probe well-known locations for libonnxruntime.so and set ORT_DYLIB_PATH
/// so the `load-dynamic` ort crate picks it up. When `prefer_gpu` is true,
/// prefers a CUDA-enabled build; otherwise accepts any build (CPU or GPU).
///
/// Safe to call multiple times — no-ops if ORT_DYLIB_PATH is already set
/// to a valid library.
pub(crate) fn auto_resolve_ort_dylib(prefer_gpu: bool) {
    // If ORT_DYLIB_PATH is already set and points to an existing file, respect it.
    // Exception: if GPU is requested and the existing path is CPU-only, override.
    if let Ok(existing) = std::env::var("ORT_DYLIB_PATH") {
        if Path::new(&existing).exists() {
            if !prefer_gpu {
                return; // Any existing lib is fine for CPU mode.
            }
            let existing_dir = Path::new(&existing).parent().unwrap_or(Path::new(""));
            if existing_dir
                .join("libonnxruntime_providers_cuda.so")
                .exists()
            {
                return; // GPU-capable, all good.
            }
            // Existing path is CPU-only but GPU requested — try to find GPU build below.
        }
    }

    // Search paths ordered by specificity: GPU-specific dirs first, then general.
    const SEARCH_PATHS: &[&str] = &[
        "/usr/local/lib/onnxruntime-gpu/libonnxruntime.so",
        "/opt/onnxruntime-gpu/lib/libonnxruntime.so",
        "/usr/local/lib/libonnxruntime.so",
        "/usr/lib/libonnxruntime.so",
        "/usr/lib/x86_64-linux-gnu/libonnxruntime.so",
    ];

    let mut best_cpu: Option<&str> = None;

    for candidate in SEARCH_PATHS {
        let p = Path::new(candidate);
        if !p.exists() {
            continue;
        }
        let dir = p.parent().unwrap();
        let has_cuda = dir.join("libonnxruntime_providers_cuda.so").exists();

        if prefer_gpu && has_cuda {
            set_ort_env(candidate, dir);
            tracing::debug!("auto-detected GPU ONNX Runtime");
            return;
        }
        if best_cpu.is_none() {
            best_cpu = Some(candidate);
        }
    }

    // Fallback: use the first CPU-capable library found.
    if let Some(cpu_path) = best_cpu {
        let dir = Path::new(cpu_path).parent().unwrap();
        set_ort_env(cpu_path, dir);
        if prefer_gpu {
            eprintln!("warning: GPU requested but no CUDA ONNX Runtime found, using CPU");
        } else {
            tracing::debug!("auto-detected CPU ONNX Runtime");
        }
    }
}

/// Set ORT_DYLIB_PATH and extend LD_LIBRARY_PATH for a given library.
fn set_ort_env(lib_path: &str, dir: &Path) {
    // SAFETY: called before any ort Session is created.
    unsafe {
        std::env::set_var("ORT_DYLIB_PATH", lib_path);
    }
    let mut ld_path = std::env::var("LD_LIBRARY_PATH").unwrap_or_default();
    let dir_str = dir.to_string_lossy();
    if !ld_path.contains(dir_str.as_ref()) {
        if !ld_path.is_empty() {
            ld_path.push(':');
        }
        ld_path.push_str(&dir_str);
        for cuda_dir in &["/usr/local/cuda/lib64", "/usr/local/cuda-12/lib64"] {
            if Path::new(cuda_dir).exists() && !ld_path.contains(cuda_dir) {
                ld_path.push(':');
                ld_path.push_str(cuda_dir);
            }
        }
        unsafe {
            std::env::set_var("LD_LIBRARY_PATH", &ld_path);
        }
    }
}

/// Legacy alias for backward compatibility.
pub(crate) fn auto_resolve_gpu_dylib() {
    auto_resolve_ort_dylib(true);
}
