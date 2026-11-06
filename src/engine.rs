// The run: selection, scheduling, the images into the run and the target set (store.rs), log. The node that holds a
// run is its head node, whichever node started it (cluster.rs): it cuts export chunks (each for one StarBreaker
// `entity export-batch` process) for the exporting nodes and render chunks (each for one Blender with render.py) for
// the rendering ones, itself included, and files the results. Between the stages each exported item waits in the pool,
// held on the node that exported it; a render chunk takes items from one holder, its own first (node.rs fetches the
// rest). Everything runs in the main thread; the processes run in node.rs and report through its messages.
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::events::{EventLog, GpuProbe, file_hash, round3};
use crate::net::{Hello, Hub, ItemJob, Msg, Payload, RenderJob, RunStatus, RunSummary};
use crate::node::LOCAL;
use crate::store::{EntityResult, Resolved, RunInfo, SetInfo, Store};
use crate::tables::Tables;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Target {
    pub uuids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub names: Option<Vec<String>>,
    pub entity: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "subType")]
    pub sub_type: String,
    #[serde(default)]
    pub worn: bool,
}

impl Target {
    pub fn name(&self) -> &str {
        self.names.as_ref().and_then(|n| n.first()).map(String::as_str).unwrap_or(&self.entity)
    }
}

pub struct Config {
    pub p4k: PathBuf,
    pub starbreaker: PathBuf,
    pub blender: PathBuf,
    pub render_script: PathBuf,
    pub work: PathBuf,
    pub size: u32,
    pub chunk: usize,
}

/// What gets rendered from the filter selection
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// only without a render
    Missing,
    /// those that failed on the last attempt in the set, even if an old image exists
    Failed,
    /// everything in the filter anew, overwrites existing ones
    All,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Scope::Missing => "missing",
            Scope::Failed => "failed last time",
            Scope::All => "re-render all (force)",
        }
    }
    pub fn next(self) -> Self {
        match self {
            Scope::Missing => Scope::Failed,
            Scope::Failed => Scope::All,
            Scope::All => Scope::Missing,
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "missing" => Some(Scope::Missing),
            "failed" => Some(Scope::Failed),
            "all" | "force" => Some(Scope::All),
            _ => None,
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Scope::Missing => "missing",
            Scope::Failed => "failed",
            Scope::All => "all",
        }
    }
}

/// Name of a run without --name: the filter terms (none: nothing), what the scope does (missing, retry, full) and the
/// first 6 characters of the add-on's and render.py's hashes, the two things that change the images:
/// "arms-legs_full_addon-6ea0d4_renderpy-6da3cb", "missing_addon-…"
pub fn default_run_name(filter: &str, scope: Scope, addon: Option<&str>, render_py: Option<&str>) -> String {
    let terms: Vec<&str> = filter.split([' ', ',']).filter(|s| !s.is_empty()).collect();
    let what = match scope {
        Scope::Missing => "missing",
        Scope::Failed => "retry",
        Scope::All => "full",
    };
    let short = |h: Option<&str>| h.map(|h| h.chars().take(6).collect::<String>()).unwrap_or_else(|| "none".into());
    let mut parts = Vec::new();
    if !terms.is_empty() {
        parts.push(crate::store::slug(&terms.join("-")).chars().take(32).collect::<String>());
    }
    parts.push(what.to_string());
    parts.push(format!("addon-{}", short(addon)));
    parts.push(format!("renderpy-{}", short(render_py)));
    parts.join("_")
}

pub struct Selection {
    /// matches of the filter
    pub matched: usize,
    /// of those: with render, without render, last failed
    pub existing: usize,
    pub missing: usize,
    pub failed: usize,
    /// what would be rendered (indices into targets, plan order)
    pub todo: Vec<usize>,
    /// of those with an existing render that gets overwritten
    pub overwrite: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Ok,
    Error,
}

pub struct LogLine {
    pub time: String,
    pub level: Level,
    pub text: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Export,
    Render,
}

pub struct Worker {
    pub kind: Kind,
    pub slot: usize,
    /// the chunk's id, the same on the node doing it
    pub chunk: u64,
    /// the node doing it; None: this node (its own Renderer, through node::LOCAL)
    pub node: Option<String>,
    /// host name of the node doing the work (this one or the node)
    pub host: String,
    pub conn: u64,
    /// items in the order the process works through them
    pub order: Vec<usize>,
    pub reported: HashSet<usize>,
    /// an export chunk: the items exported so far (held on its node once the process ends)
    pub(crate) exported: Vec<usize>,
    /// a render chunk: its files are on its node (held there, or fetched from their holders)
    pub fetched: bool,
    pub since: Instant,
    pub(crate) started: Instant,
    /// sum of the seconds export-batch reports per item (the rest of the process time is loading and writing)
    pub(crate) item_secs: f64,
}

impl Worker {
    /// "DRAGONEGG/E1": E an export (export-batch), R a render (Blender)
    pub fn label(&self) -> String {
        format!("{}/{}{}", self.host, if self.kind == Kind::Export { 'E' } else { 'R' }, self.slot)
    }
    /// the item the process is currently working on (both tools work through their list in order)
    pub fn current(&self) -> Option<usize> {
        self.order.iter().copied().find(|t| !self.reported.contains(t))
    }

    /// What the views show: (phase, current item, done, total); a render chunk whose files are still coming from
    /// their holders is in "fetch"
    pub fn progress(&self) -> (&'static str, Option<usize>, usize, usize) {
        let phase = match self.kind {
            Kind::Export => "export",
            Kind::Render if !self.fetched => "fetch",
            Kind::Render => "render",
        };
        (phase, self.current(), self.reported.len(), self.order.len())
    }
}

/// No progress (no item reported) on a chunk for this long: it is cancelled and its open items go back
pub const CHUNK_TIMEOUT: Duration = Duration::from_secs(300);
/// A render chunk still getting its files from other nodes may take this many CHUNK_TIMEOUTs (a slow link is no
/// stall); then it is cancelled and every one of its items counts
pub const FETCH_TIMEOUTS: u32 = 6;
/// An item goes back at most this often (its node dropped, its chunk timed out), then it fails
pub const MAX_REQUEUES: u32 = 2;
/// Exports wait while exported items not rendered yet (and those being exported) reach this many render chunks per
/// Blender in the cluster: bounds the disk the holders need
pub const BACKLOG: usize = 2;

/// Exported items waiting for a Blender with the time they came
pub(crate) type Pool = VecDeque<(usize, Instant)>;

/// An export chunk whose items are held on its node until every one of them is rendered or failed
pub(crate) struct XChunk {
    pub(crate) node: String,
    pub(crate) conn: u64,
    /// items not done yet
    pub(crate) open: usize,
}

pub struct Run {
    /// items to export
    pub(crate) todo: VecDeque<usize>,
    /// exported items waiting for a Blender, oldest first, with the time they came
    pub(crate) pool: Pool,
    /// exported items not done yet (in the pool or in a render chunk) -> their export chunk
    pub(crate) held: HashMap<usize, u64>,
    pub(crate) xchunks: HashMap<u64, XChunk>,
    /// how often an item went back
    pub(crate) requeues: HashMap<usize, u32>,
    /// items an Unfetched put back into the pool (counted there) and not placed since: their holder's drop does not
    /// count them again (one incident, one count)
    pub(crate) uncounted: HashSet<usize>,
    /// item -> the renderers that could not reach its holder: not placed there again; exported again once no other
    /// renderer is left (cleared when it is exported again)
    pub(crate) unreachable: HashMap<usize, HashSet<String>>,
    /// nodes that were connected at the start and take part
    pub nodes: Vec<String>,
    /// the node that started the run (this one or another), filter and scope: for the status of the others
    pub starter: String,
    pub filter: String,
    pub scope: Scope,
    /// the final status went out to the other nodes
    pub(crate) status_sent: bool,
    pub total: usize,
    pub ok: usize,
    pub failed: usize,
    pub exported: usize,
    pub started: Instant,
    pub finished: Option<Instant>,
    pub aborted: bool,
    /// times of finished renders (throughput, ETA)
    pub done_at: Vec<Instant>,
    /// times of the images per node that rendered them (the nodes' throughput in view 3)
    pub rendered_by: HashMap<String, Vec<Instant>>,
    /// sum of the phases from render.py (PHASES) over `phase_n` renders
    pub phase_sum: [f64; 5],
    pub phase_n: usize,
    /// run_end is already in the event log
    logged_end: bool,
}

/// Phases per render, as render.py appends them to the icon: line ("timing load=… prep=… render=… glass=… post=…")
pub const PHASES: [&str; 5] = ["load", "prep", "render", "glass", "post"];

pub(crate) fn parse_timing(info: &str) -> Option<[f64; 5]> {
    let rest = &info[info.find("timing ")? + 7..];
    let mut out = [0.0; 5];
    for pair in rest.split_whitespace() {
        let (k, v) = pair.split_once('=')?;
        out[PHASES.iter().position(|p| *p == k)?] = v.parse().ok()?;
    }
    Some(out)
}

impl Run {
    pub fn done(&self) -> usize {
        self.ok + self.failed
    }
    /// mean per phase over the renders so far
    pub fn phase_avg(&self) -> Option<[f64; 5]> {
        (self.phase_n > 0).then(|| self.phase_sum.map(|s| s / self.phase_n as f64))
    }
    /// (items before export, exported items waiting for a Blender)
    pub fn waiting(&self) -> (usize, usize) {
        (self.todo.len(), self.pool.len())
    }

    /// Exported items not rendered yet (waiting in the pool or in a render chunk) per node holding them, by name
    pub fn holders(&self) -> Vec<(String, usize)> {
        let mut by: std::collections::BTreeMap<&str, usize> = Default::default();
        for x in self.held.values() {
            if let Some(c) = self.xchunks.get(x) {
                *by.entry(c.node.as_str()).or_default() += 1;
            }
        }
        by.into_iter().map(|(n, c)| (n.to_string(), c)).collect()
    }
}

/// The selection's head node when it is another node: its sets and the state of the chosen set there
pub struct RemoteSel {
    pub node: String,
    pub set: String,
    pub sets: Vec<String>,
    /// its sets with their runs and target lists (the Sets view)
    pub set_list: Vec<crate::store::SetSummary>,
    /// its latest run's throughput and image size (the estimate)
    pub stats: Option<crate::store::RunStats>,
    pub images: HashSet<String>,
    pub failed: HashSet<String>,
    /// the state arrived (no start before)
    pub loaded: bool,
}

