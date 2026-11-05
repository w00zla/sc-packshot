// The cluster (part of the engine). There are no roles: every node starts with its capabilities (--render, --export)
// and can start a run, on itself or on another node, which then is that run's head node: it holds the run, its
// set and its images, hands export chunks to the exporting nodes and render chunks to the rendering ones (itself
// included), takes their results back (back into the queue when a node drops or a chunk stalls) and tells every node
// the run's status once a second; every node can abort it. Nodes fetch each other's runs (key h, they attach to live) and live manifests (m, for "missing").
// All connected nodes must have the same state, else no run starts and the TUI says what differs: the packshot
// build everywhere, Blender and add-on between the rendering nodes, Data.p4k and StarBreaker between the exporting
// ones (each compared with the first node of that capability, this one first, then by name).
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::engine::{Engine, Kind, Level, Manifest, Pool, RemoteRun, RemoteSel, Scope, Target, blender_version};
use crate::net::{DiskInfo, Hello, HubEv, Msg, Payload, RunStatus, RunSummary, WorkerStatus, collect_files, safe_name};
use crate::node::addon_dir;
use crate::store::{LIVE, RunInfo, slug};

pub fn load_manifest(store_root: &Path) -> Option<Manifest> {
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = fs::read_dir(store_root.join("manifests"))
        .ok()?
        .flatten()
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    files.sort();
    let (_, path) = files.pop()?;
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// StarBreaker commit from the COMMIT file next to the binary
pub fn starbreaker_commit(starbreaker: &Path) -> Option<String> {
    let text = fs::read_to_string(starbreaker.parent()?.join("COMMIT")).ok()?;
    text.split_whitespace().next().map(String::from)
}

/// Fingerprint of Data.p4k: FNV-1a over its size in bytes and Branch, Version and BuildId of build_manifest.id
/// next to it. Reads no archive data, so it is instant on every start
pub fn p4k_id(p4k: &Path) -> Option<String> {
    let len = fs::metadata(p4k).ok()?.len();
    let v: serde_json::Value = serde_json::from_slice(&fs::read(p4k.parent()?.join("build_manifest.id")).ok()?).ok()?;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    feed(&len.to_le_bytes());
    for key in ["Branch", "Version", "BuildId"] {
        feed(v["Data"][key].as_str().unwrap_or("").as_bytes());
        feed(&[0]);
    }
    Some(format!("{h:016x}"))
}

/// Game version from build_manifest.id next to Data.p4k: "4.10.2 (4.10.197.2878)" from Branch and Version
pub fn p4k_version(p4k: &Path) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(&fs::read(p4k.parent()?.join("build_manifest.id")).ok()?).ok()?;
    let data = &v["Data"];
    let branch = data["Branch"].as_str().unwrap_or("").rsplit('-').next().unwrap_or("").to_string();
    let version = data["Version"].as_str().unwrap_or("");
    match (branch.is_empty(), version.is_empty()) {
        (true, true) => None,
        (false, false) => Some(format!("{branch} ({version})")),
        _ => Some(format!("{branch}{version}")),
    }
}

/// GPUs from nvidia-smi as "RTX 5080 · VRAM 16 GB" (vendor prefixes dropped), several joined with +; None without one
pub fn gpu_info() -> Option<String> {
    let out = std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=name,memory.total", "--format=csv,noheader,nounits"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let gpus: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let (name, mib) = l.split_once(',')?;
            let name = name.trim().trim_start_matches("NVIDIA ").trim_start_matches("GeForce ");
            let gb = mib.trim().parse::<f64>().ok().map(|m| format!(" · VRAM {:.0} GB", m / 1024.0)).unwrap_or_default();
            Some(format!("{name}{gb}"))
        })
        .collect();
    (!gpus.is_empty()).then(|| gpus.join(" + "))
}

