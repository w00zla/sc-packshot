// The TUI: three views behind a tab bar, the same on every node. 1 Run: header (selection with head node and set, or
// progress), the to-render list or the workers (of a run here, or as the head node of another node's run reports
// them) with throughput, the image preview (Sixel/Kitty/iTerm2, else half blocks; for another node's run the images
// rendered here). 2 Sets: the sets with their runs, the selected run's result and images on the right, details on i.
// 3 Nodes: this node and the others with capabilities, disks and details, the state check, fetching runs and the
// live manifest, the compute assessment (b, this node or another one). Log and errors at the bottom, keys in the last
// line.
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Gauge, Paragraph, Sparkline, Wrap};
use ratatui::{DefaultTerminal, Frame};
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::{Resize, StatefulImage};

use crate::Args;
use crate::engine::{Engine, Kind, Level, PHASES, Scope, Selection};
use crate::net::{Hello, RunStatus, WorkerStatus};
use crate::store::LIVE;

const ACCENT: Color = Color::Cyan;
const OK: Color = Color::Green;
const WARN: Color = Color::Yellow;
const BAD: Color = Color::Red;
const DIM: Color = Color::DarkGray;
const SEL_BG: Color = Color::Rgb(31, 58, 74);

#[derive(Clone, Copy, PartialEq)]
enum View {
    Run,
    Sets,
    Nodes,
}

/// A text field with a cursor (in the dialogs)
#[derive(Default)]
struct Field {
    text: String,
    /// position in characters
    cursor: usize,
}

impl Field {
    fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.chars().count();
        Self { text, cursor }
    }

    /// Editing keys (←/→, Home/End, Backspace/Delete, Ctrl+U, characters); false for any other key
    fn key(&mut self, code: KeyCode, mods: KeyModifiers) -> bool {
        let n = self.text.chars().count();
        let byte = |s: &str, i: usize| s.char_indices().nth(i).map(|(b, _)| b).unwrap_or(s.len());
        let ctrl = mods.contains(KeyModifiers::CONTROL);
        match code {
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(n),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = n,
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    let b = byte(&self.text, self.cursor - 1);
                    self.text.remove(b);
                    self.cursor -= 1;
                }
            }
            KeyCode::Delete => {
                if self.cursor < n {
                    let b = byte(&self.text, self.cursor);
                    self.text.remove(b);
                }
            }
            KeyCode::Char('u') if ctrl => {
                self.text.clear();
                self.cursor = 0;
            }
            KeyCode::Char(c) if !ctrl => {
                let b = byte(&self.text, self.cursor);
                self.text.insert(b, c);
                self.cursor += 1;
            }
            _ => return false,
        }
        true
    }

    /// The field in `width` cells: the cursor's cell inverted when active, scrolled so the cursor stays visible
    fn spans(&self, width: usize, active: bool) -> Vec<Span<'static>> {
        let chars: Vec<char> = self.text.chars().collect();
        let width = width.max(2);
        let start = (self.cursor + 1).saturating_sub(width);
        let shown: String = chars.iter().skip(start).take(width).collect();
        let field = Style::new().bg(Color::Rgb(40, 46, 54));
        if !active {
            return vec![Span::styled(format!("{shown:<width$}"), field)];
        }
        let at = self.cursor - start;
        let before: String = shown.chars().take(at).collect();
        let cur: String = shown.chars().nth(at).map(String::from).unwrap_or_else(|| " ".into());
        let after: String = shown.chars().skip(at + 1).collect();
        let pad = width.saturating_sub(before.chars().count() + 1 + after.chars().count());
        vec![
            Span::styled(before, field),
            Span::styled(cur, Style::new().fg(Color::Black).bg(ACCENT)),
            Span::styled(format!("{after}{:pad$}", ""), field),
        ]
    }
}

/// A file offered as a target list: the JSON files of the packshot folder that hold one
struct TargetsChoice {
    path: PathBuf,
    count: usize,
    at: String,
}

/// Input: the filter in the header line, everything else in a dialog
enum Input {
    Filter,
    RunName(Field),
    NewSet(Field),
    /// a set's target list: one of the files of the packshot folder, or another path (`sel` == choices.len())
    Targets { set: String, choices: Vec<TargetsChoice>, sel: usize, field: Field },
}

/// A question in a dialog: Enter (or y) confirms, any other key goes back
enum Confirm {
    Overwrite,
    Quit,
    Abort,
    DeleteRun { id: String, text: String },
    DeleteSet(String),
    Adopt(String),
    /// replace a set's target list: (set, file, question)
    ReplaceTargets { set: String, path: String, text: String },
    /// assess a node's compute: this one (None) or another one
    Assess { conn: Option<u64>, name: String },
    /// stop the assessment going on there
    AbortAssess { conn: Option<u64>, name: String },
}

/// A row of the Sets view
#[derive(Clone, PartialEq)]
enum SetRow {
    Set(String),
    Run { set: String, id: String },
}

struct App {
    engine: Engine,
    picker: Picker,
    view: View,
    filter: String,
    scope: Scope,
    input: Option<Input>,
    sel: Selection,
    selected: usize,
    confirm: Option<Confirm>,
    /// run name for the next start (from --name, else asked at the start)
    name: String,
    preview: Option<((PathBuf, Option<SystemTime>), StatefulProtocol)>,
    /// position in the render history; None = always the latest
    history_pos: Option<usize>,
    errors_tab: bool,
    set_rows: Vec<SetRow>,
    set_sel: usize,
    details: bool,
    /// thumbnails of the selected run in the Sets view
    thumbs: HashMap<PathBuf, StatefulProtocol>,
    node_sel: usize,
    rate: VecDeque<u64>,
    /// exported items not rendered yet (the pool), sampled like the throughput
    pool: VecDeque<u64>,
    last_sample: Instant,
    was_running: bool,
    /// the state of the remote head node seen last (selection recomputed when it changes)
    state_seq: u64,
    line: String,
    quit: bool,
}

pub fn run(engine: Engine, args: Args) -> Result<()> {
    let mut terminal = ratatui::init();
    // Ask for the graphics protocol and cell size (after entering the alternate screen); else half blocks.
    // PACKSHOT_GRAPHICS=sixel|kitty|iterm2|halfblocks forces one
    let mut picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    picker.set_background_color(Some(image::Rgba([22u8, 24, 28, 255])));
    let forced = std::env::var("PACKSHOT_GRAPHICS").ok().map(|v| v.trim().to_lowercase()).filter(|v| !v.is_empty());
    let mut graphics_note = None;
    // ratatui-image leaves out Sixel in Konsole (KONSOLE_VERSION) and falls back to half blocks; Konsole's Sixel
    // shows our previews fine (checked over SSH, where the variable does not come along)
    let konsole = forced.is_none() && std::env::var("KONSOLE_VERSION").is_ok_and(|v| !v.is_empty());
    if konsole {
        picker.set_protocol_type(ProtocolType::Sixel);
    }
    if let Some(v) = forced.as_deref() {
        match v {
            "sixel" => picker.set_protocol_type(ProtocolType::Sixel),
            "kitty" => picker.set_protocol_type(ProtocolType::Kitty),
            "iterm2" => picker.set_protocol_type(ProtocolType::Iterm2),
            "halfblocks" => picker.set_protocol_type(ProtocolType::Halfblocks),
            other => graphics_note = Some(format!("PACKSHOT_GRAPHICS={other} unknown: sixel, kitty, iterm2 or halfblocks")),
        }
    }
    // the chosen protocol is not logged (no use in the app's log); only a wrong PACKSHOT_GRAPHICS is
    let mut engine = engine;
    if let Some(n) = graphics_note {
        engine.note(Level::Error, n);
    }
    let mut app = App::new(engine, picker, args);
    app.refresh_sets();
    let result = main_loop(&mut terminal, &mut app);
    app.engine.abort();
    app.engine.stop_assess();
    if let Some(s) = app.engine.serve.as_mut() {
        s.stop();
    }
    if let Some(h) = app.engine.hub.as_mut() {
        h.close_all();
    }
    ratatui::restore();
    result
}

fn main_loop(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    while !app.quit {
        app.engine.tick();
        let running = app.engine.running();
        if app.was_running && !running {
            // run over: renders and failures changed
            app.refresh();
            app.refresh_sets();
        }
        app.was_running = running;
        if app.state_seq != app.engine.state_seq {
            app.state_seq = app.engine.state_seq;
            app.refresh();
            app.refresh_sets();
        }
        app.sample();
        terminal.draw(|f| draw(f, app))?;
        if event::poll(Duration::from_millis(100))? && let Event::Key(k) = event::read()? && k.kind == KeyEventKind::Press {
            app.key(k.code, k.modifiers)?;
        }
    }
    Ok(())
}

impl App {
    fn new(engine: Engine, picker: Picker, args: Args) -> Self {
        let sel = engine.select(&args.filter, args.scope);
        App {
            engine,
            picker,
            view: View::Run,
            filter: args.filter,
            scope: args.scope,
            input: None,
            sel,
            selected: 0,
            confirm: None,
            name: args.name,
            preview: None,
            history_pos: None,
            errors_tab: false,
            set_rows: Vec::new(),
            set_sel: 0,
            details: false,
            thumbs: HashMap::new(),
            node_sel: 0,
            rate: VecDeque::new(),
            pool: VecDeque::new(),
            last_sample: Instant::now() - Duration::from_secs(10),
            was_running: false,
            state_seq: 0,
            line: args.line,
            quit: false,
        }
    }

