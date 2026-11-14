//! Bind against the system libavif and libraw.
//!
//! The libavif-sys crate always builds libavif (and dav1d) from source via cmake/meson, which
//! bloats the build graph. Every distribution ships libavif, so we generate bindings against
//! the system headers instead: the package manager keeps the library patched, and
//! bindgen guarantees the struct ABI matches rather than us hand-writing externs that could
//! silently corrupt memory when libavif changes layout.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var_os("CARGO_FEATURE_RAW").is_some() {
        bind_libraw();
    }
    // Only the `avif` feature needs libavif; without it the crate has no C dependency here.
    if std::env::var_os("CARGO_FEATURE_AVIF").is_none() {
        return;
    }
    let lib = match pkg_config::Config::new().probe("libavif") {
        Ok(lib) => lib,
        Err(e) => panic!("system libavif not found ({e}); install libavif's development files (Arch: libavif, Debian/Ubuntu: libavif-dev, Fedora: libavif-devel)"),
    };

    let mut builder = bindgen::Builder::default()
        .header_contents("wrapper.h", "#include <avif/avif.h>")
        // Only what the decoder path touches, to keep the generated file small and stable.
        .allowlist_function("avifDecoder.*")
        .allowlist_function("avifRGBImage.*")
        .allowlist_function("avifImage.*")
        .allowlist_function("avifAlloc.*")
        .allowlist_function("avifFree.*")
        .allowlist_function("avifResult.*")
        .allowlist_function("avifEncoder.*")
        .allowlist_function("avifRWData.*")
        .allowlist_type("avif.*")
        .allowlist_var("AVIF_.*")
        .derive_default(true)
        .layout_tests(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));

    for path in &lib.include_paths {
        builder = builder.clang_arg(format!("-I{}", path.display()));
    }

    let bindings = builder.generate().expect("generate libavif bindings");
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out.join("avif.rs"))
        .expect("write bindings");
}

/// libraw reads what a camera actually recorded. Its C API is small and stable, but the
/// structs it hands back are not, so bindgen rather than hand-written externs.
fn bind_libraw() {
    let lib = match pkg_config::Config::new().probe("libraw") {
        Ok(lib) => lib,
        Err(e) => panic!("system libraw not found ({e}); install libraw's development files (Arch: libraw, Debian/Ubuntu: libraw-dev, Fedora: LibRaw-devel)"),
    };
    let mut builder = bindgen::Builder::default()
        .header_contents("wrapper.h", "#include <libraw/libraw.h>")
        .allowlist_function("libraw_.*")
        .allowlist_type("libraw_.*")
        .allowlist_var("LIBRAW_.*")
        .derive_default(true)
        .layout_tests(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));
    for path in &lib.include_paths {
        builder = builder.clang_arg(format!("-I{}", path.display()));
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    builder
        .generate()
        .expect("generate libraw bindings")
        .write_to_file(out.join("raw.rs"))
        .expect("write bindings");
}
