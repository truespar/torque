use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Target GPU architectures:
///   sm_80  = A100 (Ampere data center)
///   sm_86  = RTX A6000 / RTX 3090 (Ampere consumer/pro)
///   sm_89  = RTX 6000 Ada (Lovelace)
///   sm_120 = RTX PRO 6000 Blackwell
const SM_TARGETS: &[&str] = &["sm_80", "sm_86", "sm_89", "sm_120"];

fn main() {
    // Only compile CUDA kernels when the cuda feature is enabled
    if env::var("CARGO_FEATURE_CUDA").is_err() {
        return;
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let cuda_dir = Path::new("src/gpu/cuda");

    // Find nvcc
    let nvcc = find_nvcc();
    let Some(nvcc) = nvcc else {
        println!("cargo:warning=nvcc not found in PATH. CUDA kernels will be compiled at runtime via nvrtc.");
        std::fs::write(out_dir.join("cuda_kernels_compiled"), "false").unwrap();
        return;
    };

    println!("cargo:warning=Found nvcc: {}", nvcc.display());

    // Collect all .cu files
    let cu_files: Vec<PathBuf> = std::fs::read_dir(cuda_dir)
        .expect("Failed to read src/gpu/cuda/ directory")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "cu"))
        .collect();

    if cu_files.is_empty() {
        println!("cargo:warning=No .cu files found in src/gpu/cuda/");
        return;
    }

    for cu_file in &cu_files {
        println!("cargo:rerun-if-changed={}", cu_file.display());
    }

    // Concatenate all .cu sources into a single combined file
    let mut combined_source = String::new();
    for cu_file in &cu_files {
        let source = std::fs::read_to_string(cu_file)
            .unwrap_or_else(|e| panic!("Failed to read {}: {e}", cu_file.display()));
        combined_source.push_str(&source);
        combined_source.push_str("\n\n");
    }

    let combined_cu = out_dir.join("combined_kernels.cu");
    std::fs::write(&combined_cu, &combined_source).unwrap();

    // Compile for each SM target
    let mut compiled_targets = Vec::new();

    for sm in SM_TARGETS {
        let ptx_file = out_dir.join(format!("kernels_{sm}.ptx"));

        let status = Command::new(&nvcc)
            .args(["--ptx", &format!("-arch={sm}"), "--use_fast_math", "-O3", "-I"])
            .arg(cuda_dir)
            .arg("-o")
            .arg(&ptx_file)
            .arg(&combined_cu)
            .status();

        match status {
            Ok(s) if s.success() => {
                compiled_targets.push(*sm);
                println!("cargo:warning=Compiled kernels_{sm}.ptx");
            }
            Ok(s) => {
                println!(
                    "cargo:warning=nvcc failed for {sm} with exit code: {}",
                    s.code().unwrap_or(-1)
                );
            }
            Err(e) => {
                println!("cargo:warning=Failed to run nvcc for {sm}: {e}");
            }
        }
    }

    // Write marker: comma-separated list of compiled SM targets, or "false"
    let marker = if compiled_targets.is_empty() {
        "false".to_string()
    } else {
        compiled_targets.join(",")
    };
    std::fs::write(out_dir.join("cuda_kernels_compiled"), marker).unwrap();
}

fn find_nvcc() -> Option<PathBuf> {
    // Try PATH first
    if Command::new("nvcc").arg("--version").output().is_ok() {
        return Some(PathBuf::from("nvcc"));
    }

    // Try common CUDA install locations
    let candidates = [
        "/usr/local/cuda/bin/nvcc",
        "/usr/local/cuda-12/bin/nvcc",
        "/usr/local/cuda-13/bin/nvcc",
        "C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v12.6/bin/nvcc.exe",
        "C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v12.8/bin/nvcc.exe",
        "C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v13.0/bin/nvcc.exe",
    ];

    for candidate in candidates {
        let path = PathBuf::from(candidate);
        if path.exists() {
            return Some(path);
        }
    }

    // Try CUDA_PATH env
    if let Ok(cuda_path) = env::var("CUDA_PATH") {
        let nvcc_path = PathBuf::from(cuda_path).join("bin").join("nvcc");
        if nvcc_path.exists() {
            return Some(nvcc_path);
        }
    }

    None
}
