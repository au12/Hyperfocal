//! Generates the LibRaw bindings (feature `libraw`) from the installed headers, so
//! struct layouts always match the library the binary links against.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(feature = "libraw")]
    libraw::generate();
}

#[cfg(feature = "libraw")]
mod libraw {
    use std::path::PathBuf;

    pub fn generate() {
        let lib = pkg_config::Config::new()
            .atleast_version("0.20")
            .cargo_metadata(false)
            .probe("libraw_r")
            .unwrap_or_else(|e| {
                panic!("LibRaw (libraw_r) was not found ({e}). Install libraw-dev, or build with --no-default-features --features heic")
            });
        // The thread-safe build: decodes run concurrently.
        for dir in &lib.link_paths {
            println!("cargo:rustc-link-search=native={}", dir.display());
        }
        println!("cargo:rustc-link-lib=dylib=raw_r");

        let bindings = bindgen::Builder::default()
            .header_contents("hyperfocal_libraw.h", "#include <libraw.h>\n")
            .clang_args(lib.include_paths.iter().map(|p| format!("-I{}", p.display())))
            .allowlist_function("libraw_.*")
            .allowlist_type("libraw_.*")
            .allowlist_var("LIBRAW_.*")
            .layout_tests(false)
            .generate_comments(false)
            .generate()
            .expect("generating LibRaw bindings");
        let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("libraw.rs");
        bindings.write_to_file(out).expect("writing LibRaw bindings");
    }
}