    /// The dialog for a set's target list, with the target lists in the packshot folder to choose from
    fn open_targets(&mut self, set: &str) {
        let dir = self.engine.targets_file.parent().map(PathBuf::from).filter(|d| !d.as_os_str().is_empty()).unwrap_or_else(|| self.engine.store.root.clone());
        let mut choices: Vec<TargetsChoice> = std::fs::read_dir(&dir)
            .map(|d| d.flatten().map(|e| e.path()).collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.extension().is_some_and(|x| x == "json") && p.file_name().is_some_and(|n| n != "packshot.json"))
            .filter_map(|p| {
                let list: Vec<crate::engine::Target> = serde_json::from_slice(&std::fs::read(&p).ok()?).ok()?;
                let at = std::fs::metadata(&p).ok()?.modified().ok().map(|t| chrono::DateTime::<chrono::Local>::from(t).format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default();
                (!list.is_empty()).then_some(TargetsChoice { path: p, count: list.len(), at })
            })
            .collect();
        // targets.json (what pnpm packshot:targets writes) first
        choices.sort_by_key(|c| (c.path.file_name().is_none_or(|n| n != "targets.json"), c.path.clone()));
        self.input = Some(Input::Targets { set: set.to_string(), choices, sel: 0, field: Field::default() });
    }

    /// Take the file as the set's target list; the selection follows when it is the target set here
    fn apply_targets(&mut self, set: &str, path: &str) {
        let res = self.engine.store.load_set(set).and_then(|mut s| self.engine.store.set_targets(&mut s, std::path::Path::new(path.trim())));
        match res {
            Ok(n) => {
                self.engine.note(Level::Ok, format!("Target list of {set}: {n} targets from {}", path.trim()));
                if self.engine.set.name == set && self.engine.head.is_none() {
                    let r = self.engine.switch_set(set);
                    self.report("Set not reloaded", r);
                }
            }
            Err(e) => self.engine.note(Level::Error, format!("Target list not taken: {e:#}")),
        }
        self.refresh();
        self.refresh_sets();
    }

    /// The run of another node shown here: its status while it runs
    fn remote_status(&self) -> Option<&RunStatus> {
        self.engine.remote_run.as_ref().map(|r| &r.status).filter(|s| !s.finished)
    }

    fn refresh(&mut self) {
        self.sel = self.engine.select(&self.filter, self.scope);
        self.selected = self.selected.min(self.sel.todo.len().saturating_sub(1));
    }

    /// The sets of another head node, when one is chosen (the Sets view shows them, read only)
    fn remote_sets(&self) -> Option<&crate::engine::RemoteSel> {
        if self.engine.running() { None } else { self.engine.remote_sel() }
    }

    /// Rows of the Sets view (this node's, or the chosen head node's): live first, then the experiments; each with
    /// its own runs, newest first
    fn refresh_sets(&mut self) {
        let mut sets: Vec<(String, Vec<String>)> = match self.remote_sets() {
            Some(r) => r.set_list.iter().map(|s| (s.name.clone(), s.runs.clone())).collect(),
            None => {
                let names = self.engine.store.sets().unwrap_or_default();
                names.into_iter().map(|n| { let runs = self.engine.store.load_set(&n).map(|s| s.runs).unwrap_or_default(); (n, runs) }).collect()
            }
        };
        sets.sort_by_key(|(n, _)| (n != LIVE, n.clone()));
        let mut rows = Vec::new();
        for (n, runs) in sets {
            rows.push(SetRow::Set(n.clone()));
            for id in runs.iter().rev() {
                rows.push(SetRow::Run { set: n.clone(), id: id.clone() });
            }
        }
        self.set_rows = rows;
        self.set_sel = self.set_sel.min(self.set_rows.len().saturating_sub(1));
        self.thumbs.clear();
    }

    fn sample(&mut self) {
        if self.last_sample.elapsed() < Duration::from_secs(2) {
            return;
        }
        self.last_sample = Instant::now();
        if self.engine.running() {
            push(&mut self.rate, self.engine.rate(60.0).round() as u64);
            push(&mut self.pool, self.engine.run.as_ref().map_or(0, |r| r.held.len()) as u64);
        } else if let Some(st) = self.remote_status() {
            let (r, held) = (st.rate.round() as u64, st.held as u64);
            push(&mut self.rate, r);
            push(&mut self.pool, held);
        }
    }

    /// Rows of the Nodes view: (connection, host name or address); this node first with 0, then the connected nodes
    /// by name, then the addresses (config peers, discovered nodes) that are not connected, with 0
    fn node_rows(&self) -> Vec<(u64, String)> {
        let e = &self.engine;
        let mut rows = vec![(0, e.me.name.clone())];
        if let Some(h) = e.hub.as_ref() {
            let mut peers: Vec<_> = h.peers.iter().map(|p| (p.conn, p.hello.name.clone())).collect();
            peers.sort_by(|a, b| a.1.cmp(&b.1));
            rows.extend(peers);
            let mut addrs: Vec<String> = h.cfg.peers.clone();
            for (addr, (_, seen)) in &h.discovered {
                if seen.elapsed() < Duration::from_secs(15) && !addrs.contains(addr) {
                    addrs.push(addr.clone());
                }
            }
            for addr in addrs {
                let name = h.addr_names.get(&addr).or_else(|| h.discovered.get(&addr).map(|(n, _)| n));
                let connected = name.is_some_and(|n| *n == e.me.name || h.peers.iter().any(|p| p.hello.name == *n)) || h.peers.iter().any(|p| p.outbound && p.addr == addr);
                if !connected {
                    rows.push((0, addr));
                }
            }
        }
        rows
    }

    fn selected_conn(&self) -> Option<u64> {
        let (conn, _) = self.node_rows().get(self.node_sel).cloned()?;
        self.engine.hub.as_ref()?.peer(conn).map(|p| p.conn)
    }

    /// Head node for the next start: this node, then the connected nodes by name, in turn (key n)
    fn next_head(&mut self) {
        let mut names: Vec<String> = self.engine.connected_nodes().into_iter().map(|(_, h)| h.name).collect();
        names.sort();
        let mut all = vec![None];
        all.extend(names.into_iter().map(Some));
        let i = all.iter().position(|n| *n == self.engine.head).unwrap_or(0);
        let next = all[(i + 1) % all.len()].clone();
        self.engine.set_head(next);
        self.selected = 0;
        self.refresh();
        self.refresh_sets();
    }

    /// Target set for the next start, in turn (key s): here the own sets, else the sets of the head node
    fn next_set(&mut self) {
        let (sets, current) = match self.engine.remote_sel() {
            Some(r) => (r.sets.clone(), r.set.clone()),
            None => (self.engine.store.sets().unwrap_or_default(), self.engine.set.name.clone()),
        };
        if sets.is_empty() {
            return;
        }
        let i = sets.iter().position(|n| *n == current).map(|i| (i + 1) % sets.len()).unwrap_or(0);
        let next = sets[i].clone();
        if self.engine.remote_sel().is_some() {
            self.engine.set_remote_set(&next);
        } else {
            let r = self.engine.switch_set(&next);
            self.report("Set not activated", r);
        }
        self.selected = 0;
        self.refresh();
    }

    /// Start: only when all connected nodes have the same state and no node has a run going; then name (asked unless
    /// --name), overwrite question, go
    fn start_flow(&mut self, step: u8) -> Result<()> {
        // no target list in the target set: choose one first (here; another head node's sets change only there)
        if step == 0 && self.engine.own_len == 0 {
            match self.engine.remote_sel() {
                Some(r) if r.loaded => {
                    let (set, node) = (r.set.clone(), r.node.clone());
                    self.engine.note(Level::Error, format!("Set {set} on {node} has no target list: choose it on {node} (t in view 2)"));
                }
                Some(_) => {}
                None => {
                    let set = self.engine.set.name.clone();
                    self.open_targets(&set);
                }
            }
            return Ok(());
        }
        if self.sel.todo.is_empty() {
            return Ok(());
        }
        if let Some(r) = self.engine.remote_sel().filter(|r| !r.loaded) {
            let node = r.node.clone();
            self.engine.note(Level::Error, format!("No state of {node} yet: the selection is not known"));
            return Ok(());
        }
        if let Some(other) = self.engine.running_elsewhere() {
            self.engine.note(Level::Error, format!("{other} has a run going: the nodes work on one run at a time"));
            return Ok(());
        }
        let problems = self.engine.state_problems();
        if !problems.is_empty() {
            for (name, diff) in problems {
                self.engine.note(Level::Error, format!("No run: {name} has another state: {}", diff.join(", ")));
            }
            self.engine.note(Level::Error, "Every node needs the head node's packshot, Blender, add-on and StarBreaker, the exporting nodes the same Data.p4k: update the bundles");
            return Ok(());
        }
        if let (None, Some(p)) = (self.engine.head.as_ref(), self.engine.capacity_problem()) {
            self.engine.note(Level::Error, format!("No run here: {p}"));
            return Ok(());
        }
        if step == 0 {
            let name = if self.name.is_empty() { self.engine.default_run_name(&self.filter, self.scope) } else { self.name.clone() };
            self.input = Some(Input::RunName(Field::new(name)));
            return Ok(());
        }
        if step <= 1 && self.sel.overwrite > 0 {
            self.confirm = Some(Confirm::Overwrite);
            return Ok(());
        }
        self.go()
    }

    /// Start the run: here with every connected node, or ask the chosen head node
    fn go(&mut self) -> Result<()> {
        let line = format!("{} --filter \"{}\" --scope {}", self.line, self.filter, self.scope.key());
        let name = self.name.clone();
        let todo = self.sel.todo.clone();
        let res = if self.engine.head.is_some() {
            self.engine.start_remote_run(&todo, line.trim(), &self.filter, self.scope, &name)
        } else {
            let me = self.engine.me.name.clone();
            self.engine.start_local(&todo, line.trim(), &self.filter, self.scope, &name, &me)
        };
        match res {
            Ok(()) => {
                self.history_pos = None;
                self.rate.clear();
                self.refresh_sets();
            }
            Err(e) => self.engine.note(Level::Error, format!("No run: {e:#}")),
        }
        Ok(())
    }

    fn report<T>(&mut self, what: &str, r: Result<T>) {
        if let Err(e) = r {
            self.engine.note(Level::Error, format!("{what}: {e:#}"));
        }
    }

    fn key(&mut self, code: KeyCode, mods: KeyModifiers) -> Result<()> {
        if let Some(c) = self.confirm.take() {
            let yes = matches!(code, KeyCode::Enter | KeyCode::Char('y'));
            match c {
                Confirm::Overwrite if yes => self.start_flow(2)?,
                Confirm::Quit if yes || code == KeyCode::Char('q') => self.quit = true,
                Confirm::Abort if yes => self.engine.abort_any(),
                Confirm::DeleteRun { id, .. } if yes => {
                    let r = self.engine.store.delete_run(&id).map(|_| ());
                    self.report("Run not deleted", r);
                    let r = self.engine.reload_set();
                    self.report("Set not reloaded", r);
                    self.refresh();
                    self.refresh_sets();
                }
                Confirm::DeleteSet(name) if yes => {
                    let r = self.engine.store.delete_set(&name).map(|_| ());
                    self.report("Set not deleted", r);
                    let r = self.engine.reload_set();
                    self.report("Set not reloaded", r);
                    self.refresh();
                    self.refresh_sets();
                }
                Confirm::ReplaceTargets { set, path, .. } if yes => self.apply_targets(&set, &path),
                Confirm::Assess { conn, name } if yes => {
                    let r = match conn {
                        None => self.engine.start_assess(None),
                        Some(c) => self.engine.request_assess(c),
                    };
                    self.report(&format!("No assessment of {name}"), r);
                }
                Confirm::AbortAssess { conn, name } if yes => {
                    let r = match conn {
                        None => {
                            self.engine.abort_assess(None);
                            Ok(())
                        }
                        Some(c) => self.engine.request_abort_assess(c),
                    };
                    self.report(&format!("Assessment of {name} not aborted"), r);
                }
                Confirm::Adopt(name) if yes => {
                    match self.engine.store.adopt(&name) {
                        Ok(n) => self.engine.note(Level::Ok, format!("{n} runs of {name} appended to live")),
                        Err(e) => self.engine.note(Level::Error, format!("Not adopted: {e:#}")),
                    }
                    let r = self.engine.reload_set();
                    self.report("Set not reloaded", r);
                    self.refresh();
                    self.refresh_sets();
                }
                _ => {}
            }
            return Ok(());
        }
        if let Some(mut input) = self.input.take() {
            match &mut input {
                // the filter in the header line: applied while typing
                Input::Filter => {
                    match code {
                        KeyCode::Esc | KeyCode::Enter => return Ok(()),
                        KeyCode::Backspace => {
                            self.filter.pop();
                        }
                        KeyCode::Char('u') if mods.contains(KeyModifiers::CONTROL) => self.filter.clear(),
                        KeyCode::Char(c) => self.filter.push(c),
                        _ => {}
                    }
                    self.input = Some(input);
                    self.selected = 0;
                    self.refresh();
                }
                Input::RunName(field) => match code {
                    KeyCode::Esc => {}
                    KeyCode::Enter => {
                        self.name = field.text.trim().to_string();
                        self.start_flow(1)?;
                        // --name only counts for the first run; later ones are asked again
                        self.name.clear();
                    }
                    _ => {
                        field.key(code, mods);
                        self.input = Some(input);
                    }
                },
                Input::NewSet(field) => match code {
                    KeyCode::Esc => {}
                    KeyCode::Enter => {
                        let n = field.text.trim().to_string();
                        match self.engine.store.create_set(&n, LIVE) {
                            Ok(_) => self.engine.note(Level::Ok, format!("Set {n} created from live")),
                            Err(e) => self.engine.note(Level::Error, format!("Set not created: {e:#}")),
                        }
                        self.refresh_sets();
                    }
                    _ => {
                        field.key(code, mods);
                        self.input = Some(input);
                    }
                },
                Input::Targets { set, choices, sel, field } => match code {
                    KeyCode::Esc => {}
                    KeyCode::Up => {
                        *sel = sel.saturating_sub(1);
                        self.input = Some(input);
                    }
                    KeyCode::Down => {
                        *sel = (*sel + 1).min(choices.len());
                        self.input = Some(input);
                    }
                    KeyCode::Enter => {
                        let path = match choices.get(*sel) {
                            Some(c) => c.path.display().to_string(),
                            None => field.text.trim().to_string(),
                        };
                        if path.is_empty() {
                            self.input = Some(input);
                            return Ok(());
                        }
                        let set = set.clone();
                        // a list there already: ask before replacing it
                        match self.engine.store.load_set(&set).ok().and_then(|s| s.targets) {
                            Some(t) => {
                                let text = format!("Replace the {} targets of {set} with {path}?", num(t.count));
                                self.confirm = Some(Confirm::ReplaceTargets { set, path, text });
                            }
                            None => self.apply_targets(&set, &path),
                        }
                    }
                    _ => {
                        // typing goes into the field for another file
                        if matches!(code, KeyCode::Char(_)) && !mods.contains(KeyModifiers::CONTROL) {
                            *sel = choices.len();
                        }
                        if *sel == choices.len() {
                            field.key(code, mods);
                        }
                        self.input = Some(input);
                    }
                },
            }
            return Ok(());
        }
        let running = self.engine.running();
        // keys of every view
        match code {
            KeyCode::Char('1') => self.view = View::Run,
            KeyCode::Char('2') => {
                self.view = View::Sets;
                self.refresh_sets();
            }
            KeyCode::Char('3') => self.view = View::Nodes,
            KeyCode::Tab => {
                self.view = match self.view {
                    View::Run => View::Sets,
                    View::Sets => View::Nodes,
                    View::Nodes => View::Run,
                };
                if self.view == View::Sets {
                    self.refresh_sets();
                }
            }
            KeyCode::Char('e') => self.errors_tab = !self.errors_tab,
            // abort the run going on, here or on its head node: every node may
            KeyCode::Char('a') if running || self.remote_status().is_some() => self.confirm = Some(Confirm::Abort),
            KeyCode::Char('q') | KeyCode::Esc if running => self.confirm = Some(Confirm::Quit),
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            _ => match self.view {
                View::Run => self.key_run(code, running)?,
                View::Sets => self.key_sets(code, running),
                View::Nodes => self.key_nodes(code, running),
            },
        }
        Ok(())
    }

    fn key_run(&mut self, code: KeyCode, running: bool) -> Result<()> {
        // a run of another node: this one only works for it
        let busy = running || self.remote_status().is_some();
        let e = &mut self.engine;
        // the counts change, the capabilities (--render, --export) stay
        let (render, export) = (e.me.render, e.me.export);
        match code {
            KeyCode::Char('+') if render => e.max_renderers = (e.max_renderers + 1).min(32),
            KeyCode::Char('-') if render => e.max_renderers = e.max_renderers.saturating_sub(1).max(1),
            KeyCode::Char(']' | '>') if export => e.max_exporters = (e.max_exporters + 1).min(8),
            KeyCode::Char('[' | '<') if export => e.max_exporters = e.max_exporters.saturating_sub(1).max(1),
            KeyCode::Left => {
                let n = e.history.len();
                if n > 0 {
                    self.history_pos = Some(self.history_pos.unwrap_or(n - 1).saturating_sub(1));
                }
            }
            KeyCode::Right => {
                if let Some(p) = self.history_pos {
                    self.history_pos = (p + 1 < e.history.len() - 1).then_some(p + 1);
                }
            }
            KeyCode::Char('p') if running => e.paused = !e.paused,
            _ if busy => {}
            KeyCode::Char('n') => self.next_head(),
            KeyCode::Char('s') => self.next_set(),
            KeyCode::Char('/') | KeyCode::Char('f') => self.input = Some(Input::Filter),
            KeyCode::Char('u') => {
                self.scope = self.scope.next();
                self.selected = 0;
                self.refresh();
            }
            KeyCode::Char('x') => {
                self.filter.clear();
                self.refresh();
            }
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected = (self.selected + 1).min(self.sel.todo.len().saturating_sub(1)),
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(20),
            KeyCode::PageDown => self.selected = (self.selected + 20).min(self.sel.todo.len().saturating_sub(1)),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = self.sel.todo.len().saturating_sub(1),
            KeyCode::Enter => self.start_flow(0)?,
            _ => {}
        }
        Ok(())
    }

    fn key_sets(&mut self, code: KeyCode, running: bool) {
        let row = self.set_rows.get(self.set_sel).cloned();
        // another head node's sets: look, and choose the target set there
        if let Some(head) = self.remote_sets().map(|r| r.node.clone()) {
            match code {
                KeyCode::Up => self.set_sel = self.set_sel.saturating_sub(1),
                KeyCode::Down => self.set_sel = (self.set_sel + 1).min(self.set_rows.len().saturating_sub(1)),
                KeyCode::PageUp => self.set_sel = self.set_sel.saturating_sub(20),
                KeyCode::PageDown => self.set_sel = (self.set_sel + 20).min(self.set_rows.len().saturating_sub(1)),
                KeyCode::Char('i') => self.details = !self.details,
                KeyCode::Enter => {
                    if let Some(SetRow::Set(n)) = row {
                        self.engine.set_remote_set(&n);
                        self.selected = 0;
                        self.refresh();
                    }
                }
                KeyCode::Char('n' | 't' | 'l' | 'x') => self.engine.note(Level::Info, format!("Sets of {head}: change them on {head} itself")),
                _ => {}
            }
            return;
        }
        match code {
            KeyCode::Up => self.set_sel = self.set_sel.saturating_sub(1),
            KeyCode::Down => self.set_sel = (self.set_sel + 1).min(self.set_rows.len().saturating_sub(1)),
            KeyCode::PageUp => self.set_sel = self.set_sel.saturating_sub(20),
            KeyCode::PageDown => self.set_sel = (self.set_sel + 20).min(self.set_rows.len().saturating_sub(1)),
            KeyCode::Char('i') => self.details = !self.details,
            _ if running => {}
            KeyCode::Enter => {
                if let Some(SetRow::Set(n)) = row {
                    let r = self.engine.switch_set(&n);
                    self.report("Set not activated", r);
                    self.refresh();
                }
            }
            KeyCode::Char('n') => self.input = Some(Input::NewSet(Field::default())),
            KeyCode::Char('t') => {
                let set = match row {
                    Some(SetRow::Set(n)) | Some(SetRow::Run { set: n, .. }) => n,
                    None => return,
                };
                self.open_targets(&set);
            }
            KeyCode::Char('l') => {
                let set = match row {
                    Some(SetRow::Set(n)) | Some(SetRow::Run { set: n, .. }) => n,
                    None => return,
                };
                if set != LIVE {
                    self.confirm = Some(Confirm::Adopt(set));
                }
            }
            KeyCode::Char('x') => match row {
                Some(SetRow::Run { set, id }) => {
                    let text = match self.engine.store.load_set(&set).and_then(|s| self.engine.store.removal_impact(&s, &id)) {
                        Ok((older, gone)) => format!("Delete run {id}? {set}: {older} images fall back to older runs, {gone} disappear."),
                        Err(e) => format!("Delete run {id}? ({e:#})"),
                    };
                    self.confirm = Some(Confirm::DeleteRun { id, text });
                }
                Some(SetRow::Set(n)) if n != LIVE => self.confirm = Some(Confirm::DeleteSet(n)),
                _ => {}
            },
            _ => {}
        }
    }

    fn key_nodes(&mut self, code: KeyCode, running: bool) {
        let n = self.node_rows().len();
        match code {
            KeyCode::Up => self.node_sel = self.node_sel.saturating_sub(1),
            KeyCode::Down => self.node_sel = (self.node_sel + 1).min(n.saturating_sub(1)),
            // this node: m drops the fetched manifest
            KeyCode::Char('m') if self.node_sel == 0 => {
                self.engine.drop_manifest();
                self.refresh();
            }
            // assess this node's compute (asks: it takes minutes), or abort the one going on
            KeyCode::Char('b') if self.node_sel == 0 => {
                let name = self.engine.me.name.clone();
                self.confirm = Some(if self.engine.assessing() { Confirm::AbortAssess { conn: None, name } } else { Confirm::Assess { conn: None, name } });
            }
            _ => {
                let Some(conn) = self.selected_conn() else { return };
                match code {
                    KeyCode::Char('h') if !running => {
                        if self.engine.fetch_runs(conn) == 0 {
                            self.engine.refresh_runs(conn);
                        }
                    }
                    KeyCode::Char('m') => {
                        self.engine.fetch_manifest(conn);
                    }
                    KeyCode::Char('r') => self.engine.refresh_runs(conn),
                    KeyCode::Char('b') => {
                        let name = self.node_rows().get(self.node_sel).map(|r| r.1.clone()).unwrap_or_default();
                        let assessing = self.engine.hub.as_ref().and_then(|h| h.peer(conn)).is_some_and(|p| p.hello.assessing);
                        self.confirm = Some(if assessing { Confirm::AbortAssess { conn: Some(conn), name } } else { Confirm::Assess { conn: Some(conn), name } });
                    }
                    _ => {}
                }
            }
        }
    }

    /// The item of the preview: while running the history, else the selection (with an image), else the latest
    fn preview_target(&self) -> Option<usize> {
        let h = &self.engine.history;
        if let Some(p) = self.history_pos {
            return h.get(p).copied();
        }
        // the selection's image: only when this node holds the target set
        if !self.engine.running() && self.engine.head.is_none() && let Some(&t) = self.sel.todo.get(self.selected) && self.engine.has_render(t) {
            return Some(t);
        }
        h.back().copied()
    }
}

fn push(q: &mut VecDeque<u64>, v: u64) {
    q.push_back(v);
    while q.len() > 120 {
        q.pop_front();
    }
}

fn fmt_dur(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

fn num(n: usize) -> String {
    // thousands with a comma
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1e6)
}

/// "2026-11-03_19-12_name" -> "11-03 19:12"
fn run_date(id: &str) -> String {
    let d = id.get(5..10).unwrap_or("");
    let t = id.get(11..16).unwrap_or("").replace('-', ":");
    format!("{d} {t}")
}

fn boxed(title: impl Into<String>) -> Block<'static> {
    Block::bordered().border_style(Style::new().fg(DIM)).title(Span::styled(format!(" {} ", title.into()), Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)))
}

