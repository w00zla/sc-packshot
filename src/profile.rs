// Render profiles (ADRs 0022, 0041, 0042): a profile is a folder with profile.toml (parameters such as size and
// format) and hooks.py. game-tile, found beside render.py as render.py finds it, is the base of every profile: its
// parameters lie under the profile's, its hooks run where the profile has none. A run's profile is chosen with
// --profile: a name (an own profile in <store>/profiles/<name> hides a bundled one beside render.py) or a folder. The
// head node sends render.py, game-tile and the run's profile with every render chunk (ADR 0024); its content hash
// keys the Blender processes and goes into the run record.
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::net::FileEntry;

/// The base profile, bundled beside render.py
pub const BASE: &str = "game-tile";
/// The output formats render.py writes
pub const FORMATS: [&str; 3] = ["png", "jpeg", "exr"];
/// The extensions of the images runs hold (one per format)
pub const IMAGE_EXTS: [&str; 3] = ["png", "jpg", "exr"];

/// The file extension of an output format
pub fn ext(format: &str) -> &'static str {
    match format {
        "jpeg" => "jpg",
        "exr" => "exr",
        _ => "png",
    }
}

/// A rendered image of any format (by its extension)
pub fn is_image(path: &Path) -> bool {
    path.extension().is_some_and(|x| IMAGE_EXTS.iter().any(|e| x.eq_ignore_ascii_case(e)))
}

#[derive(Clone, Debug)]
pub struct Profile {
    /// its folder name: the name it has in a render chunk's folder
    pub name: String,
    pub dir: PathBuf,
    /// game-tile as render.py finds it
    pub base: PathBuf,
}

/// Where render.py looks for a bundled profile: next to it (a render chunk's folder) and beside its folder (blender/
/// or cli/ beside profiles/)
fn bundled_dirs(script: &Path) -> Vec<PathBuf> {
    let here = script.parent().unwrap_or(Path::new(""));
    let mut dirs = vec![here.join("profiles")];
    if let Some(up) = here.parent() {
        dirs.push(up.join("profiles"));
    }
    dirs
}

fn is_profile(dir: &Path) -> bool {
    dir.join("profile.toml").is_file()
}

fn same_dir(a: &Path, b: &Path) -> bool {
    fs::canonicalize(a).ok().zip(fs::canonicalize(b).ok()).is_some_and(|(a, b)| a == b)
}

/// game-tile as render.py finds it
pub fn base_dir(script: &Path) -> Option<PathBuf> {
    bundled_dirs(script).into_iter().map(|d| d.join(BASE)).find(|d| is_profile(d))
}

/// --profile: a name (an own profile in <store>/profiles first, then the bundled ones beside render.py) or a folder
/// (relative to `cwd`); Err: why there is none
pub fn resolve(arg: &str, store: &Path, script: &Path, cwd: &Path) -> std::result::Result<Profile, String> {
    let dir = if crate::net::safe_name(arg) {
        let mut candidates = vec![store.join("profiles").join(arg)];
        candidates.extend(bundled_dirs(script).into_iter().map(|d| d.join(arg)));
        match candidates.iter().find(|d| is_profile(d)) {
            Some(d) => d.clone(),
            None => return Err(format!("{arg} not found in {}", candidates.iter().map(|d| d.parent().unwrap_or(d).display().to_string()).collect::<Vec<_>>().join(" or "))),
        }
    } else {
        let d = cwd.join(arg);
        if !is_profile(&d) {
            return Err(format!("no profile.toml in {}", d.display()));
        }
        d
    };
    let p = Profile::at(&dir, script).ok_or_else(|| format!("{BASE} not found beside {}", script.display()))?;
    // a render chunk's folder holds game-tile and the run's profile side by side, by their names
    if p.name == BASE && !same_dir(&p.dir, &p.base) {
        return Err(format!("{}: an own profile cannot be called {BASE}, the base of every profile; give it another name", p.dir.display()));
    }
    Ok(p)
}

impl Profile {
    /// The profile in `dir` for render.py `script` (None: no game-tile beside it)
    pub fn at(dir: &Path, script: &Path) -> Option<Self> {
        let name = dir.file_name()?.to_string_lossy().to_string();
        Some(Self { name, dir: dir.to_path_buf(), base: base_dir(script)? })
    }

    /// game-tile itself
    pub fn base(script: &Path) -> Option<Self> {
        base_dir(script).map(|base| Self { name: BASE.into(), dir: base.clone(), base })
    }

