// The work for a run's head node, on every node, the head node's own included (its engine talks to its own Renderer
// through LOCAL instead of a connection). Export chunks: export-batch exports each chunk into <work>/export/x<id>;
// the node then holds the exported items until the head node releases the chunk, describes them to rendering nodes
// (closure and hashes, cas.rs) and sends them the files they lack. Render chunks: items held here render straight
// from their export folder, the others are fetched from their holders into the content-addressed cache (once per
// hash and run) and linked into the chunk's folder (<work>/fetch/), laid out as on the holder; then a Blender renders
// them and every PNG goes back to the head node with its icon: line. The processes are long-lived: one Blender per
// render slot (render.py --stdin, fed one job at a time, restarted after BLENDER_JOBS jobs or BLENDER_RSS of
// memory) and one export-batch --stdin per export slot when StarBreaker has that mode (else one process per export
// chunk); an idle one quits after IDLE_STOP. It writes only into its work folder.
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::json;

use crate::cas::{Cas, HashedFile, Index, closure, hash_file, link_or_copy};
use crate::net::{FileEntry, HeldItem, Hub, ItemJob, Msg, Payload, RenderJob, safe_hash, safe_rel};

/// The "connection" of this node's own head node work: the head node's engine and its own Renderer
pub const LOCAL: u64 = 0;

/// The longest path Blender on Windows reads (MAX_PATH without the terminating NUL); a longer one is skipped, its
/// texture renders white
pub const WIN_MAX_PATH: usize = 259;

/// A Blender restarts after this many jobs (each job opens a new file, but Python's and the GPU driver's caches
/// grow over the hours of a full run)
pub const BLENDER_JOBS: usize = 200;
/// ... or once its resident memory exceeds this (a Blender takes 0.5-1.5 GB over an armour chunk)
pub const BLENDER_RSS: u64 = 3 << 30;
/// A persistent export-batch restarts after this many export chunks ...
pub const EXPORT_CHUNKS: usize = 40;
/// ... or once its resident memory exceeds this (glibc's arenas fragment: 0.75 -> 1.4 GB over 300 jobs, ~1.0 GB with
/// MALLOC_ARENA_MAX=2, which it is started with on Linux)
pub const EXPORT_RSS: u64 = 2 << 30;
/// An idle process quits after this long without work (its memory goes back)
pub const IDLE_STOP: Duration = Duration::from_secs(60);
/// A process asked to quit that is still there after this long is killed
const QUIT_GRACE: Duration = Duration::from_secs(10);
/// Jobs fed to a Blender beyond the one it renders: the next line waits in its stdin, so the main loop's tick does
/// not delay it
const LOOKAHEAD: usize = 1;

/// A render job whose files are on this node
struct Ready {
    item: usize,
    entity: String,
    src: PathBuf,
    rig: PathBuf,
    pose: [f64; 3],
}

enum Work {
    Export(Vec<ItemJob>),
    Render { dir: PathBuf, script: PathBuf, script_dir: Option<PathBuf>, size: u32, jobs: Vec<Ready> },
}

/// Work waiting for a free process
struct Queued {
    head: u64,
    id: u64,
    work: Work,
}

/// An export chunk being exported: its jobs and the packages exported so far (item -> package folder, relative)
struct ExportState {
    jobs: Vec<ItemJob>,
    packages: HashMap<usize, String>,
    item_secs: f64,
    /// the job lines for a persistent export-batch, then {"end_chunk": dir}; sent once its process is ready
    lines: Vec<String>,
    sent: bool,
    /// its process answered chunk_done: the chunk is over, the process stays
    done: bool,
    /// items with a result line
    answered: HashSet<usize>,
}

/// What a process is
#[derive(Clone, PartialEq, Debug)]
enum ProcKind {
    /// render.py --stdin (the content hash of its render.py: a chunk with another one gets another process)
    Blender(String),
    /// export-batch --stdin
    Export,
    /// export-batch over one chunk's jobs file (StarBreaker without --stdin): ends with its chunk
    ExportOnce,
}

/// A Blender or export-batch process
struct Proc {
    kind: ProcKind,
    child: Child,
    stdin: Option<ChildStdin>,
    /// it said ready (Blender "ready:", export-batch {"ready":true})
    ready: bool,
    /// results it gave since it started
    jobs: usize,
    eof: u8,
    /// the running chunk it works for
    chunk: Option<u64>,
    /// asked to quit: no new work, killed after QUIT_GRACE
    quit_at: Option<Instant>,
    idle_since: Instant,
    /// its output while it works for no chunk
    tail: String,
}

/// A chunk at work: an export chunk, or a render chunk whose files are here
struct Running {
    head: u64,
    id: u64,
    dir: PathBuf,
    /// the process working on it (None: waiting for one)
    proc: Option<u64>,
    /// PNG path (lower case) -> item
    outs: HashMap<String, usize>,
    /// item -> class name (preview)
    names: HashMap<usize, String>,
    reported: usize,
    total: usize,
    /// its processes' output since the last result (both streams): a failure carries it
    stderr_tail: String,
    started: Instant,
    export: Option<ExportState>,
    /// the head node cancelled it: nothing of it is kept
    cancelled: bool,
    /// the inbox folder render.py came in (deleted with the chunk)
    script_dir: Option<PathBuf>,
    /// a render chunk: its jobs (item, JSON line) not fed yet, those fed and not answered yet (in order), its
    /// render.py and that file's content hash
    todo: VecDeque<(usize, String)>,
    fed: VecDeque<(usize, String)>,
    script: PathBuf,
    script_hash: String,
}

/// An exported item held here: package folder, rig (relative to the chunk's folder) and pose
struct HeldInfo {
    package: String,
    rig: String,
    pose: [f64; 3],
}

/// An export chunk held here until its head node releases it
struct Held {
    root: PathBuf,
    items: HashMap<usize, HeldInfo>,
    index: Option<Index>,
    /// relative path -> (size, hash), each file hashed once
    hashes: HashMap<String, (u64, String)>,
}

/// The items of a render chunk held by one node in one export chunk
struct Group {
    holder: String,
    conn: u64,
    chunk: u64,
    entities: HashMap<usize, String>,
    info: Option<Vec<HeldItem>>,
}

/// A render chunk getting its files
struct Pending {
    head: u64,
    id: u64,
    dir: PathBuf,
    script: PathBuf,
    script_dir: Option<PathBuf>,
    size: u32,
    ready: Vec<Ready>,
    groups: Vec<Group>,
    started: Instant,
    holders: Vec<String>,
    /// unique files: bytes to fetch, bytes already here (cache or on their way for another chunk), count
    bytes: u64,
    cached: u64,
    files: usize,
    seen: HashSet<String>,
    /// fetched for an assessment's test set: no Blender, the folder stays (Renderer::kept)
    keep: bool,
    /// some of its items are held elsewhere: the head node waits for Fetched (also when none of their holders was
    /// reachable and only the items held here are left)
    remote: bool,
}

/// A test set fetched for an assessment: its folder (x<chunk>/ laid out as on the holder) and per item the scene, rig
/// and pose
pub struct Kept {
    pub id: u64,
    pub dir: PathBuf,
    pub items: Vec<(usize, PathBuf, PathBuf, [f64; 3])>,
}

/// An item of a held export chunk taken out: item, package folder, rig (relative to the chunk's folder), pose
pub type HeldTuple = (usize, String, String, [f64; 3]);

/// What a node with --export needs to export
pub struct ExportTools {
    pub starbreaker: PathBuf,
    pub p4k: PathBuf,
}

enum Line {
    Out { key: u64, line: String, stderr: bool },
    Eof { key: u64 },
}

pub fn addon_dir(blender: &Path) -> PathBuf {
    blender.parent().unwrap_or(Path::new(".")).join("portable/scripts/addons/starbreaker_addon")
}

/// A message to the head node (LOCAL: into `local`, the engine takes it from there) or to another node
fn post(local: &mut Vec<(Msg, Vec<u8>)>, hub: Option<&Hub>, conn: u64, msg: Msg, payload: Payload) {
    if conn == LOCAL {
        let data = match payload {
            Payload::Bytes(b) => b,
            _ => Vec::new(),
        };
        local.push((msg, data));
    } else if let Some(h) = hub {
        h.send(conn, msg, payload);
    }
}

/// Why these files of an item cannot be linked under `root` for Blender on Windows: a path over WIN_MAX_PATH (Blender
/// reads none: its textures render white), or one that differs from another only in case (NTFS keeps one file for
/// both); `paths`: the item's files relative to `root`; `linked`: lower-case path -> as linked so far in this folder
fn win_path_problem(root: &Path, paths: &[&str], linked: &HashMap<String, String>) -> Option<String> {
    let mut mine: HashMap<String, &str> = HashMap::new();
    for &path in paths {
        let full = root.join(path);
        let n = full.to_string_lossy().encode_utf16().count();
        if n > WIN_MAX_PATH {
            return Some(format!("path of {n} characters, over {WIN_MAX_PATH} ({}): Blender on Windows would read none, put --work in a shorter folder", full.display()));
        }
        let key = path.to_lowercase();
        if let Some(other) = linked.get(&key).map(String::as_str).or(mine.get(&key).copied()).filter(|o| *o != path) {
            return Some(format!("{path} and {other} differ only in case: one file on this file system"));
        }
        mine.insert(key, path);
    }
    None
}

/// Exports, holds, fetches and renders what a run's head node hands this node
pub struct Renderer {
    blender: PathBuf,
    /// Blender processes at the same time
    pub slots: usize,
    /// export-batch processes at the same time
    pub export_slots: usize,
    /// set with --export: exports
    pub tools: Option<ExportTools>,
    /// this node's host name, work folder and render.py (for the head node's own work, the others send theirs)
    name: String,
    work: PathBuf,
    script: PathBuf,
    /// copy of the latest image rendered here and its class name (the preview of a node that is not the head node)
    preview: PathBuf,
    pub last: Option<(PathBuf, String)>,
    queue: VecDeque<Queued>,
    /// chunks at work, by key
    running: HashMap<u64, Running>,
    /// the Blender and export-batch processes, by key (their output lines carry it)
    procs: HashMap<u64, Proc>,
    /// StarBreaker's export-batch has --stdin (probed once, at the first export chunk)
    export_stdin: Option<bool>,
    /// resident memory of the Blender processes (BLENDER_RSS)
    sys: Option<sysinfo::System>,
    /// a Blender restarts after this many jobs, a persistent export-batch after this many chunks (BLENDER_JOBS,
    /// EXPORT_CHUNKS; tests shorten them)
    pub blender_jobs: usize,
    pub export_chunks: usize,
    /// export chunks held here: (head node, chunk) -> folder and items
    held: HashMap<(String, u64), Held>,
    /// hash -> the files with that content in the held chunks (what Fetch sends; a chunk released takes only its own)
    by_hash: HashMap<String, Vec<PathBuf>>,
    pending: Vec<Pending>,
    cas: Cas,
    /// the head node whose render chunks filled the cache (it goes with that head node)
    cache_head: Option<u64>,
    /// hashes asked for and not here yet -> the connection asked
    inflight: HashMap<String, u64>,
    /// Fetch requests: id -> (connection, hashes)
    requests: HashMap<u64, (u64, Vec<String>)>,
    next_req: u64,
    /// messages to the head node's own engine (LOCAL)
    pub local: Vec<(Msg, Vec<u8>)>,
    /// test sets fetched for an assessment (fetch_keep)
    kept: Vec<Kept>,
    blender_log: Option<File>,
    /// render chunks are linked for Blender on Windows: paths over WIN_MAX_PATH and paths differing only in case
    /// (NTFS) fail their item (true on Windows; tests set it)
    win_paths: bool,
    tx: Sender<Line>,
    rx: Receiver<Line>,
    next_key: u64,
}