/// Height of the bottom row in views 1 and 3 (throughput and pool; the node details): the same, so the views line up
const BOTTOM_ROW: u16 = 14;

fn draw(f: &mut Frame, app: &mut App) {
    let [tabs, rest, log, keys] = Layout::vertical([Constraint::Length(1), Constraint::Fill(2), Constraint::Fill(1), Constraint::Length(1)]).areas(f.area());
    draw_tabs(f, app, tabs);
    match app.view {
        View::Run => {
            let [head, middle] = Layout::vertical([Constraint::Length(5), Constraint::Min(9)]).areas(rest);
            let [left, right] = Layout::horizontal([Constraint::Min(50), Constraint::Length(48)]).areas(middle);
            if app.engine.running() {
                draw_head(f, app, head);
                draw_workers(f, app, left);
                draw_preview(f, app, right);
            } else if let Some(st) = app.remote_status().cloned() {
                // another node is the head node: its status, the images rendered here
                draw_remote_head(f, app, &st, head);
                draw_remote_workers(f, app, &st, left);
                draw_own_preview(f, app, right);
            } else {
                draw_head(f, app, head);
                draw_list(f, app, left);
                draw_preview(f, app, right);
            }
        }
        View::Sets => draw_sets(f, app, rest),
        View::Nodes => draw_nodes(f, app, rest),
    }
    draw_log(f, app, log);
    draw_keys(f, app, keys);
    draw_dialog(f, app);
}

