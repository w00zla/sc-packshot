// Runs and sets. A run is a folder runs/<YYYY-MM-DD_HH-MM>_<name>/ with the images of this run
// ("<Name> (<class>) <uuid>.png", read-only), run.json (origin, result per entity) and the event log
// events.jsonl; immutable after the end. A set is a folder sets/<name>/ with all icons as hardlinks to
// the images of the runs (per entity the newest run in the chain with an image), set.json (base, chain, files in the folder,
// where its target list came from) and targets.json, the set's target list (taken from the base when branching,
// replaceable; the game's items change with its updates).
// `live` is the main set (pnpm thumbs:build reads it); experiments branch off live and are adopted
// by appending their runs to live's chain. No storage by content hash: renders are not bit-identical.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const LIVE: &str = "live";
/// The target list in a set's folder
pub const TARGETS: &str = "targets.json";

/// A set as other nodes see it (the Sets view of a remote head node)
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct SetSummary {
    pub name: String,
    #[serde(default)]
    pub base: Option<String>,
    /// runs of the base at the branch, then the own ones (oldest first)
    pub base_runs: usize,
    pub runs: Vec<String>,
    /// images in the set
    pub files: usize,
    #[serde(default)]
    pub targets: Option<TargetsInfo>,
}

/// Throughput and image size of the latest finished run with at least 50 results: the estimate for the next one
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct RunStats {
    pub run: String,
    pub items_per_s: f64,
    pub bytes_per_image: u64,
}

/// Where a set's target list came from
#[derive(Serialize, Deserialize, Clone, Default, Debug)]
pub struct TargetsInfo {
    pub count: usize,
    /// the file it was taken from, or "<set>" when inherited from the base
    pub source: String,
    pub at: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct EntityResult {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// the images in the run folder, one per UUID
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct RunInfo {
    pub id: String,
    pub name: String,
    /// the set the run was started for
    pub set: String,
    pub node: String,
    pub started: String,
    #[serde(default)]
    pub finished: Option<String>,
    #[serde(default)]
    pub aborted: bool,
    /// Origin like run_start in the event log: filter, scope, invocation, hashes of render.py and addon, Blender, p4k
    #[serde(default)]
    pub origin: Value,
    #[serde(default)]
    pub entities: BTreeMap<String, EntityResult>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct SetInfo {
    pub name: String,
    /// branched off from (None for live)
    #[serde(default)]
    pub base: Option<String>,
    /// the base's chain at the time of branching
    #[serde(default)]
    pub base_runs: Vec<String>,
    /// the own runs, oldest first
    #[serde(default)]
    pub runs: Vec<String>,
    /// files in the set folder -> run whose image the hardlink points to
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    /// the set's target list (targets.json in its folder); None = none yet
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub targets: Option<TargetsInfo>,
}

impl SetInfo {
    pub fn chain(&self) -> impl Iterator<Item = &String> {
        self.base_runs.iter().chain(&self.runs)
    }
}

/// State of a set per entity
#[derive(Default)]
pub struct Resolved {
    /// Entity -> (run, files) of the newest run with an image
    pub image: HashMap<String, (String, Vec<String>)>,
    /// Entities whose newest attempt failed (even if an older image exists)
    pub failed: HashSet<String>,
}

impl Resolved {
    pub fn apply(&mut self, run: &str, entity: &str, r: &EntityResult) {
        if r.ok {
            self.image.insert(entity.to_string(), (run.to_string(), r.files.clone()));
            self.failed.remove(entity);
        } else {
            self.failed.insert(entity.to_string());
        }
    }
}

pub struct Store {
    pub root: PathBuf,
    runs: HashMap<String, RunInfo>,
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(value)?).with_context(|| tmp.display().to_string())?;
    fs::rename(&tmp, path).with_context(|| path.display().to_string())
}

fn set_readonly(path: &Path, ro: bool) -> std::io::Result<()> {
    let mut p = fs::metadata(path)?.permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    p.set_readonly(ro);
    fs::set_permissions(path, p)
}

/// Removes a hardlink. Windows does not delete a read-only file, and the attribute belongs to the file (all
/// its links): so lift it briefly and set it again afterwards on the remaining link (`keep`)
fn remove_link(path: &Path, keep: Option<&Path>) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) if e.kind() == ErrorKind::PermissionDenied => {
            set_readonly(path, false)?;
            fs::remove_file(path).with_context(|| path.display().to_string())?;
            if let Some(k) = keep.filter(|k| k.exists()) {
                set_readonly(k, true)?;
            }
            Ok(())
        }
        Err(e) => Err(e).with_context(|| path.display().to_string()),
    }
}

