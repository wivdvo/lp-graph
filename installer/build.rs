//! Embeds the already built (and stamped) lp-tracker.exe and lp-graph.exe. build.ps1 builds
//! and stamps those first, then builds this installer.
use std::path::PathBuf;

fn main() {
    let release = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../target/release");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    for exe in ["lp-tracker.exe", "lp-graph.exe"] {
        let src = release.join(exe);
        println!("cargo:rerun-if-changed={}", src.display());
        std::fs::copy(&src, out.join(exe))
            .unwrap_or_else(|e| panic!("{} not found ({e}): build with build.ps1", src.display()));
    }
}