impl Renderer {
    /// `name`: this node's host name; `script`: render.py for the head node's own work
    pub fn new(name: &str, work: &Path, blender: PathBuf, script: PathBuf, slots: usize) -> Self {
        let (tx, rx) = channel();
        Self {
            blender,
            slots,
            export_slots: 1,
            tools: None,
            name: name.to_string(),
            work: work.to_path_buf(),
            script,
            preview: work.join("preview.png"),
            last: None,
            queue: VecDeque::new(),
            running: HashMap::new(),
            procs: HashMap::new(),
            export_stdin: None,
            sys: None,
            blender_jobs: BLENDER_JOBS,
            export_chunks: EXPORT_CHUNKS,
            held: HashMap::new(),
            by_hash: HashMap::new(),
            pending: Vec::new(),
            cas: Cas::new(work.join("cas")),
            cache_head: None,
            inflight: HashMap::new(),
            requests: HashMap::new(),
            next_req: 1,
            local: Vec::new(),
            kept: Vec::new(),
            blender_log: None,
            win_paths: cfg!(windows),
            tx,
            rx,
            next_key: 1,
        }
    }

    fn peer_name(&self, hub: Option<&Hub>, conn: u64) -> String {
        if conn == LOCAL {
            return self.name.clone();
        }
        hub.and_then(|h| h.peer(conn)).map(|p| p.hello.name.clone()).unwrap_or_default()
    }

    /// A line into <work>/blender.log (Blender's whole output, appended)
    pub fn blender_log(&mut self, line: &str) {
        if self.blender_log.is_none() {
            self.blender_log = OpenOptions::new().create(true).append(true).open(self.work.join("blender.log")).ok();
        }
        if let Some(f) = self.blender_log.as_mut() {
            let _ = writeln!(f, "{line}");
        }
    }

    /// Bytes in the cache of fetched files
    pub fn cached_bytes(&self) -> u64 {
        self.cas.bytes()
    }

    /// Work queued, fetching, running or held here
    pub fn busy(&self) -> bool {
        !self.queue.is_empty() || !self.running.is_empty() || !self.pending.is_empty() || !self.held.is_empty()
    }

    /// An export chunk held here taken out of the holdings (an assessment's test set exported here): its folder and
    /// per item package, rig (relative) and pose
    pub fn take_held(&mut self, head: &str, chunk: u64) -> Option<(PathBuf, Vec<HeldTuple>)> {
        let h = self.held.remove(&(head.to_string(), chunk))?;
        self.forget_files(&h.root);
        let mut items: Vec<_> = h.items.into_iter().map(|(i, x)| (i, x.package, x.rig, x.pose)).collect();
        items.sort_by_key(|x| x.0);
        Some((h.root, items))
    }

    /// Fetch items held on other nodes into `dir` as a render chunk would (an assessment's test set), without
    /// rendering them; take_kept gives the result
    pub fn fetch_keep(&mut self, hub: Option<&Hub>, id: u64, jobs: Vec<RenderJob>, dir: PathBuf, log: &mut dyn FnMut(String)) {
        let script = self.script.clone();
        self.start_fetch(hub, LOCAL, id, 0, jobs, script, None, Some(dir), log);
    }

    /// The test set fetched as `id`, once all of it is here
    pub fn take_kept(&mut self, id: u64) -> Option<Kept> {
        let i = self.kept.iter().position(|k| k.id == id)?;
        Some(self.kept.remove(i))
    }

    fn drop_held(&mut self, key: &(String, u64)) {
        if let Some(h) = self.held.remove(key) {
            self.forget_files(&h.root);
            let _ = fs::remove_dir_all(&h.root);
        }
    }

    /// The files of a held chunk leave by_hash; the same content in another held chunk stays
    fn forget_files(&mut self, root: &Path) {
        self.by_hash.retain(|_, paths| {
            paths.retain(|p| !p.starts_with(root));
            !paths.is_empty()
        });
    }

    /// A connection went away: as a head node, its work goes (processes stopped, folders, holdings and the cache
    /// deleted); as a holder, what was fetched from it stops (its items are back in the head node's queue)
    pub fn on_down(&mut self, hub: Option<&Hub>, conn: u64, name: &str) {
        let head = self.queue.iter().any(|q| q.head == conn)
            || self.pending.iter().any(|p| p.head == conn)
            || self.running.values().any(|r| r.head == conn)
            || self.held.keys().any(|(h, _)| h == name)
            || self.cache_head == Some(conn);
        self.queue.retain(|q| {
            let keep = q.head != conn;
            if !keep && let Work::Render { dir, script_dir, .. } = &q.work {
                let _ = fs::remove_dir_all(dir);
                if let Some(d) = script_dir {
                    let _ = fs::remove_dir_all(d);
                }
            }
            keep
        });
        self.pending.retain(|p| {
            let keep = p.head != conn;
            if !keep {
                let _ = fs::remove_dir_all(&p.dir);
                if let Some(d) = &p.script_dir {
                    let _ = fs::remove_dir_all(d);
                }
            }
            keep
        });
        let keys: Vec<u64> = self.running.iter().filter(|(_, r)| r.head == conn).map(|(k, _)| *k).collect();
        for k in keys {
            self.cancel_running(k);
        }
        let gone: Vec<(String, u64)> = self.held.keys().filter(|(h, _)| h == name).cloned().collect();
        for k in gone {
            self.drop_held(&k);
        }
        if head {
            self.clear_cache();
        }
        // as a holder: the items to fetch from it go back to their head node (it places them again)
        self.requests.retain(|_, (c, _)| *c != conn);
        self.inflight.retain(|_, c| *c != conn);
        for p in &mut self.pending {
            let (gone, keep): (Vec<Group>, Vec<Group>) = p.groups.drain(..).partition(|g| g.conn == conn);
            p.groups = keep;
            let items: Vec<usize> = gone.iter().flat_map(|g| g.entities.keys().copied()).collect();
            if !items.is_empty() {
                let reason = format!("{name} disconnected from {}", self.name);
                post(&mut self.local, hub, p.head, Msg::Unfetched { block: p.id, items, reason, export: false }, Payload::None);
            }
        }
        self.advance(hub);
    }

    fn clear_cache(&mut self) {
        self.cache_head = None;
        self.cas.clear();
        self.inflight.clear();
        self.requests.clear();
    }

    /// The run of head node `name` (connection `conn`) is over: its holdings and (if it filled it) the cache go
    pub fn end_run(&mut self, conn: u64, name: &str) {
        let gone: Vec<(String, u64)> = self.held.keys().filter(|(h, _)| h == name).cloned().collect();
        for k in gone {
            self.drop_held(&k);
        }
        self.pending.retain(|p| {
            let keep = p.head != conn;
            if !keep {
                let _ = fs::remove_dir_all(&p.dir);
            }
            keep
        });
        if self.cache_head.is_none_or(|c| c == conn) {
            self.clear_cache();
        }
        // the run's processes are not needed until the next one: their memory goes back
        self.quit_idle();
    }

    /// Export, Render, Cancel, Release and the fetch messages; returns false for any other message
    pub fn on_msg(&mut self, hub: Option<&Hub>, conn: u64, msg: Msg, dir: Option<PathBuf>, log: &mut dyn FnMut(String)) -> bool {
        match msg {
            Msg::Export { id, jobs } => {
                if self.tools.is_none() {
                    post(&mut self.local, hub, conn, Msg::ExportDone { id, exit: None, wall: 0.0, item_seconds: 0.0, error: "no Data.p4k here: exports nothing".into(), long_paths: 0, failed: Vec::new() }, Payload::None);
                    return true;
                }
                if conn != LOCAL {
                    log(format!("export chunk {id} from {}: {} items", self.peer_name(hub, conn), jobs.len()));
                }
                self.queue.push_back(Queued { head: conn, id, work: Work::Export(jobs) });
            }
            Msg::Render { id, size, jobs, .. } => {
                let (script, script_dir) = match dir {
                    Some(d) => (d.join("render.py"), Some(d)),
                    None => (self.script.clone(), None),
                };
                self.start_fetch(hub, conn, id, size, jobs, script, script_dir, None, log);
            }
            Msg::Cancel { id } => {
                self.queue.retain(|q| {
                    let keep = !(q.head == conn && q.id == id);
                    if let (false, Work::Render { dir, script_dir, .. }) = (keep, &q.work) {
                        let _ = fs::remove_dir_all(dir);
                        if let Some(d) = script_dir {
                            let _ = fs::remove_dir_all(d);
                        }
                    }
                    keep
                });
                self.pending.retain(|p| {
                    let keep = !(p.head == conn && p.id == id);
                    if !keep {
                        let _ = fs::remove_dir_all(&p.dir);
                    }
                    keep
                });
                let keys: Vec<u64> = self.running.iter().filter(|(_, r)| r.head == conn && r.id == id).map(|(k, _)| *k).collect();
                for k in keys {
                    self.cancel_running(k);
                }
                // an export chunk that ended before the cancel came: nothing of it is kept
                let head = self.peer_name(hub, conn);
                self.drop_held(&(head, id));
            }
            Msg::Release { chunks } => {
                let head = self.peer_name(hub, conn);
                for c in chunks {
                    self.drop_held(&(head.clone(), c));
                }
            }
            Msg::GetItems { head, chunk, items } => {
                let (items, missing, gone) = self.describe(&head, chunk, &items);
                post(&mut self.local, hub, conn, Msg::ItemsInfo { head, chunk, items, missing, gone }, Payload::None);
            }
            Msg::ItemsInfo { head, chunk, items, missing, gone } => self.on_items(hub, conn, &head, chunk, items, missing, gone),
            Msg::Fetch { id, hashes } => self.serve_fetch(hub, conn, id, hashes),
            Msg::FetchData { id, files, missing } => {
                // an answer nobody waits for any more (its cache cleared, its holder dropped), or from another
                // connection than the one asked: dropped, the real request stays
                let ours = self.requests.get(&id).is_some_and(|(c, _)| *c == conn);
                let Some((from, asked)) = ours.then(|| self.requests.remove(&id)).flatten() else {
                    if let Some(d) = dir {
                        let _ = fs::remove_dir_all(d);
                    }
                    return true;
                };
                let mut gone: HashSet<String> = missing.into_iter().collect();
                if let Some(d) = dir.as_ref() {
                    for f in &files {
                        if let Err(e) = self.cas.put(&f.path, &d.join(&f.path)) {
                            log(format!("fetched file not taken: {e:#}"));
                            gone.insert(f.path.clone());
                        }
                    }
                    let _ = fs::remove_dir_all(d);
                }
                for h in asked {
                    if self.inflight.get(&h) == Some(&from) {
                        self.inflight.remove(&h);
                    }
                }
                self.unfetchable(hub, from, &gone);
                self.advance(hub);
            }
            _ => return false,
        }
        true
    }

    /// The items of an export chunk held here with their closures and hashes; the ones not held here with why; true
    /// when the chunk itself is not held here (restarted, or released already)
    fn describe(&mut self, head: &str, chunk: u64, items: &[usize]) -> (Vec<HeldItem>, Vec<(usize, String)>, bool) {
        let me = self.name.clone();
        let Some(h) = self.held.get_mut(&(head.to_string(), chunk)) else {
            return (Vec::new(), items.iter().map(|&i| (i, format!("export chunk {chunk} not held on {me}"))).collect(), true);
        };
        if h.index.is_none() {
            match Index::build(&h.root) {
                Ok(i) => h.index = Some(i),
                Err(e) => return (Vec::new(), items.iter().map(|&i| (i, format!("export chunk {chunk} on {me}: {e:#}"))).collect(), false),
            }
        }
        let mut out = Vec::new();
        let mut missing = Vec::new();
        for &item in items {
            let Some(info) = h.items.get(&item) else {
                missing.push((item, format!("not exported on {me}")));
                continue;
            };
            let res = (|| -> Result<Vec<HashedFile>> {
                let mut rels = closure(&h.root, h.index.as_ref().unwrap(), &info.package)?;
                rels.push(info.rig.clone());
                let mut files = Vec::new();
                for rel in rels {
                    let (size, hash) = match h.hashes.get(&rel) {
                        Some(v) => v.clone(),
                        None => {
                            let v = hash_file(&h.root.join(&rel))?;
                            h.hashes.insert(rel.clone(), v.clone());
                            v
                        }
                    };
                    let path = h.root.join(&rel);
                    let paths = self.by_hash.entry(hash.clone()).or_default();
                    if !paths.contains(&path) {
                        paths.push(path);
                    }
                    files.push(HashedFile { path: rel, size, hash });
                }
                Ok(files)
            })();
            match res {
                Ok(files) => out.push(HeldItem { item, package: info.package.clone(), pose: info.pose, rig: info.rig.clone(), files }),
                Err(e) => missing.push((item, format!("on {me}: {e:#}"))),
            }
        }
        (out, missing, false)
    }

