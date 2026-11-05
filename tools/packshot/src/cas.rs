// Exported items one by one. An export chunk's folder holds the packages of all its items over one shared Data/ tree;
// an item needs only part of it: its closure, its package folder plus every file a JSON file of it names (scene.json,
// the material sidecars it leads to: textures, mesh .blend files, further sidecars), found as written or
// case-insensitively, as the add-on's resolve_path finds them. A rendering node fetches an item's closure from the
// node holding it by content hash (xxh3-128) into its content-addressed cache (hash -> file, per run) and links each
// render chunk's folders from there, laid out as on the holder, so the paths in the export stay valid.
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// A file of an exported item: path relative to its export chunk's folder (with /), size, content hash
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct HashedFile {
    pub path: String,
    pub size: u64,
    pub hash: String,
}

/// Case-insensitive index of an export folder: lower case relative path -> the relative paths as on disk
pub struct Index {
    files: HashMap<String, Vec<String>>,
}

impl Index {
    pub fn build(root: &Path) -> Result<Self> {
        let mut files: HashMap<String, Vec<String>> = HashMap::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for e in fs::read_dir(&dir).with_context(|| dir.display().to_string())?.flatten() {
                let p = e.path();
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    stack.push(p);
                    continue;
                }
                let rel = p.strip_prefix(root)?.to_string_lossy().replace('\\', "/");
                files.entry(rel.to_lowercase()).or_default().push(rel);
            }
        }
        Ok(Self { files })
    }

    fn get(&self, rel: &str) -> &[String] {
        self.files.get(&rel.to_lowercase()).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// The candidates the add-on tries for a path from the export (manifest.py `_candidate_relative_paths`): as written,
/// and without a leading materials/, meshes/ or textures/ in front of data/
fn candidates(s: &str) -> Vec<String> {
    let n = s.replace('\\', "/").trim_start_matches('/').to_string();
    let mut out = vec![n.clone()];
    let lower = n.to_lowercase();
    for prefix in ["materials/", "meshes/", "textures/"] {
        if lower.starts_with(prefix) && lower[prefix.len()..].starts_with("data/") {
            out.push(n[prefix.len()..].to_string());
        }
    }
    out
}

fn strings<'a>(v: &'a serde_json::Value, out: &mut Vec<&'a str>) {
    match v {
        serde_json::Value::String(s) if s.contains('/') || s.contains('\\') => out.push(s),
        serde_json::Value::Array(a) => a.iter().for_each(|x| strings(x, out)),
        serde_json::Value::Object(m) => m.values().for_each(|x| strings(x, out)),
        _ => {}
    }
}

/// An item's closure in its export chunk's folder `root`: the relative paths (as on disk) of every file in its package
/// folder (`package`, relative) and of every file its JSON files name, followed through the JSON files they name
pub fn closure(root: &Path, index: &Index, package: &str) -> Result<Vec<String>> {
    let prefix = format!("{}/", package.replace('\\', "/").trim_end_matches('/').to_lowercase());
    let mut found: BTreeSet<String> = index.files.iter().filter(|(k, _)| k.starts_with(&prefix)).flat_map(|(_, v)| v.iter().cloned()).collect();
    if found.is_empty() {
        bail!("package {package} not in the export");
    }
    let mut queue: Vec<String> = found.iter().cloned().collect();
    let mut read: HashSet<String> = HashSet::new();
    while let Some(rel) = queue.pop() {
        if !rel.to_lowercase().ends_with(".json") || !read.insert(rel.clone()) {
            continue;
        }
        let path = root.join(&rel);
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&fs::read(&path).with_context(|| path.display().to_string())?) else { continue };
        let mut refs = Vec::new();
        strings(&v, &mut refs);
        for s in refs {
            for c in candidates(s) {
                for hit in index.get(&c) {
                    if found.insert(hit.clone()) {
                        queue.push(hit.clone());
                    }
                }
            }
        }
    }
    Ok(found.into_iter().collect())
}