/// A run on another node, as its head node reports it
pub struct RemoteRun {
    pub from: String,
    pub status: RunStatus,
    pub at: Instant,
}

pub struct Engine {
    pub cfg: Config,
    /// this node's target list, then the targets of runs other nodes started here (appended, so indices stay valid)
    pub targets: Vec<Target>,
    /// how many of `targets` are this node's own list (the selection only looks at those)
    pub own_len: usize,
    pub(crate) tables: Tables,
    pub store: Store,
    /// the target set and its state per entity
    pub set: SetInfo,
    pub(crate) resolved: Resolved,
    /// run.json of the current or last run
    pub record: Option<RunInfo>,
    pub run: Option<Run>,
    pub workers: Vec<Worker>,
    pub max_exporters: usize,
    pub max_renderers: usize,
    /// physical cores (basis of the process counts, display)
    pub cores: usize,
    pub paused: bool,
    pub log: VecDeque<LogLine>,
    pub errors: Vec<(usize, String)>,
    /// most recently rendered items, newest last
    pub history: VecDeque<usize>,
    /// log lines for console mode (--plain), collected there
    pub echo: Option<Vec<String>>,
    haystack: Vec<String>,
    log_file: Option<File>,
    /// event log of the current or last run (runs/<run>/events.jsonl)
    pub events: Option<EventLog>,
    gpu: Option<GpuProbe>,
    sys: Option<sysinfo::System>,
    last_sample: Instant,
    /// cluster (cluster.rs): connections, own versions, runs of the other nodes, manifest
    pub hub: Option<Hub>,
    pub me: Hello,
    pub peer_runs: HashMap<String, Vec<RunSummary>>,
    pub manifest: Option<Manifest>,
    /// runs asked for: (node asked, run id); a run's files are taken only from that node
    pub(crate) fetching: HashSet<(String, String)>,
    pub(crate) next_remote: u64,
    /// bytes the nodes fetched from each other for this node's runs in this session
    pub transferred: u64,
    /// this node's exports and renders, for its own runs (through node::LOCAL) and for other nodes' runs (node.rs)
    pub serve: Option<crate::node::Renderer>,
    /// a chunk without progress for this long is cancelled (CHUNK_TIMEOUT; tests shorten it)
    pub chunk_timeout: Duration,
    /// head node for the next start: None = this node, else a connected node's host name
    pub head: Option<String>,
    pub remote: Option<RemoteSel>,
    /// the latest status of a run on another node
    pub remote_run: Option<RemoteRun>,
    pub(crate) last_status: Instant,
    /// folders whose disks the hello reports (packshot folder, work folder), checked every 10 s
    pub disk_paths: Vec<PathBuf>,
    pub(crate) last_disk: Instant,
    /// counts up when the remote head node's state arrives (the TUI recomputes its selection)
    pub state_seq: u64,
    /// the file offered for a set's target list (targets.json in the packshot folder)
    pub targets_file: PathBuf,
    /// throughput and image size of this node's latest run (the estimate when it is the head node)
    pub stats: Option<crate::store::RunStats>,
    /// the DataCore tables (pose, light rigs) are there; with --export they load in the background at the start, the
    /// node exports nothing until then
    pub tables_ready: bool,
    tables_load: Option<TablesLoad>,
    /// this node's compute assessment from assessment.json (assess.rs), what makes it stale (empty: valid), the one
    /// going on
    pub assessment: Option<crate::assess::Assessment>,
    pub assess_stale: Vec<String>,
    pub(crate) assess_job: Option<crate::assess::AssessJob>,
    /// test set size and the largest Blender count of a sweep (tests shorten them)
    pub assess_items: usize,
    pub assess_max: usize,
    /// --workers, --exporters given: an assessment does not change them
    pub fixed_counts: (bool, bool),
}

/// The DataCore tables loading in the background: its steps and its result come through the channel
struct TablesLoad {
    rx: Receiver<std::result::Result<String, std::result::Result<Tables, String>>>,
    pub started: Instant,
}

/// State of a foreign set (another node's live) for the scope "missing"
#[derive(Serialize, Deserialize, Clone)]
pub struct Manifest {
    pub from: String,
    pub set: String,
    pub at: String,
    pub entities: std::collections::BTreeMap<String, bool>,
}

/// What the filter searches in: class name, display names, type/subtype (lower case)
fn search_text(t: &Target) -> String {
    format!("{}\n{}\n{}/{}", t.entity, t.names.as_ref().map(|n| n.join("\n")).unwrap_or_default(), t.kind, t.sub_type).to_lowercase()
}

/// Blender version from `blender --version` ("Blender 5.2.3"), otherwise from the folder name
pub fn blender_version(blender: &Path) -> String {
    if let Ok(out) = Command::new(blender).arg("--version").stdin(Stdio::null()).stderr(Stdio::null()).output() {
        let text = String::from_utf8_lossy(&out.stdout);
        if let Some(v) = text.lines().find_map(|l| l.trim().strip_prefix("Blender ")) {
            return v.split_whitespace().next().unwrap_or(v).to_string();
        }
    }
    let dir = blender.parent().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    dir.split('-').find(|p| p.chars().next().is_some_and(|c| c.is_ascii_digit())).unwrap_or(&dir).to_string()
}

/// An error with a process's output as its trace (record: first line the message, the rest the trace): the last
/// TRACE_LINES lines of `output`
pub fn with_trace(msg: &str, output: &str) -> String {
    let lines: Vec<&str> = output.trim().lines().collect();
    let tail = lines[lines.len().saturating_sub(TRACE_LINES)..].join("\n");
    if tail.is_empty() { msg.to_string() } else { format!("{msg}\n{tail}") }
}

/// Lines of a process's output that go with a failure
pub const TRACE_LINES: usize = 20;

/// Keep about the last `max` bytes of a text, cut at a character boundary
pub fn keep_tail(s: &mut String, max: usize) {
    if s.len() > max {
        let mut cut = s.len() - max;
        while !s.is_char_boundary(cut) {
            cut += 1;
        }
        s.drain(..cut);
    }
}

fn now_local() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

fn sanitize(name: &str) -> String {
    let s: String = name.chars().filter(|c| !matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') && (*c as u32) >= 0x20).collect();
    s.trim().chars().take(80).collect()
}

/// File name of an icon as in cli.ts: "display name (class name) <uuid>.png"
fn icon_file(t: &Target, k: usize) -> String {
    let name = sanitize(t.names.as_ref().and_then(|n| n.get(k)).map(String::as_str).unwrap_or(""));
    let prefix = if name.is_empty() { String::new() } else { format!("{name} ") };
    format!("{prefix}({}) {}.png", t.entity, t.uuids[k])
}

pub(crate) fn too_long(root: &Path, limit: usize) -> (usize, Option<PathBuf>) {
    let mut count = 0;
    let mut first = None;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.as_os_str().len() > limit {
                count += 1;
                first.get_or_insert(p.clone());
            }
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(p);
            }
        }
    }
    (count, first)
}

/// The pool's items by holder that the renderer `host` can take (not those it could not reach)
pub(crate) fn pool_by_holder<'a>(run: &'a Run, host: &str) -> HashMap<&'a str, usize> {
    let mut count: HashMap<&str, usize> = HashMap::new();
    for (t, _) in &run.pool {
        if run.unreachable.get(t).is_some_and(|r| r.contains(host)) {
            continue;
        }
        if let Some(x) = run.held.get(t).and_then(|x| run.xchunks.get(x)) {
            *count.entry(x.node.as_str()).or_default() += 1;
        }
    }
    count
}

/// A chunk size that shrinks towards the end of a run: the remainder / (3 × processes), at least 10, at most
/// --chunk, and for small remainders so that every process gets something
fn guided(rem: usize, slots: usize, max: usize) -> usize {
    let (rem, slots) = (rem.max(1), slots.max(1));
    rem.div_ceil(3 * slots).max(10).min(max.max(1)).min(rem.div_ceil(slots)).max(1)
}

/// A node's items/s at `k` processes from its assessment's steps (count, items/s): linear between the steps, from 0
/// below the first, flat beyond the last (the CPU is saturated there)
pub(crate) fn rate_at(steps: &[(usize, f64)], k: usize) -> Option<f64> {
    let mut s: Vec<(usize, f64)> = steps.iter().copied().filter(|x| x.0 > 0).collect();
    s.sort_by_key(|x| x.0);
    let (first, last) = (*s.first()?, *s.last()?);
    if k <= first.0 {
        return Some(first.1 * k as f64 / first.0 as f64);
    }
    if k >= last.0 {
        return Some(last.1);
    }
    let i = s.iter().position(|x| x.0 >= k)?;
    let (a, b) = (s[i - 1], s[i]);
    Some(a.1 + (b.1 - a.1) * (k - a.0) as f64 / (b.0 - a.0) as f64)
}

/// Items/s per process of a node from its valid assessment (render or export), at the count it runs now (its
/// hello's; changed with +/- or </> from the assessed one: the rate at that count from the steps, scaled like the
/// assessed one, which for an exporting node was measured with its exports alongside); None without one
pub(crate) fn proc_rate(h: &Hello, export: bool) -> Option<f64> {
    let a = h.assess.as_ref().filter(|a| a.valid)?;
    let (rate, n, steps, now) = if export { (a.export, a.exporters, &a.export_steps, h.exporters) } else { (a.render, a.workers, &a.steps, h.slots) };
    if rate <= 0.0 || n == 0 {
        return None;
    }
    let k = if now == 0 { n } else { now };
    let total = match (k == n, rate_at(steps, k), rate_at(steps, n)) {
        (false, Some(at), Some(base)) if base > 0.0 => rate * at / base,
        _ => rate * k as f64 / n as f64,
    };
    Some(total / k as f64)
}

/// A node's chunk share: its rate per process against the mean per process of the nodes with a rate (nodes:
/// connection, processes, rate per process); 1 for a node without a rate or without any rate in the cluster
pub(crate) fn rate_share(nodes: &[(u64, usize, Option<f64>)], conn: u64) -> f64 {
    let known: Vec<(u64, usize, f64)> = nodes.iter().filter_map(|&(c, n, r)| r.map(|r| (c, n, r))).filter(|k| k.1 > 0).collect();
    let procs: usize = known.iter().map(|k| k.1).sum();
    let Some(own) = known.iter().find(|k| k.0 == conn) else { return 1.0 };
    let mean = known.iter().map(|k| k.1 as f64 * k.2).sum::<f64>() / procs.max(1) as f64;
    if mean > 0.0 { own.2 / mean } else { 1.0 }
}

