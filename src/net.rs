// Network in the cluster: TCP, one frame per message made of u32 JSON length, JSON, u64 payload length and payload (LE).
// Files (exported items' files between nodes, render.py, runs) go one after another as the payload, their paths and sizes are in the JSON;
// the reader thread writes them straight into an inbox folder, the main thread only gets the folder. Every
// connection starts with Hello (token, capabilities, versions); the accepting side checks the token and answers with
// Welcome. Afterwards both sides can send requests. The hub holds the connections of a process: listen, discovery
// (a UDP beacon every 2 s on every interface's broadcast address, the same port as TCP), dial (the discovered nodes
// and the peers of the config), ping, resolve duplicate connections. Every node connects to every other one.
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::config::{DEFAULT_PORT, NetConfig};
use crate::engine::Target;

/// Free and total space of the disk that holds a folder of the node (the packshot folder, the work folder if elsewhere)
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct DiskInfo {
    /// mount point ("/mnt/DATA", "D:\\")
    pub mount: String,
    pub free: u64,
    pub total: u64,
}

impl DiskInfo {
    /// The share in use: "78%"
    pub fn used_pct(&self) -> String {
        if self.total == 0 {
            return "?".into();
        }
        format!("{:.0}%", (self.total - self.free.min(self.total)) as f64 * 100.0 / self.total as f64)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Hello {
    #[serde(default)]
    pub token: String,
    pub name: String,
    /// random per process: tells this process from another one (a peer address that leads back to this node)
    #[serde(default)]
    pub instance: u64,
    pub version: String,
    pub os: String,
    /// capabilities from the start (--render, --export); the counts below change at runtime
    #[serde(default)]
    pub render: bool,
    #[serde(default)]
    pub export: bool,
    /// Blender version (from the folder name), empty without Blender
    pub blender: String,
    /// Hash of the StarBreaker addon (events::tree_hash)
    pub addon: Option<String>,
    /// Blender processes the node runs at the same time (0 without --render)
    pub slots: usize,
    /// export-batch processes at the same time (0 without --export)
    #[serde(default)]
    pub exporters: usize,
    pub cores: usize,
    /// head node of a run going on: the nodes work on one run at a time
    #[serde(default)]
    pub running: bool,
    /// Size of Data.p4k (nodes with --export)
    #[serde(default)]
    pub p4k_bytes: Option<u64>,
    /// fingerprint of Data.p4k (size + build_manifest.id, cluster::p4k_id): what the state check compares
    #[serde(default)]
    pub p4k_id: Option<String>,
    /// game version from build_manifest.id next to Data.p4k ("4.10.2 (4.10.197.2878)"), shown only
    #[serde(default)]
    pub p4k_version: Option<String>,
    /// StarBreaker commit (nodes with --export): part of the state the exporting nodes must share
    #[serde(default)]
    pub starbreaker: Option<String>,
    /// CPU name and total RAM (bytes), shown in the details only
    #[serde(default)]
    pub cpu: Option<String>,
    #[serde(default)]
    pub ram: Option<u64>,
    /// GPU name and memory from nvidia-smi ("RTX 5080 VRAM 16 GB"), several joined with +
    #[serde(default)]
    pub gpu: Option<String>,
    /// disks of the packshot and work folder, updated while running
    #[serde(default)]
    pub disks: Vec<DiskInfo>,
    /// the node's compute assessment (assess.rs): what a run's head node sizes its chunks by
    #[serde(default)]
    pub assess: Option<AssessInfo>,
    /// an assessment is going there: it takes no work
    #[serde(default)]
    pub assessing: bool,
}

/// A node's assessment as the others see it: items/s and process counts it was measured with, whether it is still
/// valid (same packshot, Blender, GPU, render.py, add-on, StarBreaker) and when
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct AssessInfo {
    pub render: f64,
    pub export: f64,
    pub workers: usize,
    pub exporters: usize,
    pub valid: bool,
    pub date: String,
    /// the steps it was measured at: (Blender count, items/s) and (export count, items/s), so the head node can take
    /// the rate at the count the node runs now
    #[serde(default)]
    pub steps: Vec<(usize, f64)>,
    #[serde(default)]
    pub export_steps: Vec<(usize, f64)>,
}

impl Hello {
    /// "render + export", "render", "export" or "–" (starts, stores, watches)
    pub fn caps(&self) -> &'static str {
        match (self.render, self.export) {
            (true, true) => "render + export",
            (true, false) => "render",
            (false, true) => "export",
            (false, false) => "–",
        }
    }

    /// "/mnt/DATA: 412 / 1863 GB free (78%)" per disk (the share in use), joined with " · "
    pub fn disk_text(&self) -> String {
        let gb = |b: u64| b as f64 / 1e9;
        let parts: Vec<String> = self.disks.iter().map(|d| format!("{}: {:.0} / {:.0} GB free ({})", d.mount, gb(d.free), gb(d.total), d.used_pct())).collect();
        if parts.is_empty() { "?".into() } else { parts.join(" · ") }
    }

    /// Workers per type: "2 export · 6 render", "–" for a node that only starts, stores and watches
    pub fn workers_text(&self) -> String {
        let mut parts = Vec::new();
        if self.export {
            parts.push(format!("{} export", self.exporters));
        }
        if self.render {
            parts.push(format!("{} render", self.slots));
        }
        if parts.is_empty() { "–".into() } else { parts.join(" · ") }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
}

/// An entity for an exporting node to export; pose and rig come from the exporting node's own DataCore tables (it
/// fills them in on arrival, `rig` then is the path of its rig file there)
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ItemJob {
    pub item: usize,
    pub entity: String,
    pub worn: bool,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub sub_type: String,
    #[serde(default)]
    pub pose: [f64; 3],
    #[serde(default)]
    pub rig: String,
}

/// An exported item for a render chunk: which node holds it, in which of its export chunks
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RenderJob {
    pub item: usize,
    pub entity: String,
    pub holder: String,
    pub chunk: u64,
}

/// An exported item as its holder describes it: package folder, pose and rig (paths relative to the export chunk's
/// folder) and every file it needs there with its hash
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct HeldItem {
    pub item: usize,
    pub package: String,
    pub pose: [f64; 3],
    pub rig: String,
    pub files: Vec<crate::cas::HashedFile>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RunSummary {
    pub id: String,
    pub name: String,
    pub set: String,
    pub node: String,
    pub finished: bool,
    pub images: usize,
    pub bytes: u64,
}

/// A worker of a run, as the head node shows it to the other nodes
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct WorkerStatus {
    pub label: String,
    pub phase: String,
    pub entity: String,
    pub done: usize,
    pub total: usize,
    pub secs: f64,
}

/// A run as its head node reports it to the other nodes once a second (and once at the end)
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct RunStatus {
    pub run: String,
    pub head: String,
    pub starter: String,
    pub set: String,
    pub filter: String,
    pub scope: String,
    pub total: usize,
    pub ok: usize,
    pub failed: usize,
    pub exported: usize,
    pub pending: usize,
    pub ready: usize,
    /// exported items not rendered yet, and per node holding them
    #[serde(default)]
    pub held: usize,
    #[serde(default)]
    pub holders: Vec<(String, usize)>,
    /// per node: images rendered there and its items/s over the last 2 minutes (view 3)
    #[serde(default)]
    pub nodes: Vec<(String, usize, f64)>,
    pub elapsed: f64,
    /// renders per minute over the last 2 minutes
    pub rate: f64,
    /// Blenders in the cluster
    pub slots: usize,
    pub paused: bool,
    pub finished: bool,
    pub aborted: bool,
    pub workers: Vec<WorkerStatus>,
    /// the latest failures: (entity, error)
    pub errors: Vec<(String, String)>,
    pub phase_avg: Option<[f64; 5]>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Msg {
    // boxed: a Hello and a RunStatus are many times the size of every other message
    Hello(Box<Hello>),
    Welcome(Box<Hello>),
    Reject { reason: String },
    Ping,
    /// head node -> exporting node: entities to export (by one export-batch process); the node holds what it exported
    /// until the head node releases it
    Export { id: u64, jobs: Vec<ItemJob> },
    /// exporting node -> head node: an item of an export chunk is exported (or failed)
    ItemExported { chunk: u64, item: usize, ok: bool, seconds: f64, error: String, worn: Option<bool> },
    /// exporting node -> head node: the export chunk ended (chunk_done, or its process ended); items without a line failed (`error` says why, with
    /// its last lines), `failed` are items that failed after their line
    ExportDone { id: u64, exit: Option<i32>, wall: f64, item_seconds: f64, error: String, long_paths: usize, failed: Vec<(usize, String)> },
    /// head node -> holder: these export chunks are done (every item rendered or failed), their folders can go
    Release { chunks: Vec<u64> },
    /// head node -> rendering node: render these items (by one Blender), fetching what is held elsewhere straight
    /// from its holder (files: render.py)
    Render { id: u64, size: u32, jobs: Vec<RenderJob>, files: Vec<FileEntry> },
    /// rendering node -> head node: the render chunk's files are here (from which holders, bytes fetched and taken
    /// from the cache, how long it took)
    Fetched { id: u64, holders: Vec<String>, bytes: u64, cached: u64, files: usize, secs: f64 },
    /// rendering node -> head node: these items of a render chunk did not get their files: `export` (the holder no
    /// longer has a file): export them again; else (the holder is not reachable from here) the head node places them
    /// again, into the pool while it still has their holder. Sent before the chunk's Fetched or BlockDone
    Unfetched { block: u64, items: Vec<usize>, reason: String, export: bool },
    /// rendering node -> holder: describe these items of an export chunk of `head`'s run
    GetItems { head: String, chunk: u64, items: Vec<usize> },
    /// holder -> rendering node: the items with their files and hashes, and the ones it does not hold (why);
    /// `gone`: it does not hold the export chunk at all (restarted, or released already): export them again
    ItemsInfo {
        head: String,
        chunk: u64,
        items: Vec<HeldItem>,
        missing: Vec<(usize, String)>,
        #[serde(default)]
        gone: bool,
    },
    /// rendering node -> holder: send the files with these hashes
    Fetch { id: u64, hashes: Vec<String> },
    /// holder -> rendering node: the files (named by their hash) and the hashes it does not have
    FetchData { id: u64, files: Vec<FileEntry>, missing: Vec<String> },
    Cancel { id: u64 },
    /// rendering node -> head node: an item done (payload: the PNG) or failed
    Item { block: u64, item: usize, ok: bool, info: String },
    /// rendering node -> head node: the render chunk's Blender ended (error: why it ended early, with its last lines)
    BlockDone { id: u64, exit: Option<i32>, error: String },
    /// starting node -> head node: the sets there and the state and target list of one set (for the selection)
    GetState { set: String },
    State { set: String, sets: Vec<crate::store::SetSummary>, images: Vec<String>, failed: Vec<String>, targets: Vec<Target>, stats: Option<crate::store::RunStats> },
    /// starting node -> head node: start a run over these targets into the set (created from live if missing)
    StartRun { targets: Vec<Target>, set: String, name: String, filter: String, scope: String, line: String },
    Started { run: String },
    /// any node -> head node: abort the run
    AbortRun,
    /// head node -> all: the run's progress
    Status(Box<RunStatus>),
    /// between all nodes: runs and manifests
    ListRuns,
    Runs { runs: Vec<RunSummary> },
    GetRun { id: String },
    RunFiles { id: String, files: Vec<FileEntry> },
    GetManifest { set: String },
    Manifest { set: String, from: String, entities: BTreeMap<String, bool> },
    /// any node -> node: assess yourself (refused with Error while a run is going or the node is busy)
    Assess,
    /// assessed node -> the node that asked: the assessment is over
    AssessDone { ok: bool, text: String },
    /// stop the assessment going on there
    AssessAbort,
    Error { what: String },
}

pub enum Payload {
    None,
    Bytes(Vec<u8>),
    /// Files in the order of the FileEntry list
    Files(Vec<PathBuf>),
}

pub struct Out {
    pub msg: Msg,
    pub payload: Payload,
    /// a folder to delete once the frame is written (files sent from it)
    pub cleanup: Option<PathBuf>,
}

/// What the hub reports to the main thread
pub enum HubEv {
    Up { conn: u64 },
    Down { conn: u64, name: String, error: String },
    Msg { conn: u64, msg: Msg, data: Vec<u8>, dir: Option<PathBuf> },
}

enum NetEv {
    Up { conn: u64, hello: Box<Hello>, outbound: bool, addr: String, tx: Sender<Out>, stream: TcpStream },
    Msg { conn: u64, msg: Msg, data: Vec<u8>, dir: Option<PathBuf> },
    Down { conn: u64, error: String },
    DialFailed { addr: String, error: String },
    /// a beacon of another packshot with the same token: its address (sender IP, its port) and host name
    Beacon { addr: String, name: String },
}

/// The discovery beacon (UDP, JSON): which cluster (hash of the token, never the token itself), who and on which port
#[derive(Serialize, Deserialize)]
struct Beacon {
    packshot: String,
    cluster: String,
    name: String,
    instance: u64,
    port: u16,
}

/// FNV-1a of the token: tells clusters apart in the beacon without sending the token
fn cluster_id(token: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in token.trim().as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// A random number per process (time and process id mixed), for Hello.instance
pub fn new_instance() -> u64 {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1);
    let mut h = t ^ (u64::from(std::process::id()) << 32) ^ 0x9e37_79b9_7f4a_7c15;
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 33;
    h.max(1)
}

/// Broadcast address of every IPv4 network of the node (without loopback and /32)
fn broadcast_addrs() -> Vec<Ipv4Addr> {
    let Ok(nets) = sysinfo::Networks::new_with_refreshed_list() else { return Vec::new() };
    let mut out: Vec<Ipv4Addr> = nets
        .values()
        .flat_map(|n| n.ip_networks().iter())
        .filter_map(|n| match n.addr {
            IpAddr::V4(a) if !a.is_loopback() && !a.is_link_local() && n.prefix < 32 => {
                let mask = u32::MAX.checked_shl(32 - u32::from(n.prefix)).unwrap_or(0);
                Some(Ipv4Addr::from(u32::from(a) | !mask))
            }
            _ => None,
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// This node's IP address: the local address of the interface with the default route (a connected UDP socket sends
/// nothing)
pub fn own_ip() -> Option<String> {
    // looked up once (the TUI asks every frame)
    static IP: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    IP.get_or_init(|| {
        let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
        s.connect("192.0.2.1:9").ok()?;
        Some(s.local_addr().ok()?.ip().to_string())
    })
    .clone()
}

/// Listeners test_port() bound, by port: Hub::listen takes its listener from here, so a port stays held from the
/// check to the use (no other test or test process can take it in between)
#[cfg(test)]
static TEST_LISTENERS: std::sync::Mutex<BTreeMap<u16, TcpListener>> = std::sync::Mutex::new(BTreeMap::new());

/// A listen port for a test (127.0.0.1): from the block above the cluster port (DEFAULT_PORT + 1 ..= + TEST_PORTS),
/// never DEFAULT_PORT itself (a production node on this machine listens there) and never an ephemeral one. Handed out
/// in turn by a counter shared by the tests of the process (they run in parallel threads), starting at an offset from
/// the process id (several test processes at once); each candidate is bound here and skipped while taken, the
/// listener held for the Hub listening on it. 249 ports: a test process keeps ~40 (a Hub's accept thread holds its
/// listener until the process ends), so a few test processes fit at once
#[cfg(test)]
pub fn test_port() -> u16 {
    use std::sync::atomic::{AtomicUsize, Ordering};
    const TEST_PORTS: u16 = 249;
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let offset = std::process::id() as usize;
    for _ in 0..TEST_PORTS {
        let k = (NEXT.fetch_add(1, Ordering::Relaxed).wrapping_add(offset) % usize::from(TEST_PORTS)) as u16;
        let port = crate::config::DEFAULT_PORT + 1 + k;
        if let Ok(l) = TcpListener::bind(("127.0.0.1", port)) {
            TEST_LISTENERS.lock().unwrap().insert(port, l);
            return port;
        }
    }
    let first = crate::config::DEFAULT_PORT + 1;
    panic!("no free test port in 127.0.0.1:{first}-{}: all {TEST_PORTS} taken", first + TEST_PORTS - 1);
}

/// Relative path from the network: normal components only, no drive, no ..; on Windows no ':' either (an
/// alternate data stream, "a:b")
pub fn safe_rel(p: &str) -> Option<PathBuf> {
    if cfg!(windows) && p.contains(':') {
        return None;
    }
    let path = Path::new(p);
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// A single folder or file name from the network (a run id, a set name): one normal component
pub fn safe_name(s: &str) -> bool {
    safe_rel(s).is_some_and(|p| p.components().count() == 1)
}

/// A content hash from the network (it names a file in the cache): 32 lower-case hex digits
pub fn safe_hash(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn write_frame(w: &mut impl Write, msg: &Msg, payload: &Payload) -> io::Result<()> {
    let json = serde_json::to_vec(msg).map_err(io::Error::other)?;
    w.write_all(&(json.len() as u32).to_le_bytes())?;
    w.write_all(&json)?;
    match payload {
        Payload::None => w.write_all(&0u64.to_le_bytes())?,
        Payload::Bytes(b) => {
            w.write_all(&(b.len() as u64).to_le_bytes())?;
            w.write_all(b)?;
        }
        Payload::Files(files) => {
            let mut total = 0u64;
            let mut sizes = Vec::new();
            for f in files {
                let n = fs::metadata(f)?.len();
                sizes.push(n);
                total += n;
            }
            w.write_all(&total.to_le_bytes())?;
            for (f, n) in files.iter().zip(sizes) {
                let copied = io::copy(&mut File::open(f)?.take(n), w)?;
                if copied != n {
                    return Err(io::Error::other(format!("{} changed while sending", f.display())));
                }
            }
        }
    }
    w.flush()
}

/// Reads a frame; file lists land under `inbox`/<n>/, otherwise the payload comes back as bytes
fn read_frame(r: &mut impl Read, inbox: &Path, seq: &mut u64) -> Result<(Msg, Vec<u8>, Option<PathBuf>)> {
    let mut b4 = [0u8; 4];
    r.read_exact(&mut b4)?;
    let len = u32::from_le_bytes(b4) as usize;
    if len > 64 << 20 {
        bail!("frame too large ({len} bytes)");
    }
    let mut json = vec![0u8; len];
    r.read_exact(&mut json)?;
    let msg: Msg = serde_json::from_slice(&json).context("message")?;
    let mut b8 = [0u8; 8];
    r.read_exact(&mut b8)?;
    let total = u64::from_le_bytes(b8);
    let files = match &msg {
        Msg::Render { files, .. } | Msg::FetchData { files, .. } | Msg::RunFiles { files, .. } => Some(files.clone()),
        _ => None,
    };
    let Some(files) = files else {
        if total > 256 << 20 {
            bail!("payload too large ({total} bytes)");
        }
        let mut data = vec![0u8; total as usize];
        r.read_exact(&mut data)?;
        return Ok((msg, data, None));
    };
    if files.iter().map(|f| f.size).sum::<u64>() != total {
        bail!("payload size does not match the file list");
    }
    *seq += 1;
    let dir = inbox.join(format!("{}-{}", std::process::id(), seq));
    let _ = fs::remove_dir_all(&dir);
    for f in &files {
        let rel = safe_rel(&f.path).ok_or_else(|| anyhow!("unsafe path {}", f.path))?;
        let path = dir.join(rel);
        if let Some(p) = path.parent() {
            fs::create_dir_all(p)?;
        }
        let mut out = BufWriter::new(File::create(&path).with_context(|| path.display().to_string())?);
        let n = io::copy(&mut r.take(f.size), &mut out)?;
        out.flush()?;
        if n != f.size {
            bail!("connection closed during {}", f.path);
        }
    }
    Ok((msg, Vec::new(), Some(dir)))
}

/// Files of a folder as a list (paths with /) plus the source paths, without __pycache__
pub fn collect_files(root: &Path, prefix: &str) -> Result<(Vec<FileEntry>, Vec<PathBuf>)> {
    let mut entries = Vec::new();
    let mut paths = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in fs::read_dir(&dir).with_context(|| dir.display().to_string())?.flatten() {
            let p = e.path();
            if p.is_dir() {
                if e.file_name() != "__pycache__" {
                    stack.push(p);
                }
                continue;
            }
            let rel = p.strip_prefix(root)?.to_string_lossy().replace('\\', "/");
            entries.push(FileEntry { path: format!("{prefix}{rel}"), size: fs::metadata(&p)?.len() });
            paths.push(p);
        }
    }
    Ok((entries, paths))
}

pub struct PeerConn {
    pub conn: u64,
    pub hello: Hello,
    pub outbound: bool,
    pub addr: String,
    /// the peer's IP address on this connection
    pub ip: String,
    tx: Sender<Out>,
    stream: TcpStream,
}

impl PeerConn {
    pub fn send(&self, msg: Msg, payload: Payload) {
        let _ = self.tx.send(Out { msg, payload, cleanup: None });
    }
    pub fn close(&self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

pub struct Hub {
    pub cfg: NetConfig,
    pub me: Hello,
    pub peers: Vec<PeerConn>,
    /// addresses (peers of the config, discovered nodes): last error while dialing
    pub dial_errors: BTreeMap<String, String>,
    /// address -> host name of the node that answered there
    pub addr_names: BTreeMap<String, String>,
    /// nodes heard by discovery: address -> (host name, last beacon)
    pub discovered: BTreeMap<String, (String, Instant)>,
    /// addresses where this process answered itself
    self_addrs: HashSet<String>,
    pub listen_error: Option<String>,
    pub discovery_error: Option<String>,
    tx: Sender<NetEv>,
    rx: Receiver<NetEv>,
    inbox: PathBuf,
    next_conn: u64,
    /// addresses being dialed
    dialing: HashSet<String>,
    last_dial: Option<Instant>,
    last_ping: Instant,
}

/// Set up a connection: reader and writer thread, Up to the main thread
fn run_conn(stream: TcpStream, conn: u64, hello: Hello, outbound: bool, addr: String, inbox: PathBuf, ev: Sender<NetEv>) {
    let (tx, rx) = channel::<Out>();
    let Ok(wstream) = stream.try_clone() else { return };
    let Ok(ctl) = stream.try_clone() else { return };
    thread::spawn(move || {
        let mut w = BufWriter::with_capacity(1 << 20, wstream);
        for out in rx {
            let res = write_frame(&mut w, &out.msg, &out.payload);
            if let Some(d) = out.cleanup.as_ref() {
                let _ = fs::remove_dir_all(d);
            }
            if res.is_err() {
                let _ = w.get_ref().shutdown(Shutdown::Both);
                return;
            }
        }
    });
    if ev.send(NetEv::Up { conn, hello: Box::new(hello), outbound, addr, tx, stream: ctl }).is_err() {
        return;
    }
    let mut r = BufReader::with_capacity(1 << 20, stream);
    let mut seq = conn * 1_000_000;
    loop {
        match read_frame(&mut r, &inbox, &mut seq) {
            Ok((msg, data, dir)) => {
                if ev.send(NetEv::Msg { conn, msg, data, dir }).is_err() {
                    return;
                }
            }
            Err(e) => {
                let _ = ev.send(NetEv::Down { conn, error: format!("{e:#}") });
                return;
            }
        }
    }
}

/// Binds `addr`; in a test the listener test_port() holds for its port
fn bind_listener(addr: &str, port: u16) -> io::Result<TcpListener> {
    #[cfg(test)]
    if addr.starts_with("127.0.0.1:") && let Some(l) = TEST_LISTENERS.lock().unwrap().remove(&port) {
        return Ok(l);
    }
    let _ = port;
    TcpListener::bind(addr)
}

impl Hub {
    /// Listens and announces itself when the config has a token (and discovery is on)
    pub fn new(cfg: NetConfig, mut me: Hello, inbox: PathBuf) -> Self {
        let (tx, rx) = channel();
        let _ = fs::create_dir_all(&inbox);
        if me.instance == 0 {
            me.instance = new_instance();
        }
        let mut hub = Self {
            cfg,
            me,
            peers: Vec::new(),
            dial_errors: BTreeMap::new(),
            addr_names: BTreeMap::new(),
            discovered: BTreeMap::new(),
            self_addrs: HashSet::new(),
            listen_error: None,
            discovery_error: None,
            tx,
            rx,
            inbox,
            next_conn: 1,
            dialing: HashSet::new(),
            last_dial: None,
            last_ping: Instant::now(),
        };
        if hub.cfg.enabled() {
            hub.listen();
            if hub.cfg.discovery {
                hub.discover();
            }
        }
        hub
    }

    /// Discovery: a beacon every 2 s to the broadcast address of every IPv4 network (the list refreshed every 30 s),
    /// beacons of other processes with the same token become dial candidates
    fn discover(&mut self) {
        let port = self.cfg.port();
        let beacon = Beacon { packshot: "beacon".into(), cluster: cluster_id(&self.cfg.token), name: self.me.name.clone(), instance: self.me.instance, port };
        let Ok(payload) = serde_json::to_vec(&beacon) else { return };
        // receiving needs the port; if another process holds it, this one only sends
        let (sock, receiving) = match UdpSocket::bind(("0.0.0.0", port)) {
            Ok(s) => (s, true),
            Err(e) => {
                self.discovery_error = Some(format!("discovery on UDP {port}: {e} (only sending)"));
                match UdpSocket::bind(("0.0.0.0", 0)) {
                    Ok(s) => (s, false),
                    Err(e) => {
                        self.discovery_error = Some(format!("discovery: {e}"));
                        return;
                    }
                }
            }
        };
        let _ = sock.set_broadcast(true);
        let Ok(send) = sock.try_clone() else { return };
        thread::spawn(move || {
            let mut targets = Vec::new();
            let mut listed: Option<Instant> = None;
            loop {
                if listed.is_none_or(|t| t.elapsed() > Duration::from_secs(30)) {
                    targets = broadcast_addrs();
                    listed = Some(Instant::now());
                }
                for ip in &targets {
                    let _ = send.send_to(&payload, SocketAddr::from((*ip, port)));
                }
                thread::sleep(Duration::from_secs(2));
            }
        });
        if !receiving {
            return;
        }
        let (ev, cluster, instance) = (self.tx.clone(), beacon.cluster, beacon.instance);
        thread::spawn(move || {
            let mut buf = [0u8; 2048];
            loop {
                let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
                let Ok(b) = serde_json::from_slice::<Beacon>(&buf[..n]) else { continue };
                if b.packshot != "beacon" || b.cluster != cluster || b.instance == instance {
                    continue;
                }
                if ev.send(NetEv::Beacon { addr: format!("{}:{}", from.ip(), b.port), name: b.name }).is_err() {
                    return;
                }
            }
        });
    }

    fn listen(&mut self) {
        let addr = self.cfg.listen.clone();
        let listener = match bind_listener(&addr, self.cfg.port()) {
            Ok(l) => l,
            Err(e) => {
                self.listen_error = Some(format!("listen on {addr}: {e}"));
                return;
            }
        };
        let (ev, token, me, inbox) = (self.tx.clone(), self.cfg.token.clone(), self.me.clone(), self.inbox.clone());
        // connection numbers of the accepted connections: upper range, so they do not collide with the dialed ones
        thread::spawn(move || {
            for (conn, stream) in ((1u64 << 32) + 1..).zip(listener.incoming().flatten()) {
                let (ev, token, me, inbox) = (ev.clone(), token.clone(), me.clone(), inbox.clone());
                thread::spawn(move || {
                    let addr = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(45)));
                    let _ = stream.set_nodelay(true);
                    let mut r = BufReader::new(match stream.try_clone() {
                        Ok(s) => s,
                        Err(_) => return,
                    });
                    let mut w = match stream.try_clone() {
                        Ok(s) => s,
                        Err(_) => return,
                    };
                    let mut seq = 0;
                    let hello = match read_frame(&mut r, &inbox, &mut seq) {
                        Ok((Msg::Hello(h), _, _)) if h.token == token => *h,
                        Ok((Msg::Hello(_), _, _)) => {
                            let _ = write_frame(&mut w, &Msg::Reject { reason: "wrong token".into() }, &Payload::None);
                            return;
                        }
                        _ => return,
                    };
                    let mut welcome = me.clone();
                    welcome.token.clear();
                    if write_frame(&mut w, &Msg::Welcome(Box::new(welcome)), &Payload::None).is_err() {
                        return;
                    }
                    run_conn(stream, conn, hello, false, addr, inbox, ev);
                });
            }
        });
    }

    /// Addresses to connect to: the peers of the config and the nodes heard in the last 15 s
    fn dial_targets(&self) -> Vec<String> {
        let mut out = self.cfg.peers.clone();
        for (addr, (_, seen)) in &self.discovered {
            if seen.elapsed() < Duration::from_secs(15) && !out.contains(addr) {
                out.push(addr.clone());
            }
        }
        out
    }

    /// Dial the nodes that are not connected (every 3 s)
    fn dial(&mut self) {
        if !self.cfg.enabled() || self.last_dial.is_some_and(|t| t.elapsed() < Duration::from_secs(3)) {
            return;
        }
        self.last_dial = Some(Instant::now());
        for addr in self.dial_targets() {
            // known node behind the address: this one itself, or already connected (in either direction)
            let name = self.addr_names.get(&addr).or_else(|| self.discovered.get(&addr).map(|(n, _)| n));
            let known = self.self_addrs.contains(&addr) || name.is_some_and(|n| self.peers.iter().any(|p| p.hello.name == *n));
            if known || self.dialing.contains(&addr) || self.peers.iter().any(|p| p.outbound && p.addr == addr) {
                continue;
            }
            self.dialing.insert(addr.clone());
            let conn = self.next_conn;
            self.next_conn += 1;
            let (ev, inbox) = (self.tx.clone(), self.inbox.clone());
            let mut me = self.me.clone();
            me.token = self.cfg.token.clone();
            thread::spawn(move || {
                let res = (|| -> Result<(TcpStream, Hello)> {
                    // host name or IP, the port is optional
                    let target = if addr.rsplit_once(':').is_some_and(|(_, p)| p.parse::<u16>().is_ok()) { addr.clone() } else { format!("{addr}:{DEFAULT_PORT}") };
                    // every resolved address in turn: names often resolve to IPv6 first while the node listens on IPv4
                    let mut last = anyhow!("no address for {addr}");
                    let mut connected = None;
                    for sa in target.to_socket_addrs()? {
                        match TcpStream::connect_timeout(&sa, Duration::from_secs(3)) {
                            Ok(s) => {
                                connected = Some(s);
                                break;
                            }
                            Err(e) => last = e.into(),
                        }
                    }
                    let stream = connected.ok_or(last)?;
                    stream.set_read_timeout(Some(Duration::from_secs(45)))?;
                    let _ = stream.set_nodelay(true);
                    let mut w = stream.try_clone()?;
                    write_frame(&mut w, &Msg::Hello(Box::new(me)), &Payload::None)?;
                    let mut r = BufReader::new(stream.try_clone()?);
                    let mut seq = 0;
                    match read_frame(&mut r, &inbox, &mut seq)? {
                        (Msg::Welcome(h), _, _) => Ok((stream, *h)),
                        (Msg::Reject { reason }, _, _) => bail!("rejected: {reason}"),
                        _ => bail!("unexpected answer"),
                    }
                })();
                match res {
                    Ok((stream, hello)) => run_conn(stream, conn, hello, true, addr, inbox, ev),
                    Err(e) => {
                        let _ = ev.send(NetEv::DialFailed { addr, error: format!("{e:#}") });
                    }
                }
            });
        }
    }

    /// One tick: dial, ping, collect events. The hub manages the connections itself
    pub fn poll(&mut self) -> Vec<HubEv> {
        self.dial();
        if self.last_ping.elapsed() > Duration::from_secs(10) {
            self.last_ping = Instant::now();
            for p in &self.peers {
                p.send(Msg::Ping, Payload::None);
            }
        }
        let mut out = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                NetEv::Up { conn, mut hello, outbound, addr, tx, stream } => {
                    hello.token.clear();
                    if outbound {
                        self.dialing.remove(&addr);
                        self.dial_errors.remove(&addr);
                        self.addr_names.insert(addr.clone(), hello.name.clone());
                    }
                    // an address that leads back to this process (a peer of the config pointing at this node)
                    if hello.instance == self.me.instance {
                        if outbound {
                            self.self_addrs.insert(addr.clone());
                        }
                        let _ = stream.shutdown(Shutdown::Both);
                        continue;
                    }
                    // duplicate connection: if both sides dialed, the one set up by the smaller name stays;
                    // otherwise the new one (the old one is then usually dead)
                    if let Some(i) = self.peers.iter().position(|p| p.hello.name == hello.name) {
                        let initiator = |ob: bool| if ob { self.me.name.clone() } else { hello.name.clone() };
                        let (new_init, old_init) = (initiator(outbound), initiator(self.peers[i].outbound));
                        let keep_new = new_init == old_init || new_init == self.me.name.clone().min(hello.name.clone());
                        if keep_new {
                            let old = self.peers.remove(i);
                            old.close();
                            out.push(HubEv::Down { conn: old.conn, name: old.hello.name, error: "replaced".into() });
                        } else {
                            let _ = stream.shutdown(Shutdown::Both);
                            continue;
                        }
                    }
                    let ip = stream.peer_addr().map(|a| a.ip().to_string()).unwrap_or_default();
                    let peer = PeerConn { conn, hello: *hello, outbound, addr, ip, tx, stream };
                    // the welcome of an accepted connection comes from this node's hello at its start: send the
                    // current one (counts, run, disks) right after, whichever side dialed
                    peer.send(Msg::Hello(Box::new(Hello { token: String::new(), ..self.me.clone() })), Payload::None);
                    self.peers.push(peer);
                    out.push(HubEv::Up { conn });
                }
                NetEv::Down { conn, error } => {
                    if let Some(i) = self.peers.iter().position(|p| p.conn == conn) {
                        let p = self.peers.remove(i);
                        p.close();
                        out.push(HubEv::Down { conn, name: p.hello.name, error });
                    }
                }
                NetEv::DialFailed { addr, error } => {
                    self.dialing.remove(&addr);
                    self.dial_errors.insert(addr, error);
                }
                NetEv::Beacon { addr, name } => {
                    self.discovered.insert(addr, (name, Instant::now()));
                }
                NetEv::Msg { msg: Msg::Ping, .. } => {}
                NetEv::Msg { conn, msg, data, dir } => {
                    // repeated Hello (counts, running, disks changed): take over the versions
                    if let Msg::Hello(h) = &msg && let Some(p) = self.peers.iter_mut().find(|p| p.conn == conn) {
                        p.hello = Hello { token: String::new(), ..(**h).clone() };
                    }
                    if self.peers.iter().any(|p| p.conn == conn) {
                        out.push(HubEv::Msg { conn, msg, data, dir });
                    } else if let Some(d) = dir {
                        let _ = fs::remove_dir_all(d);
                    }
                }
            }
        }
        out
    }

    pub fn close_all(&mut self) {
        for p in self.peers.drain(..) {
            p.close();
        }
    }

    pub fn peer(&self, conn: u64) -> Option<&PeerConn> {
        self.peers.iter().find(|p| p.conn == conn)
    }

    pub fn send(&self, conn: u64, msg: Msg, payload: Payload) {
        if let Some(p) = self.peer(conn) {
            p.send(msg, payload);
        }
    }

    /// Send files from a folder and delete the folder once they are written (at once if the node is gone)
    pub fn send_then_remove(&self, conn: u64, msg: Msg, payload: Payload, dir: PathBuf) {
        match self.peer(conn) {
            Some(p) => {
                if let Err(e) = p.tx.send(Out { msg, payload, cleanup: Some(dir) }) && let Some(d) = e.0.cleanup {
                    let _ = fs::remove_dir_all(d);
                }
            }
            None => {
                let _ = fs::remove_dir_all(dir);
            }
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rel_paths() {
        assert_eq!(safe_rel("a/b.png"), Some(PathBuf::from("a").join("b.png")));
        assert_eq!(safe_rel("../x"), None);
        assert_eq!(safe_rel("/etc/passwd"), None);
        // a drive only exists on Windows; on Linux "C:" is a plain folder name inside the inbox
        if cfg!(windows) {
            assert_eq!(safe_rel("C:/x"), None);
            assert_eq!(safe_rel("a:b"), None);
            assert_eq!(safe_rel("Data/tex.png:stream"), None);
            assert!(!safe_name("run:x"));
        }
        assert_eq!(safe_rel(""), None);
        assert!(safe_name("2026-11-04_10-00_arms") && !safe_name("../x") && !safe_name("a/b") && !safe_name(".."));
        assert!(safe_hash("0123456789abcdef0123456789abcdef") && !safe_hash("../../../../etc/passwd") && !safe_hash("0123456789ABCDEF0123456789abcdef"));
    }

    /// A node that accepts a connection later than its counts changed: the other side sees the current counts, not
    /// the ones from its start (the export workers come only once the DataCore tables are there)
    #[test]
    fn current_hello_on_accept() {
        let root = std::env::temp_dir().join(format!("packshot-hello-{}", std::process::id()));
        let port = crate::net::test_port();
        let cfg = |name: &str, listen: String, peers: Vec<String>| NetConfig { name: name.into(), token: "t".into(), listen, peers, discovery: false };
        let me = |name: &str| Hello { name: name.into(), version: "t".into(), export: true, exporters: 0, ..Default::default() };
        let mut a = Hub::new(cfg("a", format!("127.0.0.1:{port}"), Vec::new()), me("a"), root.join("a"));
        // the counts change before anyone is connected (what Engine::announce does)
        a.me.exporters = 2;
        let mut b = Hub::new(cfg("b", format!("127.0.0.1:{}", crate::net::test_port()), vec![format!("127.0.0.1:{port}")]), me("b"), root.join("b"));
        let t0 = Instant::now();
        loop {
            a.poll();
            b.poll();
            if b.peers.first().is_some_and(|p| p.hello.exporters == 2) {
                break;
            }
            assert!(t0.elapsed() < Duration::from_secs(10), "b still sees {:?}", b.peers.first().map(|p| p.hello.exporters));
            thread::sleep(Duration::from_millis(20));
        }
        a.close_all();
        b.close_all();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn frames_roundtrip() {
        let dir = std::env::temp_dir().join(format!("packshot-net-{}", std::process::id()));
        let src = dir.join("src");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("a.txt"), "hallo").unwrap();
        fs::write(src.join("sub").join("b.bin"), vec![7u8; 100_000]).unwrap();
        let (files, paths) = collect_files(&src, "x/").unwrap();
        let mut buf = Vec::new();
        write_frame(&mut buf, &Msg::Render { id: 3, size: 256, jobs: Vec::new(), files: files.clone() }, &Payload::Files(paths)).unwrap();
        write_frame(&mut buf, &Msg::Item { block: 3, item: 1, ok: true, info: "i".into() }, &Payload::Bytes(vec![1, 2, 3])).unwrap();
        let mut r = io::Cursor::new(buf);
        let mut seq = 0;
        let (msg, _, d) = read_frame(&mut r, &dir.join("in"), &mut seq).unwrap();
        assert!(matches!(msg, Msg::Render { id: 3, .. }));
        let d = d.unwrap();
        assert_eq!(fs::read_to_string(d.join("x").join("a.txt")).unwrap(), "hallo");
        assert_eq!(fs::read(d.join("x").join("sub").join("b.bin")).unwrap().len(), 100_000);
        let (msg, data, d) = read_frame(&mut r, &dir.join("in"), &mut seq).unwrap();
        assert!(matches!(msg, Msg::Item { item: 1, .. }) && data == vec![1, 2, 3] && d.is_none());
        fs::remove_dir_all(&dir).unwrap();
    }
}
