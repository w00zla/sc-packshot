// README screenshots of the TUI. A synthetic state goes through the real code paths (Engine::start, push_worker,
// on_item, on_block_done, tick): three nodes on loopback, an earlier run, an experiment set, a run going on with its
// workers, procedural "renders" as its images. The real draw() writes into ratatui's TestBackend, and the buffer is
// rasterized here: box-drawing, block and sparkline characters as geometry, the rest with a monospace font. Written
// as PNG (palette when 256 colours do).
//
//   PACKSHOT_SHOT_FONT=<regular.ttf>[<path separator><fallback.ttf>...] cargo test --release tui_screenshots -- --ignored
//
// The first font should be a variable font with a wght axis (bold from it), the others fill in missing glyphs.
// PACKSHOT_SHOT_RENDERS=<a set folder> (optional) takes the images from real renders there, picked by class name
// per kind; without it the images are procedural.
// Writes docs/assets/tui-run.png, tui-sets.png, tui-nodes.png; optimize them afterwards with oxipng.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ab_glyph::{Font, FontVec, PxScale, ScaleFont, VariableFont, point};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

use super::*;
use crate::config::NetConfig;
use crate::engine::{Config, Target};
use crate::net::{AssessInfo, DiskInfo, Hub};
use crate::node::LOCAL;
use crate::store::Store;
use crate::tables::Tables;

const COLS: u16 = 200;
const ROWS: u16 = 50;

// ---------------------------------------------------------------------------------------------------------------
// procedural renders

/// Signed distance and material of one shape family; materials: 0 base, 1 accent (tinted), 2 glow
type Sdf = fn([f32; 3]) -> (f32, u8);