    /// Blender on Windows and an item held here: the same path check as for a fetched item, over its closure in the
    /// held folder (`linked`: per export chunk, the paths checked so far)
    fn held_win_problem(&mut self, key: &(String, u64), package: &str, rig: &str, linked: &mut HashMap<u64, HashMap<String, String>>) -> Option<String> {
        let me = self.name.clone();
        let h = self.held.get_mut(key)?;
        if h.index.is_none() {
            match Index::build(&h.root) {
                Ok(i) => h.index = Some(i),
                Err(e) => return Some(format!("export chunk {} on {me}: {e:#}", key.1)),
            }
        }
        let mut rels = match closure(&h.root, h.index.as_ref().unwrap(), package) {
            Ok(r) => r,
            Err(e) => return Some(format!("on {me}: {e:#}")),
        };
        rels.push(rig.to_string());
        let paths: Vec<&str> = rels.iter().map(String::as_str).collect();
        let seen = linked.entry(key.1).or_default();
        let why = win_path_problem(&h.root, &paths, seen);
        if why.is_none() {
            seen.extend(rels.iter().map(|r| (r.to_lowercase(), r.clone())));
        }
        why
    }

    /// Send the files with these hashes (linked into a folder of their own first, so a release meanwhile does not
    /// pull them away under the sending thread)
    fn serve_fetch(&mut self, hub: Option<&Hub>, conn: u64, id: u64, hashes: Vec<String>) {
        let Some(hub) = hub else { return };
        self.next_req += 1;
        let dir = self.work.join("send").join(format!("{}-{}", std::process::id(), self.next_req));
        let mut files = Vec::new();
        let mut paths = Vec::new();
        let mut missing = Vec::new();
        for h in hashes {
            let srcs = self.by_hash.get(&h).map(Vec::as_slice).unwrap_or(&[]);
            let linked = srcs.iter().find_map(|src| link_or_copy(src, &dir.join(&h)).and_then(|_| Ok(fs::metadata(dir.join(&h))?.len())).ok());
            match linked {
                Some(size) => {
                    paths.push(dir.join(&h));
                    files.push(FileEntry { path: h, size });
                }
                None => missing.push(h),
            }
        }
        hub.send_then_remove(conn, Msg::FetchData { id, files, missing }, Payload::Files(paths), dir);
    }

    /// A render chunk arrives: items held here are ready at once, the others' holders are asked for their files.
    /// `keep`: only fetch them into that folder (an assessment's test set)
    #[allow(clippy::too_many_arguments)]
    fn start_fetch(&mut self, hub: Option<&Hub>, conn: u64, id: u64, size: u32, jobs: Vec<RenderJob>, script: PathBuf, script_dir: Option<PathBuf>, keep: Option<PathBuf>, log: &mut dyn FnMut(String)) {
        let head = self.peer_name(hub, conn);
        self.next_key += 1;
        let dir = keep.clone().unwrap_or_else(|| self.work.join("fetch").join(format!("{id}-{}", self.next_key)));
        let keep = keep.is_some();
        let remote = jobs.iter().any(|j| j.holder != self.name);
        let mut p = Pending { head: conn, id, dir, script, script_dir, size, ready: Vec::new(), groups: Vec::new(), started: Instant::now(), holders: Vec::new(), bytes: 0, cached: 0, files: 0, seen: HashSet::new(), keep, remote };
        let mut failed = Vec::new();
        let mut unreachable = Vec::new();
        // held here once, the chunk gone since (released, or this node restarted): exported again, as when another
        // holder answers `gone`
        let mut not_held: Vec<(u64, Vec<usize>)> = Vec::new();
        let mut linked = HashMap::new();
        for j in jobs {
            if j.holder == self.name {
                let key = (head.clone(), j.chunk);
                let Some(h) = self.held.get(&key) else {
                    match not_held.iter_mut().find(|(c, _)| *c == j.chunk) {
                        Some((_, items)) => items.push(j.item),
                        None => not_held.push((j.chunk, vec![j.item])),
                    }
                    continue;
                };
                let Some((root, package, rig, pose)) = h.items.get(&j.item).map(|i| (h.root.clone(), i.package.clone(), i.rig.clone(), i.pose)) else {
                    failed.push((j.item, format!("not exported on {}", self.name)));
                    continue;
                };
                // Blender on Windows: the same path check as for fetched items
                if self.win_paths && let Some(why) = self.held_win_problem(&key, &package, &rig, &mut linked) {
                    failed.push((j.item, why));
                    continue;
                }
                p.ready.push(Ready { item: j.item, entity: j.entity, src: root.join(&package).join("scene.blend"), rig: root.join(&rig), pose });
                continue;
            }
            if let Some(g) = p.groups.iter_mut().find(|g| g.holder == j.holder && g.chunk == j.chunk) {
                g.entities.insert(j.item, j.entity);
                continue;
            }
            let Some(c) = hub.and_then(|h| h.peers.iter().find(|p| p.hello.name == j.holder)).map(|p| p.conn) else {
                unreachable.push((j.item, format!("{} holds it and is not connected to {}", j.holder, self.name)));
                continue;
            };
            if !p.holders.contains(&j.holder) {
                p.holders.push(j.holder.clone());
            }
            self.cache_head = Some(conn);
            p.groups.push(Group { holder: j.holder, conn: c, chunk: j.chunk, entities: HashMap::from([(j.item, j.entity)]), info: None });
        }
        for g in &p.groups {
            let items: Vec<usize> = g.entities.keys().copied().collect();
            post(&mut self.local, hub, g.conn, Msg::GetItems { head: head.clone(), chunk: g.chunk, items }, Payload::None);
        }
        for (item, info) in failed {
            post(&mut self.local, hub, conn, Msg::Item { block: id, item, ok: false, info }, Payload::None);
        }
        for (chunk, items) in not_held {
            let reason = format!("export chunk {chunk} not held on {}", self.name);
            post(&mut self.local, hub, conn, Msg::Unfetched { block: id, items, reason, export: true }, Payload::None);
        }
        // the head node places them again (into the pool while it still has their holder)
        for (item, reason) in unreachable {
            post(&mut self.local, hub, conn, Msg::Unfetched { block: id, items: vec![item], reason, export: false }, Payload::None);
        }
        if conn != LOCAL {
            log(format!("render chunk {id} from {head}: {} items, {} here, {} from {}", p.ready.len() + p.groups.iter().map(|g| g.entities.len()).sum::<usize>(), p.ready.len(), p.groups.iter().map(|g| g.entities.len()).sum::<usize>(), if p.holders.is_empty() { "–".into() } else { p.holders.join(", ") }));
        }
        self.pending.push(p);
        self.advance(hub);
    }

    /// A holder's description of its items: count what is here and what to fetch, ask for the missing files
    /// `gone`: the holder does not hold the chunk (restarted, or released already): its items go back to be exported
    /// again (counted), not failed
    #[allow(clippy::too_many_arguments)]
    fn on_items(&mut self, hub: Option<&Hub>, conn: u64, head: &str, chunk: u64, items: Vec<HeldItem>, missing: Vec<(usize, String)>, gone: bool) {
        // the group that asked: an item is in one render chunk at a time
        let Some(first) = items.first().map(|i| i.item).or(missing.first().map(|m| m.0)) else { return };
        let me = self.name.clone();
        let asked = |g: &Group| g.conn == conn && g.chunk == chunk && g.info.is_none() && g.entities.contains_key(&first);
        let Some(pi) = self.pending.iter().position(|p| p.groups.iter().any(asked) && (if p.head == LOCAL { me.as_str() == head } else { hub.and_then(|h| h.peer(p.head)).is_some_and(|x| x.hello.name == head) })) else { return };
        let mut need = Vec::new();
        {
            let p = &mut self.pending[pi];
            let Some(g) = p.groups.iter_mut().find(|g| asked(g)) else { return };
            let mut again = Vec::new();
            let mut reason = String::new();
            for (item, why) in missing {
                g.entities.remove(&item);
                if gone {
                    again.push(item);
                    reason = why;
                } else {
                    post(&mut self.local, hub, p.head, Msg::Item { block: p.id, item, ok: false, info: why }, Payload::None);
                }
            }
            if !again.is_empty() {
                post(&mut self.local, hub, p.head, Msg::Unfetched { block: p.id, items: again, reason, export: true }, Payload::None);
            }
            // every path and hash from the holder ends up in a path here: only relative ones without .., hashes as
            // the cache names them
            let mut items: Vec<HeldItem> = items.into_iter().filter(|i| g.entities.contains_key(&i.item)).collect();
            items.retain(|i| {
                let bad = [&i.package, &i.rig].into_iter().chain(i.files.iter().map(|f| &f.path)).find(|x| safe_rel(x).is_none());
                let why = match bad {
                    Some(x) => Some(format!("unsafe path from {}: {x:?}", g.holder)),
                    None => i.files.iter().find(|f| !safe_hash(&f.hash)).map(|f| format!("unsafe hash from {}: {:?}", g.holder, f.hash)),
                };
                if let Some(info) = why.clone() {
                    g.entities.remove(&i.item);
                    post(&mut self.local, hub, p.head, Msg::Item { block: p.id, item: i.item, ok: false, info }, Payload::None);
                }
                why.is_none()
            });
            for f in items.iter().flat_map(|i| &i.files) {
                if !p.seen.insert(f.hash.clone()) {
                    continue;
                }
                p.files += 1;
                if self.cas.has(&f.hash) || self.inflight.contains_key(&f.hash) {
                    p.cached += f.size;
                } else {
                    p.bytes += f.size;
                    need.push(f.hash.clone());
                }
            }
            g.info = Some(items);
        }
        self.request(hub, conn, need);
        self.advance(hub);
    }

    /// Hashes the holder on `conn` does not have (any more) or that did not arrive intact: the items waiting for them
    /// from it go back to their head node to be exported again. Only the groups waiting on that holder now: the
    /// same content from another holder, or asked again later, is fetched as usual
    fn unfetchable(&mut self, hub: Option<&Hub>, conn: u64, gone: &HashSet<String>) {
        if gone.is_empty() {
            return;
        }
        for p in &mut self.pending {
            for g in p.groups.iter_mut().filter(|g| g.conn == conn) {
                let Some(info) = g.info.as_mut() else { continue };
                let mut items = Vec::new();
                let mut first = None;
                info.retain(|it| {
                    let lost = it.files.iter().find(|f| gone.contains(&f.hash) && !self.cas.has(&f.hash));
                    if let Some(f) = lost {
                        items.push(it.item);
                        first.get_or_insert_with(|| f.path.clone());
                    }
                    lost.is_none()
                });
                for t in &items {
                    g.entities.remove(t);
                }
                if let Some(path) = first {
                    let reason = format!("{path} missing on {}", g.holder);
                    post(&mut self.local, hub, p.head, Msg::Unfetched { block: p.id, items, reason, export: true }, Payload::None);
                }
            }
        }
    }

    fn request(&mut self, hub: Option<&Hub>, conn: u64, hashes: Vec<String>) {
        if hashes.is_empty() {
            return;
        }
        self.next_req += 1;
        let id = self.next_req;
        for h in &hashes {
            self.inflight.insert(h.clone(), conn);
        }
        self.requests.insert(id, (conn, hashes.clone()));
        post(&mut self.local, hub, conn, Msg::Fetch { id, hashes }, Payload::None);
    }

