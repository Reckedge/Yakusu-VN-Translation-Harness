//! A translation project: one per game folder.
//!
//! Everything lives in the user's data folder, never inside the game, except
//! what the installer copies in (and records for restore).
//!
//!   project.json   game path, engine, text box, settings, progress flags
//!   lines.jsonl    source lines: id, file, idx, name, jp, skip
//!   results.jsonl  translations, append-only; the LAST entry for an id wins
//!   costs.jsonl    one entry per API call
//!   style.md       style sheet (editable)
//!   glossary.json  {"names": {jp: en}, "terms": [{jp, en}]}
//!   work/          engine scratch space (unpacked scripts, JSON, patched output)

use crate::config::{write_json_atomic, Env};
use crate::engines::Match;
use crate::llm::Usage;
use crate::textfit::TextBox;
use indexmap::IndexMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, RwLock};

pub fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

static SLUG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^A-Za-z0-9._-]+").unwrap());

/// Absolute, normalised form of a folder path (no trailing separator).
pub fn abs_path(p: &str) -> PathBuf {
    let p = p.trim().trim_matches('"');
    let path = PathBuf::from(p);
    let abs = if path.is_absolute() { path } else { std::env::current_dir().unwrap_or_default().join(path) };
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

fn norm_case(p: &Path) -> String {
    let s = p.to_string_lossy().to_string();
    if cfg!(windows) {
        s.to_lowercase().replace('/', "\\")
    } else {
        s
    }
}

pub fn same_path(a: &Path, b: &Path) -> bool {
    norm_case(a) == norm_case(b)
}

/// True if `child` is `parent` itself or somewhere inside it.
pub fn path_within(child: &Path, parent: &Path) -> bool {
    let c = norm_case(child);
    let p = norm_case(parent);
    let p = p.trim_end_matches(['/', '\\']);
    c == p || c.starts_with(&format!("{p}{}", std::path::MAIN_SEPARATOR)) || c.starts_with(&format!("{p}/"))
}

pub fn slugify(game_dir: &Path) -> String {
    let base = game_dir.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "game".into());
    let base: String = SLUG_RE.replace_all(&base, "-").trim_matches('-').chars().take(40).collect();
    let h = sha1_smol::Sha1::from(norm_case(game_dir).as_bytes()).digest().to_string();
    format!("{}-{}", if base.is_empty() { "game" } else { &base }, &h[..8])
}

