// The Blender processes packshot starts (runs, the persistent pool, the assessment's sweeps): one command line for
// all, with the thread count and, on Linux, the RDSEED shim (shim/rdseed_shim.rs, built and embedded by build.rs)
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(target_os = "linux")]
use std::sync::OnceLock;

use anyhow::Result;

/// --threads for every Blender: with 6 Blender processes the throughput stayed the same or rose slightly, with 35-55 %
/// less CPU; exports running alongside lost 15 % of their rate instead of 32 % (2026-11-05, 9800X3D-class 16 threads)
pub const THREADS: u32 = 2;

#[cfg(target_os = "linux")]
const SHIM: &[u8] = include_bytes!(env!("PACKSHOT_SHIM"));

/// The shim written into the work folder (Linux, once at start); unset: the Blenders run without it
#[cfg(target_os = "linux")]
static PRELOAD: OnceLock<PathBuf> = OnceLock::new();

/// `blender -b --threads THREADS`, on Linux with the shim in LD_PRELOAD (before what the environment already has)
pub fn command(blender: &Path) -> Command {
    let mut c = Command::new(blender);
    c.arg("-b").arg("--threads").arg(THREADS.to_string());
    #[cfg(target_os = "linux")]
    if let Some(shim) = PRELOAD.get() {
        let mut v = shim.as_os_str().to_os_string();
        if let Some(old) = std::env::var_os("LD_PRELOAD").filter(|o| !o.is_empty()) {
            v.push(":");
            v.push(old);
        }
        c.env("LD_PRELOAD", v);
    }
    c
}

/// Linux: writes the shim into `work` as rdseed-shim-<content hash>.so (atomically: a temporary file renamed; one
/// already there with the same content stays) and preloads it into every Blender from then on. Ok(None) elsewhere,
/// Err when it cannot be used (the Blenders then run without it). LD_PRELOAD splits at blanks and colons, so a work
/// folder with one of them cannot take it
pub fn install(work: &Path) -> Result<Option<PathBuf>> {
    #[cfg(target_os = "linux")]
    {
        use anyhow::{Context, bail};
        let name = format!("rdseed-shim-{:016x}.so", twox_hash::XxHash3_128::oneshot(SHIM) as u64);
        let path = std::path::absolute(work.join(name))?;
        if path.to_string_lossy().contains([' ', '\t', '\n', ':']) {
            bail!("LD_PRELOAD cannot take {} (a blank or colon in the path): --work in another folder", path.display());
        }
        if std::fs::read(&path).ok().as_deref() != Some(SHIM) {
            let tmp = path.with_extension(format!("tmp{}", std::process::id()));
            std::fs::write(&tmp, SHIM).with_context(|| tmp.display().to_string())?;
            std::fs::rename(&tmp, &path).with_context(|| path.display().to_string())?;
        }
        let _ = PRELOAD.set(path.clone());
        Ok(Some(path))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = work;
        Ok(None::<PathBuf>)
    }
}