/// Content hash of a file: xxh3-128 as 32 hex digits, and its size
pub fn hash_file(path: &Path) -> Result<(u64, String)> {
    let data = fs::read(path).with_context(|| path.display().to_string())?;
    Ok((data.len() as u64, format!("{:032x}", twox_hash::XxHash3_128::oneshot(&data))))
}

/// Hard link, a copy where that fails (another file system), parents created
pub fn link_or_copy(src: &Path, dst: &Path) -> Result<()> {
    if let Some(p) = dst.parent() {
        fs::create_dir_all(p)?;
    }
    let _ = fs::remove_file(dst);
    if fs::hard_link(src, dst).is_err() {
        fs::copy(src, dst).with_context(|| format!("{} -> {}", src.display(), dst.display()))?;
    }
    Ok(())
}

/// The content-addressed cache of a rendering node: one file per hash, for the run going on
pub struct Cas {
    dir: PathBuf,
    have: HashMap<String, u64>,
}

impl Cas {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, have: HashMap::new() }
    }

    pub fn has(&self, hash: &str) -> bool {
        self.have.contains_key(hash)
    }

    pub fn path(&self, hash: &str) -> PathBuf {
        self.dir.join(hash)
    }

    /// Take a received file into the cache (moved), checked against its hash; one that is here already is dropped
    /// (on Windows a rename over a file with hard links open fails)
    pub fn put(&mut self, hash: &str, src: &Path) -> Result<()> {
        if self.has(hash) {
            let _ = fs::remove_file(src);
            return Ok(());
        }
        let (size, got) = hash_file(src)?;
        if got != hash {
            let _ = fs::remove_file(src);
            bail!("{} arrived with hash {got}, expected {hash}", src.display());
        }
        fs::create_dir_all(&self.dir)?;
        let dst = self.path(hash);
        if fs::rename(src, &dst).is_err() {
            fs::copy(src, &dst)?;
            let _ = fs::remove_file(src);
        }
        self.have.insert(hash.to_string(), size);
        Ok(())
    }

    /// Bytes in the cache
    pub fn bytes(&self) -> u64 {
        self.have.values().sum()
    }

    /// Empty the cache (run over, head node gone)
    pub fn clear(&mut self) {
        self.have.clear();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    /// The closure follows scene.json into the sidecars and their textures, case-insensitively and through the
    /// add-on's prefixes; files of other items stay out
    #[test]
    fn closure_follows_references() {
        let root = std::env::temp_dir().join(format!("packshot-closure-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        write(&root, "Packages/a_LOD1_TEX2/scene.json", r#"{"root": {"material_sidecar": "Data/Objects/a_TEX2.materials.json", "mesh_asset": "data\\objects\\A_LOD1.blend", "x": "Data/"}}"#);
        write(&root, "Packages/a_LOD1_TEX2/scene.blend", "b");
        write(&root, "Packages/a_LOD1_TEX2/palettes.json", "{}");
        write(&root, "Data/Objects/a_TEX2.materials.json", r#"{"slots": [{"path": "Data/textures/shared_TEX2.png"}, {"path": "textures/Data/Objects/a_diff_TEX2.png"}, {"src": "Data/objects/a_diff.tif"}]}"#);
        write(&root, "Data/Objects/A_LOD1.blend", "m");
        write(&root, "Data/Textures/shared_TEX2.png", "s");
        write(&root, "Data/Objects/a_diff_TEX2.png", "d");
        write(&root, "Packages/b_LOD1_TEX2/scene.json", r#"{"t": "Data/Objects/b_TEX2.png"}"#);
        write(&root, "Data/Objects/b_TEX2.png", "o");
        let index = Index::build(&root).unwrap();
        let c = closure(&root, &index, "Packages/a_LOD1_TEX2").unwrap();
        assert_eq!(
            c,
            vec![
                "Data/Objects/A_LOD1.blend",
                "Data/Objects/a_TEX2.materials.json",
                "Data/Objects/a_diff_TEX2.png",
                "Data/Textures/shared_TEX2.png",
                "Packages/a_LOD1_TEX2/palettes.json",
                "Packages/a_LOD1_TEX2/scene.blend",
                "Packages/a_LOD1_TEX2/scene.json",
            ]
        );
        assert!(closure(&root, &index, "Packages/missing").is_err());
        let _ = fs::remove_dir_all(&root);
    }

    /// The DataCore tables of the repo's packshot folder, read from a copy (Tables::load rewrites its rigs)
    pub(crate) fn repo_tables(work: &Path) -> crate::tables::Tables {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let p4k = PathBuf::from(std::env::var("SC_DATA_P4K").expect("SC_DATA_P4K"));
        let starbreaker = repo.join("local/starbreaker-qm").join(crate::exe("starbreaker"));
        let dcb = repo.join("local/packshot/work/dcb");
        fs::create_dir_all(work.join("dcb")).unwrap();
        fs::copy(dcb.join("pitconfig.json"), work.join("dcb/pitconfig.json")).unwrap();
        crate::cluster::copy_tree(&dcb.join("lightgroups"), &work.join("dcb/lightgroups")).unwrap();
        crate::tables::Tables::load(&starbreaker, &p4k, work, false, |_| {}).unwrap()
    }

    /// Mandatory check of the closure (a missing texture renders silently white): a varied sample (armour, clothing,
    /// weapons, ship parts, ores, food, …) exported in one export-batch, every item rendered once from the whole
    /// export folder and once from a folder holding only its closure, the images compared. Real tools, SC_DATA_P4K
    /// set: cargo test --release -- --ignored closure_renders_like_full --nocapture
    #[test]
    #[ignore]
    fn closure_renders_like_full() {
        use crate::engine::Target;
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let p4k = PathBuf::from(std::env::var("SC_DATA_P4K").expect("SC_DATA_P4K"));
        let starbreaker = repo.join("local/starbreaker-qm").join(crate::exe("starbreaker"));
        let blender = fs::read_dir(repo.join("local/blender")).unwrap().flatten().map(|e| e.path().join(crate::exe("blender"))).find(|p| p.exists()).unwrap();
        let root = std::env::temp_dir().join(format!("packshot-closure-real-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let tables = repo_tables(&root.join("work"));
        let targets: Vec<Target> = serde_json::from_slice(&fs::read(repo.join("local/packshot/targets.json")).unwrap()).unwrap();
        // evenly spread over the types, so every family of shaders and materials is in it
        let want = [
            ("Char_Armor_Helmet", 4), ("Char_Armor_Torso", 3), ("Char_Armor_Legs", 2), ("Char_Armor_Arms", 2), ("Char_Clothing_Torso_1", 3),
            ("Char_Clothing_Legs", 2), ("Char_Clothing_Feet", 1), ("Char_Armor_Backpack", 2), ("Char_Armor_Undersuit", 1), ("WeaponPersonal", 5),
            ("WeaponGun", 3), ("WeaponAttachment", 2), ("PowerPlant", 2), ("Cooler", 2), ("QuantumDrive", 2), ("Shield", 1), ("Missile", 1),
            ("Misc", 2), ("Food", 1), ("Drink", 1), ("Thumbnail", 2), ("MobiGlas", 1),
        ];
        let mut sample: Vec<&Target> = Vec::new();
        for (kind, n) in want {
            let of: Vec<&Target> = targets.iter().filter(|t| t.kind == kind).collect();
            let step = (of.len() / (n + 1)).max(1);
            sample.extend((1..=n).filter_map(|i| of.get(i * step).copied()));
        }
        let full = root.join("full");
        fs::create_dir_all(&full).unwrap();
        let jobs: Vec<_> = sample.iter().map(|t| serde_json::json!({ "name": t.entity, "output": full, "worn": t.worn })).collect();
        fs::write(root.join("jobs.json"), serde_json::to_vec(&jobs).unwrap()).unwrap();
        let out = std::process::Command::new(&starbreaker)
            .arg("entity").arg("export-batch").arg(root.join("jobs.json")).arg("--p4k").arg(&p4k)
            .args(["--kind", "decomposed", "--variant-tags", "--icon-files"])
            .output()
            .unwrap();
        let mut packages: HashMap<String, String> = HashMap::new();
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
            if let (Some(name), Some(pkg)) = (v["name"].as_str(), v["package"].as_str()) {
                let rel = Path::new(pkg).strip_prefix(&full).unwrap().to_string_lossy().replace('\\', "/");
                packages.insert(name.to_lowercase(), rel);
            }
        }
        let index = Index::build(&full).unwrap();
        let mut full_jobs = Vec::new();
        let mut part_jobs = Vec::new();
        let (mut part_bytes, mut part_files) = (0u64, 0usize);
        let mut done = Vec::new();
        for (k, t) in sample.iter().enumerate() {
            let Some(pkg) = packages.get(&t.entity.to_lowercase()) else { continue };
            let own = root.join("part").join(format!("i{k}"));
            for rel in closure(&full, &index, pkg).unwrap() {
                part_bytes += fs::metadata(full.join(&rel)).unwrap().len();
                part_files += 1;
                link_or_copy(&full.join(&rel), &own.join(&rel)).unwrap();
            }
            let pose = tables.pose(&t.kind, &t.sub_type);
            let rig = tables.rig(&t.kind, &t.sub_type).unwrap().clone();
            let job = |src: &Path, out: PathBuf| serde_json::json!({ "src": src.join(pkg).join("scene.blend"), "out": out, "pose": pose, "rig": rig, "size": 256 });
            full_jobs.push(job(&full, root.join("png/full").join(format!("{k}.png"))));
            part_jobs.push(job(&own, root.join("png/part").join(format!("{k}.png"))));
            done.push((k, t.entity.clone()));
        }
        // negative control: the first item once more without its largest texture must render differently, else the
        // comparison would not see a missing file
        let control = {
            let (k, entity) = done[0].clone();
            let src = root.join("part").join(format!("i{k}"));
            let dst = root.join("part").join("control");
            crate::cluster::copy_tree(&src, &dst).unwrap();
            let index = Index::build(&dst).unwrap();
            let biggest = index.files.values().flatten().filter(|r| r.to_lowercase().ends_with(".png")).max_by_key(|r| fs::metadata(dst.join(r)).unwrap().len()).unwrap().clone();
            fs::remove_file(dst.join(&biggest)).unwrap();
            let mut job = part_jobs[0].clone();
            job["src"] = serde_json::json!(dst.join(packages[&entity.to_lowercase()].as_str()).join("scene.blend"));
            job["out"] = serde_json::json!(root.join("png/part/control.png"));
            part_jobs.push(job);
            (k, entity, biggest)
        };
        let full_bytes: u64 = index.files.values().flatten().map(|r| fs::metadata(full.join(r)).unwrap().len()).sum();
        println!("{} of {} exported; whole export {:.1} MB, closures {part_files} files {:.1} MB in all", done.len(), sample.len(), full_bytes as f64 / 1e6, part_bytes as f64 / 1e6);
        let script = repo.join("src/icons/render.py");
        let render = |name: &str, jobs: &Vec<serde_json::Value>| {
            let file = root.join(format!("{name}.json"));
            fs::write(&file, serde_json::to_vec(jobs).unwrap()).unwrap();
            std::process::Command::new(&blender).arg("-b").args(["--gpu-backend", "vulkan", "--python"]).arg(&script).arg("--").arg("--jobs").arg(&file).output().unwrap()
        };
        let (a, b) = std::thread::scope(|s| {
            let a = s.spawn(|| render("full", &full_jobs));
            let b = s.spawn(|| render("part", &part_jobs));
            (a.join().unwrap(), b.join().unwrap())
        });
        for o in [&a, &b] {
            let text = String::from_utf8_lossy(&o.stdout);
            for l in text.lines().filter(|l| l.starts_with("failed: ")) {
                println!("{l}");
            }
        }
        let mut worst = 0.0f64;
        let mut compared = 0;
        for (k, entity) in &done {
            let (pa, pb) = (root.join("png/full").join(format!("{k}.png")), root.join("png/part").join(format!("{k}.png")));
            let (Ok(ia), Ok(ib)) = (image::open(&pa), image::open(&pb)) else {
                println!("{entity}: image missing (full {}, closure {})", pa.exists(), pb.exists());
                assert_eq!(pa.exists(), pb.exists(), "{entity}: rendered from one folder only");
                continue;
            };
            let (ia, ib) = (ia.to_rgba8(), ib.to_rgba8());
            assert_eq!(ia.dimensions(), ib.dimensions());
            let sum: f64 = ia.as_raw().iter().zip(ib.as_raw()).map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2)).sum();
            let rmse = (sum / ia.as_raw().len() as f64).sqrt() / 255.0;
            let off = ia.as_raw().chunks(4).zip(ib.as_raw().chunks(4)).filter(|(p, q)| p.iter().zip(q.iter()).any(|(x, y)| x.abs_diff(*y) > 16)).count();
            println!("{entity:<56} RMSE {:.4} %  pixels off >16: {off}", rmse * 100.0);
            worst = worst.max(rmse);
            compared += 1;
        }
        println!("{compared} compared, worst RMSE {:.4} %", worst * 100.0);
        let (k, entity, biggest) = control;
        let (ia, ib) = (image::open(root.join("png/full").join(format!("{k}.png"))).unwrap().to_rgba8(), image::open(root.join("png/part/control.png")).unwrap().to_rgba8());
        let sum: f64 = ia.as_raw().iter().zip(ib.as_raw()).map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2)).sum();
        let rmse = (sum / ia.as_raw().len() as f64).sqrt() / 255.0;
        println!("control: {entity} without {biggest}: RMSE {:.4} %", rmse * 100.0);
        assert!(rmse > 0.0, "a missing texture went unnoticed");
        assert!(compared + 5 >= done.len(), "most of the sample rendered");
        assert!(worst < 0.005, "a closure renders differently from the whole export");
        let _ = crate::store::remove_tree(&root);
    }

    #[test]
    fn cache_checks_hashes() {
        let root = std::env::temp_dir().join(format!("packshot-cas-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("in"), "data").unwrap();
        let (size, hash) = hash_file(&root.join("in")).unwrap();
        assert_eq!((size, hash.len()), (4, 32));
        let mut cas = Cas::new(root.join("cas"));
        fs::write(root.join("bad"), "other").unwrap();
        assert!(cas.put(&hash, &root.join("bad")).is_err(), "a wrong hash is refused");
        cas.put(&hash, &root.join("in")).unwrap();
        assert!(cas.has(&hash) && cas.bytes() == 4);
        // the same content once more: dropped, the cached file stays as it is
        fs::write(root.join("again"), "data").unwrap();
        cas.put(&hash, &root.join("again")).unwrap();
        assert!(!root.join("again").exists() && cas.bytes() == 4 && fs::read_to_string(cas.path(&hash)).unwrap() == "data");
        link_or_copy(&cas.path(&hash), &root.join("out/x/y.png")).unwrap();
        assert_eq!(fs::read_to_string(root.join("out/x/y.png")).unwrap(), "data");
        cas.clear();
        assert!(!cas.has(&hash) && !root.join("cas").exists());
        let _ = fs::remove_dir_all(&root);
    }
}