    /// Move render chunks on: a group whose files are all here is linked into the chunk's folder, a file no longer
    /// on its way is asked for again; a chunk with every group done goes to the Blender queue
    fn advance(&mut self, hub: Option<&Hub>) {
        let mut ask: Vec<(u64, Vec<String>)> = Vec::new();
        let mut i = 0;
        while i < self.pending.len() {
            let p = &mut self.pending[i];
            let mut gi = 0;
            while gi < p.groups.len() {
                let g = &mut p.groups[gi];
                let Some(info) = g.info.as_mut() else {
                    gi += 1;
                    continue;
                };
                let absent: Vec<String> = info.iter().flat_map(|it| &it.files).filter(|f| !self.cas.has(&f.hash)).map(|f| f.hash.clone()).collect();
                if !absent.is_empty() {
                    let again: Vec<String> = absent.into_iter().filter(|h| !self.inflight.contains_key(h)).collect::<HashSet<_>>().into_iter().collect();
                    if !again.is_empty() {
                        for h in &again {
                            self.inflight.insert(h.clone(), g.conn);
                        }
                        ask.push((g.conn, again));
                    }
                    gi += 1;
                    continue;
                }
                // all here: linked into the chunk's folder as laid out on the holder
                let root = p.dir.join(format!("x{}", g.chunk));
                let mut linked: HashMap<String, String> = HashMap::new();
                for it in info.iter() {
                    if self.win_paths {
                        let paths: Vec<&str> = it.files.iter().map(|f| f.path.as_str()).collect();
                        if let Some(why) = win_path_problem(&root, &paths, &linked) {
                            post(&mut self.local, hub, p.head, Msg::Item { block: p.id, item: it.item, ok: false, info: why }, Payload::None);
                            continue;
                        }
                    }
                    let res = it.files.iter().try_for_each(|f| link_or_copy(&self.cas.path(&f.hash), &root.join(&f.path)));
                    let entity = g.entities.get(&it.item).cloned().unwrap_or_default();
                    match res {
                        Ok(()) => {
                            // only what is linked counts against later case variants
                            if self.win_paths {
                                linked.extend(it.files.iter().map(|f| (f.path.to_lowercase(), f.path.clone())));
                            }
                            p.ready.push(Ready { item: it.item, entity, src: root.join(&it.package).join("scene.blend"), rig: root.join(&it.rig), pose: it.pose })
                        }
                        Err(e) => post(&mut self.local, hub, p.head, Msg::Item { block: p.id, item: it.item, ok: false, info: format!("not linked: {e:#}") }, Payload::None),
                    }
                }
                p.groups.remove(gi);
            }
            if !p.groups.is_empty() {
                i += 1;
                continue;
            }
            let p = self.pending.remove(i);
            if p.keep {
                let items = p.ready.into_iter().map(|r| (r.item, r.src, r.rig, r.pose)).collect();
                self.kept.push(Kept { id: p.id, dir: p.dir, items });
                continue;
            }
            if p.ready.is_empty() {
                let _ = fs::remove_dir_all(&p.dir);
                if let Some(d) = &p.script_dir {
                    let _ = fs::remove_dir_all(d);
                }
                post(&mut self.local, hub, p.head, Msg::BlockDone { id: p.id, exit: None, error: String::new() }, Payload::None);
                continue;
            }
            if p.remote {
                let msg = Msg::Fetched { id: p.id, holders: p.holders.clone(), bytes: p.bytes, cached: p.cached, files: p.files, secs: p.started.elapsed().as_secs_f64() };
                post(&mut self.local, hub, p.head, msg, Payload::None);
            }
            self.queue.push_back(Queued { head: p.head, id: p.id, work: Work::Render { dir: p.dir, script: p.script, script_dir: p.script_dir, size: p.size, jobs: p.ready } });
        }
        for (conn, hashes) in ask {
            for h in &hashes {
                self.inflight.remove(h);
            }
            self.request(hub, conn, hashes);
        }
    }

    /// One tick: start queued work on free slots, give the chunks their processes, forward the processes' results,
    /// feed the Blenders, finish chunks. `wait`: how long to wait for lines; the engine passes 0
    pub fn tick(&mut self, hub: Option<&Hub>, wait: Duration, log: &mut dyn FnMut(String)) {
        loop {
            let exporting = self.running.values().filter(|r| r.export.is_some()).count();
            let rendering = self.running.len() - exporting;
            let next = self.queue.iter().position(|q| if matches!(q.work, Work::Export(_)) { exporting < self.export_slots.max(1) } else { rendering < self.slots });
            let Some(q) = next.and_then(|i| self.queue.remove(i)) else { break };
            let key = self.next_key;
            self.next_key += 1;
            let (head, id) = (q.head, q.id);
            match self.start(q) {
                Ok(r) => {
                    self.running.insert(key, r);
                }
                Err((e, export)) => self.not_started(hub, head, id, export, &format!("{e:#}"), log),
            }
        }
        self.assign(hub, log);
        self.feed();
        let deadline = Instant::now() + Duration::from_millis(50);
        loop {
            let l = if wait.is_zero() { self.rx.try_recv().ok() } else { self.rx.recv_timeout(wait).ok() };
            let Some(l) = l else { break };
            match l {
                Line::Out { key, line, stderr } => self.on_line(hub, key, line, stderr),
                Line::Eof { key } => {
                    if let Some(p) = self.procs.get_mut(&key) {
                        p.eof += 1;
                    }
                }
            }
            if Instant::now() > deadline {
                break;
            }
        }
        self.reap(hub, log);
        self.complete(hub, log);
        self.feed();
        self.housekeeping();
    }

    /// A chunk that did not start: why to the head node
    fn not_started(&mut self, hub: Option<&Hub>, head: u64, id: u64, export: bool, error: &str, log: &mut dyn FnMut(String)) {
        log(format!("chunk {id} not started: {error}"));
        let msg = if export {
            Msg::ExportDone { id, exit: None, wall: 0.0, item_seconds: 0.0, error: format!("export-batch does not start: {error}"), long_paths: 0, failed: Vec::new() }
        } else {
            Msg::BlockDone { id, exit: None, error: format!("Blender does not start: {error}") }
        };
        post(&mut self.local, hub, head, msg, Payload::None);
    }

    /// Whether this node's export-batch has the persistent --stdin mode (probed once)
    fn export_stdin(&mut self, log: &mut dyn FnMut(String)) -> bool {
        if let Some(v) = self.export_stdin {
            return v;
        }
        let v = self.tools.as_ref().is_some_and(|t| supports_stdin(&t.starbreaker));
        // only the fallback is worth a line: it explains slower exports
        if !v {
            log("export-batch without --stdin: one process per export chunk".into());
        }
        self.export_stdin = Some(v);
        v
    }

    /// Chunks without a process get one: an idle one of their kind, else a new one (an idle one over the slots
    /// quits for it)
    fn assign(&mut self, hub: Option<&Hub>, log: &mut dyn FnMut(String)) {
        let mut waiting: Vec<u64> = self.running.iter().filter(|(_, r)| r.proc.is_none() && !r.cancelled && !r.export.as_ref().is_some_and(|x| x.done)).map(|(k, _)| *k).collect();
        waiting.sort();
        for key in waiting {
            let kind = match self.running[&key].export.is_some() {
                true if self.export_stdin(log) => ProcKind::Export,
                true => ProcKind::ExportOnce,
                false => ProcKind::Blender(self.running[&key].script_hash.clone()),
            };
            let idle = if kind == ProcKind::ExportOnce { None } else { self.procs.iter().filter(|(_, p)| p.chunk.is_none() && p.quit_at.is_none() && p.ready && p.kind == kind).map(|(k, _)| *k).min() };
            let pk = match idle {
                Some(pk) => pk,
                None => {
                    let (script, jobs) = (self.running[&key].script.clone(), self.running[&key].dir.join("jobs.json"));
                    match self.spawn(&kind, &script, &jobs) {
                        Ok(pk) => pk,
                        Err(e) => {
                            let Some(r) = self.running.remove(&key) else { continue };
                            let _ = fs::remove_dir_all(&r.dir);
                            if let Some(d) = &r.script_dir {
                                let _ = fs::remove_dir_all(d);
                            }
                            self.not_started(hub, r.head, r.id, r.export.is_some(), &format!("{e:#}"), log);
                            continue;
                        }
                    }
                }
            };
            if let Some(p) = self.procs.get_mut(&pk) {
                p.chunk = Some(key);
            }
            if let Some(r) = self.running.get_mut(&key) {
                r.proc = Some(pk);
            }
            self.trim(kind != ProcKind::Export && kind != ProcKind::ExportOnce);
        }
    }

    /// More live processes of a family (Blender, export-batch) than slots: idle ones quit
    fn trim(&mut self, blender: bool) {
        let family = |p: &Proc| matches!(p.kind, ProcKind::Blender(_)) == blender && p.quit_at.is_none();
        let limit = if blender { self.slots } else { self.export_slots.max(1) };
        let live = self.procs.values().filter(|p| family(p)).count();
        let mut idle: Vec<u64> = self.procs.iter().filter(|(_, p)| family(p) && p.chunk.is_none()).map(|(k, _)| *k).collect();
        idle.sort();
        for k in idle.into_iter().take(live.saturating_sub(limit)) {
            self.quit(k);
        }
    }

    /// Ask a process to quit (quit line, then EOF on its stdin); killed after QUIT_GRACE
    fn quit(&mut self, key: u64) {
        let Some(p) = self.procs.get_mut(&key) else { return };
        if p.quit_at.is_some() {
            return;
        }
        p.quit_at = Some(Instant::now());
        let line = if matches!(p.kind, ProcKind::Blender(_)) { "quit\n" } else { "{\"quit\":true}\n" };
        send_line(p, line);
        p.stdin = None;
    }

    /// Every idle process quits (a run is over: their memory goes back)
    pub fn quit_idle(&mut self) {
        let idle: Vec<u64> = self.procs.iter().filter(|(_, p)| p.chunk.is_none()).map(|(k, _)| *k).collect();
        for k in idle {
            self.quit(k);
        }
    }

    /// A chunk the head node gave up (or whose head node went away): its process is killed, the chunk ends once it
    /// is gone (at once without one)
    fn cancel_running(&mut self, key: u64) {
        let Some(r) = self.running.get_mut(&key) else { return };
        r.cancelled = true;
        if let Some(p) = r.proc.and_then(|pk| self.procs.get_mut(&pk)) {
            let _ = p.child.kill();
            p.quit_at.get_or_insert_with(Instant::now);
        }
    }

    /// Ready processes get their work: a persistent export-batch its chunk's job lines and end_chunk at once, a
    /// Blender the next job (and LOOKAHEAD more) as it answers
    fn feed(&mut self) {
        for r in self.running.values_mut() {
            let Some(p) = r.proc.and_then(|pk| self.procs.get_mut(&pk)) else { continue };
            if !p.ready || r.cancelled {
                continue;
            }
            if let Some(x) = r.export.as_mut() {
                if p.kind == ProcKind::Export && !x.sent {
                    x.sent = true;
                    let text: String = x.lines.iter().map(|l| format!("{l}\n")).collect();
                    send_line(p, &text);
                }
                continue;
            }
            if p.quit_at.is_some() {
                continue;
            }
            while r.fed.len() <= LOOKAHEAD {
                let Some(job) = r.todo.pop_front() else { break };
                if !send_line(p, &format!("{}\n", job.1)) {
                    r.todo.push_front(job);
                    break;
                }
                r.fed.push_back(job);
            }
        }
    }

