//! The translation pipeline: extract -> style -> translate -> review -> fit -> build -> install.
//!
//! Everything is resumable: results are appended as they arrive, so stopping
//! (or a crash, or a rate limit) loses at most the batches in flight.

use crate::config::{Env, Settings};
use crate::engines::{self, list_files, read_json_messages, write_json_messages, Ctx, Engine, Installer};
use crate::error::{user, Error, Result};
use crate::llm::{self, ClientFactory, Llm, LlmError};
use crate::project::{now, Line, Project, ResultRow, Stats, Term};
use crate::prompts;
use crate::protect::{check, foreign_codes, has_japanese, invented_codes, protect, restore, Protected};
use crate::textfit::{self, TextBox};
use indexmap::IndexMap;
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

/// Strings that are clearly not dialogue: file names, identifiers, paths.
static NOT_TEXT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^[A-Za-z0-9_\-./\\:*]+$|\.(png|jpg|ogg|wav|mp3|ks|tjs|bmp|tlg|psb|txt|dat)\b").unwrap()
});

/// Skip reason for lines that are only a speaker's name: they get the
/// glossary's English name at build time instead of a translation.
pub const SPEAKER_SKIP: &str = "speaker name (uses the English name from the glossary)";

pub struct Preset {
    pub id: &'static str,
    pub label: &'static str,
    pub translate: &'static str,
    pub review: Option<&'static str>,
    pub blurb: &'static str,
}

pub const QUALITY_PRESETS: &[Preset] = &[
    Preset {
        id: "quick",
        label: "Quick",
        translate: "claude-haiku-5-5",
        review: None,
        blurb: "Haiku translates everything. Cheapest; good enough to follow the story.",
    },
    Preset {
        id: "recommended",
        label: "Recommended",
        translate: "claude-haiku-5-5",
        review: Some("claude-sonnet-5-5"),
        blurb: "Haiku translates, then Sonnet edits every line. Reads like a fan translation.",
    },
    Preset {
        id: "best",
        label: "Best",
        translate: "claude-sonnet-5-5",
        review: Some("claude-opus-5-5"),
        blurb: "Sonnet translates and Opus edits. Highest quality, highest cost.",
    },
];