/// Delete a folder including read-only files
pub(crate) fn remove_tree(dir: &Path) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for e in fs::read_dir(dir)?.flatten() {
        let p = e.path();
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            remove_tree(&p)?;
        } else {
            remove_link(&p, None)?;
        }
    }
    fs::remove_dir(dir).with_context(|| dir.display().to_string())
}

/// Readable part of a name for folders: letters, digits, - and _
pub fn slug(s: &str) -> String {
    let s: String = s.trim().chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    let s = s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    s.chars().take(48).collect()
}

impl Store {
    /// Opens the store (runs/, sets/) and creates live if it is missing
    pub fn open(root: &Path) -> Result<Self> {
        for d in ["runs", "sets"] {
            fs::create_dir_all(root.join(d)).with_context(|| root.join(d).display().to_string())?;
        }
        let store = Self { root: root.to_path_buf(), runs: HashMap::new() };
        if !store.set_dir(LIVE).join("set.json").exists() {
            fs::create_dir_all(store.set_dir(LIVE))?;
            store.save_set(&SetInfo { name: LIVE.into(), ..Default::default() })?;
        }
        Ok(store)
    }

    /// The packshot folder as it is, without creating anything (--report only reads)
    pub fn open_read(root: &Path) -> Self {
        Self { root: root.to_path_buf(), runs: HashMap::new() }
    }

    pub fn run_dir(&self, id: &str) -> PathBuf {
        self.root.join("runs").join(id)
    }

    pub fn set_dir(&self, name: &str) -> PathBuf {
        self.root.join("sets").join(name)
    }

    pub fn sets(&self) -> Result<Vec<String>> {
        let mut out: Vec<String> = fs::read_dir(self.root.join("sets"))?
            .flatten()
            .filter(|e| e.path().join("set.json").exists())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        out.sort();
        Ok(out)
    }

    pub fn load_set(&self, name: &str) -> Result<SetInfo> {
        let path = self.set_dir(name).join("set.json");
        let bytes = fs::read(&path).with_context(|| format!("Set {name} not found ({})", path.display()))?;
        serde_json::from_slice(&bytes).with_context(|| path.display().to_string())
    }

    pub fn save_set(&self, set: &SetInfo) -> Result<()> {
        write_json(&self.set_dir(&set.name).join("set.json"), set)
    }

    pub fn run(&mut self, id: &str) -> Result<&RunInfo> {
        if !self.runs.contains_key(id) {
            let path = self.run_dir(id).join("run.json");
            let info: RunInfo = serde_json::from_slice(&fs::read(&path).with_context(|| path.display().to_string())?).with_context(|| path.display().to_string())?;
            self.runs.insert(id.to_string(), info);
        }
        Ok(&self.runs[id])
    }

    /// Node of a run without caching it (None if it has no readable run.json)
    pub fn peek_node(&self, id: &str) -> Option<String> {
        if let Some(r) = self.runs.get(id) {
            return Some(r.node.clone());
        }
        let info: RunInfo = serde_json::from_slice(&fs::read(self.run_dir(id).join("run.json")).ok()?).ok()?;
        Some(info.node)
    }

    /// All finished runs in runs/, for the other nodes (ListRuns)
    pub fn summaries(&mut self) -> Vec<crate::net::RunSummary> {
        let Ok(dir) = fs::read_dir(self.root.join("runs")) else { return Vec::new() };
        let mut ids: Vec<String> = dir.flatten().filter(|e| e.path().join("run.json").exists()).map(|e| e.file_name().to_string_lossy().to_string()).collect();
        ids.sort();
        let mut out = Vec::new();
        for id in ids {
            let bytes = fs::read_dir(self.run_dir(&id)).map(|d| d.flatten().filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum()).unwrap_or(0);
            let Ok(r) = self.run(&id) else { continue };
            out.push(crate::net::RunSummary {
                id: r.id.clone(),
                name: r.name.clone(),
                set: r.set.clone(),
                node: r.node.clone(),
                finished: r.finished.is_some(),
                images: r.entities.values().filter(|e| e.ok).count(),
                bytes,
            });
        }
        out
    }