/// The free exporter that takes the next export chunk: the fastest by assessed rate per process, then the one with
/// the most free processes, this node on a tie (free: connection, node, free processes)
pub(crate) fn pick_exporter(free: Vec<(u64, Option<String>, usize)>, rates: &[(u64, usize, Option<f64>)]) -> Option<(u64, Option<String>, usize)> {
    free.into_iter().max_by(|a, b| rate_share(rates, a.0).total_cmp(&rate_share(rates, b.0)).then((a.2, a.0 == LOCAL).cmp(&(b.2, b.0 == LOCAL))))
}

/// The order in which free Blenders are offered a render chunk, by (holds less than a chunk, rate share, free
/// Blenders): those holding a chunk's worth first, then the fastest, then the most free
pub(crate) fn renderer_order(a: (bool, f64, usize), b: (bool, f64, usize)) -> std::cmp::Ordering {
    a.0.cmp(&b.0).then(b.1.total_cmp(&a.1)).then(b.2.cmp(&a.2))
}

/// A node's chunk: the guided size scaled by its share, at least 1, at most --chunk and the remainder
pub(crate) fn sized(base: usize, share: f64, max: usize, rem: usize) -> usize {
    ((base as f64 * share).round() as usize).clamp(1, max.max(1)).min(rem.max(1))
}

impl Engine {
    /// `set`: the target set; its folder is checked against its chain and repaired
    pub fn new(cfg: Config, targets: Vec<Target>, tables: Tables, mut store: Store, mut set: SetInfo, hub: Option<Hub>, me: Hello) -> Result<Self> {
        for d in [&cfg.work.join("export"), &cfg.work.join("render")] {
            fs::create_dir_all(d).with_context(|| d.display().to_string())?;
        }
        store.sync(&mut set)?;
        let resolved = store.resolve(&set)?;
        let haystack = targets.iter().map(search_text).collect();
        let manifest = crate::cluster::load_manifest(&store.root);
        let mut serve = crate::node::Renderer::new(&me.name, &cfg.work, cfg.blender.clone(), cfg.render_script.clone(), 0);
        if me.export {
            serve.tools = Some(crate::node::ExportTools { starbreaker: cfg.starbreaker.clone(), p4k: cfg.p4k.clone() });
        }
        let stats = store.last_stats();
        let own_len = targets.len();
        Ok(Self {
            cfg,
            targets,
            own_len,
            tables,
            store,
            set,
            resolved,
            hub,
            me,
            peer_runs: HashMap::new(),
            manifest,
            fetching: HashSet::new(),
            next_remote: 1,
            transferred: 0,
            serve: Some(serve),
            chunk_timeout: CHUNK_TIMEOUT,
            record: None,
            run: None,
            workers: Vec::new(),
            max_exporters: 2,
            max_renderers: 4,
            cores: 0,
            paused: false,
            log: VecDeque::new(),
            errors: Vec::new(),
            history: VecDeque::new(),
            echo: None,
            haystack,
            log_file: None,
            events: None,
            gpu: None,
            sys: sysinfo::System::new().ok(),
            last_sample: Instant::now(),
            head: None,
            remote: None,
            remote_run: None,
            last_status: Instant::now(),
            disk_paths: Vec::new(),
            last_disk: Instant::now(),
            state_seq: 0,
            targets_file: PathBuf::new(),
            stats,
            tables_ready: true,
            tables_load: None,
            assessment: None,
            assess_stale: Vec::new(),
            assess_job: None,
            assess_items: crate::assess::TEST_ITEMS,
            assess_max: 32,
            fixed_counts: (false, false),
        })
    }

    /// Load the DataCore tables in the background (the TUI shows the steps); until they are there this node exports
    /// nothing
    pub fn load_tables(&mut self, refresh: bool) {
        let (tx, rx) = channel();
        let (sb, p4k, work) = (self.cfg.starbreaker.clone(), self.cfg.p4k.clone(), self.cfg.work.clone());
        thread::spawn(move || {
            let step = tx.clone();
            let res = Tables::load(&sb, &p4k, &work, refresh, move |s| {
                let _ = step.send(Ok(s.to_string()));
            });
            let _ = tx.send(Err(res.map_err(|e| format!("{e:#}"))));
        });
        self.tables_ready = false;
        self.tables_load = Some(TablesLoad { rx, started: Instant::now() });
    }

    /// Seconds the tables have been loading, None when they are not loading
    pub fn tables_loading(&self) -> Option<f64> {
        self.tables_load.as_ref().map(|l| l.started.elapsed().as_secs_f64())
    }

    /// The export workers this node offers now: none while its tables are not there
    pub fn exporters_now(&self) -> usize {
        if self.tables_ready { self.max_exporters } else { 0 }
    }

    fn poll_tables(&mut self) {
        let Some(load) = self.tables_load.as_ref() else { return };
        let mut msgs = Vec::new();
        while let Ok(m) = load.rx.try_recv() {
            msgs.push(m);
        }
        let secs = load.started.elapsed().as_secs_f64();
        for m in msgs {
            match m {
                // the steps are not logged: only "ready" (exports start) or the error
                Ok(_) => {}
                Err(Ok(tables)) => {
                    self.tables = tables;
                    self.tables_ready = true;
                    self.tables_load = None;
                    self.note(Level::Ok, format!("DataCore tables ready in {secs:.1} s: exports start"));
                }
                Err(Err(e)) => {
                    self.tables_load = None;
                    self.note(Level::Error, format!("DataCore tables not loaded, this node does not export: {e}"));
                }
            }
        }
    }

    /// A new target list for the selection: the target set's (here) or the one the remote head node sent. Only
    /// while no run is going here (the run's indices point into the list)
    pub fn use_targets(&mut self, targets: Vec<Target>) {
        if self.running() {
            return;
        }
        self.haystack = targets.iter().map(search_text).collect();
        self.own_len = targets.len();
        self.targets = targets;
        self.history.clear();
        self.errors.clear();
    }

    /// Targets of a run another node started here: appended behind the own list, their indices
    pub(crate) fn add_targets(&mut self, targets: Vec<Target>) -> Vec<usize> {
        let first = self.targets.len();
        for t in targets {
            self.haystack.push(String::new());
            self.targets.push(t);
        }
        (first..self.targets.len()).collect()
    }

    pub(crate) fn emit(&mut self, ev: &str, fields: serde_json::Value) {
        if let Some(l) = self.events.as_mut() {
            l.emit(ev, fields);
        }
    }