/// CPU name and total RAM in bytes
pub fn host_info() -> (Option<String>, Option<u64>) {
    let Ok(mut s) = sysinfo::System::new() else { return (None, None) };
    s.refresh_cpu_all();
    s.refresh_memory();
    // "AMD Ryzen 7 9800X3D 8-Core Processor" -> "AMD Ryzen 7 9800X3D" (the core count is shown on its own)
    let cpu = s
        .cpus()
        .first()
        .map(|c| {
            c.brand()
                .replace("(R)", "")
                .replace("(TM)", "")
                .split_whitespace()
                .filter(|w| *w != "Processor" && *w != "CPU" && !w.ends_with("-Core"))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|b| !b.is_empty());
    let ram = Some(s.total_memory()).filter(|m| *m > 0);
    (cpu, ram)
}

/// Free and total space of the disks holding these folders, one entry per disk (the mount point with the longest
/// matching prefix)
pub fn disk_info(paths: &[PathBuf]) -> Vec<DiskInfo> {
    let Ok(disks) = sysinfo::Disks::new_with_refreshed_list() else { return Vec::new() };
    let mut out: Vec<DiskInfo> = Vec::new();
    for p in paths {
        let full = fs::canonicalize(p).unwrap_or_else(|_| p.clone());
        // Windows: \\?\D:\... -> D:\...
        let text = full.to_string_lossy().trim_start_matches(r"\\?\").to_string();
        let full = PathBuf::from(text);
        let best = disks.iter().filter(|d| full.starts_with(d.mount_point())).max_by_key(|d| d.mount_point().as_os_str().len());
        if let Some(d) = best {
            let mount = d.mount_point().to_string_lossy().to_string();
            if !out.iter().any(|o| o.mount == mount) {
                out.push(DiskInfo { mount, free: d.available_space(), total: d.total_space() });
            }
        }
    }
    out
}

/// What a node can do from the start (--render, --export) and the tools it has: every bundle carries Blender and
/// StarBreaker, so their versions are reported either way; Data.p4k only with --export
pub struct Caps<'a> {
    pub render: bool,
    pub export: bool,
    pub blender: Option<&'a Path>,
    pub starbreaker: Option<&'a Path>,
    pub p4k: Option<&'a Path>,
}

/// Own versions for the handshake
pub fn own_hello(name: &str, caps: Caps, slots: usize, exporters: usize, cores: usize, disks: Vec<DiskInfo>) -> Hello {
    let (cpu, ram) = host_info();
    let p4k = caps.p4k.filter(|_| caps.export);
    Hello {
        token: String::new(),
        name: name.to_string(),
        instance: crate::net::new_instance(),
        // the build id hashes the source (build.rs), so different builds are told apart
        version: format!("{}+{}", env!("CARGO_PKG_VERSION"), env!("PACKSHOT_BUILD")),
        os: std::env::consts::OS.to_string(),
        render: caps.render,
        export: caps.export,
        blender: caps.blender.map(blender_version).unwrap_or_default(),
        addon: caps.blender.and_then(|b| crate::events::tree_hash(&addon_dir(b))),
        slots,
        exporters,
        cores,
        p4k_bytes: p4k.and_then(|p| fs::metadata(p).ok()).map(|m| m.len()),
        p4k_id: p4k.and_then(p4k_id),
        p4k_version: p4k.and_then(p4k_version),
        starbreaker: caps.starbreaker.and_then(starbreaker_commit),
        gpu: gpu_info(),
        cpu,
        ram,
        running: false,
        disks,
        assess: None,
        assessing: false,
    }
}

impl Engine {
    /// The node whose state all others must match: the head node of the run going on (this one, or the one
    /// reporting its status), else the head node chosen for the next start (this one unless n chose another)
    pub fn state_head(&self) -> Hello {
        if self.running() {
            return self.me.clone();
        }
        let peer = |name: &str| self.connected_nodes().into_iter().find(|(_, h)| h.name == name).map(|(_, h)| h);
        if let Some(h) = self.remote_run.as_ref().filter(|r| !r.status.finished).and_then(|r| peer(&r.from)) {
            return h;
        }
        if let Some(h) = self.head.as_deref().and_then(peer) {
            return h;
        }
        self.me.clone()
    }

    /// What differs from the cluster's state (`state_head`). Empty = same state
    pub fn differences(&self, h: &Hello) -> Vec<String> {
        self.differences_to(h, &self.state_head())
    }

    /// What differs from `head`'s state: packshot build, Blender, add-on and StarBreaker (every bundle carries all of
    /// them), and Data.p4k among the exporting nodes (in sync with each other; the head node needs none)
    pub fn differences_to(&self, h: &Hello, head: &Hello) -> Vec<String> {
        let mut out = Vec::new();
        if h.name != head.name {
            let mut cmp = |what: &str, there: Option<&str>, want: Option<&str>| {
                if there != want {
                    out.push(format!("{what} {} (head {} {})", there.unwrap_or("?"), head.name, want.unwrap_or("?")));
                }
            };
            cmp("packshot", Some(&h.version), Some(&head.version));
            cmp("Blender", Some(&h.blender), Some(&head.blender));
            cmp("add-on", h.addon.as_deref(), head.addon.as_deref());
            cmp("StarBreaker", h.starbreaker.as_deref(), head.starbreaker.as_deref());
        }
        if h.export {
            let mut nodes = vec![self.me.clone()];
            nodes.extend(self.connected_nodes().into_iter().map(|(_, h)| h));
            let mut other: Vec<String> = nodes
                .iter()
                .filter(|n| n.export && n.name != h.name && n.p4k_id != h.p4k_id)
                .map(|n| format!("{} {}", n.name, n.p4k_version.as_deref().or(n.p4k_id.as_deref()).unwrap_or("?")))
                .collect();
            other.sort();
            if !other.is_empty() {
                out.push(format!("Data.p4k {} ({})", h.p4k_version.as_deref().or(h.p4k_id.as_deref()).unwrap_or("?"), other.join(", ")));
            }
        }
        out
    }

    /// Nodes with another state than the head node, each with what differs; no run starts while there is one
    pub fn state_problems(&self) -> Vec<(String, Vec<String>)> {
        self.state_problems_to(&self.state_head())
    }

    fn state_problems_to(&self, head: &Hello) -> Vec<(String, Vec<String>)> {
        let mut out: Vec<(String, Vec<String>)> = self.connected_nodes().into_iter().map(|(_, h)| (h.name.clone(), self.differences_to(&h, head))).collect();
        out.push((self.me.name.clone(), self.differences_to(&self.me, head)));
        out.retain(|(_, d)| !d.is_empty());
        out
    }

    /// All connected nodes: (connection, versions)
    pub fn connected_nodes(&self) -> Vec<(u64, Hello)> {
        let Some(hub) = self.hub.as_ref() else { return Vec::new() };
        hub.peers.iter().map(|p| (p.conn, p.hello.clone())).collect()
    }

    /// Nodes that get work: connected, same state, something offered, no run of their own and (during a run)
    /// present at its start: (connection, versions and counts)
    pub(crate) fn usable_nodes(&self) -> Vec<(u64, Hello)> {
        let running = self.running();
        self.connected_nodes()
            .into_iter()
            // this node hands out the work, so it is the head node the others must match
            .filter(|(_, h)| self.differences_to(h, &self.me).is_empty() && ((h.render && h.slots > 0) || (h.export && h.exporters > 0)) && !h.running && !h.assessing)
            .filter(|(_, h)| !running || self.run.as_ref().is_some_and(|r| r.nodes.contains(&h.name)))
            .collect()
    }

    /// Why a run cannot start with this node as head node and these nodes, if it cannot: nobody exports or renders
    pub fn capacity_problem(&self) -> Option<String> {
        let usable = self.usable_nodes();
        let exports = (self.me.export && self.exporters_now() > 0) || usable.iter().any(|(_, h)| h.export && h.exporters > 0);
        let renders = (self.me.render && self.max_renderers > 0) || usable.iter().any(|(_, h)| h.render && h.slots > 0);
        match (exports, renders) {
            (true, true) => None,
            (false, true) => Some("no node exports (--export)".into()),
            (true, false) => Some("no node renders (--render)".into()),
            (false, false) => Some("no node exports or renders (--export, --render)".into()),
        }
    }

    /// Tell the others this node's counts, whether a run is going here and its disks
    pub(crate) fn announce(&mut self) {
        if let Some(h) = self.hub.as_mut() {
            h.me.slots = self.me.slots;
            h.me.exporters = self.me.exporters;
            h.me.running = self.me.running;
            h.me.disks = self.me.disks.clone();
            h.me.assess = self.me.assess.clone();
            h.me.assessing = self.me.assessing;
            for p in &h.peers {
                p.send(Msg::Hello(Box::new(h.me.clone())), Payload::None);
            }
        }
    }

    /// Collect and process the cluster's messages (from tick)
    pub(crate) fn poll_net(&mut self) {
        let Some(hub) = self.hub.as_mut() else { return };
        for ev in hub.poll() {
            match ev {
                HubEv::Up { conn } => {
                    let Some(h) = self.hub.as_ref().and_then(|h| h.peer(conn)).map(|p| p.hello.clone()) else { continue };
                    self.note(Level::Info, format!("{} connected ({}, {} Blender, {} export)", h.name, h.caps(), h.slots, h.exporters));
                    let diff = self.differences(&h);
                    if !diff.is_empty() {
                        self.note(Level::Error, format!("{} has another state, no run starts until all match: {}", h.name, diff.join(", ")));
                    }
                    if let Some(hub) = self.hub.as_ref() {
                        hub.send(conn, Msg::ListRuns, Payload::None);
                    }
                    // the head node chosen for the next start came (back)
                    if self.head.as_deref() == Some(h.name.as_str()) {
                        self.request_state();
                    }
                }
                HubEv::Down { conn, name, error } => {
                    self.note(Level::Info, format!("{name} disconnected: {error}"));
                    self.peer_runs.remove(&name);
                    self.requeue_node(conn, &name);
                    if let Some(s) = self.serve.as_mut() {
                        s.on_down(self.hub.as_ref(), conn, &name);
                    }
                    self.assess_down(conn);
                    // the head node of the run shown here is gone: that run is over
                    if let Some(r) = self.remote_run.as_mut().filter(|r| r.from == name && !r.status.finished) {
                        r.status.finished = true;
                        r.status.aborted = true;
                        self.note(Level::Error, format!("{name} (head node) disconnected: its run is over here"));
                    }
                    if let Some(r) = self.remote.as_mut().filter(|r| r.node == name) {
                        r.loaded = false;
                    }
                }
                HubEv::Msg { conn, msg, data, dir } => {
                    // work for a run's head node and the fetches between the nodes: exported, held, rendered here
                    let work = matches!(
                        msg,
                        Msg::Export { .. } | Msg::Render { .. } | Msg::Cancel { .. } | Msg::Release { .. } | Msg::GetItems { .. } | Msg::ItemsInfo { .. } | Msg::Fetch { .. } | Msg::FetchData { .. }
                    );
                    if !work {
                        self.on_msg(conn, msg, data, dir);
                        continue;
                    }
                    // an export chunk: pose and rig from this node's tables, items without a rig fail at once
                    let msg = match msg {
                        Msg::Export { id, jobs } => {
                            let (jobs, bad) = self.fill_items(jobs);
                            for (item, error) in bad {
                                self.reply(conn, Msg::ItemExported { chunk: id, item, ok: false, seconds: 0.0, error, worn: None }, Payload::None);
                            }
                            if jobs.is_empty() {
                                self.reply(conn, Msg::ExportDone { id, exit: None, wall: 0.0, item_seconds: 0.0, error: String::new(), long_paths: 0, failed: Vec::new() }, Payload::None);
                                continue;
                            }
                            Msg::Export { id, jobs }
                        }
                        m => m,
                    };
                    let mut lines = Vec::new();
                    if let Some(s) = self.serve.as_mut() {
                        s.on_msg(self.hub.as_ref(), conn, msg, dir, &mut |l| lines.push(l));
                    }
                    for l in lines {
                        self.note(Level::Info, l);
                    }
                }
            }
        }
        // the others see this node's counts (also after + / - and < / >), whether a run is going here and its disks
        let running = self.running();
        let mut changed = self.max_renderers != self.me.slots || self.exporters_now() != self.me.exporters || running != self.me.running;
        if self.last_disk.elapsed() > std::time::Duration::from_secs(10) && !self.disk_paths.is_empty() {
            self.last_disk = std::time::Instant::now();
            let disks = disk_info(&self.disk_paths);
            let moved = disks.len() != self.me.disks.len() || disks.iter().zip(&self.me.disks).any(|(a, b)| a.free.abs_diff(b.free) > 1_000_000_000);
            if moved {
                self.me.disks = disks;
                changed = true;
            }
        }
        if changed {
            self.me.slots = self.max_renderers;
            self.me.exporters = self.exporters_now();
            self.me.running = running;
            self.announce();
        }
        self.send_status();
    }

    /// The head node that has a run going, if another node is: the nodes work on one run at a time
    pub fn running_elsewhere(&self) -> Option<String> {
        self.connected_nodes().into_iter().find(|(_, h)| h.running).map(|(_, h)| h.name)
    }

    /// A node dropped out: the items it held go back to be exported again, so do its chunks' open items (a render
    /// chunk's into the pool while their holder is there), and the items render chunks elsewhere were still fetching
    /// from it
    fn requeue_node(&mut self, conn: u64, name: &str) {
        if !self.running() {
            return;
        }
        let reason = format!("{name} disconnected");
        // its export chunks are gone with it (nothing to release)
        let gone: HashSet<u64> = self.run.as_ref().map(|r| r.xchunks.iter().filter(|(_, x)| x.conn == conn).map(|(id, _)| *id).collect()).unwrap_or_default();
        if let Some(run) = self.run.as_mut() {
            for x in &gone {
                run.xchunks.remove(x);
            }
        }
        while let Some(idx) = self.workers.iter().position(|w| w.conn == conn) {
            let w = self.workers.remove(idx);
            let open = w.order.iter().filter(|t| !w.reported.contains(t)).count();
            if open > 0 {
                self.note(Level::Info, format!("{} chunk {} of {name} back in the queue: {open} items open", if w.kind == Kind::Export { "Export" } else { "Render" }, w.chunk));
            }
            self.requeue_open(&w, &reason, false);
        }
        let held_there = |r: &crate::engine::Run, t: &usize| r.held.get(t).is_some_and(|x| gone.contains(x));
        // render chunks elsewhere that have not got their files yet
        let mut lost = Vec::new();
        if let Some(run) = self.run.as_ref() {
            for w in self.workers.iter_mut().filter(|w| w.kind == Kind::Render && !w.fetched) {
                let (out, keep): (Vec<usize>, Vec<usize>) = w.order.iter().partition(|t| !w.reported.contains(t) && held_there(run, t));
                w.order = keep;
                lost.extend(out);
            }
        }
        // waiting in the pool
        if let Some(run) = self.run.as_mut() {
            let (out, keep): (Pool, Pool) = run.pool.drain(..).partition(|(t, _)| run.held.get(t).is_some_and(|x| gone.contains(x)));
            run.pool = keep;
            lost.extend(out.into_iter().map(|(t, _)| t));
        }
        if !lost.is_empty() {
            self.note(Level::Info, format!("{} exported items held on {name} back to be exported again", lost.len()));
        }
        for t in lost.into_iter().rev() {
            self.requeue(t, true, &reason, true, false);
        }
    }

    fn reply(&self, conn: u64, msg: Msg, payload: Payload) {
        if let Some(h) = self.hub.as_ref() {
            h.send(conn, msg, payload);
        }
    }

    pub(crate) fn peer_name(&self, conn: u64) -> String {
        self.hub.as_ref().and_then(|h| h.peer(conn)).map(|p| p.hello.name.clone()).unwrap_or_default()
    }

    /// Connection to a node by host name
    pub fn conn_of(&self, name: &str) -> Option<u64> {
        self.hub.as_ref()?.peers.iter().find(|p| p.hello.name == name).map(|p| p.conn)
    }

    pub(crate) fn on_msg(&mut self, conn: u64, msg: Msg, data: Vec<u8>, dir: Option<PathBuf>) {
        // an assessment's requests and its test set's export
        let Some(msg) = self.assess_msg(conn, msg) else { return };
        match msg {
            Msg::ItemExported { chunk, item, ok, seconds, error, worn } => self.on_item_exported(conn, chunk, item, ok, seconds, error, worn),
            Msg::ExportDone { id, exit, wall, item_seconds, error, long_paths, failed } => self.on_export_done(conn, id, exit, wall, item_seconds, error, long_paths, failed),
            Msg::Fetched { id, holders, bytes, cached, files, secs } => self.on_fetched(conn, id, holders, bytes, cached, files, secs),
            Msg::Item { block, item, ok, info } => self.on_item(conn, block, item, ok, info, &data),
            Msg::BlockDone { id, exit, error } => self.on_block_done(conn, id, exit, error),
            Msg::Unfetched { block, items, reason, export } => self.on_unfetched(conn, block, items, reason, export),
            Msg::Hello(_) => {}
            Msg::GetState { set } => {
                let reply = (|| -> Result<Msg> {
                    let sets = self.store.sets()?;
                    // a set that does not exist yet would branch off live
                    let s = self.store.load_set(if sets.contains(&set) { &set } else { LIVE })?;
                    let r = self.store.resolve(&s)?;
                    let targets = self.store.load_targets(&s)?;
                    let sets = self.store.set_summaries()?;
                    Ok(Msg::State { set: set.clone(), sets, images: r.image.into_keys().collect(), failed: r.failed.into_iter().collect(), targets, stats: self.stats.clone() })
                })();
                match reply {
                    Ok(m) => self.reply(conn, m, Payload::None),
                    Err(e) => self.reply(conn, Msg::Error { what: format!("state of {set}: {e:#}") }, Payload::None),
                }
            }
            Msg::State { set, sets, images, failed, targets, stats } => {
                let from = self.peer_name(conn);
                if let Some(r) = self.remote.as_mut().filter(|r| r.node == from && r.set == set) {
                    r.sets = sets.iter().map(|s| s.name.clone()).collect();
                    r.set_list = sets;
                    r.stats = stats;
                    r.images = images.into_iter().collect();
                    r.failed = failed.into_iter().collect();
                    r.loaded = true;
                    self.state_seq += 1;
                    // the selection runs over the target list of that set
                    self.use_targets(targets);
                }
            }
            Msg::StartRun { targets, set, name, filter, scope, line } => {
                let from = self.peer_name(conn);
                let me = self.me.name.clone();
                match self.start_for(&from, targets, &set, &name, &filter, &scope, &line) {
                    Ok(run) => self.reply(conn, Msg::Started { run }, Payload::None),
                    Err(e) => {
                        self.note(Level::Error, format!("No run for {from}: {e:#}"));
                        self.reply(conn, Msg::Error { what: format!("no run on {me}: {e:#}") }, Payload::None);
                    }
                }
            }
            Msg::Started { run } => {
                let from = self.peer_name(conn);
                self.note(Level::Ok, format!("Run {run} started on {from}"));
            }
            Msg::AbortRun => {
                let from = self.peer_name(conn);
                if self.running() {
                    self.note(Level::Info, format!("{from} aborts the run"));
                    self.abort();
                }
            }
            Msg::Status(status) => {
                let from = self.peer_name(conn);
                // the end of a run that was not known to be over here (a short run may only send its end)
                let ended = status.finished && !self.remote_run.as_ref().is_some_and(|r| r.from == from && r.status.run == status.run && r.status.finished);
                // its run is over: what this node holds and cached for it goes
                if status.finished && let Some(s) = self.serve.as_mut() {
                    s.end_run(conn, &from);
                }
                self.remote_run = Some(RemoteRun { from: from.clone(), status: *status, at: std::time::Instant::now() });
                // the run there is over: the selection against that node changed
                if ended && self.remote_sel().is_some_and(|r| r.node == from) {
                    self.request_state();
                }
            }
            Msg::ListRuns => {
                let runs = self.store.summaries();
                self.reply(conn, Msg::Runs { runs }, Payload::None);
            }
            Msg::Runs { runs } => {
                let name = self.peer_name(conn);
                self.peer_runs.insert(name, runs);
            }
            // names from the network that become paths here: one folder name only
            Msg::GetRun { id } if !safe_name(&id) => self.reply(conn, Msg::Error { what: format!("run {id:?} not available") }, Payload::None),
            Msg::RunFiles { id, .. } if !safe_name(&id) => {
                let from = self.peer_name(conn);
                self.note(Level::Error, format!("Run {id:?} from {from} not taken: not a folder name"));
                if let Some(d) = dir {
                    let _ = fs::remove_dir_all(d);
                }
            }
            Msg::GetManifest { set } if !safe_name(&set) => self.reply(conn, Msg::Error { what: format!("manifest {set:?}: not a set name") }, Payload::None),
            Msg::GetRun { id } => {
                let dir = self.store.run_dir(&id);
                let finished = self.store.run(&id).is_ok_and(|r| r.finished.is_some());
                match collect_files(&dir, "") {
                    Ok((files, paths)) if finished => self.reply(conn, Msg::RunFiles { id, files }, Payload::Files(paths)),
                    _ => self.reply(conn, Msg::Error { what: format!("run {id} not available") }, Payload::None),
                }
            }
            Msg::RunFiles { id, .. } => {
                let Some(dir) = dir else { return };
                let from = self.peer_name(conn);
                // only a run this node asked that node for
                if !self.fetching.remove(&(from.clone(), id.clone())) {
                    self.note(Level::Error, format!("Run {id} from {from} not taken: not asked for"));
                    let _ = fs::remove_dir_all(&dir);
                    return;
                }
                match self.import_run(&id, &dir) {
                    Ok(Some((new_id, images))) => self.note(Level::Ok, format!("Run {new_id} fetched from {from}: {images} images, appended to live")),
                    Ok(None) => self.note(Level::Info, format!("Run {id} is here already")),
                    Err(e) => self.note(Level::Error, format!("Run {id} from {from} not taken: {e:#}")),
                }
                let _ = fs::remove_dir_all(&dir);
            }
            Msg::GetManifest { set } => match self.manifest_of(&set) {
                Ok(entities) => {
                    let from = self.me.name.clone();
                    self.reply(conn, Msg::Manifest { set, from, entities }, Payload::None);
                }
                Err(e) => self.reply(conn, Msg::Error { what: format!("manifest {set}: {e:#}") }, Payload::None),
            },
            Msg::Manifest { set, from, entities } => {
                let m = Manifest { from: from.clone(), set: set.clone(), at: chrono::Local::now().to_rfc3339(), entities };
                let path = self.store.root.join("manifests").join(format!("{}.json", slug(&from)));
                let res = fs::create_dir_all(self.store.root.join("manifests")).map_err(anyhow::Error::from).and_then(|_| Ok(fs::write(&path, serde_json::to_vec(&m)?)?));
                let present = m.entities.values().filter(|v| **v).count();
                self.manifest = Some(m);
                // the selection counts it: the TUI recomputes
                self.state_seq += 1;
                match res {
                    Ok(()) => self.note(Level::Ok, format!("Manifest of {set} from {from}: {present} entities with an image (counts for “missing” on live)")),
                    Err(e) => self.note(Level::Error, format!("Manifest not saved: {e:#}")),
                }
            }
            Msg::Error { what } => {
                let from = self.peer_name(conn);
                self.note(Level::Error, format!("{from}: {what}"));
            }
            _ => {
                if let Some(d) = dir {
                    let _ = fs::remove_dir_all(d);
                }
            }
        }
    }

    /// Start a run that another node asked for, with this node as head node: its targets appended, the set
    /// (created from live if missing) becomes the target set
    #[allow(clippy::too_many_arguments)]
    fn start_for(&mut self, starter: &str, targets: Vec<Target>, set: &str, name: &str, filter: &str, scope: &str, line: &str) -> Result<String> {
        if self.running() {
            bail!("a run is going here");
        }
        if targets.is_empty() {
            bail!("nothing to render");
        }
        // entity and UUIDs become file names here (the render, the icons)
        if let Some(why) = targets.iter().find_map(target_problem) {
            bail!("{why}");
        }
        if !self.store.sets()?.contains(&set.to_string()) {
            self.store.create_set(set, LIVE)?;
            self.note(Level::Info, format!("New set {set}, branched off live (for {starter})"));
        }
        if self.set.name != set {
            self.switch_set(set)?;
        }
        let todo = self.add_targets(targets);
        let scope = Scope::parse(scope).unwrap_or(Scope::Missing);
        self.start_local(&todo, line, filter, scope, name, starter)?;
        Ok(self.record.as_ref().map(|r| r.id.clone()).unwrap_or_default())
    }

    /// Start a run with this node as head node, with every connected node: only with the same state everywhere, no
    /// other run going, and someone to export and render
    pub fn start_local(&mut self, todo: &[usize], line: &str, filter: &str, scope: Scope, name: &str, starter: &str) -> Result<()> {
        if let Some(other) = self.running_elsewhere() {
            bail!("{other} has a run going: the nodes work on one run at a time");
        }
        if self.assessing() {
            bail!("an assessment is going here");
        }
        let problems = self.state_problems_to(&self.me.clone());
        if !problems.is_empty() {
            let list: Vec<String> = problems.iter().map(|(n, d)| format!("{n}: {}", d.join(", "))).collect();
            bail!("not the same state: {}", list.join(" · "));
        }
        if let Some(p) = self.capacity_problem() {
            bail!("{p}");
        }
        let nodes: Vec<String> = self.connected_nodes().into_iter().map(|(_, h)| h.name).collect();
        self.start(todo, line, filter, scope, name, nodes, starter)
    }

    /// Ask another head node to start a run over `todo` into the chosen set there
    pub fn start_remote_run(&mut self, todo: &[usize], line: &str, filter: &str, scope: Scope, name: &str) -> Result<()> {
        let Some(r) = self.remote_sel() else { bail!("no head node chosen") };
        let (node, set) = (r.node.clone(), r.set.clone());
        if !r.loaded {
            bail!("no state of {node} yet");
        }
        let conn = self.conn_of(&node).with_context(|| format!("{node} is not connected"))?;
        let targets: Vec<Target> = todo.iter().map(|&t| self.targets[t].clone()).collect();
        self.reply(conn, Msg::StartRun { targets, set: set.clone(), name: name.to_string(), filter: filter.to_string(), scope: scope.key().to_string(), line: line.to_string() }, Payload::None);
        Ok(())
    }

    /// Choose the head node for the next start (None = this node) and ask it for its state
    pub fn set_head(&mut self, node: Option<String>) {
        self.head = node.filter(|n| *n != self.me.name);
        match self.head.clone() {
            None => {
                self.remote = None;
                // back to this node's own target set and its list
                let name = self.set.name.clone();
                if let Err(e) = self.switch_set(&name) {
                    self.note(Level::Error, format!("Set {name} not reloaded: {e:#}"));
                }
            }
            Some(n) => {
                let set = self.remote.as_ref().filter(|r| r.node == n).map(|r| r.set.clone()).unwrap_or_else(|| LIVE.to_string());
                let sets = self.remote.as_ref().filter(|r| r.node == n).map(|r| r.sets.clone()).unwrap_or_default();
                let set_list = self.remote.as_ref().filter(|r| r.node == n).map(|r| r.set_list.clone()).unwrap_or_default();
                self.remote = Some(RemoteSel { node: n, set, sets, set_list, stats: None, images: Default::default(), failed: Default::default(), loaded: false });
                self.request_state();
            }
        }
    }

    /// Choose the target set on the remote head node and ask for its state
    pub fn set_remote_set(&mut self, set: &str) {
        if let Some(r) = self.remote.as_mut() {
            r.set = set.to_string();
            r.loaded = false;
        }
        self.request_state();
    }

    /// Ask the remote head node for its sets and the state of the chosen set
    pub fn request_state(&mut self) {
        let Some((node, set)) = self.remote_sel().map(|r| (r.node.clone(), r.set.clone())) else { return };
        if let Some(conn) = self.conn_of(&node) {
            self.reply(conn, Msg::GetState { set }, Payload::None);
            // its runs too, for the Sets view
            self.reply(conn, Msg::ListRuns, Payload::None);
        }
    }

    /// Abort the run going on: here, or ask its head node
    pub fn abort_any(&mut self) {
        if self.running() {
            self.abort();
            return;
        }
        let Some(from) = self.remote_run.as_ref().filter(|r| !r.status.finished).map(|r| r.from.clone()) else { return };
        if let Some(conn) = self.conn_of(&from) {
            self.reply(conn, Msg::AbortRun, Payload::None);
        }
    }

    /// The final status of a run that is over, if it has not gone out yet (--plain before it exits)
    pub fn final_status(&mut self) {
        if !self.running() {
            self.send_status();
        }
    }

    /// The run's status to every other node: once a second while it runs, once more at its end
    fn send_status(&mut self) {
        let Some(run) = self.run.as_ref() else { return };
        let finished = run.finished.is_some();
        if (finished && run.status_sent) || (!finished && self.last_status.elapsed() < std::time::Duration::from_secs(1)) {
            return;
        }
        self.last_status = std::time::Instant::now();
        let workers = self
            .workers
            .iter()
            .map(|w| {
                let (phase, current, done, total) = w.progress();
                WorkerStatus { label: w.label(), phase: phase.into(), entity: current.map(|t| self.targets[t].entity.clone()).unwrap_or_default(), done, total, secs: w.since.elapsed().as_secs_f64() }
            })
            .collect();
        let errors = self.errors.iter().rev().take(50).rev().map(|(t, e)| (self.targets[*t].entity.clone(), e.clone())).collect();
        let status = RunStatus {
            run: self.record.as_ref().map(|r| r.id.clone()).unwrap_or_default(),
            head: self.me.name.clone(),
            starter: run.starter.clone(),
            set: self.set.name.clone(),
            filter: run.filter.clone(),
            scope: run.scope.label().to_string(),
            total: run.total,
            ok: run.ok,
            failed: run.failed,
            exported: run.exported,
            pending: run.todo.len(),
            ready: run.pool.len(),
            held: run.held.len(),
            holders: run.holders(),
            nodes: self.run_throughput(120.0),
            elapsed: run.finished.unwrap_or_else(std::time::Instant::now).duration_since(run.started).as_secs_f64(),
            rate: self.rate(120.0),
            slots: self.total_slots(),
            paused: self.paused,
            finished,
            aborted: run.aborted,
            workers,
            errors,
            phase_avg: run.phase_avg(),
        };
        if let Some(h) = self.hub.as_ref() {
            for p in &h.peers {
                p.send(Msg::Status(Box::new(status.clone())), Payload::None);
            }
        }
        if let (true, Some(r)) = (finished, self.run.as_mut()) {
            r.status_sent = true;
        }
    }

    /// Entity -> has an image (false: only failed) for a set
    fn manifest_of(&mut self, set: &str) -> Result<BTreeMap<String, bool>> {
        let s = self.store.load_set(set)?;
        let r = self.store.resolve(&s)?;
        let mut out: BTreeMap<String, bool> = r.failed.into_iter().map(|e| (e, false)).collect();
        for e in r.image.into_keys() {
            out.insert(e, true);
        }
        Ok(out)
    }

    /// Take over a fetched run: folder to runs/ (with -<node> if the same id comes from another machine), images
    /// read-only, appended to live's chain. None: was already there
    fn import_run(&mut self, id: &str, dir: &Path) -> Result<Option<(String, usize)>> {
        if self.running() {
            bail!("a run is in progress");
        }
        let mut info: RunInfo = serde_json::from_slice(&fs::read(dir.join("run.json")).context("run.json missing")?).context("run.json")?;
        // its entities and files become paths in the sets here (sync links and removes them): file names only
        for (entity, r) in &info.entities {
            if !safe_name(entity) {
                bail!("entity {entity:?} in run.json is not a file name");
            }
            if let Some(f) = r.files.iter().find(|f| !safe_name(f)) {
                bail!("file {f:?} of {entity} in run.json is not a file name");
            }
        }
        let mut new_id = id.to_string();
        if self.store.run_dir(&new_id).exists() {
            if self.store.run(&new_id).is_ok_and(|r| r.node == info.node) {
                return Ok(None);
            }
            new_id = format!("{id}-{}", slug(&info.node));
            if self.store.run_dir(&new_id).exists() {
                return Ok(None);
            }
        }
        let target = self.store.run_dir(&new_id);
        if fs::rename(dir, &target).is_err() {
            copy_tree(dir, &target)?;
        }
        info.id = new_id.clone();
        self.store.save_run(&info)?;
        for e in fs::read_dir(&target)?.flatten() {
            if e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("png")) {
                let mut p = fs::metadata(e.path())?.permissions();
                p.set_readonly(true);
                fs::set_permissions(e.path(), p)?;
            }
        }
        let mut live = self.store.load_set(LIVE)?;
        live.runs.push(new_id.clone());
        self.store.sync(&mut live)?;
        if self.set.name == LIVE {
            self.set = live;
            self.resolved = self.store.resolve(&self.set)?;
        }
        self.stats = self.store.last_stats();
        Ok(Some((new_id, info.entities.values().filter(|e| e.ok).count())))
    }

    /// Runs of another node that are missing here: finished and rendered for its live
    pub fn fetchable(&self, peer: &str) -> Vec<RunSummary> {
        let Some(runs) = self.peer_runs.get(peer) else { return Vec::new() };
        runs.iter()
            .filter(|r| r.finished && r.set == LIVE && !self.fetching.contains(&(peer.to_string(), r.id.clone())))
            .filter(|r| {
                let here = |id: &str| self.store.run_dir(id).join("run.json").exists();
                !(here(&r.id) && self.store.peek_node(&r.id).as_deref() == Some(r.node.as_str())) && !here(&format!("{}-{}", r.id, slug(&r.node)))
            })
            .cloned()
            .collect()
    }

    /// Key h: fetch the missing runs
    pub fn fetch_runs(&mut self, conn: u64) -> usize {
        let name = self.peer_name(conn);
        let runs = self.fetchable(&name);
        for r in &runs {
            self.fetching.insert((name.clone(), r.id.clone()));
            self.reply(conn, Msg::GetRun { id: r.id.clone() }, Payload::None);
        }
        runs.len()
    }

    /// Key m on this node: drop the fetched manifest (its file too, else it comes back at the next start); "missing"
    /// counts only this node's live again
    pub fn drop_manifest(&mut self) {
        let Some(m) = self.manifest.take() else {
            self.note(Level::Info, "No manifest to drop");
            return;
        };
        let dir = self.store.root.join("manifests");
        let removed = fs::remove_dir_all(&dir).map_or_else(|e| e.kind() == std::io::ErrorKind::NotFound, |_| true);
        if removed {
            self.note(Level::Ok, format!("Manifest of {} dropped: \"missing\" counts only this node's live again", m.from));
        } else {
            self.note(Level::Error, format!("Manifest of {} dropped here, but {} not deleted", m.from, dir.display()));
        }
    }

    /// Key m: fetch the other node's live manifest
    pub fn fetch_manifest(&mut self, conn: u64) {
        self.reply(conn, Msg::GetManifest { set: LIVE.into() }, Payload::None);
    }

    /// Request a node's run list again
    pub fn refresh_runs(&mut self, conn: u64) {
        self.reply(conn, Msg::ListRuns, Payload::None);
    }
}

