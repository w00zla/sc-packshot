// Render profiles (ADRs 0022, 0041, 0042): a profile is a folder with profile.toml (parameters such as samples,
// [addon] with the add-on override and knobs, ADR 0049) and hooks.py. default, found beside render.py as render.py
// finds it, is the base of every profile: its parameters lie under the profile's, its hooks run where the profile has
// none. A run's profile is chosen with
// --profile: a name (an own profile in <store>/profiles/<name> hides a bundled one beside render.py) or a folder. The
// head node sends render.py, default and the run's profile with every render chunk (ADR 0024); its content hash
// keys the Blender processes and goes into the run record. Its add-on knobs are checked against the add-on the run
// uses before a run starts (check_knobs), render.py checks them again.
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::net::FileEntry;

/// The base profile, bundled beside render.py
pub const BASE: &str = "default";
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
    /// default as render.py finds it
    pub base: PathBuf,
    /// where it came from: store (an own one), bundled, folder (--profile with a path), or sent by the run's starter
    pub from: String,
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

pub fn same_dir(a: &Path, b: &Path) -> bool {
    a == b || fs::canonicalize(a).ok().zip(fs::canonicalize(b).ok()).is_some_and(|(a, b)| a == b)
}

/// default as render.py finds it
pub fn base_dir(script: &Path) -> Option<PathBuf> {
    bundled_dirs(script).into_iter().map(|d| d.join(BASE)).find(|d| is_profile(d))
}

/// --profile: a name (an own profile in <store>/profiles first, then the bundled ones beside render.py) or a folder
/// (relative to `cwd`); Err: why there is none
pub fn resolve(arg: &str, store: &Path, script: &Path, cwd: &Path) -> std::result::Result<Profile, String> {
    let (dir, from) = if crate::net::safe_name(arg) {
        let own = store.join("profiles").join(arg);
        let mut candidates = vec![own.clone()];
        candidates.extend(bundled_dirs(script).into_iter().map(|d| d.join(arg)));
        match candidates.iter().find(|d| is_profile(d)) {
            // in a bundle the store's profiles/ is the bundled one
            Some(d) => (d.clone(), if *d == own && !bundled_dirs(script).iter().any(|b| same_dir(&b.join(arg), d)) { "store" } else { "bundled" }),
            None => return Err(format!("{arg} not found in {}", candidates.iter().map(|d| d.parent().unwrap_or(d).display().to_string()).collect::<Vec<_>>().join(" or "))),
        }
    } else {
        let d = cwd.join(arg);
        if !is_profile(&d) {
            return Err(format!("no profile.toml in {}", d.display()));
        }
        (d, "folder")
    };
    let mut p = Profile::at(&dir, script).ok_or_else(|| format!("{BASE} not found beside {}", script.display()))?;
    p.from = from.into();
    // a render chunk's folder holds default and the run's profile side by side, by their names
    if p.name.eq_ignore_ascii_case(BASE) && !same_dir(&p.dir, &p.base) {
        return Err(format!("{}: the name {BASE} is reserved for the bundled profile, the base of every profile; give it another name", p.dir.display()));
    }
    check_params(&p.dir)?;
    check_params(&p.base)?;
    Ok(p)
}

/// Size and format are the set's (ADR 0045): a profile.toml that still sets them is refused; so is an `[addon]` table
/// packshot cannot read (ADR 0049; whether the add-on has its knobs is Profile::check_knobs)
pub fn check_params(dir: &Path) -> std::result::Result<(), String> {
    let text = fs::read_to_string(dir.join("profile.toml")).unwrap_or_default();
    if let Some(k) = ["size", "format"].into_iter().find(|k| value(&text, "[params]", k).is_some()) {
        return Err(format!("{}: profile.toml sets `{k}`: size and format belong to the set, remove it", dir.join("profile.toml").display()));
    }
    addon_table(&text).map(|_| ()).map_err(|e| format!("{}: {e}", dir.join("profile.toml").display()))
}

/// A value of profile.toml's `[addon]` table: a TOML scalar or array
#[derive(Clone, Debug, PartialEq)]
pub enum Knob {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Array(Vec<Knob>),
}

impl Knob {
    fn is_number(&self) -> bool {
        matches!(self, Knob::Int(_) | Knob::Float(_))
    }

    fn kind(&self) -> String {
        match self {
            Knob::Int(_) | Knob::Float(_) => "a number".into(),
            Knob::Bool(_) => "a bool".into(),
            Knob::Str(_) => "a string".into(),
            Knob::Array(v) => format!("a list of {}", v.len()),
        }
    }
}

/// What packshot reads of a constant in the add-on's runtime/constants.py: the type of its literal; Unknown for
/// anything else (an expression, Matrix(...)): known by name, render.py checks its type
#[derive(Clone, Debug, PartialEq)]
pub enum Constant {
    Number,
    Bool,
    Str,
    /// a tuple or list of this many numbers
    Numbers(usize),
    Unknown,
}

/// Whether `knob` may replace the constant: numbers for numbers (int and float alike, a bool is none), a bool for a
/// bool, a string for a string, a list of as many numbers for a tuple or list of numbers
pub fn check_knob(knob: &Knob, c: &Constant) -> std::result::Result<(), String> {
    let ok = match (c, knob) {
        (Constant::Unknown, _) => true,
        (Constant::Number, k) => k.is_number(),
        (Constant::Bool, Knob::Bool(_)) | (Constant::Str, Knob::Str(_)) => true,
        (Constant::Numbers(n), Knob::Array(v)) => v.len() == *n && v.iter().all(Knob::is_number),
        _ => false,
    };
    let want = match c {
        Constant::Number => "a number".to_string(),
        Constant::Bool => "a bool".to_string(),
        Constant::Str => "a string".to_string(),
        Constant::Numbers(n) => format!("{n} numbers"),
        Constant::Unknown => String::new(),
    };
    if ok { Ok(()) } else { Err(format!("{}, the constant is {want}", knob.kind())) }
}

