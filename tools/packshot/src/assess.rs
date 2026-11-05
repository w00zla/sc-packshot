// Compute assessment of a node, on demand only (--assess, key b in view 3, also for another node, which then runs it
// itself). A test set of TEST_ITEMS targets, stratified by type from the target set's target list (deterministic),
// is exported once, on this node if it exports, else by a connected exporting node and fetched from it like a render
// chunk's files (node.rs), and kept under <work>/assess/testset-<p4k>-<list>/ for later assessments. After an
// unmeasured warm-up (one Blender over the whole set: file cache, Blender's own caches; items failing there leave the
// set) the render sweep runs n Blender processes at once, n = 1, 2, 3, 4, 6, 8, …, long-lived and fed one job at a
// time as in a run, each warmed by one unmeasured item (start-up and first shader compilations, paid once per
// process in a run) and then counted over a window of at least MEASURED_PER_BLENDER images per process, fed the set
// round (rotated, so they do not render the same item at once), until the gain
// over the previous step is under PLATEAU or the next step would not fit RAM or VRAM; a sampler records CPU (total
// and the busiest core), GPU and VRAM (nvidia-smi) and RAM every 0.5 s. Workers = the smallest n within PLATEAU of
// the best, capped by memory. An exporting node then runs export-batch with e = 1 … EXPORT_MAX while the chosen n
// Blender render alongside (export-batch --stdin as in a run when StarBreaker has it) and takes the e where the
// smaller of export and render rate stops improving, capped by RAM with the most an export process took. The result
// goes into its own file, assessment.json in the packshot folder, with the keys that make it stale.
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::engine::{Engine, Level, Target, parse_timing};
use crate::net::{AssessInfo, Hello, ItemJob, Msg, Payload, RenderJob};
use crate::node::LOCAL;

/// A step must gain this much over the previous one, else the ladder stops; the pick takes the smallest count
/// within this much of the best
pub const PLATEAU: f64 = 0.05;
/// Items in the test set
pub const TEST_ITEMS: usize = 48;
/// Blender processes per step
const LADDER: [usize; 12] = [1, 2, 3, 4, 6, 8, 10, 12, 16, 20, 24, 32];
/// export-batch processes at most in the export sweep
const EXPORT_MAX: usize = 4;
/// Share of RAM and VRAM kept free
const MARGIN: f64 = 0.1;
const SAMPLE_EVERY: Duration = Duration::from_millis(500);
/// No line from any process for this long: the step fails
const SILENCE: Duration = Duration::from_secs(300);
/// Exporting and fetching the test set may take this long
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(600);
/// Assessment log folders kept in <work>/assess/ (the oldest go at the start of the next one)
const KEEP_LOGS: usize = 5;
/// Images counted per Blender in a render step (after its first, unmeasured one)
const MEASURED_PER_BLENDER: usize = 8;

/// What an assessment depends on: when any of it differs, the assessment is stale and the formula counts again.
/// Not the packshot build: any source change gives a new one without changing the load (a "packshot" key in an
/// older file is ignored)
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Keys {
    pub blender: String,
    #[serde(default)]
    pub gpu: Option<String>,
    #[serde(default)]
    pub render_py: Option<String>,
    #[serde(default)]
    pub addon: Option<String>,
    #[serde(default)]
    pub starbreaker: Option<String>,
    /// host name and CPU: an assessment.json copied to another machine is stale there (also without nvidia-smi)
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub cpu: Option<String>,
}

impl Keys {
    pub fn of(me: &Hello, render_script: &Path) -> Self {
        Self {
            blender: me.blender.clone(),
            gpu: me.gpu.clone(),
            render_py: crate::events::file_hash(render_script),
            addon: me.addon.clone(),
            starbreaker: me.starbreaker.clone(),
            host: Some(me.name.clone()),
            cpu: me.cpu.clone(),
        }
    }

    /// What differs now ("Blender 5.2.4 (assessed 5.2.3)"); empty = valid
    pub fn diff(&self, now: &Keys) -> Vec<String> {
        let mut out = Vec::new();
        let mut cmp = |what: &str, was: Option<&str>, is: Option<&str>| {
            if was != is {
                out.push(format!("{what} {} (assessed {})", is.unwrap_or("?"), was.unwrap_or("?")));
            }
        };
        cmp("Blender", Some(&self.blender), Some(&now.blender));
        cmp("GPU", self.gpu.as_deref(), now.gpu.as_deref());
        cmp("render.py", self.render_py.as_deref(), now.render_py.as_deref());
        cmp("add-on", self.addon.as_deref(), now.addon.as_deref());
        cmp("StarBreaker", self.starbreaker.as_deref(), now.starbreaker.as_deref());
        cmp("host", self.host.as_deref(), now.host.as_deref());
        cmp("CPU", self.cpu.as_deref(), now.cpu.as_deref());
        out
    }
}

/// One step of a sweep: `n` Blender processes (and `e` export-batch processes in the export sweep)
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Step {
    pub n: usize,
    #[serde(default)]
    pub e: usize,
    /// render sweep: rendered items/s; export sweep: exported items/s
    pub items_s: f64,
    /// render items/s (export sweep: of the Blender rendering meanwhile)
    pub render_s: f64,
    /// mean Blender start-up (first image minus its phases) and the mean load and render phases
    #[serde(default)]
    pub start_s: f64,
    #[serde(default)]
    pub load_s: f64,
    #[serde(default)]
    pub render_phase_s: f64,
    /// CPU in % while every process ran: all cores, and the busiest core per sample (means)
    pub cpu_total: f64,
    pub cpu_max_core: f64,
    #[serde(default)]
    pub gpu: Option<f64>,
    #[serde(default)]
    pub gpu_max: Option<f64>,
    /// peak used VRAM and RAM (MB), and the rise per process over the use before the step
    #[serde(default)]
    pub vram_mb: Option<u64>,
    pub ram_mb: u64,
    #[serde(default)]
    pub vram_proc_mb: Option<u64>,
    #[serde(default)]
    pub ram_proc_mb: u64,
    /// items that failed (a step with failures does not count)
    #[serde(default)]
    pub failed: usize,
    pub secs: f64,
}

/// The result of an assessment: assessment.json in the packshot folder
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Assessment {
    pub date: String,
    pub workers: usize,
    /// None: assessed without --export
    #[serde(default)]
    pub exporters: Option<usize>,
    /// items/s of the chosen workers as in a run: with the chosen exports alongside on an exporting node
    pub render: f64,
    #[serde(default)]
    pub export: Option<f64>,
    /// items in the test set, per Blender chunk, and why the render ladder stopped
    pub items: usize,
    pub chunk: usize,
    #[serde(default)]
    pub stop: String,
    /// RAM and VRAM caps on the workers (processes that fit)
    #[serde(default)]
    pub mem_cap: Option<usize>,
    /// RAM cap on the exporters: the most resident memory an export process took, next to the chosen Blenders
    #[serde(default)]
    pub export_mem_cap: Option<usize>,
    pub render_steps: Vec<Step>,
    #[serde(default)]
    pub export_steps: Vec<Step>,
    pub secs: f64,
    pub keys: Keys,
}

impl Assessment {
    /// What the hello carries
    pub fn info(&self, valid: bool) -> AssessInfo {
        AssessInfo {
            render: self.render,
            export: self.export.unwrap_or(0.0),
            workers: self.workers,
            exporters: self.exporters.unwrap_or(0),
            valid,
            date: self.date.get(..16).unwrap_or(&self.date).replace('T', " "),
            steps: self.render_steps.iter().filter(|s| s.failed == 0).map(|s| (s.n, s.items_s)).collect(),
            export_steps: self.export_steps.iter().filter(|s| s.failed == 0).map(|s| (s.e, s.items_s)).collect(),
        }
    }
}

/// The node's assessment file in the packshot folder
pub const FILE: &str = "assessment.json";

/// The assessment in its file, if there is one
pub fn load(path: &Path) -> Option<Assessment> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// The assessment into its file (replaced as a whole), written atomically
pub fn save(path: &Path, a: &Assessment) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(a)?).with_context(|| tmp.display().to_string())?;
    fs::rename(&tmp, path).with_context(|| path.display().to_string())
}

/// Whether the ladder stops after a step with rate `cur`: it gained less than PLATEAU over `prev` (or dropped)
pub fn levelled(prev: f64, cur: f64) -> bool {
    cur < prev * (1.0 + PLATEAU)
}

/// The smallest count within PLATEAU of the best rate, at most `cap` (steps: count, rate)
pub fn pick(steps: &[(usize, f64)], cap: usize) -> Option<usize> {
    let best = steps.iter().map(|s| s.1).fold(0.0, f64::max);
    if best <= 0.0 {
        return None;
    }
    let cap = cap.max(1);
    let within = steps.iter().filter(|s| s.1 >= best * (1.0 - PLATEAU)).map(|s| s.0).min()?;
    Some(within.min(cap))
}

/// How many processes of `per` bytes fit next to `base` bytes in use, keeping MARGIN of `total` free
pub fn fits(total: u64, base: u64, per: u64) -> usize {
    if per == 0 {
        return usize::MAX;
    }
    let room = ((total as f64) * (1.0 - MARGIN)) as u64;
    (room.saturating_sub(base) / per) as usize
}

