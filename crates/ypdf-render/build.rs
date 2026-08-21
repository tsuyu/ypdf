//! Exposes the build target triple so the runtime can find the matching
//! vendored PDFium binary.

fn main() {
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo::rustc-env=YPDF_TARGET_TRIPLE={target}");
    println!("cargo::rerun-if-changed=build.rs");
}
