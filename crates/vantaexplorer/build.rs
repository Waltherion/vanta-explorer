use std::{env, fs, path::PathBuf, process::Command};

/// Compile the GLSL in shaders/ to SPIR-V with glslc (shaderc package). The .spv lands in
/// OUT_DIR and is pulled in with include_bytes!.
fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let shader_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("shaders");

    println!("cargo:rerun-if-changed=shaders");

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