/// The test set: `n` targets stratified by type (largest remainder, proportional to the type's share of the list),
/// within a type evenly spread over its targets sorted by class name; deterministic for a list
pub fn test_set(targets: &[Target], n: usize) -> Vec<usize> {
    let mut by: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, t) in targets.iter().enumerate() {
        by.entry(t.kind.as_str()).or_default().push(i);
    }
    for v in by.values_mut() {
        v.sort_by(|a, b| targets[*a].entity.to_lowercase().cmp(&targets[*b].entity.to_lowercase()).then(a.cmp(b)));
    }
    let total = targets.len();
    if total <= n {
        return by.into_values().flatten().collect();
    }
    let mut quota: Vec<(&str, usize, f64)> = by.iter().map(|(k, v)| {
        let q = v.len() as f64 * n as f64 / total as f64;
        (*k, q.floor() as usize, q - q.floor())
    }).collect();
    let left = n - quota.iter().map(|q| q.1).sum::<usize>();
    let mut order: Vec<usize> = (0..quota.len()).collect();
    order.sort_by(|a, b| quota[*b].2.total_cmp(&quota[*a].2).then(quota[*a].0.cmp(quota[*b].0)));
    for &i in order.iter().take(left) {
        quota[i].1 += 1;
    }
    let mut out = Vec::new();
    for (kind, k, _) in quota {
        let v = &by[kind];
        out.extend((0..k).map(|j| v[((j as f64 + 0.5) * v.len() as f64 / k as f64) as usize]));
    }
    out
}

/// Folder name part for a test set: the exporting nodes' Data.p4k (its fingerprint may come from another node:
/// letters and digits only) and the entities
fn testset_key(p4k: &str, entities: &[&str]) -> String {
    let p4k: String = p4k.chars().filter(char::is_ascii_alphanumeric).take(8).collect();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in entities.join("\n").bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{p4k}-{:08x}", h >> 32)
}

/// An item of the test set: paths relative to its export folder (x/)
#[derive(Serialize, Deserialize, Clone, Debug)]
struct TestEntry {
    entity: String,
    worn: bool,
    package: String,
    rig: String,
    pose: [f64; 3],
}

#[derive(Serialize, Deserialize)]
struct TestSetFile {
    p4k_id: String,
    items: Vec<TestEntry>,
}

/// What the sweep renders and exports
pub struct SweepParams {
    pub blender: PathBuf,
    pub script: PathBuf,
    pub size: u32,
    pub chunk: usize,
    /// test set: (entity, worn, scene.blend, rig, pose)
    pub items: Vec<(String, bool, PathBuf, PathBuf, [f64; 3])>,
    /// StarBreaker and Data.p4k: the export sweep (exporting nodes); whether its export-batch has --stdin (then the
    /// sweep runs it long-lived, as a run does)
    pub export: Option<(PathBuf, PathBuf)>,
    pub export_stdin: bool,
    /// scratch folder of this assessment
    pub dir: PathBuf,
    /// largest Blender count
    pub max: usize,
    pub keys: Keys,
}

pub enum SweepEv {
    Line(String),
    Done(std::result::Result<Box<Assessment>, String>),
}

#[derive(Clone, Copy)]
struct Sample {
    t: Instant,
    cpu: f64,
    core: f64,
    ram: u64,
    gpu: Option<f64>,
    vram: Option<u64>,
}

/// CPU per core, RAM and (nvidia-smi, first GPU) utilisation and VRAM every SAMPLE_EVERY
struct Sampler {
    samples: Arc<Mutex<Vec<Sample>>>,
    stop: Arc<AtomicBool>,
    smi: Option<Child>,
    ram_total: Arc<AtomicU64>,
    vram_total: Arc<AtomicU64>,
}

const NONE: u64 = u64::MAX;

