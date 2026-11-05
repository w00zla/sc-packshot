// Build id for the cluster handshake: a hash over the source files (paths with /, line endings ignored), so the same
// source gives the same id on Windows and Linux and nodes with different builds are told apart.
// Linux targets: also the RDSEED shim for Blender (shim/rdseed_shim.rs, see there), built as a .so without libc by
// rustc and rust-lld (both part of the Rust toolchain, so no C compiler is needed, on Windows hosts neither) and
// embedded by src/blender.rs
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const SHIM: &str = "shim/rdseed_shim.rs";

fn main() {
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed={SHIM}");
    let mut files: Vec<_> = fs::read_dir("src").unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "rs")).collect();
    files.sort();
    files.push(PathBuf::from(SHIM));
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    for f in &files {
        feed(Path::new(f).file_name().unwrap().to_string_lossy().as_bytes());
        let text = fs::read_to_string(f).unwrap().replace("\r\n", "\n");
        feed(text.as_bytes());
    }
    println!("cargo:rustc-env=PACKSHOT_BUILD={:08x}", h >> 32);
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        shim();
    }
}

/// shim/rdseed_shim.rs -> $OUT_DIR/rdseed_shim.so: a no_std cdylib for the target, linked by rust-lld (no crt, no
/// libc); -crt-static: the musl target links statically by default and would drop the cdylib
fn shim() {
    let target = env::var("TARGET").unwrap();
    if !target.starts_with("x86_64-") {
        panic!("the RDSEED shim (shim/rdseed_shim.rs) is x86-64 only, target {target}");
    }
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("rdseed_shim.so");
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let status = Command::new(&rustc)
        .args(["--edition", "2024", "--crate-type", "cdylib", "--crate-name", "rdseed_shim", "--target", &target])
        .args(["-C", "panic=abort", "-C", "opt-level=2", "-C", "target-feature=-crt-static", "-C", "linker=rust-lld", "-C", "strip=symbols"])
        .args(["-C", "link-arg=--build-id=none"])
        .arg(SHIM)
        .arg("-o")
        .arg(&out)
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => panic!("building the RDSEED shim ({SHIM}) with {rustc} and rust-lld failed ({s}); it needs the Rust target {target} (rustup target add {target})"),
        Err(e) => panic!("building the RDSEED shim ({SHIM}): {rustc} does not start: {e}"),
    }
    println!("cargo:rustc-env=PACKSHOT_SHIM={}", out.display());
}