fn len(v: [f32; 3]) -> f32 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}
fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn norm(v: [f32; 3]) -> [f32; 3] {
    let l = len(v).max(1e-6);
    [v[0] / l, v[1] / l, v[2] / l]
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn rbox(p: [f32; 3], b: [f32; 3], r: f32) -> f32 {
    let q = [p[0].abs() - b[0] + r, p[1].abs() - b[1] + r, p[2].abs() - b[2] + r];
    len([q[0].max(0.0), q[1].max(0.0), q[2].max(0.0)]) + q[0].max(q[1]).max(q[2]).min(0.0) - r
}
fn capsule(p: [f32; 3], h: f32, r: f32) -> f32 {
    let y = p[1].clamp(-h, h);
    len([p[0], p[1] - y, p[2]]) - r
}
fn smin(a: f32, b: f32, k: f32) -> f32 {
    let h = (0.5 + 0.5 * (b - a) / k).clamp(0.0, 1.0);
    b + (a - b) * h - k * h * (1.0 - h)
}

fn helmet(p: [f32; 3]) -> (f32, u8) {
    let shell = smin(len(sub(p, [0.0, 0.08, 0.0])) - 0.62, rbox(sub(p, [0.0, -0.38, 0.05]), [0.42, 0.18, 0.42], 0.12), 0.18);
    let visor = rbox(sub(p, [0.0, 0.02, 0.45]), [0.44, 0.16, 0.22], 0.12);
    if visor < shell + 0.02 { (visor.min(shell), 2) } else if p[1] > 0.42 && p[0].abs() < 0.12 { (shell, 1) } else { (shell, 0) }
}
fn crate_box(p: [f32; 3]) -> (f32, u8) {
    let d = rbox(p, [0.72, 0.48, 0.5], 0.06);
    let band = (p[0].abs() - 0.62).abs() < 0.05 || (p[1] - 0.0).abs() < 0.04;
    (d, if band { 1 } else { 0 })
}
fn canister(p: [f32; 3]) -> (f32, u8) {
    let d = capsule(p, 0.42, 0.34);
    let cap = rbox(sub(p, [0.0, 0.8, 0.0]), [0.14, 0.08, 0.14], 0.03);
    let ring = (p[1].abs() - 0.22).abs() < 0.04;
    if cap < d { (cap, 1) } else { (d, if ring { 2 } else { 0 }) }
}
fn rifle(p: [f32; 3]) -> (f32, u8) {
    let body = rbox(sub(p, [-0.05, 0.0, 0.0]), [0.62, 0.11, 0.07], 0.03);
    let barrel = capsule([p[1], p[0] - 0.78, p[2]], 0.22, 0.035);
    let grip = rbox(sub(p, [-0.12, -0.24, 0.0]), [0.06, 0.16, 0.05], 0.02);
    let stock = rbox(sub(p, [-0.78, -0.03, 0.0]), [0.18, 0.13, 0.06], 0.04);
    let scope = capsule([p[1] - 0.2, p[0] + 0.05, p[2]], 0.2, 0.05);
    let mag = rbox(sub(p, [0.18, -0.25, 0.0]), [0.05, 0.15, 0.04], 0.01);
    let d = body.min(barrel).min(grip).min(stock).min(mag);
    if scope < d { (scope, 2) } else if mag < d + 0.001 || stock < d + 0.001 { (d, 1) } else { (d, 0) }
}
fn backpack(p: [f32; 3]) -> (f32, u8) {
    let main = rbox(p, [0.42, 0.6, 0.24], 0.14);
    let pouch = rbox(sub(p, [0.0, -0.18, 0.26]), [0.3, 0.22, 0.1], 0.08);
    let side = rbox([p[0].abs() - 0.48, p[1] + 0.05, p[2]], [0.08, 0.3, 0.16], 0.05);
    let d = main.min(pouch).min(side);
    if pouch < main && pouch < side { (d, 1) } else if (p[1] - 0.42).abs() < 0.03 { (d, 2) } else { (d, 0) }
}
fn core(p: [f32; 3]) -> (f32, u8) {
    let ring = len([len([p[0], 0.0, p[2]]) - 0.52, p[1], 0.0]) - 0.16;
    let ball = len(p) - 0.26;
    if ball < ring { (ball, 2) } else { (ring, if p[1] > 0.06 { 1 } else { 0 }) }
}

const SHAPES: [Sdf; 6] = [helmet, crate_box, canister, rifle, backpack, core];

fn hsv(h: f32, s: f32, v: f32) -> [f32; 3] {
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - f * s), v * (1.0 - (1.0 - f) * s));
    match i as i32 % 6 {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

/// One procedural item render as PNG bytes: `kind` picks the shape, `seed` the colours and the turn
fn render_item(kind: usize, seed: u32, size: u32) -> Vec<u8> {
    let sdf = SHAPES[kind % SHAPES.len()];
    let rnd = |k: u32| ((seed.wrapping_mul(2_654_435_761).wrapping_add(k.wrapping_mul(40_503)) >> 8) % 1000) as f32 / 1000.0;
    let base = hsv(rnd(1), 0.15 + 0.35 * rnd(2), 0.35 + 0.4 * rnd(3));
    let accent = hsv((rnd(1) + 0.45 + 0.2 * rnd(4)) % 1.0, 0.65, 0.75);
    let glow = hsv(0.48 + 0.12 * rnd(5), 0.8, 1.0);
    let yaw = -0.6 + 0.25 * rnd(6);
    let (cy, sy) = (yaw.cos(), yaw.sin());
    let (cp, sp) = (0.94f32, -0.33f32); // the camera looks down a little
    let rot = |q: [f32; 3]| {
        // into the object's frame: pitch, then yaw
        let y = q[1] * cp - q[2] * sp;
        let z = q[1] * sp + q[2] * cp;
        [q[0] * cy + z * sy, y, -q[0] * sy + z * cy]
    };
    let scene = |q: [f32; 3]| sdf(rot(q));
    let light = norm([-0.5, 0.75, 0.6]);
    let rim = norm([0.6, 0.3, -0.8]);
    let ss = 2u32;
    let n = size * ss;
    let mut out = image::RgbaImage::new(size, size);
    for py in 0..size {
        for px in 0..size {
            let (mut acc, mut cov) = ([0.0f32; 3], 0.0f32);
            for sy_ in 0..ss {
                for sx_ in 0..ss {
                    let u = ((px * ss + sx_) as f32 + 0.5) / n as f32 * 2.0 - 1.0;
                    let v = 1.0 - ((py * ss + sy_) as f32 + 0.5) / n as f32 * 2.0;
                    // orthographic, front view
                    let mut p = [u * 1.05, v * 1.05, 2.0];
                    let dir = [0.0, 0.0, -1.0];
                    let mut hit = None;
                    for _ in 0..96 {
                        let (d, m) = scene(p);
                        if d < 0.002 {
                            hit = Some(m);
                            break;
                        }
                        p = [p[0] + dir[0] * d, p[1] + dir[1] * d, p[2] + dir[2] * d];
                        if p[2] < -2.0 {
                            break;
                        }
                    }
                    let Some(m) = hit else { continue };
                    let e = 0.002;
                    let nrm = norm([
                        scene([p[0] + e, p[1], p[2]]).0 - scene([p[0] - e, p[1], p[2]]).0,
                        scene([p[0], p[1] + e, p[2]]).0 - scene([p[0], p[1] - e, p[2]]).0,
                        scene([p[0], p[1], p[2] + e]).0 - scene([p[0], p[1], p[2] - e]).0,
                    ]);
                    let diff = dot(nrm, light).max(0.0);
                    let half = norm([light[0], light[1], light[2] + 1.0]);
                    let spec = dot(nrm, half).max(0.0).powf(if m == 2 { 90.0 } else { 28.0 }) * if m == 2 { 1.2 } else { 0.45 };
                    let rimv = dot(nrm, rim).max(0.0).powf(2.0) * 0.35 + (1.0 - nrm[2].max(0.0)).powf(3.0) * 0.25;
                    let albedo = match m {
                        0 => base,
                        1 => accent,
                        _ => [glow[0] * 0.25, glow[1] * 0.25, glow[2] * 0.25],
                    };
                    let emit = if m == 2 { 0.55 } else { 0.0 };
                    for c in 0..3 {
                        let g = if m == 2 { glow[c] } else { 1.0 };
                        acc[c] += albedo[c] * (0.12 + 0.9 * diff) + spec + rimv * 0.6 + emit * g;
                    }
                    cov += 1.0;
                }
            }
            let k = (ss * ss) as f32;
            if cov > 0.0 {
                let px_rgb: Vec<u8> = acc.iter().map(|v| {
                    let lin = v / cov;
                    let mapped = lin / (1.0 + 0.35 * lin); // soft shoulder
                    (mapped.clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0).round() as u8
                }).collect();
                out.put_pixel(px, py, image::Rgba([px_rgb[0], px_rgb[1], px_rgb[2], (cov / k * 255.0).round() as u8]));
            }
        }
    }
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(out).write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();
    bytes
}

// ---------------------------------------------------------------------------------------------------------------
// synthetic state

const MAKERS: [&str; 6] = ["aurel", "vexa", "korin", "tessar", "ombra", "halden"];
const VARIANTS: [&str; 8] = ["", "ember", "frost", "dusk", "slate", "verdant", "cobalt", "ash"];

/// (type, subtype, word, shape)
const KINDS: [(&str, &str, &str, usize); 8] = [
    ("Char_Armor_Helmet", "Heavy", "helmet_heavy", 0),
    ("Char_Armor_Helmet", "Light", "helmet_light", 0),
    ("Char_Armor_Backpack", "Medium", "backpack", 4),
    ("WeaponPersonal", "Rifle", "rifle", 3),
    ("WeaponPersonal", "Sniper", "marksman", 3),
    ("WeaponPersonal", "Pistol", "pistol", 1),
    ("WeaponPersonal", "SMG", "smg", 2),
    ("Char_Armor_Torso", "Heavy", "core", 5),
];

/// Per shape: the class-name part real renders of that kind carry (PACKSHOT_SHOT_RENDERS)
const SAMPLE_KEYS: [&str; 6] = ["_helmet_", "_pistol_", "_smg_", "_rifle_", "_backpack_", "_core_"];

/// `per` real renders per shape from a set folder, laid out like the procedural ones (shape + variant × shapes);
/// a shape without matches stays procedural
fn sample_renders(dir: &Path, per: usize, procedural: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.ends_with(".png")).collect();
    names.sort();
    let mut out = procedural.to_vec();
    for (s, key) in SAMPLE_KEYS.iter().enumerate() {
        // the class name in parentheses; magazines and attachments are no items to show
        let hits: Vec<&String> = names.iter().filter(|n| n.to_lowercase().contains(key) && !n.contains("_mag") && !n.contains("(20 Cap)")).collect();
        if hits.is_empty() {
            continue;
        }
        for v in 0..per {
            let n = hits[(v * 7919 + 13) % hits.len()];
            out[s + v * SHAPES.len()] = std::fs::read(dir.join(n)).unwrap();
        }
    }
    out
}