    /// Origin of the run for run_start: machine, processes, versions of render.py, addon, StarBreaker and p4k
    #[allow(clippy::too_many_arguments)]
    fn run_meta(&self, run_id: &str, args_line: &str, filter: &str, scope: Scope, total: usize, blocks: usize, size: usize) -> serde_json::Value {
        let version = self.me.blender.clone();
        // the game version comes from the exporting nodes (their Data.p4k are in sync): this one, else the first
        // exporting node that takes part
        let exporter = if self.me.export {
            Some(self.me.clone())
        } else {
            let nodes = self.run.as_ref().map(|r| r.nodes.clone()).unwrap_or_default();
            self.connected_nodes().into_iter().map(|(_, h)| h).find(|h| h.export && nodes.contains(&h.name))
        };
        let p4k = self.me.export.then(|| fs::metadata(&self.cfg.p4k).ok()).flatten();
        json!({
            "run": run_id,
            "set": self.set.name,
            "host": self.me.name,
            "starter": self.run.as_ref().map(|r| r.starter.clone()).unwrap_or_default(),
            "nodes": self.run.as_ref().map(|r| r.nodes.clone()).unwrap_or_default(),
            "os": std::env::consts::OS,
            "args": args_line,
            "filter": filter,
            "scope": scope.key(),
            "total": total,
            "blocks": blocks,
            "block_size": size,
            "renderers": self.max_renderers,
            "exporters": self.max_exporters,
            "cores": self.cores,
            "blender": self.cfg.blender,
            "blender_version": version,
            "gpu_backend": "vulkan",
            "render_py": file_hash(&self.cfg.render_script),
            "addon": self.me.addon,
            "starbreaker": self.me.starbreaker,
            "p4k": self.me.export.then(|| self.cfg.p4k.clone()),
            "p4k_from": exporter.as_ref().map(|h| h.name.clone()),
            "p4k_bytes": exporter.as_ref().and_then(|h| h.p4k_bytes),
            "p4k_id": exporter.as_ref().and_then(|h| h.p4k_id.clone()),
            "p4k_version": exporter.as_ref().and_then(|h| h.p4k_version.clone()),
            "p4k_mtime": p4k.and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()),
            "size": self.cfg.size,
        })
    }

    /// Sample every 2 s during a run: CPU, GPU, running processes, queue
    fn sample(&mut self) {
        if !self.running() || self.last_sample.elapsed() < Duration::from_secs(2) {
            return;
        }
        self.last_sample = Instant::now();
        let cpu = self.sys.as_mut().map(|s| {
            s.refresh_cpu_usage();
            round3(f64::from(s.global_cpu_usage()))
        });
        let gpu = self.gpu.as_ref().and_then(GpuProbe::get);
        let exporting = self.workers.iter().filter(|w| w.kind == Kind::Export).count();
        let rendering = self.workers.len() - exporting;
        let (pending, ready, held, done) = self.run.as_ref().map(|r| (r.todo.len(), r.pool.len(), r.held.len(), r.done())).unwrap_or_default();
        let (max_renderers, max_exporters, paused, slots) = (self.max_renderers, self.max_exporters, self.paused, self.total_slots());
        let cache = self.serve.as_ref().map(crate::node::Renderer::cached_bytes).unwrap_or(0);
        self.emit("sample", json!({
            "cpu": cpu, "gpu": gpu, "exporting": exporting, "rendering": rendering, "pending": pending, "ready": ready, "held": held,
            "done": done, "max_renderers": max_renderers, "max_exporters": max_exporters, "slots": slots, "paused": paused, "cache_bytes": cache,
        }));
    }

    /// Conclusion in the event log (finished or aborted)
    fn emit_run_end(&mut self) {
        let Some(r) = self.run.as_mut() else { return };
        if r.logged_end {
            return;
        }
        r.logged_end = true;
        let r = &*r;
        let wall = r.finished.unwrap_or_else(Instant::now).duration_since(r.started).as_secs_f64();
        let phases: serde_json::Map<String, serde_json::Value> =
            r.phase_avg().map(|a| PHASES.iter().zip(a).map(|(k, v)| (k.to_string(), json!(round3(v)))).collect()).unwrap_or_default();
        let fields = json!({ "wall": round3(wall), "ok": r.ok, "failed": r.failed, "total": r.total, "aborted": r.aborted, "phase_avg": phases });
        self.emit("run_end", fields);
        self.gpu = None;
    }

    /// Where Blender renders to; the image then moves into the run
    pub(crate) fn render_png(&self, t: usize) -> PathBuf {
        self.cfg.work.join("render").join(format!("{}.png", self.targets[t].entity))
    }

    /// The entity's image in the target set (first UUID)
    pub fn image(&self, t: usize) -> Option<PathBuf> {
        let (_, files) = self.resolved.image.get(&self.targets[t].entity)?;
        files.first().map(|f| self.store.set_dir(&self.set.name).join(f))
    }

    pub fn has_render(&self, t: usize) -> bool {
        self.resolved.image.contains_key(&self.targets[t].entity)
    }

    /// The head node's state when it is another node (None: this node is the head node)
    pub fn remote_sel(&self) -> Option<&RemoteSel> {
        self.head.as_ref().and(self.remote.as_ref())
    }

    /// For "missing": in the target set of the head node; here also in the fetched live manifest of another node
    pub fn present(&self, t: usize) -> bool {
        if let Some(r) = self.remote_sel() {
            return r.images.contains(&self.targets[t].entity);
        }
        self.has_render(t)
            || (self.set.name == crate::store::LIVE && self.manifest.as_ref().is_some_and(|m| m.entities.get(&self.targets[t].entity) == Some(&true)))
    }

    pub fn failed_last(&self, t: usize) -> bool {
        match self.remote_sel() {
            Some(r) => r.failed.contains(&self.targets[t].entity),
            None => self.resolved.failed.contains(&self.targets[t].entity),
        }
    }

    /// Name of the target set for the next start: on the head node
    pub fn target_set(&self) -> String {
        match self.remote_sel() {
            Some(r) => r.set.clone(),
            None => self.set.name.clone(),
        }
    }

    pub fn run_dir(&self) -> Option<PathBuf> {
        self.record.as_ref().map(|r| self.store.run_dir(&r.id))
    }

    pub fn pose_rig(&self, t: usize) -> Result<([f64; 3], PathBuf)> {
        let tg = &self.targets[t];
        Ok((self.tables.pose(&tg.kind, &tg.sub_type), self.tables.rig(&tg.kind, &tg.sub_type)?.clone()))
    }

    /// Filter: terms separated by spaces or commas, a match in class name, display name or
    /// type/subtype is enough (OR), case-insensitive. Empty = all.
    pub fn select(&self, filter: &str, scope: Scope) -> Selection {
        let terms: Vec<String> = filter.split([' ', ',']).filter(|s| !s.is_empty()).map(str::to_lowercase).collect();
        let mut s = Selection { matched: 0, existing: 0, missing: 0, failed: 0, todo: Vec::new(), overwrite: 0 };
        for (i, h) in self.haystack.iter().enumerate().take(self.own_len) {
            if !terms.is_empty() && !terms.iter().any(|t| h.contains(t.as_str())) {
                continue;
            }
            s.matched += 1;
            let has = self.present(i);
            let failed = self.failed_last(i);
            if has { s.existing += 1 } else { s.missing += 1 }
            if failed {
                s.failed += 1;
            }
            let take = match scope {
                Scope::Missing => !has,
                Scope::Failed => failed,
                Scope::All => true,
            };
            if take {
                s.todo.push(i);
                if has {
                    s.overwrite += 1;
                }
            }
        }
        s
    }

    pub fn note(&mut self, level: Level, text: impl Into<String>) {
        let text = text.into();
        if let Some(f) = self.log_file.as_mut() {
            let _ = writeln!(f, "{text}");
        }
        if let Some(e) = self.echo.as_mut() {
            e.push(text.clone());
        }
        self.log.push_back(LogLine { time: now_local(), level, text });
        while self.log.len() > 500 {
            self.log.pop_front();
        }
    }

    /// Starts a run over `todo` as a new run (`name`, empty = from filter and scope) in the target set; `nodes`: the
    /// connected nodes that take part, `starter`: the node that asked for it (cluster.rs)
    #[allow(clippy::too_many_arguments)]
    pub fn start(&mut self, todo: &[usize], args_line: &str, filter: &str, scope: Scope, name: &str, nodes: Vec<String>, starter: &str) -> Result<()> {
        if let Some(s) = self.serve.as_mut() {
            s.blender_log(&format!("=== {}", chrono::Local::now().to_rfc3339()));
        }
        self.errors.clear();

        let n = todo.len();
        self.run = Some(Run {
            todo: todo.iter().copied().collect(),
            pool: VecDeque::new(),
            held: HashMap::new(),
            xchunks: HashMap::new(),
            requeues: HashMap::new(),
            uncounted: HashSet::new(),
            unreachable: HashMap::new(),
            nodes,
            starter: starter.to_string(),
            filter: filter.to_string(),
            scope,
            status_sent: false,
            total: n,
            ok: 0,
            failed: 0,
            exported: 0,
            started: Instant::now(),
            finished: None,
            aborted: false,
            done_at: Vec::new(),
            rendered_by: HashMap::new(),
            phase_sum: [0.0; 5],
            phase_n: 0,
            logged_end: false,
        });
        // The run: folder with start time and name, run.json with the origin, at the end of the chain in the target set
        let name = if name.trim().is_empty() { self.default_run_name(filter, scope) } else { name.trim().to_string() };
        let run_id = self.store.new_run_id(&name);
        fs::create_dir_all(self.store.run_dir(&run_id))?;
        // the run's log: the console with full error messages and the failures' traces
        let mut log_file = OpenOptions::new().create(true).append(true).open(self.store.run_dir(&run_id).join("run.log"))?;
        writeln!(log_file, "=== {} packshot {}", chrono::Local::now().to_rfc3339(), args_line)?;
        self.log_file = Some(log_file);
        let slots = self.total_slots();
        let meta = self.run_meta(&run_id, args_line, filter, scope, n, n.div_ceil(self.export_size().max(1)), self.export_size());
        let record = RunInfo {
            id: run_id.clone(),
            name,
            set: self.set.name.clone(),
            node: self.me.name.clone(),
            started: chrono::Local::now().to_rfc3339(),
            origin: meta.clone(),
            ..Default::default()
        };
        self.store.save_run(&record)?;
        self.record = Some(record);
        self.set.runs.push(run_id.clone());
        self.store.save_set(&self.set)?;
        let nodes = self.run.as_ref().map(|r| r.nodes.join(", ")).unwrap_or_default();
        let by = if starter == self.me.name { String::new() } else { format!(" (started by {starter})") };
        self.note(
            Level::Info,
            format!(
                "Run {run_id} on set {}{by}: {n} entities, {} export · {} render here{}, {slots} render in all",
                self.set.name,
                self.max_exporters,
                self.max_renderers,
                if nodes.is_empty() { String::new() } else { format!(" + {nodes}") }
            ),
        );
        match EventLog::create(&self.store.run_dir(&run_id).join("events.jsonl")) {
            Ok(mut log) => {
                log.emit("run_start", meta);
                self.events = Some(log);
            }
            Err(e) => self.note(Level::Error, format!("Event log not created: {e}")),
        }
        self.gpu = GpuProbe::start();
        self.last_sample = Instant::now();
        self.paused = false;
        Ok(())
    }

    /// Blenders in the cluster: the own plus the slots of the participating nodes with a matching version
    pub fn total_slots(&self) -> usize {
        self.max_renderers + self.usable_nodes().iter().map(|(_, h)| if h.render { h.slots } else { 0 }).sum::<usize>()
    }

    /// export-batch processes in the cluster: the own plus those of the participating nodes
    pub fn export_slots(&self) -> usize {
        (if self.me.export { self.exporters_now() } else { 0 }) + self.usable_nodes().iter().map(|(_, h)| if h.export { h.exporters } else { 0 }).sum::<usize>()
    }

    /// Next export chunk: the items to export / (3 × export processes in the cluster), see `guided`
    pub fn export_size(&self) -> usize {
        guided(self.run.as_ref().map(|r| r.todo.len()).unwrap_or(0), self.export_slots(), self.cfg.chunk)
    }

    /// Next render chunk: the items not in a render chunk yet / (3 × Blenders in the cluster), see `guided`
    /// The pool's backpressure: exporters take no new chunk while exported-unrendered plus exporting items reach it
    pub fn pool_limit(&self) -> usize {
        BACKLOG * self.total_slots() * self.render_size()
    }

    pub fn render_size(&self) -> usize {
        guided(self.render_rem(), self.total_slots(), self.cfg.chunk)
    }

    fn render_rem(&self) -> usize {
        let exporting: usize = self.workers.iter().filter(|w| w.kind == Kind::Export).map(|w| w.order.len() - w.reported.len() + w.exported.len()).sum();
        self.run.as_ref().map(|r| r.todo.len() + r.pool.len()).unwrap_or(0) + exporting
    }

    /// The nodes taking part with their processes of a kind and their assessed rate per process (LOCAL: this one)
    fn node_rates(&self, export: bool) -> Vec<(u64, usize, Option<f64>)> {
        let mut out = Vec::new();
        if !export {
            out.push((LOCAL, self.max_renderers, proc_rate(&self.me, false)));
        } else if self.me.export {
            out.push((LOCAL, self.exporters_now(), proc_rate(&self.me, true)));
        }
        for (c, h) in self.usable_nodes() {
            match export {
                false if h.render => out.push((c, h.slots, proc_rate(&h, false))),
                true if h.export => out.push((c, h.exporters, proc_rate(&h, true))),
                _ => {}
            }
        }
        out
    }

    /// Next render chunk for the node on `conn`: proportional to its assessed rate (smaller on slow nodes, so the
    /// tail does not wait for them), the plain size without rates
    pub fn render_size_for(&self, conn: u64) -> usize {
        sized(self.render_size(), rate_share(&self.node_rates(false), conn), self.cfg.chunk, self.render_rem())
    }

    /// Next export chunk for the node on `conn`, like render_size_for
    pub fn export_size_for(&self, conn: u64) -> usize {
        let rem = self.run.as_ref().map(|r| r.todo.len()).unwrap_or(0);
        sized(self.export_size(), rate_share(&self.node_rates(true), conn), self.cfg.chunk, rem)
    }

    /// Make another set the target set (Sets view, Enter); its folder is checked against its chain
    pub fn switch_set(&mut self, name: &str) -> Result<()> {
        let mut set = self.store.load_set(name)?;
        self.store.sync(&mut set)?;
        self.resolved = self.store.resolve(&set)?;
        // the selection runs over the set's target list while this node is the head node
        if self.head.is_none() {
            let targets = self.store.load_targets(&set)?;
            self.set = set;
            self.use_targets(targets);
        } else {
            self.set = set;
        }
        Ok(())
    }

    /// Re-read the target set after store operations (adopt, delete); falls back to live if it is gone
    pub fn reload_set(&mut self) -> Result<()> {
        let name = if self.store.sets()?.contains(&self.set.name) { self.set.name.clone() } else { crate::store::LIVE.to_string() };
        self.switch_set(&name)
    }

    pub fn running(&self) -> bool {
        self.run.as_ref().is_some_and(|r| r.finished.is_none())
    }

    /// Aborts: end the chunks' processes (here and on the nodes), save state; what is missing stays for the next run
    pub fn abort(&mut self) {
        for (conn, id) in self.workers.iter().map(|w| (w.conn, w.chunk)).collect::<Vec<_>>() {
            self.dispatch(conn, Msg::Cancel { id }, Payload::None);
        }
        self.workers.clear();
        self.end_local();
        if let Some(r) = self.run.as_mut() && r.finished.is_none() {
            r.finished = Some(Instant::now());
            r.aborted = true;
        }
        if self.run.as_ref().is_some_and(|r| r.aborted) {
            self.note(Level::Info, "Run aborted");
            self.emit_run_end();
            self.close_record(true);
        }
    }

    /// Save run.json and set.json (after every block and at the end)
    pub(crate) fn persist(&mut self) {
        let res = match self.record.as_ref() {
            Some(rec) => self.store.save_run(rec).and_then(|_| self.store.save_set(&self.set)),
            None => Ok(()),
        };
        if let Err(e) = res {
            self.note(Level::Error, format!("Run/set not saved: {e:#}"));
        }
    }

    /// Finish the run: end in run.json, immutable afterwards. Without a single result it gets thrown out again
    fn close_record(&mut self, aborted: bool) {
        let Some(rec) = self.record.as_mut() else { return };
        if rec.finished.is_some() {
            return;
        }
        rec.finished = Some(chrono::Local::now().to_rfc3339());
        rec.aborted = aborted;
        if rec.entities.is_empty() {
            let id = rec.id.clone();
            self.record = None;
            self.events = None;
            self.set.runs.retain(|r| *r != id);
            let res = self.store.save_set(&self.set).and_then(|_| self.store.delete_run(&id));
            match res {
                Ok(_) => self.note(Level::Info, format!("Run {id} removed again: no results")),
                Err(e) => self.note(Level::Error, format!("Run {id} not removed: {e:#}")),
            }
            return;
        }
        self.persist();
        self.stats = self.store.last_stats();
    }

    /// Register a worker; the slot number counts per machine (local or per node)
    pub(crate) fn push_worker(&mut self, kind: Kind, chunk: u64, node: Option<String>, conn: u64, order: Vec<usize>) {
        let used: HashSet<usize> = self.workers.iter().filter(|w| w.kind == kind && w.node == node).map(|w| w.slot).collect();
        let slot = (1..).find(|s| !used.contains(s)).unwrap_or(1);
        let now = Instant::now();
        let host = node.clone().unwrap_or_else(|| self.me.name.clone());
        self.workers.push(Worker { kind, slot, chunk, host, node, conn, order, reported: HashSet::new(), exported: Vec::new(), fetched: false, since: now, started: now, item_secs: 0.0 });
    }

    /// A message to the node doing a chunk: this node's own Renderer (LOCAL) or another node
    pub(crate) fn dispatch(&mut self, conn: u64, msg: Msg, payload: Payload) {
        if conn == LOCAL {
            let mut lines = Vec::new();
            if let Some(s) = self.serve.as_mut() {
                s.on_msg(self.hub.as_ref(), LOCAL, msg, None, &mut |l| lines.push(l));
            }
            for l in lines {
                self.note(Level::Info, l);
            }
        } else if let Some(h) = self.hub.as_ref() {
            h.send(conn, msg, payload);
        }
    }

    /// Pose and rig from this node's DataCore tables (`rig` then is the path of its rig file); the items without
    /// a rig with why
    pub(crate) fn fill_items(&self, jobs: Vec<ItemJob>) -> (Vec<ItemJob>, Vec<(usize, String)>) {
        let mut out = Vec::new();
        let mut bad = Vec::new();
        for mut j in jobs {
            match self.tables.rig(&j.kind, &j.sub_type) {
                Ok(rig) => {
                    j.rig = rig.to_string_lossy().to_string();
                    j.pose = self.tables.pose(&j.kind, &j.sub_type);
                    out.push(j);
                }
                Err(e) => bad.push((j.item, format!("rig: {e}"))),
            }
        }
        (out, bad)
    }

    /// An export chunk to a node (LOCAL: here); it exports with its own Data.p4k and tables and holds the result
    fn start_export_chunk(&mut self, conn: u64, node: Option<String>, items: Vec<usize>) {
        let id = self.next_remote;
        self.next_remote += 1;
        let jobs: Vec<ItemJob> = items
            .iter()
            .map(|&t| {
                let tg = &self.targets[t];
                ItemJob { item: t, entity: tg.entity.clone(), worn: tg.worn, kind: tg.kind.clone(), sub_type: tg.sub_type.clone(), pose: [0.0; 3], rig: String::new() }
            })
            .collect();
        let n = items.len();
        self.push_worker(Kind::Export, id, node, conn, items);
        let worker = self.workers.last().map(Worker::label).unwrap_or_default();
        self.emit("export_start", json!({ "block": id, "worker": worker, "items": n }));
        if conn != LOCAL {
            self.dispatch(conn, Msg::Export { id, jobs }, Payload::None);
            return;
        }
        let (jobs, bad) = self.fill_items(jobs);
        for (item, error) in bad {
            self.on_item_exported(LOCAL, id, item, false, 0.0, error, None);
        }
        if jobs.is_empty() {
            self.on_export_done(LOCAL, id, None, 0.0, 0.0, String::new(), 0, Vec::new());
        } else {
            self.dispatch(LOCAL, Msg::Export { id, jobs }, Payload::None);
        }
    }

    /// A render chunk to a rendering node (LOCAL: here): items of the pool with their holders; the node fetches what
    /// it does not hold itself
    fn start_render_chunk(&mut self, conn: u64, node: Option<String>, items: Vec<usize>, waits: Vec<f64>) {
        // placed: a later requeue counts again
        if let Some(run) = self.run.as_mut() {
            for t in &items {
                run.uncounted.remove(t);
            }
        }
        let id = self.next_remote;
        self.next_remote += 1;
        let renderer = node.clone().unwrap_or_else(|| self.me.name.clone());
        let Some(run) = self.run.as_ref() else { return };
        let jobs: Vec<RenderJob> = items
            .iter()
            .map(|&t| {
                let chunk = run.held.get(&t).copied().unwrap_or_default();
                let holder = run.xchunks.get(&chunk).map(|x| x.node.clone()).unwrap_or_default();
                RenderJob { item: t, entity: self.targets[t].entity.clone(), holder, chunk }
            })
            .collect();
        let mut holders: Vec<String> = jobs.iter().map(|j| j.holder.clone()).collect();
        holders.sort();
        holders.dedup();
        let n = items.len();
        self.push_worker(Kind::Render, id, node, conn, items.clone());
        if let Some(w) = self.workers.last_mut() {
            w.fetched = holders.iter().all(|h| *h == renderer);
        }
        let worker = self.workers.last().map(Worker::label).unwrap_or_default();
        let wait = if waits.is_empty() { 0.0 } else { waits.iter().sum::<f64>() / waits.len() as f64 };
        self.emit("render_start", json!({ "block": id, "worker": worker, "items": n, "queue_wait": round3(wait), "holders": holders }));
        // the other nodes render with the head node's render.py
        let (files, payload) = if conn == LOCAL {
            (Vec::new(), Payload::None)
        } else {
            match fs::metadata(&self.cfg.render_script) {
                Ok(m) => (vec![crate::net::FileEntry { path: "render.py".into(), size: m.len() }], Payload::Files(vec![self.cfg.render_script.clone()])),
                Err(e) => {
                    self.workers.pop();
                    for t in items {
                        self.record(t, false, Some(format!("render.py: {e}")), 0.0, "render", "");
                    }
                    return;
                }
            }
        };
        self.dispatch(conn, Msg::Render { id, size: self.cfg.size, jobs, files }, payload);
    }

    /// Result of an item (export failure or render result); a failure's first line is the message, further lines the
    /// output of the process before it (traceback, last lines), which goes into run.log and events.jsonl
    pub(crate) fn record(&mut self, t: usize, ok: bool, error: Option<String>, seconds: f64, what: &str, detail: &str) {
        let entity = self.targets[t].entity.clone();
        let (error, trace) = match error.as_deref().map(str::trim).and_then(|e| e.split_once('\n')) {
            Some((msg, trace)) => (Some(msg.trim().to_string()), trace.trim().to_string()),
            None => (error.map(|e| e.trim().to_string()), String::new()),
        };
        // A success is already in the run (place_icons), a failure goes in here
        if !ok && let Some(rec) = self.record.as_mut() {
            let r = EntityResult { ok: false, error: error.clone(), files: Vec::new() };
            self.resolved.apply(&rec.id, &entity, &r);
            rec.entities.insert(entity.clone(), r);
        }
        let Some(run) = self.run.as_mut() else { return };
        let i = run.done() + 1;
        let total = run.total;
        let w = total.to_string().len();
        if ok {
            run.ok += 1;
            run.done_at.push(Instant::now());
            self.history.push_back(t);
            while self.history.len() > 50 {
                self.history.pop_front();
            }
            self.note(Level::Ok, format!("[{i:>w$}/{total}] {what:<6} {entity}  {seconds:.1} s{detail}"));
        } else {
            run.failed += 1;
            run.done_at.push(Instant::now());
            let e = error.unwrap_or_default();
            self.note(Level::Error, format!("[{i:>w$}/{total}] {what:<6} {entity}  FAILED {e}{detail}"));
            // the trace only into the files, the views stay one line per item
            if let Some(f) = self.log_file.as_mut() {
                for l in trace.lines() {
                    let _ = writeln!(f, "    | {l}");
                }
            }
            let trace = if trace.is_empty() { serde_json::Value::Null } else { json!(trace) };
            self.emit("failure", json!({ "entity": entity, "stage": what, "error": e, "trace": trace }));
            self.errors.push((t, e));
        }
        self.drop_held(t);
    }

    /// The finished image into the run (one file per UUID) and as hardlinks into the target set; the entity's old ones go
    fn place_icons(&mut self, t: usize) -> Result<()> {
        let tg = &self.targets[t];
        let files: Vec<String> = (0..tg.uuids.len()).map(|k| icon_file(tg, k)).collect();
        let entity = tg.entity.clone();
        let run_id = self.record.as_ref().map(|r| r.id.clone()).context("no run")?;
        self.store.put_image(&run_id, &self.render_png(t), &files)?;
        let old = self.resolved.image.get(&entity).map(|(_, f)| f.clone()).unwrap_or_default();
        self.store.link_entity(&mut self.set, &old, &run_id, &files)?;
        let r = EntityResult { ok: true, error: None, files };
        self.resolved.apply(&run_id, &entity, &r);
        if let Some(rec) = self.record.as_mut() {
            rec.entities.insert(entity, r);
        }
        Ok(())
    }

    /// An item leaves its holder's account (done, failed, or exported again): the holder releases its export chunk
    /// once none of its items is open
    fn drop_held(&mut self, t: usize) {
        let Some(run) = self.run.as_mut() else { return };
        let Some(x) = run.held.remove(&t) else { return };
        let Some(c) = run.xchunks.get_mut(&x) else { return };
        c.open = c.open.saturating_sub(1);
        if c.open == 0 {
            let conn = c.conn;
            run.xchunks.remove(&x);
            self.dispatch(conn, Msg::Release { chunks: vec![x] }, Payload::None);
        }
    }

    /// An item goes back: to be exported again (its holder is gone) or into the pool (its holder still has it), in
    /// front. A `counted` requeue counts against the item (after MAX_REQUEUES it fails with the reason); a `culprit`
    /// (the item its chunk was stuck on) also goes to the back, so the items behind it get their turn first
    pub(crate) fn requeue(&mut self, t: usize, export_again: bool, reason: &str, counted: bool, culprit: bool) {
        let Some(run) = self.run.as_mut() else { return };
        if run.finished.is_some() {
            return;
        }
        // one incident counts once: an item an Unfetched put back, not placed since, is not counted again
        let counted = counted && !run.uncounted.remove(&t);
        if export_again {
            run.unreachable.remove(&t);
        }
        let n = run.requeues.entry(t).or_default();
        if counted {
            *n += 1;
        }
        let n = *n;
        if n > MAX_REQUEUES {
            self.record(t, false, Some(format!("gave up after {MAX_REQUEUES} requeues: {reason}")), 0.0, if export_again { "export" } else { "render" }, "");
            return;
        }
        let entity = self.targets[t].entity.clone();
        self.emit("requeue", json!({ "entity": entity, "reason": reason, "count": n, "counted": counted, "to": if export_again { "export" } else { "render" } }));
        if export_again {
            self.drop_held(t);
        }
        let Some(run) = self.run.as_mut() else { return };
        match (export_again, culprit) {
            (true, false) => run.todo.push_front(t),
            (true, true) => run.todo.push_back(t),
            (false, false) => run.pool.push_front((t, Instant::now())),
            (false, true) => run.pool.push_back((t, Instant::now())),
        }
    }

    /// Open items of a chunk that is given up back: an export's to be exported again (those exported already too: its
    /// node keeps nothing of it), a render's into the pool while their holder is there, else to be exported again.
    /// `stuck`: it timed out, only the item it was working on counts (the culprit), the others go back as they were;
    /// else (its node dropped) every one counts
    pub(crate) fn requeue_open(&mut self, w: &Worker, reason: &str, stuck: bool) {
        let open: Vec<usize> = w.order.iter().copied().filter(|t| !w.reported.contains(t) || w.exported.contains(t)).collect();
        let current = w.current();
        // every item reported and still no end (an export that never sends ExportDone): no culprit, every one counts,
        // so it ends
        let stuck = stuck && current.is_some();
        for t in open.into_iter().rev() {
            let held = self.run.as_ref().is_some_and(|r| r.held.get(&t).is_some_and(|x| r.xchunks.contains_key(x)));
            let culprit = stuck && Some(t) == current;
            self.requeue(t, w.kind == Kind::Export || !held, reason, !stuck || culprit, culprit);
        }
    }

    /// An item of an export chunk exported (or failed) on its node
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn on_item_exported(&mut self, conn: u64, chunk: u64, t: usize, ok: bool, seconds: f64, error: String, worn: Option<bool>) {
        let Some(idx) = self.workers.iter().position(|w| w.kind == Kind::Export && w.conn == conn && w.chunk == chunk) else { return };
        let w = &mut self.workers[idx];
        if !w.order.contains(&t) || !w.reported.insert(t) {
            return;
        }
        w.since = Instant::now();
        w.item_secs += seconds;
        let worker = w.label();
        self.emit("export_item", json!({
            "entity": self.targets[t].entity, "block": chunk, "worker": worker, "seconds": round3(seconds), "ok": ok, "worn": worn,
            "error": if ok { serde_json::Value::Null } else { json!(error) },
        }));
        if ok {
            self.workers[idx].exported.push(t);
            if let Some(run) = self.run.as_mut() {
                run.exported += 1;
            }
        } else {
            self.record(t, false, Some(if error.is_empty() { "export failed".into() } else { error }), seconds, "export", "");
        }
    }

    /// An export chunk's process ended on its node: its exported items go into the pool, held there; items without a
    /// result failed
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn on_export_done(&mut self, conn: u64, id: u64, exit: Option<i32>, wall: f64, item_seconds: f64, error: String, long_paths: usize, failed: Vec<(usize, String)>) {
        let Some(idx) = self.workers.iter().position(|w| w.kind == Kind::Export && w.conn == conn && w.chunk == id) else {
            // an export chunk given up already (timed out, its run over): its node must not keep holding it
            self.dispatch(conn, Msg::Release { chunks: vec![id] }, Payload::None);
            return;
        };
        let w = self.workers.remove(idx);
        let missing: Vec<usize> = w.order.iter().copied().filter(|t| !w.reported.contains(t)).collect();
        let worker = w.label();
        let wall = if conn == LOCAL || wall > 0.0 { wall } else { w.started.elapsed().as_secs_f64() };
        self.emit("export_end", json!({
            "block": id, "worker": worker, "items": w.order.len(), "missing": missing.len(), "wall": round3(wall),
            "item_seconds": round3(item_seconds.max(w.item_secs)), "exit": exit, "long_paths": long_paths,
        }));
        if long_paths > 0 {
            self.note(Level::Error, format!("Export chunk {id} on {}: {long_paths} paths over 259 characters, put --work in a shorter folder", w.host));
        }
        let error = if error.trim().is_empty() { format!("export-batch ended early (exit {exit:?})") } else { error };
        for t in missing {
            self.record(t, false, Some(error.clone()), 0.0, "export", "");
        }
        let bad: HashSet<usize> = failed.iter().map(|(t, _)| *t).collect();
        for (t, e) in failed {
            if w.exported.contains(&t) {
                self.record(t, false, Some(e), 0.0, "export", "");
            }
        }
        let held: Vec<usize> = w.exported.iter().copied().filter(|t| !bad.contains(t)).collect();
        self.persist();
        let Some(run) = self.run.as_mut().filter(|r| r.finished.is_none()) else { return };
        if held.is_empty() {
            return;
        }
        run.xchunks.insert(id, XChunk { node: w.host.clone(), conn, open: held.len() });
        let now = Instant::now();
        for t in held {
            run.held.insert(t, id);
            run.pool.push_back((t, now));
        }
    }

    /// A render chunk's files are on its node
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn on_fetched(&mut self, conn: u64, id: u64, holders: Vec<String>, bytes: u64, cached: u64, files: usize, secs: f64) {
        let Some(w) = self.workers.iter_mut().find(|w| w.kind == Kind::Render && w.conn == conn && w.chunk == id) else { return };
        w.fetched = true;
        // the first render's time counts from here
        w.since = Instant::now();
        let worker = w.label();
        self.transferred += bytes;
        self.emit("fetch", json!({ "block": id, "worker": worker, "holders": holders, "bytes": bytes, "cached": cached, "files": files, "secs": round3(secs) }));
    }

    /// Items of a render chunk that did not get their files on its node: out of the chunk and back, to be exported
    /// again when `export` (their holder lost a file) or their holder is gone here, else into the pool for another
    /// renderer (their holder is still there but not reachable from that node), or exported again when every
    /// renderer has reported it cannot reach that holder; every one counts
    pub(crate) fn on_unfetched(&mut self, conn: u64, block: u64, items: Vec<usize>, reason: String, export: bool) {
        let Some(w) = self.workers.iter_mut().find(|w| w.kind == Kind::Render && w.conn == conn && w.chunk == block) else { return };
        let host = w.host.clone();
        let items: Vec<usize> = items.into_iter().filter(|t| w.order.contains(t) && !w.reported.contains(t)).collect();
        w.order.retain(|t| !items.contains(t));
        let renderers = self.renderer_hosts();
        for t in items.into_iter().rev() {
            let Some(run) = self.run.as_mut() else { return };
            let held = run.held.get(&t).is_some_and(|x| run.xchunks.contains_key(x));
            let mut again = export || !held;
            if !again {
                let tried = run.unreachable.entry(t).or_default();
                tried.insert(host.clone());
                again = renderers.iter().all(|r| tried.contains(r));
            }
            self.requeue(t, again, &reason, true, false);
            if let Some(run) = self.run.as_mut().filter(|r| !again && r.held.contains_key(&t)) {
                run.uncounted.insert(t);
            }
        }
    }

    /// Host names of the nodes that render for this run, this one included when it renders
    fn renderer_hosts(&self) -> Vec<String> {
        let mut out: Vec<String> = self.usable_nodes().into_iter().filter(|(_, h)| h.render && h.slots > 0).map(|(_, h)| h.name).collect();
        if self.max_renderers > 0 {
            out.push(self.me.name.clone());
        }
        out
    }

    /// A render result from a node (LOCAL: this one): the PNG comes as the payload
    pub(crate) fn on_item(&mut self, conn: u64, block: u64, item: usize, ok: bool, info: String, data: &[u8]) {
        let Some(idx) = self.workers.iter().position(|w| w.kind == Kind::Render && w.conn == conn && w.chunk == block) else { return };
        if !self.workers[idx].order.contains(&item) {
            return;
        }
        let (ok, info) = if ok {
            match fs::write(self.render_png(item), data) {
                Ok(()) => (true, info),
                Err(e) => (false, format!("image from {} not written: {e}", self.workers[idx].host)),
            }
        } else {
            (false, info)
        };
        self.render_result(idx, item, ok, info);
    }

    /// A render chunk's Blender ended: items without a result failed (why and its last lines from the node)
    pub(crate) fn on_block_done(&mut self, conn: u64, id: u64, exit: Option<i32>, error: String) {
        let Some(idx) = self.workers.iter().position(|w| w.kind == Kind::Render && w.conn == conn && w.chunk == id) else { return };
        let w = self.workers.remove(idx);
        let missing: Vec<usize> = w.order.iter().copied().filter(|t| !w.reported.contains(t)).collect();
        let worker = w.label();
        self.emit("render_end", json!({ "block": id, "worker": worker, "items": w.order.len(), "missing": missing.len(), "wall": round3(w.started.elapsed().as_secs_f64()), "exit": exit }));
        for t in missing {
            let error = match &w.node {
                Some(n) if !error.trim().is_empty() => format!("{n}: {}", error.trim()),
                Some(n) => format!("no image from {n} (Blender aborted?)"),
                None if !error.trim().is_empty() => error.clone(),
                None => format!("no image (Blender aborted, exit {exit:?}?)"),
            };
            self.record(t, false, Some(error), 0.0, "render", "");
        }
        self.persist();
    }

    /// Result of a render (the PNG is already at render_png)
    pub(crate) fn render_result(&mut self, idx: usize, t: usize, ok: bool, info: String) {
        let w = &mut self.workers[idx];
        if !w.reported.insert(t) {
            return;
        }
        let seconds = w.since.elapsed().as_secs_f64();
        w.since = Instant::now();
        let (block, worker, host) = (w.chunk, w.label(), w.host.clone());
        let timing = parse_timing(&info);
        let phases: serde_json::Map<String, serde_json::Value> = timing.map(|ph| PHASES.iter().zip(ph).map(|(k, v)| (k.to_string(), json!(v))).collect()).unwrap_or_default();
        self.emit("render_item", json!({
            "entity": self.targets[t].entity, "block": block, "worker": worker, "wall": round3(seconds), "ok": ok,
            "phases": if timing.is_some() { serde_json::Value::Object(phases) } else { serde_json::Value::Null },
            "error": if ok { serde_json::Value::Null } else { json!(info.lines().next().unwrap_or("")) },
        }));
        if ok {
            if let Err(e) = self.place_icons(t) {
                self.record(t, false, Some(format!("image not stored: {e:#}")), seconds, "render", &format!("  {worker}"));
                return;
            }
            let mut detail = format!("  {worker}");
            if let Some(ph) = timing {
                if let Some(run) = self.run.as_mut() {
                    run.phase_n += 1;
                    for (s, v) in run.phase_sum.iter_mut().zip(ph) {
                        *s += v;
                    }
                }
                let parts: Vec<String> = PHASES.iter().zip(ph).map(|(k, v)| format!("{k} {v:.2}")).collect();
                detail = format!("  {worker}  ({})", parts.join(" · "));
            }
            if let Some(run) = self.run.as_mut() {
                run.rendered_by.entry(host).or_default().push(Instant::now());
            }
            self.record(t, true, None, seconds, "render", &detail);
        } else {
            self.record(t, false, Some(if info.is_empty() { "render failed".into() } else { info }), seconds, "render", &format!("  {worker}"));
        }
    }

    /// This node's work: its Renderer's processes (for its own runs and other nodes'), its messages to this node's
    /// engine
    fn work(&mut self) {
        let export_slots = self.exporters_now();
        let mut lines = Vec::new();
        let mut local = Vec::new();
        if let Some(s) = self.serve.as_mut() {
            s.slots = self.max_renderers;
            s.export_slots = export_slots;
            s.tick(self.hub.as_ref(), Duration::ZERO, &mut |l| lines.push(l));
            local = std::mem::take(&mut s.local);
        }
        for l in lines {
            self.note(Level::Info, l);
        }
        for (msg, data) in local {
            self.on_msg(LOCAL, msg, data, None);
        }
    }

    /// This node's holdings and cache of its own run go (run over or aborted)
    fn end_local(&mut self) {
        let me = self.me.name.clone();
        if let Some(s) = self.serve.as_mut() {
            s.end_run(LOCAL, &me);
        }
    }

    /// One tick: the cluster's messages, this node's processes, scheduling, samples
    pub fn tick(&mut self) {
        self.poll_tables();
        self.poll_net();
        self.poll_assess();
        self.work();
        self.schedule();
        self.sample();
    }

    /// Chunks without progress for `chunk_timeout`: cancelled on their node, their open items back
    /// (a render chunk still fetching its files gets FETCH_TIMEOUTS of them: the transfer is no stall)
    fn check_timeouts(&mut self) {
        let timeout = self.chunk_timeout;
        let fetching = |w: &Worker| w.kind == Kind::Render && !w.fetched;
        while let Some(idx) = self.workers.iter().position(|w| w.since.elapsed() > if fetching(w) { timeout * FETCH_TIMEOUTS } else { timeout }) {
            let w = self.workers.remove(idx);
            let reason = if fetching(&w) {
                format!("files not fetched in {:.0} s on {}", (timeout * FETCH_TIMEOUTS).as_secs_f64(), w.label())
            } else {
                format!("no progress for {:.0} s on {}", timeout.as_secs_f64(), w.label())
            };
            self.note(Level::Error, format!("{} chunk {} cancelled: {reason}", if w.kind == Kind::Export { "Export" } else { "Render" }, w.chunk));
            self.dispatch(w.conn, Msg::Cancel { id: w.chunk }, Payload::None);
            // a fetch that does not end is not an item's fault: every one counts
            self.requeue_open(&w, &reason, !fetching(&w));
        }
    }

    /// The nodes with a free export process: (connection, node; None = this node, free count)
    fn free_exporters(&self) -> Vec<(u64, Option<String>, usize)> {
        let busy = |conn: u64| self.workers.iter().filter(|w| w.kind == Kind::Export && w.conn == conn).count();
        let mut out = Vec::new();
        if self.me.export {
            out.push((LOCAL, None, self.exporters_now().saturating_sub(busy(LOCAL))));
        }
        for (c, h) in self.usable_nodes() {
            if h.export {
                out.push((c, Some(h.name.clone()), h.exporters.saturating_sub(busy(c))));
            }
        }
        out.retain(|(_, _, free)| *free > 0);
        out
    }

    /// The nodes with a free Blender: (connection, node; None = this node, host name, free count)
    fn free_renderers(&self) -> Vec<(u64, Option<String>, String, usize)> {
        let busy = |conn: u64| self.workers.iter().filter(|w| w.kind == Kind::Render && w.conn == conn).count();
        let mut out = vec![(LOCAL, None, self.me.name.clone(), self.max_renderers.saturating_sub(busy(LOCAL)))];
        for (c, h) in self.usable_nodes() {
            if h.render {
                out.push((c, Some(h.name.clone()), h.name.clone(), h.slots.saturating_sub(busy(c))));
            }
        }
        out.retain(|(.., free)| *free > 0);
        out
    }

    /// The next render chunk: a free Blender takes the items its own node holds when there are enough, else those of
    /// the holder with the most waiting (its own on a tie); with fewer than a chunk waiting there it waits while
    /// exports are going, unless enough wait in all
    fn pick_render(&self) -> Option<(u64, Option<String>, String, usize)> {
        let run = self.run.as_ref()?;
        if run.pool.is_empty() {
            return None;
        }
        let size_of = |conn: u64| self.render_size_for(conn);
        // per free Blender: what waits for it by holder (without the items it could not reach)
        let mut free: Vec<_> = self.free_renderers().into_iter().map(|f| {
            let count = pool_by_holder(run, &f.2);
            (f, count)
        }).collect();
        // the nodes holding enough for a chunk first, then the fastest by assessed rate per process, then the most
        // free Blenders
        let rates = self.node_rates(false);
        free.sort_by(|((ca, _, ha, na), cnt_a), ((cb, _, hb, nb), cnt_b)| {
            let rank = |c: u64, h: &String, n: usize, cnt: &HashMap<&str, usize>| (cnt.get(h.as_str()).copied().unwrap_or(0) < size_of(c), rate_share(&rates, c), n);
            renderer_order(rank(*ca, ha, *na, cnt_a), rank(*cb, hb, *nb, cnt_b))
        });
        let exporting = self.workers.iter().any(|w| w.kind == Kind::Export);
        for ((conn, node, host, _), count) in free {
            let own = |name: &str| count.get(name).copied().unwrap_or(0);
            let size = size_of(conn);
            let holder = if own(&host) >= size {
                host.clone()
            } else {
                let Some(best) = count.iter().map(|(h, n)| (*n, *h == host.as_str(), *h)).max() else { continue };
                best.2.to_string()
            };
            let avail = own(&holder);
            if avail >= size || !exporting || count.values().sum::<usize>() >= size {
                return Some((conn, node, holder, size.min(avail)));
            }
        }
        None
    }

    fn schedule(&mut self) {
        let Some(run) = self.run.as_ref() else { return };
        if run.finished.is_some() {
            return;
        }
        self.check_timeouts();
        if !self.paused {
            // export chunks while the exported items not rendered yet stay below the backlog (disk on the holders)
            loop {
                let run = self.run.as_ref().unwrap();
                if run.todo.is_empty() {
                    break;
                }
                let exporting: usize = self.workers.iter().filter(|w| w.kind == Kind::Export).map(|w| w.order.len() - w.reported.len() + w.exported.len()).sum();
                if run.held.len() + exporting >= self.pool_limit() {
                    break;
                }
                // the fastest node by its assessed rate per process first, then the most free processes
                let Some((conn, node, _)) = pick_exporter(self.free_exporters(), &self.node_rates(true)) else { break };
                // cut the chunk only now: smaller towards the end of the run, by the node's rate
                let size = self.export_size_for(conn);
                let run = self.run.as_mut().unwrap();
                let items: Vec<usize> = (0..size).filter_map(|_| run.todo.pop_front()).collect();
                self.start_export_chunk(conn, node, items);
            }
            // an exported item whose holder is no longer known cannot be fetched: exported again
            let orphans: Vec<usize> = {
                let run = self.run.as_mut().unwrap();
                let (out, keep): (Pool, Pool) = run.pool.drain(..).partition(|(t, _)| !run.held.get(t).is_some_and(|x| run.xchunks.contains_key(x)));
                run.pool = keep;
                out.into_iter().map(|(t, _)| t).collect()
            };
            for t in orphans.into_iter().rev() {
                self.requeue(t, true, "its holder is gone", true, false);
            }
            // render chunks on free Blenders
            while let Some((conn, node, holder, n)) = self.pick_render() {
                let renderer = node.clone().unwrap_or_else(|| self.me.name.clone());
                let run = self.run.as_mut().unwrap();
                let mut items = Vec::new();
                let mut waits = Vec::new();
                let mut rest = VecDeque::new();
                while let Some((t, at)) = run.pool.pop_front() {
                    let here = run.held.get(&t).and_then(|x| run.xchunks.get(x)).is_some_and(|x| x.node == holder) && !run.unreachable.get(&t).is_some_and(|r| r.contains(&renderer));
                    if here && items.len() < n {
                        items.push(t);
                        waits.push(at.elapsed().as_secs_f64());
                    } else {
                        rest.push_back((t, at));
                    }
                }
                run.pool = rest;
                if items.is_empty() {
                    break;
                }
                self.start_render_chunk(conn, node, items, waits);
            }
        }
        let run = self.run.as_ref().unwrap();
        if self.workers.is_empty() && run.pool.is_empty() && run.todo.is_empty() {
            let r = self.run.as_mut().unwrap();
            r.finished = Some(Instant::now());
            let (ok, failed, mins) = (r.ok, r.failed, r.started.elapsed().as_secs_f64() / 60.0);
            let dir = self.run_dir().map(|d| d.display().to_string()).unwrap_or_default();
            self.note(Level::Info, format!("Done: {ok} rendered, {failed} failed, {mins:.1} min -> {dir}, set {}", self.set.name));
            self.emit_run_end();
            self.close_record(false);
            self.end_local();
        }
    }

    /// Renders per minute over the last `window` seconds
    /// Per node of the run going on here: (node, images, items/s over the last `window` seconds)
    pub fn run_throughput(&self, window: f64) -> Vec<(String, usize, f64)> {
        let Some(run) = self.run.as_ref() else { return Vec::new() };
        let end = run.finished.unwrap_or_else(Instant::now);
        let span = window.min(end.duration_since(run.started).as_secs_f64()).max(1.0);
        let mut out: Vec<(String, usize, f64)> = run
            .rendered_by
            .iter()
            .map(|(node, at)| (node.clone(), at.len(), at.iter().filter(|t| end.duration_since(**t).as_secs_f64() <= span).count() as f64 / span))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// default_run_name with this node's add-on and render.py (they are the same on every node of a run)
    pub fn default_run_name(&self, filter: &str, scope: Scope) -> String {
        default_run_name(filter, scope, self.me.addon.as_deref(), crate::events::file_hash(&self.cfg.render_script).as_deref())
    }

    pub fn rate(&self, window: f64) -> f64 {
        let Some(run) = self.run.as_ref() else { return 0.0 };
        let end = run.finished.unwrap_or_else(Instant::now);
        let span = window.min(end.duration_since(run.started).as_secs_f64()).max(1.0);
        let n = run.done_at.iter().filter(|t| end.duration_since(**t).as_secs_f64() <= span).count();
        n as f64 * 60.0 / span
    }
}