pub fn presets_json() -> Value {
    let mut m = serde_json::Map::new();
    for p in QUALITY_PRESETS {
        m.insert(
            p.id.into(),
            json!({"label": p.label, "translate": p.translate, "review": p.review, "blurb": p.blurb}),
        );
    }
    Value::Object(m)
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Progress {
    pub stage: String,
    pub done: usize,
    pub total: usize,
    pub message: String,
    pub spent: Option<f64>,
}

/// (translation model, review model if reviewing) for these settings.
pub fn models_for(s: &Settings) -> (String, Option<String>) {
    if s.provider == "anthropic" {
        if let Some(q) = QUALITY_PRESETS.iter().find(|q| q.id == s.preset) {
            return (q.translate.into(), if s.review { q.review.map(String::from) } else { None });
        }
    }
    let tm = s.translate_model.get(&s.provider).cloned().unwrap_or_default();
    let rm = s.review_model.get(&s.provider).cloned().unwrap_or_default();
    (tm, if !rm.is_empty() && s.review { Some(rm) } else { None })
}

pub fn client_for(s: &Settings, factory: &ClientFactory, model: &str) -> Result<Arc<dyn Llm>> {
    let prov = s.provider.as_str();
    let key = s.key();
    if prov != "local" && key.is_empty() {
        return user("Add your API key first.");
    }
    if model.is_empty() {
        return user("Choose a model first.");
    }
    let base = s.base_urls.get(prov).map(String::as_str).filter(|b| !b.is_empty());
    Ok(factory(prov, key, model, base))
}

pub type LogFn = Arc<dyn Fn(&str) + Send + Sync>;
pub type ProgressFn = Arc<dyn Fn(Progress) + Send + Sync>;

#[derive(Clone, Debug, Serialize)]
pub struct Note {
    pub level: &'static str,
    pub text: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct BuildResult {
    pub out: String,
    pub applied: usize,
    /// Translated lines left in Japanese because they failed the final check.
    pub blocked: usize,
    pub manual_output: Option<String>,
}

/// Everything `render` needs, gathered once per stage.
pub struct RenderCtx {
    pub eng: Box<dyn Engine>,
    pub text_box: TextBox,
    pub quotes: String,
    pub insert_breaks: bool,
    pub names: IndexMap<String, String>,
}

fn idx_of(v: &Value) -> Option<usize> {
    match v.get("i")? {
        Value::Number(n) => n.as_u64().map(|n| n as usize).or_else(|| n.as_f64().map(|f| f as usize)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn en_of(v: &Value) -> String {
    match v.get("en") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn jsonl<T: Serialize>(items: &[T]) -> String {
    items.iter().map(|it| serde_json::to_string(it).unwrap()).collect::<Vec<_>>().join("\n")
}

fn chunks(lines: &[Line], n: usize) -> Vec<Vec<Line>> {
    lines.chunks(n.max(1)).map(|c| c.to_vec()).collect()
}

/// Parse a JSON object out of a model reply.
fn parse_object(text: &str) -> Result<Value> {
    let t = text.trim();
    match (t.find('{'), t.rfind('}')) {
        (Some(a), Some(b)) if b > a => serde_json::from_str(&t[a..=b])
            .map_err(|_| Error::User("The model didn't return a style sheet. Try again.".into())),
        _ => user("The model didn't return a style sheet. Try again."),
    }
}

pub struct Pipeline {
    pub env: Env,
    pub p: Arc<Project>,
    pub s: Settings,
    pub log: LogFn,
    pub progress: ProgressFn,
    pub cancel: Arc<AtomicBool>,
    pub factory: ClientFactory,
    /// Worst-case cost of model calls in flight (see `reserve`).
    reserved: Mutex<f64>,
}

impl Pipeline {
    pub fn new(env: Env, p: Arc<Project>, s: Settings, factory: ClientFactory) -> Pipeline {
        Pipeline {
            env,
            p,
            s,
            log: Arc::new(|_| {}),
            progress: Arc::new(|_| {}),
            cancel: Arc::new(AtomicBool::new(false)),
            factory,
            reserved: Mutex::new(0.0),
        }
    }

    fn log(&self, msg: impl AsRef<str>) {
        (self.log)(msg.as_ref())
    }

    fn prog(&self, stage: &str, done: usize, total: usize, message: impl Into<String>) {
        (self.progress)(Progress {
            stage: stage.into(),
            done,
            total,
            message: message.into(),
            spent: Some(self.p.spent_total()),
        })
    }

    fn cx(&self) -> Ctx<'_> {
        Ctx { env: &self.env, project: &self.p, log: &*self.log }
    }

    // ------------------------------------------------------------ engine
    pub fn engine(&self) -> Result<Box<dyn Engine>> {
        let m = self.p.meta().engine.clone();
        match m {
            Some(m) => engines::engine_for(&m),
            None => user("Choose a game folder first."),
        }
    }

    fn save_engine(&self, eng: &dyn Engine) -> Result<()> {
        let details = eng.m().details.clone();
        self.p.update_meta(|m| {
            if let Some(e) = m.engine.as_mut() {
                e.details = details;
            }
        })?;
        Ok(())
    }

    pub fn render_ctx(&self) -> Result<RenderCtx> {
        let eng = self.engine()?;
        let meta = self.p.meta();
        Ok(RenderCtx {
            eng,
            text_box: meta.text_box.clone().unwrap_or_default(),
            quotes: meta.settings.quotes.clone(),
            insert_breaks: meta.settings.insert_breaks,
            names: self.p.glossary().names,
        })
    }

    fn prot(&self, eng: &dyn Engine, l: &Line) -> Arc<Protected> {
        protect(&l.jp, eng.line_break_for(&l.file))
    }

    // ------------------------------------------------------------ extract
    /// Read the game's scripts into the project.
    ///
    /// The work folders from the previous read are set aside first and come
    /// back if anything fails, so a failed re-read (wrong decryption profile,
    /// a patch in the way, a damaged archive) never leaves the project worse
    /// than it was. Lines are replaced only after everything succeeded.
    pub fn extract(&self) -> Result<Stats> {
        const DIRS: &[&str] = &["scripts", "scripts_scn", "json", "unpack"];
        let stash = self.p.work("previous-read");
        if stash.is_dir() {
            // Left by a read that was interrupted: those folders are the last good ones.
            self.unstash(&stash, DIRS)?;
        }
        std::fs::create_dir_all(&stash)?;
        for d in DIRS {
            let p = self.p.work(d);
            if p.exists() {
                std::fs::rename(&p, stash.join(d))?;
            }
        }
        let engine_before = self.p.meta().engine.clone();
        match self.read_scripts() {
            Ok((stats, lines, measured)) => {
                let _ = std::fs::remove_dir_all(&stash);
                self.p.set_lines(lines)?;
                let st = stats.clone();
                self.p.update_meta(move |m| {
                    if !m.text_box.as_ref().map(|b| b.user_set).unwrap_or(false) {
                        m.text_box = Some(measured);
                    }
                    m.stats = Some(st);
                    m.extracted_at = Some(now());
                })?;
                self.log(format!("Found {} lines of text in {} files.", fmt_n(stats.lines), stats.files));
                Ok(stats)
            }
            Err(e) => {
                if let Err(u) = self.unstash(&stash, DIRS) {
                    self.log(format!("Couldn't put the previous work folders back: {u}"));
                }
                let _ = self.p.update_meta(|m| m.engine = engine_before);
                Err(e)
            }
        }
    }

    /// Put work folders set aside by `extract` back in place.
    fn unstash(&self, stash: &std::path::Path, dirs: &[&str]) -> Result<()> {
        for d in dirs {
            let p = self.p.work(d);
            if p.exists() {
                std::fs::remove_dir_all(&p)?;
            }
            let s = stash.join(d);
            if s.exists() {
                std::fs::rename(&s, &p)?;
            }
        }
        std::fs::remove_dir_all(stash)?;
        Ok(())
    }

    fn read_scripts(&self) -> Result<(Stats, Vec<Line>, TextBox)> {
        let mut eng = self.engine()?;
        self.prog("extract", 0, 0, "Reading the game's scripts");
        let json_dir = eng.prepare(&self.cx())?;
        self.save_engine(&*eng)?;
        let mut lines = Vec::new();
        let mut speakers: IndexMap<String, usize> = IndexMap::new();
        let files: Vec<(String, Vec<engines::Entry>)> = list_files(&json_dir, Some(&[".json"]))
            .into_iter()
            .map(|rel| read_json_messages(&engines::join_rel(&json_dir, &rel)).map(|e| (rel, e)))
            .collect::<Result<_>>()?;
        // Every speaker name in the game. In some engines (FVP) a name and a
        // line with the same text are one string in the script, so a line
        // that is just a name must get the glossary's English name, never a
        // separate translation, or the rebuilt name boxes go wrong.
        let names: HashSet<String> = files
            .iter()
            .flat_map(|(_, es)| es.iter())
            .flat_map(|e| e.name.iter().chain(e.names.iter().flatten()))
            .filter(|n| !n.trim().is_empty())
            .map(|n| n.trim().to_string())
            .collect();
        for (rel, entries) in files {
            let lb = eng.line_break_for(&rel);
            for (i, e) in entries.into_iter().enumerate() {
                let jp = e.message;
                let mut name = e.name.filter(|n| !n.is_empty()).or_else(|| e.names.and_then(|n| n.into_iter().next()));
                let pr = protect(&jp, lb);
                if name.is_none() {
                    if let Some((_, n, _)) = &pr.name_prefix {
                        name = Some(n.clone());
                    }
                }
                let skip = if !has_japanese(&pr.text) {
                    "no Japanese"
                } else if NOT_TEXT_RE.is_match(pr.text.trim()) {
                    "looks like a file name"
                } else if names.contains(pr.text.trim()) {
                    SPEAKER_SKIP
                } else {
                    ""
                };
                if let (Some(n), "") = (&name, skip) {
                    *speakers.entry(n.clone()).or_default() += 1;
                }
                lines.push(Line { id: format!("{rel}#{i}"), file: rel.clone(), idx: i, name, jp, skip: skip.into() });
            }
        }
        if lines.iter().all(|l| !l.skip.is_empty()) {
            return user("The scripts were read, but no Japanese text was found in them. If an English patch from another tool is installed, remove it and read the scripts again.");
        }
        let live: Vec<&Line> = lines.iter().filter(|l| l.skip.is_empty()).collect();
        let msgs: Vec<String> = live.iter().map(|l| l.jp.replace(eng.line_break_for(&l.file), "\n")).collect();
        let measured = textfit::measure_box(&msgs);
        let mut sp: Vec<(String, usize)> = speakers.into_iter().collect();
        sp.sort_by_key(|e| std::cmp::Reverse(e.1)); // stable: ties keep first-seen order
        let stats = Stats {
            lines: live.len(),
            skipped: lines.len() - live.len(),
            files: lines.iter().map(|l| &l.file).collect::<HashSet<_>>().len(),
            jp_chars: live.iter().map(|l| self.prot(&*eng, l).text.chars().count()).sum(),
            speakers: sp,
        };
        Ok((stats, lines, measured))
    }

    // ------------------------------------------------------------ helpers
    pub fn live_lines(&self) -> Vec<Line> {
        self.p.live_lines()
    }

    /// (translation model, review model if reviewing).
    pub fn models(&self) -> (String, Option<String>) {
        models_for(&self.s)
    }

    pub fn client(&self, model: &str) -> Result<Arc<dyn Llm>> {
        client_for(&self.s, &self.factory, model)
    }

    fn who(l: &Line, names: &IndexMap<String, String>) -> String {
        match &l.name {
            None => "narration".into(),
            Some(n) => names.get(n).filter(|s| !s.is_empty()).cloned().unwrap_or_else(|| n.clone()),
        }
    }

    /// USD per million tokens (input, output, cache write, cache read): the
    /// built-in Claude prices, else what the user entered for the model.
    /// For a user price the cache rates are taken as the full input rate, so
    /// spending is never undercounted. Local models are free.
    pub fn price(&self, model: &str) -> Option<[f64; 4]> {
        if self.s.provider == "local" {
            return Some([0.0; 4]);
        }
        llm::price_of(model).or_else(|| self.s.prices.get(model).map(|p| [p[0], p[1], p[0], p[0]]))
    }

    /// Reserve the most a call could cost before making it, so parallel
    /// calls together can't push spending past the limit. Returns the amount
    /// reserved; release it with [`Pipeline::release`] once the call is done.
    fn reserve(&self, model: &str, system: &str, user_msg: &str, max_tokens: u32) -> Result<f64> {
        let cap = self.s.budget_usd;
        if cap <= 0.0 {
            return Ok(0.0);
        }
        let Some(p) = self.price(model) else {
            return Err(Error::Budget(format!(
                "Yakusu doesn't know what {model} costs, so it can't keep this run under your ${cap:.2} spending \
                 limit. Enter the model's price in Settings (next to the model name), or clear the spending limit \
                 to run without one."
            )));
        };
        // Generous upper bound: ~3 bytes per input token, every output token used.
        let input_tokens = (system.len() + user_msg.len()) as f64 / 3.0;
        let worst = (input_tokens * p[0].max(p[2]) + max_tokens as f64 * p[1]) / 1e6;
        let mut reserved = self.reserved.lock().unwrap();
        let spent = self.p.spent_total();
        if spent + *reserved + worst > cap {
            return Err(Error::Budget(format!(
                "Stopped at your ${cap:.2} spending limit (spent ${spent:.4}; the next request could cost up to \
                 ${worst:.4}). Raise it in Settings to continue. Progress is kept."
            )));
        }
        *reserved += worst;
        Ok(worst)
    }

    fn release(&self, amount: f64) {
        if amount > 0.0 {
            let mut r = self.reserved.lock().unwrap();
            *r = (*r - amount).max(0.0);
        }
    }

    fn call(&self, client: &dyn Llm, stage: &str, system: &str, user_msg: &str, max_tokens: u32) -> Result<String> {
        if self.cancel.load(Ordering::SeqCst) {
            return Err(Error::Cancelled);
        }
        let model = client.model().to_string();
        let held = self.reserve(&model, system, user_msg, max_tokens)?;
        let result = client.complete(system, user_msg, max_tokens, &self.cancel);
        let cost = |u: &llm::Usage| self.price(&model).map(|p| llm::cost_at(&p, u)).unwrap_or(0.0);
        let out = match result {
            Ok(c) => self.p.log_cost(stage, &model, &c.usage, cost(&c.usage)).map(|_| c.text).map_err(Error::from),
            Err(LlmError::Truncated(u)) => {
                self.p.log_cost(stage, &model, &u, cost(&u)).map_err(Error::from).and(Err(Error::Truncated))
            }
            Err(LlmError::Fatal(_)) if self.cancel.load(Ordering::SeqCst) => Err(Error::Cancelled),
            Err(LlmError::Fatal(m)) => Err(Error::Llm(m)),
        };
        self.release(held);
        out
    }

    /// A model call whose reply should be a JSON array. `Ok(None)` when the
    /// reply was cut off or unreadable.
    fn call_array(
        &self,
        client: &dyn Llm,
        stage: &str,
        system: &str,
        user_msg: &str,
        max_tokens: u32,
    ) -> Result<Option<Vec<Value>>> {
        match self.call(client, stage, system, user_msg, max_tokens) {
            Ok(text) => match llm::parse_json_array(&text) {
                Ok(a) => Ok(Some(a)),
                Err(e) => {
                    self.log(format!("Couldn't read a reply ({e})."));
                    Ok(None)
                }
            },
            Err(Error::Truncated) => {
                self.log("A reply was cut off by the length limit.");
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    // ------------------------------------------------------------ estimate
    /// Rough cost range in USD for each stage, from character counts.
    ///
    /// Calibrated against a 53,000-line VN translated with Haiku + a Sonnet
    /// review (~$31 actual). Shown as a range because output length varies.
    pub fn estimate(&self) -> Value {
        let st = self.p.meta().stats.clone().unwrap_or_default();
        let n = st.lines as f64;
        let chars = st.jp_chars as f64;
        let live = self.live_lines();
        let todo = self
            .p
            .with_results(|r| live.iter().filter(|l| !r.get(&l.id).map(|x| x.translated()).unwrap_or(false)).count());
        let frac = if st.lines > 0 { todo as f64 / n } else { 1.0 };
        let (tm, rm) = self.models();
        let bs = self.s.batch_size.max(1) as f64;
        let ctx = self.s.context_lines as f64;
        let gl = serde_json::to_string(&self.p.glossary()).unwrap_or_default().chars().count();
        let sys_tokens = 700.0 + (self.p.style().chars().count() / 3) as f64 + (gl / 2) as f64;
        let batches = if st.lines > 0 { (n / bs).ceil() } else { 0.0 };
        let tr_in = chars * 1.3 + n * 22.0 + (ctx / bs) * (chars * 1.3 + chars * 0.6 + n * 25.0);
        let tr_out = chars * 0.62 + n * 12.0;
        let cost = |model: &str, tin: f64, tout: f64, sys_reads: f64| -> Option<f64> {
            let p = self.price(model)?;
            Some((tin * p[0] + tout * p[1] + sys_reads * sys_tokens * p[3] + sys_tokens * p[2]) / 1e6)
        };
        let r4 = |x: f64| (x * 1e4).round() / 1e4;
        let range = |c: Option<f64>| c.map(|c| vec![r4(c), r4(c * 1.6)]);
        let translate = range(cost(&tm, tr_in * frac, tr_out * frac, batches * frac));
        let review = match &rm {
            Some(rm) => {
                let rv_in = chars * 1.3 + chars * 0.62 + n * 30.0;
                let rv_out = (chars * 0.62 + n * 12.0) * 0.4;
                range(cost(rm, rv_in, rv_out, (n / 30.0).ceil()))
            }
            None => Some(vec![0.0, 0.0]),
        };
        let total = match (&translate, &review) {
            (Some(t), Some(r)) => Some(vec![r4(t[0] + r[0]), r4(t[1] + r[1])]),
            _ => None,
        };
        json!({"lines": st.lines, "todo": todo, "translate_model": tm, "review_model": rm,
               "translate": translate, "review": review, "total": total, "spent": r4(self.p.spent_total())})
    }

    // ------------------------------------------------------------ style
    pub fn draft_style(&self) -> Result<Value> {
        let live = self.live_lines();
        if live.is_empty() {
            return user("Read the game's scripts first.");
        }
        let (tm, rm) = self.models();
        let client = self.client(rm.as_deref().unwrap_or(&tm))?;
        self.prog("style", 0, 1, "Reading a sample of the script");
        let eng = self.engine()?;
        let mut sample: Vec<&Line> = live.iter().take(150).collect();
        if live.len() > 150 {
            let rest = &live[150..];
            let step = (rest.len() / 250).max(1);
            sample.extend(rest.iter().step_by(step).take(250));
        }
        let speakers = self.p.meta().stats.clone().unwrap_or_default().speakers;
        let mut user_msg = String::from("Speaker names (with line counts):\n");
        user_msg += &speakers.iter().take(80).map(|(n, c)| format!("- {n} ({c})")).collect::<Vec<_>>().join("\n");
        user_msg += "\n\nScript sample:\n";
        user_msg += &sample
            .iter()
            .map(|l| format!("[{}] {}", l.name.as_deref().unwrap_or("narration"), self.prot(&*eng, l).text))
            .collect::<Vec<_>>()
            .join("\n");
        let text = self.call(&*client, "style", prompts::STYLE_SYSTEM, &user_msg, 6000).map_err(|e| match e {
            Error::Truncated => Error::User("The style sheet reply was cut off. Try again.".into()),
            e => e,
        })?;
        let data = parse_object(&text)?;
        let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
        let mut style = s(&data, "style_sheet");
        let summary = s(&data, "summary");
        if !summary.is_empty() {
            style = format!("{summary}\n\n{style}");
        }
        self.p.set_style(&style)?;
        let mut g = self.p.glossary();
        let mut names: IndexMap<String, String> = IndexMap::new();
        for n in data.get("names").and_then(Value::as_array).into_iter().flatten() {
            let (jp, en) = (s(n, "jp"), s(n, "en"));
            if !jp.is_empty() && !en.is_empty() {
                names.insert(jp, en);
            }
        }
        // Names the user already set win.
        for (k, v) in g.names.iter().filter(|(_, v)| !v.is_empty()) {
            names.insert(k.clone(), v.clone());
        }
        g.names = names;
        let mut terms: IndexMap<String, Term> =
            g.terms.iter().filter(|t| !t.jp.is_empty()).map(|t| (t.jp.clone(), t.clone())).collect();
        for t in data.get("terms").and_then(Value::as_array).into_iter().flatten() {
            let (jp, en) = (s(t, "jp"), s(t, "en"));
            if !jp.is_empty() && !en.is_empty() && !terms.contains_key(&jp) {
                terms.insert(jp.clone(), Term { jp, en });
            }
        }
        g.terms = terms.into_values().collect();
        self.p.set_glossary(g.clone())?;
        self.p.update_meta(|m| m.style_at = Some(now()))?;
        self.prog("style", 1, 1, "Style sheet ready");
        Ok(json!({"style": style, "glossary": g}))
    }

    // ------------------------------------------------------------ names
    pub fn missing_names(&self) -> Vec<String> {
        let names = self.p.glossary().names;
        let speakers = self.p.meta().stats.clone().unwrap_or_default().speakers;
        speakers.into_iter().map(|(n, _)| n).filter(|n| names.get(n).map(|e| e.is_empty()).unwrap_or(true)).collect()
    }

    /// Give every speaker an English name before translating, so name boxes
    /// and pronouns stay consistent even if the style step was skipped.
    pub fn ensure_names(&self, client: &dyn Llm) -> Result<()> {
        let missing = self.missing_names();
        if missing.is_empty() {
            return Ok(());
        }
        self.log(format!("Naming {} speakers without an English name.", missing.len()));
        let ask: Vec<String> = missing.iter().take(300).cloned().collect();
        let budget = 40 * ask.len() as u32 + 400;
        let Some(arr) = self.call_array(client, "names", prompts::NAMES_SYSTEM, &ask.join("\n"), budget)? else {
            self.log("Couldn't get English names; speaker names stay in Japanese for now.");
            return Ok(());
        };
        let mut g = self.p.glossary();
        for o in arr {
            let jp = o.get("jp").and_then(Value::as_str).unwrap_or("");
            let en = o.get("en").and_then(Value::as_str).unwrap_or("").trim();
            if missing.iter().any(|m| m == jp)
                && !en.is_empty()
                && g.names.get(jp).map(|e| e.is_empty()).unwrap_or(true)
            {
                g.names.insert(jp.to_string(), en.to_string());
            }
        }
        self.p.set_glossary(g)?;
        Ok(())
    }

    fn row(&self, l: &Line, en: &str, model: &str, stage: &str, problems: Vec<String>) -> ResultRow {
        ResultRow {
            id: l.id.clone(),
            en_ph: en.trim().to_string(),
            stage: stage.into(),
            model: model.into(),
            src: Some(l.jp.clone()),
            problems,
            ..Default::default()
        }
    }

    // ------------------------------------------------------------ translate
    pub fn translate(&self, ids: Option<&[String]>, redo: bool) -> Result<usize> {
        let eng = self.engine()?;
        let (tm, _) = self.models();
        let client = self.client(&tm)?;
        self.ensure_names(&*client)?;
        let g = self.p.glossary();
        let names = g.names.clone();
        let system = prompts::translate_system(&self.p.style(), &g);
        let live = self.live_lines();
        let index: HashMap<&str, usize> = live.iter().enumerate().map(|(k, l)| (l.id.as_str(), k)).collect();
        let want: Option<HashSet<&str>> = ids.map(|v| v.iter().map(String::as_str).collect());
        let todo: Vec<&Line> = self.p.with_results(|done| {
            live.iter()
                .filter(|l| want.as_ref().is_none_or(|w| w.contains(l.id.as_str())))
                .filter(|l| redo || done.get(&l.id).is_none_or(|r| !r.good()))
                .collect()
        });
        if todo.is_empty() {
            self.log("Nothing left to translate in this scope.");
            return Ok(0);
        }
        // Translation memory: a line identical to one already translated (same
        // text, same speaker) reuses that translation. VNs repeat short lines
        // ("……", "Yeah.", choices) a lot, so this saves money and keeps them consistent.
        let key = |l: &Line| (self.prot(&*eng, l).text.clone(), l.name.clone().unwrap_or_default());
        let mut known: HashMap<(String, String), String> = HashMap::new();
        if !redo {
            self.p.with_results(|done| {
                for l in &live {
                    if let Some(r) = done.get(&l.id).filter(|r| r.good()) {
                        known.entry(key(l)).or_insert_with(|| r.en_ph.clone());
                    }
                }
            });
        }
        let mut unique: Vec<Line> = Vec::new();
        let mut first: HashMap<(String, String), String> = HashMap::new(); // key -> id of first occurrence
        let mut reuse_now = Vec::new();
        let mut pending_copies: Vec<(&Line, String)> = Vec::new();
        for l in todo {
            let k = key(l);
            if let Some(en) = known.get(&k) {
                reuse_now.push(self.row(l, en, "memory", "translate", vec![]));
            } else if let Some(src) = first.get(&k) {
                pending_copies.push((l, src.clone()));
            } else {
                first.insert(k, l.id.clone());
                unique.push(l.clone());
            }
        }
        let reused = reuse_now.len();
        if reused > 0 {
            self.p.add_results(reuse_now)?;
            self.log(format!("Reused earlier translations for {} repeated lines.", fmt_n(reused)));
        }
        if unique.is_empty() {
            return Ok(reused);
        }
        let ctx_n = self.s.context_lines;
        let batches = chunks(&unique, self.s.batch_size);
        let total = unique.len();
        let done = AtomicUsize::new(0);
        let msg = format!("Translating with {tm}");
        self.prog("translate", 0, total, &msg);
        self.run_parallel(batches, |batch| {
            let first = index[batch[0].id.as_str()];
            let ctx: Vec<Line> = live[first.saturating_sub(ctx_n)..first].to_vec();
            let rows = self.translate_batch(&*client, &system, &batch, &ctx, &names, &*eng)?;
            self.p.add_results(rows)?;
            let d = done.fetch_add(batch.len(), Ordering::SeqCst) + batch.len();
            self.prog("translate", d, total, &msg);
            Ok(())
        })?;
        let n_copies = pending_copies.len();
        if n_copies > 0 {
            let rows: Vec<ResultRow> = pending_copies
                .into_iter()
                .filter_map(|(l, src)| {
                    self.p
                        .result(&src)
                        .filter(|r| r.good())
                        .map(|r| self.row(l, &r.en_ph, "memory", "translate", vec![]))
                })
                .collect();
            if !rows.is_empty() {
                self.log(format!("Reused translations for {} repeated lines.", fmt_n(rows.len())));
            }
            self.p.add_results(rows)?;
        }
        Ok(total + reused + n_copies)
    }

    /// Translate a batch. Unreadable or cut-off replies split the batch in
    /// half; lines that come back broken are retried, then retried alone.
    fn translate_batch(
        &self,
        client: &dyn Llm,
        system: &str,
        batch: &[Line],
        ctx: &[Line],
        names: &IndexMap<String, String>,
        eng: &dyn Engine,
    ) -> Result<Vec<ResultRow>> {
        let prot: Vec<Arc<Protected>> = batch.iter().map(|l| self.prot(eng, l)).collect();
        let got = match self.ask_translate(client, system, batch, &prot, ctx, names, eng, "")? {
            Some(g) => g,
            None if batch.len() > 1 => {
                let mid = batch.len() / 2;
                self.log(format!("Splitting a batch of {} lines after an unusable reply.", batch.len()));
                let mut first = self.translate_batch(client, system, &batch[..mid], ctx, names, eng)?;
                let mut ctx2: Vec<Line> = ctx.to_vec();
                ctx2.extend_from_slice(&batch[..mid]);
                let keep = ctx.len().max(5);
                let ctx2 = ctx2[ctx2.len().saturating_sub(keep)..].to_vec();
                first.extend(self.translate_batch(client, system, &batch[mid..], &ctx2, names, eng)?);
                return Ok(first);
            }
            None => HashMap::new(),
        };
        let mut rows = Vec::new();
        let mut retry: Vec<(usize, Vec<String>)> = Vec::new();
        for (k, l) in batch.iter().enumerate() {
            let en = got.get(&(k + 1));
            let probs = check(&prot[k], en.map(String::as_str));
            if probs.is_empty() {
                rows.push(self.row(l, en.unwrap(), client.model(), "translate", vec![]));
            } else {
                retry.push((k, probs));
            }
        }
        if retry.len() > 2 {
            // One more try for the broken lines together before going one by one.
            let sub: Vec<Line> = retry.iter().map(|(k, _)| batch[*k].clone()).collect();
            let subp: Vec<Arc<Protected>> = retry.iter().map(|(k, _)| prot[*k].clone()).collect();
            let got2 = self
                .ask_translate(
                    client,
                    system,
                    &sub,
                    &subp,
                    ctx,
                    names,
                    eng,
                    "Some of these lines came back broken last time. Keep every placeholder.",
                )?
                .unwrap_or_default();
            let mut still = Vec::new();
            for (j, (k, _)) in retry.into_iter().enumerate() {
                let en = got2.get(&(j + 1));
                let p2 = check(&prot[k], en.map(String::as_str));
                if p2.is_empty() {
                    rows.push(self.row(&batch[k], en.unwrap(), client.model(), "translate", vec![]));
                } else {
                    still.push((k, p2));
                }
            }
            retry = still;
        }
        for (k, probs) in retry {
            let (en, probs2) = self.translate_single(client, system, &batch[k], &prot[k], names, &probs)?;
            rows.push(self.row(&batch[k], &en, client.model(), "translate", probs2));
        }
        Ok(rows)
    }

    /// One model call. `None` if the reply was unusable.
    #[allow(clippy::too_many_arguments)]
    fn ask_translate(
        &self,
        client: &dyn Llm,
        system: &str,
        batch: &[Line],
        prot: &[Arc<Protected>],
        ctx: &[Line],
        names: &IndexMap<String, String>,
        eng: &dyn Engine,
        note: &str,
    ) -> Result<Option<HashMap<usize, String>>> {
        let context: Vec<Value> = ctx
            .iter()
            .map(|l| {
                let mut c = json!({"who": Self::who(l, names), "jp": self.prot(eng, l).text});
                if let Some(r) = self.p.result(&l.id).filter(|r| r.translated()) {
                    c["en"] = json!(r.en_ph);
                }
                c
            })
            .collect();
        let items: Vec<Value> = batch
            .iter()
            .zip(prot)
            .enumerate()
            .map(|(k, (l, pr))| json!({"i": k + 1, "who": Self::who(l, names), "jp": pr.text}))
            .collect();
        let mut user_msg = String::new();
        if !context.is_empty() {
            user_msg += &format!("CONTEXT (earlier lines, do not translate):\n{}\n\n", jsonl(&context));
        }
        if !note.is_empty() {
            user_msg += &format!("{note}\n\n");
        }
        user_msg += &format!("TRANSLATE:\n{}", jsonl(&items));
        let budget = prot.iter().map(|p| p.text.chars().count()).sum::<usize>() * 3 + 60 * batch.len() + 400;
        let Some(arr) = self.call_array(client, "translate", system, &user_msg, budget.clamp(1500, 16000) as u32)?
        else {
            return Ok(None);
        };
        let got: HashMap<usize, String> = arr.iter().filter_map(|o| Some((idx_of(o)?, en_of(o)))).collect();
        if got.is_empty() && !batch.is_empty() {
            return Ok(None);
        }
        Ok(Some(got))
    }

    fn translate_single(
        &self,
        client: &dyn Llm,
        system: &str,
        l: &Line,
        pr: &Protected,
        names: &IndexMap<String, String>,
        probs: &[String],
    ) -> Result<(String, Vec<String>)> {
        let user_msg = format!(
            "TRANSLATE (the last attempt had problems: {}):\n{}",
            probs.join(", "),
            json!({"i": 1, "who": Self::who(l, names), "jp": pr.text})
        );
        let mut en = String::new();
        for _ in 0..2 {
            let budget = (pr.text.chars().count() * 4 + 200).max(800) as u32;
            en = match self.call_array(client, "translate", system, &user_msg, budget)? {
                Some(arr) => arr.first().map(en_of).unwrap_or_default(),
                None => String::new(),
            };
            if check(pr, Some(&en)).is_empty() {
                return Ok((en.trim().to_string(), vec![]));
            }
        }
        let mut p = check(pr, Some(&en));
        if p.is_empty() {
            p.push("failed".into());
        }
        Ok((en.trim().to_string(), p))
    }

    // ------------------------------------------------------------ review
    pub fn review(&self, ids: Option<&[String]>) -> Result<usize> {
        let (_, rm) = self.models();
        let Some(rm) = rm else {
            self.log("Review is off.");
            return Ok(0);
        };
        let eng = self.engine()?;
        let client = self.client(&rm)?;
        let g = self.p.glossary();
        let names = g.names.clone();
        let system = prompts::review_system(&self.p.style(), &g);
        let want: Option<HashSet<&str>> = ids.map(|v| v.iter().map(String::as_str).collect());
        let live = self.live_lines();
        let todo: Vec<Line> = self.p.with_results(|res| {
            live.iter()
                .filter(|l| want.as_ref().is_none_or(|w| w.contains(l.id.as_str())))
                .filter(|l| res.get(&l.id).is_some_and(|r| r.stage == "translate" && r.translated()))
                .cloned()
                .collect()
        });
        if todo.is_empty() {
            self.log("Nothing to review yet.");
            return Ok(0);
        }
        let total = todo.len();
        let done = AtomicUsize::new(0);
        let changed = AtomicUsize::new(0);
        self.prog("review", 0, total, format!("Reviewing with {rm}"));
        self.run_parallel(chunks(&todo, 30), |batch| {
            let rows = self.review_batch(&*client, &system, &batch, &names, &*eng)?;
            let c = rows.iter().filter(|r| r.stage == "review").count();
            self.p.add_results(rows)?;
            let d = done.fetch_add(batch.len(), Ordering::SeqCst) + batch.len();
            let ch = changed.fetch_add(c, Ordering::SeqCst) + c;
            self.prog("review", d, total, format!("Reviewing with {rm} ({ch} lines improved)"));
            Ok(())
        })?;
        Ok(total)
    }

    fn review_batch(
        &self,
        client: &dyn Llm,
        system: &str,
        batch: &[Line],
        names: &IndexMap<String, String>,
        eng: &dyn Engine,
    ) -> Result<Vec<ResultRow>> {
        let prot: Vec<Arc<Protected>> = batch.iter().map(|l| self.prot(eng, l)).collect();
        let current: Vec<Arc<ResultRow>> = batch.iter().map(|l| self.p.result(&l.id).unwrap()).collect();
        let items: Vec<Value> = batch
            .iter()
            .enumerate()
            .map(|(k, l)| json!({"i": k + 1, "who": Self::who(l, names), "jp": prot[k].text, "en": current[k].en_ph}))
            .collect();
        let budget = current.iter().map(|r| r.en_ph.chars().count()).sum::<usize>() + 40 * batch.len() + 400;
        let Some(arr) = self.call_array(client, "review", system, &jsonl(&items), budget.clamp(1500, 16000) as u32)?
        else {
            if batch.len() > 1 {
                let mid = batch.len() / 2;
                let mut a = self.review_batch(client, system, &batch[..mid], names, eng)?;
                a.extend(self.review_batch(client, system, &batch[mid..], names, eng)?);
                return Ok(a);
            }
            return Ok(vec![]); // left unreviewed; a later review run picks it up
        };
        let changed: HashMap<usize, String> = arr.iter().filter_map(|o| Some((idx_of(o)?, en_of(o)))).collect();
        let mut rows = Vec::new();
        for (k, l) in batch.iter().enumerate() {
            let before = &current[k].en_ph;
            match changed.get(&(k + 1)) {
                Some(en) if en.trim() != before && check(&prot[k], Some(en)).is_empty() => {
                    let mut r = self.row(l, en, client.model(), "review", vec![]);
                    r.before = Some(before.clone());
                    rows.push(r);
                }
                _ => {
                    let mut r = (*current[k]).clone();
                    r.stage = "reviewed".into();
                    r.model_review = Some(client.model().into());
                    r.t = 0.0;
                    rows.push(r);
                }
            }
        }
        Ok(rows)
    }

    // ------------------------------------------------------------ fit
    /// Final message text for one line, plus how many rows it overflows by.
    pub fn render(&self, rc: &RenderCtx, l: &Line, r: &ResultRow) -> (String, usize) {
        let lb = rc.eng.line_break_for(&l.file);
        let pr = protect(&l.jp, lb);
        let straight = rc.eng.straight_quotes();
        let quotes: Option<(&str, &str)> = match rc.quotes.as_str() {
            "keep" => None,
            "none" => Some(("", "")),
            q if q == "curly" || (q == "auto" && !straight) => Some(("\u{201c}", "\u{201d}")),
            _ => Some(("\"", "\"")),
        };
        // A glossary name is written into the script as-is: one carrying
        // engine syntax or control characters falls back to the Japanese.
        let en_name = pr
            .name_prefix
            .as_ref()
            .and_then(|(_, jp, _)| rc.names.get(jp))
            .filter(|n| !n.is_empty() && invented_codes(n).is_empty() && !n.chars().any(char::is_control));
        let text = restore(&pr, &r.en_ph, quotes, en_name.map(String::as_str));
        let text = textfit::normalize_encoding(&text, &rc.eng.encoding(), straight && rc.quotes == "auto");
        textfit::fit(&text, &rc.text_box, lb, rc.insert_breaks)
    }

    pub fn overflow_lines(&self) -> Result<Vec<String>> {
        let rc = self.render_ctx()?;
        let live = self.live_lines();
        Ok(self.p.with_results(|res| {
            live.iter()
                .filter(|l| res.get(&l.id).is_some_and(|r| r.translated() && self.render(&rc, l, r).1 > 0))
                .map(|l| l.id.clone())
                .collect()
        }))
    }

    pub fn shorten(&self, ids: Option<Vec<String>>) -> Result<usize> {
        let rc = self.render_ctx()?;
        let ids = match ids {
            Some(i) if !i.is_empty() => i,
            _ => self.overflow_lines()?,
        };
        if ids.is_empty() {
            self.log("Every line fits the text box.");
            return Ok(0);
        }
        let (tm, rm) = self.models();
        let client = self.client(rm.as_deref().unwrap_or(&tm))?;
        let live = self.live_lines();
        let by_id: HashMap<&str, &Line> = live.iter().map(|l| (l.id.as_str(), l)).collect();
        let todo: Vec<Line> = ids
            .iter()
            .filter_map(|i| by_id.get(i.as_str()))
            .filter(|l| self.p.result(&l.id).is_some())
            .map(|l| (*l).clone())
            .collect();
        let b = &rc.text_box;
        let limit_for = |l: &Line| -> i64 {
            let pr = self.prot(&*rc.eng, l);
            let mut room = (b.width as f64 * b.lines as f64 * 0.92) as i64;
            if !pr.open_quote.is_empty() {
                room -= 2;
            }
            if let Some((_, jp, _)) = &pr.name_prefix {
                room -= rc.names.get(jp).filter(|s| !s.is_empty()).unwrap_or(jp).chars().count() as i64 + 2;
            }
            room.max(10)
        };
        let total = todo.len();
        let done = AtomicUsize::new(0);
        let msg = "Shortening lines that overflow the text box";
        self.prog("fit", 0, total, msg);
        self.run_parallel(chunks(&todo, 30), |batch| {
            let prot: Vec<Arc<Protected>> = batch.iter().map(|l| self.prot(&*rc.eng, l)).collect();
            let cur: Vec<Arc<ResultRow>> = batch.iter().map(|l| self.p.result(&l.id).unwrap()).collect();
            let items: Vec<Value> = batch
                .iter()
                .enumerate()
                .map(|(k, l)| json!({"i": k + 1, "max": limit_for(l), "en": cur[k].en_ph}))
                .collect();
            let arr = self
                .call_array(
                    &*client,
                    "fit",
                    prompts::SHORTEN_SYSTEM,
                    &jsonl(&items),
                    (100 * batch.len()).max(1200) as u32,
                )?
                .unwrap_or_default();
            let mut rows = Vec::new();
            for o in &arr {
                let Some(k) = idx_of(o).and_then(|i| i.checked_sub(1)).filter(|k| *k < batch.len()) else { continue };
                let en = en_of(o);
                if check(&prot[k], Some(&en)).is_empty() {
                    let mut r = self.row(&batch[k], &en, client.model(), "fit", vec![]);
                    r.before = Some(cur[k].en_ph.clone());
                    rows.push(r);
                }
            }
            self.p.add_results(rows)?;
            let d = done.fetch_add(batch.len(), Ordering::SeqCst) + batch.len();
            self.prog("fit", d, total, msg);
            Ok(())
        })?;
        Ok(total)
    }

    // ------------------------------------------------------------ checks
    /// Translated lines whose Japanese uses a glossary name/term but whose
    /// English doesn't use the agreed rendering. {line id: [missing English]}
    pub fn glossary_issues(&self) -> HashMap<String, Vec<String>> {
        let g = self.p.glossary();
        let mut pairs: Vec<(String, String)> = g
            .names
            .iter()
            .filter(|(jp, en)| !en.is_empty() && jp.chars().count() >= 2)
            .map(|(a, b)| (a.clone(), b.clone()))
            .collect();
        pairs.extend(
            g.terms
                .iter()
                .filter(|t| !t.en.is_empty() && t.jp.chars().count() >= 2)
                .map(|t| (t.jp.clone(), t.en.clone())),
        );
        if pairs.is_empty() {
            return HashMap::new();
        }
        let live = self.live_lines();
        self.p.with_results(|res| {
            let mut out = HashMap::new();
            for l in &live {
                let Some(r) = res.get(&l.id).filter(|r| r.translated()) else { continue };
                let en = r.en_ph.to_lowercase();
                let miss: Vec<String> = pairs
                    .iter()
                    .filter(|(jp, word)| {
                        if !l.jp.contains(jp.as_str()) {
                            return false;
                        }
                        let w = word.to_lowercase();
                        let key = w.split_whitespace().next().unwrap_or("").trim_matches(|c| "\"'.,!?".contains(c));
                        !key.is_empty() && !en.contains(key)
                    })
                    .map(|(_, w)| w.clone())
                    .collect();
                if !miss.is_empty() {
                    out.insert(l.id.clone(), miss);
                }
            }
            out
        })
    }

    /// Ask the editor model to fix lines that don't use the glossary's spellings.
    pub fn fix_glossary(&self) -> Result<usize> {
        let issues = self.glossary_issues();
        if issues.is_empty() {
            return Ok(0);
        }
        let eng = self.engine()?;
        let (tm, rm) = self.models();
        let client = self.client(rm.as_deref().unwrap_or(&tm))?;
        let g = self.p.glossary();
        let names = g.names.clone();
        let system = prompts::review_system(&self.p.style(), &g);
        let todo: Vec<Line> = self.live_lines().into_iter().filter(|l| issues.contains_key(&l.id)).collect();
        let total = todo.len();
        let done = AtomicUsize::new(0);
        let msg = "Fixing name and term spellings";
        self.prog("glossary", 0, total, msg);
        self.run_parallel(chunks(&todo, 30), |batch| {
            let prot: Vec<Arc<Protected>> = batch.iter().map(|l| self.prot(&*eng, l)).collect();
            let cur: Vec<Arc<ResultRow>> = batch.iter().map(|l| self.p.result(&l.id).unwrap()).collect();
            let items: Vec<Value> = batch
                .iter()
                .enumerate()
                .map(|(k, l)| {
                    json!({"i": k + 1, "who": Self::who(l, &names), "jp": prot[k].text, "en": cur[k].en_ph,
                           "must_use": issues[&l.id]})
                })
                .collect();
            let user_msg = format!(
                "Each line below must use the English spellings listed in \"must_use\". Rewrite only what is needed to use them.\n{}",
                jsonl(&items)
            );
            let arr = self
                .call_array(&*client, "glossary", &system, &user_msg, (120 * batch.len()).max(1500) as u32)?
                .unwrap_or_default();
            let mut rows = Vec::new();
            for o in &arr {
                let Some(k) = idx_of(o).and_then(|i| i.checked_sub(1)).filter(|k| *k < batch.len()) else { continue };
                let en = en_of(o);
                if !en.trim().is_empty() && check(&prot[k], Some(&en)).is_empty() {
                    let mut r = self.row(&batch[k], &en, client.model(), "review", vec![]);
                    r.before = Some(cur[k].en_ph.clone());
                    rows.push(r);
                }
            }
            self.p.add_results(rows)?;
            let d = done.fetch_add(batch.len(), Ordering::SeqCst) + batch.len();
            self.prog("glossary", d, total, msg);
            Ok(())
        })?;
        Ok(total)
    }

    /// Things worth knowing before a full run.
    pub fn preflight(&self) -> Vec<Note> {
        let mut notes = Vec::new();
        let warn = |t: String| Note { level: "warn", text: t };
        let info = |t: String| Note { level: "info", text: t };
        if self.s.provider != "local" && self.s.key().is_empty() {
            notes.push(warn("Add your API key in step 2 before translating.".into()));
        }
        let (tm, _) = self.models();
        if tm.is_empty() {
            notes.push(warn("Choose a translation model in step 2.".into()));
        }
        let (engine, built) = {
            let m = self.p.meta();
            (m.engine.clone(), m.built_at.is_some())
        };
        if let Some(n) =
            engine.as_ref().and_then(|e| e.details.get("verify_mismatches")).and_then(Value::as_u64).filter(|n| *n > 0)
        {
            notes.push(warn(format!(
                "The last build's check found {n} text record{} that came out differently in the rebuilt script. See Activity for details before playing.",
                if n == 1 { "" } else { "s" }
            )));
        }
        if let Some(e) = engine.filter(|e| e.support == "experimental") {
            notes.push(warn(format!(
                "{} support is experimental. Make a test patch and check it in the game before translating everything.",
                e.name
            )));
        }
        if self.p.style().trim().is_empty() {
            notes.push(warn(
                "No style sheet yet. Drafting one (step 3, a few cents) keeps names, tone and the narrator's voice consistent."
                    .into(),
            ));
        }
        let missing = self.missing_names().len();
        if missing > 0 {
            notes.push(info(format!(
                "{missing} speaker{} no English name yet. Yakusu will name them before translating.",
                if missing == 1 { " has" } else { "s have" }
            )));
        }
        if !built {
            notes.push(info(
                "You haven't made a test patch. It's the cheapest way to check that text shows up correctly in this game.".into(),
            ));
        }
        let est = self.estimate();
        let cap = self.s.budget_usd;
        let (tm, rm) = self.models();
        let unpriced: Vec<String> =
            std::iter::once(tm).chain(rm).filter(|m| !m.is_empty() && self.price(m).is_none()).collect();
        if cap > 0.0 && !unpriced.is_empty() {
            notes.push(warn(format!(
                "Yakusu doesn't know what {} costs, so your ${cap:.2} spending limit can't be enforced and the run \
                 will stop before it starts. Enter the price in Settings, or clear the limit.",
                unpriced.join(" and ")
            )));
        }
        if cap > 0.0 {
            if let (Some(lo), Some(spent)) = (est.pointer("/total/0").and_then(Value::as_f64), est["spent"].as_f64()) {
                if lo + spent > cap {
                    notes.push(warn(format!(
                        "Your ${cap:.2} spending limit is probably lower than this run will cost. Yakusu will stop when it reaches it; progress is kept."
                    )));
                }
            }
        }
        notes
    }

    // ------------------------------------------------------------ build / install
    pub fn build(&self) -> Result<BuildResult> {
        let rc = self.render_ctx()?;
        let src = self.p.work("json");
        let dst = self.p.work("json_tl");
        if dst.is_dir() {
            std::fs::remove_dir_all(&dst)?;
        }
        let all = self.p.lines();
        let by_id: HashMap<&str, &Line> = all.iter().map(|l| (l.id.as_str(), l)).collect();
        let enc = rc.eng.encoding();
        let sq = rc.eng.straight_quotes();
        // A name goes into the script as-is, so it may not carry engine syntax.
        let safe_name = |e: &str| invented_codes(e).is_empty() && !e.chars().any(char::is_control);
        let name_en = |n: &str| -> Option<String> {
            rc.names.get(n).filter(|e| !e.is_empty() && safe_name(e)).map(|e| textfit::normalize_encoding(e, &enc, sq))
        };
        let mut applied = 0usize;
        // Lines whose translation can't be written safely stay in Japanese.
        let mut blocked: Vec<(String, String)> = Vec::new();
        self.prog("build", 0, 0, "Writing the translation into the scripts");
        for rel in list_files(&src, Some(&[".json"])) {
            let mut entries = read_json_messages(&engines::join_rel(&src, &rel))?;
            self.p.with_results(|res| {
                for (i, e) in entries.iter_mut().enumerate() {
                    let lid = format!("{rel}#{i}");
                    let line = by_id.get(lid.as_str()).filter(|l| l.jp == e.message);
                    if let (Some(l), Some(r)) = (line, res.get(&lid)) {
                        if r.translated() && l.skip.is_empty() && r.src.as_deref().unwrap_or(&l.jp) == l.jp {
                            // Check again here, whatever was stored: this is the
                            // last step before the text becomes game script.
                            let lb = rc.eng.line_break_for(&l.file);
                            let pr = protect(&l.jp, lb);
                            let mut why: Vec<String> =
                                check(&pr, Some(&r.en_ph)).into_iter().filter(|x| x != "still has Japanese").collect();
                            let text = self.render(&rc, l, r).0;
                            let foreign = foreign_codes(&pr, &text, lb);
                            if why.is_empty() && !foreign.is_empty() {
                                why.push(format!("would add engine code {}", foreign.join(" ")));
                            }
                            if why.is_empty() {
                                e.message = text;
                                applied += 1;
                            } else {
                                blocked.push((lid.clone(), why.join("; ")));
                            }
                        }
                    }
                    if let Some(l) = line.filter(|l| l.skip == SPEAKER_SKIP) {
                        // The same English as every name box using this string.
                        let lb = rc.eng.line_break_for(&l.file);
                        let pr = protect(&l.jp, lb);
                        if let Some(en) = rc.names.get(pr.text.trim()).filter(|n| !n.is_empty() && safe_name(n)) {
                            let text = restore(&pr, en, None, None);
                            e.message = textfit::normalize_encoding(&text, &enc, sq);
                        }
                    }
                    if let Some(en) = e.name.as_deref().and_then(name_en) {
                        e.name = Some(en);
                    }
                    if let Some(ns) = e.names.as_mut() {
                        for n in ns.iter_mut() {
                            *n = name_en(n).unwrap_or_else(|| textfit::normalize_encoding(n, &enc, sq));
                        }
                    }
                }
            });
            write_json_messages(&engines::join_rel(&dst, &rel), &entries)?;
        }
        if !blocked.is_empty() {
            self.log(format!(
                "Left {} line{} in Japanese because the translation would break the game's codes. Fix them in Proofread:",
                fmt_n(blocked.len()),
                if blocked.len() == 1 { "" } else { "s" }
            ));
            for (id, why) in blocked.iter().take(20) {
                self.log(format!("  {id}: {why}"));
            }
        }
        if applied == 0 {
            if !blocked.is_empty() {
                return user(format!(
                    "None of the translated lines can be written safely ({} have code problems, listed in Activity). \
                     Fix or retranslate them in Proofread, then build again.",
                    fmt_n(blocked.len())
                ));
            }
            return user("No translated lines yet. Translate a few lines first.");
        }
        let mut eng = rc.eng;
        let out = eng.build(&self.cx())?;
        self.save_engine(&*eng)?;
        self.p.update_meta(|m| {
            m.built_at = Some(now());
            m.built_lines = Some(applied);
        })?;
        self.log(format!("Built a patch with {} translated lines.", fmt_n(applied)));
        Ok(BuildResult {
            out: out.to_string_lossy().to_string(),
            applied,
            blocked: blocked.len(),
            manual_output: eng.m().str("manual_output").map(String::from),
        })
    }

    pub fn install(&self) -> Result<Vec<String>> {
        let mut eng = self.engine()?;
        if let Some(manual) = eng.m().str("manual_output") {
            if list_files(&self.p.work("out"), None).is_empty() {
                return user(format!("This engine needs a manual step: the patched files are in {manual}."));
            }
        }
        let files = eng.install(&self.cx())?;
        self.p.update_meta(|m| m.installed_at = Some(now()))?;
        Ok(files)
    }

    pub fn restore(&self) -> Result<()> {
        let mut eng = self.engine()?;
        eng.restore(&self.cx())?;
        self.p.update_meta(|m| m.installed_at = None)?;
        Ok(())
    }

    pub fn installed(&self) -> bool {
        Installer::new(&self.p).installed()
    }

    // ------------------------------------------------------------ runner
    fn run_parallel<F>(&self, batches: Vec<Vec<Line>>, f: F) -> Result<()>
    where
        F: Fn(Vec<Line>) -> Result<()> + Sync,
    {
        let workers = if self.s.provider == "local" { 1 } else { self.s.parallel.max(1) };
        let next = AtomicUsize::new(0);
        let errors: Mutex<Vec<Error>> = Mutex::new(Vec::new());
        let batches: Vec<Mutex<Option<Vec<Line>>>> = batches.into_iter().map(|b| Mutex::new(Some(b))).collect();
        std::thread::scope(|scope| {
            for _ in 0..workers.min(batches.len()) {
                scope.spawn(|| loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    if i >= batches.len() {
                        break;
                    }
                    if self.cancel.load(Ordering::SeqCst) {
                        errors.lock().unwrap().push(Error::Cancelled);
                        break;
                    }
                    let b = batches[i].lock().unwrap().take().unwrap();
                    if let Err(e) = f(b) {
                        match e {
                            Error::Cancelled | Error::Budget(_) | Error::Llm(_) => {
                                self.cancel.store(true, Ordering::SeqCst)
                            }
                            ref other => self.log(format!("Batch failed: {other}")),
                        }
                        errors.lock().unwrap().push(e);
                    }
                });
            }
        });
        let mut errors = errors.into_inner().unwrap();
        // Report the root cause, not the cancellations it triggered.
        if let Some(i) = errors.iter().position(|e| matches!(e, Error::Budget(_) | Error::Llm(_))) {
            return Err(errors.swap_remove(i));
        }
        if let Some(i) = errors.iter().position(|e| !matches!(e, Error::Cancelled)) {
            return Err(errors.swap_remove(i));
        }
        match errors.pop() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    pub fn work_dir(&self) -> PathBuf {
        self.p.work("")
    }
}

/// 12345 -> "12,345"
pub fn fmt_n(n: usize) -> String {
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