fn title(s: &str) -> String {
    s.split('_').filter(|w| !w.is_empty()).map(|w| {
        let mut c = w.chars();
        c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
    }).collect::<Vec<_>>().join(" ")
}

/// Fictional targets, n of them, spread over the kinds
fn targets(n: usize) -> Vec<Target> {
    (0..n)
        .map(|i| {
            let (kind, sub, word, _) = KINDS[i % KINDS.len()];
            let maker = MAKERS[(i / KINDS.len()) % MAKERS.len()];
            let num = (i / (KINDS.len() * MAKERS.len())) % 12 + 1;
            let var = VARIANTS[(i * 7 / 3) % VARIANTS.len()];
            let entity = if var.is_empty() { format!("{maker}_{word}_{num:02}") } else { format!("{maker}_{word}_{num:02}_{var}") };
            let shown = format!("{} {}{}", title(maker), title(word), if var.is_empty() { format!(" Mk {num}") } else { format!(" \"{}\"", title(var)) });
            Target { uuids: vec![format!("{i:08x}-0000-4000-8000-{i:012x}")], names: Some(vec![shown]), entity, kind: kind.into(), sub_type: sub.into(), worn: false }
        })
        .collect()
}

fn shape_of(t: usize) -> usize {
    KINDS[t % KINDS.len()].3
}

#[allow(clippy::too_many_arguments)]
fn hello(name: &str, os: &str, render: bool, export: bool, slots: usize, exporters: usize, cores: usize, cpu: &str, gpu: &str, ram_gb: u64, free_gb: u64, total_gb: u64) -> Hello {
    Hello {
        name: name.into(),
        version: "0.1.0+3f9c2ab1".into(),
        os: os.into(),
        render,
        export,
        blender: "5.2.3".into(),
        addon: Some("9e41c7d2a0".into()),
        slots,
        exporters,
        cores,
        cpu: Some(cpu.into()),
        ram: Some(ram_gb << 30),
        gpu: Some(gpu.into()),
        p4k_bytes: export.then_some(146_300_000_000),
        p4k_id: export.then(|| "a7c3e91f0b2d4e65".into()),
        p4k_version: export.then(|| "4.3.1 (4.3.141.9927)".into()),
        starbreaker: Some("fef53fd0".into()),
        disks: vec![DiskInfo { mount: if os == "windows" { "D:\\".into() } else { "/srv".into() }, free: free_gb * 1_000_000_000, total: total_gb * 1_000_000_000 }],
        ..Default::default()
    }
}

/// The render of item `t`: one of those of its shape
fn image_of(images: &[Vec<u8>], t: usize) -> &[u8] {
    // the variant from maker and number, so neighbours in a sorted list differ
    let v = (t / KINDS.len() + t / (KINDS.len() * MAKERS.len()) * 2) % (images.len() / SHAPES.len());
    &images[shape_of(t) + v * SHAPES.len()]
}

/// Simulates `items` of a render chunk on `conn` (None: this node) through on_item, then ends the chunk
fn render_chunk(e: &mut Engine, chunk: u64, node: Option<&str>, conn: u64, items: &[usize], images: &[Vec<u8>], fail: &[usize]) {
    e.push_worker(Kind::Render, chunk, node.map(String::from), conn, items.to_vec());
    if let Some(w) = e.workers.iter_mut().find(|w| w.chunk == chunk && w.conn == conn) {
        w.fetched = true;
    }
    for &t in items {
        let ok = !fail.contains(&t);
        let j = |a: f64, b: u32| a * (0.85 + 0.3 * (((t as u32).wrapping_mul(b) % 100) as f64 / 100.0));
        let phases = [j(0.38, 7), j(0.11, 13), j(1.62, 17), j(0.27, 19), j(0.08, 23)];
        let info = if ok {
            format!("timing load={:.3} prep={:.3} render={:.3} glass={:.3} post={:.3}", phases[0], phases[1], phases[2], phases[3], phases[4])
        } else {
            "Blender: the package has no mesh (export empty?)".into()
        };
        // the item took its phases plus Blender's own overhead
        if let Some(w) = e.workers.iter_mut().find(|w| w.chunk == chunk && w.conn == conn) {
            w.since = Instant::now() - Duration::from_secs_f64(phases.iter().sum::<f64>() + 0.31);
        }
        e.on_item(conn, chunk, t, ok, info, image_of(images, t));
    }
    e.on_block_done(conn, chunk, Some(0), String::new());
}