/// A dialog over everything: the questions (confirm) and the inputs other than the filter
fn draw_dialog(f: &mut Frame, app: &App) {
    let area = f.area();
    let width = 84u16.min(area.width.saturating_sub(4)).max(20);
    let inner_w = width.saturating_sub(4) as usize;
    let e = &app.engine;
    let mut lines: Vec<Line> = Vec::new();
    let (title, keys, warn) = if let Some(c) = app.confirm.as_ref() {
        let (title, text, keys, warn) = match c {
            Confirm::Overwrite => ("Start a run", format!("{} images in set {} will be replaced.", num(app.sel.overwrite), e.target_set()), "Enter: start  ·  any other key: back", true),
            Confirm::Quit => ("Quit", "A run is going here: quitting aborts it. What is missing stays for the next run.".to_string(), "Enter or q: abort and quit  ·  any other key: back", true),
            Confirm::Abort => ("Abort the run", "Abort the run for all nodes? What is missing stays for the next run.".to_string(), "Enter: abort  ·  any other key: back", true),
            Confirm::DeleteRun { text, .. } => ("Delete run", text.clone(), "Enter: delete  ·  any other key: back", true),
            Confirm::DeleteSet(n) => ("Delete set", format!("Delete set {n} and its runs that no other set uses?"), "Enter: delete  ·  any other key: back", true),
            Confirm::Adopt(n) => ("Adopt into live", format!("Append the runs of {n} to live? Nothing is rendered."), "Enter: adopt  ·  any other key: back", false),
            Confirm::ReplaceTargets { text, .. } => ("Replace target list", text.clone(), "Enter: replace  ·  any other key: back", true),
            Confirm::Assess { name, .. } => ("Assess compute", format!("Assess {name}? It takes a few minutes; {name} takes no work meanwhile."), "Enter: assess  ·  any other key: back", false),
            Confirm::AbortAssess { name, .. } => ("Abort the assessment", format!("Abort the assessment going on {}? Nothing of it is saved.", if name == &e.me.name { "here".to_string() } else { format!("on {name}") }), "Enter: abort  ·  any other key: back", true),
        };
        lines.push(Line::from(text));
        (title.to_string(), keys, warn)
    } else {
        match app.input.as_ref() {
            Some(Input::RunName(field)) => {
                let head = e.head.clone().unwrap_or_else(|| e.me.name.clone());
                lines.push(Line::from(format!("{} items into set {} on {head}.", num(app.sel.todo.len()), e.target_set())).fg(DIM));
                lines.push(Line::from(""));
                let mut l = vec![Span::styled("Name  ", Style::new().fg(DIM))];
                l.extend(field.spans(inner_w.saturating_sub(6), true));
                lines.push(Line::from(l));
                ("Start a run".to_string(), "Enter: start  ·  ←/→ Home End: edit  ·  Esc: back", false)
            }
            Some(Input::NewSet(field)) => {
                lines.push(Line::from("Branches off live: its runs and its target list. Letters, digits, - and _.").fg(DIM));
                lines.push(Line::from(""));
                let mut l = vec![Span::styled("Name  ", Style::new().fg(DIM))];
                l.extend(field.spans(inner_w.saturating_sub(6), true));
                lines.push(Line::from(l));
                ("New set".to_string(), "Enter: create  ·  Esc: back", false)
            }
            Some(Input::Targets { set, choices, sel, field }) => {
                let current = e.store.load_set(set).ok().and_then(|s| s.targets);
                lines.push(
                    Line::from(match &current {
                        Some(t) => format!("{set} has {} targets from {}; a new list replaces them (asks first).", num(t.count), t.source),
                        None => format!("{set} has no target list yet: runs start once it has one."),
                    })
                    .fg(DIM),
                );
                lines.push(Line::from(""));
                for (i, c) in choices.iter().enumerate() {
                    let name = c.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    let info = format!("{} targets · {}", num(c.count), c.at);
                    let name_w = inner_w.saturating_sub(info.chars().count() + 3);
                    let mut l = Line::from(format!("{} {name:<name_w$.name_w$} {info}", if i == *sel { "▸" } else { " " }));
                    if i == *sel {
                        l = l.style(Style::new().bg(SEL_BG));
                    }
                    lines.push(l);
                }
                if choices.is_empty() {
                    lines.push(Line::from("  no target list in the packshot folder (pnpm packshot:targets writes targets.json)").fg(DIM));
                }
                let other = *sel == choices.len();
                let label = format!("{} Other file  ", if other { "▸" } else { " " });
                let mut l = vec![Span::styled(label.clone(), Style::new().fg(if other { Color::Reset } else { DIM }))];
                l.extend(field.spans(inner_w.saturating_sub(label.chars().count()), other));
                lines.push(Line::from(l));
                let keys = if current.is_none() { "Enter: take  ·  ↑↓: choose  ·  type: other file  ·  Esc: later" } else { "Enter: take  ·  ↑↓: choose  ·  type: other file  ·  Esc: back" };
                (format!("Target list for {set}"), keys, false)
            }
            _ => return,
        }
    };
    lines.push(Line::from(""));
    lines.push(Line::from(keys).fg(DIM));
    // the text wraps: count the rows it takes
    let rows: u16 = lines.iter().map(|l| (l.width().max(1).div_ceil(inner_w.max(1))) as u16).sum();
    let height = (rows + 2).min(area.height);
    let rect = Rect { x: area.x + (area.width - width) / 2, y: area.y + area.height.saturating_sub(height) / 3, width, height };
    f.render_widget(Clear, rect);
    let color = if warn { WARN } else { ACCENT };
    let block = Block::bordered()
        .border_style(Style::new().fg(color))
        .title(Span::styled(format!(" {title} "), Style::new().fg(color).add_modifier(Modifier::BOLD)))
        .padding(ratatui::widgets::Padding::horizontal(1))
        .style(Style::new().bg(Color::Rgb(22, 24, 28)));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_tabs(f: &mut Frame, app: &App, area: Rect) {
    let tab = |on: bool, s: &str| if on { Span::styled(format!(" {s} "), Style::new().fg(Color::Black).bg(ACCENT).bold()) } else { Span::styled(format!(" {s} "), Style::new().fg(DIM)) };
    let e = &app.engine;
    let nodes = e.connected_nodes();
    let net = match e.hub.as_ref() {
        None => "local".to_string(),
        Some(h) => match &h.listen_error {
            Some(_) => format!("{} ({}) · not listening · {} other nodes", e.me.name, e.me.caps(), nodes.len()),
            None => format!("{} ({}) · {} other nodes", e.me.name, e.me.caps(), nodes.len()),
        },
    };
    // a node with another state blocks every run: said up here in every view
    let problem = if e.state_problems().is_empty() { String::new() } else { " · not the same state: no run ".to_string() };
    let head = e.head.clone().unwrap_or_else(|| "here".into());
    let right = format!("head node {head} · set {} · {net} ", e.target_set());
    let left = vec![tab(app.view == View::Run, "1 Run"), " ".into(), tab(app.view == View::Sets, "2 Sets"), " ".into(), tab(app.view == View::Nodes, "3 Nodes")];
    let width = (right.chars().count() + problem.chars().count()) as u16;
    let [l, r] = Layout::horizontal([Constraint::Min(30), Constraint::Length(width)]).areas(area);
    f.render_widget(Paragraph::new(Line::from(left)), l);
    f.render_widget(Paragraph::new(Line::from(vec![Span::styled(right, Style::new().fg(DIM)), Span::styled(problem, Style::new().fg(Color::Black).bg(BAD).bold())])), r);
}

/// This node's workers with their current counts: "2 export · 6 render · 8 cores"
fn local_text(e: &Engine) -> String {
    if let Some(secs) = e.tables_loading() {
        return format!("{} render · export after the DataCore tables ({secs:.0} s) · {} cores", e.max_renderers, e.cores);
    }
    let mut me = e.me.clone();
    me.slots = e.max_renderers;
    me.exporters = e.exporters_now();
    let workers = if me.render || me.export { me.workers_text() } else { "starts, stores, watches".into() };
    format!("{workers} · {} cores", e.cores)
}

fn draw_head(f: &mut Frame, app: &App, area: Rect) {
    let e = &app.engine;
    let local = local_text(e);
    if let (true, Some(run)) = (e.running(), e.run.as_ref()) {
        let done = run.done();
        let pct = if run.total > 0 { done as f64 / run.total as f64 } else { 0.0 };
        let rate = e.rate(120.0);
        let eta = if rate > 0.1 { fmt_dur(Duration::from_secs_f64((run.total - done) as f64 / rate * 60.0)) } else { "–".into() };
        let mut l1 = vec![
            Span::styled(format!("{}/{}", num(done), num(run.total)), Style::new().fg(ACCENT).bold()),
            format!("  {:.1} %  ·  ok ", pct * 100.0).into(),
            Span::styled(num(run.ok), Style::new().fg(OK)),
            "  failed ".into(),
            Span::styled(num(run.failed), Style::new().fg(if run.failed > 0 { BAD } else { DIM })),
            format!("  ·  {} elapsed  ·  ETA ", fmt_dur(run.started.elapsed())).into(),
            Span::styled(eta, Style::new().fg(ACCENT)),
            format!("  ·  {rate:.0}/min  ·  {local}  ·  {} render in all", e.total_slots()).into(),
        ];
        if e.paused {
            l1.push(Span::styled("  PAUSED: starts nothing new", Style::new().fg(WARN).bold()));
        }
        let run_id = e.record.as_ref().map(|r| r.id.as_str()).unwrap_or("");
        let by = if run.starter == e.me.name { String::new() } else { format!("  ·  started by {}", run.starter) };
        let l3 = Line::from(format!("Run {run_id} → set {}  ·  Filter: {}  ·  Scope: {}{by}", e.set.name, if run.filter.is_empty() { "all" } else { &run.filter }, run.scope.label())).fg(DIM);
        let block = boxed(format!("Run · head node here · target set {}", e.set.name));
        let inner = block.inner(area);
        f.render_widget(block, area);
        let [a, b, c] = Layout::vertical([Constraint::Length(1); 3]).areas(inner);
        f.render_widget(Paragraph::new(Line::from(l1)), a);
        f.render_widget(Gauge::default().ratio(pct.clamp(0.0, 1.0)).label("").gauge_style(Style::new().fg(if run.failed > 0 { WARN } else { OK }).bg(Color::Black)), b);
        f.render_widget(Paragraph::new(l3), c);
        return;
    }

    let s = &app.sel;
    let editing = matches!(app.input, Some(Input::Filter));
    let cursor = if editing { "▏" } else { "" };
    let filter_style = if editing { Style::new().fg(Color::Black).bg(ACCENT) } else { Style::new().fg(ACCENT) };
    let head = e.head.clone().unwrap_or_else(|| "here".into());
    let l1 = Line::from(vec![
        "Head node ".fg(DIM),
        Span::styled(format!(" {head} "), Style::new().fg(ACCENT)),
        "   Set ".fg(DIM),
        Span::styled(format!(" {} ", e.target_set()), Style::new().fg(ACCENT)),
        "   Filter ".fg(DIM),
        Span::styled(format!(" {}{cursor} ", if app.filter.is_empty() && !editing { "all" } else { &app.filter }), filter_style),
        "   Scope ".fg(DIM),
        Span::styled(format!(" {} ", app.scope.label()), if app.scope == Scope::All { Style::new().fg(Color::Black).bg(WARN) } else { Style::new().fg(ACCENT) }),
        format!("   {local}").fg(DIM),
    ]);
    let mut l2 = vec![
        format!("{} matches: ", num(s.matched)).into(),
        if e.own_len == 0 { Span::styled(format!("set {} has no target list yet: Enter chooses one  ·  ", e.target_set()), Style::new().fg(WARN)) } else { "".into() },
        match e.remote_sel() {
            Some(r) if !r.loaded => Span::styled(format!("waiting for the state of {} …  ·  ", r.node), Style::new().fg(WARN)),
            _ => "".into(),
        },
        Span::styled(format!("{} in the set", num(s.existing)), Style::new().fg(OK)),
        format!(" · {} missing · ", num(s.missing)).into(),
        Span::styled(format!("{} failed last time", num(s.failed)), Style::new().fg(if s.failed > 0 { BAD } else { DIM })),
        "  →  ".fg(DIM),
        Span::styled(format!("{} to render", num(s.todo.len())), Style::new().fg(ACCENT).bold()),
    ];
    if s.overwrite > 0 {
        l2.push(Span::styled(format!(", {} of them replaced", num(s.overwrite)), Style::new().fg(WARN)));
    }
    if let (Some(m), true, None) = (e.manifest.as_ref(), e.set.name == LIVE, e.remote_sel()) {
        l2.push(format!("  ·  manifest of {} ({})", m.from, m.at.get(..16).unwrap_or(&m.at).replace('T', " ")).fg(DIM));
    }
    let last_remote = e.remote_run.as_ref().filter(|r| r.status.finished);
    let l3 = match (e.run.as_ref(), last_remote) {
        // the latest run shown: another node's if it ended later than the own one
        (_, Some(r)) if e.run.as_ref().is_none_or(|run| run.finished.is_some_and(|t| t < r.at)) => {
            let st = &r.status;
            Line::from(vec![
                Span::styled(format!("Last run on {}{}: ", st.head, if st.aborted { " aborted" } else { " done" }), Style::new().fg(DIM)),
                Span::styled(format!("{} ok", num(st.ok)), Style::new().fg(OK)),
                ", ".fg(DIM),
                Span::styled(format!("{} failed", num(st.failed)), Style::new().fg(if st.failed > 0 { BAD } else { DIM })),
                format!(" in {}", fmt_dur(Duration::from_secs_f64(st.elapsed))).fg(DIM),
            ])
        }
        (Some(run), _) => {
            let end = run.finished.map(|t| t.duration_since(run.started)).unwrap_or_default();
            Line::from(vec![
                Span::styled(if run.aborted { "Last run aborted: " } else { "Last run done: " }, Style::new().fg(DIM)),
                Span::styled(format!("{} ok", num(run.ok)), Style::new().fg(OK)),
                ", ".fg(DIM),
                Span::styled(format!("{} failed", num(run.failed)), Style::new().fg(if run.failed > 0 { BAD } else { DIM })),
                format!(" in {}", fmt_dur(end)).fg(DIM),
            ])
        }
        (None, _) => Line::from(""),
    };
    let block = boxed("Run · selection");
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(Paragraph::new(vec![l1, Line::from(l2), l3]), inner);
}

/// Header of another node's run: its progress as its head node reports it
fn draw_remote_head(f: &mut Frame, app: &App, st: &RunStatus, area: Rect) {
    let e = &app.engine;
    let done = st.ok + st.failed;
    let pct = if st.total > 0 { done as f64 / st.total as f64 } else { 0.0 };
    let eta = if st.rate > 0.1 { fmt_dur(Duration::from_secs_f64(st.total.saturating_sub(done) as f64 / st.rate * 60.0)) } else { "–".into() };
    let mut l1 = vec![
        Span::styled(format!("{}/{}", num(done), num(st.total)), Style::new().fg(ACCENT).bold()),
        format!("  {:.1} %  ·  ok ", pct * 100.0).into(),
        Span::styled(num(st.ok), Style::new().fg(OK)),
        "  failed ".into(),
        Span::styled(num(st.failed), Style::new().fg(if st.failed > 0 { BAD } else { DIM })),
        format!("  ·  {} elapsed  ·  ETA ", fmt_dur(Duration::from_secs_f64(st.elapsed))).into(),
        Span::styled(eta, Style::new().fg(ACCENT)),
        format!("  ·  {:.0}/min  ·  here {}  ·  {} render in all", st.rate, local_text(e), st.slots).into(),
    ];
    if st.paused {
        l1.push(Span::styled("  PAUSED on the head node", Style::new().fg(WARN).bold()));
    }
    let stale = e.remote_run.as_ref().is_some_and(|r| r.at.elapsed() > Duration::from_secs(5));
    let l3 = {
        let by = if st.starter.is_empty() { String::new() } else { format!("  ·  started by {}", st.starter) };
        let text = format!("Run {} → set {} on {}  ·  Filter: {}  ·  Scope: {}{by}", st.run, st.set, st.head, if st.filter.is_empty() { "all" } else { &st.filter }, st.scope);
        if stale { Line::from(Span::styled(format!("{text}  ·  no status for a while"), Style::new().fg(WARN))) } else { Line::from(text).fg(DIM) }
    };
    let block = boxed(format!("Run · head node {} · set {}", st.head, st.set));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [a, b, c] = Layout::vertical([Constraint::Length(1); 3]).areas(inner);
    f.render_widget(Paragraph::new(Line::from(l1)), a);
    f.render_widget(Gauge::default().ratio(pct.clamp(0.0, 1.0)).label("").gauge_style(Style::new().fg(if st.failed > 0 { WARN } else { OK }).bg(Color::Black)), b);
    f.render_widget(Paragraph::new(l3), c);
}

fn draw_list(f: &mut Frame, app: &mut App, area: Rect) {
    let block = boxed(format!("To render · {}", num(app.sel.todo.len())));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let h = inner.height as usize;
    if app.engine.own_len == 0 {
        let hint = format!("Set {} has no target list yet: Enter chooses one.", app.engine.target_set());
        f.render_widget(Paragraph::new(hint).fg(DIM), inner);
        return;
    }
    if app.sel.todo.is_empty() {
        let hint = match app.scope {
            Scope::Missing => "Nothing missing in the filter.",
            Scope::Failed => "Nothing failed in the filter.",
            Scope::All => "No matches.",
        };
        f.render_widget(Paragraph::new(hint).fg(DIM), inner);
        return;
    }
    let top = app.selected.saturating_sub(h.saturating_sub(1).min(h / 2)).min(app.sel.todo.len().saturating_sub(h));
    let e = &app.engine;
    let w = inner.width as usize;
    let lines: Vec<Line> = app.sel.todo[top..(top + h).min(app.sel.todo.len())]
        .iter()
        .enumerate()
        .map(|(i, &t)| {
            let tg = &e.targets[t];
            let (mark, color) = if e.failed_last(t) { ("✗", BAD) } else if e.present(t) { ("↻", WARN) } else { ("·", DIM) };
            let kind = format!("{}/{}", tg.kind, tg.sub_type);
            let ent_w = w.saturating_sub(4 + 34).clamp(20, 60);
            let mut line = Line::from(vec![
                Span::styled(format!(" {mark} "), Style::new().fg(color)),
                Span::raw(format!("{:<ent_w$.ent_w$} ", tg.entity)),
                Span::styled(format!("{:<28.28} ", kind), Style::new().fg(DIM)),
                Span::styled(tg.name().to_string(), Style::new().fg(DIM)),
            ]);
            if top + i == app.selected {
                line = line.style(Style::new().bg(SEL_BG));
            }
            line
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

/// Workers of a run here: the local processes and the work on other nodes, as rows
fn draw_workers(f: &mut Frame, app: &App, area: Rect) {
    let e = &app.engine;
    let gone: Vec<String> = e
        .run
        .as_ref()
        .map(|r| r.nodes.iter().filter(|n| !e.connected_nodes().iter().any(|(_, h)| &h.name == *n)).cloned().collect())
        .unwrap_or_default();
    let mut ws: Vec<_> = e.workers.iter().collect();
    ws.sort_by_key(|w| (w.node.clone(), w.kind == Kind::Render, w.slot));
    let rows: Vec<WorkerStatus> = ws
        .into_iter()
        .map(|w| {
            let (phase, current, done, total) = w.progress();
            WorkerStatus { label: w.label(), phase: phase.into(), entity: current.map(|t| e.targets[t].entity.clone()).unwrap_or_default(), done, total, secs: w.since.elapsed().as_secs_f64() }
        })
        .collect();
    let mut summary = Vec::new();
    if let Some(run) = e.run.as_ref() {
        let (queued, ready) = run.waiting();
        let idle = e.workers.iter().filter(|w| w.kind == Kind::Render).count() < e.total_slots() && ready == 0 && queued > 0;
        summary.push(Line::from(vec![
            queue_text(queued, run.held.len(), ready, run.exported).fg(DIM),
            if e.transferred > 0 { format!(" · {} moved between nodes", mb(e.transferred)).fg(DIM) } else { "".into() },
            if idle { Span::styled("  Blender waits for export: > for more exports", Style::new().fg(WARN)) } else { "".into() },
        ]));
        summary.push(phase_line(run.phase_avg()));
    }
    let rate = format!("Renders/min (60 s): {:.0}", e.rate(60.0));
    workers_panel(f, app, area, &rows, &gone, summary, rate);
}

/// Workers of another node's run, as its head node reports them
fn draw_remote_workers(f: &mut Frame, app: &App, st: &RunStatus, area: Rect) {
    let summary = vec![
        Line::from(queue_text(st.pending, st.held, st.ready, st.exported)).fg(DIM),
        phase_line(st.phase_avg),
    ];
    let rate = format!("Renders/min (2 min, {}): {:.0}", st.head, st.rate);
    workers_panel(f, app, area, &st.workers, &[], summary, rate);
}

/// The run's queue in one row, fixed widths: waiting for export, the pool (exported, not rendered yet) with those of
/// it waiting for a Blender, exported in all
fn queue_text(queued: usize, pool: usize, ready: usize, exported: usize) -> String {
    format!("{:>6} waiting for export · pool {:>6} ({:>6} waiting for Blender) · {:>6} exported", num(queued), num(pool), num(ready), num(exported))
}

/// Mean per phase from render.py; the largest highlighted (that is where the CPU time goes)
fn phase_line(avg: Option<[f64; 5]>) -> Line<'static> {
    let mut spans = vec!["Avg per render: ".fg(DIM)];
    match avg {
        Some(avg) => {
            let max = avg.iter().copied().fold(0.0, f64::max);
            for (k, v) in PHASES.iter().zip(avg) {
                let style = if v == max && v > 0.0 { Style::new().fg(WARN).bold() } else { Style::new() };
                spans.push(Span::styled(format!("{k} {v:.2} s"), style));
                spans.push("  ".into());
            }
        }
        None => spans.push("no timings yet".fg(DIM)),
    }
    Line::from(spans)
}

/// The workers table with its summary, above throughput and pool
fn workers_panel(f: &mut Frame, app: &App, area: Rect, rows: &[WorkerStatus], gone: &[String], summary: Vec<Line<'static>>, rate_title: String) {
    // fixed heights, so nothing jumps while workers come and go: throughput and pool as high as the details row of
    // view 3 (both at the bottom), the workers take the rest
    let [top, bottom] = Layout::vertical([Constraint::Fill(1), Constraint::Length(BOTTOM_ROW)]).areas(area);
    let block = boxed("Workers");
    let inner = block.inner(top);
    f.render_widget(block, top);
    let [head_area, rows_area, sum_area] = Layout::vertical([Constraint::Length(1), Constraint::Min(1), Constraint::Length(2)]).areas(inner);
    f.render_widget(Paragraph::new(Line::from(format!("{:<20}{:<8}{:<40}{:>8}{:>9}", "Node/id", "Phase", "Entity", "Block", "Time")).fg(DIM)), head_area);
    let mut lines = Vec::new();
    for w in rows {
        let color = match w.phase.as_str() {
            "render" => ACCENT,
            "fetch" => DIM,
            _ => WARN,
        };
        let current = if w.entity.is_empty() { "…" } else { w.entity.as_str() };
        lines.push(Line::from(vec![
            Span::styled(format!("{:<20.19}", w.label), Style::new().fg(color).bold()),
            Span::raw(format!("{:<8}", w.phase)),
            Span::raw(format!("{current:<40.40}")),
            Span::styled(format!("{:>8}", format!("{}/{}", w.done, w.total)), Style::new().fg(DIM)),
            Span::raw(format!("{:>8.1}s", w.secs)),
        ]));
    }
    for n in gone {
        lines.push(Line::from(vec![Span::styled(format!("{n:<20.19}"), Style::new().fg(DIM)), Span::styled("—       not connected", Style::new().fg(DIM))]));
    }
    let h = rows_area.height as usize;
    if lines.len() > h {
        let more = lines.len() - h + 1;
        lines.truncate(h.saturating_sub(1));
        lines.push(Line::from(format!("… {more} more")).fg(DIM));
    }
    f.render_widget(Paragraph::new(lines), rows_area);
    f.render_widget(Paragraph::new(summary), sum_area);

    // the run's throughput, and its pool (exported, not rendered yet): near 0 the exports hold the run back, at the
    // limit (the backpressure, known on the head node) the renderers do
    let block = boxed("Throughput and pool");
    let inner = block.inner(bottom);
    f.render_widget(block, bottom);
    let [a, b] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).spacing(2).areas(inner);
    let spark = |f: &mut Frame, r: Rect, title: String, data: &VecDeque<u64>, max: Option<u64>, color: Color| {
        let [t, s] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(r);
        f.render_widget(Paragraph::new(title).fg(DIM), t);
        let v: Vec<u64> = data.iter().copied().skip(data.len().saturating_sub(s.width as usize)).collect();
        let mut sp = Sparkline::default().data(&v).style(Style::new().fg(color));
        if let Some(m) = max {
            sp = sp.max(m.max(1));
        }
        f.render_widget(sp, s);
    };
    spark(f, a, rate_title, &app.rate, None, ACCENT);
    let limit = app.engine.running().then(|| app.engine.pool_limit() as u64);
    let now = app.pool.back().copied().unwrap_or(0);
    let title = match limit {
        Some(l) => format!("Waiting for Blender: {now} / {l}"),
        None => format!("Waiting for Blender: {now}"),
    };
    spark(f, b, title, &app.pool, limit, WARN);
}

/// Preview of a node that is not the head node: the latest image rendered here
fn draw_own_preview(f: &mut Frame, app: &mut App, area: Rect) {
    let block = boxed("Preview · rendered here");
    let inner = block.inner(area);
    f.render_widget(block, area);
    if !app.engine.me.render {
        f.render_widget(Paragraph::new("this node does not render (no --render)").fg(DIM), inner);
        return;
    }
    let Some((png, entity)) = app.engine.serve.as_ref().and_then(|s| s.last.clone()) else {
        f.render_widget(Paragraph::new("nothing rendered here yet").fg(DIM), inner);
        return;
    };
    let [img_area, text] = Layout::vertical([Constraint::Min(4), Constraint::Length(1)]).areas(inner);
    let key = (png.clone(), std::fs::metadata(&png).and_then(|m| m.modified()).ok());
    if app.preview.as_ref().map(|(k, _)| k) != Some(&key) {
        app.preview = image::open(&png).ok().map(|img| (key, app.picker.new_resize_protocol(img)));
    }
    if let Some((_, proto)) = app.preview.as_mut() {
        f.render_stateful_widget(StatefulImage::default().resize(Resize::Fit(None)), img_area, proto);
    }
    f.render_widget(Paragraph::new(Line::from(Span::styled(entity, Style::new().fg(ACCENT).bold()))), text);
}

fn draw_preview(f: &mut Frame, app: &mut App, area: Rect) {
    let title = match app.history_pos {
        Some(p) => format!("Preview · {}/{}", p + 1, app.engine.history.len()),
        None if app.engine.running() => "Preview · latest render".into(),
        None => "Preview".into(),
    };
    let block = boxed(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [img_area, text] = Layout::vertical([Constraint::Min(4), Constraint::Length(3)]).areas(inner);
    let Some(t) = app.preview_target() else {
        f.render_widget(Paragraph::new("no render yet").fg(DIM), inner);
        return;
    };
    let Some(png) = app.engine.image(t) else {
        f.render_widget(Paragraph::new("no render yet").fg(DIM), inner);
        return;
    };
    let key = (png.clone(), std::fs::metadata(&png).and_then(|m| m.modified()).ok());
    if app.preview.as_ref().map(|(k, _)| k) != Some(&key) {
        app.preview = image::open(&png).ok().map(|img| (key, app.picker.new_resize_protocol(img)));
    }
    if let Some((_, proto)) = app.preview.as_mut() {
        f.render_stateful_widget(StatefulImage::default().resize(Resize::Fit(None)), img_area, proto);
    } else {
        f.render_widget(Paragraph::new("image not readable").fg(BAD), img_area);
    }
    let tg = &app.engine.targets[t];
    let rig = app.engine.pose_rig(t).ok().map(|(p, r)| {
        let rig = r.file_stem().map(|s| s.to_string_lossy().replace("inventorylightgroup_", "")).unwrap_or_default();
        format!("pose {},{},{} · rig {rig}", p[0], p[1], p[2])
    });
    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(tg.name().to_string(), Style::new().fg(ACCENT).bold())),
            Line::from(format!("{} · {}/{}", tg.entity, tg.kind, tg.sub_type)).fg(DIM),
            Line::from(rig.unwrap_or_default()).fg(DIM),
        ]),
        text,
    );
}