/// A constant's name: upper case (the add-on's convention), the only names `[addon]` sets besides `override`
fn is_constant_name(s: &str) -> bool {
    let mut c = s.chars();
    c.next().is_some_and(|f| f.is_ascii_uppercase()) && c.all(|x| x.is_ascii_uppercase() || x.is_ascii_digit() || x == '_')
}

/// The top-level `NAME = <literal>` assignments of a runtime/constants.py (NAME upper case, an annotation allowed):
/// numbers, bools, strings and tuples/lists of numbers by their type, any other value (an expression, a call such as
/// Matrix(...), a dict, nested tuples) as Unknown; a later assignment wins, as in Python
pub fn constants(py: &str) -> std::collections::HashMap<String, Constant> {
    let mut out = std::collections::HashMap::new();
    let lines: Vec<&str> = py.lines().collect();
    let (mut i, mut in_string) = (0, false);
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        // a triple-quoted string (the docstring) over several lines: nothing in it is an assignment
        if (line.matches("\"\"\"").count() + line.matches("'''").count()) % 2 == 1 {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        let name: String = line.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        if !is_constant_name(&name) {
            continue;
        }
        let mut rest = line[name.len()..].trim_start();
        if let Some(r) = rest.strip_prefix(':') {
            rest = r.find('=').map_or("", |at| &r[at..]);
        }
        let Some(v) = rest.strip_prefix('=').filter(|v| !v.starts_with('=')) else { continue };
        // a value over several lines: until its brackets close
        let mut value = py_code(v);
        while depth(&value) > 0 && i < lines.len() {
            value.push(' ');
            value.push_str(&py_code(lines[i]));
            i += 1;
        }
        out.insert(name, py_literal(value.trim()));
    }
    out
}

/// A line of Python without its comment (a # outside a string)
fn py_code(line: &str) -> String {
    let mut quote = None;
    let mut out = String::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match quote {
            Some(_) if c == '\\' => {
                out.push(c);
                out.extend(chars.next());
                continue;
            }
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '#' => break,
            None if c == '"' || c == '\'' => quote = Some(c),
            None => {}
        }
        out.push(c);
    }
    out
}

/// Open brackets of a piece of Python code (strings left out)
fn depth(code: &str) -> i32 {
    let (mut d, mut quote, mut esc) = (0, None, false);
    for c in code.chars() {
        match quote {
            Some(_) if esc => esc = false,
            Some(_) if c == '\\' => esc = true,
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                '"' | '\'' => quote = Some(c),
                '(' | '[' | '{' => d += 1,
                ')' | ']' | '}' => d -= 1,
                _ => {}
            },
        }
    }
    d
}

fn py_number(s: &str) -> bool {
    let t = s.strip_prefix(['-', '+']).unwrap_or(s).replace('_', "");
    let radix = [("0x", 16), ("0X", 16), ("0o", 8), ("0O", 8), ("0b", 2), ("0B", 2)].into_iter().find(|(p, _)| t.starts_with(p));
    match radix {
        Some((p, r)) => i64::from_str_radix(&t[p.len()..], r).is_ok(),
        // f64 also reads inf and nan, which are names in Python
        None => t.starts_with(|c: char| c.is_ascii_digit() || c == '.') && t.parse::<f64>().is_ok(),
    }
}

fn py_string(s: &str) -> bool {
    let Some(q) = s.chars().next().filter(|c| *c == '"' || *c == '\'') else { return false };
    if s.starts_with(&format!("{q}{q}{q}")) {
        return false;
    }
    let mut esc = false;
    for (at, c) in s.char_indices().skip(1) {
        match c {
            _ if esc => esc = false,
            '\\' => esc = true,
            c if c == q => return at + 1 == s.len(),
            _ => {}
        }
    }
    false
}

/// The type of a Python literal (Unknown: not one packshot reads)
fn py_literal(v: &str) -> Constant {
    match v {
        "True" | "False" => return Constant::Bool,
        _ if py_number(v) => return Constant::Number,
        _ if py_string(v) => return Constant::Str,
        _ => {}
    }
    let inner = v.strip_prefix('(').and_then(|r| r.strip_suffix(')')).or_else(|| v.strip_prefix('[').and_then(|r| r.strip_suffix(']')));
    let Some(inner) = inner.filter(|i| !i.contains(['(', ')', '[', ']'])) else { return Constant::Unknown };
    if inner.trim().is_empty() {
        return Constant::Numbers(0);
    }
    let items: Vec<&str> = inner.split(',').map(str::trim).collect();
    // (1.0) is a number in Python, (1.0,) a tuple
    if v.starts_with('(') && items.len() == 1 {
        return if py_number(items[0]) { Constant::Number } else { Constant::Unknown };
    }
    let items: Vec<&str> = match items.split_last() {
        Some((&"", head)) => head.to_vec(),
        _ => items,
    };
    if items.iter().all(|i| py_number(i)) { Constant::Numbers(items.len()) } else { Constant::Unknown }
}

/// The knobs of an `[addon]` table: constant names with their values, in file order
pub type Knobs = Vec<(String, Knob)>;

