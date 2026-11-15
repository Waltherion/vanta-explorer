use std::{env, fs, path::PathBuf, process::Command};

/// Compile the GLSL in shaders/ to SPIR-V with glslc (shaderc package). The .spv lands in
/// OUT_DIR and is pulled in with include_bytes!.
fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let shader_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("shaders");

    println!("cargo:rerun-if-changed=shaders");
    version_info();

    for entry in fs::read_dir(&shader_dir).expect("read shaders/") {
        let path = entry.expect("dir entry").path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !matches!(ext, "vert" | "frag" | "comp") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("shader name");
        let out = out_dir.join(format!("{name}.spv"));

        let status = Command::new("glslc")
            .arg("-O")
            .arg(&path)
            .arg("-o")
            .arg(&out)
            .status()
            .expect("failed to run glslc, install the `shaderc` package");

        assert!(status.success(), "glslc failed on {}", path.display());
    }
}

/// Which commit this is and when it was built, for the F1 window. Without git (a source
/// tarball), "unknown". Built again when the checked-out commit changes.
fn version_info() {
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let commit = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let commit_time = git(&["log", "-1", "--format=%ct"]).unwrap_or_else(|| "0".into());
    let built = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    println!("cargo:rustc-env=VANTA_EXPLORER_COMMIT={commit}");
    println!("cargo:rustc-env=VANTA_EXPLORER_COMMIT_TIME={commit_time}");
    println!("cargo:rustc-env=VANTA_EXPLORER_BUILT={built}");
    if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        println!("cargo:rerun-if-changed={dir}/HEAD");
        if let Some(head) = git(&["symbolic-ref", "-q", "HEAD"]) {
            println!("cargo:rerun-if-changed={dir}/{head}");
        }
        println!("cargo:rerun-if-changed={dir}/packed-refs");
    }
}
