// Pose and light tables of the inventory from the DataCore (`PlayerChoice_PITConfig.inventoryParams`,
// `inventorylightgroup_*`), cached in the working directory under <work>/dcb (port of loadGameTables from the
// former src/icons/cli.ts, checked identical for all targets and rigs).
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

const DEFAULT_RIG: &str = "inventorylightgroup_other";
const LIGHT_GROUPS_PATH: &str = "libs/foundry/records/entities/lights/scene/inventory";

#[derive(Default)]
pub struct Tables {
    base: [f64; 3],
    offsets: Vec<(Vec<Category>, [f64; 3])>,
    groups: Vec<(Vec<Category>, String)>,
    default_group: String,
    rigs: HashMap<String, PathBuf>,
}

struct Category {
    kind: String,
    sub_type: String,
    tagged: bool,
}

fn run(bin: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new(bin).args(args).output().with_context(|| format!("{} does not start", bin.display()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(if out.stderr.is_empty() { &out.stdout } else { &out.stderr }).trim().to_string();
        bail!("{} {} failed: {}", bin.display(), args.join(" "), &err[err.len().saturating_sub(2000)..]);
    }
    Ok(out.stdout)
}

fn vec3(v: &Value) -> [f64; 3] {
    let f = |k: &str| v.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    [f("x"), f("y"), f("z")]
}

/// Truthiness as in JS: requiredTag is null, "" or a reference
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(_) => true,
    }
}