fn draw_sets(f: &mut Frame, app: &mut App, area: Rect) {
    if app.remote_sets().is_some() {
        draw_remote_sets(f, app, area);
        return;
    }
    let [left, right] = Layout::horizontal([Constraint::Min(60), Constraint::Length(52)]).areas(area);
    let block = boxed("Sets and runs");
    let inner = block.inner(left);
    f.render_widget(block, left);
    let [list_area, foot] = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(inner);
    let h = list_area.height as usize;
    let top = app.set_sel.saturating_sub(h.saturating_sub(1).min(h / 2)).min(app.set_rows.len().saturating_sub(h));
    let me = app.engine.me.name.clone();
    let active = app.engine.set.name.clone();
    let mut lines = vec![Line::from(format!("{:<44}{:<14}{:<18}{:>8}  {:<10}", "Set / run", "Node", "Targets / filter", "Images", "Add-on")).fg(DIM)];
    let rows: Vec<SetRow> = app.set_rows.iter().skip(top).take(h.saturating_sub(1)).cloned().collect();
    for (i, row) in rows.iter().enumerate() {
        let mut line = match row {
            SetRow::Set(n) => {
                let s = app.engine.store.load_set(n).ok();
                let (runs, files) = s.as_ref().map(|s| (s.chain().count(), s.files.len())).unwrap_or_default();
                let base = s.as_ref().and_then(|s| s.base.clone()).map(|b| format!(" from {b}")).unwrap_or_default();
                let mut spans = vec![Span::styled(format!("▾ {n}"), Style::new().fg(ACCENT).bold()), base.fg(DIM)];
                if *n == active {
                    spans.push(Span::styled(" target ", Style::new().fg(OK)));
                }
                let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
                let targets = s.as_ref().and_then(|s| s.targets.as_ref()).map(|t| format!("{} targets", num(t.count))).unwrap_or_else(|| "no targets".into());
                spans.push(format!("{:w$}{:<14}{:<18}{:>8}", "", format!("{runs} runs"), targets, num(files), w = 44usize.saturating_sub(used)).fg(DIM));
                Line::from(spans)
            }
            SetRow::Run { id, .. } => {
                let r = app.engine.store.run(id).ok().cloned();
                let name = r.as_ref().map(|r| r.name.clone()).unwrap_or_default();
                let node = r.as_ref().map(|r| r.node.clone()).unwrap_or_default();
                let foreign = !node.is_empty() && node != me;
                let filter = r.as_ref().map(|r| r.origin["filter"].as_str().unwrap_or("").to_string()).unwrap_or_default();
                let images = r.as_ref().map(|r| r.entities.values().filter(|e| e.ok).count()).unwrap_or(0);
                let addon = r.as_ref().and_then(|r| r.origin["addon"].as_str().map(|s| s.chars().take(8).collect::<String>())).unwrap_or_default();
                let running = r.as_ref().is_some_and(|r| r.finished.is_none());
                let label = format!("    {}  {}", run_date(id), name);
                Line::from(vec![
                    Span::raw(format!("{label:<44.44}")),
                    Span::raw(format!("{:<14.14}", format!("{node}{}", if foreign { " ⇣" } else { "" }))),
                    Span::styled(format!("{:<18.18}", if filter.is_empty() { "all".into() } else { filter }), Style::new().fg(DIM)),
                    Span::raw(format!("{:>8}", num(images))),
                    Span::styled(format!("  {addon:<10}"), Style::new().fg(DIM)),
                    if running { Span::styled(" running", Style::new().fg(WARN)) } else { "".into() },
                ])
            }
        };
        if top + i == app.set_sel {
            line = line.style(Style::new().bg(SEL_BG));
        }
        lines.push(line);
    }
    f.render_widget(Paragraph::new(lines), list_area);
    let foot_line = Line::from("⇣ = run from another node (its head node), fetched with h in view 3").fg(DIM);
    f.render_widget(Paragraph::new(foot_line), foot);

    // right: the selected run (or set)
    let row = app.set_rows.get(app.set_sel).cloned();
    match row {
        Some(SetRow::Run { id, .. }) => draw_run_detail(f, app, right, &id),
        Some(SetRow::Set(n)) => {
            let block = boxed(format!("Set {n}"));
            let inner = block.inner(right);
            f.render_widget(block, right);
            let s = app.engine.store.load_set(&n).ok();
            let mut lines = Vec::new();
            if let Some(s) = s {
                lines.push(Line::from(format!("Base       {}", s.base.clone().unwrap_or_else(|| "–".into()))));
                lines.push(Line::from(format!("Runs       {} own, {} from the base", s.runs.len(), s.base_runs.len())));
                lines.push(Line::from(format!("Images     {}", num(s.files.len()))));
                lines.push(Line::from(match &s.targets {
                    Some(t) => format!("Targets    {} · {} · {}", num(t.count), t.source, t.at.get(..16).unwrap_or(&t.at).replace('T', " ")),
                    None => "Targets    none (t takes a target list)".into(),
                }));
                lines.push(Line::from(format!("Folder     {}", app.engine.store.set_dir(&n).display())).fg(DIM));
            }
            f.render_widget(Paragraph::new(lines), inner);
        }
        None => {}
    }
}

