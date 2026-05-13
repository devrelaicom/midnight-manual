//! Build script: stamp the target triple into the binary so `mnm version`
//! can report it without depending on rustc-time `HOST` (which is only set
//! at rustc invocation time, not at cargo build time).

fn main() {
    let triple = std::env::var("TARGET").unwrap_or_else(|_| "unknown".into());
    println!("cargo:rustc-env=TARGET_TRIPLE={triple}");
}