// ---------------------------------------------------------------- data types

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Line {
    pub id: String,
    pub file: String,
    pub idx: usize,
    #[serde(default)]
    pub name: Option<String>,
    pub jp: String,
    #[serde(default)]
    pub skip: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ResultRow {
    pub id: String,
    #[serde(default)]
    pub en_ph: String,
    #[serde(default)]
    pub stage: String,
    #[serde(default)]
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub src: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_review: Option<String>,
    #[serde(default)]
    pub t: f64,
}

impl ResultRow {
    pub fn translated(&self) -> bool {
        !self.en_ph.is_empty()
    }
    pub fn good(&self) -> bool {
        !self.en_ph.is_empty() && self.problems.is_empty()
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Term {
    pub jp: String,
    pub en: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Glossary {
    pub names: IndexMap<String, String>,
    pub terms: Vec<Term>,
}

impl Glossary {
    pub fn name<'a>(&'a self, jp: &'a str) -> &'a str {
        self.names.get(jp).map(String::as_str).filter(|s| !s.is_empty()).unwrap_or(jp)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjSettings {
    /// auto | curly | straight | keep | none
    pub quotes: String,
    pub insert_breaks: bool,
}

impl Default for ProjSettings {
    fn default() -> Self {
        ProjSettings { quotes: "auto".into(), insert_breaks: true }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Stats {
    pub lines: usize,
    pub skipped: usize,
    pub files: usize,
    pub jp_chars: usize,
    /// (speaker, line count), most common first.
    pub speakers: Vec<(String, usize)>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Meta {
    pub game_dir: String,
    pub created: f64,
    pub settings: ProjSettings,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<Match>,
    #[serde(rename = "box", skip_serializing_if = "Option::is_none")]
    pub text_box: Option<TextBox>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<Stats>,
    pub extracted_at: Option<f64>,
    pub style_at: Option<f64>,
    pub built_at: Option<f64>,
    pub built_lines: Option<usize>,
    pub installed_at: Option<f64>,
    pub test_file: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

// ---------------------------------------------------------------- append-only logs

/// Append whole lines to a JSONL log. If a crash left a half-written last
/// line, it is cut off first: otherwise the next record would be glued to
/// the broken bytes and lost on the next load.
pub fn append_lines(path: &Path, data: &[u8]) -> std::io::Result<()> {
    repair_tail(path)?;
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(data)?;
    f.flush()
}

/// Truncate a JSONL file back to its last complete line.
pub fn repair_tail(path: &Path) -> std::io::Result<()> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let len = f.metadata()?.len();
    if len == 0 {
        return Ok(());
    }
    let mut last = [0u8; 1];
    f.seek(SeekFrom::Start(len - 1))?;
    f.read_exact(&mut last)?;
    if last[0] == b'\n' {
        return Ok(());
    }
    // Walk back to the previous newline (in chunks; lines are short).
    let mut end = len;
    let mut keep = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    while end > 0 {
        let start = end.saturating_sub(buf.len() as u64);
        let n = (end - start) as usize;
        f.seek(SeekFrom::Start(start))?;
        f.read_exact(&mut buf[..n])?;
        if let Some(i) = buf[..n].iter().rposition(|b| *b == b'\n') {
            keep = start + i as u64 + 1;
            break;
        }
        end = start;
    }
    f.set_len(keep)?;
    f.sync_all()
}

/// Forget results made for a different version of their line's Japanese
/// (the game was updated, or a script was swapped). They stay in
/// results.jsonl as history but no longer count as done, so the line is
/// translated again and the old English never reaches a build.
fn drop_stale(map: &mut HashMap<String, Arc<ResultRow>>, lines: &[Line]) {
    if lines.is_empty() {
        return;
    }
    let jp: HashMap<&str, &str> = lines.iter().map(|l| (l.id.as_str(), l.jp.as_str())).collect();
    map.retain(|id, r| match (jp.get(id.as_str()), r.src.as_deref()) {
        (Some(now), Some(was)) => *now == was,
        _ => true,
    });
}

// ---------------------------------------------------------------- project

type Spent = (f64, BTreeMap<String, f64>);

pub struct Project {
    pub root: PathBuf,
    meta: Mutex<Meta>,
    lines: RwLock<Option<Arc<Vec<Line>>>>,
    results: RwLock<Option<HashMap<String, Arc<ResultRow>>>>,
    spent: Mutex<Option<Spent>>,
    glossary: Mutex<Option<Glossary>>,
    /// Serialises appends to results.jsonl / costs.jsonl.
    files: Mutex<()>,
}

impl Project {
    pub fn open(root: PathBuf) -> std::io::Result<Project> {
        fs::create_dir_all(&root)?;
        let meta = fs::read_to_string(root.join("project.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Ok(Project {
            root,
            meta: Mutex::new(meta),
            lines: RwLock::new(None),
            results: RwLock::new(None),
            spent: Mutex::new(None),
            glossary: Mutex::new(None),
            files: Mutex::new(()),
        })
    }

    pub fn for_game(env: &Env, game_dir: &Path) -> std::io::Result<Project> {
        let p = Project::open(env.projects_dir().join(slugify(game_dir)))?;
        let fresh = p.meta().game_dir.is_empty();
        if fresh {
            p.update_meta(|m| {
                m.game_dir = game_dir.to_string_lossy().to_string();
                m.created = now();
            })?;
        }
        Ok(p)
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    pub fn work(&self, rel: &str) -> PathBuf {
        let mut p = self.root.join("work");
        for part in rel.split('/').filter(|s| !s.is_empty()) {
            p.push(part);
        }
        p
    }

    pub fn game_dir(&self) -> PathBuf {
        PathBuf::from(&self.meta().game_dir)
    }

    // -- meta ----------------------------------------------------------
    pub fn meta(&self) -> MutexGuard<'_, Meta> {
        self.meta.lock().unwrap()
    }

    pub fn update_meta<R>(&self, f: impl FnOnce(&mut Meta) -> R) -> std::io::Result<R> {
        let mut m = self.meta.lock().unwrap();
        let r = f(&mut m);
        write_json_atomic(&self.path("project.json"), &serde_json::to_value(&*m)?)?;
        Ok(r)
    }

    // -- lines ---------------------------------------------------------
    pub fn set_lines(&self, lines: Vec<Line>) -> std::io::Result<()> {
        let mut guard = self.lines.write().unwrap();
        let mut buf = Vec::with_capacity(lines.len() * 120);
        for l in &lines {
            serde_json::to_writer(&mut buf, l)?;
            buf.push(b'\n');
        }
        let tmp = self.path("lines.jsonl.tmp");
        fs::write(&tmp, buf)?;
        fs::rename(&tmp, self.path("lines.jsonl"))?;
        let lines = Arc::new(lines);
        *guard = Some(lines.clone());
        drop(guard);
        // Results for lines whose Japanese changed no longer apply.
        if let Some(map) = self.results.write().unwrap().as_mut() {
            drop_stale(map, &lines);
        }
        Ok(())
    }

    pub fn lines(&self) -> Arc<Vec<Line>> {
        if let Some(l) = self.lines.read().unwrap().as_ref() {
            return l.clone();
        }
        let mut guard = self.lines.write().unwrap();
        if guard.is_none() {
            let mut out = Vec::new();
            if let Ok(f) = fs::File::open(self.path("lines.jsonl")) {
                for line in BufReader::new(f).lines().map_while(Result::ok) {
                    if let Ok(l) = serde_json::from_str::<Line>(&line) {
                        out.push(l);
                    }
                }
            }
            *guard = Some(Arc::new(out));
        }
        guard.as_ref().unwrap().clone()
    }

    pub fn live_lines(&self) -> Vec<Line> {
        self.lines().iter().filter(|l| l.skip.is_empty()).cloned().collect()
    }

    // -- results -------------------------------------------------------
    fn load_results(&self) {
        if self.results.read().unwrap().is_some() {
            return;
        }
        let lines = self.lines(); // before taking the results lock (lock order: lines, results)
        let mut guard = self.results.write().unwrap();
        if guard.is_some() {
            return;
        }
        let mut map = HashMap::new();
        if let Ok(f) = fs::File::open(self.path("results.jsonl")) {
            for line in BufReader::new(f).lines().map_while(Result::ok) {
                // A half-written last line after a crash is skipped.
                if let Ok(r) = serde_json::from_str::<ResultRow>(&line) {
                    map.insert(r.id.clone(), Arc::new(r));
                }
            }
        }
        drop_stale(&mut map, &lines);
        *guard = Some(map);
    }

    /// Read access to all results (id -> row).
    pub fn with_results<R>(&self, f: impl FnOnce(&HashMap<String, Arc<ResultRow>>) -> R) -> R {
        self.load_results();
        f(self.results.read().unwrap().as_ref().unwrap())
    }

    pub fn result(&self, id: &str) -> Option<Arc<ResultRow>> {
        self.with_results(|r| r.get(id).cloned())
    }

    pub fn results_len(&self) -> usize {
        self.with_results(|r| r.len())
    }

    pub fn add_results(&self, rows: Vec<ResultRow>) -> std::io::Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        self.load_results();
        let _f = self.files.lock().unwrap();
        let mut buf = Vec::new();
        let t = now();
        let rows: Vec<ResultRow> = rows
            .into_iter()
            .map(|mut r| {
                if r.t == 0.0 {
                    r.t = t;
                }
                r
            })
            .collect();
        for r in &rows {
            serde_json::to_writer(&mut buf, r)?;
            buf.push(b'\n');
        }
        append_lines(&self.path("results.jsonl"), &buf)?;
        let mut guard = self.results.write().unwrap();
        let map = guard.as_mut().unwrap();
        for r in rows {
            map.insert(r.id.clone(), Arc::new(r));
        }
        Ok(())
    }

    // -- costs ---------------------------------------------------------
    pub fn log_cost(&self, stage: &str, model: &str, usage: &Usage, usd: f64) -> std::io::Result<()> {
        let (total, mut by_stage) = self.spent();
        {
            let _f = self.files.lock().unwrap();
            let line = json!({"t": now(), "stage": stage, "model": model, "usage": usage, "usd": usd});
            append_lines(&self.path("costs.jsonl"), format!("{line}\n").as_bytes())?;
        }
        *by_stage.entry(stage.to_string()).or_insert(0.0) += usd;
        let mut guard = self.spent.lock().unwrap();
        // Another thread may have logged meanwhile: add to whatever is current.
        match guard.as_mut() {
            Some((t, by)) => {
                *t += usd;
                *by.entry(stage.to_string()).or_insert(0.0) += usd;
            }
            None => *guard = Some((total + usd, by_stage)),
        }
        Ok(())
    }

    pub fn spent(&self) -> Spent {
        let mut guard = self.spent.lock().unwrap();
        if guard.is_none() {
            let mut total = 0.0;
            let mut by: BTreeMap<String, f64> = BTreeMap::new();
            if let Ok(f) = fs::File::open(self.path("costs.jsonl")) {
                for line in BufReader::new(f).lines().map_while(Result::ok) {
                    if let Ok(v) = serde_json::from_str::<Value>(&line) {
                        let usd = v.get("usd").and_then(Value::as_f64).unwrap_or(0.0);
                        total += usd;
                        let stage = v.get("stage").and_then(Value::as_str).unwrap_or("?").to_string();
                        *by.entry(stage).or_insert(0.0) += usd;
                    }
                }
            }
            *guard = Some((total, by));
        }
        guard.clone().unwrap()
    }

    pub fn spent_total(&self) -> f64 {
        self.spent().0
    }

    // -- style / glossary ----------------------------------------------
    pub fn style(&self) -> String {
        fs::read_to_string(self.path("style.md")).unwrap_or_default()
    }

    pub fn set_style(&self, text: &str) -> std::io::Result<()> {
        fs::write(self.path("style.md"), text)
    }

    pub fn glossary(&self) -> Glossary {
        let mut g = self.glossary.lock().unwrap();
        if g.is_none() {
            *g = Some(
                fs::read_to_string(self.path("glossary.json"))
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default(),
            );
        }
        g.clone().unwrap()
    }

    pub fn set_glossary(&self, g: Glossary) -> std::io::Result<()> {
        let mut guard = self.glossary.lock().unwrap();
        write_json_atomic(&self.path("glossary.json"), &serde_json::to_value(&g)?)?;
        *guard = Some(g);
        Ok(())
    }
}