    pub fn save_run(&mut self, info: &RunInfo) -> Result<()> {
        write_json(&self.run_dir(&info.id).join("run.json"), info)?;
        self.runs.insert(info.id.clone(), info.clone());
        Ok(())
    }

    /// Folder name of a new run: start time (minute) and name, on a tie with -2, -3, …
    pub fn new_run_id(&self, name: &str) -> String {
        let base = format!("{}_{}", chrono::Local::now().format("%Y-%m-%d_%H-%M"), slug(name));
        let mut id = base.clone();
        let mut k = 2;
        while self.run_dir(&id).exists() {
            id = format!("{base}-{k}");
            k += 1;
        }
        id
    }

    /// New set, branched off `base` (key n)
    pub fn create_set(&mut self, name: &str, base: &str) -> Result<SetInfo> {
        if slug(name) != name || name.is_empty() {
            bail!("Set name \"{name}\": only letters, digits, - and _");
        }
        if self.set_dir(name).join("set.json").exists() {
            bail!("Set {name} exists already");
        }
        let b = self.load_set(base)?;
        let mut set = SetInfo { name: name.into(), base: Some(base.into()), base_runs: b.chain().cloned().collect(), ..Default::default() };
        fs::create_dir_all(self.set_dir(name))?;
        // the base's target list comes along
        if b.targets.is_some() {
            fs::copy(self.set_dir(base).join(TARGETS), self.set_dir(name).join(TARGETS)).with_context(|| format!("{TARGETS} of {base}"))?;
            set.targets = b.targets.map(|t| TargetsInfo { source: format!("<{base}>"), ..t });
        }
        self.sync(&mut set)?;
        Ok(set)
    }

    /// The set's target list; empty without one
    pub fn load_targets(&self, set: &SetInfo) -> Result<Vec<crate::engine::Target>> {
        if set.targets.is_none() {
            return Ok(Vec::new());
        }
        let path = self.set_dir(&set.name).join(TARGETS);
        serde_json::from_slice(&fs::read(&path).with_context(|| path.display().to_string())?).with_context(|| path.display().to_string())
    }

    /// Every set as a summary (for another node's Sets view)
    pub fn set_summaries(&self) -> Result<Vec<SetSummary>> {
        let mut out = Vec::new();
        for name in self.sets()? {
            let s = self.load_set(&name)?;
            out.push(SetSummary { name, base: s.base.clone(), base_runs: s.base_runs.len(), runs: s.runs.clone(), files: s.files.len(), targets: s.targets.clone() });
        }
        Ok(out)
    }

    /// Throughput and image size of the latest finished run with at least 50 results, None without one
    pub fn last_stats(&mut self) -> Option<RunStats> {
        let mut ids: Vec<String> = fs::read_dir(self.root.join("runs")).ok()?.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        ids.sort();
        for id in ids.into_iter().rev() {
            let Ok(r) = self.run(&id).cloned() else { continue };
            let (Some(end), Ok(start)) = (r.finished.as_deref().and_then(|f| chrono::DateTime::parse_from_rfc3339(f).ok()), chrono::DateTime::parse_from_rfc3339(&r.started)) else { continue };
            let secs = (end - start).num_milliseconds() as f64 / 1000.0;
            if r.entities.len() < 50 || secs <= 0.0 {
                continue;
            }
            let dir = self.run_dir(&id);
            let sizes: Vec<u64> = r.entities.values().filter_map(|e| e.files.first()).filter_map(|f| fs::metadata(dir.join(f)).ok()).map(|m| m.len()).collect();
            let bytes_per_image = if sizes.is_empty() { 0 } else { sizes.iter().sum::<u64>() / sizes.len() as u64 };
            return Some(RunStats { run: id, items_per_s: r.entities.len() as f64 / secs, bytes_per_image });
        }
        None
    }

