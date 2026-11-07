// The game's entities from the DataCore, for the target picker (ADR 0039): record id, class name, attach type and
// subtype, English name. Four quick StarBreaker queries (`dcb query --ids`, starbreaker-ps 0.3.2-ps.4 or newer) plus
// the English global.ini from the P4k, cached per game build in <work>/dcb/entities-<p4k fingerprint>.json (ADR 0040).
// select() and type_counts() are the picker's rules: ticked types (none = all) narrowed by the search terms, OR'ed;
// several exact class names make exactly that list.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::engine::Target;
use crate::tables::run;

const RECORD: &str = "EntityClassDefinition";
const ATTACH: &str = "EntityClassDefinition.Components[SAttachableComponentParams].AttachDef";
const GLOBAL_INI: &str = "Data/Localization/english/global.ini";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Entity {
    pub uuid: String,
    pub entity: String,
    /// attach type, empty for entities without an AttachDef (e.g. the ore thumbnail entities)
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(rename = "subType", default)]
    pub sub_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Entity {
    /// Armour and clothing render worn, not as their loot crate
    pub fn worn(&self) -> bool {
        self.kind.starts_with("Char_Armor_") || self.kind.starts_with("Char_Clothing_")
    }

    pub fn target(&self) -> Target {
        Target {
            uuids: vec![self.uuid.clone()],
            names: self.name.clone().map(|n| vec![n]),
            entity: self.entity.clone(),
            kind: self.kind.clone(),
            sub_type: if self.sub_type.is_empty() { "UNDEFINED".into() } else { self.sub_type.clone() },
            worn: self.worn(),
        }
    }
}

/// The entity list of this Data.p4k: from the cache, else read from the DataCore (a few seconds) and cached
pub fn load(starbreaker: &Path, p4k: &Path, work: &Path, say: impl Fn(&str)) -> Result<Vec<Entity>> {
    let id = crate::cluster::p4k_id(p4k).ok_or_else(|| anyhow!("build_manifest.id missing or unreadable next to {}", p4k.display()))?;
    let dir = work.join("dcb");
    let file = dir.join(format!("entities-{id}.json"));
    if let Some(list) = fs::read(&file).ok().and_then(|b| serde_json::from_slice::<Vec<Entity>>(&b).ok()) {
        return Ok(list);
    }
    fs::create_dir_all(&dir)?;
    let p4k_arg = p4k.to_string_lossy();
    let query = |field: &str| -> Result<String> {
        let out = run(starbreaker, &["dcb", "query", "--p4k", &p4k_arg, "--ids", field])
            .context("listing entities needs StarBreaker with `dcb query --ids` (starbreaker-ps 0.3.2-ps.4 or newer)")?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    };
    say("DataCore: listing the entities …");
    let all = query(&format!("{RECORD}.Category"))?;
    let types = query(&format!("{ATTACH}.Type"))?;
    let subs = query(&format!("{ATTACH}.SubType"))?;
    let keys = query(&format!("{ATTACH}.Localization.Name"))?;
    say("Localization: reading the English names …");
    let tmp = dir.join("loc");
    let _ = fs::remove_dir_all(&tmp);
    run(starbreaker, &["p4k", "extract", "--p4k", &p4k_arg, "-o", &tmp.to_string_lossy(), "--filter", GLOBAL_INI])?;
    let ini_file = tmp.join(GLOBAL_INI);
    let ini = fs::read(&ini_file).with_context(|| ini_file.display().to_string())?;
    let _ = fs::remove_dir_all(&tmp);
    let list = build(&all, &types, &subs, &keys, &String::from_utf8_lossy(&ini));
    let part = dir.join(format!("entities-{id}.json.part"));
    fs::write(&part, serde_json::to_vec(&list)?)?;
    fs::rename(&part, &file)?;
    // the lists of earlier game builds go
    for e in fs::read_dir(&dir)?.flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        if n.starts_with("entities-") && e.path() != file {
            let _ = fs::remove_file(e.path());
        }
    }
    Ok(list)
}

/// `<record type>.<name>\t<id>\t<value>` lines of `dcb query --ids` as (name, id, value)
fn rows(text: &str) -> impl Iterator<Item = (&str, &str, &str)> {
    text.lines().filter_map(|l| {
        let mut p = l.splitn(3, '\t');
        let name = p.next()?.strip_prefix(RECORD)?.strip_prefix('.')?;
        Some((name, p.next()?, p.next().unwrap_or("").trim_end()))
    })
}