impl Sampler {
    fn start() -> Self {
        let (util, used, vtotal) = (Arc::new(AtomicU64::new(NONE)), Arc::new(AtomicU64::new(NONE)), Arc::new(AtomicU64::new(NONE)));
        let ms = SAMPLE_EVERY.as_millis().to_string();
        let mut smi = Command::new("nvidia-smi")
            .args(["-i", "0", "--query-gpu=utilization.gpu,memory.used,memory.total", "--format=csv,noheader,nounits", "-lms", ms.as_str()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok();
        if let Some(out) = smi.as_mut().and_then(|c| c.stdout.take()) {
            let (u, m, t) = (util.clone(), used.clone(), vtotal.clone());
            thread::spawn(move || {
                for line in BufReader::new(out).lines().map_while(std::result::Result::ok) {
                    let v: Vec<u64> = line.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                    if let [a, b, c] = v[..] {
                        u.store(a, Ordering::Relaxed);
                        m.store(b, Ordering::Relaxed);
                        t.store(c, Ordering::Relaxed);
                    }
                }
            });
        }
        let samples = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let ram_total = Arc::new(AtomicU64::new(0));
        let (s, st, rt) = (samples.clone(), stop.clone(), ram_total.clone());
        thread::spawn(move || {
            let Ok(mut sys) = sysinfo::System::new() else { return };
            sys.refresh_cpu_usage();
            while !st.load(Ordering::Relaxed) {
                thread::sleep(SAMPLE_EVERY);
                sys.refresh_cpu_usage();
                sys.refresh_memory();
                rt.store(sys.total_memory(), Ordering::Relaxed);
                let core = sys.cpus().iter().map(|c| f64::from(c.usage())).fold(0.0, f64::max);
                let opt = |a: &AtomicU64| Some(a.load(Ordering::Relaxed)).filter(|v| *v != NONE);
                let sample = Sample { t: Instant::now(), cpu: f64::from(sys.global_cpu_usage()), core, ram: sys.used_memory(), gpu: opt(&util).map(|v| v as f64), vram: opt(&used).map(|v| v << 20) };
                if let Ok(mut v) = s.lock() {
                    v.push(sample);
                }
            }
        });
        Self { samples, stop, smi, ram_total, vram_total: vtotal }
    }

    fn window(&self, from: Instant, to: Instant) -> Vec<Sample> {
        self.samples.lock().map(|v| v.iter().filter(|s| s.t >= from && s.t <= to).copied().collect()).unwrap_or_default()
    }

    fn latest(&self) -> Option<Sample> {
        self.samples.lock().ok().and_then(|v| v.last().copied())
    }

    fn ram_total(&self) -> u64 {
        self.ram_total.load(Ordering::Relaxed)
    }

    fn vram_total(&self) -> Option<u64> {
        Some(self.vram_total.load(Ordering::Relaxed)).filter(|v| *v != NONE).map(|v| v << 20)
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(c) = self.smi.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// Means and peaks over the samples of a window
struct Agg {
    cpu: f64,
    core: f64,
    gpu: Option<f64>,
    gpu_max: Option<f64>,
    vram: Option<u64>,
    ram: u64,
}

fn agg(s: &[Sample]) -> Agg {
    let mean = |v: Vec<f64>| if v.is_empty() { None } else { Some(v.iter().sum::<f64>() / v.len() as f64) };
    let gpu: Vec<f64> = s.iter().filter_map(|x| x.gpu).collect();
    Agg {
        cpu: mean(s.iter().map(|x| x.cpu).collect()).unwrap_or(0.0),
        core: mean(s.iter().map(|x| x.core).collect()).unwrap_or(0.0),
        gpu_max: gpu.iter().copied().reduce(f64::max),
        gpu: mean(gpu),
        vram: s.iter().filter_map(|x| x.vram).max(),
        ram: s.iter().map(|x| x.ram).max().unwrap_or(0),
    }
}

enum Ev {
    Line(usize, Instant, String),
    Eof(usize, Instant),
}

/// Jobs fed to a Blender beyond the one it renders (as in node.rs)
const LOOKAHEAD: usize = 1;

/// A Blender (render.py --stdin, as in a run) or export-batch process of the sweep
struct Proc {
    export: bool,
    child: Child,
    stdin: Option<ChildStdin>,
    started: Instant,
    /// it said ready (Blender "ready:", export-batch --stdin {"ready": true}; one over a jobs file at once)
    ready_at: Option<Instant>,
    /// export-batch --stdin: when its chunk went in and when it answered chunk_done
    sent_at: Option<Instant>,
    chunk_done: Option<Instant>,
    ended: Option<Instant>,
    /// render: every answer (time, rendered, phases) and the item indices that failed; export: exported items
    answers: Vec<(Instant, bool, Option<[f64; 5]>)>,
    failed: Vec<usize>,
    exported: usize,
    /// render: job lines not sent yet, lines sent and not answered; `cycle`: the next item and the folder of a
    /// Blender fed the set round for ever (rendering alongside the exports)
    todo: VecDeque<String>,
    fed: usize,
    cycle: Option<(usize, PathBuf)>,
    /// export-batch --stdin: its chunk's lines, sent once it is ready
    lines: Vec<String>,
    /// the most resident memory seen (export processes)
    rss_peak: u64,
    tail: Arc<Mutex<String>>,
}

impl Proc {
    fn icons(&self) -> usize {
        self.answers.iter().filter(|a| a.1).count()
    }
    fn done(&self) -> usize {
        if self.export { self.exported } else { self.icons() }
    }
    /// An export's time: from its chunk going in to chunk_done (--stdin, its start-up not counted, as a run pays it
    /// once), else the process's whole life
    fn wall(&self) -> f64 {
        let (a, b) = match (self.sent_at, self.chunk_done) {
            (Some(a), Some(b)) => (a, b),
            _ => (self.started, self.ended.unwrap_or_else(Instant::now)),
        };
        b.duration_since(a).as_secs_f64().max(1e-3)
    }
}

/// A render job line for item `i` of the test set, its image into `dir`
fn render_line(p: &SweepParams, i: usize, dir: &Path) -> String {
    let (_, _, src, rig, pose) = &p.items[i];
    json!({ "src": src, "out": dir.join(format!("{i}.png")), "pose": pose, "rig": rig, "size": p.size }).to_string()
}

/// A ready process gets its work: export-batch --stdin its chunk at once, a Blender its next job (and LOOKAHEAD
/// more) as it answers; a Blender with nothing left gets EOF and ends
fn feed(params: &SweepParams, p: &mut Proc) {
    if p.ready_at.is_none() {
        return;
    }
    let Some(stdin) = p.stdin.as_mut() else { return };
    if p.export {
        if !p.lines.is_empty() {
            let text: String = p.lines.iter().map(|l| format!("{l}\n")).collect();
            let _ = stdin.write_all(text.as_bytes()).and_then(|_| stdin.flush());
            p.sent_at = Some(Instant::now());
            p.lines.clear();
        }
        return;
    }
    while p.fed <= LOOKAHEAD {
        let line = match (p.todo.pop_front(), p.cycle.as_mut()) {
            (Some(l), _) => l,
            (None, Some((next, dir))) => {
                let i = *next % params.items.len().max(1);
                *next += 1;
                render_line(params, i, dir)
            }
            (None, None) => break,
        };
        if stdin.write_all(format!("{line}\n").as_bytes()).and_then(|_| stdin.flush()).is_err() {
            break;
        }
        p.fed += 1;
    }
    if p.todo.is_empty() && p.fed == 0 && p.cycle.is_none() {
        p.stdin = None;
    }
}

struct Sweep {
    p: SweepParams,
    tx: Sender<Ev>,
    rx: Receiver<Ev>,
    procs: HashMap<usize, Proc>,
    next: usize,
    cancel: Arc<AtomicBool>,
    out: Sender<SweepEv>,
    last_line: Instant,
    sampler: Sampler,
    /// export processes' resident memory, sampled in pump
    sys: Option<sysinfo::System>,
    last_rss: Instant,
}

impl Drop for Sweep {
    fn drop(&mut self) {
        for p in self.procs.values_mut() {
            let _ = p.child.kill();
            let _ = p.child.wait();
        }
    }
}

/// The item index from an output path "<dir>/<i>.png" in a render.py line
fn item_of(rest: &str) -> Option<usize> {
    let end = rest.to_lowercase().find(".png")?;
    let path = &rest[..end];
    let stem = path.rsplit(['/', '\\']).next()?;
    stem.parse().ok()
}

impl Sweep {
    fn say(&self, s: String) {
        let _ = self.out.send(SweepEv::Line(s));
    }

    /// A process over these items of the test set: a Blender with render.py --stdin (`cycle`: fed the set round for
    /// ever from that index), or export-batch into `dir` (--stdin when StarBreaker has it, else over a jobs file)
    fn spawn(&mut self, export: bool, items: &[usize], dir: &Path, cycle: Option<usize>) -> Result<usize> {
        let _ = fs::remove_dir_all(dir);
        fs::create_dir_all(dir)?;
        let persistent = !export || self.p.export_stdin;
        let (mut lines, mut todo) = (Vec::new(), VecDeque::new());
        let mut cmd = if export {
            let (sb, p4k) = self.p.export.clone().context("no export tools")?;
            let list: Vec<Value> = items.iter().map(|&i| json!({ "name": self.p.items[i].0, "output": dir, "worn": self.p.items[i].1 })).collect();
            let mut c = Command::new(&sb);
            c.arg("entity").arg("export-batch");
            if persistent {
                c.arg("--stdin");
                lines = list.iter().map(Value::to_string).collect();
                lines.push(json!({ "end_chunk": dir }).to_string());
            } else {
                fs::write(dir.join("jobs.json"), serde_json::to_vec(&list)?)?;
                c.arg(dir.join("jobs.json"));
            }
            c.arg("--p4k").arg(&p4k).args(["--kind", "decomposed", "--variant-tags", "--icon-files"]);
            if cfg!(target_os = "linux") {
                c.env("MALLOC_ARENA_MAX", "2");
            }
            c
        } else {
            todo = items.iter().map(|&i| render_line(&self.p, i, dir)).collect();
            let mut c = crate::blender::command(&self.p.blender);
            c.arg("--gpu-backend").arg("vulkan").arg("--python").arg(&self.p.script).arg("--").arg("--stdin");
            c
        };
        let started = Instant::now();
        let mut child = cmd
            .stdin(if persistent { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("{:?} does not start", cmd.get_program()))?;
        let id = self.next;
        self.next += 1;
        let tail = Arc::new(Mutex::new(String::new()));
        if let Some(out) = child.stdout.take() {
            let tx = self.tx.clone();
            thread::spawn(move || {
                let mut r = BufReader::new(out);
                let mut buf = Vec::new();
                while r.read_until(b'\n', &mut buf).unwrap_or(0) > 0 {
                    let line = String::from_utf8_lossy(&buf).trim_end().to_string();
                    buf.clear();
                    if tx.send(Ev::Line(id, Instant::now(), line)).is_err() {
                        return;
                    }
                }
                let _ = tx.send(Ev::Eof(id, Instant::now()));
            });
        }
        if let Some(mut err) = child.stderr.take() {
            let t = tail.clone();
            thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(n) = err.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    if let Ok(mut s) = t.lock() {
                        s.push_str(&String::from_utf8_lossy(&buf[..n]));
                        crate::engine::keep_tail(&mut s, 4000);
                    }
                }
            });
        }
        let stdin = child.stdin.take();
        let proc = Proc {
            export,
            child,
            stdin,
            started,
            ready_at: (!persistent).then_some(started),
            sent_at: None,
            chunk_done: None,
            ended: None,
            answers: Vec::new(),
            failed: Vec::new(),
            exported: 0,
            todo,
            fed: 0,
            cycle: cycle.map(|i| (i, dir.to_path_buf())),
            lines,
            rss_peak: 0,
            tail,
        };
        self.procs.insert(id, proc);
        Ok(id)
    }

    /// Lines for about 100 ms (the processes fed as they answer); the processes whose output ended (reaped)
    fn pump(&mut self) -> Result<Vec<usize>> {
        if self.cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        let until = Instant::now() + Duration::from_millis(100);
        let mut ended = Vec::new();
        while let Ok(ev) = self.rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
            match ev {
                Ev::Line(id, t, line) => {
                    self.last_line = t;
                    let Some(p) = self.procs.get_mut(&id) else { continue };
                    let line = line.trim();
                    if p.export {
                        if let Ok(v) = serde_json::from_str::<Value>(line) {
                            if let Some(ok) = v.get("ready").and_then(Value::as_bool) {
                                if ok {
                                    p.ready_at = Some(t);
                                } else if let Ok(mut s) = p.tail.lock() {
                                    s.push_str(&format!("export-batch not ready: {}\n", v["error"].as_str().unwrap_or("no reason")));
                                }
                            } else if v.get("chunk_done").is_some() {
                                p.chunk_done = Some(t);
                                p.stdin = None;
                            } else if v["ok"] == true {
                                p.exported += 1;
                            } else if v.get("ok").is_some() {
                                p.failed.push(0);
                            }
                        }
                    } else if line.starts_with("ready:") {
                        p.ready_at.get_or_insert(t);
                    } else if let Some(rest) = line.strip_prefix("icon: ") {
                        p.answers.push((t, true, parse_timing(rest)));
                        p.fed = p.fed.saturating_sub(1);
                    } else if let Some(rest) = line.strip_prefix("failed: ") {
                        p.answers.push((t, false, None));
                        p.failed.push(item_of(rest).unwrap_or(usize::MAX));
                        p.fed = p.fed.saturating_sub(1);
                    }
                    feed(&self.p, p);
                }
                Ev::Eof(id, t) => {
                    if let Some(p) = self.procs.get_mut(&id) {
                        p.ended = Some(t);
                        let _ = p.child.wait();
                        ended.push(id);
                    }
                }
            }
            if Instant::now() >= until {
                break;
            }
        }
        // the export processes' resident memory (the export memory cap)
        if self.last_rss.elapsed() >= SAMPLE_EVERY {
            self.last_rss = Instant::now();
            for p in self.procs.values_mut().filter(|p| p.export && p.ended.is_none()) {
                if let Some(m) = crate::node::rss_of(&mut self.sys, p.child.id()) {
                    p.rss_peak = p.rss_peak.max(m);
                }
            }
        }
        if !self.procs.is_empty() && self.last_line.elapsed() > SILENCE {
            bail!("no output from any process for {} s", SILENCE.as_secs());
        }
        Ok(ended)
    }

    /// Wait until all of `ids` ended; `other` is called for any other process that ended
    fn wait_all(&mut self, ids: &[usize], other: &mut dyn FnMut(&mut Self, usize) -> Result<()>) -> Result<Vec<Proc>> {
        let mut done: HashMap<usize, Proc> = HashMap::new();
        while done.len() < ids.len() {
            for id in self.pump()? {
                if ids.contains(&id) {
                    if let Some(p) = self.procs.remove(&id) {
                        done.insert(id, p);
                    }
                } else {
                    other(self, id)?;
                }
            }
        }
        Ok(ids.iter().filter_map(|i| done.remove(i)).collect())
    }

    fn settle(&mut self) -> Result<()> {
        // the memory of the processes before goes back, the sampler gets a fresh value
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(1) {
            self.pump()?;
        }
        Ok(())
    }

    /// The unmeasured warm-up: one Blender over the whole set (file cache, Blender's own caches); items failing
    /// there leave it
    fn warm_up(&mut self) -> Result<()> {
        let len = self.p.items.len();
        self.say(format!("Warm-up: 1 Blender over the {len} test items (not measured)"));
        let dir = self.p.dir.join("warm");
        let all: Vec<usize> = (0..len).collect();
        let id = self.spawn(false, &all, &dir, None)?;
        let p = self.wait_all(&[id], &mut |_, _| Ok(()))?.remove(0);
        let _ = fs::remove_dir_all(&dir);
        if p.answers.len() < len {
            let tail = p.tail.lock().map(|s| s.clone()).unwrap_or_default();
            bail!("{}", crate::engine::with_trace(&format!("warm-up: Blender ended early ({} of {len} items)", p.answers.len()), &tail));
        }
        let mut bad = p.failed.clone();
        bad.sort();
        bad.dedup();
        for &i in bad.iter().rev() {
            if i < self.p.items.len() {
                let e = self.p.items.remove(i);
                self.say(format!("Warm-up: {} failed, left out of the test set", e.0));
            }
        }
        let wall = p.ended.unwrap_or_else(Instant::now).duration_since(p.started).as_secs_f64().max(1e-3);
        self.say(format!("Warm-up done in {wall:.1} s ({:.2} items/s)", p.icons() as f64 / wall));
        if self.p.items.len() * 2 < len {
            bail!("only {} of the {len} test items render", self.p.items.len());
        }
        Ok(())
    }

    fn chunk(&self) -> usize {
        self.p.chunk.max(1).min(self.p.items.len())
    }

    /// n Blender at once as in a run (long-lived, fed the test set round, rotated so they do not render the same item
    /// at once): once every process has answered its first item (its start-up and first shader compilations, paid once
    /// per process in a run), the images of all of them are counted until there are at least MEASURED_PER_BLENDER per
    /// process (and the whole set); items/s = those images / that window, the same steady-state measure as the render
    /// rate in the export sweep (one chunk per process made many processes look slower than they are: servbox 30.0
    /// items/s with 16 Blender alone against 36.6 alongside the exports, 2026-11-05)
    fn render_step(&mut self, n: usize) -> Result<Step> {
        let len = self.p.items.len();
        let base = self.sampler.latest();
        let t0 = Instant::now();
        let dir = self.p.dir.join(format!("r{n}"));
        let mut ids = Vec::new();
        for k in 0..n {
            ids.push(self.spawn(false, &[], &dir.join(format!("p{k}")), Some(k * len / n))?);
        }
        let ended = |s: &mut Self, id: usize| -> Result<()> {
            let tail = s.procs.get(&id).and_then(|p| p.tail.lock().ok().map(|t| t.clone())).unwrap_or_default();
            bail!("{}", crate::engine::with_trace(&format!("a Blender of the step with {n} ended"), &tail))
        };
        // warm: every process has answered once
        while !ids.iter().all(|id| self.procs.get(id).is_some_and(|p| !p.answers.is_empty())) {
            for id in self.pump()? {
                ended(self, id)?;
            }
        }
        let from = Instant::now();
        let seen: Vec<usize> = ids.iter().map(|id| self.procs.get(id).map_or(0, |p| p.answers.len())).collect();
        let want = len.max(MEASURED_PER_BLENDER * n);
        let counted = |s: &Self| ids.iter().zip(&seen).map(|(id, k)| s.procs.get(id).map_or(0, |p| p.answers.len().saturating_sub(*k))).sum::<usize>();
        while counted(self) < want {
            for id in self.pump()? {
                ended(self, id)?;
            }
        }
        let t1 = Instant::now();
        let mut procs = Vec::new();
        for id in &ids {
            if let Some(mut p) = self.procs.remove(id) {
                let _ = p.child.kill();
                let _ = p.child.wait();
                procs.push(p);
            }
        }
        let _ = fs::remove_dir_all(&dir);
        let measured: Vec<&(Instant, bool, Option<[f64; 5]>)> = procs.iter().zip(&seen).flat_map(|(p, k)| p.answers.iter().skip(*k)).filter(|a| a.0 <= t1).collect();
        let images = measured.iter().filter(|a| a.1).count();
        let failed = measured.iter().filter(|a| !a.1).count();
        let rate = images as f64 / t1.duration_since(from).as_secs_f64().max(1e-3);
        let a = agg(&self.sampler.window(from, t1));
        let starts: Vec<f64> = procs.iter().filter_map(|p| p.ready_at.map(|r| r.duration_since(p.started).as_secs_f64())).collect();
        let phases: Vec<[f64; 5]> = measured.iter().filter_map(|x| x.2).collect();
        let mean = |v: &[f64]| if v.is_empty() { 0.0 } else { v.iter().sum::<f64>() / v.len() as f64 };
        let per = |peak: Option<u64>, before: Option<u64>| peak.zip(before).map(|(p, b)| (p.saturating_sub(b) / n as u64) >> 20);
        Ok(Step {
            n,
            e: 0,
            items_s: rate,
            render_s: rate,
            start_s: mean(&starts),
            load_s: mean(&phases.iter().map(|p| p[0]).collect::<Vec<_>>()),
            render_phase_s: mean(&phases.iter().map(|p| p[2]).collect::<Vec<_>>()),
            cpu_total: a.cpu,
            cpu_max_core: a.core,
            gpu: a.gpu,
            gpu_max: a.gpu_max,
            vram_mb: a.vram.map(|v| v >> 20),
            ram_mb: a.ram >> 20,
            vram_proc_mb: per(a.vram, base.and_then(|b| b.vram)),
            ram_proc_mb: per(Some(a.ram), base.map(|b| b.ram)).unwrap_or(0),
            failed,
            secs: t0.elapsed().as_secs_f64(),
        })
    }

    /// Steps of `e` export-batch at once while `n` Blender keep rendering (fed the set round); also the most
    /// resident memory one export process took
    fn export_sweep(&mut self, n: usize) -> Result<(Vec<Step>, u64)> {
        let len = self.p.items.len();
        let c = self.chunk();
        let bg_dir = self.p.dir.join("bg");
        let mut bg = Vec::new();
        for k in 0..n {
            bg.push(self.spawn(false, &[], &bg_dir.join(format!("p{k}")), Some(k * len / n))?);
        }
        let mut alongside = |s: &mut Self, id: usize| -> Result<()> {
            let tail = s.procs.get(&id).and_then(|p| p.tail.lock().ok().map(|t| t.clone())).unwrap_or_default();
            bail!("{}", crate::engine::with_trace("a Blender rendering alongside the exports ended", &tail))
        };
        // warm-up: one export of a chunk (Data.p4k into the file cache) while the Blender start
        self.say(format!("Export sweep: {n} Blender rendering alongside, warm-up export of {c} items (not measured){}", if self.p.export_stdin { ", export-batch --stdin" } else { "" }));
        let xw = self.p.dir.join("xw");
        let w = self.spawn(true, &(0..c).collect::<Vec<_>>(), &xw, None)?;
        self.wait_all(&[w], &mut alongside)?;
        let _ = fs::remove_dir_all(&xw);
        while !bg.iter().all(|id| self.procs.get(id).is_some_and(|p| !p.answers.is_empty())) {
            for id in self.pump()? {
                alongside(self, id)?;
            }
        }
        let mut steps: Vec<Step> = Vec::new();
        let mut rss = 0u64;
        for e in 1..=EXPORT_MAX {
            let t0 = Instant::now();
            let dir = self.p.dir.join(format!("x{e}"));
            let mut ids = Vec::new();
            for k in 0..e {
                let items: Vec<usize> = (0..c).map(|j| (k * len / e + j) % len).collect();
                ids.push(self.spawn(true, &items, &dir.join(format!("p{k}")), None)?);
            }
            let procs = self.wait_all(&ids, &mut alongside)?;
            let _ = fs::remove_dir_all(&dir);
            rss = rss.max(procs.iter().map(|p| p.rss_peak).max().unwrap_or(0));
            let from = procs.iter().map(|p| p.sent_at.unwrap_or(p.started)).min().unwrap_or(t0);
            let t1 = procs.iter().filter_map(|p| p.chunk_done.or(p.ended)).max().unwrap_or_else(Instant::now);
            let first_end = procs.iter().filter_map(|p| p.chunk_done.or(p.ended)).min().unwrap_or(t1);
            let rate: f64 = procs.iter().map(|p| p.done() as f64 / p.wall()).sum();
            let rendered = bg.iter().filter_map(|id| self.procs.get(id)).flat_map(|p| p.answers.iter()).filter(|a| a.1 && a.0 >= from && a.0 <= t1).count();
            let render = rendered as f64 / t1.duration_since(from).as_secs_f64().max(1e-3);
            let a = agg(&self.sampler.window(from, first_end));
            let failed = procs.iter().map(|p| c.saturating_sub(p.done())).sum();
            let step = Step {
                n,
                e,
                items_s: rate,
                render_s: render,
                cpu_total: a.cpu,
                cpu_max_core: a.core,
                gpu: a.gpu,
                gpu_max: a.gpu_max,
                vram_mb: a.vram.map(|v| v >> 20),
                ram_mb: a.ram >> 20,
                failed,
                secs: t0.elapsed().as_secs_f64(),
                ..Default::default()
            };
            self.say(export_row(&step, steps.last()));
            let stop = failed > 0 || steps.last().is_some_and(|p| levelled(combined(p), combined(&step)));
            let full = a.ram as f64 > self.sampler.ram_total() as f64 * (1.0 - MARGIN);
            steps.push(step);
            if stop || full {
                break;
            }
        }
        for id in bg {
            if let Some(mut p) = self.procs.remove(&id) {
                let _ = p.child.kill();
                let _ = p.child.wait();
            }
        }
        let _ = fs::remove_dir_all(&bg_dir);
        Ok((steps, rss))
    }
}

/// What a node gets out of an export step on its own: the smaller of export and render rate
fn combined(s: &Step) -> f64 {
    s.items_s.min(s.render_s)
}

fn opt(v: Option<f64>, digits: usize, unit: &str) -> String {
    v.map(|x| format!("{x:.digits$}{unit}")).unwrap_or_else(|| "–".into())
}

fn gb(mb: Option<u64>) -> String {
    mb.map(|m| format!("{:.1}", m as f64 / 1024.0)).unwrap_or_else(|| "–".into())
}

const RENDER_HEAD: &str = "   n  items/s   gain  start  load  render   CPU  max core   GPU  GPU max  VRAM GB  RAM GB";
const EXPORT_HEAD: &str = "   e  export/s  render/s  combined   CPU  max core   GPU  GPU max  VRAM GB  RAM GB";

fn render_row(s: &Step, prev: Option<&Step>) -> String {
    let gain = prev.map(|p| format!("{:+.0}%", (s.items_s / p.items_s.max(1e-9) - 1.0) * 100.0)).unwrap_or_else(|| "–".into());
    format!(
        "{:>4}  {:>7.2}  {:>5}  {:>5.2}  {:>4.2}  {:>6.2}  {:>3.0}%  {:>7.0}%  {:>4}  {:>7}  {:>7}  {:>6}{}",
        s.n,
        s.items_s,
        gain,
        s.start_s,
        s.load_s,
        s.render_phase_s,
        s.cpu_total,
        s.cpu_max_core,
        opt(s.gpu, 0, "%"),
        opt(s.gpu_max, 0, "%"),
        gb(s.vram_mb),
        gb(Some(s.ram_mb)),
        if s.failed > 0 { format!("  {} failed", s.failed) } else { String::new() }
    )
}

fn export_row(s: &Step, prev: Option<&Step>) -> String {
    let gain = prev.map(|p| format!(" ({:+.0}%)", (combined(s) / combined(p).max(1e-9) - 1.0) * 100.0)).unwrap_or_default();
    format!(
        "{:>4}  {:>8.2}  {:>8.2}  {:>8.2}  {:>3.0}%  {:>7.0}%  {:>4}  {:>7}  {:>7}  {:>6}{gain}{}",
        s.e,
        s.items_s,
        s.render_s,
        combined(s),
        s.cpu_total,
        s.cpu_max_core,
        opt(s.gpu, 0, "%"),
        opt(s.gpu_max, 0, "%"),
        gb(s.vram_mb),
        gb(Some(s.ram_mb)),
        if s.failed > 0 { format!("  {} failed", s.failed) } else { String::new() }
    )
}

/// The result as a table: the render sweep, the export sweep, the choice
pub fn table(a: &Assessment) -> String {
    let mut out = vec![format!("Render sweep: {} test items fed round, each Blender warmed by one item, then at least {MEASURED_PER_BLENDER} images per Blender counted", a.items), RENDER_HEAD.to_string()];
    for (i, s) in a.render_steps.iter().enumerate() {
        out.push(render_row(s, i.checked_sub(1).map(|j| &a.render_steps[j])));
    }
    if !a.export_steps.is_empty() {
        out.push(format!("Export sweep: {} items per export-batch, {} Blender rendering alongside", a.chunk, a.workers));
        out.push(EXPORT_HEAD.to_string());
        for (i, s) in a.export_steps.iter().enumerate() {
            out.push(export_row(s, i.checked_sub(1).map(|j| &a.export_steps[j])));
        }
    }
    out.push(summary(a));
    out.join("\n")
}

/// One line: the choice with its rates, why the ladder stopped, how long it took
pub fn summary(a: &Assessment) -> String {
    let export = match (a.exporters, a.export) {
        (Some(e), Some(x)) => format!(", {e} export ({x:.2} items/s)"),
        _ => String::new(),
    };
    let cap = a.mem_cap.map(|c| format!(" · memory fits {c}")).unwrap_or_default();
    let cap = match a.export_mem_cap {
        Some(e) => format!("{cap} · RAM fits {e} export"),
        None => cap,
    };
    format!("Chosen: {} render ({:.2} items/s){export} · {}{cap} · {:.1} min", a.workers, a.render, a.stop, a.secs / 60.0)
}

/// The whole assessment over a test set (in its own thread): warm-up, render sweep, export sweep, pick
pub fn sweep(p: SweepParams, cancel: Arc<AtomicBool>, out: Sender<SweepEv>) -> Result<Assessment> {
    let t0 = Instant::now();
    let (tx, rx) = channel();
    fs::create_dir_all(&p.dir)?;
    let mut s = Sweep { p, tx, rx, procs: HashMap::new(), next: 1, cancel, out, last_line: Instant::now(), sampler: Sampler::start(), sys: None, last_rss: Instant::now() };
    s.warm_up()?;
    s.settle()?;
    let c = s.chunk();
    s.say(format!("Render sweep: {} test items fed round, at least {MEASURED_PER_BLENDER} images per Blender counted", s.p.items.len()));
    s.say(RENDER_HEAD.to_string());
    let mut steps: Vec<Step> = Vec::new();
    let mut stop = format!("ladder end at {}", s.p.max);
    let idle = s.sampler.latest();
    let max = s.p.max.max(1);
    for &n in LADDER.iter().filter(|&&n| n <= max) {
        // the next step must fit RAM and VRAM with the most any step needed per process
        if let Some(last) = steps.last() {
            let base = s.sampler.latest();
            let ram_per = steps.iter().map(|x| x.ram_proc_mb).max().unwrap_or(0) << 20;
            let vram_per = steps.iter().filter_map(|x| x.vram_proc_mb).max().map(|v| v << 20);
            let ram_fits = fits(s.sampler.ram_total(), base.map(|b| b.ram).unwrap_or(0), ram_per);
            let vram_fits = match (s.sampler.vram_total(), vram_per, base.and_then(|b| b.vram)) {
                (Some(t), Some(per), Some(b)) => fits(t, b, per),
                _ => usize::MAX,
            };
            if n > ram_fits.min(vram_fits) {
                stop = format!("{n} would not fit {} after n={}", if ram_fits < vram_fits { "RAM" } else { "VRAM" }, last.n);
                break;
            }
        }
        let step = s.render_step(n)?;
        s.say(render_row(&step, steps.last()));
        let failed = step.failed > 0;
        let flat = steps.last().map(|prev| (levelled(prev.items_s, step.items_s), prev.items_s));
        steps.push(step);
        if failed {
            stop = format!("items failed at n={n}");
            break;
        }
        if let Some((true, prev)) = flat {
            let cur = steps.last().map(|x| x.items_s).unwrap_or(0.0);
            stop = format!("gain {:+.0}% < {:.0}% at n={n}", (cur / prev.max(1e-9) - 1.0) * 100.0, PLATEAU * 100.0);
            break;
        }
        s.settle()?;
    }
    // memory cap: the most any step needed per process next to what was in use before the sweep
    let ram_per = steps.iter().map(|x| x.ram_proc_mb).max().unwrap_or(0) << 20;
    let vram_per = steps.iter().filter_map(|x| x.vram_proc_mb).max().map(|v| v << 20);
    let ram_cap = fits(s.sampler.ram_total(), idle.map(|b| b.ram).unwrap_or(0), ram_per);
    let vram_cap = match (s.sampler.vram_total(), vram_per, idle.and_then(|b| b.vram)) {
        (Some(t), Some(per), Some(b)) => fits(t, b, per),
        _ => usize::MAX,
    };
    let cap = ram_cap.min(vram_cap);
    let usable: Vec<(usize, f64)> = steps.iter().filter(|x| x.failed == 0).map(|x| (x.n, x.items_s)).collect();
    let workers = pick(&usable, cap).ok_or_else(|| anyhow!("no render step without failures"))?;
    let alone = steps.iter().find(|x| x.n == workers).map(|x| x.items_s).unwrap_or(0.0);
    let (mut exporters, mut export, mut render, mut export_steps, mut export_cap) = (None, None, alone, Vec::new(), usize::MAX);
    if s.p.export.is_some() {
        s.settle()?;
        s.say(EXPORT_HEAD.to_string());
        let (steps, rss) = s.export_sweep(workers)?;
        export_steps = steps;
        // the exporters must fit RAM next to the chosen Blenders, with the most one export process took
        export_cap = fits(s.sampler.ram_total(), idle.map(|b| b.ram).unwrap_or(0) + ram_per * workers as u64, rss);
        let usable: Vec<(usize, f64)> = export_steps.iter().filter(|x| x.failed == 0).map(|x| (x.e, combined(x))).collect();
        if let Some(e) = pick(&usable, export_cap) {
            let st = export_steps.iter().find(|x| x.e == e).cloned().unwrap_or_default();
            exporters = Some(e);
            export = Some(st.items_s);
            render = st.render_s;
        }
    }
    let _ = fs::remove_dir_all(s.p.dir.join("bg"));
    Ok(Assessment {
        date: chrono::Local::now().to_rfc3339(),
        workers,
        exporters,
        render,
        export,
        items: s.p.items.len(),
        chunk: c,
        stop,
        mem_cap: (cap != usize::MAX).then_some(cap),
        export_mem_cap: (export_cap != usize::MAX).then_some(export_cap),
        render_steps: steps,
        export_steps,
        secs: t0.elapsed().as_secs_f64(),
        keys: s.p.keys.clone(),
    })
}

/// An assessment going on this node: getting the test set, then the sweep in its own thread
pub(crate) struct AssessJob {
    /// the node that asked (None: this one)
    requester: Option<u64>,
    started: Instant,
    /// <work>/assess/<date>_<time>/ (assess.log, the sweep's scratch)
    dir: PathBuf,
    log: Option<File>,
    testset: PathBuf,
    p4k_id: String,
    phase: Phase,
}

enum Phase {
    /// the test set being exported by `holder` (connection `conn`, LOCAL: here) as export chunk `id`
    Export { conn: u64, id: u64, holder: String, ok: Vec<usize> },
    /// fetched from the holder
    Fetch { conn: u64, id: u64 },
    Sweep { rx: Receiver<SweepEv>, cancel: Arc<AtomicBool>, handle: Option<thread::JoinHandle<()>> },
}

impl Engine {
    pub fn assessing(&self) -> bool {
        self.assess_job.is_some()
    }