/// The Sets view for another head node: its sets and runs from its state and run list, read only, no images
fn draw_remote_sets(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(r) = app.remote_sets() else { return };
    let head = r.node.clone();
    let target = r.set.clone();
    let list = r.set_list.clone();
    let runs = app.engine.peer_runs.get(&head).cloned().unwrap_or_default();
    let [left, right] = Layout::horizontal([Constraint::Min(60), Constraint::Length(52)]).areas(area);
    let block = boxed(format!("Sets and runs on {head} · read only"));
    let inner = block.inner(left);
    f.render_widget(block, left);
    let [list_area, foot] = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(inner);
    let h = list_area.height as usize;
    let top = app.set_sel.saturating_sub(h.saturating_sub(1).min(h / 2)).min(app.set_rows.len().saturating_sub(h));
    let mut lines = vec![Line::from(format!("{:<44}{:<14}{:<18}{:>8}", "Set / run", "Node", "Targets", "Images")).fg(DIM)];
    for (i, row) in app.set_rows.iter().skip(top).take(h.saturating_sub(1)).enumerate() {
        let mut line = match row {
            SetRow::Set(n) => {
                let s = list.iter().find(|s| s.name == *n);
                let base = s.and_then(|s| s.base.clone()).map(|b| format!(" from {b}")).unwrap_or_default();
                let mut spans = vec![Span::styled(format!("▾ {n}"), Style::new().fg(ACCENT).bold()), base.fg(DIM)];
                if *n == target {
                    spans.push(Span::styled(" target ", Style::new().fg(OK)));
                }
                let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
                let chain = s.map(|s| s.base_runs + s.runs.len()).unwrap_or(0);
                let targets = s.and_then(|s| s.targets.as_ref()).map(|t| format!("{} targets", num(t.count))).unwrap_or_else(|| "no targets".into());
                spans.push(format!("{:w$}{:<14}{:<18}{:>8}", "", format!("{chain} runs"), targets, num(s.map(|s| s.files).unwrap_or(0)), w = 44usize.saturating_sub(used)).fg(DIM));
                Line::from(spans)
            }
            SetRow::Run { id, .. } => {
                let r = runs.iter().find(|r| r.id == *id);
                let label = format!("    {}  {}", run_date(id), r.map(|r| r.name.as_str()).unwrap_or(""));
                Line::from(vec![
                    Span::raw(format!("{label:<44.44}")),
                    Span::raw(format!("{:<14.14}", r.map(|r| r.node.as_str()).unwrap_or("?"))),
                    Span::raw(format!("{:<18}", "")),
                    Span::raw(format!("{:>8}", r.map(|r| num(r.images)).unwrap_or_else(|| "?".into()))),
                    if r.is_some_and(|r| !r.finished) { Span::styled(" running", Style::new().fg(WARN)) } else { "".into() },
                ])
            }
        };
        if top + i == app.set_sel {
            line = line.style(Style::new().bg(SEL_BG));
        }
        lines.push(line);
    }
    f.render_widget(Paragraph::new(lines), list_area);
    f.render_widget(Paragraph::new(Line::from(format!("Enter: target set on {head} · n, t, l, x only on {head} itself")).fg(DIM)), foot);

    let row = app.set_rows.get(app.set_sel).cloned();
    let kv = |k: &str, v: String| Line::from(vec![Span::styled(format!("{k:<11}"), Style::new().fg(DIM)), Span::raw(v)]);
    let (title, detail) = match row {
        Some(SetRow::Set(n)) => {
            let s = list.iter().find(|s| s.name == n);
            let mut l = Vec::new();
            if let Some(s) = s {
                l.push(kv("Base", s.base.clone().unwrap_or_else(|| "–".into())));
                l.push(kv("Runs", format!("{} own, {} from the base", s.runs.len(), s.base_runs)));
                l.push(kv("Images", num(s.files)));
                l.push(kv(
                    "Targets",
                    match &s.targets {
                        Some(t) => format!("{} · {} · {}", num(t.count), t.source, t.at.get(..16).unwrap_or(&t.at).replace('T', " ")),
                        None => "none".into(),
                    },
                ));
            }
            (format!("Set {n} on {head}"), l)
        }
        Some(SetRow::Run { id, .. }) => {
            let r = runs.iter().find(|r| r.id == id);
            let l = match r {
                Some(r) => vec![
                    Line::from(Span::styled(r.name.clone(), Style::new().fg(ACCENT).bold())),
                    kv("Node", r.node.clone()),
                    kv("Images", format!("{} · {}", num(r.images), mb(r.bytes))),
                    kv("State", if r.finished { "finished".into() } else { "running".into() }),
                    kv("Folder", format!("runs/{} on {head}", r.id)),
                ],
                None => vec![Line::from("not in the run list of the head node yet").fg(DIM)],
            };
            (format!("Run {}", run_date(&id)), l)
        }
        None => (String::new(), Vec::new()),
    };
    let block = boxed(title);
    let inner = block.inner(right);
    f.render_widget(block, right);
    f.render_widget(Paragraph::new(detail), inner);
}