    /// game-tile at `dir`, which need not exist: engines in tests that render nothing
    #[cfg(test)]
    pub fn stub(dir: &Path) -> Self {
        Self { name: BASE.into(), dir: dir.to_path_buf(), base: dir.to_path_buf() }
    }

    /// game-tile, then the profile when it is another one, by their names
    fn folders(&self) -> Vec<(&str, &Path)> {
        let mut v = vec![(BASE, self.base.as_path())];
        if !same_dir(&self.dir, &self.base) {
            v.push((self.name.as_str(), self.dir.as_path()));
        }
        v
    }

    /// Content hash of the profile and game-tile (its hooks are the defaults)
    pub fn hash(&self) -> Option<String> {
        crate::events::content_hash(&self.folders())
    }

    /// Content hash of render.py with the profile: a Blender process runs one of each
    pub fn key(&self, script: &Path) -> Option<String> {
        let mut parts = vec![("render.py", script)];
        parts.extend(self.folders());
        crate::events::content_hash(&parts)
    }

    /// A `[params]` value of the profile, else game-tile's
    pub fn param(&self, key: &str) -> Option<String> {
        let of = |dir: &Path| fs::read_to_string(dir.join("profile.toml")).ok().and_then(|t| param(&t, key));
        of(&self.dir).or_else(|| of(&self.base))
    }

    /// The files a render chunk carries: render.py, profiles/game-tile/... and profiles/<name>/... (without
    /// __pycache__); laid out so in the chunk's folder, render.py finds game-tile next to it
    pub fn payload(&self, script: &Path) -> Result<(Vec<FileEntry>, Vec<PathBuf>)> {
        let size = fs::metadata(script).with_context(|| script.display().to_string())?.len();
        let (mut entries, mut paths) = (vec![FileEntry { path: "render.py".into(), size }], vec![script.to_path_buf()]);
        for (name, dir) in self.folders() {
            let (e, p) = crate::net::collect_files(dir, &format!("profiles/{name}/"))?;
            entries.extend(e);
            paths.extend(p);
        }
        Ok((entries, paths))
    }
}