    /// What the hello carries about this node's assessment
    pub fn assess_info(&self) -> Option<AssessInfo> {
        self.assessment.as_ref().map(|a| a.info(self.assess_stale.is_empty()))
    }

    /// Why this node cannot assess now
    pub fn assess_problem(&self) -> Option<String> {
        if !self.me.render || self.cfg.blender.as_os_str().is_empty() {
            return Some("this node does not render (--render)".into());
        }
        if self.assessing() {
            return Some("an assessment is going here".into());
        }
        if self.running() {
            return Some("a run is going here".into());
        }
        if let Some(other) = self.running_elsewhere() {
            return Some(format!("{other} has a run going"));
        }
        if self.serve.as_ref().is_some_and(crate::node::Renderer::busy) {
            return Some("this node is working on chunks".into());
        }
        if self.me.export && !self.tables_ready {
            return Some("the DataCore tables are not there yet".into());
        }
        if self.own_len == 0 {
            return Some(format!("set {} has no target list", self.set.name));
        }
        None
    }

    fn assess_note(&mut self, level: Level, text: String) {
        if let Some(f) = self.assess_job.as_mut().and_then(|j| j.log.as_mut()) {
            let _ = writeln!(f, "{text}");
        }
        self.note(level, text);
    }

    /// Start an assessment of this node (`requester`: the node that asked)
    pub fn start_assess(&mut self, requester: Option<u64>) -> Result<()> {
        if let Some(p) = self.assess_problem() {
            bail!("{p}");
        }
        let items = test_set(&self.targets[..self.own_len], self.assess_items);
        // who exports the test set: this node, else a connected exporting node with the same state
        let (conn, holder, p4k_id) = if self.me.export {
            (LOCAL, self.me.name.clone(), self.me.p4k_id.clone().unwrap_or_default())
        } else {
            let me = self.me.clone();
            let mut nodes: Vec<(u64, Hello)> = self.connected_nodes().into_iter().filter(|(_, h)| h.export && h.exporters > 0 && !h.running && !h.assessing && self.differences_to(h, &me).is_empty()).collect();
            nodes.sort_by(|a, b| a.1.name.cmp(&b.1.name));
            let (c, h) = nodes.into_iter().next().context("no exporting node connected to export the test set")?;
            (c, h.name, h.p4k_id.unwrap_or_default())
        };
        let entities: Vec<&str> = items.iter().map(|&t| self.targets[t].entity.as_str()).collect();
        let testset = self.cfg.work.join("assess").join(format!("testset-{}", testset_key(&p4k_id, &entities)));
        let dir = self.cfg.work.join("assess").join(chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string());
        fs::create_dir_all(&dir).with_context(|| dir.display().to_string())?;
        prune_logs(&self.cfg.work.join("assess"), KEEP_LOGS);
        let log = OpenOptions::new().create(true).append(true).open(dir.join("assess.log")).ok();
        let id = self.next_remote;
        self.next_remote += 1;
        let by = requester.map(|c| format!(" (asked by {})", self.peer_name(c))).unwrap_or_default();
        self.assess_job = Some(AssessJob { requester, started: Instant::now(), dir: dir.clone(), log, testset: testset.clone(), p4k_id, phase: Phase::Export { conn, id, holder: holder.clone(), ok: Vec::new() } });
        self.assess_note(Level::Info, format!("Assessment of {}{by}: {} test items · log {}", self.me.name, items.len(), dir.join("assess.log").display()));
        self.announce_assess();
        if let Some(entries) = read_testset(&testset) {
            self.assess_note(Level::Info, "Test set: kept from an earlier assessment".to_string());
            self.start_sweep(entries);
            return Ok(());
        }
        let jobs: Vec<ItemJob> = items
            .iter()
            .map(|&t| {
                let tg = &self.targets[t];
                ItemJob { item: t, entity: tg.entity.clone(), worn: tg.worn, kind: tg.kind.clone(), sub_type: tg.sub_type.clone(), pose: [0.0; 3], rig: String::new() }
            })
            .collect();
        self.assess_note(Level::Info, format!("Test set: {} items exported on {holder}", jobs.len()));
        if conn == LOCAL {
            let (jobs, bad) = self.fill_items(jobs);
            for (t, e) in bad {
                let entity = self.targets[t].entity.clone();
                self.assess_note(Level::Error, format!("Test set: {entity} left out: {e}"));
            }
            self.dispatch(LOCAL, Msg::Export { id, jobs }, Payload::None);
        } else {
            self.dispatch(conn, Msg::Export { id, jobs }, Payload::None);
        }
        Ok(())
    }