/// The `[addon]` table of a profile.toml (ADR 0049): `override` (the profile's own add-on copy, a string) and the knobs
/// (constants' upper-case names with TOML scalars or arrays); any other key, a value packshot cannot read, a key
/// twice or 1.0.0's top-level `addon` next to the table is refused
pub fn addon_table(toml: &str) -> std::result::Result<(Option<String>, Knobs), String> {
    let Some(start) = header_end(toml, "[addon]") else { return Ok((None, Vec::new())) };
    if value(toml, "", "addon").is_some() {
        return Err("a top-level `addon` and an [addon] table: put the add-on copy into the table as `override = \"...\"`".into());
    }
    let (mut copy, mut knobs) = (None, Vec::<(String, Knob)>::new());
    let mut rest = &toml[start..];
    loop {
        rest = skip_blank(rest);
        if rest.is_empty() || rest.starts_with('[') {
            break;
        }
        let key: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
        let after = rest[key.len()..].trim_start_matches([' ', '\t']);
        let Some(after) = after.strip_prefix('=').filter(|_| !key.is_empty()) else {
            return Err(format!("[addon]: unreadable line `{}`", rest.lines().next().unwrap_or("").trim()));
        };
        let (v, r) = knob_value(after.trim_start_matches([' ', '\t'])).map_err(|e| format!("[addon] {key}: {e}"))?;
        let r = r.trim_start_matches([' ', '\t']);
        if !(r.is_empty() || r.starts_with(['#', '\n', '\r'])) {
            return Err(format!("[addon] {key}: unreadable after the value: `{}`", r.lines().next().unwrap_or("").trim()));
        }
        rest = r.split_once('\n').map_or("", |(_, r)| r);
        if key == "override" {
            match v {
                Knob::Str(s) if copy.is_none() => copy = Some(s),
                Knob::Str(_) => return Err("[addon] override: set twice".into()),
                _ => return Err(format!("[addon] override: {}, the add-on copy's folder is a string", v.kind())),
            }
        } else if !is_constant_name(&key) {
            return Err(format!("[addon] {key}: unknown option (`override`, or an add-on constant's UPPER_CASE name)"));
        } else if knobs.iter().any(|(k, _)| *k == key) {
            return Err(format!("[addon] {key}: set twice"));
        } else {
            knobs.push((key, v));
        }
    }
    Ok((copy, knobs))
}

/// Where the text after a table header line (`[addon]  # ...`) starts
fn header_end(toml: &str, header: &str) -> Option<usize> {
    let mut at = 0;
    for line in toml.split_inclusive('\n') {
        at += line.len();
        if line.split('#').next().unwrap_or("").trim() == header {
            return Some(at);
        }
    }
    None
}

/// Whitespace, line breaks and comments
fn skip_blank(mut s: &str) -> &str {
    loop {
        s = s.trim_start();
        match s.strip_prefix('#') {
            Some(c) => s = c.split_once('\n').map_or("", |(_, r)| r),
            None => return s,
        }
    }
}

/// A TOML value at the start of `s` and what follows it: integers, floats, bools, basic and literal strings (one
/// line), arrays (over several lines, comments between items); dates, inline tables and multi-line strings are no
/// add-on values
fn knob_value(s: &str) -> std::result::Result<(Knob, &str), String> {
    match s.chars().next() {
        None | Some('\n' | '\r' | '#') => Err("no value".into()),
        Some('{') => Err("an inline table, the add-on's constants take scalars and lists".into()),
        Some('[') => {
            let mut items = Vec::new();
            let mut rest = skip_blank(&s[1..]);
            loop {
                if let Some(r) = rest.strip_prefix(']') {
                    return Ok((Knob::Array(items), r));
                }
                let (v, r) = knob_value(rest)?;
                items.push(v);
                rest = skip_blank(r);
                match rest.strip_prefix(',') {
                    Some(r) => rest = skip_blank(r),
                    None if rest.starts_with(']') => {}
                    None => return Err("an array without its closing ]".into()),
                }
            }
        }
        Some(q @ ('"' | '\'')) => {
            if s.starts_with(&format!("{q}{q}{q}")) {
                return Err("a multi-line string".into());
            }
            let mut out = String::new();
            let mut chars = s.char_indices().skip(1);
            while let Some((at, c)) = chars.next() {
                match c {
                    '\n' => break,
                    c if c == q => return Ok((Knob::Str(out), &s[at + 1..])),
                    '\\' if q == '"' => {
                        let e = chars.next().map(|(_, e)| e);
                        let hex = |n: usize, chars: &mut dyn Iterator<Item = (usize, char)>| {
                            let h: String = chars.take(n).map(|(_, c)| c).collect();
                            u32::from_str_radix(&h, 16).ok().and_then(char::from_u32)
                        };
                        let ch = match e {
                            Some('n') => Some('\n'),
                            Some('t') => Some('\t'),
                            Some('r') => Some('\r'),
                            Some('b') => Some('\u{8}'),
                            Some('f') => Some('\u{c}'),
                            Some('"') => Some('"'),
                            Some('\\') => Some('\\'),
                            Some('u') => hex(4, &mut chars),
                            Some('U') => hex(8, &mut chars),
                            _ => None,
                        };
                        out.push(ch.ok_or("a string with an unknown escape")?);
                    }
                    c => out.push(c),
                }
            }
            Err("a string without its closing quote".into())
        }
        Some(_) => {
            let end = s.find(|c: char| c.is_whitespace() || c == ',' || c == ']' || c == '#').unwrap_or(s.len());
            let (tok, rest) = s.split_at(end);
            Ok((toml_scalar(tok).ok_or_else(|| format!("`{tok}` is no number, bool or string"))?, rest))
        }
    }
}

/// A bare TOML scalar: true/false, an integer (decimal, 0x/0o/0b, `_` between digits), a float (with inf and nan)
fn toml_scalar(tok: &str) -> Option<Knob> {
    match tok {
        "true" => return Some(Knob::Bool(true)),
        "false" => return Some(Knob::Bool(false)),
        _ => {}
    }
    let t = tok.replace('_', "");
    let unsigned = t.strip_prefix(['-', '+']).unwrap_or(&t);
    if let Some((p, r)) = [("0x", 16), ("0o", 8), ("0b", 2)].into_iter().find(|(p, _)| t.starts_with(p)) {
        return i64::from_str_radix(&t[p.len()..], r).ok().map(Knob::Int);
    }
    if matches!(unsigned, "inf" | "nan") {
        return t.parse::<f64>().ok().map(Knob::Float);
    }
    if !unsigned.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    match t.parse::<i64>() {
        Ok(i) => Some(Knob::Int(i)),
        Err(_) => t.parse::<f64>().ok().filter(|_| !unsigned.ends_with('.') && !unsigned.contains(".e")).map(Knob::Float),
    }
}