/// The entity list from the query outputs and global.ini, sorted by class name
pub fn build(all: &str, types: &str, subs: &str, keys: &str, ini: &str) -> Vec<Entity> {
    let by_id = |text| rows(text).fold(HashMap::new(), |mut m: HashMap<&str, &str>, (_, id, v)| {
        m.entry(id).or_insert(v);
        m
    });
    let (types, subs, keys) = (by_id(types), by_id(subs), by_id(keys));
    // `key=value`, some keys carry a `,P` suffix; the game treats keys case-insensitively
    let loc: HashMap<String, &str> = ini
        .lines()
        .filter_map(|l| l.trim_start_matches('\u{feff}').split_once('='))
        .map(|(k, v)| (k.split(',').next().unwrap_or(k).trim().to_lowercase(), v.trim_end()))
        .collect();
    let mut seen = HashSet::new();
    let mut list: Vec<Entity> = rows(all)
        .filter(|(_, id, _)| seen.insert(*id))
        .map(|(name, id, _)| Entity {
            uuid: id.to_string(),
            entity: name.to_string(),
            kind: types.get(id).copied().unwrap_or_default().to_string(),
            sub_type: subs.get(id).copied().unwrap_or_default().to_string(),
            name: keys
                .get(id)
                .and_then(|k| k.strip_prefix('@'))
                .and_then(|k| loc.get(&k.to_lowercase()))
                // LOC_PLACEHOLDER / LOC_UNINITIALIZED are "<= PLACEHOLDER =>"-like texts, not names
                .filter(|v| !v.is_empty() && !v.starts_with("<="))
                .map(|v| v.to_string()),
        })
        .collect();
    list.sort_by_key(|e| e.entity.to_lowercase());
    list
}

/// Ticked types in the picker: a whole type, or single subtypes of it
#[derive(Default, Clone, Debug)]
pub struct Ticks {
    pub types: HashSet<String>,
    pub subs: HashSet<(String, String)>,
}

impl Ticks {
    pub fn is_empty(&self) -> bool {
        self.types.is_empty() && self.subs.is_empty()
    }

    fn admit(&self, e: &Entity) -> bool {
        self.is_empty() || self.types.contains(&e.kind) || self.subs.contains(&(e.kind.clone(), e.sub_type.clone()))
    }
}

/// `*` matches any run of characters, case-insensitively
fn glob(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.to_lowercase(), text.to_lowercase());
    let parts: Vec<&str> = p.split('*').collect();
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if parts.len() == 1 {
        return p == t;
    }
    if !t.starts_with(first) || !t[first.len()..].ends_with(last) || t.len() < first.len() + last.len() {
        return false;
    }
    let mut rest = &t[first.len()..t.len() - last.len()];
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(i) => rest = &rest[i + part.len()..],
            None => return false,
        }
    }
    true
}

fn terms(search: &str) -> Vec<&str> {
    search.split(|c: char| c.is_whitespace() || c == ',' || c == ';').filter(|t| !t.is_empty()).collect()
}

/// One search term: a class-name pattern with `*`, else a part of the class name, the display name or the type
fn term_matches(term: &str, e: &Entity) -> bool {
    if term.contains('*') {
        return glob(term, &e.entity);
    }
    let t = term.to_lowercase();
    e.entity.to_lowercase().contains(&t) || e.name.as_deref().is_some_and(|n| n.to_lowercase().contains(&t)) || e.kind.to_lowercase().contains(&t)
}

fn search_matches(terms: &[&str], e: &Entity) -> bool {
    terms.is_empty() || terms.iter().any(|t| term_matches(t, e))
}

/// Several terms that are all exact class names: exactly those entities (a pasted sample list), in the given order
fn exact_list<'a>(list: &'a [Entity], terms: &[&str]) -> Option<Vec<&'a Entity>> {
    if terms.len() < 2 || terms.iter().any(|t| t.contains('*')) {
        return None;
    }
    let by_name: HashMap<String, &Entity> = list.iter().map(|e| (e.entity.to_lowercase(), e)).collect();
    let mut seen = HashSet::new();
    terms.iter().map(|t| by_name.get(&t.to_lowercase()).copied()).collect::<Option<Vec<_>>>().map(|v| v.into_iter().filter(|e| seen.insert(&e.uuid)).collect())
}