    /// Ask another node to assess itself
    pub fn request_assess(&mut self, conn: u64) -> Result<()> {
        let h = self.hub.as_ref().and_then(|h| h.peer(conn)).map(|p| p.hello.clone()).context("not connected")?;
        if self.running() {
            bail!("a run is going here");
        }
        if let Some(other) = self.running_elsewhere() {
            bail!("{other} has a run going");
        }
        if h.assessing {
            bail!("{} is assessing already", h.name);
        }
        if !h.render {
            bail!("{} does not render", h.name);
        }
        self.dispatch(conn, Msg::Assess, Payload::None);
        Ok(())
    }

    /// Stop the assessment going on here (b in view 3, or another node's AssessAbort): the sweep and its processes,
    /// the test set's export or fetch on the other node, as when it fails
    pub fn abort_assess(&mut self, by: Option<&str>) {
        if !self.assessing() {
            return;
        }
        let who = by.map(|b| format!(" by {b}")).unwrap_or_default();
        self.assess_note(Level::Info, format!("Assessment aborted{who}"));
        if let Some(c) = self.end_job().and_then(|j| j.requester) {
            self.reply_to(c, Msg::AssessDone { ok: false, text: format!("assessment aborted{who}") });
        }
        self.announce_assess();
    }

    /// Ask another node to stop its assessment
    pub fn request_abort_assess(&mut self, conn: u64) -> Result<()> {
        let h = self.hub.as_ref().and_then(|h| h.peer(conn)).map(|p| p.hello.clone()).context("not connected")?;
        if !h.assessing {
            bail!("{} is not assessing", h.name);
        }
        self.dispatch(conn, Msg::AssessAbort, Payload::None);
        Ok(())
    }