    /// Processes whose output ended: reaped; the chunk of one that died goes on (Blender: the job it was on fails
    /// with its last lines, a new process takes the rest) or ends (export-batch, a Blender that never got ready)
    fn reap(&mut self, hub: Option<&Hub>, log: &mut dyn FnMut(String)) {
        let ended: Vec<u64> = self.procs.iter().filter(|(_, p)| p.eof >= 2).map(|(k, _)| *k).collect();
        for pk in ended {
            let Some(mut p) = self.procs.remove(&pk) else { continue };
            let exit = p.child.wait().ok().and_then(|s| s.code());
            let Some(mut r) = p.chunk.and_then(|ck| self.running.remove(&ck)) else { continue };
            r.proc = None;
            if let Some(mut state) = r.export.take() {
                // a persistent export-batch that died at work (a panic in a job ends it): the job it was on fails with
                // its last lines, the chunk's other jobs go to a new process into the same folder
                let open: Vec<usize> = state.jobs.iter().map(|j| j.item).filter(|i| !state.answered.contains(i)).collect();
                if p.kind == ProcKind::Export && p.ready && !r.cancelled && !open.is_empty() {
                    let item = open[0];
                    let error = crate::engine::with_trace(&format!("export-batch ended while exporting it (exit {exit:?})"), &std::mem::take(&mut r.stderr_tail));
                    state.answered.insert(item);
                    r.reported += 1;
                    post(&mut self.local, hub, r.head, Msg::ItemExported { chunk: r.id, item, ok: false, seconds: 0.0, error, worn: None }, Payload::None);
                    if open.len() > 1 {
                        let mut lines: Vec<String> = state.jobs.iter().filter(|j| !state.answered.contains(&j.item)).map(|j| json!({ "name": j.entity, "output": r.dir, "worn": j.worn }).to_string()).collect();
                        lines.push(json!({ "end_chunk": r.dir }).to_string());
                        state.lines = lines;
                        state.sent = false;
                        r.export = Some(state);
                        log(format!("export chunk {}: export-batch ended (exit {exit:?}), the chunk goes on with a new one", r.id));
                        if let Some(ck) = p.chunk {
                            self.running.insert(ck, r);
                        }
                        continue;
                    }
                }
                let why = if p.ready { format!("export-batch ended early (exit {exit:?})") } else { format!("export-batch does not start (exit {exit:?})") };
                self.after_export(hub, r, state, exit, &why, log);
                continue;
            }
            if r.cancelled {
                self.finish_render(hub, r, exit, String::new(), log);
                continue;
            }
            // a Blender that quit as asked (its job limit) after answering what it had: the chunk goes on with the next
            if p.quit_at.is_some() && p.ready && r.fed.is_empty() {
                if let Some(ck) = p.chunk {
                    self.running.insert(ck, r);
                }
                continue;
            }
            if !p.ready {
                let error = crate::engine::with_trace(&format!("Blender ended before it was ready (exit {exit:?})"), &r.stderr_tail);
                self.finish_render(hub, r, exit, error, log);
                continue;
            }
            let Some((item, _)) = r.fed.pop_front() else {
                let error = crate::engine::with_trace(&format!("Blender ended early (exit {exit:?})"), &r.stderr_tail);
                self.finish_render(hub, r, exit, error, log);
                continue;
            };
            // the job it was on fails with its last lines (the head node counts it as any failed render); the jobs
            // fed behind it never started and go to the next process
            let info = crate::engine::with_trace(&format!("Blender ended while rendering it (exit {exit:?})"), &std::mem::take(&mut r.stderr_tail));
            r.reported += 1;
            post(&mut self.local, hub, r.head, Msg::Item { block: r.id, item, ok: false, info }, Payload::None);
            while let Some(j) = r.fed.pop_back() {
                r.todo.push_front(j);
            }
            self.blender_log(&format!("[r{}] Blender ended (exit {exit:?}), the chunk goes on with a new one", r.id));
            if let Some(ck) = p.chunk {
                self.running.insert(ck, r);
            }
        }
    }

    /// Chunks whose work is over: an export chunk its process answered chunk_done for (or cancelled before it had
    /// one), a render chunk with every job answered (or cancelled without a process); a Blender that is quitting
    /// leaves its chunk once its jobs are answered (the chunk gets a new one)
    fn complete(&mut self, hub: Option<&Hub>, log: &mut dyn FnMut(String)) {
        let mut keys: Vec<u64> = self.running.keys().copied().collect();
        keys.sort();
        for key in keys {
            let Some(r) = self.running.get(&key) else { continue };
            let quitting = r.proc.and_then(|pk| self.procs.get(&pk)).is_some_and(|p| p.quit_at.is_some());
            let done = match r.export.as_ref() {
                Some(x) => x.done || (r.cancelled && r.proc.is_none()),
                None => (r.cancelled && r.proc.is_none()) || (!r.cancelled && r.todo.is_empty() && r.fed.is_empty()),
            };
            // a retiring Blender with nothing of this chunk left in it: the chunk takes a new one
            let leave = done || (!r.cancelled && r.export.is_none() && quitting && r.fed.is_empty());
            if leave && let Some(p) = self.running.get_mut(&key).and_then(|r| r.proc.take()).and_then(|pk| self.procs.get_mut(&pk)) {
                p.chunk = None;
                p.idle_since = Instant::now();
            }
            if !done {
                continue;
            }
            let Some(mut r) = self.running.remove(&key) else { continue };
            match r.export.take() {
                Some(state) => {
                    let why = format!("export-batch finished the chunk without a result for {} items", state.jobs.len().saturating_sub(r.reported));
                    self.after_export(hub, r, state, None, &why, log);
                }
                None => self.finish_render(hub, r, None, String::new(), log),
            }
        }
    }

    /// Idle processes quit after IDLE_STOP and when there are more than slots; idle ones asked to quit and still there
    /// after QUIT_GRACE are killed
    fn housekeeping(&mut self) {
        let now = Instant::now();
        let idle: Vec<u64> = self.procs.iter().filter(|(_, p)| p.chunk.is_none() && p.quit_at.is_none() && now.duration_since(p.idle_since) > IDLE_STOP).map(|(k, _)| *k).collect();
        for k in idle {
            self.quit(k);
        }
        self.trim(true);
        self.trim(false);
        // (one still answering the jobs of its chunk is left alone: the chunk's timeout watches it)
        for p in self.procs.values_mut().filter(|p| p.chunk.is_none()) {
            if p.quit_at.is_some_and(|t| now.duration_since(t) > QUIT_GRACE) {
                let _ = p.child.kill();
            }
        }
    }

    /// A render chunk is over: BlockDone (with why it ended early and its process's last lines) to the head node,
    /// its folder goes (--filter <entity> repeats it)
    fn finish_render(&mut self, hub: Option<&Hub>, r: Running, exit: Option<i32>, error: String, log: &mut dyn FnMut(String)) {
        if r.head != LOCAL {
            log(format!("render chunk {} done: {}/{} in {:.1} s", r.id, r.reported, r.total, r.started.elapsed().as_secs_f64()));
            for l in error.lines() {
                log(format!("  {l}"));
            }
        }
        post(&mut self.local, hub, r.head, Msg::BlockDone { id: r.id, exit, error }, Payload::None);
        let _ = fs::remove_dir_all(&r.dir);
        if let Some(d) = &r.script_dir {
            let _ = fs::remove_dir_all(d);
        }
    }