/// A worker left running: `done` of its items reported, started `secs` ago
#[allow(clippy::too_many_arguments)]
fn running_worker(e: &mut Engine, kind: Kind, chunk: u64, node: Option<&str>, conn: u64, items: &[usize], done: usize, secs: f64) {
    e.push_worker(kind, chunk, node.map(String::from), conn, items.to_vec());
    let w = e.workers.iter_mut().find(|w| w.chunk == chunk && w.conn == conn).unwrap();
    w.fetched = true;
    for &t in items.iter().take(done) {
        w.reported.insert(t);
    }
    w.since = Instant::now() - Duration::from_secs_f64(secs);
}

/// Spreads timestamps evenly over [start, now), a little jitter, keeps the count
fn spread(v: &mut [Instant], start: Instant, now: Instant) {
    let span = now.duration_since(start).as_secs_f64();
    let n = v.len().max(1) as f64;
    for (i, t) in v.iter_mut().enumerate() {
        let jitter = ((i * 37) % 11) as f64 / 11.0 - 0.5;
        *t = start + Duration::from_secs_f64(((i as f64 + 0.5 + jitter * 0.6) / n * span).clamp(0.0, span - 0.01));
    }
}

/// Ends the run going on through the engine's own end (tick: nothing left to do)
fn finish(e: &mut Engine, minutes: f64) {
    if let Some(r) = e.run.as_mut() {
        r.todo.clear();
        r.pool.clear();
        r.started = Instant::now() - Duration::from_secs_f64(minutes * 60.0);
    }
    e.tick();
    // run.json: its start back by the same time, so the Sets view shows a real duration
    if let Some(info) = fixed_origin(e)
        && let Ok(t) = chrono::DateTime::parse_from_rfc3339(&info.started)
    {
        let mut info = info;
        info.started = (t - chrono::Duration::seconds((minutes * 60.0) as i64)).to_rfc3339();
        e.store.save_run(&info).unwrap();
    }
}

/// The synthetic state has no render.py, profile or Blender to read: fixed values in the current run's record (on
/// disk for the runs table, in memory for the run tile), finished or not; the record as saved
fn fixed_origin(e: &mut Engine) -> Option<crate::store::RunInfo> {
    let id = e.record.as_ref().map(|r| r.id.clone())?;
    let mut info = e.store.run(&id).unwrap().clone();
    info.origin["render_py"] = "6da3cb0c523f77d7".into();
    info.origin["blender_version"] = "5.2.3".into();
    info.origin["profile_hash"] = "3f9c0e7a51d2b864".into();
    info.origin["size"] = 256.into();
    // the Data.p4k's date in the run tile (2026-11-04)
    info.origin["p4k_mtime"] = 1_793_779_200u64.into();
    if let Some(r) = e.record.as_mut() {
        r.origin = info.origin.clone();
    }
    e.store.save_run(&info).unwrap();
    Some(info)
}

struct Scene {
    app: App,
    root: PathBuf,
}