fn draw_run_detail(f: &mut Frame, app: &mut App, area: Rect, id: &str) {
    let block = boxed(format!("Run {}", run_date(id)));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Ok(r) = app.engine.store.run(id).cloned() else {
        f.render_widget(Paragraph::new("run.json not readable").fg(BAD), inner);
        return;
    };
    let ok = r.entities.values().filter(|e| e.ok).count();
    let failed = r.entities.len() - ok;
    let dur = match (chrono::DateTime::parse_from_rfc3339(&r.started), r.finished.as_deref().and_then(|f| chrono::DateTime::parse_from_rfc3339(f).ok())) {
        (Ok(a), Some(b)) => fmt_dur(Duration::from_secs((b - a).num_seconds().max(0) as u64)),
        _ => "running".into(),
    };
    let o = &r.origin;
    let s = |k: &str| o[k].as_str().unwrap_or("?").to_string();
    let row = |k: &str, v: String| Line::from(vec![Span::styled(format!("{k:<11}"), Style::new().fg(DIM)), Span::raw(v)]);
    let mut lines = vec![
        Line::from(Span::styled(r.name.clone(), Style::new().fg(ACCENT).bold())),
        row("Node", format!("{}{}", r.node, if r.node != app.engine.me.name { " (fetched)" } else { "" })),
        row("Selection", format!("{} · {}", if s("filter").is_empty() { "all".into() } else { s("filter") }, s("scope"))),
        row("Result", format!("{ok} ok · {failed} failed · {dur}{}", if r.aborted { " · aborted" } else { "" })),
    ];
    if app.details {
        let nodes: Vec<String> = o["nodes"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
        lines.push(row("Add-on", s("addon")));
        lines.push(row("render.py", s("render_py")));
        lines.push(row("Blender", format!("{} · {}", s("blender_version"), s("gpu_backend"))));
        lines.push(row("StarBreaker", s("starbreaker")));
        let p4k_date = o["p4k_mtime"].as_i64().and_then(|t| chrono::DateTime::from_timestamp(t, 0)).map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default();
        let p4k_v = o["p4k_version"].as_str().map(|v| format!("{v} · ")).unwrap_or_default();
        let p4k_id = o["p4k_id"].as_str().map(|v| format!(" · {}", v.get(..8).unwrap_or(v))).unwrap_or_default();
        lines.push(row("p4k", format!("{p4k_v}{:.1} GB · {p4k_date}{p4k_id}", o["p4k_bytes"].as_f64().unwrap_or(0.0) / 1e9)));
        lines.push(row("Workers", format!("{} export · {} render{}", o["exporters"], o["renderers"], if nodes.is_empty() { String::new() } else { format!(" + {}", nodes.join(", ")) })));
        lines.push(row("Folder", app.engine.store.run_dir(id).display().to_string()).fg(DIM));
    }
    let text_h = lines.len() as u16 + 1;
    let [text, imgs] = Layout::vertical([Constraint::Length(text_h), Constraint::Min(3)]).areas(inner);
    f.render_widget(Paragraph::new(lines), text);
    // up to 8 images of the run
    let dir = app.engine.store.run_dir(id);
    let files: Vec<PathBuf> = r.entities.values().filter_map(|e| e.files.first()).take(8).map(|f| dir.join(f)).collect();
    if files.is_empty() || imgs.height < 3 {
        return;
    }
    let cols = 4u16;
    let rows_n = (files.len() as u16).div_ceil(cols).min((imgs.height / 5).max(1));
    let cell_w = imgs.width / cols;
    let cell_h = (imgs.height / rows_n).min(cell_w / 2 + 1);
    for (k, p) in files.iter().enumerate().take((rows_n * cols) as usize) {
        let (cx, cy) = (k as u16 % cols, k as u16 / cols);
        let cell = Rect { x: imgs.x + cx * cell_w, y: imgs.y + cy * cell_h, width: cell_w.saturating_sub(1), height: cell_h.saturating_sub(1) };
        if !app.thumbs.contains_key(p) && let Ok(img) = image::open(p) {
            let proto = app.picker.new_resize_protocol(img);
            app.thumbs.insert(p.clone(), proto);
        }
        if let Some(proto) = app.thumbs.get_mut(p) {
            f.render_stateful_widget(StatefulImage::default().resize(Resize::Fit(None)), cell, proto);
        }
    }
}

/// Hardware of a node in one line: "AMD Ryzen 7 9800X3D (8 cores) · 64 GB RAM · RTX 5080 · VRAM 16 GB"
fn hardware_line(h: &Hello) -> String {
    let mut parts = vec![format!("{} ({} cores)", h.cpu.as_deref().unwrap_or("CPU ?"), h.cores)];
    if let Some(ram) = h.ram {
        parts.push(format!("{:.0} GB RAM", ram as f64 / (1u64 << 30) as f64));
    }
    parts.push(h.gpu.clone().unwrap_or_else(|| "GPU ?".into()));
    parts.join(" · ")
}

/// Free space of a node's first disk with the share in use ("412 GB (78%)"), "?" without one
fn disk_cell(h: &Hello) -> String {
    h.disks.first().map(|d| format!("{:.0} GB ({})", d.free as f64 / 1e9, d.used_pct())).unwrap_or_else(|| "?".into())
}

/// Exported items of the run going on (here, or the one another head node reports) that `node` holds; None without
/// a run
fn held_by(e: &Engine, node: &str) -> Option<usize> {
    if let (true, Some(run)) = (e.running(), e.run.as_ref()) {
        return Some(run.holders().into_iter().find(|h| h.0 == node).map(|h| h.1).unwrap_or(0));
    }
    let st = e.remote_run.as_ref().map(|r| &r.status).filter(|s| !s.finished)?;
    Some(st.holders.iter().find(|h| h.0 == node).map(|h| h.1).unwrap_or(0))
}

fn draw_nodes(f: &mut Frame, app: &mut App, area: Rect) {
    // the details tile needs 11 lines (node, hardware, disk, workers, holds, assessed, fetch, manifest, app, p4k,
    // Blender),
    // the state tile gets room for a few more nodes
    let [top, bottom] = Layout::vertical([Constraint::Fill(1), Constraint::Length(BOTTOM_ROW)]).areas(area);
    let e = &app.engine;
    // this node with its current counts
    let mut me = e.me.clone();
    me.slots = e.max_renderers;
    me.exporters = e.exporters_now();
    me.assess = e.assess_info();
    let title = match e.hub.as_ref() {
        None => "Nodes · no cluster (no token in packshot.json)".to_string(),
        Some(h) => match (&h.listen_error, &h.discovery_error) {
            (Some(err), _) => format!("Nodes · {err}"),
            (None, Some(err)) => format!("Nodes · listening on {} · {err}", h.cfg.listen),
            (None, None) => format!("Nodes · listening on {} · discovery {}", h.cfg.listen, if h.cfg.discovery { "on" } else { "off" }),
        },
    };
    let block = boxed(title);
    let inner = block.inner(top);
    f.render_widget(block, top);
    // columns: node, connection, workers (the assessment's marker, then per type the count and, assessed, its
    // items/s at that count), the run's throughput there, runs to fetch, free disk, prerequisites, status
    let cols = |n: &str, c: &str, w: &str, t: &str, ft: &str, k: &str, d: &str| format!("{n:<14.13}{c:<13.12}{w:<43.42}{t:<17.16}{ft:<26.25}{k:<15.14}{d:<36.35}");
    // the run going on (here or the one this node works for): per node its items/s over the last 2 minutes and
    // its images so far
    let throughput: Vec<(String, usize, f64)> = if e.running() {
        e.run_throughput(120.0)
    } else {
        e.remote_run.as_ref().filter(|r| !r.status.finished).map(|r| r.status.nodes.clone()).unwrap_or_default()
    };
    let run_cell = |h: &Hello| match throughput.iter().find(|x| x.0 == h.name) {
        Some((_, n, r)) => format!("{r:.1}/s · {}", num(*n)),
        None if !throughput.is_empty() || e.running() => "0/s".to_string(),
        None => "–".to_string(),
    };
    // "✓ export 4 (26.9/s) · render 8 (38.4/s)": ✓ valid / ! stale / blank without one (no date: the details have it)
    let workers_cell = |h: &Hello| {
        let a = h.assess.as_ref();
        let rate = |steps: Option<&Vec<(usize, f64)>>, k: usize| steps.and_then(|s| crate::engine::rate_at(s, k)).map(|r| format!(" ({r:.1}/s)")).unwrap_or_default();
        let mut parts = Vec::new();
        if h.export {
            parts.push(format!("export {}{}", h.exporters, rate(a.map(|a| &a.export_steps), h.exporters)));
        }
        if h.render {
            parts.push(format!("render {}{}", h.slots, rate(a.map(|a| &a.steps), h.slots)));
        }
        format!("{} {}", crate::assess::marker(h), if parts.is_empty() { "–".to_string() } else { parts.join(" · ") })
    };
    // runs there that are missing here (h fetches them)
    let fetch_cell = |name: &str| {
        let runs = e.fetchable(name);
        if runs.is_empty() { "–".to_string() } else { format!("{} runs · {} images", runs.len(), num(runs.iter().map(|r| r.images).sum())) }
    };
    // Prerequisites: what has to be the same on every node (packshot build, Blender, add-on, StarBreaker, Data.p4k
    // among the exporting nodes); the same: ✓, else what differs (the row is red then, the details say how)
    let prereq_cell = |h: &Hello| {
        let diff = e.differences(h);
        if diff.is_empty() {
            "✓".to_string()
        } else {
            diff.iter().map(|d| match d.split(' ').next().unwrap_or("") { "packshot" => "build", w => w }).collect::<Vec<_>>().join(", ")
        }
    };
    let mut lines = vec![Line::from(format!("{}Status", cols("Node", "Connection", "  Workers", "Run", "Fetch (h)", "Disk", "Prerequisites"))).fg(DIM)];
    let rows = app.node_rows();
    for (i, (conn, name)) in rows.iter().enumerate() {
        let row = |h: &Hello, c: &str, status: (&str, Color)| {
            let fetch = if h.name == e.me.name { "–".to_string() } else { fetch_cell(&h.name) };
            let text = cols(name, c, &workers_cell(h), &run_cell(h), &fetch, &disk_cell(h), &prereq_cell(h));
            Line::from(vec![Span::raw(text), Span::styled(status.0.to_string(), Style::new().fg(status.1))])
        };
        let mut line = if i == 0 {
            let status = if e.running() {
                ("head node", WARN)
            } else if e.assessing() {
                ("assessing", WARN)
            } else {
                ("ready", OK)
            };
            row(&me, "here", status)
        } else if let Some(p) = e.hub.as_ref().and_then(|h| h.peer(*conn)) {
            let h = &p.hello;
            let conn_s = if p.outbound { "connected ↗" } else { "connected ↙" };
            // another state: the whole row red, the details tile and the state tile say what differs
            if !e.differences(h).is_empty() {
                row(h, conn_s, ("another state", BAD)).fg(BAD)
            } else if h.running {
                row(h, conn_s, ("head node", WARN))
            } else if h.assessing {
                row(h, conn_s, ("assessing", WARN))
            } else {
                row(h, conn_s, ("ready", OK))
            }
        } else {
            // an address that is not connected (config peer or discovered node): its host name once known
            let h = e.hub.as_ref();
            let label = h.and_then(|h| h.addr_names.get(name).or_else(|| h.discovered.get(name).map(|(n, _)| n))).cloned().unwrap_or_else(|| name.clone());
            let err = h.and_then(|h| h.dial_errors.get(name)).cloned().unwrap_or_else(|| "connecting …".into());
            let err = if label == *name { err } else { format!("{name}: {err}") };
            Line::from(vec![Span::raw(cols(&label, "–", "", "", "", "", "")), Span::styled(err, Style::new().fg(DIM))])
        };
        if i == app.node_sel {
            line = line.style(Style::new().bg(SEL_BG));
        }
        lines.push(line);
    }
    f.render_widget(Paragraph::new(lines), inner);

    let [l, r] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(bottom);
    // details of the selected node: the same lines for every node
    let sel = rows.get(app.node_sel).cloned();
    let peer = sel.as_ref().and_then(|(c, _)| e.hub.as_ref().and_then(|h| h.peer(*c)));
    let here = app.node_sel == 0;
    let title = match (here, peer) {
        (true, _) => format!("Node · {} (here)", e.me.name),
        (false, Some(p)) => format!("Node · {}", p.hello.name),
        (false, None) => format!("Node · {}", sel.as_ref().map(|(_, n)| n.as_str()).unwrap_or("")),
    };
    let block = boxed(title);
    let inner = block.inner(l);
    f.render_widget(block, l);
    let row = |k: &str, v: String| Line::from(vec![Span::styled(format!("{k:<13}"), Style::new().fg(DIM)), Span::raw(v)]);
    let mut lines = Vec::new();
    let h = if here { Some(me.clone()) } else { peer.map(|p| p.hello.clone()) };
    match h {
        Some(h) => {
            let conn = if here { String::new() } else if peer.is_some_and(|p| p.outbound) { " · connected ↗".into() } else { " · connected ↙".into() };
            let ip = if here { crate::net::own_ip().unwrap_or_else(|| "?".into()) } else { peer.map(|p| p.ip.clone()).unwrap_or_default() };
            lines.push(row("Node", format!("{} · {} · {ip}{conn}", h.caps(), h.os)));
            lines.push(row("Hardware", hardware_line(&h)));
            lines.push(row("Disk", h.disk_text()));
            // workers: another state in red, a run going there in yellow (the state tile says why)
            let color = if !here && !e.differences(&h).is_empty() {
                BAD
            } else if !here && h.running {
                WARN
            } else {
                Color::Reset
            };
            let workers = if h.render || h.export { h.workers_text() } else { "– (starts, stores, watches)".into() };
            lines.push(Line::from(vec![Span::styled(format!("{:<13}", "Workers"), Style::new().fg(DIM)), Span::styled(workers, Style::new().fg(color))]));
            // exported items of the run going on that it holds, not rendered yet
            let holds = held_by(e, &h.name).map(|n| format!("{} items", num(n))).unwrap_or_else(|| "–".into());
            lines.push(row("Holds", holds));
            // the assessment: its result, stale in yellow, or the one going on
            let assessed = if here { e.assess_text() } else if h.assessing { "assessing …".into() } else { crate::assess::info_text(h.assess.as_ref(), h.render) };
            let stale = h.assess.as_ref().is_some_and(|a| !a.valid);
            lines.push(Line::from(vec![Span::styled(format!("{:<13}", "Assessed (b)"), Style::new().fg(DIM)), Span::styled(assessed, Style::new().fg(if stale { WARN } else { Color::Reset }))]));
            if !here {
                let runs = e.fetchable(&h.name);
                let images: usize = runs.iter().map(|r| r.images).sum();
                let bytes: u64 = runs.iter().map(|r| r.bytes).sum();
                let known = e.peer_runs.get(&h.name).map(|v| v.len()).unwrap_or(0);
                lines.push(row("Fetch (h)", format!("{} runs · {} images · {} (of {known} runs there)", runs.len(), num(images), mb(bytes))));
                let m = match e.manifest.as_ref() {
                    Some(m) => format!("{} entities from {} ({})", m.entities.values().filter(|v| **v).count(), m.from, m.at.get(..16).unwrap_or(&m.at).replace('T', " ")),
                    None => "none".into(),
                };
                lines.push(row("Manifest (m)", m));
            }
            lines.push(row("App", format!("packshot {} · StarBreaker {}", h.version, h.starbreaker.as_deref().unwrap_or("–"))));
            let p4k = match h.p4k_bytes {
                Some(b) => format!("{}{} · {:.0} GB", h.p4k_version.as_ref().map(|v| format!("{v} · ")).unwrap_or_default(), h.p4k_id.as_deref().unwrap_or("?").get(..8).unwrap_or("?"), b as f64 / 1e9),
                None => "– (does not export)".into(),
            };
            lines.push(row("p4k", p4k));
            let blender = if h.blender.is_empty() { "?".to_string() } else { format!("{} · add-on {}", h.blender, h.addon.as_deref().unwrap_or("?")) };
            lines.push(row("Blender", blender));
        }
        None => {
            let addr = sel.as_ref().map(|(_, n)| n.clone()).unwrap_or_default();
            let err = e.hub.as_ref().and_then(|h| h.dial_errors.get(&addr)).cloned().unwrap_or_else(|| "not connected".into());
            lines.push(row("Node", format!("{addr}: {err}")));
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
    draw_state(f, app, r);
}

/// The next run as a whole: where it starts (or why it does not), who exports and renders with how many workers,
/// how long it takes and how much room the images need, both from the head node's latest run
fn draw_state(f: &mut Frame, app: &App, area: Rect) {
    let e = &app.engine;
    let block = boxed("Next run");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let head_name = e.head.clone().unwrap_or_else(|| e.me.name.clone());
    let mut me = e.me.clone();
    me.slots = e.max_renderers;
    me.exporters = e.exporters_now();
    let mut peers: Vec<Hello> = e.connected_nodes().into_iter().map(|(_, h)| h).collect();
    peers.sort_by(|a, b| a.name.cmp(&b.name));
    let head = if e.head.is_none() { Some(me.clone()) } else { peers.iter().find(|h| h.name == head_name).cloned() };
    let mut nodes = vec![me];
    nodes.extend(peers);
    // who would take part: the nodes with the head node's state
    let taking: Vec<&Hello> = match &head {
        Some(hd) => nodes.iter().filter(|h| e.differences_to(h, hd).is_empty()).collect(),
        None => Vec::new(),
    };
    let todo = app.sel.todo.len();
    let problems = e.state_problems();
    let starts: (String, Color) = if e.running() {
        ("no: a run is going here".into(), WARN)
    } else if let Some(other) = e.running_elsewhere() {
        (format!("no: {other} has a run going"), WARN)
    } else if head.is_none() {
        (format!("no: {head_name} is not connected"), BAD)
    } else if e.remote_sel().is_some_and(|r| !r.loaded) {
        (format!("waiting for the state of {head_name} …"), DIM)
    } else if let Some((name, diff)) = problems.first() {
        let more = if problems.len() > 1 { format!(" (+{} more nodes)", problems.len() - 1) } else { String::new() };
        (format!("no: {name} has another state: {}{more}", diff.first().cloned().unwrap_or_default()), BAD)
    } else if e.own_len == 0 {
        (format!("no: set {} has no target list (t in view 2)", e.target_set()), BAD)
    } else if todo == 0 {
        ("nothing to render with this filter and scope".into(), DIM)
    } else if let (None, Some(p)) = (e.head.as_ref(), e.capacity_problem()) {
        (format!("no: {p}"), BAD)
    } else {
        (format!("on {head_name} · set {} · {} to render (filter {}, {})", e.target_set(), num(todo), if app.filter.is_empty() { "all" } else { &app.filter }, app.scope.key()), OK)
    };
    let row = |k: &str, v: Span<'static>| Line::from(vec![Span::styled(format!("{k:<14}"), Style::new().fg(DIM)), v]);
    // only the counts: who has how many is in the table above
    let workers = |export: bool| {
        let total: usize = taking.iter().filter(|h| if export { h.export } else { h.render }).map(|h| if export { h.exporters } else { h.slots }).sum();
        if total == 0 {
            Span::styled(format!("no node {}", if export { "exports" } else { "renders" }), Style::new().fg(BAD))
        } else {
            Span::raw(format!("{total} workers"))
        }
    };
    let stats = match e.remote_sel() {
        Some(r) => r.stats.clone(),
        None => e.stats.clone(),
    };
    let estimate = match (&stats, todo) {
        (Some(st), n) if n > 0 && st.items_per_s > 0.0 => {
            Span::raw(format!("~{} for {} items (last run {}: {:.1} items/s)", fmt_dur(Duration::from_secs_f64(n as f64 / st.items_per_s)), num(n), run_date(&st.run), st.items_per_s))
        }
        (None, _) => Span::styled(format!("– (no finished run of 50+ items on {head_name})"), Style::new().fg(DIM)),
        _ => Span::styled("–", Style::new().fg(DIM)),
    };
    let disk = head.as_ref().and_then(|h| h.disks.first().cloned());
    let free = |place: &str| disk.as_ref().map(|d| format!("{:.0} GB free {place} ({})", d.free as f64 / 1e9, d.used_pct())).unwrap_or_else(|| format!("free space {place} unknown"));
    let images = match &stats {
        Some(st) if st.bytes_per_image > 0 && todo > 0 => {
            let need = todo as u64 * st.bytes_per_image;
            let short = disk.as_ref().is_some_and(|d| d.free < need);
            Span::styled(format!("~{} into runs/ on {head_name} · {}", mb(need), free("there")), Style::new().fg(if short { BAD } else { Color::Reset }))
        }
        _ => Span::raw(free(&format!("on {head_name}"))),
    };
    let lines = vec![
        row("Starts", Span::styled(starts.0, Style::new().fg(starts.1))),
        row("Export", workers(true)),
        row("Render", workers(false)),
        row("Estimate", estimate),
        row("Images", images),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_log(f: &mut Frame, app: &App, area: Rect) {
    let e = &app.engine;
    // errors of another node's run come with its status
    let remote: Option<&Vec<(String, String)>> = if e.running() { None } else { app.remote_status().map(|s| &s.errors) };
    let count = remote.map(|r| r.len()).unwrap_or(e.errors.len());
    let tab = |on: bool, s: String| if on { Span::styled(format!(" {s} "), Style::new().fg(Color::Black).bg(ACCENT)) } else { Span::styled(format!(" {s} "), Style::new().fg(DIM)) };
    let block = Block::bordered()
        .border_style(Style::new().fg(DIM))
        .title(Line::from(vec![" ".into(), tab(!app.errors_tab, "Log".into()), tab(app.errors_tab, format!("Errors {count}")), " ".into()]));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let h = inner.height as usize;
    let lines: Vec<Line> = if app.errors_tab {
        let list: Vec<(String, String)> = match remote {
            Some(r) => r.clone(),
            None => e.errors.iter().map(|(t, err)| (e.targets[*t].entity.clone(), err.clone())).collect(),
        };
        list.iter()
            .rev()
            .take(h)
            .rev()
            .map(|(ent, err)| Line::from(vec![Span::styled(format!("{ent}  "), Style::new().fg(BAD)), Span::raw(err.clone())]))
            .collect()
    } else {
        e.log
            .iter()
            .rev()
            .take(h)
            .rev()
            .map(|l| {
                let color = match l.level {
                    Level::Ok => Color::Reset,
                    Level::Error => BAD,
                    Level::Info => ACCENT,
                };
                Line::from(vec![Span::styled(format!("{} ", l.time), Style::new().fg(DIM)), Span::styled(l.text.clone(), Style::new().fg(color))])
            })
            .collect()
    };
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_keys(f: &mut Frame, app: &App, area: Rect) {
    let running = app.engine.running();
    let remote = !running && app.remote_status().is_some();
    let keys: &[(&str, &str)] = if app.input.is_some() {
        &[("type", "text"), ("Ctrl+U", "clear"), ("Enter", "ok"), ("Esc", "back")]
    } else {
        match (app.view, running, remote) {
            (View::Run, true, _) => &[("p", "pause"), ("a", "abort"), ("+/-", "render"), ("</>", "export"), ("←/→", "preview"), ("e", "log/errors"), ("Tab", "view"), ("q", "abort+quit")],
            (View::Run, false, true) => &[("a", "abort"), ("+/-", "render"), ("</>", "export"), ("e", "log/errors"), ("Tab", "view"), ("q", "quit")],
            (View::Run, false, false) => &[("n", "head node"), ("s", "set"), ("/", "filter"), ("x", "clear"), ("u", "scope"), ("↑↓", "list"), ("Enter", "start"), ("+/-", "render"), ("</>", "export"), ("e", "log"), ("Tab", "view"), ("q", "quit")],
            (View::Sets, true, _) => &[("↑↓", "set/run"), ("i", "details"), ("Tab", "view"), ("q", "abort+quit")],
            (View::Sets, false, _) if app.remote_sets().is_some() => &[("↑↓", "set/run"), ("Enter", "target set there"), ("i", "details"), ("Tab", "view"), ("q", "quit")],
            (View::Sets, false, _) => &[("↑↓", "set/run"), ("Enter", "target set"), ("n", "new set"), ("t", "target list"), ("l", "to live"), ("x", "delete"), ("i", "details"), ("Tab", "view"), ("q", "quit")],
            (View::Nodes, _, _) if app.node_sel == 0 => &[("↑↓", "node"), ("m", "drop manifest"), ("b", "assess/abort"), ("Tab", "view"), ("q", if running { "abort+quit" } else { "quit" })],
            (View::Nodes, _, _) => &[("↑↓", "node"), ("h", "fetch runs"), ("m", "fetch manifest"), ("r", "refresh"), ("b", "assess/abort"), ("Tab", "view"), ("q", if running { "abort+quit" } else { "quit" })],
        }
    };
    let mut spans = Vec::new();
    for (k, v) in keys {
        spans.push(Span::styled(format!(" {k} "), Style::new().fg(Color::Black).bg(Color::Gray)));
        spans.push(Span::styled(format!(" {v}  "), Style::new().fg(DIM)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Config, RemoteRun, RemoteSel, Target};
    use crate::net::{Hello, RunStatus, WorkerStatus};
    use crate::store::Store;
    use crate::tables::Tables;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn screen(app: &mut App) -> String {
        let mut t = Terminal::new(TestBackend::new(200, 50)).unwrap();
        t.draw(|f| draw(f, app)).unwrap();
        let buf = t.backend().buffer();
        (0..buf.area.height).map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    /// The dialogs' text field: insert at the cursor, move, delete on both sides, clear
    #[test]
    fn field_editing() {
        let mut f = Field::new("live");
        assert_eq!(f.cursor, 4);
        for k in [KeyCode::Home, KeyCode::Char('x'), KeyCode::End, KeyCode::Char('2'), KeyCode::Left, KeyCode::Backspace, KeyCode::Right, KeyCode::Home, KeyCode::Delete] {
            assert!(f.key(k, KeyModifiers::NONE));
        }
        assert_eq!(f.text, "liv2");
        assert!(!f.key(KeyCode::Up, KeyModifiers::NONE), "not an editing key");
        assert!(f.key(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert!(f.text.is_empty() && f.cursor == 0);
        // umlauts are one character each
        let mut f = Field::new("Größe");
        f.key(KeyCode::Left, KeyModifiers::NONE);
        f.key(KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(f.text, "Gröe");
    }

    /// Every view draws on a node with capabilities, without panics and with its parts: the selection with head node
    /// and set, the nodes with the state tile, the sets, the selection against another head node and another node's
    /// run with its workers
    #[test]
    fn views_draw() {
        let root = std::env::temp_dir().join(format!("packshot-ui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = Store::open(&root.join("store")).unwrap();
        let set = store.load_set(LIVE).unwrap();
        let me = Hello {
            name: "DRAGONEGG".into(),
            version: "t".into(),
            os: "linux".into(),
            render: true,
            export: true,
            blender: "5.2.3".into(),
            addon: Some("1a2b3c4d5e".into()),
            slots: 6,
            exporters: 2,
            cores: 8,
            cpu: Some("AMD Ryzen 7 9800X3D".into()),
            ram: Some(64 << 30),
            gpu: Some("RTX 5080 · VRAM 16 GB".into()),
            p4k_bytes: Some(161_000_000_000),
            p4k_id: Some("d5ab91cf194a8f60".into()),
            p4k_version: Some("4.10.2 (4.10.197.2878)".into()),
            starbreaker: Some("655d5f68".into()),
            disks: vec![crate::net::DiskInfo { mount: "/mnt/DATA".into(), free: 116_000_000_000, total: 417_000_000_000 }],
            ..Default::default()
        };
        let x = root.join("x");
        let cfg = Config { p4k: x.clone(), starbreaker: x.clone(), blender: x.clone(), render_script: x, work: root.join("work"), size: 256, chunk: 50 };
        let targets = vec![Target { uuids: vec!["0".into()], names: Some(vec!["Gun".into()]), entity: "gun_a".into(), kind: "WeaponPersonal".into(), sub_type: "Small".into(), worn: false }];
        let mut engine = Engine::new(cfg, targets, Tables::default(), store, set, None, me).unwrap();
        engine.max_renderers = 6;
        engine.max_exporters = 2;
        engine.cores = 8;
        let args = Args { filter: String::new(), scope: Scope::Missing, plain: false, serve: false, dry_run: false, workers: 6, exporters: 2, name: String::new(), line: String::new() };
        let mut app = App::new(engine, Picker::halfblocks(), args);
        app.refresh_sets();

        let s = screen(&mut app);
        assert!(!s.contains("Target list for live"), "no dialog at the start: {s}");
        // without a target list: a hint in the run view, Enter opens the dialog
        let saved = app.engine.targets.clone();
        app.engine.use_targets(Vec::new());
        app.refresh();
        let s = screen(&mut app);
        assert!(s.contains("has no target list yet: Enter chooses one") && !s.contains("Target list for live"), "{s}");
        app.key(KeyCode::Enter, KeyModifiers::NONE).unwrap();
        let s = screen(&mut app);
        assert!(s.contains("Target list for live") && s.contains("live has no target list yet") && s.contains("Other file"), "{s}");
        app.input = None;
        app.engine.use_targets(saved);
        app.refresh();
        // with target lists in the packshot folder: they are offered, targets.json first
        std::fs::write(root.join("targets.json"), serde_json::to_vec(&app.engine.targets).unwrap()).unwrap();
        std::fs::write(root.join("older.json"), serde_json::to_vec(&app.engine.targets).unwrap()).unwrap();
        std::fs::write(root.join("packshot.json"), "{}").unwrap();
        app.engine.targets_file = root.join("targets.json");
        app.open_targets(LIVE);
        let s = screen(&mut app);
        assert!(s.contains("▸ targets.json") && s.contains("  older.json") && !s.contains("packshot.json"), "{s}");
        if std::env::var("PACKSHOT_SHOW").is_ok() {
            println!("{s}");
        }
        app.input = None;
        let s = screen(&mut app);
        assert!(s.contains("Head node  here") && s.contains("gun_a") && s.contains("2 export · 6 render · 8 cores"), "{s}");
        app.view = View::Nodes;
        let s = screen(&mut app);
        assert!(s.contains("Next run") && s.contains("on DRAGONEGG · set live · 1 to render") && s.contains("2 workers") && s.contains("6 workers"), "{s}");
        assert!(s.contains("Assessed (b) none") && s.contains("  export 2 · render 6 ") && s.contains("Prerequisites"), "{s}");
        // a stale assessment: the marker and the rates in the workers column (no date), all of it in the details
        app.engine.assessment = Some(crate::assess::Assessment { workers: 6, render: 6.3, exporters: Some(2), export: Some(7.1), date: "2026-11-04T14:02:00+01:00".into(), ..Default::default() });
        app.engine.assess_stale = vec!["Blender 5.2.4 (assessed 5.2.3)".into()];
        let s = screen(&mut app);
        assert!(s.contains("Assessed (b) ! stale · 6 render 6.3/s · 2 export 7.1/s · 2026-11-04 14:02") && s.contains("! export 2 · render 6"), "{s}");
        app.engine.assess_stale.clear();
        let s = screen(&mut app);
        assert!(s.contains("Assessed (b) ✓ 6 render") && s.contains("✓ export 2 · render 6"), "{s}");
        app.engine.assessment = None;
        if std::env::var("PACKSHOT_SHOW").is_ok() {
            println!("{s}");
        }
        app.view = View::Sets;
        let s = screen(&mut app);
        assert!(s.contains("Sets and runs") && s.contains("live"), "{s}");

        // another node as head node, its state not there yet
        app.view = View::Run;
        app.engine.head = Some("servbox".into());
        let set_list = vec![crate::store::SetSummary { name: "exp".into(), base: Some("live".into()), base_runs: 3, runs: vec!["2026-11-04_10-00_x".into()], files: 120, targets: None }];
        app.engine.remote = Some(RemoteSel { node: "servbox".into(), set: "exp".into(), sets: vec!["exp".into()], set_list, stats: None, images: Default::default(), failed: Default::default(), loaded: false });
        let s = screen(&mut app);
        assert!(s.contains("Head node  servbox") && s.contains("waiting for the state of servbox"), "{s}");
        // the Sets view shows the head node's sets, read only
        app.view = View::Sets;
        app.refresh_sets();
        let s = screen(&mut app);
        assert!(s.contains("Sets and runs on servbox · read only") && s.contains("▾ exp") && s.contains("4 runs"), "{s}");
        app.view = View::Run;

        // a run on another node: its status and workers, no image here yet
        let status = RunStatus {
            run: "r1".into(),
            head: "servbox".into(),
            starter: "DRAGONEGG".into(),
            set: "exp".into(),
            total: 10,
            ok: 3,
            failed: 1,
            held: 7,
            ready: 3,
            holders: vec![("DRAGONEGG".into(), 5), ("servbox".into(), 2)],
            workers: vec![
                WorkerStatus { label: "servbox/R1".into(), phase: "render".into(), entity: "gun_b".into(), done: 1, total: 4, secs: 2.0 },
                WorkerStatus { label: "DRAGONEGG/R1".into(), phase: "fetch".into(), entity: "gun_d".into(), done: 0, total: 2, secs: 0.5 },
            ],
            errors: vec![("gun_c".into(), "boom".into())],
            ..Default::default()
        };
        app.engine.remote_run = Some(RemoteRun { from: "servbox".into(), status, at: Instant::now() });
        let s = screen(&mut app);
        assert!(s.contains("head node servbox") && s.contains("servbox/R1") && s.contains("gun_b") && s.contains("4/10") && s.contains("Errors 1"), "{s}");
        assert!(s.contains("pool      7 (     3 waiting for Blender)") && s.contains("DRAGONEGG/R1        fetch"), "{s}");
        // the items each node holds, in view 3's details
        app.view = View::Nodes;
        let s = screen(&mut app);
        assert!(s.contains("Holds        5 items"), "{s}");
        app.view = View::Run;
        let _ = crate::store::remove_tree(&root);
    }
}