    /// Give a set the target list from a file (replaces its own); returns the number of targets
    pub fn set_targets(&self, set: &mut SetInfo, src: &Path) -> Result<usize> {
        let bytes = fs::read(src).with_context(|| src.display().to_string())?;
        let list: Vec<crate::engine::Target> = serde_json::from_slice(&bytes).with_context(|| format!("{}: not a target list", src.display()))?;
        if list.is_empty() {
            bail!("{}: no targets", src.display());
        }
        let path = self.set_dir(&set.name).join(TARGETS);
        let tmp = path.with_extension("json.part");
        fs::write(&tmp, &bytes)?;
        fs::rename(&tmp, &path)?;
        set.targets = Some(TargetsInfo { count: list.len(), source: src.display().to_string(), at: chrono::Local::now().to_rfc3339() });
        self.save_set(set)?;
        Ok(list.len())
    }

    pub fn resolve(&mut self, set: &SetInfo) -> Result<Resolved> {
        let mut r = Resolved::default();
        for id in set.chain().cloned().collect::<Vec<_>>() {
            let run = self.run(&id)?;
            for (entity, res) in &run.entities {
                r.apply(&id, entity, res);
            }
        }
        Ok(r)
    }

    /// Builds the set folder from the chain: create, replace and remove hardlinks, foreign PNGs gone.
    /// Returns (created, removed)
    pub fn sync(&mut self, set: &mut SetInfo) -> Result<(usize, usize)> {
        let resolved = self.resolve(set)?;
        let mut want: BTreeMap<String, String> = BTreeMap::new();
        for (run, files) in resolved.image.values() {
            for f in files {
                want.insert(f.clone(), run.clone());
            }
        }
        let dir = self.set_dir(&set.name);
        fs::create_dir_all(&dir)?;
        let (mut added, mut removed) = (0, 0);
        for (f, run) in &set.files {
            if want.get(f) != Some(run) {
                let keep = self.run_dir(run).join(f);
                remove_link(&dir.join(f), Some(&keep))?;
                removed += 1;
            }
        }
        for e in fs::read_dir(&dir)?.flatten() {
            let f = e.file_name().to_string_lossy().to_string();
            if f.to_lowercase().ends_with(".png") && !want.contains_key(&f) && !set.files.contains_key(&f) {
                remove_link(&e.path(), None)?;
                removed += 1;
            }
        }
        for (f, run) in &want {
            let path = dir.join(f);
            if set.files.get(f) == Some(run) && path.exists() {
                continue;
            }
            let src = self.run_dir(run).join(f);
            remove_link(&path, Some(&src))?;
            fs::hard_link(&src, &path).with_context(|| format!("{} -> {}", src.display(), path.display()))?;
            added += 1;
        }
        set.files = want;
        self.save_set(set)?;
        Ok((added, removed))
    }

    /// What deleting a run changes in a set: (entities that fall back to an older run, entities that lose their image)
    pub fn removal_impact(&mut self, set: &SetInfo, id: &str) -> Result<(usize, usize)> {
        let before = self.resolve(set)?;
        let mut without = set.clone();
        without.base_runs.retain(|r| r != id);
        without.runs.retain(|r| r != id);
        let after = self.resolve(&without)?;
        let (mut older, mut gone) = (0, 0);
        for (entity, (run, _)) in &before.image {
            if run == id {
                if after.image.contains_key(entity) { older += 1 } else { gone += 1 }
            }
        }
        Ok((older, gone))
    }

    /// Link an entity's images from a run into the set (during a run), the entity's old ones gone
    pub fn link_entity(&self, set: &mut SetInfo, old: &[String], run: &str, files: &[String]) -> Result<()> {
        let dir = self.set_dir(&set.name);
        for f in old {
            if let Some(prev) = set.files.remove(f) {
                remove_link(&dir.join(f), Some(&self.run_dir(&prev).join(f)))?;
            }
        }
        for f in files {
            let src = self.run_dir(run).join(f);
            if let Some(prev) = set.files.remove(f) {
                remove_link(&dir.join(f), Some(&self.run_dir(&prev).join(f)))?;
            }
            fs::hard_link(&src, dir.join(f)).with_context(|| format!("{} -> {}", src.display(), dir.join(f).display()))?;
            set.files.insert(f.clone(), run.to_string());
        }
        Ok(())
    }

    /// Adopt an experiment into live (key l): append its runs in their order to live's chain
    pub fn adopt(&mut self, name: &str) -> Result<usize> {
        if name == LIVE {
            bail!("live cannot be adopted into itself");
        }
        let exp = self.load_set(name)?;
        let mut live = self.load_set(LIVE)?;
        let new: Vec<String> = exp.runs.iter().filter(|r| !live.runs.contains(r)).cloned().collect();
        live.runs.extend(new.iter().cloned());
        self.sync(&mut live)?;
        Ok(new.len())
    }