fn scene(cell: (u16, u16)) -> Scene {
    let root = std::env::temp_dir().join(format!("packshot-shots-{}", std::process::id()));
    let _ = crate::store::remove_tree(&root);
    let store = Store::open(&root.join("store")).unwrap();
    let all = targets(6734);
    let main = store.load_set(MAIN).unwrap();
    let tfile = root.join("targets").join("targets.json");
    std::fs::create_dir_all(tfile.parent().unwrap()).unwrap();
    std::fs::write(&tfile, serde_json::to_vec(&all).unwrap()).unwrap();
    let mut main = main;
    store.set_targets(&mut main, &tfile).unwrap();

    let me = hello("atlas", "windows", true, true, 6, 2, 8, "AMD Ryzen 7 9800X3D", "RTX 5080 · VRAM 16 GB", 64, 612, 2000);
    let net = NetConfig { name: "atlas".into(), listen: "0.0.0.0:47950".into(), ..Default::default() };
    let mut hub = Hub::new(net, me.clone(), root.join("inbox"));
    let mut borealis = hello("borealis", "linux", true, true, 8, 4, 16, "AMD Ryzen 9 7950X", "RX 7900 XTX · VRAM 24 GB", 128, 1430, 3840);
    let mut cinder = hello("cinder", "windows", true, false, 4, 0, 6, "Intel Core i5-12400", "RTX 3060 · VRAM 12 GB", 32, 188, 1000);
    let info = |workers, render, exporters, export, steps: Vec<(usize, f64)>, export_steps: Vec<(usize, f64)>| AssessInfo { render, export, workers, exporters, valid: true, date: "2026-11-06 21:14".into(), steps, export_steps };
    borealis.assess = Some(info(8, 9.4, 4, 13.8, vec![(1, 1.6), (2, 3.1), (4, 6.0), (6, 8.2), (8, 9.4)], vec![(1, 4.1), (2, 7.9), (4, 13.8)]));
    cinder.assess = Some(info(4, 3.2, 0, 0.0, vec![(1, 1.1), (2, 2.0), (3, 2.8), (4, 3.2)], Vec::new()));
    let c_bor = hub.add_fake_peer(borealis, "192.0.2.21:47950", "192.0.2.21", true);
    let c_cin = hub.add_fake_peer(cinder, "192.0.2.34:47950", "192.0.2.34", false);

    let x = root.join("x");
    let rig = root.join("inventorylightgroup_studio_a.json");
    std::fs::write(&rig, "{}").unwrap();
    let cfg = Config { p4k: x.clone(), starbreaker: x.clone(), blender: x.clone(), render_script: x.clone(), work: root.join("work"), size: None, format: None, profile: Ok(crate::profile::Profile::stub(&x)), chunk: 50, cache_limit: 0 };
    let mut e = Engine::new(cfg, all.clone(), Tables::with_rig(rig), store, main, Some(hub), me).unwrap();
    e.max_renderers = 6;
    e.max_exporters = 2;
    e.cores = 8;
    e.targets_file = tfile;
    // no render.py or profile files to hash here: fixed values for the profile lines
    e.profile_info["render_py"] = "6da3cb0c523f77d7".into();
    e.profile_info["profile_hash"] = "3f9c0e7a51d2b864".into();
    e.profile_info["size"] = 256.into();
    e.assessment = Some(crate::assess::Assessment { workers: 6, render: 6.3, exporters: Some(2), export: Some(7.1), date: "2026-11-06T20:41:00+01:00".into(), ..Default::default() });

    // the procedural renders, a few per shape
    let mut images: Vec<Vec<u8>> = (0..30).map(|i| render_item(i % SHAPES.len(), 11 + i as u32 * 7, 256)).collect();
    if let Some(dir) = std::env::var_os("PACKSHOT_SHOT_RENDERS") {
        images = sample_renders(Path::new(&dir), images.len() / SHAPES.len(), &images);
    }
    let nodes = vec!["borealis".to_string(), "cinder".to_string()];

    // 1. an earlier run over the helmets and backpacks, finished
    let first: Vec<usize> = (0..all.len()).filter(|t| matches!(t % KINDS.len(), 0..=2)).take(60).collect();
    e.start(&first, "packshot --filter helmet,backpack", "helmet,backpack", Scope::Missing, "helmets", nodes.clone(), "atlas", crate::engine::RunLook::of(crate::profile::Profile::stub(&root))).unwrap();
    let mut chunk = 1u64;
    for part in first.chunks(12) {
        render_chunk(&mut e, chunk, None, LOCAL, part, &images, &[]);
        chunk += 1;
    }
    finish(&mut e, 6.5);

    // 2. an experiment set off main with its own run
    e.store.create_set("glow-test", MAIN).unwrap();
    e.switch_set("glow-test").unwrap();
    let glow: Vec<usize> = (0..all.len()).filter(|t| t % KINDS.len() == 7).take(16).collect();
    e.start(&glow, "packshot --filter core --scope all --set glow-test", "core", Scope::All, "glow", Vec::new(), "atlas", crate::engine::RunLook::of(crate::profile::Profile::stub(&root))).unwrap();
    for part in glow.chunks(8) {
        render_chunk(&mut e, chunk, None, LOCAL, part, &images, &[]);
        chunk += 1;
    }
    finish(&mut e, 2.2);
    e.switch_set(MAIN).unwrap();

    // 3. the full run going on: everything missing in main, on all three nodes
    let sel = e.select("", Scope::Missing);
    let todo = sel.todo.clone();
    e.start(&todo, "packshot --name full", "", Scope::Missing, "full", nodes, "atlas", crate::engine::RunLook::of(crate::profile::Profile::stub(&root))).unwrap();
    fixed_origin(&mut e);
    let done_n = 1284;
    let fails = [todo[211], todo[730], todo[1102]];
    let hosts: [(Option<&str>, u64); 3] = [(None, LOCAL), (Some("borealis"), c_bor), (Some("cinder"), c_cin)];
    for (k, part) in todo[..done_n].chunks(20).enumerate() {
        // borealis renders the most, cinder the least
        let (node, conn) = hosts[[0, 1, 1, 0, 1, 2][k % 6]];
        render_chunk(&mut e, chunk, node, conn, part, &images, &fails);
        chunk += 1;
    }
    // the last renders here, ending on a helmet for the preview
    let mut tail: Vec<usize> = todo[done_n..done_n + 6].to_vec();
    if let Some(h) = todo[done_n + 6..].iter().position(|t| shape_of(*t) == 0) {
        tail.push(todo[done_n + 6 + h]);
    }
    render_chunk(&mut e, chunk, None, LOCAL, &tail, &images, &[]);
    chunk += 1;
    let rendered: std::collections::HashSet<usize> = tail.iter().copied().chain(todo[..done_n].iter().copied()).collect();
    let open: Vec<usize> = todo.iter().copied().filter(|t| !rendered.contains(t)).collect();
    let mut next = 0;
    let mut take = |n: usize| {
        let v = open[next..next + n].to_vec();
        next += n;
        v
    };
    // workers going on: exports and renders on every node
    // (kind, node, connection, items, reported, seconds since the last item)
    type Running<'a> = (Kind, Option<&'a str>, u64, Vec<usize>, usize, f64);
    let mut ws: Vec<Running> = Vec::new();
    for (i, d, s) in [(0usize, 14usize, 41.2f64), (1, 3, 9.8)] {
        let _ = i;
        ws.push((Kind::Export, None, LOCAL, take(50), d, s));
    }
    for (d, s) in [(7, 1.4), (2, 0.9), (11, 2.1), (5, 0.3), (9, 1.7), (0, 0.6)] {
        ws.push((Kind::Render, None, LOCAL, take(24), d, s));
    }
    for (d, s) in [(22, 18.4), (6, 4.0), (31, 27.5), (17, 12.2)] {
        ws.push((Kind::Export, Some("borealis"), c_bor, take(50), d, s));
    }
    for (d, s) in [(4, 1.1), (13, 0.4), (8, 2.6), (19, 1.9), (1, 0.8), (10, 1.2), (15, 0.2), (3, 2.2)] {
        ws.push((Kind::Render, Some("borealis"), c_bor, take(24), d, s));
    }
    for (d, s) in [(6, 2.4), (2, 1.0), (9, 3.1), (4, 0.7)] {
        ws.push((Kind::Render, Some("cinder"), c_cin, take(24), d, s));
    }
    for (kind, node, conn, items, d, s) in ws {
        running_worker(&mut e, kind, chunk, node, conn, &items, d, s);
        chunk += 1;
    }
    // the pool: exported items waiting for a Blender, held by the nodes that exported them
    {
        let in_chunks = next;
        let rest: Vec<usize> = open[next..next + 1412].to_vec();
        next += rest.len();
        let run = e.run.as_mut().unwrap();
        for (k, &t) in rest.iter().enumerate() {
            let (holder, conn, x) = if k % 3 == 0 { ("atlas", LOCAL, 9001u64) } else { ("borealis", c_bor, 9002u64) };
            run.xchunks.entry(x).or_insert(crate::engine::XChunk { node: holder.into(), conn, open: 0 }).open += 1;
            run.held.insert(t, x);
            if k < 1347 {
                run.pool.push_back((t, Instant::now()));
            }
        }
        // what left the queue so far: rendered, in a chunk, in the pool
        let gone: std::collections::HashSet<usize> = rendered.iter().copied().chain(open[..next].iter().copied()).collect();
        run.todo.retain(|t| !gone.contains(t));
        run.exported = rendered.len() + in_chunks - 6 * 50 + rest.len();
        let now = Instant::now();
        let start = now - Duration::from_secs(17 * 60 + 12);
        run.started = start;
        spread(&mut run.done_at, start, now);
        for v in run.rendered_by.values_mut() {
            spread(v, start, now);
        }
    }
    // log times like a run of 17 minutes: one line per second or so, the newest now
    let n = e.log.len();
    let now = chrono::Local::now();
    for (i, l) in e.log.iter_mut().enumerate() {
        let back = ((n - 1 - i) as f64 * 0.8) as i64;
        l.time = (now - chrono::Duration::seconds(back)).format("%H:%M:%S").to_string();
    }

    // a finished run on borealis that this node has not fetched yet
    e.peer_runs.insert(
        "borealis".into(),
        vec![crate::net::RunSummary { id: "2026-11-06_22-10_weapons".into(), name: "weapons".into(), set: MAIN.into(), node: "borealis".into(), finished: true, images: 412, bytes: 61_400_000 }],
    );

    let args = Args { filter: String::new(), scope: Scope::Missing, plain: false, serve: false, dry_run: false, workers: 6, exporters: 2, name: String::new(), line: String::new() };
    // half blocks sized like the real cells: the images then take the area a Sixel terminal gives them, and the
    // rasterizer puts the full image there
    #[allow(deprecated)]
    let mut picker = Picker::from_fontsize(ratatui_image::FontSize::new(cell.0, cell.1));
    picker.set_protocol_type(ProtocolType::Halfblocks);
    picker.set_background_color(Some(image::Rgba([BG[0], BG[1], BG[2], 255])));
    let mut app = App::new(e, picker, args);
    app.refresh_sets();
    // throughput and pool as the TUI samples them every 2 s
    for i in 0..120u64 {
        let ramp = (i as f64 / 10.0).min(1.0);
        let wave = ((i as f64) * 0.37).sin() * 4.0 + ((i as f64) * 0.11).cos() * 3.0;
        app.rate.push_back(((74.0 + wave) * ramp).round() as u64);
        // the pool fills up to near the backpressure limit, then the renderers set the pace
        let pool = (i as f64 * 45.0).min(1290.0) + ((i as f64) * 0.21).sin() * 60.0 + ((i * 13 % 7) as f64) * 6.0;
        app.pool.push_back(pool.round().max(0.0) as u64);
    }
    app.was_running = true;
    Scene { app, root }
}