#[cfg(test)]
mod name_tests {
    use super::*;

    /// Without --name: filter terms, what the scope does, the add-on's and render.py's hashes
    #[test]
    fn run_names() {
        assert_eq!(default_run_name("", Scope::All, Some("6ea0d4c6b77492ba"), Some("6da3cb0c523f77d7")), "full_addon-6ea0d4_renderpy-6da3cb");
        assert_eq!(default_run_name("arms, legs", Scope::Missing, Some("6ea0d4c6"), None), "arms-legs_missing_addon-6ea0d4_renderpy-none");
        assert_eq!(default_run_name("", Scope::Failed, None, Some("abcdef12")), "retry_addon-none_renderpy-abcdef");
    }
}

#[cfg(test)]
mod rate_tests {
    use super::*;
    use crate::net::AssessInfo;

    fn node(render: f64, workers: usize, valid: bool) -> Hello {
        Hello { assess: Some(AssessInfo { render, export: 4.0, workers, exporters: 2, valid, ..Default::default() }), ..Default::default() }
    }

    /// The rate at the count a node runs now: from its steps when +/- changed it (fewer Blenders are faster each, more
    /// share the CPU), scaled like the assessed rate; without steps the assessed rate per process
    #[test]
    fn rate_at_actual_count() {
        let steps = vec![(1, 2.0), (2, 3.6), (4, 6.0), (6, 6.3)];
        assert_eq!(rate_at(&steps, 3), Some(4.8));
        assert_eq!(rate_at(&steps, 12), Some(6.3), "flat beyond the last step");
        assert_eq!(rate_at(&[(2, 4.0)], 1), Some(2.0));
        assert_eq!(rate_at(&[], 3), None);
        let mut h = node(6.3, 6, true);
        h.assess.as_mut().unwrap().steps = steps;
        h.slots = 6;
        assert!((proc_rate(&h, false).unwrap() - 1.05).abs() < 1e-9, "the assessed count: as assessed");
        h.slots = 3;
        assert!((proc_rate(&h, false).unwrap() - 1.6).abs() < 1e-9, "3 Blender: 4.8 items/s, 1.6 each");
        h.slots = 12;
        assert!((proc_rate(&h, false).unwrap() - 6.3 / 12.0).abs() < 1e-9, "12 share what 6 made");
        // with exports alongside the assessed rate is lower than the step's: the step rates scale by it
        let mut h = node(5.0, 6, true);
        h.assess.as_mut().unwrap().steps = vec![(1, 2.0), (2, 3.6), (4, 6.0), (6, 6.3)];
        h.slots = 3;
        assert!((proc_rate(&h, false).unwrap() - 5.0 * 4.8 / 6.3 / 3.0).abs() < 1e-9);
        // old assessments without steps: per process as assessed
        let mut h = node(6.3, 6, true);
        h.slots = 3;
        assert!((proc_rate(&h, false).unwrap() - 1.05).abs() < 1e-9);
    }