    /// The others see that this node is assessing (and its result)
    fn announce_assess(&mut self) {
        self.me.assessing = self.assessing();
        self.me.assess = self.assess_info();
        self.announce();
    }

    /// Assessment messages; the others come back
    pub(crate) fn assess_msg(&mut self, conn: u64, msg: Msg) -> Option<Msg> {
        let mine = |job: &Option<AssessJob>, chunk: u64| matches!(job, Some(AssessJob { phase: Phase::Export { conn: c, id, .. }, .. }) if *c == conn && *id == chunk);
        match msg {
            Msg::Assess => {
                let from = self.peer_name(conn);
                if let Err(e) = self.start_assess(Some(conn)) {
                    self.note(Level::Error, format!("No assessment for {from}: {e:#}"));
                    let me = self.me.name.clone();
                    self.reply_to(conn, Msg::Error { what: format!("no assessment on {me}: {e:#}") });
                }
                None
            }
            Msg::AssessAbort => {
                let from = self.peer_name(conn);
                self.abort_assess(Some(&from));
                None
            }
            Msg::AssessDone { ok, text } => {
                let from = self.peer_name(conn);
                self.note(if ok { Level::Ok } else { Level::Error }, format!("{from}: {text}"));
                None
            }
            Msg::ItemExported { chunk, item, ok, error, .. } if mine(&self.assess_job, chunk) => {
                let entity = self.targets.get(item).map(|t| t.entity.clone()).unwrap_or_default();
                if let (true, Some(AssessJob { phase: Phase::Export { ok: list, .. }, .. })) = (ok, self.assess_job.as_mut()) {
                    list.push(item);
                }
                if !ok {
                    self.assess_note(Level::Error, format!("Test set: {entity} left out: {error}"));
                }
                None
            }
            Msg::ExportDone { id, error, failed, .. } if mine(&self.assess_job, id) => {
                self.assess_exported(error, failed);
                None
            }
            m => Some(m),
        }
    }

    fn reply_to(&self, conn: u64, msg: Msg) {
        if let Some(h) = self.hub.as_ref() {
            h.send(conn, msg, Payload::None);
        }
    }