    /// Delete a run (key x): remove from all sets (older ones show through again), rebuild sets, folder gone.
    /// Returns the affected sets
    pub fn delete_run(&mut self, id: &str) -> Result<Vec<String>> {
        let mut touched = Vec::new();
        for name in self.sets()? {
            let mut set = self.load_set(&name)?;
            let before = set.base_runs.len() + set.runs.len();
            set.base_runs.retain(|r| r != id);
            set.runs.retain(|r| r != id);
            if set.base_runs.len() + set.runs.len() != before {
                self.sync(&mut set)?;
                touched.push(name);
            }
        }
        remove_tree(&self.run_dir(id))?;
        self.runs.remove(id);
        Ok(touched)
    }

    /// Delete an experiment set: the folder and its runs that no other set uses any more
    pub fn delete_set(&mut self, name: &str) -> Result<usize> {
        if name == LIVE {
            bail!("live cannot be deleted");
        }
        let set = self.load_set(name)?;
        remove_tree(&self.set_dir(name))?;
        let mut used = HashSet::new();
        for other in self.sets()? {
            used.extend(self.load_set(&other)?.chain().cloned());
        }
        let mut n = 0;
        for r in set.runs.iter().filter(|r| !used.contains(*r)) {
            remove_tree(&self.run_dir(r))?;
            self.runs.remove(r);
            n += 1;
        }
        Ok(n)
    }

    /// Take a finished image into the run: move (otherwise copy) as the first file, the further UUIDs as
    /// hardlinks to it, all read-only
    pub fn put_image(&self, run: &str, src: &Path, files: &[String]) -> Result<()> {
        let dir = self.run_dir(run);
        let Some(first) = files.first() else { return Ok(()) };
        let dst = dir.join(first);
        remove_link(&dst, None)?;
        if fs::rename(src, &dst).is_err() {
            fs::copy(src, &dst).with_context(|| format!("{} -> {}", src.display(), dst.display()))?;
            let _ = fs::remove_file(src);
        }
        set_readonly(&dst, true)?;
        for f in &files[1..] {
            let p = dir.join(f);
            remove_link(&p, None)?;
            if fs::hard_link(&dst, &p).is_err() {
                fs::copy(&dst, &p).with_context(|| p.display().to_string())?;
                set_readonly(&p, true)?;
            }
        }
        Ok(())
    }