// ---------------------------------------------------------------------------------------------------------------
// rasterizing the buffer

const CW: u32 = 10;
const BG: [u8; 3] = [22, 24, 28];
const FG: [u8; 3] = [212, 216, 222];

fn rgb(c: Color, fg: bool) -> [u8; 3] {
    match c {
        Color::Reset => if fg { FG } else { BG },
        Color::Black => [12, 13, 16],
        Color::Red => [232, 92, 92],
        Color::Green => [98, 194, 112],
        Color::Yellow => [226, 184, 76],
        Color::Blue => [92, 142, 230],
        Color::Magenta => [198, 120, 221],
        Color::Cyan => [72, 200, 214],
        Color::Gray => [176, 182, 192],
        Color::DarkGray => [112, 120, 132],
        Color::LightRed => [255, 128, 128],
        Color::LightGreen => [140, 220, 150],
        Color::LightYellow => [240, 210, 120],
        Color::LightBlue => [130, 170, 250],
        Color::LightMagenta => [220, 160, 240],
        Color::LightCyan => [130, 225, 235],
        Color::White => [240, 242, 245],
        Color::Rgb(r, g, b) => [r, g, b],
        Color::Indexed(i) => [i, i, i],
    }
}

struct Fonts {
    regular: FontVec,
    bold: Option<FontVec>,
    fallback: Vec<FontVec>,
    scale: PxScale,
    ch: u32,
    baseline: f32,
}