    /// The test set's export chunk ended: taken from this node's holdings, or fetched from the holder
    fn assess_exported(&mut self, error: String, failed: Vec<(usize, String)>) {
        let Some(job) = self.assess_job.as_mut() else { return };
        let Phase::Export { conn, id, holder, ok } = &mut job.phase else { return };
        let (conn, id, holder) = (*conn, *id, holder.clone());
        let bad: Vec<usize> = failed.iter().map(|f| f.0).collect();
        let ok: Vec<usize> = ok.iter().copied().filter(|t| !bad.contains(t)).collect();
        let testset = job.testset.clone();
        if conn == LOCAL {
            let me = self.me.name.clone();
            let Some((root, held)) = self.serve.as_mut().and_then(|s| s.take_held(&me, id)) else {
                return self.assess_fail(format!("test set not exported: {}", error.lines().next().unwrap_or("no item")));
            };
            let entries: Vec<TestEntry> = held
                .into_iter()
                .map(|(t, package, rig, pose)| TestEntry { entity: self.targets[t].entity.clone(), worn: self.targets[t].worn, package, rig, pose })
                .collect();
            match keep_testset(&testset, &root, &self.assess_job.as_ref().map(|j| j.p4k_id.clone()).unwrap_or_default(), entries) {
                Ok(entries) => self.start_sweep(entries),
                Err(e) => self.assess_fail(format!("test set not kept: {e:#}")),
            }
            return;
        }
        if ok.is_empty() {
            return self.assess_fail(format!("test set not exported on {holder}: {}", error.lines().next().unwrap_or("no item")));
        }
        let jobs: Vec<RenderJob> = ok.iter().map(|&t| RenderJob { item: t, entity: self.targets[t].entity.clone(), holder: holder.clone(), chunk: id }).collect();
        let tmp = testset.with_extension("part");
        let _ = fs::remove_dir_all(&tmp);
        if let Some(job) = self.assess_job.as_mut() {
            job.phase = Phase::Fetch { conn, id };
        }
        let mut lines = Vec::new();
        if let Some(s) = self.serve.as_mut() {
            s.fetch_keep(self.hub.as_ref(), id, jobs, tmp, &mut |l| lines.push(l));
        }
        for l in lines {
            self.note(Level::Info, l);
        }
    }

    /// A node went away: the test set coming from it is lost
    pub(crate) fn assess_down(&mut self, conn: u64) {
        let gone = matches!(self.assess_job.as_ref().map(|j| &j.phase), Some(Phase::Export { conn: c, .. } | Phase::Fetch { conn: c, .. }) if *c == conn && conn != LOCAL);
        if gone {
            self.assess_fail("the node exporting the test set disconnected".into());
        }
        if let Some(j) = self.assess_job.as_mut().filter(|j| j.requester == Some(conn)) {
            j.requester = None;
        }
    }

    /// From tick: the fetched test set, the sweep's lines and result
    pub(crate) fn poll_assess(&mut self) {
        let Some(job) = self.assess_job.as_mut() else { return };
        match &mut job.phase {
            Phase::Export { .. } | Phase::Fetch { .. } if job.started.elapsed() > ACQUIRE_TIMEOUT => {
                self.assess_fail(format!("no test set after {} s", ACQUIRE_TIMEOUT.as_secs()));
            }
            Phase::Export { .. } => {}
            Phase::Fetch { conn, id } => {
                let (conn, id) = (*conn, *id);
                let Some(kept) = self.serve.as_mut().and_then(|s| s.take_kept(id)) else { return };
                let me = self.me.name.clone();
                self.dispatch(conn, Msg::Release { chunks: vec![id] }, Payload::None);
                if let Some(s) = self.serve.as_mut() {
                    s.end_run(LOCAL, &me);
                }
                let job = self.assess_job.as_ref().unwrap();
                let (testset, p4k) = (job.testset.clone(), job.p4k_id.clone());
                let root = kept.dir.join(format!("x{id}"));
                let rel = |p: &Path| p.strip_prefix(&root).map(|r| r.to_string_lossy().replace('\\', "/")).unwrap_or_default();
                let entries: Vec<TestEntry> = kept
                    .items
                    .iter()
                    .map(|(t, src, rig, pose)| TestEntry { entity: self.targets[*t].entity.clone(), worn: self.targets[*t].worn, package: rel(src.parent().unwrap_or(src)), rig: rel(rig), pose: *pose })
                    .collect();
                if entries.is_empty() {
                    let _ = fs::remove_dir_all(&kept.dir);
                    return self.assess_fail("test set not fetched".into());
                }
                let res = keep_testset(&testset, &root, &p4k, entries);
                let _ = fs::remove_dir_all(&kept.dir);
                match res {
                    Ok(entries) => self.start_sweep(entries),
                    Err(e) => self.assess_fail(format!("test set not kept: {e:#}")),
                }
            }
            Phase::Sweep { rx, .. } => {
                let mut evs = Vec::new();
                loop {
                    match rx.try_recv() {
                        Ok(ev) => evs.push(ev),
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        // the thread ended without a result (it panicked)
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            if !evs.iter().any(|e| matches!(e, SweepEv::Done(_))) {
                                evs.push(SweepEv::Done(Err("the sweep ended without a result".into())));
                            }
                            break;
                        }
                    }
                }
                for ev in evs {
                    match ev {
                        SweepEv::Line(l) => self.assess_note(Level::Info, l),
                        SweepEv::Done(Ok(a)) => return self.finish_assess(*a),
                        SweepEv::Done(Err(e)) => return self.assess_fail(e),
                    }
                }
            }
        }
    }

    fn start_sweep(&mut self, entries: Vec<TestEntry>) {
        // the test set's export-batch is not needed any more: its memory goes back before the sweep measures
        if let Some(s) = self.serve.as_mut() {
            s.quit_idle();
        }
        let Some(job) = self.assess_job.as_mut() else { return };
        let x = job.testset.join("x");
        let items = entries.iter().map(|e| (e.entity.clone(), e.worn, x.join(&e.package).join("scene.blend"), x.join(&e.rig), e.pose)).collect();
        let export = (self.me.export && self.tables_ready && self.max_exporters > 0).then(|| (self.cfg.starbreaker.clone(), self.cfg.p4k.clone()));
        let export_stdin = export.as_ref().is_some_and(|(sb, _)| crate::node::supports_stdin(sb));
        let p = SweepParams {
            blender: self.cfg.blender.clone(),
            script: self.cfg.render_script.clone(),
            size: self.cfg.size,
            chunk: self.cfg.chunk,
            items,
            export,
            export_stdin,
            dir: job.dir.join("sweep"),
            max: self.assess_max,
            keys: Keys::of(&self.me, &self.cfg.render_script),
        };
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let c = cancel.clone();
        let handle = thread::spawn(move || {
            let res = sweep(p, c, tx.clone()).map(Box::new).map_err(|e| format!("{e:#}"));
            let _ = tx.send(SweepEv::Done(res));
        });
        job.phase = Phase::Sweep { rx, cancel, handle: Some(handle) };
    }

    fn end_job(&mut self) -> Option<AssessJob> {
        let mut job = self.assess_job.take()?;
        match &mut job.phase {
            Phase::Sweep { cancel, handle, .. } => {
                cancel.store(true, Ordering::Relaxed);
                if let Some(h) = handle.take() {
                    let _ = h.join();
                }
            }
            // the test set's export chunk: stopped and dropped on its node (here or the exporting node)
            Phase::Export { conn, id, .. } => {
                let (conn, id) = (*conn, *id);
                self.dispatch(conn, Msg::Cancel { id }, Payload::None);
                self.dispatch(conn, Msg::Release { chunks: vec![id] }, Payload::None);
            }
            // its fetch here stops (pending, requests, cache; a test set fetched but not taken yet), the holder drops
            // the chunk
            Phase::Fetch { conn, id } => {
                let (conn, id) = (*conn, *id);
                let me = self.me.name.clone();
                if let Some(s) = self.serve.as_mut() {
                    s.end_run(LOCAL, &me);
                    let _ = s.take_kept(id);
                }
                self.dispatch(conn, Msg::Release { chunks: vec![id] }, Payload::None);
            }
        }
        let _ = fs::remove_dir_all(job.dir.join("sweep"));
        let _ = fs::remove_dir_all(job.testset.with_extension("part"));
        Some(job)
    }

    fn assess_fail(&mut self, why: String) {
        let first = why.lines().next().unwrap_or("").to_string();
        self.assess_note(Level::Error, format!("Assessment failed: {why}"));
        if let Some(c) = self.end_job().and_then(|j| j.requester) {
            self.reply_to(c, Msg::AssessDone { ok: false, text: format!("assessment failed: {first}") });
        }
        self.announce_assess();
    }

    fn finish_assess(&mut self, a: Assessment) {
        for l in table(&a).lines() {
            if let Some(f) = self.assess_job.as_mut().and_then(|j| j.log.as_mut()) {
                let _ = writeln!(f, "{l}");
            }
        }
        let path = self.store.root.join(FILE);
        match save(&path, &a) {
            Ok(()) => self.assess_note(Level::Ok, format!("Assessment saved in {}: {}", path.display(), summary(&a))),
            Err(e) => self.assess_note(Level::Error, format!("Assessment not saved: {e:#}")),
        }
        // the new counts, unless given on the command line
        if !self.fixed_counts.0 {
            self.max_renderers = a.workers;
        }
        if let (false, Some(e), true) = (self.fixed_counts.1, a.exporters, self.me.export) {
            self.max_exporters = e;
        }
        let text = summary(&a);
        self.assessment = Some(a);
        self.assess_stale.clear();
        if let Some(c) = self.end_job().and_then(|j| j.requester) {
            self.reply_to(c, Msg::AssessDone { ok: true, text: format!("assessment done: {text}") });
        }
        self.announce_assess();
    }

    /// On quit: the sweep's processes end
    pub fn stop_assess(&mut self) {
        self.end_job();
    }

    /// The Assessed line of view 3 for this node
    pub fn assess_text(&self) -> String {
        if let Some(j) = self.assess_job.as_ref() {
            let phase = match j.phase {
                Phase::Export { .. } => "exporting the test set",
                Phase::Fetch { .. } => "fetching the test set",
                Phase::Sweep { .. } => "sweep",
            };
            let s = j.started.elapsed().as_secs();
            return format!("assessing … {phase} · {}:{:02}", s / 60, s % 60);
        }
        info_text(self.assess_info().as_ref(), self.me.render)
    }
}