/// Why a target from another node cannot be rendered here: its entity and UUIDs must be file names (they name the
/// render in the work folder and the icons)
fn target_problem(t: &Target) -> Option<String> {
    if !safe_name(&t.entity) {
        return Some(format!("target {:?}: not a file name", t.entity));
    }
    t.uuids.iter().find(|u| !safe_name(u)).map(|u| format!("target {}: UUID {u:?} is not a file name", t.entity))
}

pub(crate) fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for e in fs::read_dir(src)?.flatten() {
        let p = e.path();
        let t = dst.join(e.file_name());
        if p.is_dir() {
            copy_tree(&p, &t)?;
        } else {
            fs::copy(&p, &t).with_context(|| p.display().to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NetConfig;
    use crate::engine::{Config, Scope, Target};
    use crate::net::Hub;
    use crate::store::{EntityResult, Store};
    use crate::tables::Tables;
    use std::time::{Duration, Instant};

    fn net(name: &str, listen: String, peers: Vec<String>) -> NetConfig {
        NetConfig { name: name.into(), token: "t".into(), listen, peers, discovery: false }
    }

    /// An engine with fake tools: `render`/`export` only as capabilities (the processes fail at once)
    fn engine(root: &Path, cfg: NetConfig, render: bool, export: bool) -> Engine {
        let store = Store::open(&root.join("store")).unwrap();
        let set = store.load_set(LIVE).unwrap();
        let me = Hello { name: cfg.name.clone(), version: "t".into(), render, export, blender: "5.2.3".into(), addon: Some("a".into()), slots: render as usize, exporters: export as usize, ..Default::default() };
        let hub = Hub::new(cfg, me.clone(), root.join("inbox"));
        let x = root.join("no-such-tool");
        let config = Config { p4k: x.clone(), starbreaker: x.clone(), blender: x.clone(), render_script: x, work: root.join("work"), size: 256, chunk: 50 };
        let mut e = Engine::new(config, Vec::new(), Tables::default(), store, set, Some(hub), me).unwrap();
        e.max_renderers = render as usize;
        e.max_exporters = export as usize;
        e
    }

    fn target(entity: &str) -> Target {
        Target { uuids: vec!["0000".into()], names: None, entity: entity.into(), kind: "WeaponPersonal".into(), sub_type: "Small".into(), worn: false }
    }

    fn until(nodes: &mut [&mut Engine], what: &str, done: &dyn Fn(&[&mut Engine]) -> bool) {
        let t0 = Instant::now();
        while !done(nodes) {
            assert!(t0.elapsed() < Duration::from_secs(20), "timeout: {what}");
            for b in nodes.iter_mut() {
                b.tick();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Same state or no run: packshot build, Blender, add-on and StarBreaker as on the head node (whatever the node
    /// does), Data.p4k in sync among the exporting nodes (the head node needs none)
    #[test]
    fn state_differences() {
        let root = std::env::temp_dir().join(format!("packshot-state-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        // a: the head node, without capabilities and without Data.p4k
        let mut e = engine(&root, NetConfig { name: "a".into(), ..Default::default() }, false, false);
        e.me.starbreaker = Some("655d5f68".into());
        let head = e.me.clone();
        let same = Hello { name: "b".into(), render: true, export: true, p4k_id: Some("1a2b".into()), ..head.clone() };
        assert!(e.differences_to(&same, &head).is_empty());
        let addon = Hello { addon: Some("other".into()), ..same.clone() };
        assert_eq!(e.differences_to(&addon, &head), vec!["add-on other (head a a)".to_string()]);
        let sb = Hello { render: false, export: false, starbreaker: Some("cc0c3c91".into()), ..same.clone() };
        assert_eq!(e.differences_to(&sb, &head).len(), 1, "StarBreaker counts even for a node without --export");
        let blender = Hello { render: false, export: false, blender: "5.2.2".into(), ..same.clone() };
        assert_eq!(e.differences_to(&blender, &head).len(), 1, "Blender counts even for a node without --render");
        // Data.p4k: only among the exporting nodes; here a single one, so nothing to compare
        assert!(e.differences_to(&Hello { p4k_id: Some("ffff".into()), ..same.clone() }, &head).is_empty());
        let _ = crate::store::remove_tree(&root);
    }

    /// Two exporting nodes with different Data.p4k: both are out of sync, whichever is the head node
    #[test]
    fn p4k_in_sync() {
        let root = std::env::temp_dir().join(format!("packshot-p4k-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let port = crate::net::test_port();
        let mut a = engine(&root.join("a"), net("a", format!("127.0.0.1:{port}"), Vec::new()), true, true);
        let mut b = engine(&root.join("b"), net("b", format!("127.0.0.1:{}", crate::net::test_port()), vec![format!("127.0.0.1:{port}")]), true, true);
        a.me.p4k_version = Some("4.10.197".into());
        a.me.p4k_id = Some("aaaa".into());
        b.me.p4k_version = Some("4.10.196".into());
        b.me.p4k_id = Some("bbbb".into());
        for e in [&mut a, &mut b] {
            let me = e.me.clone();
            e.hub.as_mut().unwrap().me = Hello { instance: e.hub.as_ref().unwrap().me.instance, ..me };
        }
        until(&mut [&mut a, &mut b], "connected", &|e| !e[0].connected_nodes().is_empty() && !e[1].connected_nodes().is_empty());
        let problems = a.state_problems();
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems.iter().any(|(n, d)| n == "a" && d == &vec!["Data.p4k 4.10.197 (b 4.10.196)".to_string()]), "{problems:?}");
        let todo = a.add_targets(vec![target("gun_a")]);
        assert!(a.start_local(&todo, "t", "", Scope::All, "x", "a").is_err(), "no run while out of sync");
        for e in [&mut a, &mut b] {
            e.hub.as_mut().unwrap().close_all();
        }
        let _ = crate::store::remove_tree(&root);
    }

    /// Two nodes: b lists a's runs, fetches the missing one (h) and a's live manifest (m)
    #[test]
    fn fetch_runs_and_manifest() {
        let root = std::env::temp_dir().join(format!("packshot-cluster-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let port = crate::net::test_port();
        let mut a = engine(&root.join("a"), net("a", format!("127.0.0.1:{port}"), Vec::new()), false, false);
        let mut b = engine(&root.join("b"), net("b", format!("127.0.0.1:{}", crate::net::test_port()), vec![format!("127.0.0.1:{port}")]), false, false);

        // a finished run on a's live with one image
        let id = "2026-11-03_10-00_x_missing";
        fs::create_dir_all(a.store.run_dir(id)).unwrap();
        let src = root.join("a").join("r.png");
        fs::write(&src, "png").unwrap();
        let files = vec!["X (x) 0000.png".to_string()];
        a.store.put_image(id, &src, &files).unwrap();
        let mut info = RunInfo { id: id.into(), name: "x".into(), set: LIVE.into(), node: "a".into(), finished: Some("now".into()), ..Default::default() };
        info.entities.insert("x".into(), EntityResult { ok: true, error: None, files: files.clone() });
        a.store.save_run(&info).unwrap();
        let mut live = a.store.load_set(LIVE).unwrap();
        live.runs.push(id.into());
        a.store.sync(&mut live).unwrap();

        until(&mut [&mut a, &mut b], "runs listed", &|e| e[1].fetchable("a").len() == 1);
        let conn = b.hub.as_ref().unwrap().peers[0].conn;
        assert_eq!(b.fetch_runs(conn), 1);
        until(&mut [&mut a, &mut b], "run fetched", &|e| e[1].store.run_dir(id).join("run.json").exists() && e[1].fetching.is_empty());
        assert_eq!(fs::read_to_string(b.store.set_dir(LIVE).join(&files[0])).unwrap(), "png");
        assert_eq!(b.set.runs, vec![id.to_string()]);
        assert!(b.fetchable("a").is_empty());

        let seq = b.state_seq;
        b.fetch_manifest(conn);
        until(&mut [&mut a, &mut b], "manifest", &|e| e[1].manifest.is_some());
        assert!(b.state_seq > seq, "the TUI recomputes its selection");
        assert_eq!(b.manifest.as_ref().unwrap().entities.get("x"), Some(&true));
        assert!(load_manifest(&b.store.root).is_some(), "kept for the next start");
        // m on this node drops it, the file too
        b.drop_manifest();
        assert!(b.manifest.is_none() && load_manifest(&b.store.root).is_none());
        for e in [&mut a, &mut b] {
            e.hub.as_mut().unwrap().close_all();
        }
        let _ = crate::store::remove_tree(&root);
    }

    /// A node without capabilities chooses another node as head node: gets its sets and the state of its set, starts
    /// a run there (into a new set), follows its status to the end; the selection then sees the result there
    #[test]
    fn start_on_another_box() {
        let root = std::env::temp_dir().join(format!("packshot-remote-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let port = crate::net::test_port();
        let mut m = engine(&root.join("m"), net("m", format!("127.0.0.1:{port}"), Vec::new()), true, true);
        let mut a = engine(&root.join("a"), net("a", format!("127.0.0.1:{}", crate::net::test_port()), vec![format!("127.0.0.1:{port}")]), false, false);
        // the target list lives in m's live; a has none of its own
        let file = root.join("targets.json");
        fs::write(&file, serde_json::to_vec(&vec![target("gun_a"), target("gun_b")]).unwrap()).unwrap();
        let mut live = m.store.load_set(LIVE).unwrap();
        m.store.set_targets(&mut live, &file).unwrap();
        assert_eq!(a.own_len, 0);
        until(&mut [&mut m, &mut a], "connected", &|e| !e[1].connected_nodes().is_empty());

        a.set_head(Some("m".into()));
        until(&mut [&mut m, &mut a], "state of m", &|e| e[1].remote_sel().is_some_and(|r| r.loaded));
        assert_eq!(a.remote_sel().unwrap().sets, vec![LIVE.to_string()]);
        assert_eq!(a.own_len, 2, "the target list of m's live came with its state");
        a.set_remote_set("exp");
        until(&mut [&mut m, &mut a], "state of exp", &|e| e[1].remote_sel().is_some_and(|r| r.loaded));
        let sel = a.select("", Scope::Missing);
        assert_eq!(sel.todo.len(), 2);
        a.start_remote_run(&sel.todo, "test", "", Scope::Missing, "remote").unwrap();
        // m runs it (its fake StarBreaker fails every export) and reports the end to a
        until(&mut [&mut m, &mut a], "run over on a", &|e| e[1].remote_run.as_ref().is_some_and(|r| r.status.finished));
        let st = &a.remote_run.as_ref().unwrap().status;
        assert_eq!((st.head.as_str(), st.starter.as_str(), st.set.as_str(), st.total, st.failed), ("m", "a", "exp", 2, 2));
        assert_eq!(st.errors.len(), 2);
        assert_eq!(m.set.name, "exp", "the set was created on m");
        assert_eq!(m.store.load_targets(&m.set).unwrap().len(), 2, "with live's target list");
        assert!(m.record.as_ref().is_some_and(|r| r.origin["starter"] == "a"));
        // the failures are part of m's set now: the selection on a sees them after the end
        until(&mut [&mut m, &mut a], "state after the run", &|e| e[1].remote_sel().is_some_and(|r| r.loaded && r.failed.len() == 2));
        assert_eq!(a.select("", Scope::Failed).todo.len(), 2);
        for e in [&mut m, &mut a] {
            e.hub.as_mut().unwrap().close_all();
        }
        let _ = crate::store::remove_tree(&root);
    }

    /// Every node may abort: a run going on m (paused, so nothing starts) is aborted from a
    #[test]
    fn abort_from_another_box() {
        let root = std::env::temp_dir().join(format!("packshot-abort-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let port = crate::net::test_port();
        let mut m = engine(&root.join("m"), net("m", format!("127.0.0.1:{port}"), Vec::new()), true, true);
        let mut a = engine(&root.join("a"), net("a", format!("127.0.0.1:{}", crate::net::test_port()), vec![format!("127.0.0.1:{port}")]), false, false);
        until(&mut [&mut m, &mut a], "connected", &|e| !e[1].connected_nodes().is_empty());
        let todo = m.add_targets(vec![target("gun_a")]);
        m.start_local(&todo, "test", "", Scope::All, "abort", "m").unwrap();
        m.paused = true;
        until(&mut [&mut m, &mut a], "status on a", &|e| e[1].remote_run.as_ref().is_some_and(|r| !r.status.finished) && e[1].running_elsewhere().is_some());
        // a node with a run going elsewhere starts nothing itself
        assert_eq!(a.running_elsewhere().as_deref(), Some("m"));
        a.abort_any();
        until(&mut [&mut m, &mut a], "aborted", &|e| e[1].remote_run.as_ref().is_some_and(|r| r.status.finished && r.status.aborted));
        assert!(!m.running());
        for e in [&mut m, &mut a] {
            e.hub.as_mut().unwrap().close_all();
        }
        let _ = crate::store::remove_tree(&root);
    }

    /// No run without someone to export and render
    #[test]
    fn capacity() {
        let root = std::env::temp_dir().join(format!("packshot-cap-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mut e = engine(&root, NetConfig { name: "a".into(), ..Default::default() }, false, false);
        assert!(e.capacity_problem().is_some_and(|p| p.contains("exports or renders")));
        let todo = e.add_targets(vec![target("gun_a")]);
        assert!(e.start_local(&todo, "t", "", Scope::All, "x", "a").is_err());
        e.me.render = true;
        e.max_renderers = 1;
        assert!(e.capacity_problem().is_some_and(|p| p.contains("exports")));
        let _ = crate::store::remove_tree(&root);
    }

    /// The DataCore tables in the background: no export workers until they are there; failing leaves the node
    /// without export (here a StarBreaker that does not exist and an empty work folder)
    #[test]
    fn tables_in_background() {
        let root = std::env::temp_dir().join(format!("packshot-tables-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mut e = engine(&root, NetConfig { name: "a".into(), ..Default::default() }, true, true);
        e.max_exporters = 2;
        assert_eq!(e.exporters_now(), 2);
        e.load_tables(false);
        assert_eq!(e.exporters_now(), 0, "nothing offered while loading");
        assert!(e.tables_loading().is_some());
        let t0 = Instant::now();
        while e.tables_loading().is_some() {
            assert!(t0.elapsed() < Duration::from_secs(10), "loading does not end");
            e.tick();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(e.exporters_now(), 0, "failed: still no export");
        assert!(e.log.iter().any(|l| l.text.starts_with("DataCore tables not loaded")), "the failure is in the log");
        assert!(e.capacity_problem().is_some_and(|p| p.contains("exports")));
        let _ = crate::store::remove_tree(&root);
    }

    /// A run from another node is taken only when this node asked for it, and only with file names as entities and
    /// files in its run.json (they become paths in the sets here, which sync links and removes)
    #[test]
    fn run_files_checked() {
        let root = std::env::temp_dir().join(format!("packshot-runfiles-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mut e = engine(&root, NetConfig { name: "b".into(), ..Default::default() }, false, false);
        // sets/live/../../outside.png
        let outside = root.join("store/outside.png");
        fs::write(&outside, "keep").unwrap();
        let id = "2026-11-04_10-00_x";
        let inbox = |entity: &str, file: &str| -> PathBuf {
            let d = root.join("inbox-run");
            let _ = fs::remove_dir_all(&d);
            fs::create_dir_all(&d).unwrap();
            let mut info = RunInfo { id: id.into(), name: "x".into(), set: LIVE.into(), node: "a".into(), finished: Some("now".into()), ..Default::default() };
            info.entities.insert(entity.into(), EntityResult { ok: true, error: None, files: vec![file.into()] });
            fs::write(d.join("run.json"), serde_json::to_vec(&info).unwrap()).unwrap();
            if safe_name(file) {
                fs::write(d.join(file), "png").unwrap();
            }
            d
        };
        let good = "X (x) 0000.png";
        let take = |e: &mut Engine, d: PathBuf| e.on_msg(7, Msg::RunFiles { id: id.into(), files: Vec::new() }, Vec::new(), Some(d));
        // not asked for, or asked from another node than the one sending it (conn 7 has no name here)
        take(&mut e, inbox("x", good));
        assert!(!e.store.run_dir(id).exists() && e.log.iter().any(|l| l.text.ends_with("not taken: not asked for")));
        e.log.clear();
        e.fetching.insert(("servbox".into(), id.into()));
        take(&mut e, inbox("x", good));
        assert!(!e.store.run_dir(id).exists() && e.log.iter().any(|l| l.text.ends_with("not taken: not asked for")));
        assert_eq!(e.fetching.len(), 1, "the request to servbox stays");
        e.fetching.clear();
        // asked for, but a file or an entity that leaves the set folder: not taken, nothing outside touched
        for (entity, file) in [("x", "../../outside.png"), ("../x", good)] {
            e.fetching.insert((String::new(), id.into()));
            take(&mut e, inbox(entity, file));
            assert!(!e.store.run_dir(id).exists() && e.fetching.is_empty(), "{entity} {file}");
            assert!(e.log.back().is_some_and(|l| l.text.contains("is not a file name")), "{}", e.log.back().unwrap().text);
        }
        assert_eq!(fs::read_to_string(&outside).unwrap(), "keep");
        // a proper one is taken
        e.fetching.insert((String::new(), id.into()));
        take(&mut e, inbox("x", good));
        assert_eq!(fs::read_to_string(e.store.set_dir(LIVE).join(good)).unwrap(), "png");
        let _ = crate::store::remove_tree(&root);
    }

    /// A start from another node with a target whose entity or UUID is not a file name is refused with the reason
    #[test]
    fn start_for_checks_targets() {
        let root = std::env::temp_dir().join(format!("packshot-startfor-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mut e = engine(&root, NetConfig { name: "h".into(), ..Default::default() }, true, true);
        let bad = |entity: &str, uuid: &str| Target { uuids: vec!["0000".into(), uuid.into()], entity: entity.into(), ..target("x") };
        for (t, want) in [(bad("../../evil", "0001"), "target \"../../evil\": not a file name"), (bad("a/b", "0001"), "not a file name"), (bad("gun", "../x"), "target gun: UUID \"../x\" is not a file name")] {
            let err = e.start_for("a", vec![target("gun_ok"), t], LIVE, "x", "", "all", "t").unwrap_err().to_string();
            assert!(err.contains(want), "{err}");
        }
        assert!(!e.running() && e.record.is_none());
        let _ = crate::store::remove_tree(&root);
    }

    /// Unfetched: an item a renderer cannot get from its holder goes to another renderer (not back to it), and is
    /// exported again when no other renderer is left; the holder dropping after that does not count it again
    #[test]
    fn unfetched_placement_and_count() {
        use crate::engine::XChunk;
        let root = std::env::temp_dir().join(format!("packshot-unfetched-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        // h renders itself (LOCAL); r stands for a rendering node
        let mut e = engine(&root, NetConfig { name: "h".into(), ..Default::default() }, true, false);
        let todo = e.add_targets(vec![target("gun_a"), target("gun_b")]);
        e.start(&todo, "t", "", Scope::All, "x", Vec::new(), "h").unwrap();
        let (a, b) = (todo[0], todo[1]);
        {
            let run = e.run.as_mut().unwrap();
            run.todo.clear();
            run.xchunks.insert(1, XChunk { node: "e".into(), conn: 7, open: 2 });
            run.held.insert(a, 1);
            run.held.insert(b, 1);
        }
        // r cannot reach e: a into the pool, for h but not for r
        e.push_worker(Kind::Render, 5, Some("r".into()), 8, vec![a]);
        e.on_unfetched(8, 5, vec![a], "e holds it and is not connected to r".into(), false);
        let run = e.run.as_ref().unwrap();
        assert!(run.pool.iter().any(|(t, _)| *t == a) && run.requeues[&a] == 1);
        assert!(crate::engine::pool_by_holder(run, "r").is_empty() && crate::engine::pool_by_holder(run, "h").get("e") == Some(&1));
        // h cannot reach e either and is the only renderer left: b is exported again
        e.push_worker(Kind::Render, 6, None, crate::node::LOCAL, vec![b]);
        e.on_unfetched(crate::node::LOCAL, 6, vec![b], "e holds it and is not connected to h".into(), false);
        let run = e.run.as_ref().unwrap();
        assert!(run.todo.contains(&b) && run.requeues[&b] == 1 && !run.unreachable.contains_key(&b));
        // e drops: a, back in the pool for that same incident, goes to be exported again without counting twice
        e.requeue_node(7, "e");
        let run = e.run.as_ref().unwrap();
        assert!(run.todo.contains(&a) && run.requeues[&a] == 1, "{:?}", run.requeues);
        let _ = crate::store::remove_tree(&root);
    }

    #[test]
    fn disks() {
        let d = disk_info(&[std::env::temp_dir()]);
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(d[0].total > 0 && d[0].free <= d[0].total);
    }
}

/// The chunks between nodes with fake tools (python scripts standing in for StarBreaker and Blender): exported on one
/// node and rendered on another, holder and renderer dropping out, timeouts, the backlog
#[cfg(all(test, unix))]
mod chunk_tests {
    use super::*;
    use crate::config::NetConfig;
    use crate::engine::{Config, Scope, Target};
    use crate::net::Hub;
    use crate::store::Store;
    use crate::tables::Tables;
    use serde_json::json;
    use std::time::{Duration, Instant};

    /// export-batch: per job a package with scene.json -> its sidecar -> a texture shared by all (15000 bytes, the
    /// same content in every chunk) and one of its own; bad_* fail, hang_e* hang, linger_* hang after the chunk's
    /// jobs, crash_e* end the process. With --stdin (and in its --help) the persistent mode: {"ready": true}, a job
    /// per line, {"end_chunk": dir} -> {"chunk_done": dir}. Every process writes its pid into pids-export next to it
    const FAKE_EXPORT: &str = r#"#!/usr/bin/env python3
import json, os, sys, time
if "--help" in sys.argv:
    print("Usage: starbreaker entity export-batch [OPTIONS] [JOBS]\n      --stdin  Read one JSON job per line")
    sys.exit(0)
open(os.path.join(os.path.dirname(sys.argv[0]), "pids-export"), "a").write("%d\n" % os.getpid())
def export(j):
    name, out = j["name"], j["output"]
    if name.startswith("bad_"):
        print(json.dumps({"name": name, "ok": False, "error": "no such entity", "seconds": 0.01}), flush=True)
        return
    if name.startswith("hang_e"):
        time.sleep(60)
    if name.startswith("crash_e"):
        os._exit(3)
    pkg = os.path.join(out, "Packages", name + "_LOD1_TEX2")
    os.makedirs(pkg, exist_ok=True)
    os.makedirs(os.path.join(out, "Data", "Textures"), exist_ok=True)
    os.makedirs(os.path.join(out, "Data", "Objects"), exist_ok=True)
    open(os.path.join(out, "Data", "Textures", "shared_TEX2.png"), "w").write("s" * 15000)
    open(os.path.join(out, "Data", "Objects", name + "_TEX2.png"), "w").write("own " + name)
    json.dump({"tex": ["Data/textures/shared_TEX2.png", "Data/Objects/" + name + "_TEX2.png"]}, open(os.path.join(out, "Data", "Objects", name + "_TEX2.materials.json"), "w"))
    json.dump({"root": {"material_sidecar": "Data/Objects/" + name + "_TEX2.materials.json"}, "package_rule": {"package_dir": "Packages/" + name + "_LOD1_TEX2"}}, open(os.path.join(pkg, "scene.json"), "w"))
    open(os.path.join(pkg, "scene.blend"), "w").write("blend")
    print(json.dumps({"name": name, "ok": True, "package": pkg, "seconds": 0.01, "worn": False}), flush=True)
if "--stdin" in sys.argv:
    print(json.dumps({"ready": True}), flush=True)
    names = []
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        v = json.loads(line)
        if v.get("quit"):
            break
        if "end_chunk" in v:
            if any(n.startswith("linger_") for n in names):
                time.sleep(60)
            names = []
            print(json.dumps({"chunk_done": v["end_chunk"]}), flush=True)
            continue
        names.append(v["name"])
        export(v)
    sys.exit(0)
jobs = json.load(open(sys.argv[3]))
for j in jobs:
    export(j)
if any(j["name"].startswith("linger_") for j in jobs):
    time.sleep(60)
"#;

    /// export-batch without --stdin (a StarBreaker before that mode): the jobs file only, its --help without it
    fn fake_export_once() -> String {
        FAKE_EXPORT.replace("\\n      --stdin  Read one JSON job per line", "").replace("if \"--stdin\" in sys.argv:", "if False:")
    }

    /// export-batch --stdin that cannot start: {"error": …, "ready": false}, exit 1
    const FAKE_EXPORT_DEAD: &str = r#"#!/usr/bin/env python3
import json, sys
if "--help" in sys.argv:
    print("      --stdin  Read one JSON job per line")
    sys.exit(0)
print(json.dumps({"error": "Data.p4k: no such file", "ready": False}), flush=True)
sys.exit(1)
"#;

    /// Blender with render.py --stdin ("ready:", a job per line, quit): reads scene.json and its sidecar like the
    /// add-on (case-insensitive), fails a job with a missing file, writes the "image" (the files' first bytes);
    /// slow_* take 0.3 s, hang_r* hang, crash_r* end the process. Every process writes its pid into pids-blender
    const FAKE_BLENDER: &str = r#"#!/usr/bin/env python3
import json, os, sys, time
open(os.path.join(os.path.dirname(sys.argv[0]), "pids-blender"), "a").write("%d\n" % os.getpid())
def find(root, rel):
    cur = root
    for part in rel.split("/"):
        hit = [e for e in os.listdir(cur) if e.lower() == part.lower()] if os.path.isdir(cur) else []
        if not hit:
            return None
        cur = os.path.join(cur, hit[0])
    return cur
def render(j):
    src, out = j["src"], j["out"]
    if "hang_r" in src:
        time.sleep(60)
    if "crash_r" in src:
        time.sleep(0.2)
        print("Segmentation fault in crash_r", file=sys.stderr, flush=True)
        os._exit(11)
    if "slow_" in src:
        time.sleep(0.3)
    pkg = os.path.dirname(src)
    root = os.path.dirname(os.path.dirname(pkg))
    scene = json.load(open(os.path.join(pkg, "scene.json")))
    sidecar = find(root, scene["root"]["material_sidecar"])
    files = [find(root, t) for t in json.load(open(sidecar))["tex"]] if sidecar else [None]
    if None in files or not os.path.exists(j["rig"]):
        print("failed: %s missing file" % out, flush=True)
        return
    open(out, "w").write("|".join(open(f).read()[:8] for f in files))
    print("icon: %s timing load=0.01 prep=0.00 render=0.01 glass=0.00 post=0.00" % out, flush=True)
if "--stdin" in sys.argv:
    print("ready: pid %d" % os.getpid(), flush=True)
    for line in sys.stdin:
        line = line.strip()
        if line == "quit":
            break
        if line:
            render(json.loads(line))
else:
    for j in json.load(open(sys.argv[sys.argv.index("--jobs") + 1])):
        render(j)
"#;

    fn net(name: &str, listen: String, peers: Vec<String>) -> NetConfig {
        NetConfig { name: name.into(), token: "t".into(), listen, peers, discovery: false }
    }

    fn script(path: &Path, text: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        path.to_path_buf()
    }

    /// A node with the fake tools: `blenders` Blender processes, `exporters` export processes (0: without the
    /// capability)
    fn node(root: &Path, cfg: NetConfig, blenders: usize, exporters: usize, chunk: usize) -> Engine {
        node_with(root, cfg, blenders, exporters, chunk, FAKE_EXPORT)
    }

    /// node() with another export-batch script
    fn node_with(root: &Path, cfg: NetConfig, blenders: usize, exporters: usize, chunk: usize, export_script: &str) -> Engine {
        let dir = root.join(&cfg.name);
        let store = Store::open(&dir.join("store")).unwrap();
        let set = store.load_set(LIVE).unwrap();
        let (render, export) = (blenders > 0, exporters > 0);
        let me = Hello { name: cfg.name.clone(), version: "t".into(), render, export, blender: "5.2.3".into(), addon: Some("a".into()), slots: blenders, exporters, ..Default::default() };
        let hub = Hub::new(cfg, me.clone(), dir.join("inbox"));
        let rig = dir.join("rig.json");
        fs::write(&rig, "{}").unwrap();
        fs::write(dir.join("render.py"), "# render").unwrap();
        fs::write(dir.join("Data.p4k"), "").unwrap();
        let config = Config {
            p4k: dir.join("Data.p4k"),
            starbreaker: script(&dir.join("tools/starbreaker"), export_script),
            blender: script(&dir.join("tools/blender"), FAKE_BLENDER),
            render_script: dir.join("render.py"),
            work: dir.join("work"),
            size: 64,
            chunk,
        };
        let mut e = Engine::new(config, Vec::new(), Tables::with_rig(rig), store, set, Some(hub), me).unwrap();
        e.max_renderers = blenders;
        e.max_exporters = exporters;
        e
    }

    fn target(entity: &str) -> Target {
        Target { uuids: vec!["0000".into()], names: None, entity: entity.into(), kind: "WeaponPersonal".into(), sub_type: "Small".into(), worn: false }
    }

    fn tick_until(nodes: &mut [&mut Engine], what: &str, secs: u64, done: &mut dyn FnMut(&mut [&mut Engine]) -> bool) {
        let t0 = Instant::now();
        while !done(nodes) {
            assert!(t0.elapsed() < Duration::from_secs(secs), "timeout: {what}");
            for b in nodes.iter_mut() {
                b.tick();
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Start a run over `entities` on the first node once the others are usable
    fn start(nodes: &mut [&mut Engine], entities: &[&str]) {
        let others = nodes.len() - 1;
        tick_until(nodes, "nodes usable", 15, &mut |n| n[0].usable_nodes().len() == others && n[0].capacity_problem().is_none());
        let todo = nodes[0].add_targets(entities.iter().map(|e| target(e)).collect());
        let me = nodes[0].me.name.clone();
        nodes[0].start_local(&todo, "test", "", Scope::All, "chunks", &me).unwrap();
    }

    fn events(e: &Engine, ev: &str) -> Vec<serde_json::Value> {
        let text = fs::read_to_string(e.run_dir().unwrap().join("events.jsonl")).unwrap();
        text.lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok()).filter(|v| v["ev"] == ev).collect()
    }

    fn close(nodes: &mut [&mut Engine]) {
        for e in nodes.iter_mut() {
            e.abort();
            if let Some(h) = e.hub.as_mut() {
                h.close_all();
            }
        }
    }

    /// Exported on a (no Blender), rendered on b (no export): b fetches the items' files from a straight; the
    /// second render chunk takes the texture all items share from b's cache and fetches only its own files. At the
    /// end a's export folders and b's cache are gone
    #[test]
    fn exported_here_rendered_there() {
        let root = std::env::temp_dir().join(format!("packshot-chunks-ab-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let port = crate::net::test_port();
        let mut a = node(&root, net("a", format!("127.0.0.1:{port}"), Vec::new()), 0, 1, 3);
        let mut b = node(&root, net("b", format!("127.0.0.1:{}", crate::net::test_port()), vec![format!("127.0.0.1:{port}")]), 1, 0, 3);
        let items = ["gun_1", "gun_2", "gun_3", "gun_4", "gun_5", "gun_6"];
        start(&mut [&mut a, &mut b], &items);
        tick_until(&mut [&mut a, &mut b], "run over", 30, &mut |n| !n[0].running());
        let run = a.run.as_ref().unwrap();
        assert_eq!((run.ok, run.failed), (6, 0), "{:?}", a.errors);
        let png = fs::read_to_string(a.image(a.history[0]).unwrap()).unwrap();
        assert!(png.starts_with("ssssssss|own gun_"), "rendered from the fetched files: {png}");
        let renders = events(&a, "render_start");
        assert!(renders.iter().all(|e| e["worker"].as_str().unwrap().starts_with("b/R") && e["holders"] == json!(["a"])), "{renders:?}");
        // the throughput per node (view 3 and the status): all six images on b
        let tp = a.run_throughput(120.0);
        assert!(tp.len() == 1 && tp[0].0 == "b" && tp[0].1 == 6 && tp[0].2 > 0.0, "{tp:?}");
        let fetches = events(&a, "fetch");
        assert_eq!(fetches.len(), 2, "{fetches:?}");
        let (first, second) = (&fetches[0], &fetches[1]);
        println!("fetches: {first} / {second}");
        assert!(first["bytes"].as_u64().unwrap() >= 15000 && first["cached"] == 0, "the first chunk fetches the shared texture: {first}");
        assert!(second["cached"].as_u64().unwrap() >= 15000 && second["bytes"].as_u64().unwrap() < 15000, "the second only what it lacks: {second}");
        // the head node's export folders released, b's cache and folders gone with the run
        tick_until(&mut [&mut a, &mut b], "cleaned up", 10, &mut |n| {
            let empty = |d: PathBuf| fs::read_dir(d).map(|mut r| r.next().is_none()).unwrap_or(true);
            empty(n[0].cfg.work.join("export")) && n[1].serve.as_ref().unwrap().cached_bytes() == 0 && empty(n[1].cfg.work.join("fetch")) && empty(n[1].cfg.work.join("cas"))
        });
        close(&mut [&mut a, &mut b]);
        let _ = crate::store::remove_tree(&root);
    }

    /// The holder drops while its exported items wait for a Blender: they go back to be exported again (here, by the
    /// head node) and render; a requeue each in the event log
    #[test]
    fn holder_drop_reexports() {
        let root = std::env::temp_dir().join(format!("packshot-chunks-holder-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let port = crate::net::test_port();
        let mut h = node(&root, net("h", format!("127.0.0.1:{port}"), Vec::new()), 1, 1, 50);
        let mut e = node(&root, net("e", format!("127.0.0.1:{}", crate::net::test_port()), vec![format!("127.0.0.1:{port}")]), 0, 1, 50);
        let items = ["slow_1", "slow_2", "slow_3", "slow_4", "slow_5", "slow_6"];
        start(&mut [&mut h, &mut e], &items);
        // e's exported items wait in the pool while h's single Blender renders its own
        tick_until(&mut [&mut h, &mut e], "items held on e waiting", 20, &mut |n| {
            let r = n[0].run.as_ref().unwrap();
            r.pool.iter().any(|(t, _)| r.held.get(t).and_then(|x| r.xchunks.get(x)).is_some_and(|x| x.node == "e"))
        });
        e.hub.as_mut().unwrap().close_all();
        tick_until(&mut [&mut h], "run over", 30, &mut |n| !n[0].running());
        let run = h.run.as_ref().unwrap();
        assert_eq!((run.ok, run.failed), (6, 0), "{:?}", h.errors);
        let requeues = events(&h, "requeue");
        println!("holder drop: {requeues:?}");
        assert!(!requeues.is_empty() && requeues.iter().all(|r| r["reason"] == "e disconnected" && r["to"] == "export"), "{requeues:?}");
        let again: Vec<String> = requeues.iter().map(|r| r["entity"].as_str().unwrap().to_string()).collect();
        let exports = events(&h, "export_item");
        assert!(again.iter().all(|t| exports.iter().any(|x| x["entity"] == t.as_str() && x["worker"] == "h/E1")), "exported again on h: {exports:?}");
        close(&mut [&mut h]);
        let _ = crate::store::remove_tree(&root);
    }

    /// A renderer drops in the middle of its chunk: the open items go back into the pool (their holder still has
    /// them) and render on the other renderer, fetched from the holder again; nothing is exported twice
    #[test]
    fn renderer_drop_rerenders_from_holder() {
        let root = std::env::temp_dir().join(format!("packshot-chunks-renderer-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let port = crate::net::test_port();
        let mut h = node(&root, net("h", format!("127.0.0.1:{port}"), Vec::new()), 0, 1, 50);
        let mut r1 = node(&root, net("r1", format!("127.0.0.1:{}", crate::net::test_port()), vec![format!("127.0.0.1:{port}")]), 1, 0, 50);
        let mut r2 = node(&root, net("r2", format!("127.0.0.1:{}", crate::net::test_port()), vec![format!("127.0.0.1:{port}")]), 1, 0, 50);
        let items = ["slow_1", "slow_2", "slow_3", "slow_4"];
        start(&mut [&mut h, &mut r1, &mut r2], &items);
        tick_until(&mut [&mut h, &mut r1, &mut r2], "r1 renders", 20, &mut |n| n[0].workers.iter().any(|w| w.host == "r1" && w.fetched));
        r1.hub.as_mut().unwrap().close_all();
        tick_until(&mut [&mut h, &mut r2], "run over", 30, &mut |n| !n[0].running());
        let run = h.run.as_ref().unwrap();
        assert_eq!((run.ok, run.failed), (4, 0), "{:?}", h.errors);
        // the head node goes away without its final status: r2's cache goes all the same
        assert!(r2.serve.as_ref().unwrap().cached_bytes() > 0, "r2 fetched from h");
        h.hub.as_mut().unwrap().close_all();
        tick_until(&mut [&mut r2], "cache gone", 10, &mut |n| n[0].serve.as_ref().unwrap().cached_bytes() == 0);
        let requeues = events(&h, "requeue");
        println!("renderer drop: {requeues:?}");
        assert!(!requeues.is_empty() && requeues.iter().all(|r| r["reason"] == "r1 disconnected" && r["to"] == "render"), "{requeues:?}");
        assert_eq!(events(&h, "export_item").len(), 4, "nothing exported twice");
        let last = events(&h, "render_start").into_iter().last().unwrap();
        assert!(last["worker"].as_str().unwrap().starts_with("r2/") && last["holders"] == json!(["h"]), "{last}");
        close(&mut [&mut h, &mut r2]);
        let _ = crate::store::remove_tree(&root);
    }

    /// A chunk without progress is cancelled after the timeout and its open items go back, at most twice each,
    /// then they fail with the reason; the others render
    #[test]
    fn chunk_timeout_and_requeue_limit() {
        let root = std::env::temp_dir().join(format!("packshot-chunks-timeout-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mut h = node(&root, NetConfig { name: "h".into(), ..Default::default() }, 1, 1, 50);
        h.chunk_timeout = Duration::from_millis(1500);
        let todo = h.add_targets(vec![target("gun_1"), target("hang_r1"), target("hang_e1")]);
        h.start_local(&todo, "test", "", Scope::All, "timeout", "h").unwrap();
        tick_until(&mut [&mut h], "run over", 40, &mut |n| !n[0].running());
        let run = h.run.as_ref().unwrap();
        assert_eq!((run.ok, run.failed), (1, 2), "{:?}", h.errors);
        for (entity, stage) in [("hang_r1", "render"), ("hang_e1", "export")] {
            let error = h.errors.iter().find(|(t, _)| h.targets[*t].entity == entity).map(|(_, e)| e.clone()).unwrap_or_default();
            assert!(error.starts_with("gave up after 2 requeues: no progress for 2 s on h/"), "{entity}: {error}");
            let requeues = events(&h, "requeue").into_iter().filter(|r| r["entity"] == entity && r["counted"] == true).count();
            assert_eq!(requeues, 2, "{entity}");
            assert!(events(&h, "failure").iter().any(|f| f["entity"] == entity && f["stage"] == stage));
        }
        close(&mut [&mut h]);
        let _ = crate::store::remove_tree(&root);
    }

    /// An export that reports every item and then never ends: no item is the culprit, every requeue counts, so the
    /// item fails after MAX_REQUEUES instead of going back for ever
    #[test]
    fn export_without_end_gives_up() {
        let root = std::env::temp_dir().join(format!("packshot-chunks-linger-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mut h = node(&root, NetConfig { name: "h".into(), ..Default::default() }, 1, 1, 50);
        h.chunk_timeout = Duration::from_millis(1000);
        let todo = h.add_targets(vec![target("linger_1")]);
        h.start_local(&todo, "test", "", Scope::All, "linger", "h").unwrap();
        tick_until(&mut [&mut h], "run over", 20, &mut |n| !n[0].running());
        let run = h.run.as_ref().unwrap();
        assert_eq!((run.ok, run.failed), (0, 1), "{:?}", h.errors);
        assert!(h.errors[0].1.starts_with("gave up after 2 requeues: no progress for 1 s on h/E"), "{}", h.errors[0].1);
        assert_eq!(events(&h, "requeue").iter().filter(|r| r["counted"] == true).count(), 2);
        tick_until(&mut [&mut h], "nothing held", 10, &mut |n| !n[0].serve.as_ref().unwrap().busy());
        close(&mut [&mut h]);
        let _ = crate::store::remove_tree(&root);
    }

    /// An ExportDone for an export chunk the head node gave up already (it timed out): the chunk is released, its node
    /// does not hold it for ever
    #[test]
    fn late_export_done_released() {
        let root = std::env::temp_dir().join(format!("packshot-chunks-late-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mut h = node(&root, NetConfig { name: "h".into(), ..Default::default() }, 0, 1, 50);
        let t = h.add_targets(vec![target("gun_1")])[0];
        let tg = h.targets[t].clone();
        let (jobs, bad) = h.fill_items(vec![crate::net::ItemJob { item: t, entity: tg.entity, worn: false, kind: tg.kind, sub_type: tg.sub_type, pose: [0.0; 3], rig: String::new() }]);
        assert!(bad.is_empty());
        // no worker for it on the head node, as after a timeout
        h.dispatch(crate::node::LOCAL, Msg::Export { id: 900, jobs }, Payload::None);
        let x = h.cfg.work.join("export/x900");
        let mut seen = false;
        tick_until(&mut [&mut h], "exported, then released", 10, &mut |n| {
            seen = seen || x.join("Packages").exists();
            seen && !x.exists() && !n[0].serve.as_ref().unwrap().busy()
        });
        close(&mut [&mut h]);
        let _ = crate::store::remove_tree(&root);
    }

    /// The renderer is not connected to the holder (both are to the head node) and is the only renderer: the items
    /// go back to be exported again (a pool would hand them to the same renderer), counted; after MAX_REQUEUES they
    /// fail with the reason instead of at once (and never as "no image" without one)
    #[test]
    fn renderer_cannot_reach_holder() {
        let root = std::env::temp_dir().join(format!("packshot-chunks-reach-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let port = crate::net::test_port();
        let mut x = node(&root, net("x", format!("127.0.0.1:{port}"), Vec::new()), 0, 0, 50);
        let mut e = node(&root, net("e", format!("127.0.0.1:{}", crate::net::test_port()), vec![format!("127.0.0.1:{port}")]), 0, 1, 50);
        let mut r = node(&root, net("r", format!("127.0.0.1:{}", crate::net::test_port()), vec![format!("127.0.0.1:{port}")]), 1, 0, 50);
        start(&mut [&mut x, &mut e, &mut r], &["gun_1"]);
        tick_until(&mut [&mut x, &mut e, &mut r], "run over", 30, &mut |n| !n[0].running());
        let run = x.run.as_ref().unwrap();
        assert_eq!((run.ok, run.failed), (0, 1), "{:?}", x.errors);
        assert_eq!(x.errors[0].1, "gave up after 2 requeues: e holds it and is not connected to r");
        let requeues = events(&x, "requeue");
        assert!(requeues.len() == 2 && requeues.iter().all(|q| q["to"] == "export" && q["counted"] == true), "{requeues:?}");
        assert_eq!(events(&x, "export_item").len(), 3, "exported again each time");
        close(&mut [&mut x, &mut e, &mut r]);
        let _ = crate::store::remove_tree(&root);
    }

    /// A render chunk still fetching its files is cancelled only after FETCH_TIMEOUTS timeouts, not after the plain
    /// one; then every one of its items counts (here a renderer that never answers: after MAX_REQUEUES they fail)
    #[test]
    fn fetch_timeout() {
        let root = std::env::temp_dir().join(format!("packshot-chunks-fetch-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let port = crate::net::test_port();
        let mut h = node(&root, net("h", format!("127.0.0.1:{port}"), Vec::new()), 0, 1, 50);
        h.chunk_timeout = Duration::from_millis(300);
        // r: a rendering node that takes its render chunks and answers nothing
        let hello = Hello { name: "r".into(), version: "t".into(), render: true, blender: "5.2.3".into(), addon: Some("a".into()), slots: 1, ..Default::default() };
        let mut r = Hub::new(net("r", format!("127.0.0.1:{}", crate::net::test_port()), vec![format!("127.0.0.1:{port}")]), hello, root.join("r-inbox"));
        let t0 = Instant::now();
        while h.usable_nodes().is_empty() || h.capacity_problem().is_some() {
            assert!(t0.elapsed() < Duration::from_secs(15), "r not usable");
            r.poll();
            h.tick();
            std::thread::sleep(Duration::from_millis(10));
        }
        let todo = h.add_targets(vec![target("gun_1")]);
        h.start_local(&todo, "test", "", Scope::All, "fetch", "h").unwrap();
        let (mut first, mut cancelled) = (None::<Instant>, None::<Duration>);
        while h.running() {
            assert!(t0.elapsed() < Duration::from_secs(40), "run not over");
            r.poll();
            h.tick();
            if first.is_none() && h.workers.iter().any(|w| w.kind == Kind::Render) {
                first = Some(Instant::now());
            }
            if cancelled.is_none() && h.log.iter().any(|l| l.text.contains("cancelled: files not fetched")) {
                cancelled = first.map(|f| f.elapsed());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let after = cancelled.expect("cancelled");
        println!("fetch timeout after {after:?}");
        assert!(after >= h.chunk_timeout * crate::engine::FETCH_TIMEOUTS - Duration::from_millis(50), "cancelled after {after:?}");
        let run = h.run.as_ref().unwrap();
        assert_eq!((run.ok, run.failed), (0, 1), "{:?}", h.errors);
        assert_eq!(h.errors[0].1, "gave up after 2 requeues: files not fetched in 2 s on r/R1");
        let requeues = events(&h, "requeue");
        assert!(requeues.len() == 2 && requeues.iter().all(|q| q["to"] == "render" && q["counted"] == true), "{requeues:?}");
        close(&mut [&mut h]);
        r.close_all();
        let _ = crate::store::remove_tree(&root);
    }

    /// Exports wait while the exported items not rendered yet reach the backlog: one slow Blender, many items
    #[test]
    fn backpressure() {
        let root = std::env::temp_dir().join(format!("packshot-chunks-backlog-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mut h = node(&root, NetConfig { name: "h".into(), ..Default::default() }, 1, 2, 50);
        let names: Vec<String> = (0..120).map(|k| format!("slow_{k}")).collect();
        let todo = h.add_targets(names.iter().map(|n| target(n)).collect());
        h.start_local(&todo, "test", "", Scope::All, "backlog", "h").unwrap();
        let mut most = 0;
        let mut held_back = false;
        tick_until(&mut [&mut h], "exports held back", 30, &mut |n| {
            let e = &n[0];
            let r = e.run.as_ref().unwrap();
            let exporting: usize = e.workers.iter().filter(|w| w.kind == Kind::Export).map(|w| w.order.len()).sum();
            most = most.max(r.held.len() + exporting);
            held_back = held_back || (!r.todo.is_empty() && exporting == 0 && !r.pool.is_empty());
            held_back && r.done() >= 3
        });
        println!("backlog: at most {most} exported items not rendered, held back {held_back}");
        // 1 Blender: render chunks of 120 / 3 = 40, the backlog 2 × 1 × 40, plus the export chunk that crosses it
        assert!(most <= 2 * 40 + 20, "{most} exported items not rendered at most");
        close(&mut [&mut h]);
        let _ = crate::store::remove_tree(&root);
    }

    /// --plain stops ticking once its run is over, in the same tick that ended it: final_status still tells the
    /// other nodes the run's end (they drop what they hold for it)
    #[test]
    fn final_status_after_the_last_tick() {
        let root = std::env::temp_dir().join(format!("packshot-chunks-final-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let port = crate::net::test_port();
        let mut h = node(&root, net("h", format!("127.0.0.1:{port}"), Vec::new()), 1, 1, 50);
        let mut x = node(&root, net("x", format!("127.0.0.1:{}", crate::net::test_port()), vec![format!("127.0.0.1:{port}")]), 0, 0, 50);
        // x only watches (no capabilities): connected, not taking part
        tick_until(&mut [&mut h, &mut x], "connected", 15, &mut |n| n[0].connected_nodes().len() == 1 && n[1].connected_nodes().len() == 1);
        let todo = h.add_targets(vec![target("gun_1")]);
        h.start_local(&todo, "test", "", Scope::All, "final", "h").unwrap();
        // as --plain: tick while it runs, then not any more
        while h.running() {
            h.tick();
            x.tick();
            std::thread::sleep(Duration::from_millis(10));
        }
        h.final_status();
        tick_until(&mut [&mut x], "the end on x", 10, &mut |n| n[0].remote_run.as_ref().is_some_and(|r| r.status.finished && r.status.ok == 1));
        close(&mut [&mut h, &mut x]);
        let _ = crate::store::remove_tree(&root);
    }

    /// An assessment aborted: here (the sweep's Blenders gone, nothing saved) and on another node by request (its
    /// requester told)
    #[test]
    fn assess_aborted() {
        let root = std::env::temp_dir().join(format!("packshot-chunks-assess-abort-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let port = crate::net::test_port();
        let mut x = node(&root, net("x", format!("127.0.0.1:{port}"), Vec::new()), 0, 0, 6);
        let mut b = node(&root, net("b", format!("127.0.0.1:{}", crate::net::test_port()), vec![format!("127.0.0.1:{port}")]), 1, 1, 6);
        b.use_targets((0..12).map(|k| target(&format!("slow_{k}"))).collect());
        b.assess_items = 6;
        b.assess_max = 3;
        tick_until(&mut [&mut x, &mut b], "connected", 15, &mut |n| n[0].connected_nodes().len() == 1);
        let sweeping = |e: &Engine| e.log.iter().any(|l| l.text.starts_with("Warm-up: 1 Blender"));
        // here
        b.start_assess(None).unwrap();
        tick_until(&mut [&mut x, &mut b], "sweep going", 20, &mut |n| sweeping(n[1]));
        b.abort_assess(None);
        assert!(!b.assessing() && b.assessment.is_none());
        assert!(b.log.iter().any(|l| l.text == "Assessment aborted"));
        assert!(!b.cfg.work.join("assess").read_dir().unwrap().flatten().any(|d| d.path().join("sweep").exists()), "its scratch gone");
        // on request from x, once x sees b back
        b.log.clear();
        tick_until(&mut [&mut x, &mut b], "b not assessing, seen on x", 15, &mut |n| n[0].connected_nodes().iter().any(|(_, h)| !h.assessing));
        let conn_b = x.conn_of("b").unwrap();
        x.request_assess(conn_b).unwrap();
        tick_until(&mut [&mut x, &mut b], "b assessing, seen on x", 20, &mut |n| sweeping(n[1]) && n[0].connected_nodes().iter().any(|(_, h)| h.assessing));
        x.request_abort_assess(conn_b).unwrap();
        tick_until(&mut [&mut x, &mut b], "aborted, x told", 15, &mut |n| !n[1].assessing() && n[0].log.iter().any(|l| l.text == "b: assessment aborted by x"));
        assert!(b.assessment.is_none());
        close(&mut [&mut x, &mut b]);
        let _ = crate::store::remove_tree(&root);
    }

    /// The processes a node's fake tools started (their pids, one line each)
    fn pids(e: &Engine, kind: &str) -> usize {
        let f = e.cfg.starbreaker.parent().unwrap().join(format!("pids-{kind}"));
        let text = fs::read_to_string(f).unwrap_or_default();
        text.lines().collect::<std::collections::HashSet<_>>().len()
    }

    /// Long-lived processes: one Blender and one export-batch --stdin do every chunk of a run (3 export chunks, 3+
    /// render chunks); a Blender restarts after its job limit and the chunk goes on with the next one; the idle
    /// processes quit when the run is over
    #[test]
    fn persistent_processes() {
        let root = std::env::temp_dir().join(format!("packshot-chunks-persist-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mut h = node(&root, NetConfig { name: "h".into(), ..Default::default() }, 1, 1, 2);
        let names: Vec<String> = (0..6).map(|k| format!("gun_{k}")).collect();
        let todo = h.add_targets(names.iter().map(|n| target(n)).collect());
        h.start_local(&todo, "test", "", Scope::All, "persist", "h").unwrap();
        tick_until(&mut [&mut h], "run over", 30, &mut |n| !n[0].running());
        let run = h.run.as_ref().unwrap();
        assert_eq!((run.ok, run.failed), (6, 0), "{:?}", h.errors);
        assert!(events(&h, "export_end").len() >= 3 && events(&h, "render_end").len() >= 3);
        assert_eq!((pids(&h, "export"), pids(&h, "blender")), (1, 1), "one process of each kind for all chunks");
        assert!(!h.log.iter().any(|l| l.text.contains("export-batch without --stdin")), "the --stdin mode was found");
        // a job limit of 4: the second Blender takes over in the middle of a chunk, nothing is lost
        h.serve.as_mut().unwrap().blender_jobs = 4;
        let todo = h.add_targets((6..16).map(|k| target(&format!("gun_{k}"))).collect());
        h.start_local(&todo, "test", "", Scope::All, "restart", "h").unwrap();
        tick_until(&mut [&mut h], "second run over", 30, &mut |n| !n[0].running());
        let run = h.run.as_ref().unwrap();
        assert_eq!((run.ok, run.failed), (10, 0), "{:?}", h.errors);
        assert!(pids(&h, "blender") >= 4, "restarted after 4 jobs each: {}", pids(&h, "blender"));
        // the run is over: the idle processes quit
        tick_until(&mut [&mut h], "processes gone", 15, &mut |n| n[0].serve.as_ref().unwrap().processes() == 0);
        close(&mut [&mut h]);
        let _ = crate::store::remove_tree(&root);
    }

    /// A process that dies at work: only the job it was on fails, with its last lines (Blender's "Segmentation
    /// fault"); the chunk's other jobs go to a new process, for Blender and for a persistent export-batch alike
    #[test]
    fn crash_fails_only_the_job_in_flight() {
        let root = std::env::temp_dir().join(format!("packshot-chunks-crash-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mut h = node(&root, NetConfig { name: "h".into(), ..Default::default() }, 1, 1, 50);
        let todo = h.add_targets(["gun_1", "crash_r1", "gun_2", "crash_e1", "gun_3", "gun_4"].iter().map(|n| target(n)).collect());
        h.start_local(&todo, "test", "", Scope::All, "crash", "h").unwrap();
        tick_until(&mut [&mut h], "run over", 30, &mut |n| !n[0].running());
        let run = h.run.as_ref().unwrap();
        assert_eq!((run.ok, run.failed), (4, 2), "{:?}", h.errors);
        let err = |entity: &str| h.errors.iter().find(|(t, _)| h.targets[*t].entity == entity).map(|(_, e)| e.clone()).unwrap_or_default();
        assert!(err("crash_r1").starts_with("Blender ended while rendering it"), "{}", err("crash_r1"));
        assert!(err("crash_e1").starts_with("export-batch ended while exporting it"), "{}", err("crash_e1"));
        let trace = events(&h, "failure").into_iter().find(|f| f["entity"] == "crash_r1").unwrap();
        assert!(trace["trace"].as_str().unwrap_or("").contains("Segmentation fault in crash_r"), "{trace}");
        assert_eq!((pids(&h, "export"), pids(&h, "blender")), (2, 2), "one restart each");
        close(&mut [&mut h]);
        let _ = crate::store::remove_tree(&root);
    }

    /// A StarBreaker without --stdin: one export-batch per export chunk over its jobs file, as before; one that
    /// cannot start ({"ready": false}) fails its chunk's items with why
    #[test]
    fn export_without_stdin_and_not_ready() {
        let root = std::env::temp_dir().join(format!("packshot-chunks-once-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mut h = node_with(&root, NetConfig { name: "h".into(), ..Default::default() }, 1, 1, 2, &fake_export_once());
        let todo = h.add_targets((0..6).map(|k| target(&format!("gun_{k}"))).collect());
        h.start_local(&todo, "test", "", Scope::All, "once", "h").unwrap();
        tick_until(&mut [&mut h], "run over", 30, &mut |n| !n[0].running());
        let run = h.run.as_ref().unwrap();
        assert_eq!((run.ok, run.failed), (6, 0), "{:?}", h.errors);
        assert_eq!(pids(&h, "export"), events(&h, "export_end").len(), "one process per export chunk");
        assert!(h.log.iter().any(|l| l.text.contains("export-batch without --stdin")));
        close(&mut [&mut h]);
        let mut d = node_with(&root, NetConfig { name: "d".into(), ..Default::default() }, 1, 1, 50, FAKE_EXPORT_DEAD);
        let todo = d.add_targets(vec![target("gun_1")]);
        d.start_local(&todo, "test", "", Scope::All, "dead", "d").unwrap();
        tick_until(&mut [&mut d], "run over", 30, &mut |n| !n[0].running());
        assert_eq!(d.run.as_ref().map(|r| (r.ok, r.failed)), Some((0, 1)));
        assert!(d.errors[0].1.starts_with("export-batch does not start"), "{}", d.errors[0].1);
        let trace = events(&d, "failure").into_iter().next().unwrap();
        assert!(trace["trace"].as_str().unwrap_or("").contains("not ready: Data.p4k: no such file"), "{trace}");
        close(&mut [&mut d]);
        let _ = crate::store::remove_tree(&root);
    }

    /// Assessments on request with fake tools: x (no capabilities) asks r (renders only) to assess itself; r gets
    /// the test set exported by b (renders and exports) and fetches it from there, sweeps, saves its assessment.json
    /// (the token kept) and announces the result; while it assesses, x's head node work skips it and r refuses a
    /// second one. Then b assesses itself (test set exported here, export sweep as well); its rates size the chunks
    #[test]
    fn assess_on_request() {
        let root = std::env::temp_dir().join(format!("packshot-chunks-assess-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (port, port2) = (crate::net::test_port(), crate::net::test_port());
        let mut x = node(&root, net("x", format!("127.0.0.1:{port}"), Vec::new()), 0, 0, 6);
        let mut b = node(&root, net("b", format!("127.0.0.1:{port2}"), vec![format!("127.0.0.1:{port}")]), 1, 1, 6);
        let mut r = node(&root, net("r", format!("127.0.0.1:{}", crate::net::test_port()), vec![format!("127.0.0.1:{port}"), format!("127.0.0.1:{port2}")]), 1, 0, 6);
        let list: Vec<Target> = (0..12).map(|k| target(&format!("gun_{k}"))).collect();
        for e in [&mut r, &mut b] {
            e.use_targets(list.clone());
            e.assess_items = 6;
            e.assess_max = 3;
        }
        fs::write(r.store.root.join("packshot.json"), r#"{"token": "t", "discovery": false}"#).unwrap();
        tick_until(&mut [&mut x, &mut b, &mut r], "mesh", 15, &mut |n| n[2].connected_nodes().len() == 2 && n[0].usable_nodes().len() == 2);
        let conn_r = x.conn_of("r").unwrap();
        x.request_assess(conn_r).unwrap();
        tick_until(&mut [&mut x, &mut b, &mut r], "r assessing, seen on x", 15, &mut |n| n[2].assessing() && n[0].connected_nodes().iter().any(|(_, h)| h.name == "r" && h.assessing));
        assert!(!x.usable_nodes().iter().any(|(_, h)| h.name == "r"), "an assessing node takes no work");
        assert!(r.start_assess(None).is_err_and(|e| e.to_string().contains("assessment is going")));
        tick_until(&mut [&mut x, &mut b, &mut r], "r assessed", 60, &mut |n| !n[2].assessing());
        for l in &r.log {
            println!("r: {}", l.text);
        }
        let a = r.assessment.clone().expect("r has an assessment");
        assert!(a.workers >= 1 && a.workers <= 3 && a.render > 0.0 && a.exporters.is_none() && a.export_steps.is_empty(), "{a:?}");
        assert_eq!(a.items, 6);
        assert!(r.log.iter().any(|l| l.text.contains("items exported on b")), "the test set came from b");
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(r.store.root.join(crate::assess::FILE)).unwrap()).unwrap();
        assert_eq!(saved["workers"].as_u64(), Some(a.workers as u64));
        assert_eq!(fs::read_to_string(r.store.root.join("packshot.json")).unwrap(), r#"{"token": "t", "discovery": false}"#, "packshot.json untouched");
        assert!(b.serve.as_ref().is_some_and(|s| !s.busy()), "b released the test set's chunk");
        tick_until(&mut [&mut x, &mut b, &mut r], "result on x", 15, &mut |n| {
            n[0].log.iter().any(|l| l.text.starts_with("r: assessment done")) && n[0].connected_nodes().iter().any(|(_, h)| h.name == "r" && h.assess.as_ref().is_some_and(|i| i.valid && i.workers == a.workers))
        });
        // a second assessment takes the kept test set
        r.start_assess(None).unwrap();
        tick_until(&mut [&mut x, &mut b, &mut r], "r assessed again", 60, &mut |n| !n[2].assessing());
        assert!(r.log.iter().any(|l| l.text.starts_with("Test set: kept")), "kept test set used");
        // b: exports the test set itself, then the export sweep with its Blender rendering alongside
        b.start_assess(None).unwrap();
        tick_until(&mut [&mut x, &mut b, &mut r], "b assessed", 60, &mut |n| !n[1].assessing());
        for l in &b.log {
            println!("b: {}", l.text);
        }
        let a = b.assessment.clone().expect("b has an assessment");
        assert!(a.exporters.is_some_and(|e| e >= 1) && a.export.is_some_and(|x| x > 0.0) && !a.export_steps.is_empty(), "{a:?}");
        assert!(b.serve.as_ref().is_some_and(|s| !s.busy()), "the test set left b's holdings");
        // both rates reach x: its chunks follow them
        tick_until(&mut [&mut x, &mut b, &mut r], "rates on x", 15, &mut |n| {
            let usable = n[0].usable_nodes();
            usable.len() == 2 && usable.iter().all(|(_, h)| h.assess.as_ref().is_some_and(|i| i.valid))
        });
        let conn_b = x.conn_of("b").unwrap();
        let todo = x.add_targets(list.clone());
        x.start_local(&todo, "test", "", Scope::All, "rates", "x").unwrap();
        x.paused = true;
        let (sb, sr) = (x.render_size_for(conn_b), x.render_size_for(x.conn_of("r").unwrap()));
        println!("render chunks: b {sb}, r {sr}");
        assert!(sb >= 1 && sr >= 1);
        close(&mut [&mut x, &mut b, &mut r]);
        let _ = crate::store::remove_tree(&root);
    }
}

/// Runs with real tools (SC_DATA_P4K must be set; Blender in local/blender, StarBreaker in local/starbreaker-qm)
#[cfg(test)]
mod real_tests {
    use super::*;
    use crate::config::NetConfig;
    use crate::engine::{Config, Scope, Target};
    use crate::net::Hub;
    use crate::store::Store;
    use std::time::{Duration, Instant};

    /// A node with real tools and the given capabilities; its DataCore tables from a copy of the repo's
    fn real(root: &Path, name: &str, listen: String, peers: Vec<String>, render: bool, export: bool) -> Engine {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let p4k = PathBuf::from(std::env::var("SC_DATA_P4K").expect("SC_DATA_P4K"));
        let starbreaker = repo.join("local/starbreaker-qm").join(crate::exe("starbreaker"));
        let blender = fs::read_dir(repo.join("local/blender")).unwrap().flatten().map(|e| e.path().join(crate::exe("blender"))).find(|p| p.exists()).unwrap();
        let targets: Vec<Target> = serde_json::from_slice(&fs::read(repo.join("local/packshot/targets.json")).unwrap()).unwrap();
        let dir = root.join(name);
        let store = Store::open(&dir.join("store")).unwrap();
        let set = store.load_set(LIVE).unwrap();
        let caps = Caps { render, export, blender: Some(blender.as_path()), starbreaker: Some(starbreaker.as_path()), p4k: Some(p4k.as_path()) };
        let mut me = own_hello(name, caps, if render { 2 } else { 0 }, if export { 2 } else { 0 }, 8, Vec::new());
        me.name = name.into();
        let hub = Hub::new(NetConfig { name: name.into(), token: "t".into(), listen, peers, discovery: false }, me.clone(), dir.join("inbox"));
        let tables = if export { crate::cas::tests::repo_tables(&dir.join("work")) } else { crate::tables::Tables::default() };
        let cfg = Config { p4k, starbreaker, blender, render_script: repo.join("src/icons/render.py"), work: dir.join("work"), size: 128, chunk: 50 };
        let mut e = Engine::new(cfg, targets, tables, store, set, Some(hub), me).unwrap();
        e.max_renderers = if render { 2 } else { 0 };
        e.max_exporters = if export { 2 } else { 0 };
        e
    }

    fn run_with(nodes: &mut [Engine], filter: &str, expect: usize) {
        let t0 = Instant::now();
        let others = nodes.len() - 1;
        while nodes[0].usable_nodes().len() < others || nodes[0].capacity_problem().is_some() {
            assert!(t0.elapsed() < Duration::from_secs(15), "nodes not usable");
            for b in nodes.iter_mut() {
                b.tick();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(nodes[0].state_problems().is_empty(), "same state");
        let sel = nodes[0].select(filter, Scope::All);
        assert_eq!(sel.todo.len(), expect);
        let me = nodes[0].me.name.clone();
        nodes[0].start_local(&sel.todo, "test", "", Scope::All, "chunks", &me).unwrap();
        while nodes[0].running() {
            assert!(t0.elapsed() < Duration::from_secs(240), "timeout");
            for b in nodes.iter_mut() {
                b.tick();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let a = &nodes[0];
        for l in &a.log {
            println!("{}", l.text);
        }
        let run = a.run.as_ref().unwrap();
        assert_eq!((run.ok, run.failed), (expect, 0));
        assert!(a.history.iter().all(|t| a.image(*t).is_some()), "images in the head node's set");
        let text = fs::read_to_string(a.run_dir().unwrap().join("events.jsonl")).unwrap();
        for l in text.lines().filter(|l| l.contains("\"ev\":\"fetch\"") || l.contains("\"ev\":\"render_start\"")) {
            println!("{l}");
        }
        for b in nodes.iter_mut() {
            b.hub.as_mut().unwrap().close_all();
        }
    }

    /// The head node without capabilities hands the whole run to a node that exports and renders (everything stays
    /// there, nothing is fetched): cargo test --release -- --ignored run_on_another_node
    #[test]
    #[ignore]
    fn run_on_another_node() {
        let root = std::env::temp_dir().join(format!("packshot-run-node-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let port = crate::net::test_port();
        let a = real(&root, "a", format!("127.0.0.1:{port}"), Vec::new(), false, false);
        let b = real(&root, "b", format!("127.0.0.1:{}", crate::net::test_port()), vec![format!("127.0.0.1:{port}")], true, true);
        run_with(&mut [a, b], "rock_gold rock_aluminum klwe_pistol_energy_01_tan01", 3);
        let _ = crate::store::remove_tree(&root);
    }

    /// Export and render on different nodes: e exports without Blender and holds the items, r renders them with the
    /// files it fetches from e straight (the nodes form a mesh, as discovery makes them; real tools): cargo test
    /// --release -- --ignored export_here_render_there
    #[test]
    #[ignore]
    fn export_here_render_there() {
        let root = std::env::temp_dir().join(format!("packshot-relay-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (port, port2) = (crate::net::test_port(), crate::net::test_port());
        let head = real(&root, "head", format!("127.0.0.1:{port}"), Vec::new(), false, false);
        let e = real(&root, "e", format!("127.0.0.1:{port2}"), vec![format!("127.0.0.1:{port}")], false, true);
        let r = real(&root, "r", format!("127.0.0.1:{}", crate::net::test_port()), vec![format!("127.0.0.1:{port}"), format!("127.0.0.1:{port2}")], true, false);
        let mut nodes = [head, e, r];
        let t0 = Instant::now();
        while nodes[2].connected_nodes().len() < 2 {
            assert!(t0.elapsed() < Duration::from_secs(15), "no mesh");
            for b in nodes.iter_mut() {
                b.tick();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        run_with(&mut nodes, "rock_gold rock_aluminum klwe_pistol_energy_01_tan01", 3);
        let _ = crate::store::remove_tree(&root);
    }
}

#[cfg(test)]
mod p4k_tests {
    /// Fingerprint and version of the real Data.p4k (SC_DATA_P4K): cargo test --release -- --ignored p4k_fingerprint --nocapture
    #[test]
    #[ignore]
    fn p4k_fingerprint() {
        let p4k = std::path::PathBuf::from(std::env::var("SC_DATA_P4K").expect("SC_DATA_P4K"));
        let t0 = std::time::Instant::now();
        let id = super::p4k_id(&p4k).expect("id");
        println!("id {id} in {:.0} ms, version {:?}", t0.elapsed().as_secs_f64() * 1000.0, super::p4k_version(&p4k));
        assert_eq!(super::p4k_id(&p4k).unwrap(), id, "stable");
    }
}

#[cfg(test)]
mod host_tests {
    #[test]
    fn host_and_gpu() {
        let (cpu, ram) = super::host_info();
        println!("cpu {cpu:?}, ram {ram:?}, gpu {:?}", super::gpu_info());
        assert!(cpu.is_some() && ram.is_some());
    }
}