/// The profiles to choose from: default first, then by name; an own one (in <store>/profiles) hides a bundled one
/// of the same name, an own default is left out (resolve refuses it)
pub fn available(store: &Path, script: &Path, cwd: &Path) -> Vec<Profile> {
    let mut names: Vec<String> = std::iter::once(store.join("profiles"))
        .chain(bundled_dirs(script))
        .filter_map(|d| fs::read_dir(d).ok())
        .flat_map(|rd| rd.flatten().map(|e| e.path()))
        .filter(|d| is_profile(d))
        .filter_map(|d| d.file_name().map(|n| n.to_string_lossy().to_string()))
        .collect();
    names.sort_by_key(|n| (n != BASE, n.clone()));
    names.dedup();
    names.iter().filter_map(|n| resolve(n, store, script, cwd).ok()).collect()
}

impl Profile {
    /// The profile in `dir` for render.py `script` (None: no default beside it)
    pub fn at(dir: &Path, script: &Path) -> Option<Self> {
        let name = dir.file_name()?.to_string_lossy().to_string();
        Some(Self { name, dir: dir.to_path_buf(), base: base_dir(script)?, from: String::new() })
    }

    /// default itself
    pub fn base(script: &Path) -> Option<Self> {
        base_dir(script).map(|base| Self { name: BASE.into(), dir: base.clone(), base, from: "bundled".into() })
    }

    /// default at `dir`, which need not exist: engines in tests that render nothing
    #[cfg(test)]
    pub fn stub(dir: &Path) -> Self {
        Self { name: BASE.into(), dir: dir.to_path_buf(), base: dir.to_path_buf(), from: "bundled".into() }
    }

    /// default, then the profile when it is another one, by their names
    fn folders(&self) -> Vec<(&str, &Path)> {
        let mut v = vec![(BASE, self.base.as_path())];
        if !same_dir(&self.dir, &self.base) {
            v.push((self.name.as_str(), self.dir.as_path()));
        }
        v
    }

    /// Content hash of the profile and default (its hooks are the defaults)
    pub fn hash(&self) -> Option<String> {
        crate::events::content_hash(&self.folders())
    }

    /// Content hash of render.py with the profile: a Blender process runs one of each
    pub fn key(&self, script: &Path) -> Option<String> {
        let mut parts = vec![("render.py", script)];
        parts.extend(self.folders());
        crate::events::content_hash(&parts)
    }

    /// A `[params]` value of the profile, else default's
    #[cfg(test)]
    pub fn param(&self, key: &str) -> Option<String> {
        let of = |dir: &Path| fs::read_to_string(dir.join("profile.toml")).ok().and_then(|t| value(&t, "[params]", key));
        of(&self.dir).or_else(|| of(&self.base))
    }

    /// A top-level value of the profile's own profile.toml (`scene`, 1.0.0's `addon`: not inherited from default)
    fn own(&self, key: &str) -> Option<String> {
        fs::read_to_string(self.dir.join("profile.toml")).ok().and_then(|t| value(&t, "", key)).filter(|v| !v.is_empty())
    }

    /// Its scene template (`scene`), as named in profile.toml
    pub fn scene(&self) -> Option<String> {
        self.own("scene")
    }

    /// Its own copy of the StarBreaker add-on (`[addon] override`, or 1.0.0's top-level `addon`: a folder in the
    /// profile), render.py uses it instead of the installed one
    pub fn addon(&self) -> Option<PathBuf> {
        let text = fs::read_to_string(self.dir.join("profile.toml")).ok()?;
        let copy = addon_table(&text).ok().and_then(|(copy, _)| copy);
        copy.or_else(|| self.own("addon")).filter(|c| !c.is_empty()).map(|c| self.dir.join(c))
    }