impl Fonts {
    fn load() -> Fonts {
        let paths: Vec<PathBuf> = std::env::split_paths(&std::env::var_os("PACKSHOT_SHOT_FONT").expect("PACKSHOT_SHOT_FONT: path(s) of a monospace font")).collect();
        let read = |p: &Path| FontVec::try_from_vec(std::fs::read(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        let regular = read(&paths[0]);
        let mut bold = read(&paths[0]);
        let bold = if bold.variations().iter().any(|a| &a.tag == b"wght") {
            bold.set_variation(b"wght", 700.0);
            Some(bold)
        } else {
            None
        };
        let fallback = paths[1..].iter().map(|p| read(p)).collect();
        // the scale that makes a cell CW wide; the line a little taller than the font
        let adv = regular.as_scaled(PxScale::from(100.0)).h_advance(regular.glyph_id('0')) / 100.0;
        let px = CW as f32 / adv;
        let scale = PxScale::from(px);
        let sf = regular.as_scaled(scale);
        let height = sf.ascent() - sf.descent();
        let ch = (height * 1.12).ceil() as u32;
        let baseline = ((ch as f32 - height) / 2.0 + sf.ascent()).round();
        Fonts { regular, bold, fallback, scale, ch, baseline }
    }
}

struct Canvas {
    w: u32,
    h: u32,
    px: Vec<[u8; 3]>,
}

impl Canvas {
    fn fill(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, c: [u8; 3]) {
        for y in y0.max(0)..y1.min(self.h as i32) {
            for x in x0.max(0)..x1.min(self.w as i32) {
                self.px[(y as u32 * self.w + x as u32) as usize] = c;
            }
        }
    }
    fn blend(&mut self, x: i32, y: i32, c: [u8; 3], a: f32) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 || a <= 0.0 {
            return;
        }
        let p = &mut self.px[(y as u32 * self.w + x as u32) as usize];
        let a = a.min(1.0);
        for k in 0..3 {
            p[k] = (p[k] as f32 * (1.0 - a) + c[k] as f32 * a).round() as u8;
        }
    }
}

/// Box drawing (light lines, rounded corners as square ones): which arms the character has (up, down, left, right)
fn box_arms(c: char) -> Option<(bool, bool, bool, bool)> {
    Some(match c {
        '─' => (false, false, true, true),
        '│' => (true, true, false, false),
        '┌' | '╭' => (false, true, false, true),
        '┐' | '╮' => (false, true, true, false),
        '└' | '╰' => (true, false, false, true),
        '┘' | '╯' => (true, false, true, false),
        '├' => (true, true, false, true),
        '┤' => (true, true, true, false),
        '┬' => (false, true, true, true),
        '┴' => (true, false, true, true),
        '┼' => (true, true, true, true),
        _ => return None,
    })
}

/// Block elements as rectangles in the cell: (x0, y0, x1, y1) as fractions
fn block_rect(c: char) -> Option<(f32, f32, f32, f32)> {
    let lower = |n: f32| (0.0, 1.0 - n / 8.0, 1.0, 1.0);
    let left = |n: f32| (0.0, 0.0, n / 8.0, 1.0);
    Some(match c {
        '█' => (0.0, 0.0, 1.0, 1.0),
        '▀' => (0.0, 0.0, 1.0, 0.5),
        '▐' => (0.5, 0.0, 1.0, 1.0),
        '▁' => lower(1.0),
        '▂' => lower(2.0),
        '▃' => lower(3.0),
        '▄' => lower(4.0),
        '▅' => lower(5.0),
        '▆' => lower(6.0),
        '▇' => lower(7.0),
        '▏' => left(1.0),
        '▎' => left(2.0),
        '▍' => left(3.0),
        '▌' => left(4.0),
        '▋' => left(5.0),
        '▊' => left(6.0),
        '▉' => left(7.0),
        _ => return None,
    })
}

fn rasterize(buf: &Buffer, fonts: &Fonts) -> Canvas {
    let ch = fonts.ch;
    let (w, h) = (buf.area.width as u32 * CW, buf.area.height as u32 * ch);
    let mut cv = Canvas { w, h, px: vec![BG; (w * h) as usize] };
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            let cell = &buf[(x, y)];
            let (mut fg, mut bg) = (rgb(cell.fg, true), rgb(cell.bg, false));
            if cell.modifier.contains(Modifier::REVERSED) {
                std::mem::swap(&mut fg, &mut bg);
            }
            let (x0, y0) = ((x as u32 * CW) as i32, (y as u32 * ch) as i32);
            cv.fill(x0, y0, x0 + CW as i32, y0 + ch as i32, bg);
            let sym = cell.symbol();
            let mut chars = sym.chars();
            let Some(c) = chars.next() else { continue };
            if c == ' ' {
                continue;
            }
            if let Some((u, d, l, r)) = box_arms(c) {
                let (cx, cy) = (x0 + CW as i32 / 2, y0 + ch as i32 / 2);
                if u {
                    cv.fill(cx, y0, cx + 1, cy + 1, fg);
                }
                if d {
                    cv.fill(cx, cy, cx + 1, y0 + ch as i32, fg);
                }
                if l {
                    cv.fill(x0, cy, cx + 1, cy + 1, fg);
                }
                if r {
                    cv.fill(cx, cy, x0 + CW as i32, cy + 1, fg);
                }
                continue;
            }
            if let Some((a, b, c2, d)) = block_rect(c) {
                let fx = |f: f32| x0 + (f * CW as f32).round() as i32;
                let fy = |f: f32| y0 + (f * ch as f32).round() as i32;
                cv.fill(fx(a), fy(b), fx(c2), fy(d), fg);
                continue;
            }
            let bold = cell.modifier.contains(Modifier::BOLD);
            let font: &FontVec = if fonts.regular.glyph_id(c).0 != 0 {
                if bold { fonts.bold.as_ref().unwrap_or(&fonts.regular) } else { &fonts.regular }
            } else {
                fonts.fallback.iter().find(|f| f.glyph_id(c).0 != 0).unwrap_or(&fonts.regular)
            };
            // a fallback font's glyph scaled to the cell width when it is wider
            let mut scale = fonts.scale;
            let adv = font.as_scaled(scale).h_advance(font.glyph_id(c));
            if adv > CW as f32 * 1.05 {
                scale = PxScale::from(scale.y * CW as f32 / adv);
            }
            let glyph = font.glyph_id(c).with_scale_and_position(scale, point(x0 as f32 + ((CW as f32 - adv.min(CW as f32)) / 2.0).max(0.0), y0 as f32 + fonts.baseline));
            let faux = bold && fonts.bold.is_none();
            if let Some(og) = font.outline_glyph(glyph) {
                let b = og.px_bounds();
                og.draw(|gx, gy, a| {
                    let (px, py) = (b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32);
                    cv.blend(px, py, fg, a);
                    if faux {
                        cv.blend(px + 1, py, fg, a);
                    }
                });
            }
        }
    }
    cv
}