fn categories(v: &Value) -> Vec<Category> {
    v.get("itemCategories")
        .and_then(Value::as_array)
        .map(|cs| {
            cs.iter()
                .map(|c| Category {
                    kind: c.get("type").and_then(Value::as_str).unwrap_or_default().to_string(),
                    sub_type: c.get("subType").and_then(Value::as_str).unwrap_or_default().to_string(),
                    tagged: truthy(c.get("requiredTag")),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Best entry for type/subtype: exact before type+UNDEFINED; entries with a tag condition do not count,
/// on a tie the first wins.
fn best<'a, T>(entries: &'a [(Vec<Category>, T)], kind: &str, sub_type: &str) -> Option<&'a T> {
    let mut found = None;
    let mut score = 0;
    for (cats, value) in entries {
        for c in cats {
            if c.tagged || c.kind != kind {
                continue;
            }
            let s = if c.sub_type == sub_type { 2 } else if c.sub_type == "UNDEFINED" { 1 } else { 0 };
            if s > score {
                score = s;
                found = Some(value);
            }
        }
    }
    found
}

fn group_name(reference: &str) -> String {
    let name = reference.rsplit(['/', '\\']).next().unwrap_or(reference);
    name.strip_suffix(".json").unwrap_or(name).to_lowercase()
}

/// A light record as compact rig JSON for render.py
fn rig_json(record: &Value) -> Value {
    let comp = record
        .pointer("/_RecordValue_/Components")
        .and_then(Value::as_array)
        .and_then(|cs| cs.iter().find(|c| c.get("_Type_").and_then(Value::as_str) == Some("SLightGroupComponentParams")));
    let lights: Vec<Value> = comp
        .and_then(|c| c.get("lightItems"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|li| {
                    let lp = &li["lightParams"];
                    let off = li.pointer("/attachmentImplementation/offset");
                    let color = &lp["defaultState"]["color"];
                    json!({
                        "name": li["name"].as_str().unwrap_or_default().trim_start_matches("SLightItem_"),
                        "type": lp["lightType"],
                        "active": lp["active"],
                        "position": off.map(|o| vec3(&o["Position"])),
                        "rotation": off.map(|o| vec3(&o["Rotation"])),
                        "color": [color["r"], color["g"], color["b"]],
                        "intensity": lp["defaultState"]["intensity"],
                        "radius": lp.pointer("/sizeParams/lightRadius"),
                        "bulb": lp.pointer("/sizeParams/bulbRadius"),
                        "fov": lp.pointer("/projectorParams/FOV"),
                        "shadows": lp.pointer("/shadowParams/shadowCasting").and_then(Value::as_bool).unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    json!({ "record": record["_RecordName_"], "lights": lights })
}

impl Tables {
    pub fn load(starbreaker: &Path, p4k: &Path, work: &Path, refresh: bool, say: impl Fn(&str)) -> Result<Self> {
        let dir = work.join("dcb");
        let config_file = dir.join("pitconfig.json");
        let groups_dir = dir.join("lightgroups");
        fs::create_dir_all(dir.join("rigs"))?;
        let p4k_arg = p4k.to_string_lossy();
        if refresh || !config_file.exists() {
            say("DataCore: reading inventory tables (PlayerChoice_PITConfig) …");
            let out = run(starbreaker, &["dcb", "query", "--p4k", &p4k_arg, "PlayerChoice_PITConfig"])?;
            fs::write(&config_file, out)?;
        }
        if refresh || !groups_dir.exists() {
            say("DataCore: reading light rigs (inventorylightgroup_*) …");
            let filter = format!("{LIGHT_GROUPS_PATH}/**");
            let groups_arg = groups_dir.to_string_lossy();
            run(starbreaker, &["dcb", "extract", "--p4k", &p4k_arg, "--format", "json", "-o", &groups_arg, "--filter", &filter])?;
        }
        let config: Value = serde_json::from_slice(&fs::read(&config_file)?).context("pitconfig.json")?;
        let params = config.pointer("/_RecordValue_/inventoryParams").ok_or_else(|| anyhow!("inventoryParams missing in {}", config_file.display()))?;
        let list = |key: &str| params.get(key).and_then(Value::as_array).cloned().unwrap_or_default();

        let mut rigs = HashMap::new();
        let records_dir = groups_dir.join(LIGHT_GROUPS_PATH);
        for entry in fs::read_dir(&records_dir).with_context(|| records_dir.display().to_string())? {
            let path = entry?.path();
            let Some(file) = path.file_name().and_then(|f| f.to_str()) else { continue };
            let name = file.strip_suffix(".json").unwrap_or(file).to_lowercase();
            let record: Value = serde_json::from_slice(&fs::read(&path)?).with_context(|| path.display().to_string())?;
            let rig_file = dir.join("rigs").join(format!("{name}.json"));
            fs::write(&rig_file, serde_json::to_string_pretty(&rig_json(&record))?)?;
            rigs.insert(name, rig_file);
        }

        Ok(Self {
            base: vec3(&params["defaultOrientation"]),
            offsets: list("listOfItemsOrientationOffset").iter().map(|e| (categories(e), vec3(&e["orientationOffset"]))).collect(),
            groups: list("listOfItemLightGroups")
                .iter()
                .map(|e| (categories(e), e["lightGroupEntityClass"].as_str().unwrap_or_default().to_string()))
                .collect(),
            default_group: params["defaultLightGroupEntityClass"].as_str().unwrap_or_default().to_string(),
            rigs,
        })
    }

    pub fn pose(&self, kind: &str, sub_type: &str) -> [f64; 3] {
        let o = best(&self.offsets, kind, sub_type).copied().unwrap_or([0.0; 3]);
        [self.base[0] + o[0], self.base[1] + o[1], self.base[2] + o[2]]
    }

    /// Tables with one rig, the default one (tests with fake tools)
    #[cfg(test)]
    pub fn with_rig(rig: PathBuf) -> Self {
        Self { rigs: HashMap::from([(DEFAULT_RIG.to_string(), rig)]), ..Default::default() }
    }

    pub fn rig(&self, kind: &str, sub_type: &str) -> Result<&PathBuf> {
        let reference = best(&self.groups, kind, sub_type).unwrap_or(&self.default_group);
        let name = group_name(reference);
        self.rigs.get(&name).or_else(|| self.rigs.get(DEFAULT_RIG)).ok_or_else(|| anyhow!("light rig {name} not found"))
    }
}