    /// The newest event log (run folders sort by date)
    pub fn latest_events(&self) -> Option<PathBuf> {
        let mut dirs: Vec<PathBuf> = fs::read_dir(self.root.join("runs")).ok()?.flatten().map(|e| e.path().join("events.jsonl")).filter(|p| p.exists()).collect();
        dirs.sort();
        dirs.pop()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("packshot-test-{name}-{}", std::process::id()));
        let _ = remove_tree(&d);
        d
    }

    /// Create a run with images: per entity (name, ok, content)
    fn add_run(store: &mut Store, set: &mut SetInfo, id: &str, items: &[(&str, bool, &str)]) {
        fs::create_dir_all(store.run_dir(id)).unwrap();
        let mut info = RunInfo { id: id.into(), name: id.into(), set: set.name.clone(), ..Default::default() };
        for (entity, ok, content) in items {
            let mut files = Vec::new();
            if *ok {
                let src = store.root.join("render.png");
                fs::write(&src, content).unwrap();
                files.push(format!("{entity} 0000.png"));
                store.put_image(id, &src, &files).unwrap();
            }
            info.entities.insert(entity.to_string(), EntityResult { ok: *ok, error: (!ok).then(|| "x".into()), files });
        }
        store.save_run(&info).unwrap();
        set.runs.push(id.into());
        store.sync(set).unwrap();
    }

    fn read(store: &Store, set: &str, entity: &str) -> Option<String> {
        fs::read_to_string(store.set_dir(set).join(format!("{entity} 0000.png"))).ok()
    }

    /// A set's target list: none in a new store, set from a file, inherited when branching, replaced per set, kept
    /// by the folder repair, gone with the set
    #[test]
    fn targets_per_set() {
        let root = tmp("targets");
        let store = Store::open(&root).unwrap();
        let mut live = store.load_set(LIVE).unwrap();
        assert!(live.targets.is_none() && store.load_targets(&live).unwrap().is_empty());
        let file = |name: &str, entities: &[&str]| {
            let list: Vec<serde_json::Value> = entities.iter().map(|e| serde_json::json!({ "uuids": ["0"], "entity": e, "type": "T", "subType": "S" })).collect();
            let p = root.join(name);
            fs::write(&p, serde_json::to_vec(&list).unwrap()).unwrap();
            p
        };
        assert_eq!(store.set_targets(&mut live, &file("a.json", &["x", "y"])).unwrap(), 2);
        assert!(store.set_targets(&mut live, &file("bad.json", &[])).is_err(), "an empty list is refused");
        let mut store = store;
        let mut exp = store.create_set("exp", LIVE).unwrap();
        assert_eq!(store.load_targets(&exp).unwrap().len(), 2, "inherited from live");
        assert_eq!(exp.targets.as_ref().unwrap().source, "<live>");
        store.set_targets(&mut exp, &file("b.json", &["x", "y", "z"])).unwrap();
        assert_eq!(store.load_targets(&store.load_set("exp").unwrap()).unwrap().len(), 3);
        assert_eq!(store.load_targets(&store.load_set(LIVE).unwrap()).unwrap().len(), 2, "live keeps its own");
        let mut live = store.load_set(LIVE).unwrap();
        store.sync(&mut live).unwrap();
        assert!(store.set_dir(LIVE).join(TARGETS).exists(), "the folder repair keeps it");
        store.delete_set("exp").unwrap();
        assert!(!store.set_dir("exp").exists());
        remove_tree(&root).unwrap();
    }

    /// The estimate's basis: the latest finished run with at least 50 results, its items per second and image size
    #[test]
    fn last_stats() {
        let root = tmp("stats");
        let mut store = Store::open(&root).unwrap();
        assert!(store.last_stats().is_none());
        let mut info = RunInfo { id: "2026-11-04_10-00_big".into(), name: "big".into(), set: LIVE.into(), node: "a".into(), started: "2026-11-04T10:00:00+01:00".into(), finished: Some("2026-11-04T10:00:20+01:00".into()), ..Default::default() };
        fs::create_dir_all(store.run_dir(&info.id)).unwrap();
        for k in 0..100 {
            let f = format!("e{k} 0000.png");
            fs::write(store.run_dir(&info.id).join(&f), vec![0u8; 1000]).unwrap();
            info.entities.insert(format!("e{k}"), EntityResult { ok: true, error: None, files: vec![f] });
        }
        store.save_run(&info).unwrap();
        // a newer, small run does not count
        let small = RunInfo { id: "2026-11-04_11-00_small".into(), started: "2026-11-04T11:00:00+01:00".into(), finished: Some("2026-11-04T11:00:01+01:00".into()), ..info.clone() };
        let mut small = small;
        small.entities = info.entities.iter().take(5).map(|(k, v)| (k.clone(), v.clone())).collect();
        fs::create_dir_all(store.run_dir(&small.id)).unwrap();
        store.save_run(&small).unwrap();
        let st = store.last_stats().unwrap();
        assert_eq!((st.run.as_str(), st.items_per_s, st.bytes_per_image), ("2026-11-04_10-00_big", 5.0, 1000));
        remove_tree(&root).unwrap();
    }

    #[test]
    fn chain_branch_adopt_delete() {
        let root = tmp("chain");
        let mut store = Store::open(&root).unwrap();
        let mut live = store.load_set(LIVE).unwrap();
        add_run(&mut store, &mut live, "r1", &[("a", true, "a1"), ("b", true, "b1"), ("c", false, "")]);
        add_run(&mut store, &mut live, "r2", &[("a", true, "a2"), ("b", false, "")]);
        assert_eq!(read(&store, LIVE, "a").as_deref(), Some("a2"));
        // b: old image stays, but counts as failed
        assert_eq!(read(&store, LIVE, "b").as_deref(), Some("b1"));
        let r = store.resolve(&live).unwrap();
        assert!(r.failed.contains("b") && r.failed.contains("c") && !r.failed.contains("a"));
        // images read-only, also via the set link
        assert!(fs::metadata(store.set_dir(LIVE).join("a 0000.png")).unwrap().permissions().readonly());

        // experiment: branches off, own run overrides only there
        let mut exp = store.create_set("exp", LIVE).unwrap();
        assert_eq!(read(&store, "exp", "a").as_deref(), Some("a2"));
        add_run(&mut store, &mut exp, "r3", &[("a", true, "a3"), ("c", true, "c3")]);
        assert_eq!(read(&store, "exp", "a").as_deref(), Some("a3"));
        assert_eq!(read(&store, LIVE, "a").as_deref(), Some("a2"));
        assert_eq!(read(&store, LIVE, "c"), None);

        // adopt: r3 attaches to live
        assert_eq!(store.adopt("exp").unwrap(), 1);
        let live = store.load_set(LIVE).unwrap();
        assert_eq!(live.runs, vec!["r1", "r2", "r3"]);
        assert_eq!(read(&store, LIVE, "a").as_deref(), Some("a3"));
        assert_eq!(read(&store, LIVE, "c").as_deref(), Some("c3"));

        // delete a run from the middle of live: r1 shows through for b, a still comes from r3
        let touched = store.delete_run("r2").unwrap();
        assert_eq!(touched, vec!["exp".to_string(), LIVE.to_string()]);
        assert_eq!(read(&store, LIVE, "b").as_deref(), Some("b1"));
        assert!(!store.resolve(&store.load_set(LIVE).unwrap()).unwrap().failed.contains("b"));
        assert!(!store.run_dir("r2").exists());
        // r1's images are still read-only after the links are removed
        assert!(fs::metadata(store.run_dir("r1").join("b 0000.png")).unwrap().permissions().readonly());

        // delete r3: a falls back to r1, c is missing again
        store.delete_run("r3").unwrap();
        assert_eq!(read(&store, LIVE, "a").as_deref(), Some("a1"));
        assert_eq!(read(&store, LIVE, "c"), None);

        // delete set: folder gone, live stays
        store.delete_set("exp").unwrap();
        assert!(!store.set_dir("exp").exists());
        assert_eq!(store.sets().unwrap(), vec![LIVE.to_string()]);
        remove_tree(&root).unwrap();
    }

    #[test]
    fn sync_repairs_folder() {
        let root = tmp("sync");
        let mut store = Store::open(&root).unwrap();
        let mut live = store.load_set(LIVE).unwrap();
        add_run(&mut store, &mut live, "r1", &[("a", true, "a1"), ("b", true, "b1")]);
        // deleted link comes back, foreign PNG gets thrown out
        remove_link(&store.set_dir(LIVE).join("a 0000.png"), None).unwrap();
        fs::write(store.set_dir(LIVE).join("stray.png"), "x").unwrap();
        let (added, removed) = store.sync(&mut live).unwrap();
        assert_eq!((added, removed), (1, 1));
        assert_eq!(read(&store, LIVE, "a").as_deref(), Some("a1"));
        assert!(!store.set_dir(LIVE).join("stray.png").exists());
        remove_tree(&root).unwrap();
    }

    #[test]
    fn link_entity_replaces_old_names() {
        let root = tmp("link");
        let mut store = Store::open(&root).unwrap();
        let mut live = store.load_set(LIVE).unwrap();
        add_run(&mut store, &mut live, "r1", &[("a", true, "a1")]);
        // new display name: different file, the old one disappears
        fs::create_dir_all(store.run_dir("r2")).unwrap();
        let src = root.join("render.png");
        fs::write(&src, "a2").unwrap();
        let files = vec!["A neu 0000.png".to_string()];
        store.put_image("r2", &src, &files).unwrap();
        store.link_entity(&mut live, &["a 0000.png".to_string()], "r2", &files).unwrap();
        assert!(!store.set_dir(LIVE).join("a 0000.png").exists());
        assert_eq!(fs::read_to_string(store.set_dir(LIVE).join("A neu 0000.png")).unwrap(), "a2");
        assert!(fs::metadata(store.run_dir("r1").join("a 0000.png")).unwrap().permissions().readonly());
        remove_tree(&root).unwrap();
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("arms legs, helmet"), "arms-legs-helmet");
        assert_eq!(slug("_arms_"), "_arms_");
        assert_eq!(slug("  "), "");
    }

    /// --report opens the packshot folder without creating anything in it
    #[test]
    fn open_read_creates_nothing() {
        let d = tmp("read");
        let store = Store::open_read(&d);
        assert!(store.latest_events().is_none() && !d.exists());
    }
}