/// PNG: indexed when 256 colours do, else RGB; best compression
fn write_png(cv: &Canvas, path: &Path) {
    let mut index: HashMap<[u8; 3], u8> = HashMap::new();
    let mut palette = Vec::new();
    let mut indexed = true;
    for p in &cv.px {
        if !index.contains_key(p) {
            if index.len() == 256 {
                indexed = false;
                break;
            }
            index.insert(*p, index.len() as u8);
            palette.extend_from_slice(p);
        }
    }
    let file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let mut enc = png::Encoder::new(file, cv.w, cv.h);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::High);
    let data: Vec<u8> = if indexed {
        enc.set_color(png::ColorType::Indexed);
        enc.set_palette(palette);
        cv.px.iter().map(|p| index[p]).collect()
    } else {
        enc.set_color(png::ColorType::Rgb);
        cv.px.iter().flatten().copied().collect()
    };
    let mut w = enc.write_header().unwrap();
    w.write_image_data(&data).unwrap();
}

/// The areas the half-block images take: connected cells with RGB colours both ways, as cell rectangles (x, y, w, h),
/// top to bottom, left to right
fn image_areas(buf: &Buffer) -> Vec<(u16, u16, u16, u16)> {
    let (w, h) = (buf.area.width, buf.area.height);
    let is_img = |x: u16, y: u16| {
        let c = &buf[(x, y)];
        matches!(c.fg, Color::Rgb(..)) && matches!(c.bg, Color::Rgb(..))
    };
    let mut seen = vec![false; w as usize * h as usize];
    let mut out = Vec::new();
    for y in 0..h {
        for x in 0..w {
            if seen[(y * w + x) as usize] || !is_img(x, y) {
                continue;
            }
            let (mut x0, mut y0, mut x1, mut y1) = (x, y, x, y);
            let mut stack = vec![(x, y)];
            seen[(y * w + x) as usize] = true;
            while let Some((cx, cy)) = stack.pop() {
                (x0, y0, x1, y1) = (x0.min(cx), y0.min(cy), x1.max(cx), y1.max(cy));
                let mut push = |nx: u16, ny: u16| {
                    if nx < w && ny < h && !seen[(ny * w + nx) as usize] && is_img(nx, ny) {
                        seen[(ny * w + nx) as usize] = true;
                        stack.push((nx, ny));
                    }
                };
                push(cx + 1, cy);
                push(cx, cy + 1);
                if cx > 0 {
                    push(cx - 1, cy);
                }
                if cy > 0 {
                    push(cx, cy - 1);
                }
            }
            out.push((x0, y0, x1 - x0 + 1, y1 - y0 + 1));
        }
    }
    out.sort_by_key(|a| (a.1, a.0));
    out
}

/// The full image where a Sixel terminal would draw it: fitted into the area, top left, never enlarged
fn paint_image(cv: &mut Canvas, ch: u32, area: (u16, u16, u16, u16), png: &Path) {
    let Ok(img) = image::open(png) else { return };
    let (aw, ah) = (area.2 as u32 * CW, area.3 as u32 * ch);
    let f = (aw as f32 / img.width() as f32).min(ah as f32 / img.height() as f32).min(1.0);
    let (w, h) = (((img.width() as f32 * f).round() as u32).max(1), ((img.height() as f32 * f).round() as u32).max(1));
    let img = image::imageops::resize(&img.to_rgba8(), w, h, image::imageops::FilterType::Lanczos3);
    let (x0, y0) = (area.0 as i32 * CW as i32, area.1 as i32 * ch as i32);
    cv.fill(x0, y0, x0 + aw as i32, y0 + ah as i32, BG);
    for (x, y, p) in img.enumerate_pixels() {
        cv.blend(x0 + x as i32, y0 + y as i32, [p[0], p[1], p[2]], p[3] as f32 / 255.0);
    }
}

fn shoot(app: &mut App, fonts: &Fonts, images: &[PathBuf], path: &Path) {
    let mut t = Terminal::new(TestBackend::new(COLS, ROWS)).unwrap();
    // twice: the image protocols size themselves on the first draw
    t.draw(|f| draw(f, app)).unwrap();
    t.draw(|f| draw(f, app)).unwrap();
    let buf = t.backend().buffer();
    let mut cv = rasterize(buf, fonts);
    let areas = image_areas(buf);
    assert_eq!(areas.len(), images.len(), "image areas on the screen against the images expected: {areas:?}");
    for (a, p) in areas.into_iter().zip(images) {
        paint_image(&mut cv, fonts.ch, a, p);
    }
    write_png(&cv, path);
    println!("{} ({}x{})", path.display(), cv.w, cv.h);
}

#[test]
#[ignore = "writes docs/assets/tui-*.png; needs PACKSHOT_SHOT_FONT"]
fn tui_screenshots() {
    let fonts = Fonts::load();
    let Scene { mut app, root } = scene((CW as u16, fonts.ch as u16));
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/assets");
    std::fs::create_dir_all(&out).unwrap();

    app.view = View::Run;
    let preview: Vec<PathBuf> = app.preview_target().and_then(|t| app.engine.image(t)).into_iter().collect();
    shoot(&mut app, &fonts, &preview, &out.join("tui-run.png"));

    app.view = View::Sets;
    // the earlier helmet run: its result and images on the right (the same 8 draw_run_detail takes)
    let id = app.engine.set.runs.iter().find(|id| id.contains("helmets")).cloned().unwrap();
    app.set_sel = app.set_rows.iter().position(|r| matches!(r, SetRow::Run { id: i, .. } if *i == id)).unwrap();
    let dir = app.engine.store.run_dir(&id);
    let thumbs: Vec<PathBuf> = app.engine.store.run(&id).unwrap().entities.values().filter_map(|e| e.files.first()).take(8).map(|f| dir.join(f)).collect();
    shoot(&mut app, &fonts, &thumbs, &out.join("tui-sets.png"));

    app.view = View::Nodes;
    // a peer: the details of this node would show its real IP address
    app.node_sel = 1;
    shoot(&mut app, &fonts, &[], &out.join("tui-nodes.png"));

    app.engine.abort();
    let _ = crate::store::remove_tree(&root);
}