    /// Placement: the fastest free exporter (by assessed rate per process) gets the next export chunk, before the
    /// one with more free processes; a Blender holding a chunk's worth comes first, then the fastest
    #[test]
    fn placement_prefers_fast_nodes() {
        let rates = vec![(LOCAL, 2, Some(1.0)), (5, 2, Some(3.0)), (6, 4, None)];
        let free = vec![(LOCAL, None, 2), (5, Some("fast".into()), 1), (6, Some("unrated".into()), 4)];
        assert_eq!(pick_exporter(free, &rates).map(|f| f.0), Some(5));
        let none: Vec<(u64, usize, Option<f64>)> = vec![(LOCAL, 2, None), (5, 2, None)];
        assert_eq!(pick_exporter(vec![(LOCAL, None, 1), (5, Some("b".into()), 2)], &none).map(|f| f.0), Some(5), "no rates: the most free");
        assert_eq!(pick_exporter(vec![(LOCAL, None, 2), (5, Some("b".into()), 2)], &none).map(|f| f.0), Some(LOCAL), "a tie: this node");
        let mut order = vec![(true, 2.0, 6), (false, 0.5, 1), (false, 1.5, 1), (true, 0.5, 8)];
        order.sort_by(|a, b| renderer_order(*a, *b));
        assert_eq!(order, vec![(false, 1.5, 1), (false, 0.5, 1), (true, 2.0, 6), (true, 0.5, 8)]);
    }