    fn on_line(&mut self, hub: Option<&Hub>, key: u64, line: String, stderr: bool) {
        let Some(p) = self.procs.get(&key) else { return };
        let blender = matches!(p.kind, ProcKind::Blender(_));
        let chunk = p.chunk;
        if blender {
            let tag = chunk.and_then(|c| self.running.get(&c)).map(|r| format!("r{}", r.id)).unwrap_or_else(|| format!("p{key}"));
            self.blender_log(&format!("[{tag}] {line}"));
        }
        let Some(p) = self.procs.get_mut(&key) else { return };
        // every line but the results is output since the last result (both streams: Blender reports some errors on
        // stdout, render.py's tracebacks go to stderr); a failure carries it
        let keep = |tail: &mut String, text: &str| {
            tail.push_str(text);
            tail.push('\n');
            crate::engine::keep_tail(tail, 4000);
        };
        // ready: Blender "ready: …"; export-batch {"ready": true}, or {"ready": false, "error": …} and it exits
        if !stderr && !p.ready {
            let t = line.trim();
            if blender && t.starts_with("ready:") {
                p.ready = true;
                return;
            }
            if let (ProcKind::Export, Ok(v)) = (&p.kind, serde_json::from_str::<serde_json::Value>(t))
                && let Some(ok) = v.get("ready").and_then(serde_json::Value::as_bool)
            {
                p.ready = ok;
                if !ok {
                    let why = format!("export-batch not ready: {}", v["error"].as_str().unwrap_or("no reason"));
                    match chunk.and_then(|c| self.running.get_mut(&c)) {
                        Some(r) => keep(&mut r.stderr_tail, &why),
                        None => keep(&mut p.tail, &why),
                    }
                }
                return;
            }
        }
        let Some(r) = chunk.and_then(|c| self.running.get_mut(&c)) else {
            keep(&mut p.tail, &line);
            return;
        };
        if stderr {
            keep(&mut r.stderr_tail, &line);
            return;
        }
        // export-batch: one JSON line per job {name, ok, package, worn, seconds, error}; {"chunk_done": dir} ends a
        // chunk of the persistent one
        if let Some(state) = r.export.as_mut() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
                keep(&mut r.stderr_tail, &line);
                return;
            };
            if v.get("chunk_done").is_some() {
                state.done = true;
                // restarted after EXPORT_CHUNKS chunks or over EXPORT_RSS (between chunks: nothing is lost)
                p.jobs += 1;
                let rss = rss_of(&mut self.sys, p.child.id());
                if p.jobs >= self.export_chunks || rss.is_some_and(|m| m > EXPORT_RSS) {
                    self.quit(key);
                }
                return;
            }
            let name = v["name"].as_str().unwrap_or_default().to_lowercase();
            let Some(j) = state.jobs.iter().find(|j| j.entity.to_lowercase() == name) else { return };
            let item = j.item;
            if !state.answered.insert(item) {
                return;
            }
            let seconds = v["seconds"].as_f64().unwrap_or(0.0);
            state.item_secs += seconds;
            r.reported += 1;
            let ok = match (v["ok"].as_bool() == Some(true), v["package"].as_str()) {
                (true, Some(pkg)) => {
                    let rel = match Path::new(pkg).strip_prefix(&r.dir) {
                        Ok(p) => p.to_string_lossy().replace('\\', "/"),
                        Err(_) => format!("Packages/{}", Path::new(pkg).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()),
                    };
                    state.packages.insert(item, rel);
                    true
                }
                _ => false,
            };
            let error = if ok { String::new() } else { v["error"].as_str().unwrap_or("export failed").to_string() };
            let msg = Msg::ItemExported { chunk: r.id, item, ok, seconds, error, worn: v["worn"].as_bool() };
            post(&mut self.local, hub, r.head, msg, Payload::None);
            return;
        }
        // render.py: "icon: <png> <info>" / "failed: <png> <error>"
        let (ok, rest) = if let Some(x) = line.trim().strip_prefix("icon: ") {
            (true, x)
        } else if let Some(x) = line.trim().strip_prefix("failed: ") {
            (false, x)
        } else {
            keep(&mut r.stderr_tail, &line);
            return;
        };
        let Some(end) = rest.to_lowercase().find(".png") else { return };
        let path = rest[..end + 4].to_string();
        let mut info = rest[end + 4..].trim().to_string();
        let Some(&item) = r.outs.get(&path.to_lowercase()) else { return };
        let output = std::mem::take(&mut r.stderr_tail);
        if !ok {
            info = crate::engine::with_trace(&info, &output);
        }
        r.reported += 1;
        r.fed.retain(|(i, _)| *i != item);
        let data = if ok { fs::read(&path).ok() } else { None };
        let ok = ok && data.is_some();
        if ok && fs::copy(&path, &self.preview).is_ok() {
            self.last = Some((self.preview.clone(), r.names.get(&item).cloned().unwrap_or_default()));
        }
        post(&mut self.local, hub, r.head, Msg::Item { block: r.id, item, ok, info }, data.map(Payload::Bytes).unwrap_or(Payload::None));
        // restarted after BLENDER_JOBS jobs or over BLENDER_RSS: it quits after the jobs fed to it already
        p.jobs += 1;
        let rss = rss_of(&mut self.sys, p.child.id());
        let why = if p.jobs >= self.blender_jobs {
            Some(format!("{} jobs", p.jobs))
        } else {
            rss.filter(|m| *m > BLENDER_RSS).map(|m| format!("{:.1} GB resident", m as f64 / (1u64 << 30) as f64))
        };
        if let Some(why) = why {
            let id = r.id;
            self.quit(key);
            self.blender_log(&format!("[r{id}] Blender restarts after this chunk's jobs fed to it: {why}"));
        }
    }

    /// Export done: its rigs copied in, the exported items held here until the head node releases the chunk; items
    /// without a line failed (`why`: what ended it early)
    fn after_export(&mut self, hub: Option<&Hub>, r: Running, state: ExportState, exit: Option<i32>, why: &str, log: &mut dyn FnMut(String)) {
        let wall = r.started.elapsed().as_secs_f64();
        if r.cancelled {
            let _ = fs::remove_dir_all(&r.dir);
            post(&mut self.local, hub, r.head, Msg::ExportDone { id: r.id, exit, wall, item_seconds: state.item_secs, error: "cancelled".into(), long_paths: 0, failed: Vec::new() }, Payload::None);
            return;
        }
        let mut items = HashMap::new();
        let mut failed = Vec::new();
        for j in &state.jobs {
            let Some(package) = state.packages.get(&j.item) else { continue };
            let rig = Path::new(&j.rig);
            let name = rig.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            match fs::create_dir_all(r.dir.join("rigs")).and_then(|_| fs::copy(rig, r.dir.join("rigs").join(&name))) {
                Ok(_) => {
                    items.insert(j.item, HeldInfo { package: package.clone(), rig: format!("rigs/{name}"), pose: j.pose });
                }
                Err(e) => failed.push((j.item, format!("rig {}: {e}", rig.display()))),
            }
        }
        let error = if r.reported < state.jobs.len() { crate::engine::with_trace(why, &r.stderr_tail) } else { String::new() };
        // On Windows Blender cannot read paths over 259 characters: missing textures silently render white
        let (long_paths, first) = if cfg!(windows) { crate::engine::too_long(&r.dir, 259) } else { (0, None) };
        if let Some(p) = first {
            log(format!("export chunk {}: {long_paths} paths over 259 characters ({}), put --work in a shorter folder", r.id, p.display()));
        }
        if r.head != LOCAL {
            log(format!("export chunk {} done: {} of {} in {wall:.1} s", r.id, items.len(), state.jobs.len()));
        }
        if items.is_empty() {
            let _ = fs::remove_dir_all(&r.dir);
        } else {
            let head = self.peer_name(hub, r.head);
            self.held.insert((head, r.id), Held { root: r.dir.clone(), items, index: None, hashes: HashMap::new() });
        }
        post(&mut self.local, hub, r.head, Msg::ExportDone { id: r.id, exit, wall, item_seconds: state.item_secs, error, long_paths, failed }, Payload::None);
    }

    /// Reader threads for a child's stdout and stderr
    fn pipe(&self, child: &mut Child, key: u64) {
        for (stream, stderr) in [(child.stdout.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>), false), (child.stderr.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>), true)] {
            let Some(stream) = stream else { continue };
            let tx = self.tx.clone();
            thread::spawn(move || {
                let mut reader = BufReader::new(stream);
                let mut buf = Vec::new();
                while reader.read_until(b'\n', &mut buf).unwrap_or(0) > 0 {
                    let line = String::from_utf8_lossy(&buf).trim_end().to_string();
                    buf.clear();
                    if tx.send(Line::Out { key, line, stderr }).is_err() {
                        return;
                    }
                }
                let _ = tx.send(Line::Eof { key });
            });
        }
    }

    /// Stop every process (on quit)
    pub fn stop(&mut self) {
        for p in self.procs.values_mut() {
            let _ = p.child.kill();
            let _ = p.child.wait();
        }
        self.procs.clear();
        self.running.clear();
        self.queue.clear();
    }

    /// Processes running here, the idle ones included (the fake-tool tests, unix only)
    #[cfg(all(test, unix))]
    pub fn processes(&self) -> usize {
        self.procs.len()
    }

    /// Start a queued chunk (its process comes in assign); the error says whether it was an export
    fn start(&self, q: Queued) -> std::result::Result<Running, (anyhow::Error, bool)> {
        match q.work {
            Work::Export(jobs) => self.start_export(q.head, q.id, jobs).map_err(|e| (e, true)),
            Work::Render { dir, script, script_dir, size, jobs } => self.start_render(q.head, q.id, dir, script, script_dir, size, jobs).map_err(|e| (e, false)),
        }
    }

    fn running(head: u64, id: u64, dir: PathBuf, names: HashMap<usize, String>, total: usize) -> Running {
        Running {
            head,
            id,
            dir,
            proc: None,
            outs: HashMap::new(),
            names,
            reported: 0,
            total,
            stderr_tail: String::new(),
            started: Instant::now(),
            export: None,
            cancelled: false,
            script_dir: None,
            todo: VecDeque::new(),
            fed: VecDeque::new(),
            script: PathBuf::new(),
            script_hash: String::new(),
        }
    }

    /// An export chunk into <work>/export/x<id>: its jobs file (a StarBreaker without --stdin reads it) and the same
    /// jobs as lines for a persistent export-batch, closed by end_chunk
    fn start_export(&self, head: u64, id: u64, jobs: Vec<ItemJob>) -> Result<Running> {
        self.tools.as_ref().context("no Data.p4k here")?;
        let dir = self.work.join("export").join(format!("x{id}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir)?;
        let list: Vec<_> = jobs.iter().map(|j| json!({ "name": j.entity, "output": dir, "worn": j.worn })).collect();
        fs::write(dir.join("jobs.json"), serde_json::to_vec(&list)?)?;
        let mut lines: Vec<String> = list.iter().map(|v| v.to_string()).collect();
        lines.push(json!({ "end_chunk": dir }).to_string());
        let mut r = Self::running(head, id, dir, jobs.iter().map(|j| (j.item, j.entity.clone())).collect(), jobs.len());
        r.export = Some(ExportState { jobs, packages: HashMap::new(), item_secs: 0.0, lines, sent: false, done: false, answered: HashSet::new() });
        Ok(r)
    }

    /// A render chunk whose files are here: one job line per item for a Blender, the PNGs into its folder
    #[allow(clippy::too_many_arguments)]
    fn start_render(&self, head: u64, id: u64, dir: PathBuf, script: PathBuf, script_dir: Option<PathBuf>, size: u32, jobs: Vec<Ready>) -> Result<Running> {
        let out_dir = dir.join("out");
        fs::create_dir_all(&out_dir)?;
        let script_hash = crate::events::file_hash(&script).with_context(|| format!("{} not readable", script.display()))?;
        let mut r = Self::running(head, id, dir, jobs.iter().map(|j| (j.item, j.entity.clone())).collect(), jobs.len());
        for j in &jobs {
            let out = out_dir.join(format!("{}.png", j.item));
            r.outs.insert(out.to_string_lossy().to_lowercase(), j.item);
            r.todo.push_back((j.item, json!({ "src": j.src, "out": out, "pose": j.pose, "rig": j.rig, "size": size }).to_string()));
        }
        r.script = script;
        r.script_hash = script_hash;
        r.script_dir = script_dir;
        Ok(r)
    }

    /// A process of `kind`: a Blender with render.py --stdin, a persistent export-batch --stdin, or export-batch over
    /// the chunk's jobs file
    fn spawn(&mut self, kind: &ProcKind, script: &Path, jobs_file: &Path) -> Result<u64> {
        let mut cmd = match kind {
            // Vulkan: under OpenGL the render phase (shader compilation) grew with 8 Blenders from 0.21 to 0.86 s,
            // under Vulkan it stayed at 0.30 s; the images differ by 0.04/255 on average (2026-11-02)
            ProcKind::Blender(_) => {
                let mut c = crate::blender::command(&self.blender);
                c.arg("--gpu-backend").arg("vulkan").arg("--python").arg(script).arg("--").arg("--stdin");
                c
            }
            ProcKind::Export | ProcKind::ExportOnce => {
                let tools = self.tools.as_ref().context("no Data.p4k here")?;
                let mut c = Command::new(&tools.starbreaker);
                c.arg("entity").arg("export-batch");
                if *kind == ProcKind::Export {
                    c.arg("--stdin");
                } else {
                    c.arg(jobs_file);
                }
                // --variant-tags: paint variants with their SubGeometry; --icon-files: without animations and diagnostics
                c.arg("--p4k").arg(&tools.p4k).args(["--kind", "decomposed", "--variant-tags", "--icon-files"]);
                // glibc's arenas fragment over hundreds of jobs in one process (1.4 GB instead of 1.0 GB after 300)
                if cfg!(target_os = "linux") {
                    c.env("MALLOC_ARENA_MAX", "2");
                }
                c
            }
        };
        let once = *kind == ProcKind::ExportOnce;
        let mut child = cmd
            .stdin(if once { Stdio::null() } else { Stdio::piped() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| Path::new(cmd.get_program()).display().to_string())?;
        let key = self.next_key;
        self.next_key += 1;
        let stdin = child.stdin.take();
        self.pipe(&mut child, key);
        let now = Instant::now();
        self.procs.insert(key, Proc { kind: kind.clone(), child, stdin, ready: once, jobs: 0, eof: 0, chunk: None, quit_at: None, idle_since: now, tail: String::new() });
        Ok(key)
    }
}

/// Whether a StarBreaker's export-batch has the persistent --stdin mode (its --help names it)
pub(crate) fn supports_stdin(starbreaker: &Path) -> bool {
    Command::new(starbreaker)
        .args(["entity", "export-batch", "--help"])
        .stdin(Stdio::null())
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("--stdin") || String::from_utf8_lossy(&o.stderr).contains("--stdin"))
}

/// Resident memory of a process (BLENDER_RSS, EXPORT_RSS)
pub(crate) fn rss_of(sys: &mut Option<sysinfo::System>, pid: u32) -> Option<u64> {
    if sys.is_none() {
        *sys = sysinfo::System::new().ok();
    }
    let s = sys.as_mut()?;
    let pid = sysinfo::Pid::from_u32(pid);
    s.refresh_processes_specifics(sysinfo::ProcessesToUpdate::Some(&[pid]), true, sysinfo::ProcessRefreshKind::nothing().with_memory());
    s.process(pid).map(sysinfo::Process::memory)
}

/// A line (or lines) into a process's stdin; false when it is closed
fn send_line(p: &mut Proc, text: &str) -> bool {
    p.stdin.as_mut().is_some_and(|s| s.write_all(text.as_bytes()).and_then(|_| s.flush()).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NetConfig;
    use crate::net::{Hello, HubEv};

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("packshot-node-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    /// A render chunk `id` of this node's own run waiting for `items` of export chunk `chunk` from `holder` (`conn`)
    fn waiting(r: &mut Renderer, id: u64, conn: u64, holder: &str, chunk: u64, items: &[usize]) {
        let dir = r.work.join("fetch").join(format!("{id}"));
        let groups = vec![Group { holder: holder.into(), conn, chunk, entities: items.iter().map(|&i| (i, format!("e{i}"))).collect(), info: None }];
        r.pending.push(Pending { head: LOCAL, id, dir, script: PathBuf::new(), script_dir: None, size: 64, ready: Vec::new(), groups, started: Instant::now(), holders: vec![holder.into()], bytes: 0, cached: 0, files: 0, seen: HashSet::new(), keep: false, remote: true });
    }

    fn file(path: &str, hash: &str) -> HashedFile {
        HashedFile { path: path.into(), size: 1, hash: hash.into() }
    }

    fn held_item(item: usize, package: &str, files: Vec<HashedFile>) -> HeldItem {
        HeldItem { item, package: package.into(), pose: [0.0; 3], rig: "rigs/r.json".into(), files }
    }

    /// What this node sent its own engine: (item, failed info) and (items, reason, export) of Unfetched
    fn sent(r: &mut Renderer) -> (Vec<(usize, String)>, Vec<(Vec<usize>, String, bool)>) {
        let (mut failed, mut back) = (Vec::new(), Vec::new());
        for (m, _) in r.local.drain(..) {
            match m {
                Msg::Item { item, ok: false, info, .. } => failed.push((item, info)),
                Msg::Unfetched { mut items, reason, export, .. } => {
                    items.sort();
                    back.push((items, reason, export));
                }
                _ => {}
            }
        }
        failed.sort();
        (failed, back)
    }

    const H1: &str = "00000000000000000000000000000001";
    const H2: &str = "00000000000000000000000000000002";
    const SHARED: &str = "000000000000000000000000000000ff";

    /// Paths and hashes from a holder's ItemsInfo end up in paths here: one with .., an absolute one or a hash that is
    /// not one fails its item with the reason; the others are fetched
    #[test]
    fn unsafe_paths_from_holder() {
        let root = scratch("unsafe");
        let mut r = Renderer::new("me", &root.join("work"), PathBuf::new(), PathBuf::new(), 1);
        waiting(&mut r, 10, 5, "e", 1, &[1, 2, 3, 4, 5]);
        let items = vec![
            held_item(1, "Packages/p1", vec![file("Packages/p1/scene.blend", H1)]),
            held_item(2, "../../../evil", vec![file("Packages/p2/scene.blend", H2)]),
            held_item(3, "Packages/p3", vec![file("../../outside.png", H2)]),
            held_item(4, "Packages/p4", vec![file("/etc/x", H2)]),
            held_item(5, "Packages/p5", vec![file("Packages/p5/scene.blend", "../../../cas-escape")]),
        ];
        r.on_items(None, 5, "me", 1, items, Vec::new(), false);
        let (failed, back) = sent(&mut r);
        assert_eq!(failed.iter().map(|f| f.0).collect::<Vec<_>>(), vec![2, 3, 4, 5], "{failed:?}");
        assert!(failed.iter().all(|(_, why)| why.starts_with("unsafe path from e: ") || why.starts_with("unsafe hash from e: ")), "{failed:?}");
        assert!(back.is_empty());
        let asked: Vec<&String> = r.requests.values().flat_map(|(_, h)| h).collect();
        assert_eq!(asked, vec![H1], "only the safe item's file is fetched");
        assert_eq!(r.pending[0].groups[0].entities.len(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    /// A hash a holder no longer has fails only the items waiting for it from that holder now (back to be exported
    /// again); a late answer to a request nobody waits for is dropped; the same content asked later (from another
    /// holder) is fetched as usual, not poisoned
    #[test]
    fn lost_hash_only_hits_its_request() {
        let root = scratch("lost");
        let mut r = Renderer::new("me", &root.join("work"), PathBuf::new(), PathBuf::new(), 1);
        let mut log = |_: String| {};
        waiting(&mut r, 10, 5, "e", 1, &[1]);
        r.on_items(None, 5, "me", 1, vec![held_item(1, "Packages/p1", vec![file("Data/shared.png", SHARED), file("Packages/p1/scene.blend", H1)])], Vec::new(), false);
        let req = *r.requests.keys().next().unwrap();
        // an answer to a request that is gone (a chunk cancelled, the cache cleared): nothing happens
        let inbox = root.join("inbox-late");
        fs::create_dir_all(&inbox).unwrap();
        r.on_msg(None, 5, Msg::FetchData { id: 999, files: Vec::new(), missing: vec![SHARED.into()] }, Some(inbox.clone()), &mut log);
        assert!(!inbox.exists(), "its files dropped");
        assert_eq!(sent(&mut r), (Vec::new(), Vec::new()));
        // the holder lost the shared file: the item goes back to be exported again
        r.on_msg(None, 5, Msg::FetchData { id: req, files: Vec::new(), missing: vec![SHARED.into(), H1.into()] }, None, &mut log);
        let (failed, back) = sent(&mut r);
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(back, vec![(vec![1], "Data/shared.png missing on e".to_string(), true)]);
        // a later chunk needing the same content from another holder asks for it
        waiting(&mut r, 11, 6, "f", 2, &[2]);
        r.on_items(None, 6, "me", 2, vec![held_item(2, "Packages/p2", vec![file("Data/shared.png", SHARED)])], Vec::new(), false);
        let (failed, back) = sent(&mut r);
        assert!(failed.is_empty() && back.is_empty(), "{failed:?} {back:?}");
        assert!(r.requests.values().any(|(c, h)| *c == 6 && h.contains(&SHARED.to_string())), "asked again: {:?}", r.requests);
        let _ = fs::remove_dir_all(&root);
    }

    /// A holder disconnects while a render chunk waits for its files: the items go back to the head node (Unfetched)
    /// before the chunk ends, nothing is dropped silently
    #[test]
    fn holder_drop_reports_items() {
        let root = scratch("down");
        let mut r = Renderer::new("me", &root.join("work"), PathBuf::new(), PathBuf::new(), 1);
        waiting(&mut r, 10, 5, "e", 1, &[1, 2]);
        r.on_down(None, 5, "e");
        let order: Vec<&str> = r.local.iter().map(|(m, _)| if matches!(m, Msg::Unfetched { .. }) { "unfetched" } else if matches!(m, Msg::BlockDone { .. }) { "done" } else { "other" }).collect();
        assert_eq!(order, vec!["unfetched", "done"]);
        let (_, back) = sent(&mut r);
        assert_eq!(back, vec![(vec![1, 2], "e disconnected from me".to_string(), false)]);
        let _ = fs::remove_dir_all(&root);
    }

    /// Cancel drops an export chunk held here (it ended before the cancel came)
    #[test]
    fn cancel_drops_held() {
        let root = scratch("cancel");
        let mut r = Renderer::new("me", &root.join("work"), PathBuf::new(), PathBuf::new(), 1);
        let dir = root.join("work/export/x3");
        fs::create_dir_all(&dir).unwrap();
        r.held.insert(("me".into(), 3), Held { root: dir.clone(), items: HashMap::new(), index: None, hashes: HashMap::new() });
        r.on_msg(None, LOCAL, Msg::Cancel { id: 3 }, None, &mut |_| {});
        assert!(r.held.is_empty() && !dir.exists());
        let _ = fs::remove_dir_all(&root);
    }

    /// Linked for Blender on Windows: a path over 259 characters and one differing from another only in case fail
    /// their item with the reason (naming --work); the rest renders
    #[test]
    fn windows_paths_checked_before_render() {
        let root = scratch("winpaths");
        let mut r = Renderer::new("me", &root.join("work"), PathBuf::new(), PathBuf::new(), 1);
        r.win_paths = true;
        let mut put = |text: &str| {
            let src = root.join("in");
            fs::write(&src, text).unwrap();
            let (_, h) = hash_file(&src).unwrap();
            r.cas.put(&h, &src).unwrap();
            h
        };
        let (a, b, c, d) = (put("a"), put("b"), put("c"), put("d"));
        let long = format!("Data/{}/t.png", "x".repeat(260));
        waiting(&mut r, 10, 5, "e", 1, &[1, 2, 3, 4]);
        r.pending[0].groups[0].info = Some(vec![
            held_item(1, "Packages/p1", vec![file("Packages/p1/scene.blend", &a), file(&long, &b)]),
            held_item(2, "Packages/p2", vec![file("Packages/p2/scene.blend", &a), file("Data/Objects/Tex.png", &c)]),
            held_item(3, "Packages/p3", vec![file("Packages/p3/scene.blend", &a), file("data/objects/tex.png", &d)]),
            held_item(4, "Packages/p4", vec![file("Packages/p4/scene.blend", &a), file("Data/Objects/Tex.png", &c)]),
        ]);
        r.advance(None);
        let (failed, _) = sent(&mut r);
        assert_eq!(failed.iter().map(|f| f.0).collect::<Vec<_>>(), vec![1, 3], "{failed:?}");
        assert!(failed[0].1.contains("over 259") && failed[0].1.contains("--work"), "{}", failed[0].1);
        assert!(failed[1].1.contains("differ only in case"), "{}", failed[1].1);
        let Some(Queued { work: Work::Render { jobs, .. }, .. }) = r.queue.front() else { panic!("not queued") };
        assert_eq!(jobs.iter().map(|j| j.item).collect::<Vec<_>>(), vec![2, 4]);
        let _ = fs::remove_dir_all(&root);
    }

    /// The same content in two held chunks: the later one released, the earlier one still sends it (by_hash keeps
    /// every chunk's file, a release takes only its own)
    #[test]
    fn shared_content_survives_release() {
        let root = scratch("shared");
        let port = crate::net::test_port();
        let me = |name: &str| Hello { name: name.into(), version: "t".into(), ..Default::default() };
        let mut a = Hub::new(NetConfig { name: "a".into(), token: "t".into(), listen: format!("127.0.0.1:{port}"), peers: Vec::new(), discovery: false }, me("a"), root.join("a"));
        let mut b = Hub::new(NetConfig { name: "b".into(), token: "t".into(), listen: format!("127.0.0.1:{}", crate::net::test_port()), peers: vec![format!("127.0.0.1:{port}")], discovery: false }, me("b"), root.join("b"));
        let mut r = Renderer::new("b", &root.join("work"), PathBuf::new(), PathBuf::new(), 1);
        for (chunk, item) in [(1u64, 1usize), (2, 2)] {
            let x = root.join(format!("work/export/x{chunk}"));
            for (rel, text) in [("Data/shared.png", "shared texture".to_string()), (&*format!("Packages/p{item}/scene.json"), r#"{"t": "Data/shared.png"}"#.to_string()), ("rigs/r.json", "{}".to_string())] {
                fs::create_dir_all(x.join(rel).parent().unwrap()).unwrap();
                fs::write(x.join(rel), text).unwrap();
            }
            let info = HeldInfo { package: format!("Packages/p{item}"), rig: "rigs/r.json".into(), pose: [0.0; 3] };
            r.held.insert(("a".into(), chunk), Held { root: x, items: HashMap::from([(item, info)]), index: None, hashes: HashMap::new() });
        }
        // a renderer asks about chunk 1, another about chunk 2; chunk 2 is done and released
        let (one, ..) = r.describe("a", 1, &[1]);
        let (two, ..) = r.describe("a", 2, &[2]);
        let hash = |items: &[HeldItem]| items[0].files.iter().find(|f| f.path == "Data/shared.png").unwrap().hash.clone();
        assert_eq!(hash(&one), hash(&two));
        r.drop_held(&("a".into(), 2));
        let t0 = Instant::now();
        let conn_b = loop {
            b.poll();
            if let Some(HubEv::Up { conn }) = a.poll().into_iter().find(|e| matches!(e, HubEv::Up { .. })) {
                break conn;
            }
            assert!(t0.elapsed() < Duration::from_secs(10), "no connection");
            thread::sleep(Duration::from_millis(20));
        };
        // the first renderer fetches the shared file only now
        a.send(conn_b, Msg::Fetch { id: 1, hashes: vec![hash(&one)] }, Payload::None);
        let got = 'got: loop {
            assert!(t0.elapsed() < Duration::from_secs(10), "no answer");
            for ev in b.poll() {
                if let HubEv::Msg { conn, msg, dir, .. } = ev {
                    r.on_msg(Some(&b), conn, msg, dir, &mut |_| {});
                }
            }
            for ev in a.poll() {
                if let HubEv::Msg { msg: Msg::FetchData { files, missing, .. }, .. } = ev {
                    break 'got (files.len(), missing);
                }
            }
            thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(got, (1, Vec::<String>::new()), "sent from chunk 1");
        a.close_all();
        b.close_all();
        let _ = fs::remove_dir_all(&root);
    }

    /// A FetchData from another connection than the one asked, with a known id: dropped, the real request and its
    /// hashes stay; the real answer is taken
    #[test]
    fn fetch_data_from_wrong_connection() {
        let root = scratch("wrongconn");
        let mut r = Renderer::new("me", &root.join("work"), PathBuf::new(), PathBuf::new(), 1);
        let mut log = |_: String| {};
        let inbox = root.join("inbox");
        fs::create_dir_all(&inbox).unwrap();
        fs::write(inbox.join("f"), "blend").unwrap();
        let (_, h) = hash_file(&inbox.join("f")).unwrap();
        fs::rename(inbox.join("f"), inbox.join(&h)).unwrap();
        waiting(&mut r, 10, 5, "e", 1, &[1]);
        r.on_items(None, 5, "me", 1, vec![held_item(1, "Packages/p1", vec![file("Packages/p1/scene.blend", &h)])], Vec::new(), false);
        let req = *r.requests.keys().next().unwrap();
        r.on_msg(None, 6, Msg::FetchData { id: req, files: Vec::new(), missing: vec![h.clone()] }, None, &mut log);
        assert_eq!(sent(&mut r), (Vec::new(), Vec::new()), "nothing reported for the wrong answer");
        assert_eq!(r.requests.get(&req).map(|(c, _)| *c), Some(5), "the real request stays");
        assert_eq!(r.inflight.get(&h), Some(&5), "its hash still on its way");
        r.on_msg(None, 5, Msg::FetchData { id: req, files: vec![FileEntry { path: h.clone(), size: 5 }], missing: Vec::new() }, Some(inbox), &mut log);
        assert!(r.requests.is_empty() && r.inflight.is_empty() && r.cas.has(&h));
        assert!(matches!(r.queue.front(), Some(Queued { work: Work::Render { .. }, .. })), "the chunk goes to Blender");
        let _ = fs::remove_dir_all(&root);
    }

    /// Only holders that are not connected plus items held here: the unreachable ones go back (Unfetched), the head
    /// node still gets Fetched (it waits for it), the chunk renders the rest
    #[test]
    fn unreachable_holders_still_fetched() {
        let root = scratch("unreach");
        let mut r = Renderer::new("me", &root.join("work"), PathBuf::new(), PathBuf::new(), 1);
        let x = root.join("work/export/x1");
        fs::create_dir_all(x.join("Packages/p1")).unwrap();
        let info = HeldInfo { package: "Packages/p1".into(), rig: "rigs/r.json".into(), pose: [0.0; 3] };
        r.held.insert(("me".into(), 1), Held { root: x, items: HashMap::from([(1, info)]), index: None, hashes: HashMap::new() });
        let jobs = vec![RenderJob { item: 1, entity: "e1".into(), holder: "me".into(), chunk: 1 }, RenderJob { item: 2, entity: "e2".into(), holder: "far".into(), chunk: 2 }];
        r.start_fetch(None, LOCAL, 10, 64, jobs, PathBuf::new(), None, None, &mut |_| {});
        let order: Vec<&str> = r.local.iter().map(|(m, _)| match m {
            Msg::Unfetched { .. } => "unfetched",
            Msg::Fetched { .. } => "fetched",
            _ => "other",
        }).collect();
        assert_eq!(order, vec!["unfetched", "fetched"]);
        let Some(Queued { work: Work::Render { jobs, .. }, .. }) = r.queue.front() else { panic!("not queued") };
        assert_eq!(jobs.iter().map(|j| j.item).collect::<Vec<_>>(), vec![1]);
        let _ = fs::remove_dir_all(&root);
    }

    /// A holder that does not hold the chunk (restarted, released already): the items go back to be exported again
    /// (Unfetched with export), not failed
    #[test]
    fn chunk_not_held_reexports() {
        let root = scratch("notheld");
        let mut r = Renderer::new("me", &root.join("work"), PathBuf::new(), PathBuf::new(), 1);
        let (items, missing, gone) = r.describe("h", 7, &[1, 2]);
        assert!(items.is_empty() && missing.len() == 2 && gone, "the holder side says the chunk is gone");
        waiting(&mut r, 10, 5, "e", 7, &[1, 2]);
        r.on_items(None, 5, "me", 7, items, missing, gone);
        let (failed, back) = sent(&mut r);
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(back, vec![(vec![1, 2], "export chunk 7 not held on me".to_string(), true)]);
        let _ = fs::remove_dir_all(&root);
    }

    /// Windows: an item whose link fails does not block a later item with a case variant of its paths
    #[test]
    fn failed_link_does_not_block_case_variant() {
        let root = scratch("winlink");
        let mut r = Renderer::new("me", &root.join("work"), PathBuf::new(), PathBuf::new(), 1);
        r.win_paths = true;
        let mut put = |text: &str| {
            let src = root.join("in");
            fs::write(&src, text).unwrap();
            let (_, h) = hash_file(&src).unwrap();
            r.cas.put(&h, &src).unwrap();
            h
        };
        let (a, b, c) = (put("a"), put("b"), put("c"));
        waiting(&mut r, 10, 5, "e", 1, &[1, 2]);
        // a file where item 1 needs a folder: its link fails before it links anything
        let x = r.work.join("fetch/10/x1");
        fs::create_dir_all(&x).unwrap();
        fs::write(x.join("Blocked"), "file").unwrap();
        r.pending[0].groups[0].info = Some(vec![
            held_item(1, "Packages/p1", vec![file("Blocked/t.png", &a), file("Data/Objects/Tex.png", &b)]),
            held_item(2, "Packages/p2", vec![file("Packages/p2/scene.blend", &a), file("data/objects/tex.png", &c)]),
        ]);
        r.advance(None);
        let (failed, _) = sent(&mut r);
        assert_eq!(failed.iter().map(|f| f.0).collect::<Vec<_>>(), vec![1], "{failed:?}");
        assert!(failed[0].1.starts_with("not linked"), "{}", failed[0].1);
        let Some(Queued { work: Work::Render { jobs, .. }, .. }) = r.queue.front() else { panic!("not queued") };
        assert_eq!(jobs.iter().map(|j| j.item).collect::<Vec<_>>(), vec![2]);
        let _ = fs::remove_dir_all(&root);
    }

    /// Windows: items held here get the same path check before rendering as fetched ones (a path over 259 characters
    /// fails its item, the rest renders)
    #[test]
    fn windows_paths_checked_for_held_items() {
        let root = scratch("winheld");
        let mut r = Renderer::new("me", &root.join("work"), PathBuf::new(), PathBuf::new(), 1);
        r.win_paths = true;
        let x = root.join("work/export/x1");
        let long = format!("Data/{}/{}/t.png", "x".repeat(150), "y".repeat(120));
        for (rel, text) in [(long.as_str(), "t".to_string()), ("Packages/p1/scene.json", format!(r#"{{"t": "{long}"}}"#)), ("Packages/p2/scene.json", "{}".to_string()), ("rigs/r.json", "{}".to_string())] {
            fs::create_dir_all(x.join(rel).parent().unwrap()).unwrap();
            fs::write(x.join(rel), text).unwrap();
        }
        let info = |p: &str| HeldInfo { package: p.into(), rig: "rigs/r.json".into(), pose: [0.0; 3] };
        r.held.insert(("me".into(), 1), Held { root: x, items: HashMap::from([(1, info("Packages/p1")), (2, info("Packages/p2"))]), index: None, hashes: HashMap::new() });
        let jobs = (1..=2).map(|i| RenderJob { item: i, entity: format!("e{i}"), holder: "me".into(), chunk: 1 }).collect();
        r.start_fetch(None, LOCAL, 10, 64, jobs, PathBuf::new(), None, None, &mut |_| {});
        let (failed, _) = sent(&mut r);
        assert_eq!(failed.iter().map(|f| f.0).collect::<Vec<_>>(), vec![1], "{failed:?}");
        assert!(failed[0].1.contains("over 259"), "{}", failed[0].1);
        let Some(Queued { work: Work::Render { jobs, .. }, .. }) = r.queue.front() else { panic!("not queued") };
        assert_eq!(jobs.iter().map(|j| j.item).collect::<Vec<_>>(), vec![2]);
        let _ = fs::remove_dir_all(&root);
    }

    /// A node exports an export chunk, holds it, renders an item of it from there and drops it on release (real
    /// StarBreaker, Data.p4k and Blender of the repo; SC_DATA_P4K must be set): cargo test --release -- --ignored
    /// export_hold_render
    #[test]
    #[ignore]
    fn export_hold_render() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let p4k = PathBuf::from(std::env::var("SC_DATA_P4K").expect("SC_DATA_P4K"));
        let blender = fs::read_dir(repo.join("local/blender")).unwrap().flatten().map(|e| e.path().join(crate::exe("blender"))).find(|p| p.exists()).unwrap();
        let root = std::env::temp_dir().join(format!("packshot-node-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let tables = crate::cas::tests::repo_tables(&root.join("tables"));
        let rig = tables.rig("", "").unwrap().to_string_lossy().to_string();
        let port = crate::net::test_port();
        let me = |name: &str| Hello { name: name.into(), version: "t".into(), render: true, export: true, slots: 1, ..Default::default() };
        // a: the head node (listens), b: the node that exports, holds and renders
        let mut a = Hub::new(NetConfig { name: "a".into(), token: "t".into(), listen: format!("127.0.0.1:{port}"), peers: Vec::new(), discovery: false }, me("a"), root.join("a"));
        let mut b = Hub::new(NetConfig { name: "b".into(), token: "t".into(), listen: format!("127.0.0.1:{}", crate::net::test_port()), peers: vec![format!("127.0.0.1:{port}")], discovery: false }, me("b"), root.join("b"));
        let mut r = Renderer::new("b", &root.join("work"), blender, PathBuf::new(), 1);
        r.tools = Some(ExportTools { starbreaker: repo.join("local/starbreaker-qm").join(crate::exe("starbreaker")), p4k });
        let t0 = Instant::now();
        let conn_a = loop {
            b.poll();
            if let Some(HubEv::Up { conn }) = a.poll().into_iter().find(|e| matches!(e, HubEv::Up { .. })) {
                break conn;
            }
            assert!(t0.elapsed() < Duration::from_secs(10), "no connection");
            thread::sleep(Duration::from_millis(20));
        };
        let job = |item: usize, entity: &str| ItemJob { item, entity: entity.into(), worn: false, kind: String::new(), sub_type: String::new(), pose: [90.0, 180.0, 0.0], rig: rig.clone() };
        a.send(conn_a, Msg::Export { id: 1, jobs: vec![job(7, "thumbnailentity_resource_rock_gold"), job(8, "no_such_entity_xyz")] }, Payload::None);
        let mut log = |s: String| println!("{s}");
        let mut exported = Vec::new();
        let mut items = Vec::new();
        let mut stage = 0;
        while stage < 2 {
            assert!(t0.elapsed() < Duration::from_secs(120), "timeout");
            for ev in b.poll() {
                if let HubEv::Msg { conn, msg, dir, .. } = ev {
                    r.on_msg(Some(&b), conn, msg, dir, &mut log);
                }
            }
            r.tick(Some(&b), Duration::from_millis(20), &mut log);
            for ev in a.poll() {
                match ev {
                    HubEv::Msg { msg: Msg::ItemExported { item, ok, .. }, .. } => exported.push((item, ok)),
                    HubEv::Msg { msg: Msg::ExportDone { .. }, .. } => {
                        // render the exported item where it is held, with the repo's render.py
                        let script = repo.join("src/icons/render.py");
                        let files = vec![FileEntry { path: "render.py".into(), size: fs::metadata(&script).unwrap().len() }];
                        let jobs = vec![RenderJob { item: 7, entity: "thumbnailentity_resource_rock_gold".into(), holder: "b".into(), chunk: 1 }];
                        a.send(conn_a, Msg::Render { id: 2, size: 128, jobs, files }, Payload::Files(vec![script]));
                        stage = 1;
                    }
                    HubEv::Msg { msg: Msg::Item { item, ok, info, .. }, data, .. } => items.push((item, ok, data.len(), info)),
                    HubEv::Msg { msg: Msg::BlockDone { .. }, .. } => stage = 2,
                    _ => {}
                }
            }
        }
        exported.sort();
        assert_eq!(exported, vec![(7, true), (8, false)], "the unknown entity fails in the export");
        println!("{items:?}");
        assert!(items.len() == 1 && items[0].0 == 7 && items[0].1 && items[0].2 > 1000, "rendered from the held chunk and sent");
        assert!(root.join("work/export/x1").exists(), "held until released");
        a.send(conn_a, Msg::Release { chunks: vec![1] }, Payload::None);
        while root.join("work/export/x1").exists() {
            assert!(t0.elapsed() < Duration::from_secs(130), "not released");
            for ev in b.poll() {
                if let HubEv::Msg { conn, msg, dir, .. } = ev {
                    r.on_msg(Some(&b), conn, msg, dir, &mut log);
                }
            }
            thread::sleep(Duration::from_millis(20));
        }
        a.close_all();
        b.close_all();
        let _ = fs::remove_dir_all(&root);
    }
}