/// A `[params]` value of a profile.toml as text (`key = value  # comment`, a string without its quotes): packshot
/// needs only size and format, render.py reads the whole file
pub fn param(toml: &str, key: &str) -> Option<String> {
    let mut section = "";
    for line in toml.lines().map(str::trim) {
        if line.starts_with('[') {
            section = line.split('#').next().unwrap_or("").trim();
        } else if section == "[params]" && let Some(rest) = line.strip_prefix(key).map(str::trim_start).and_then(|r| r.strip_prefix('=')) {
            let v = rest.trim();
            return Some(match v.strip_prefix('"') {
                Some(s) => s.split('"').next().unwrap_or("").to_string(),
                None => v.split('#').next().unwrap_or("").trim().to_string(),
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// A repo-like layout: blender/render.py, profiles/game-tile, profiles/studio, an own profile in the store
    fn layout(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("packshot-profile-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        write(&root.join("blender/render.py"), "# core");
        write(&root.join("profiles/game-tile/profile.toml"), "[params]\nsize = 256          # px\nformat = \"png\"      # png | jpeg | exr\n");
        write(&root.join("profiles/game-tile/hooks.py"), "def post(ctx, exr): pass\n");
        write(&root.join("profiles/game-tile/__pycache__/hooks.cpython-313.pyc"), "bytecode");
        write(&root.join("profiles/studio/profile.toml"), "[params]\nformat = \"jpeg\"\n");
        write(&root.join("store/profiles/mine/profile.toml"), "[params]\nsize = 512\n");
        (root.join("blender/render.py"), root.join("store"), root)
    }

    #[test]
    fn params() {
        let t = "# c\nscene = \"x.blend\"\n[params]\nsize = 256          # px\nsize_x = 3\nformat = \"jpeg\"  # png | jpeg\nmirror = false\n[other]\nformat = \"exr\"\n";
        assert_eq!(param(t, "size").as_deref(), Some("256"));
        assert_eq!(param(t, "format").as_deref(), Some("jpeg"));
        assert_eq!(param(t, "mirror").as_deref(), Some("false"));
        assert_eq!(param(t, "scene"), None, "only [params]");
        assert_eq!(param(t, "samples"), None);
        assert_eq!((ext("png"), ext("jpeg"), ext("exr")), ("png", "jpg", "exr"));
        assert!(is_image(Path::new("a/X (x) 1.JPG")) && is_image(Path::new("b.exr")) && !is_image(Path::new("run.json")));
    }

    /// Names: an own profile first, then the bundled ones beside render.py; a folder path; game-tile cannot be an own
    /// profile's name; parameters fall back to game-tile's
    #[test]
    fn resolution() {
        let (script, store, root) = layout("resolve");
        let gt = resolve(BASE, &store, &script, &root).unwrap();
        assert!(same_dir(&gt.dir, &root.join("profiles/game-tile")) && same_dir(&gt.base, &gt.dir));
        let studio = resolve("studio", &store, &script, &root).unwrap();
        assert_eq!((studio.name.as_str(), studio.param("format").as_deref(), studio.param("size").as_deref()), ("studio", Some("jpeg"), Some("256")));
        let mine = resolve("mine", &store, &script, &root).unwrap();
        assert_eq!(mine.param("size").as_deref(), Some("512"));
        // an own one hides the bundled one of the same name
        write(&store.join("profiles/studio/profile.toml"), "[params]\nformat = \"exr\"\n");
        assert_eq!(resolve("studio", &store, &script, &root).unwrap().param("format").as_deref(), Some("exr"));
        // a folder path, relative to the current folder
        let by_path = resolve("store/profiles/mine", &store, &script, &root).unwrap();
        assert_eq!(by_path.name, "mine");
        let missing = resolve("nope", &store, &script, &root).unwrap_err();
        assert!(missing.starts_with("nope not found in"), "{missing}");
        assert!(resolve("store/profiles/none", &store, &script, &root).unwrap_err().starts_with("no profile.toml in"));
        write(&store.join("profiles/game-tile/profile.toml"), "[params]\n");
        assert!(resolve(BASE, &store, &script, &root).unwrap_err().contains("cannot be called game-tile"));
        // without game-tile beside render.py there is no profile at all
        assert!(resolve("mine", &store, &root.join("a/b/render.py"), &root).unwrap_err().starts_with("game-tile not found beside"));
        let _ = fs::remove_dir_all(&root);
    }

    /// The hash covers game-tile and the profile, not __pycache__; the key render.py too; the payload lays them out as
    /// render.py, profiles/game-tile/..., profiles/<name>/... and hashes the same in a chunk's folder
    #[test]
    fn hash_and_payload() {
        let (script, store, root) = layout("hash");
        let gt = resolve(BASE, &store, &script, &root).unwrap();
        let studio = resolve("studio", &store, &script, &root).unwrap();
        let (h_gt, h_studio, k_studio) = (gt.hash().unwrap(), studio.hash().unwrap(), studio.key(&script).unwrap());
        assert!(h_gt != h_studio && h_studio != k_studio);
        write(&root.join("profiles/game-tile/__pycache__/hooks.cpython-313.pyc"), "other bytecode");
        assert_eq!(gt.hash().unwrap(), h_gt, "__pycache__ is not part of it");
        write(&root.join("profiles/game-tile/hooks.py"), "def post(ctx, exr): return []\n");
        assert_ne!(studio.hash().unwrap(), h_studio, "game-tile's hooks are the defaults: part of every profile's hash");
        let h_studio = studio.hash().unwrap();
        write(&script, "# core 2");
        assert_eq!(studio.hash().unwrap(), h_studio);
        assert_ne!(studio.key(&script).unwrap(), k_studio, "the key covers render.py");

        let (entries, paths) = studio.payload(&script).unwrap();
        let mut names: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["profiles/game-tile/hooks.py", "profiles/game-tile/profile.toml", "profiles/studio/profile.toml", "render.py"]);
        assert!(names.iter().all(|n| crate::net::safe_rel(n).is_some()));
        let (gt_entries, _) = gt.payload(&script).unwrap();
        assert_eq!(gt_entries.len(), 3, "game-tile once");
        // laid out in a chunk's folder as the receiving node does: the same hash and key there
        let chunk = root.join("chunk");
        for (e, p) in entries.iter().zip(&paths) {
            write(&chunk.join(crate::net::safe_rel(&e.path).unwrap()), &fs::read_to_string(p).unwrap());
        }
        let there = Profile::at(&chunk.join("profiles/studio"), &chunk.join("render.py")).unwrap();
        assert!(same_dir(&there.base, &chunk.join("profiles/game-tile")), "render.py finds game-tile next to it");
        assert_eq!((there.hash().unwrap(), there.key(&chunk.join("render.py")).unwrap()), (studio.hash().unwrap(), studio.key(&script).unwrap()));
        let _ = fs::remove_dir_all(&root);
    }
}