    /// Chunks by the nodes' assessed rates: a slow node's chunks shrink, a fast one's grow up to --chunk; without
    /// rates (or with a stale one) every node gets the plain size
    #[test]
    fn chunk_size_by_rate() {
        // per process: DRAGONEGG 6.3/6 = 1.05, servbox 8.1/9 = 0.9, DRAGONVR 1.8/3 = 0.6 items/s
        let (egg, serv, vr) = (node(6.3, 6, true), node(8.1, 9, true), node(1.8, 3, true));
        assert!((proc_rate(&egg, false).unwrap() - 1.05).abs() < 1e-9);
        assert_eq!(proc_rate(&egg, true), Some(2.0));
        assert_eq!(proc_rate(&node(6.3, 6, false), false), None, "stale: no rate");
        assert_eq!(proc_rate(&Hello::default(), false), None);
        let nodes = vec![(0, 6, proc_rate(&egg, false)), (1, 9, proc_rate(&serv, false)), (2, 3, proc_rate(&vr, false))];
        // mean per process (6.3 + 8.1 + 1.8) / 18 = 0.9
        let share = |c| rate_share(&nodes, c);
        assert!((share(0) - 1.05 / 0.9).abs() < 1e-9 && (share(1) - 1.0).abs() < 1e-9 && (share(2) - 0.6 / 0.9).abs() < 1e-9);
        // early in a run the plain size is --chunk: the fast node stays at 50, the slow one gets 33
        assert_eq!((sized(50, share(0), 50, 5000), sized(50, share(1), 50, 5000), sized(50, share(2), 50, 5000)), (50, 50, 33));
        // towards the end (plain 10): 12, 10, 7
        assert_eq!((sized(10, share(0), 50, 200), sized(10, share(1), 50, 200), sized(10, share(2), 50, 200)), (12, 10, 7));
        assert_eq!(sized(10, share(0), 50, 3), 3, "never more than is left");
        // a node without a rate: share 1, the others against the mean of those with one
        let mixed = vec![(0, 6, proc_rate(&egg, false)), (1, 9, None)];
        assert_eq!((rate_share(&mixed, 0), rate_share(&mixed, 1)), (1.0, 1.0));
        let none: Vec<(u64, usize, Option<f64>)> = vec![(0, 6, None), (1, 9, None)];
        assert_eq!(sized(guided(1000, 15, 50), rate_share(&none, 1), 50, 1000), guided(1000, 15, 50), "no rates: as before");
    }
}