/// The Assessed line for a node's hello: its result with a stale marker
pub fn info_text(a: Option<&AssessInfo>, render: bool) -> String {
    match a {
        _ if !render => "– (does not render)".into(),
        None => "none".into(),
        Some(a) => {
            let export = if a.exporters > 0 { format!(" · {} export {:.1}/s", a.exporters, a.export) } else { String::new() };
            format!("{} {} render {:.1}/s{export} · {}", if a.valid { "✓" } else { "! stale ·" }, a.workers, a.render, a.date)
        }
    }
}

/// One character before a node's workers in view 3: ✓ assessed, ! stale, blank without an assessment
pub fn marker(h: &Hello) -> char {
    match h.assess.as_ref() {
        Some(a) if a.valid => '✓',
        Some(_) => '!',
        None => ' ',
    }
}

/// The assessment log folders (<date>_<time>/) but the latest `keep` go
fn prune_logs(root: &Path, keep: usize) {
    let Ok(rd) = fs::read_dir(root) else { return };
    let mut logs: Vec<PathBuf> = rd.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir()) && e.file_name().to_string_lossy().starts_with(|c: char| c.is_ascii_digit())).map(|e| e.path()).collect();
    logs.sort();
    for d in &logs[..logs.len().saturating_sub(keep)] {
        let _ = fs::remove_dir_all(d);
    }
}

fn read_testset(dir: &Path) -> Option<Vec<TestEntry>> {
    let f: TestSetFile = serde_json::from_slice(&fs::read(dir.join("testset.json")).ok()?).ok()?;
    let x = dir.join("x");
    f.items.iter().all(|e| x.join(&e.package).join("scene.blend").exists() && x.join(&e.rig).exists()).then_some(f.items)
}

/// The exported folder becomes the test set's x/, its list testset.json (written last: a test set without it is
/// incomplete)
fn keep_testset(testset: &Path, root: &Path, p4k_id: &str, entries: Vec<TestEntry>) -> Result<Vec<TestEntry>> {
    let _ = crate::store::remove_tree(testset);
    fs::create_dir_all(testset)?;
    let x = testset.join("x");
    if fs::rename(root, &x).is_err() {
        crate::cluster::copy_tree(root, &x)?;
        let _ = fs::remove_dir_all(root);
    }
    let file = TestSetFile { p4k_id: p4k_id.to_string(), items: entries };
    fs::write(testset.join("testset.json"), serde_json::to_vec_pretty(&file)?)?;
    Ok(file.items)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(n: usize, rate: f64) -> (usize, f64) {
        (n, rate)
    }

    /// The ladder stops under 5 % gain or on a drop; the pick takes the smallest count within 5 % of the best
    #[test]
    fn pick_plateau() {
        assert!(!levelled(5.3, 6.3), "+19 % goes on");
        assert!(levelled(6.3, 6.2), "a drop stops");
        assert!(levelled(6.3, 6.5), "+3 % stops");
        // the dev PC's numbers of 2026-11-02: 6 Blender
        let steps = [step(1, 1.3), step(2, 2.6), step(3, 3.9), step(4, 5.3), step(6, 6.3), step(8, 6.2)];
        assert_eq!(pick(&steps, usize::MAX), Some(6));
        // 8 is the best, but 6 is within 5 % of it
        let steps = [step(4, 5.3), step(6, 6.3), step(8, 6.5)];
        assert_eq!(pick(&steps, usize::MAX), Some(6));
        assert_eq!(pick(&[], usize::MAX), None);
        assert_eq!(pick(&[step(1, 0.0)], usize::MAX), None);
    }

    /// Memory caps the pick: (total × 0.9 − in use) / per process
    #[test]
    fn memory_cap() {
        let gb = 1u64 << 30;
        // 16 GB RAM, 4 GB in use, 2 GB per Blender: (14.4 − 4) / 2 = 5
        assert_eq!(fits(16 * gb, 4 * gb, 2 * gb), 5);
        assert_eq!(fits(16 * gb, 15 * gb, gb), 0);
        assert_eq!(fits(16 * gb, 0, 0), usize::MAX);
        let steps = [step(4, 5.3), step(6, 6.3), step(8, 6.2)];
        assert_eq!(pick(&steps, 5), Some(5), "the memory cap wins over the plateau");
        assert_eq!(pick(&steps, 0), Some(1), "at least one");
    }

    fn keys() -> Keys {
        Keys {
            blender: "5.2.3".into(),
            gpu: Some("RTX 5080 · VRAM 16 GB".into()),
            render_py: Some("r1".into()),
            addon: Some("a1".into()),
            starbreaker: Some("655d5f68".into()),
            host: Some("DRAGONEGG".into()),
            cpu: Some("AMD Ryzen 7 9800X3D 8-Core Processor".into()),
        }
    }

    /// Stale when any key differs: Blender, GPU, render.py, add-on, StarBreaker, host, CPU (a
    /// assessment.json copied to a machine without nvidia-smi is stale there); a peer's p4k fingerprint stays one
    /// folder name
    #[test]
    fn staleness() {
        let k = keys();
        assert!(k.diff(&k).is_empty());
        let copied = Keys { host: Some("DRAGONVR".into()), cpu: Some("Intel(R) Core(TM) i7-4790K CPU @ 4.00GHz".into()), gpu: None, ..k.clone() };
        assert_eq!(k.diff(&copied).len(), 3, "{:?}", k.diff(&copied));
        assert!(k.diff(&copied)[1].starts_with("host DRAGONVR (assessed DRAGONEGG)"));
        let key = testset_key("../../..//x", &["a"]);
        assert!(crate::net::safe_name(&format!("testset-{key}")) && key.starts_with("x-"), "{key}");
        let other = Keys { blender: "5.2.4".into(), ..k.clone() };
        assert_eq!(k.diff(&other), vec!["Blender 5.2.4 (assessed 5.2.3)".to_string()]);
        let other = Keys { gpu: None, render_py: Some("r2".into()), ..k.clone() };
        assert_eq!(k.diff(&other).len(), 2);
        let a = Assessment { workers: 6, render: 6.3, date: "2026-11-04T14:02:11+01:00".into(), keys: k, ..Default::default() };
        let info = a.info(false);
        assert!(!info.valid && info.date == "2026-11-04 14:02");
        let h = Hello { assess: Some(info.clone()), ..Default::default() };
        assert_eq!(marker(&h), '!');
        assert!(info_text(Some(&info), true).starts_with("! stale · 6 render 6.3/s"));
        assert_eq!(marker(&Hello::default()), ' ');
    }

    /// The assessment has a file of its own: packshot.json (token, peers) is not touched; a second save replaces it
    #[test]
    fn own_file() {
        let dir = std::env::temp_dir().join(format!("packshot-assess-json-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let cfg = r#"{"token": "s3cret", "peers": ["servbox"], "discovery": false}"#;
        fs::write(dir.join("packshot.json"), cfg).unwrap();
        let path = dir.join(FILE);
        assert_eq!(load(&path), None);
        let a = Assessment { workers: 6, render: 6.3, keys: keys(), ..Default::default() };
        save(&path, &a).unwrap();
        assert_eq!(load(&path), Some(a.clone()));
        let b = Assessment { workers: 4, ..a };
        save(&path, &b).unwrap();
        assert_eq!(load(&path).unwrap().workers, 4);
        assert!(!dir.join("assessment.json.tmp").exists());
        assert_eq!(fs::read_to_string(dir.join("packshot.json")).unwrap(), cfg, "packshot.json untouched");
    }

    /// Old assessment log folders go, the latest KEEP_LOGS and the test sets stay
    #[test]
    fn old_logs_pruned() {
        let dir = std::env::temp_dir().join(format!("packshot-assess-logs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for d in ["2026-11-01_10-00-00", "2026-11-02_10-00-00", "2026-11-03_10-00-00", "2026-11-04_09-00-00", "2026-11-04_10-00-00", "testset-abc-0001"] {
            fs::create_dir_all(dir.join(d)).unwrap();
        }
        prune_logs(&dir, 3);
        let mut left: Vec<String> = fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        left.sort();
        assert_eq!(left, vec!["2026-11-03_10-00-00", "2026-11-04_09-00-00", "2026-11-04_10-00-00", "testset-abc-0001"]);
        let _ = fs::remove_dir_all(&dir);
    }

    /// The test set: proportional to the types, evenly within a type, the same for the same list
    #[test]
    fn test_set_stratified() {
        let t = |kind: &str, i: usize| Target { uuids: vec![], names: None, entity: format!("{kind}_{i:03}"), kind: kind.into(), sub_type: String::new(), worn: false };
        let mut targets: Vec<Target> = (0..600).map(|i| t("Char_Armor_Helmet", i)).collect();
        targets.extend((0..300).map(|i| t("WeaponPersonal", i)));
        targets.extend((0..90).map(|i| t("Cooler", i)));
        targets.extend((0..10).map(|i| t("Bomb", i)));
        let set = test_set(&targets, 48);
        assert_eq!(set.len(), 48);
        let count = |k: &str| set.iter().filter(|&&i| targets[i].kind == k).count();
        assert_eq!((count("Char_Armor_Helmet"), count("WeaponPersonal"), count("Cooler"), count("Bomb")), (29, 14, 4, 1));
        assert_eq!(test_set(&targets, 48), set, "deterministic");
        let mut reversed = targets.clone();
        reversed.reverse();
        let names = |ts: &[Target], s: &[usize]| {
            let mut v: Vec<String> = s.iter().map(|&i| ts[i].entity.clone()).collect();
            v.sort();
            v
        };
        assert_eq!(names(&reversed, &test_set(&reversed, 48)), names(&targets, &set), "independent of the list's order");
        assert_eq!(test_set(&targets[..20], 48).len(), 20);
    }
}