/// The picker's result: the ticked types (none ticked = all) narrowed by the search; a pasted list of exact class
/// names is taken as it is
pub fn select<'a>(list: &'a [Entity], ticks: &Ticks, search: &str) -> Vec<&'a Entity> {
    let terms = terms(search);
    if let Some(exact) = exact_list(list, &terms) {
        return exact;
    }
    list.iter().filter(|e| ticks.admit(e) && search_matches(&terms, e)).collect()
}

/// Per type and subtype: how many entities the search matches (the counts in the picker's type tree)
pub fn type_counts(list: &[Entity], search: &str) -> BTreeMap<String, (usize, BTreeMap<String, usize>)> {
    let terms = terms(search);
    let mut out: BTreeMap<String, (usize, BTreeMap<String, usize>)> = BTreeMap::new();
    for e in list.iter().filter(|e| search_matches(&terms, e)) {
        let t = out.entry(e.kind.clone()).or_default();
        t.0 += 1;
        *t.1.entry(e.sub_type.clone()).or_default() += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: &str = "EntityClassDefinition.behr_rifle_ballistic_01\t02d4cd2e-0000-0000-0000-000000000001\tDefault\n\
                       EntityClassDefinition.behr_smg_ballistic_01\t02d4cd2e-0000-0000-0000-000000000002\tDefault\n\
                       EntityClassDefinition.cds_undersuit_01_01_01\t02d4cd2e-0000-0000-0000-000000000003\tDefault\n\
                       EntityClassDefinition.thumbnailentity_resource_rock_gold\t02d4cd2e-0000-0000-0000-000000000004\tDefault\n";
    const TYPES: &str = "EntityClassDefinition.behr_rifle_ballistic_01\t02d4cd2e-0000-0000-0000-000000000001\tWeaponPersonal\n\
                         EntityClassDefinition.behr_smg_ballistic_01\t02d4cd2e-0000-0000-0000-000000000002\tWeaponPersonal\n\
                         EntityClassDefinition.cds_undersuit_01_01_01\t02d4cd2e-0000-0000-0000-000000000003\tChar_Armor_Undersuit\n";
    const SUBS: &str = "EntityClassDefinition.behr_rifle_ballistic_01\t02d4cd2e-0000-0000-0000-000000000001\tMedium\n\
                        EntityClassDefinition.behr_smg_ballistic_01\t02d4cd2e-0000-0000-0000-000000000002\tSmall\n\
                        EntityClassDefinition.cds_undersuit_01_01_01\t02d4cd2e-0000-0000-0000-000000000003\tUNDEFINED\n";
    const KEYS: &str = "EntityClassDefinition.behr_rifle_ballistic_01\t02d4cd2e-0000-0000-0000-000000000001\t@item_NameRifle\n\
                        EntityClassDefinition.behr_smg_ballistic_01\t02d4cd2e-0000-0000-0000-000000000002\t@LOC_PLACEHOLDER\n\
                        EntityClassDefinition.cds_undersuit_01_01_01\t02d4cd2e-0000-0000-0000-000000000003\t@item_Name_Suit\n";
    const INI: &str = "\u{feff}item_namerifle=Test Rifle\nitem_Name_Suit,P=Test Suit\nLOC_PLACEHOLDER=<= PLACEHOLDER =>\n";

    fn list() -> Vec<Entity> {
        build(ALL, TYPES, SUBS, KEYS, INI)
    }

    fn names(v: &[&Entity]) -> Vec<String> {
        v.iter().map(|e| e.entity.clone()).collect()
    }

    #[test]
    fn build_joins_types_subtypes_and_english_names() {
        let l = list();
        assert_eq!(l.len(), 4);
        let rifle = l.iter().find(|e| e.entity == "behr_rifle_ballistic_01").unwrap();
        assert_eq!((rifle.kind.as_str(), rifle.sub_type.as_str(), rifle.name.as_deref()), ("WeaponPersonal", "Medium", Some("Test Rifle")));
        let suit = l.iter().find(|e| e.entity == "cds_undersuit_01_01_01").unwrap();
        assert_eq!(suit.name.as_deref(), Some("Test Suit"));
        assert!(suit.worn() && suit.target().worn);
        let smg = l.iter().find(|e| e.entity == "behr_smg_ballistic_01").unwrap();
        assert_eq!(smg.name, None);
        let gold = l.iter().find(|e| e.entity == "thumbnailentity_resource_rock_gold").unwrap();
        assert_eq!((gold.kind.as_str(), gold.target().sub_type.as_str()), ("", "UNDEFINED"));
    }

    #[test]
    fn glob_matches_stars_anywhere() {
        assert!(glob("behr_*", "BEHR_rifle_ballistic_01"));
        assert!(glob("*ballistic*01", "behr_rifle_ballistic_01"));
        assert!(!glob("behr_*_smg", "behr_rifle_ballistic_01"));
        assert!(glob("*", "x"));
    }

    #[test]
    fn select_takes_ticked_types_narrowed_by_the_search() {
        let l = list();
        assert_eq!(select(&l, &Ticks::default(), "").len(), 4);
        let ticks = Ticks { types: HashSet::from(["WeaponPersonal".to_string()]), ..Default::default() };
        assert_eq!(names(&select(&l, &ticks, "")), ["behr_rifle_ballistic_01", "behr_smg_ballistic_01"]);
        assert_eq!(names(&select(&l, &ticks, "rifle")), ["behr_rifle_ballistic_01"]);
        let sub = Ticks { subs: HashSet::from([("WeaponPersonal".to_string(), "Small".to_string())]), ..Default::default() };
        assert_eq!(names(&select(&l, &sub, "")), ["behr_smg_ballistic_01"]);
        // terms are OR'ed: class-name pattern, display name, type
        assert_eq!(names(&select(&l, &Ticks::default(), "behr_* suit")), ["behr_rifle_ballistic_01", "behr_smg_ballistic_01", "cds_undersuit_01_01_01"]);
        assert_eq!(names(&select(&l, &Ticks::default(), "Test Rifle")).len(), 2, "two terms: Test (both names) or Rifle");
        assert_eq!(names(&select(&l, &Ticks::default(), "char_armor")), ["cds_undersuit_01_01_01"]);
    }

    #[test]
    fn pasted_exact_class_names_are_exactly_that_list() {
        let l = list();
        let ticks = Ticks { types: HashSet::from(["Cooler".to_string()]), ..Default::default() };
        let pasted = "cds_undersuit_01_01_01\nthumbnailentity_resource_rock_gold, cds_undersuit_01_01_01";
        assert_eq!(names(&select(&l, &ticks, pasted)), ["cds_undersuit_01_01_01", "thumbnailentity_resource_rock_gold"]);
        // one unknown name: an ordinary search again
        assert_eq!(select(&l, &Ticks::default(), "cds_undersuit_01_01_01 nope").len(), 1);
    }

    /// The real list: SC_DATA_P4K=<Data.p4k> PACKSHOT_TEST_STARBREAKER=<starbreaker with dcb query --ids>
    /// cargo test --release -- --ignored entities_from_the_real_p4k --nocapture
    #[test]
    #[ignore]
    fn entities_from_the_real_p4k() {
        let p4k = std::path::PathBuf::from(std::env::var("SC_DATA_P4K").expect("SC_DATA_P4K"));
        let sb = std::path::PathBuf::from(std::env::var("PACKSHOT_TEST_STARBREAKER").expect("PACKSHOT_TEST_STARBREAKER"));
        let work = std::env::temp_dir().join(format!("packshot-entities-{}", std::process::id()));
        let t = std::time::Instant::now();
        let l = load(&sb, &p4k, &work, |s| println!("{s}")).unwrap();
        println!("{} entities in {:.1} s", l.len(), t.elapsed().as_secs_f64());
        let t = std::time::Instant::now();
        assert_eq!(load(&sb, &p4k, &work, |_| {}).unwrap(), l);
        println!("cached in {:.2} s", t.elapsed().as_secs_f64());
        let rifle = l.iter().find(|e| e.entity == "behr_rifle_ballistic_01").unwrap();
        println!("{rifle:?}");
        assert_eq!((rifle.kind.as_str(), rifle.sub_type.as_str()), ("WeaponPersonal", "Medium"));
        assert!(rifle.name.is_some());
        let named = l.iter().filter(|e| e.name.is_some()).count();
        let typed = l.iter().filter(|e| !e.kind.is_empty()).count();
        println!("{typed} with a type, {named} with an English name");
        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn type_counts_follow_the_search() {
        let l = list();
        let c = type_counts(&l, "behr_*");
        assert_eq!(c.len(), 1);
        let (n, subs) = &c["WeaponPersonal"];
        assert_eq!((*n, subs["Medium"], subs["Small"]), (2, 1, 1));
    }
}