    /// Its add-on knobs (`[addon]` without `override`, ADR 0049): constants of the add-on's runtime/constants.py with
    /// their values, in file order; not inherited from default
    pub fn knobs(&self) -> std::result::Result<Knobs, String> {
        let path = self.dir.join("profile.toml");
        let text = fs::read_to_string(&path).unwrap_or_default();
        addon_table(&text).map(|(_, knobs)| knobs).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Its knobs against the add-on the run uses: its own copy, else `installed` (None: not known on this node, say
    /// without Blender; render.py checks them where it runs). Err: a name the add-on does not define or a value of
    /// another type
    pub fn check_knobs(&self, installed: Option<&Path>) -> std::result::Result<(), String> {
        let knobs = self.knobs()?;
        let Some(addon) = self.addon().or_else(|| installed.map(Path::to_path_buf)).filter(|_| !knobs.is_empty()) else { return Ok(()) };
        let file = addon.join("runtime").join("constants.py");
        let py = fs::read_to_string(&file).map_err(|e| format!("{}: [addon] sets add-on constants, but {} is not readable ({e})", self.dir.join("profile.toml").display(), file.display()))?;
        let have = constants(&py);
        let bad: Vec<String> = knobs
            .iter()
            .filter_map(|(name, knob)| match have.get(name) {
                None => Some(format!("{name}: the add-on defines no such constant")),
                Some(c) => check_knob(knob, c).err().map(|why| format!("{name}: {why}")),
            })
            .collect();
        match bad.is_empty() {
            true => Ok(()),
            false => Err(format!("{}, [addon] ({}): {}", self.dir.join("profile.toml").display(), file.display(), bad.join("; "))),
        }
    }

    /// The hash of its add-on copy, taken as the installed add-on's (over its .py files)
    pub fn addon_hash(&self) -> Option<String> {
        self.addon().and_then(|a| crate::events::tree_hash(&a))
    }

    /// The files a render chunk carries: render.py, profiles/default/... and profiles/<name>/... (without
    /// __pycache__); laid out so in the chunk's folder, render.py finds default next to it
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

/// The fields of a run record's origin that say what it renders with (Engine::profile_origin)
pub const ORIGIN_KEYS: [&str; 13] = ["render_py", "render_py_from", "profile", "profile_from", "profile_dir", "profile_hash", "scene", "size", "format", "addon", "addon_from", "starbreaker", "starbreaker_version"];

/// Those fields of a run record's origin
pub fn origin_fields(origin: &serde_json::Value) -> serde_json::Value {
    serde_json::Value::Object(ORIGIN_KEYS.iter().filter_map(|k| origin.get(*k).map(|v| (k.to_string(), v.clone()))).collect())
}

/// A `[params]` value of a profile.toml as text
#[cfg(test)]
pub fn param(toml: &str, key: &str) -> Option<String> {
    value(toml, "[params]", key)
}

/// A value of a profile.toml as text (`key = value  # comment`, a string without its quotes) in `section` ("" for the
/// top level): packshot needs only scene and 1.0.0's addon (and refuses size and format; [addon] is addon_table),
/// render.py reads the whole file
fn value(toml: &str, want: &str, key: &str) -> Option<String> {
    let mut section = "";
    for line in toml.lines().map(str::trim) {
        if line.starts_with('[') {
            section = line.split('#').next().unwrap_or("").trim();
        } else if section == want && let Some(rest) = line.strip_prefix(key).map(str::trim_start).and_then(|r| r.strip_prefix('=')) {
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

    /// A repo-like layout: blender/render.py, profiles/default, profiles/studio, an own profile in the store
    fn layout(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("packshot-profile-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        write(&root.join("blender/render.py"), "# core");
        write(&root.join("profiles/default/profile.toml"), "[params]\nsamples = 16        # Eevee\nquality = 90\n");
        write(&root.join("profiles/default/hooks.py"), "def post(ctx, exr): pass\n");
        write(&root.join("profiles/default/__pycache__/hooks.cpython-313.pyc"), "bytecode");
        write(&root.join("profiles/studio/profile.toml"), "[params]\nquality = 70\n");
        write(&root.join("store/profiles/mine/profile.toml"), "[params]\nsamples = 64\n");
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

    /// Names: an own profile first, then the bundled ones beside render.py; a folder path; default cannot be an own
    /// profile's name; parameters fall back to default's
    #[test]
    fn resolution() {
        let (script, store, root) = layout("resolve");
        let gt = resolve(BASE, &store, &script, &root).unwrap();
        assert!(same_dir(&gt.dir, &root.join("profiles/default")) && same_dir(&gt.base, &gt.dir));
        let studio = resolve("studio", &store, &script, &root).unwrap();
        assert_eq!((studio.name.as_str(), studio.param("quality").as_deref(), studio.param("samples").as_deref()), ("studio", Some("70"), Some("16")));
        let mine = resolve("mine", &store, &script, &root).unwrap();
        assert_eq!(mine.param("samples").as_deref(), Some("64"));
        // an own one hides the bundled one of the same name
        write(&store.join("profiles/studio/profile.toml"), "[params]\nquality = 50\n");
        assert_eq!(resolve("studio", &store, &script, &root).unwrap().param("quality").as_deref(), Some("50"));
        // size and format are the set's (ADR 0045): a profile that sets one is refused
        for (name, toml) in [("big", "[params]\nsize = 1024\n"), ("jpg", "[params]\nformat = \"jpeg\"\n")] {
            write(&store.join("profiles").join(name).join("profile.toml"), toml);
            let err = resolve(name, &store, &script, &root).unwrap_err();
            assert!(err.contains("belong to the set"), "{err}");
        }
        assert_eq!(available(&store, &script, &root).iter().filter(|p| p.name == "big" || p.name == "jpg").count(), 0, "not offered");
        // a folder path, relative to the current folder
        let by_path = resolve("store/profiles/mine", &store, &script, &root).unwrap();
        assert_eq!(by_path.name, "mine");
        let missing = resolve("nope", &store, &script, &root).unwrap_err();
        assert!(missing.starts_with("nope not found in"), "{missing}");
        assert!(resolve("store/profiles/none", &store, &script, &root).unwrap_err().starts_with("no profile.toml in"));
        write(&store.join("profiles/default/profile.toml"), "[params]\n");
        assert!(resolve(BASE, &store, &script, &root).unwrap_err().contains("the name default is reserved"));
        // without default beside render.py there is no profile at all
        assert!(resolve("mine", &store, &root.join("a/b/render.py"), &root).unwrap_err().starts_with("default not found beside"));
        let _ = fs::remove_dir_all(&root);
    }

    /// The name default is reserved (ADR 0048): --profile default is the bundled one; an own profile of that name, in
    /// the store or as a folder, in any case, is refused and not offered
    #[test]
    fn reserved_name() {
        let (script, store, root) = layout("reserved");
        let d = resolve(BASE, &store, &script, &root).unwrap();
        assert_eq!((d.name.as_str(), d.from.as_str()), ("default", "bundled"));
        assert!(same_dir(&d.dir, &root.join("profiles/default")));
        // a folder of that name elsewhere, and a case variant (one folder on Windows in a render chunk)
        write(&root.join("elsewhere/default/profile.toml"), "[params]\nsamples = 4\n");
        write(&root.join("elsewhere/Default/profile.toml"), "[params]\nsamples = 4\n");
        for arg in ["elsewhere/default", "elsewhere/Default"] {
            assert!(resolve(arg, &store, &script, &root).unwrap_err().contains("is reserved"), "{arg}");
        }
        write(&store.join("profiles/DEFAULT/profile.toml"), "[params]\n");
        assert!(resolve("DEFAULT", &store, &script, &root).unwrap_err().contains("is reserved"));
        assert!(available(&store, &script, &root).iter().all(|p| !p.name.eq_ignore_ascii_case(BASE) || p.name == BASE), "the variant is not offered");
        // an own `default` in the store is refused: no start with it, and the list does not offer it
        write(&store.join("profiles/default/profile.toml"), "[params]\nsamples = 4\n");
        assert!(resolve(BASE, &store, &script, &root).unwrap_err().contains("is reserved"));
        let _ = fs::remove_dir_all(&root);
    }

    /// The hash covers default and the profile, not __pycache__; the key render.py too; the payload lays them out as
    /// render.py, profiles/default/..., profiles/<name>/... and hashes the same in a chunk's folder
    #[test]
    fn hash_and_payload() {
        let (script, store, root) = layout("hash");
        let gt = resolve(BASE, &store, &script, &root).unwrap();
        let studio = resolve("studio", &store, &script, &root).unwrap();
        let (h_gt, h_studio, k_studio) = (gt.hash().unwrap(), studio.hash().unwrap(), studio.key(&script).unwrap());
        assert!(h_gt != h_studio && h_studio != k_studio);
        write(&root.join("profiles/default/__pycache__/hooks.cpython-313.pyc"), "other bytecode");
        assert_eq!(gt.hash().unwrap(), h_gt, "__pycache__ is not part of it");
        write(&root.join("profiles/default/hooks.py"), "def post(ctx, exr): return []\n");
        assert_ne!(studio.hash().unwrap(), h_studio, "default's hooks are the defaults: part of every profile's hash");
        let h_studio = studio.hash().unwrap();
        write(&script, "# core 2");
        assert_eq!(studio.hash().unwrap(), h_studio);
        assert_ne!(studio.key(&script).unwrap(), k_studio, "the key covers render.py");

        let (entries, paths) = studio.payload(&script).unwrap();
        let mut names: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["profiles/default/hooks.py", "profiles/default/profile.toml", "profiles/studio/profile.toml", "render.py"]);
        assert!(names.iter().all(|n| crate::net::safe_rel(n).is_some()));
        let (gt_entries, _) = gt.payload(&script).unwrap();
        assert_eq!(gt_entries.len(), 3, "default once");
        // laid out in a chunk's folder as the receiving node does: the same hash and key there
        let chunk = root.join("chunk");
        for (e, p) in entries.iter().zip(&paths) {
            write(&chunk.join(crate::net::safe_rel(&e.path).unwrap()), &fs::read_to_string(p).unwrap());
        }
        let there = Profile::at(&chunk.join("profiles/studio"), &chunk.join("render.py")).unwrap();
        assert!(same_dir(&there.base, &chunk.join("profiles/default")), "render.py finds default next to it");
        assert_eq!((there.hash().unwrap(), there.key(&chunk.join("render.py")).unwrap()), (studio.hash().unwrap(), studio.key(&script).unwrap()));
        let _ = fs::remove_dir_all(&root);
    }

    /// The start dialog's list: default first, own ones hide bundled ones, where each is from; a profile's own
    /// add-on copy and template (top level, not inherited) go into its hash and its payload, its __pycache__ not
    #[test]
    fn listing_addon_and_template() {
        let (script, store, root) = layout("list");
        write(&store.join("profiles/studio/profile.toml"), "scene = \"stage.blend\"  # template\naddon = \"starbreaker_addon\"\n[params]\nquality = 80\n");
        write(&store.join("profiles/studio/stage.blend"), "blend");
        write(&store.join("profiles/studio/starbreaker_addon/__init__.py"), "VERSION = 1\n");
        write(&store.join("profiles/studio/starbreaker_addon/runtime/constants.py"), "GAIN = 1.0\n");
        write(&store.join("profiles/studio/starbreaker_addon/__pycache__/x.pyc"), "bytecode");
        let list = available(&store, &script, &root);
        let names: Vec<(&str, &str)> = list.iter().map(|p| (p.name.as_str(), p.from.as_str())).collect();
        assert_eq!(names, vec![("default", "bundled"), ("mine", "store"), ("studio", "store")]);
        let studio = &list[2];
        assert_eq!((studio.scene().as_deref(), studio.param("quality").as_deref()), (Some("stage.blend"), Some("80")));
        assert_eq!(studio.addon(), Some(store.join("profiles/studio/starbreaker_addon")));
        assert!(list[0].addon().is_none() && list[0].scene().is_none(), "default has neither");
        let (h, a) = (studio.hash().unwrap(), studio.addon_hash().unwrap());
        write(&store.join("profiles/studio/starbreaker_addon/runtime/constants.py"), "GAIN = 2.0\n");
        assert!(studio.hash().unwrap() != h && studio.addon_hash().unwrap() != a, "the add-on copy is part of the profile");
        let (entries, _) = studio.payload(&script).unwrap();
        assert!(entries.iter().any(|e| e.path == "profiles/studio/starbreaker_addon/runtime/constants.py"));
        assert!(entries.iter().all(|e| !e.path.contains("__pycache__")));
        let _ = fs::remove_dir_all(&root);
    }

    /// The [addon] table (ADR 0049): override and the knobs with TOML scalars and arrays (over several lines, comments
    /// between); anything packshot cannot read, another lowercase option, a key twice, override not a string and 1.0.0's
    /// top-level addon next to the table are refused
    #[test]
    fn addon_table_values() {
        let t = "scene = \"s.blend\"\n[params]\nsamples = 8\n[addon]  # the add-on\noverride = \"starbreaker_addon\"\n\
                 POM_SCALE_MULTIPLIER = 60.0   # deeper\nLIGHT_VISUAL_GAIN = 20\nUSE_X = true\nMODE = \"HIGH # not a comment\"\n\
                 LIT = 'C:\\x'\nESC = \"a\\\"b\\u00e9\"\n# a comment line\n\nRADIUS = [\n  1.0,  # red\n  0.35,\n  2e-1,\n]\n\
                 BIG = 1_000\nHEX = 0x10\nNEG = -0.5\nINF = inf\nNESTED = [[1, 2], []]\n[other]\nX = 1\n";
        let (copy, knobs) = addon_table(t).unwrap();
        assert_eq!(copy.as_deref(), Some("starbreaker_addon"));
        let want = vec![
            ("POM_SCALE_MULTIPLIER", Knob::Float(60.0)),
            ("LIGHT_VISUAL_GAIN", Knob::Int(20)),
            ("USE_X", Knob::Bool(true)),
            ("MODE", Knob::Str("HIGH # not a comment".into())),
            ("LIT", Knob::Str("C:\\x".into())),
            ("ESC", Knob::Str("a\"bé".into())),
            ("RADIUS", Knob::Array(vec![Knob::Float(1.0), Knob::Float(0.35), Knob::Float(0.2)])),
            ("BIG", Knob::Int(1000)),
            ("HEX", Knob::Int(16)),
            ("NEG", Knob::Float(-0.5)),
            ("INF", Knob::Float(f64::INFINITY)),
            ("NESTED", Knob::Array(vec![Knob::Array(vec![Knob::Int(1), Knob::Int(2)]), Knob::Array(Vec::new())])),
        ];
        assert_eq!(knobs, want.into_iter().map(|(k, v)| (k.to_string(), v)).collect::<Vec<_>>());
        assert_eq!(addon_table("[params]\nsamples = 8\n").unwrap(), (None, Vec::new()), "no table");
        assert_eq!(addon_table("[addon]\n").unwrap(), (None, Vec::new()), "an empty table");
        for (toml, want) in [
            ("[addon]\nfoo = 1\n", "foo: unknown option"),
            ("[addon]\nPom_Scale = 1\n", "Pom_Scale: unknown option"),
            ("[addon]\noverride = 1\n", "override: a number"),
            ("[addon]\noverride = \"a\"\noverride = \"b\"\n", "override: set twice"),
            ("[addon]\nA = 1\nA = 2\n", "A: set twice"),
            ("addon = \"starbreaker_addon\"\n[addon]\nA = 1\n", "a top-level `addon` and an [addon] table"),
            ("[addon]\nA = {x = 1}\n", "an inline table"),
            ("[addon]\nA = 1979-05-27\n", "is no number"),
            ("[addon]\nA = 1 2\n", "unreadable after the value"),
            ("[addon]\nA = [1, 2\n", "without its closing ]"),
            ("[addon]\nA = \"x\n", "without its closing quote"),
            ("[addon]\nA = \"\"\"x\"\"\"\n", "a multi-line string"),
            ("[addon]\nA =\n", "no value"),
            ("[addon]\nA.B = 1\n", "unreadable line"),
            ("[addon]\nA = yes\n", "`yes` is no number"),
        ] {
            let err = addon_table(toml).unwrap_err();
            assert!(err.contains(want), "{toml:?}: {err}");
        }
    }

    /// A runtime/constants.py as the add-on writes it: top-level upper-case literals by type; expressions, calls,
    /// dicts, nested tuples as Unknown; indented, lowercase and private names, imports and functions are none
    #[test]
    fn constants_py() {
        let py = "\"\"\"Top-level constants.\n\nX = 1 in the docstring\n\"\"\"\nfrom __future__ import annotations\nimport math\n\
                  from mathutils import Matrix\n\nPROP_A = \"starbreaker_a\"  # a property\nPOM_SCALE_MULTIPLIER = 30.0\nPOM_SCALE_MAX = 3\n\
                  BIG = 1_000\nHEX = 0x1F\nNEG = -0.5\nTINY = 1e-3\nFLAG = True\nHASH = \"a # b\"\nSQ = 'it''s'\nGAIN: float = 2.0\n\
                  RADIUS = (1.0, 0.35, 0.2)  # per channel\nPAIR = [1, 2,]\nONE = (4.0)\nSINGLE = (4.0,)\nEMPTY = ()\n\
                  WATT = (4.0 * math.pi) / 683.0\nNAMES = (\"a\", \"b\")\nAXIS = Matrix(\n    (\n        (1.0, 0.0),\n    )\n)\n\
                  AFTER = 5.0\nITEMS = (\n    (\"LOW\", \"Low\"),\n    (\"HIGH\", \"High\"),\n)\nTRI = (\n    1.0,\n    2.0,\n    3.0,\n)\n\
                  _PRIVATE = {\"LOW\": 20.0}\nlower = 1.0\nINF = inf\nCMP == 1\nREDEF = 1.0\nREDEF = \"now a string\"\n\
                  def f(mode: str) -> tuple[int, float]:\n    INSIDE = 1.0\n    return 1, 2.0\n";
        let c = constants(py);
        let want = [
            ("PROP_A", Constant::Str),
            ("POM_SCALE_MULTIPLIER", Constant::Number),
            ("POM_SCALE_MAX", Constant::Number),
            ("BIG", Constant::Number),
            ("HEX", Constant::Number),
            ("NEG", Constant::Number),
            ("TINY", Constant::Number),
            ("FLAG", Constant::Bool),
            ("HASH", Constant::Str),
            ("SQ", Constant::Unknown),
            ("GAIN", Constant::Number),
            ("RADIUS", Constant::Numbers(3)),
            ("PAIR", Constant::Numbers(2)),
            ("ONE", Constant::Number),
            ("SINGLE", Constant::Numbers(1)),
            ("EMPTY", Constant::Numbers(0)),
            ("WATT", Constant::Unknown),
            ("NAMES", Constant::Unknown),
            ("AXIS", Constant::Unknown),
            ("AFTER", Constant::Number),
            ("ITEMS", Constant::Unknown),
            ("TRI", Constant::Numbers(3)),
            ("INF", Constant::Unknown),
            ("REDEF", Constant::Str),
        ];
        for (name, kind) in &want {
            assert_eq!(c.get(*name), Some(kind), "{name}");
        }
        assert_eq!(c.len(), want.len(), "nothing else: {:?}", c.keys().collect::<Vec<_>>());
    }

    /// The type rules: int and float are both numbers, a bool is none; bool for bool, string for string; a list of as
    /// many numbers for a tuple of numbers; anything for a constant packshot cannot read
    #[test]
    fn knob_types() {
        use Constant as C;
        use Knob as K;
        let ok = [(K::Float(1.5), C::Number), (K::Int(2), C::Number), (K::Bool(false), C::Bool), (K::Str("x".into()), C::Str), (K::Array(vec![K::Int(1), K::Float(0.5)]), C::Numbers(2)), (K::Str("x".into()), C::Unknown), (K::Array(Vec::new()), C::Unknown)];
        for (k, c) in &ok {
            assert!(check_knob(k, c).is_ok(), "{k:?} for {c:?}");
        }
        let refused = [
            (K::Bool(true), C::Number, "a bool, the constant is a number"),
            (K::Str("1".into()), C::Number, "a string, the constant is a number"),
            (K::Int(1), C::Bool, "a number, the constant is a bool"),
            (K::Float(1.0), C::Str, "a number, the constant is a string"),
            (K::Array(vec![K::Float(1.0)]), C::Number, "a list of 1, the constant is a number"),
            (K::Float(1.0), C::Numbers(3), "a number, the constant is 3 numbers"),
            (K::Array(vec![K::Float(1.0), K::Float(1.0)]), C::Numbers(3), "a list of 2, the constant is 3 numbers"),
            (K::Array(vec![K::Float(1.0), K::Bool(true)]), C::Numbers(2), "a list of 2, the constant is 2 numbers"),
        ];
        for (k, c, want) in &refused {
            assert_eq!(check_knob(k, c).unwrap_err(), *want, "{k:?} for {c:?}");
        }
    }

    /// A profile's knobs against the add-on it uses: its own copy (`[addon] override`, or 1.0.0's top-level `addon`), else
    /// the installed one; a typo or another type is refused with the profile.toml and every bad name; without an
    /// add-on known here nothing is checked; a table packshot cannot read is refused when the profile is resolved
    #[test]
    fn knob_refusal() {
        let (script, store, root) = layout("knobs");
        let installed = root.join("blender/portable/scripts/addons/starbreaker_addon");
        write(&installed.join("runtime/constants.py"), "POM_SCALE_MULTIPLIER = 30.0\nSKIN_SUBSURFACE_RADIUS = (1.0, 0.35, 0.2)\nAXIS = Matrix(\n  (1.0,),\n)\n");
        let mine = |toml: &str| {
            write(&store.join("profiles/mine/profile.toml"), toml);
            resolve("mine", &store, &script, &root)
        };
        let p = mine("[addon]\nPOM_SCALE_MULTIPLIER = 60\nSKIN_SUBSURFACE_RADIUS = [1.0, 0.5, 0.2]\nAXIS = \"anything\"\n").unwrap();
        assert_eq!(p.knobs().unwrap().len(), 3);
        assert!(p.addon().is_none(), "knobs alone: the installed add-on");
        p.check_knobs(Some(&installed)).unwrap();
        p.check_knobs(None).unwrap();
        let p = mine("[addon]\nPOM_SCALE_MULTIPLER = 60.0\nSKIN_SUBSURFACE_RADIUS = [1.0, 0.5]\nPOM_SCALE_MULTIPLIER = true\n").unwrap();
        let err = p.check_knobs(Some(&installed)).unwrap_err();
        for want in ["profiles", "mine", "profile.toml, [addon]", "constants.py", "POM_SCALE_MULTIPLER: the add-on defines no such constant", "SKIN_SUBSURFACE_RADIUS: a list of 2, the constant is 3 numbers", "POM_SCALE_MULTIPLIER: a bool, the constant is a number"] {
            assert!(err.contains(want), "{want}: {err}");
        }
        assert!(p.check_knobs(None).is_ok(), "no add-on known here: render.py checks");
        assert!(p.check_knobs(Some(&root.join("nowhere"))).unwrap_err().contains("is not readable"));
        // its own copy is the add-on: the installed one does not count
        let copy = store.join("profiles/mine/starbreaker_addon");
        write(&copy.join("__init__.py"), "");
        write(&copy.join("runtime/constants.py"), "NEW_KNOB = 1.0\n");
        let p = mine("[addon]\noverride = \"starbreaker_addon\"\nNEW_KNOB = 2.0\n").unwrap();
        assert_eq!(p.addon(), Some(copy.clone()));
        p.check_knobs(Some(&installed)).unwrap();
        let p = mine("[addon]\noverride = \"starbreaker_addon\"\nPOM_SCALE_MULTIPLIER = 60.0\n").unwrap();
        assert!(p.check_knobs(Some(&installed)).unwrap_err().contains("POM_SCALE_MULTIPLIER: the add-on defines no such constant"));
        // 1.0.0's top-level addon still names the copy, without knobs
        let p = mine("addon = \"starbreaker_addon\"\n[params]\nsamples = 4\n").unwrap();
        assert_eq!((p.addon(), p.knobs().unwrap().len()), (Some(copy), 0));
        p.check_knobs(Some(&installed)).unwrap();
        // what packshot cannot read is refused at once, the profile is not offered
        for toml in ["[addon]\nfoo = 1\n", "[addon]\nA = {x = 1}\n", "addon = \"starbreaker_addon\"\n[addon]\nA = 1\n"] {
            let err = mine(toml).unwrap_err();
            assert!(err.contains("profile.toml: ") && err.contains("[addon]"), "{toml:?}: {err}");
            assert!(available(&store, &script, &root).iter().all(|p| p.name != "mine"));
        }
        let _ = fs::remove_dir_all(&root);
    }
}
