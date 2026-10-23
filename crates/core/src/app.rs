//! The app behind the window: one `call(command, args)` entry point that the
//! desktop shell (Tauri) and the browser dev server both forward to.

use crate::config::{Env, Settings};
use crate::engines::{self, kirikiri::XP3_GAMES, manual, Installer, Match};
use crate::error::{user, Error, Result};
use crate::llm::{self, ClientFactory};
use crate::pipeline::{self, Pipeline, Progress, QUALITY_PRESETS};
use crate::project::{abs_path, now, same_path, slugify, Line, Project, ResultRow};
use crate::protect::{check, protect, CODE_RE};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Default, Serialize)]
pub struct Job {
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    pub done: usize,
    pub total: usize,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage_started: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub stopped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LogLine {
    pub t: f64,
    pub m: String,
}

struct Inner {
    settings: Settings,
    project: Option<Arc<Project>>,
    matches: Vec<Match>,
}

pub type Opener = Box<dyn Fn(&Path) -> std::io::Result<()> + Send + Sync>;

pub struct App {
    pub env: Env,
    factory: ClientFactory,
    inner: Mutex<Inner>,
    logs: Arc<Mutex<VecDeque<LogLine>>>,
    job: Arc<Mutex<Job>>,
    cancel: Arc<AtomicBool>,
    /// Opens a folder or file with the system (replaceable in tests).
    pub opener: Opener,
}

fn system_open(path: &Path) -> std::io::Result<()> {
    let mut cmd = if cfg!(windows) {
        let mut c = std::process::Command::new("explorer");
        c.arg(path);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg(path);
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(path);
        c
    };
    cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().map(|_| ())
}

fn s_arg<'a>(args: &'a Value, k: &str) -> &'a str {
    args.get(k).and_then(Value::as_str).unwrap_or("")
}

fn r4(x: f64) -> f64 {
    (x * 1e4).round() / 1e4
}

impl App {
    pub fn new(env: Env, factory: ClientFactory) -> Arc<App> {
        App::with_opener(env, factory, Box::new(system_open))
    }

    /// Like `new`, with a custom opener.
    pub fn with_opener(env: Env, factory: ClientFactory, opener: Opener) -> Arc<App> {
        let settings = env.load_settings();
        let last = settings.last_game.clone();
        let app = Arc::new(App {
            env,
            factory,
            inner: Mutex::new(Inner { settings, project: None, matches: vec![] }),
            logs: Arc::new(Mutex::new(VecDeque::new())),
            job: Arc::new(Mutex::new(Job::default())),
            cancel: Arc::new(AtomicBool::new(false)),
            opener,
        });
        if let Some(last) = last.filter(|l| Path::new(l).is_dir()) {
            let _ = app.open_game(&last, true);
        }
        app
    }

    /// The single entry point for the interface. Errors are user-facing text.
    pub fn call(self: &Arc<Self>, cmd: &str, args: &Value) -> std::result::Result<Value, String> {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.dispatch(cmd, args)));
        match r {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(e.to_string()),
            Err(p) => {
                let msg =
                    p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()));
                let msg = format!("Unexpected error: {}", msg.unwrap_or_else(|| "internal error".into()));
                self.log(&msg);
                Err(msg)
            }
        }
    }

    fn dispatch(self: &Arc<Self>, cmd: &str, a: &Value) -> Result<Value> {
        match cmd {
            "state" => Ok(self.state()),
            "estimate" => self.estimate(a.get("all").and_then(Value::as_bool).unwrap_or(false)),
            "get_style" => Ok(match self.project() {
                Some(p) => json!({"style": p.style(), "glossary": p.glossary()}),
                None => json!({}),
            }),
            "preview" => self.preview(6),
            "lines" => self.lines(
                s_arg(a, "q"),
                a.get("filter").and_then(Value::as_str).unwrap_or("all"),
                a.get("page").and_then(Value::as_u64).unwrap_or(0) as usize,
                a.get("per").and_then(Value::as_u64).unwrap_or(60).clamp(1, 500) as usize,
            ),
            "preflight" => self.preflight(),
            "xp3_games" => {
                let q = s_arg(a, "q").to_lowercase();
                let games: Vec<_> = XP3_GAMES
                    .iter()
                    .filter(|g| g.name.to_lowercase().contains(&q) || g.title.to_lowercase().contains(&q))
                    .take(60)
                    .collect();
                Ok(json!({"games": games}))
            }
            "open" => self.open_game(s_arg(a, "game_dir"), false),
            "engine" => self.set_engine(a),
            "settings" => self.save_settings(a),
            "test_key" => self.test_key(),
            "box" => self.set_box(a),
            "save_style" => self.save_style(a),
            "line" => self.edit_line(a),
            "run" => self.run_action(s_arg(a, "action")),
            "cancel" => {
                self.cancel.store(true, Ordering::SeqCst);
                Ok(json!({"ok": true}))
            }
            "launch" => self.launch(),
            "reveal" => self.reveal(a.get("which").and_then(Value::as_str).unwrap_or("game")),
            "retranslate" => {
                let ids: Vec<String> = a
                    .get("ids")
                    .and_then(Value::as_array)
                    .map(|v| v.iter().filter_map(|x| x.as_str()).filter(|x| !x.is_empty()).map(String::from).collect())
                    .unwrap_or_default();
                self.retranslate(ids)
            }
            "export_csv" => self.export_csv(),
            "import_csv" => self.import_csv(s_arg(a, "text")),
            "forget" => self.forget(s_arg(a, "game_dir")),
            "recent" => Ok(json!(self.recent(8))),
            _ => user(format!("Unknown command {cmd}")),
        }
    }

    // ------------------------------------------------------------ logging / jobs
    pub fn log(&self, msg: &str) {
        let mut l = self.logs.lock().unwrap();
        l.push_back(LogLine { t: now(), m: msg.to_string() });
        while l.len() > 600 {
            l.pop_front();
        }
    }

    pub fn logs(&self) -> Vec<LogLine> {
        self.logs.lock().unwrap().iter().cloned().collect()
    }

    pub fn job(&self) -> Job {
        self.job.lock().unwrap().clone()
    }

    pub fn project(&self) -> Option<Arc<Project>> {
        self.inner.lock().unwrap().project.clone()
    }

    pub fn settings(&self) -> Settings {
        self.inner.lock().unwrap().settings.clone()
    }

    fn need_project(&self) -> Result<Arc<Project>> {
        self.project().ok_or_else(|| Error::User("Choose a game folder first.".into()))
    }

    /// A pipeline over the open project with a snapshot of the settings:
    /// changing them mid-run doesn't affect a running job.
    pub fn pipeline(&self) -> Result<Pipeline> {
        let (p, s) = {
            let i = self.inner.lock().unwrap();
            (i.project.clone(), i.settings.clone())
        };
        let p = p.ok_or_else(|| Error::User("Choose a game folder first.".into()))?;
        let mut pipe = Pipeline::new(self.env.clone(), p, s, self.factory.clone());
        let logs = self.logs.clone();
        pipe.log = Arc::new(move |m: &str| {
            let mut l = logs.lock().unwrap();
            l.push_back(LogLine { t: now(), m: m.to_string() });
            while l.len() > 600 {
                l.pop_front();
            }
        });
        let job = self.job.clone();
        pipe.progress = Arc::new(move |pr: Progress| {
            let mut j = job.lock().unwrap();
            if j.stage.as_deref() != Some(pr.stage.as_str()) || j.stage_started.is_none() {
                j.stage_started = Some(now());
            }
            j.stage = Some(pr.stage);
            j.done = pr.done;
            j.total = pr.total;
            j.message = pr.message;
            if pr.spent.is_some() {
                j.spent = pr.spent;
            }
        });
        pipe.cancel = self.cancel.clone();
        Ok(pipe)
    }

    /// Run `f` as the background job. Only one job runs at a time.
    pub fn start(self: &Arc<Self>, action: &str, f: impl FnOnce() -> Result<Value> + Send + 'static) -> Result<Value> {
        {
            let mut j = self.job.lock().unwrap();
            if j.running {
                return user("Something is already running. Wait for it or press Stop.");
            }
            // The same flag the pipeline holds, so Stop reaches it.
            self.cancel.store(false, Ordering::SeqCst);
            *j = Job {
                running: true,
                action: Some(action.into()),
                stage: Some(action.into()),
                started: Some(now()),
                stage_started: Some(now()),
                ..Default::default()
            };
        }
        let app = self.clone();
        std::thread::spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
            let mut j = app.job.lock().unwrap();
            j.running = false;
            j.finished = Some(now());
            match r {
                Ok(Ok(v)) => j.result = Some(v),
                Ok(Err(Error::Cancelled)) => {
                    j.stopped = true;
                    drop(j);
                    app.log("Stopped. Everything translated so far is saved.");
                }
                Ok(Err(e)) => {
                    let msg = e.to_string();
                    j.error = Some(msg.clone());
                    drop(j);
                    app.log(&format!("Error: {msg}"));
                }
                Err(p) => {
                    let msg =
                        p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()));
                    let msg = format!("Unexpected error: {}", msg.unwrap_or_default());
                    j.error = Some(msg.clone());
                    drop(j);
                    app.log(&msg);
                }
            }
        });
        Ok(json!({"started": action}))
    }

    // ------------------------------------------------------------ state
    pub fn state(&self) -> Value {
        let (settings, matches, project) = {
            let i = self.inner.lock().unwrap();
            (i.settings.clone(), i.matches.clone(), i.project.clone())
        };
        let keys: Map<String, Value> = settings
            .keys
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, v)| {
                let n = v.chars().count();
                let masked = if n > 14 {
                    let head: String = v.chars().take(7).collect();
                    let tail: String = v.chars().skip(n - 4).collect();
                    format!("{head}…{tail}")
                } else {
                    "••••".into()
                };
                (k.clone(), Value::from(masked))
            })
            .collect();
        let mut sv = serde_json::to_value(&settings).unwrap_or(json!({}));
        sv["keys"] = Value::Object(keys);
        let proj = project.map(|p| self.project_state(&p));
        let logs = self.logs();
        json!({
            "settings": sv,
            "presets": pipeline::presets_json(),
            "providers": llm::providers_json(),
            "prices": llm::prices_json(),
            "matches": matches,
            "project": proj,
            "job": self.job(),
            "logs": logs[logs.len().saturating_sub(200)..],
            "platform": std::env::consts::OS,
            "recent": self.recent(8),
            "version": env!("CARGO_PKG_VERSION"),
        })
    }

    fn project_state(&self, p: &Project) -> Value {
        let lines = p.lines();
        let live: Vec<&Line> = lines.iter().filter(|l| l.skip.is_empty()).collect();
        let (done, reviewed, problems) = p.with_results(|res| {
            let mut d = (0, 0, 0);
            for l in &live {
                if let Some(r) = res.get(&l.id) {
                    if r.translated() {
                        d.0 += 1;
                    }
                    if matches!(r.stage.as_str(), "review" | "reviewed" | "manual" | "fit") {
                        d.1 += 1;
                    }
                    if !r.problems.is_empty() {
                        d.2 += 1;
                    }
                }
            }
            d
        });
        let inst = Installer::new(p);
        let (spent, by_stage) = p.spent();
        let files: BTreeSet<&str> = live.iter().map(|l| l.file.as_str()).collect();
        let m = p.meta().clone();
        let game_dir = PathBuf::from(&m.game_dir);
        json!({
            "game_dir": m.game_dir,
            "name": game_dir.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
            "engine": m.engine,
            "stats": m.stats,
            "box": m.text_box,
            "settings": m.settings,
            "extracted": m.extracted_at.is_some(),
            "has_style": !p.style().trim().is_empty(),
            "translated": done,
            "reviewed": reviewed,
            "problems": problems,
            "total": live.len(),
            "built_at": m.built_at,
            "installed": inst.installed(),
            "installed_at": inst.manifest.installed_at,
            "spent": r4(spent),
            "spent_by_stage": by_stage.iter().map(|(k, v)| (k.clone(), json!(r4(*v)))).collect::<Map<_, _>>(),
            "files": files,
            "test_file": m.test_file,
            "manual_output": m.engine.as_ref().and_then(|e| e.str("manual_output")),
            "root": p.root,
        })
    }

    pub fn recent(&self, limit: usize) -> Vec<Value> {
        let current = self.project().map(|p| p.game_dir());
        let mut out: Vec<(f64, Value)> = Vec::new();
        let Ok(rd) = std::fs::read_dir(self.env.projects_dir()) else { return vec![] };
        for e in rd.flatten() {
            let pj = e.path().join("project.json");
            let Ok(text) = std::fs::read_to_string(&pj) else { continue };
            let Ok(meta) = serde_json::from_str::<crate::project::Meta>(&text) else { continue };
            if meta.game_dir.is_empty() || meta.hidden {
                continue;
            }
            let gd = PathBuf::from(&meta.game_dir);
            if current.as_ref().is_some_and(|c| same_path(c, &gd)) {
                continue;
            }
            let mtime = std::fs::metadata(&pj)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0);
            out.push((
                mtime,
                json!({"game_dir": meta.game_dir,
                       "name": gd.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
                       "engine": meta.engine.as_ref().map(|e| e.name.clone()),
                       "lines": meta.stats.as_ref().map(|s| s.lines),
                       "translated": meta.built_lines,
                       "exists": gd.is_dir(), "t": mtime}),
            ));
        }
        out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        out.into_iter().take(limit).map(|(_, v)| v).collect()
    }

    // ------------------------------------------------------------ actions
    pub fn open_game(&self, game_dir: &str, quiet: bool) -> Result<Value> {
        if game_dir.trim().is_empty() {
            return user("Choose the game folder.");
        }
        let game = abs_path(game_dir);
        let matches = engines::detect(&game)?;
        {
            let mut j = self.job.lock().unwrap();
            if !j.running {
                *j = Job::default(); // old errors belong to the previous game
            }
        }
        let p = Project::for_game(&self.env, &game)?;
        let top = matches.first().filter(|m| m.engine_id != "unsupported").cloned();
        p.update_meta(|m| {
            m.hidden = false;
            if m.engine.is_none() {
                m.engine = top;
            }
        })?;
        let mut i = self.inner.lock().unwrap();
        i.project = Some(Arc::new(p));
        i.matches = matches.clone();
        i.settings.last_game = Some(game.to_string_lossy().to_string());
        self.env.save_settings(&i.settings)?;
        drop(i);
        if !quiet {
            match matches.first() {
                Some(m) => self.log(&format!("Opened {}: looks like {}.", game.display(), m.name)),
                None => self.log(&format!("Opened {}: engine not recognized.", game.display())),
            }
        }
        Ok(json!({"matches": matches}))
    }

    fn set_engine(&self, a: &Value) -> Result<Value> {
        let p = self.need_project()?;
        let matches = self.inner.lock().unwrap().matches.clone();
        let chosen = if !s_arg(a, "scripts_folder").is_empty() {
            Some(manual::for_folder(s_arg(a, "scripts_folder"), &p.game_dir()))
        } else if !s_arg(a, "engine_id").is_empty() {
            match matches.into_iter().find(|m| m.engine_id == s_arg(a, "engine_id")) {
                Some(m) => Some(m),
                None => return user("That engine isn't available for this folder."),
            }
        } else {
            None
        };
        p.update_meta(|m| {
            if let Some(c) = chosen {
                m.engine = Some(c);
            }
            if let (Some(t), Some(e)) = (a.get("xp3_title"), m.engine.as_mut()) {
                match t.as_str().filter(|s| !s.is_empty()) {
                    Some(s) => e.set("xp3_title", s),
                    None => e.set("xp3_title", Value::Null),
                }
            }
        })?;
        Ok(json!({"ok": true}))
    }

    fn save_settings(&self, a: &Value) -> Result<Value> {
        let mut i = self.inner.lock().unwrap();
        let s = &mut i.settings;
        if let Some(v) = a.get("provider").and_then(Value::as_str) {
            s.provider = v.into();
        }
        if let Some(v) = a.get("preset").and_then(Value::as_str) {
            s.preset = v.into();
        }
        if let Some(v) = a.get("review").and_then(Value::as_bool) {
            s.review = v;
        }
        let num =
            |k: &str| a.get(k).and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())));
        if let Some(v) = num("budget_usd") {
            s.budget_usd = v.max(0.0);
        }
        if let Some(v) = num("batch_size") {
            s.batch_size = (v as usize).clamp(1, 200);
        }
        if let Some(v) = num("context_lines") {
            s.context_lines = (v as usize).min(60);
        }
        if let Some(v) = num("parallel") {
            s.parallel = (v as usize).clamp(1, 16);
        }
        let prov = s.provider.clone();
        if let Some(k) = a.get("api_key").and_then(Value::as_str).filter(|k| !k.trim().is_empty()) {
            s.keys.insert(prov.clone(), k.trim().into());
        }
        if a.get("clear_key").and_then(Value::as_bool).unwrap_or(false) {
            s.keys.shift_remove(&prov);
        }
        if let Some(v) = a.get("base_url").and_then(Value::as_str) {
            s.base_urls.insert(prov.clone(), v.trim().into());
        }
        if let Some(v) = a.get("translate_model").and_then(Value::as_str) {
            s.translate_model.insert(prov.clone(), v.trim().into());
        }
        if let Some(v) = a.get("review_model").and_then(Value::as_str) {
            s.review_model.insert(prov.clone(), v.trim().into());
        }
        self.env.save_settings(s)?;
        Ok(json!({"ok": true}))
    }

    fn test_key(&self) -> Result<Value> {
        let s = self.settings();
        let (tm, _) = pipeline::models_for(&s);
        let client = pipeline::client_for(&s, &self.factory, &tm)?;
        let cancel = AtomicBool::new(false);
        match client.complete("Reply with the single word OK.", "Say OK.", 10, &cancel) {
            Ok(c) => Ok(json!({"ok": true, "reply": c.text.trim().chars().take(40).collect::<String>(), "model": tm})),
            Err(llm::LlmError::Truncated(_)) => Ok(json!({"ok": true, "reply": "", "model": tm})),
            Err(llm::LlmError::Fatal(m)) => Err(Error::Llm(m)),
        }
    }

    fn set_box(&self, a: &Value) -> Result<Value> {
        let p = self.need_project()?;
        p.update_meta(|m| {
            let mut b = m.text_box.clone().unwrap_or_default();
            if let Some(w) = a.get("width").and_then(Value::as_f64) {
                b.width = (w as i64).clamp(8, 200) as usize;
            }
            if let Some(n) = a.get("lines").and_then(Value::as_f64) {
                b.lines = (n as i64).clamp(1, 12) as usize;
            }
            b.user_set = true;
            m.text_box = Some(b);
            if let Some(v) = a.get("insert_breaks").and_then(Value::as_bool) {
                m.settings.insert_breaks = v;
            }
            if let Some(v) = a.get("quotes").and_then(Value::as_str) {
                m.settings.quotes = v.into();
            }
            if let Some(v) = a.get("test_file") {
                m.test_file = v.as_str().filter(|s| !s.is_empty()).map(String::from);
            }
        })?;
        Ok(json!({"ok": true}))
    }

    /// Translated lines rendered for the text box preview.
    fn preview(&self, n: usize) -> Result<Value> {
        let Some(p) = self.project() else { return Ok(json!({"lines": []})) };
        let pipe = self.pipeline()?;
        let rc = pipe.render_ctx()?;
        let names = p.glossary().names;
        let mut out: Vec<Value> = Vec::new();
        let live = pipe.live_lines();
        p.with_results(|res| {
            for l in &live {
                let Some(r) = res.get(&l.id).filter(|r| r.translated()) else { continue };
                let (text, over) = pipe.render(&rc, l, r);
                let lb = rc.eng.line_break_for(&l.file);
                let shown: Vec<String> = text.split(lb).map(|x| CODE_RE.replace_all(x, "").into_owned()).collect();
                let who = l.name.as_ref().map(|n| names.get(n).filter(|e| !e.is_empty()).unwrap_or(n).clone());
                out.push(json!({"id": l.id, "name": who, "lines": shown, "over": over, "jp": protect(&l.jp, lb).text}));
                if out.len() >= n * 4 {
                    break;
                }
            }
        });
        // Prefer long lines: they show overflow problems first.
        let len = |v: &Value| -> usize {
            v["lines"].as_array().map(|a| a.iter().map(|s| s.as_str().unwrap_or("").chars().count()).sum()).unwrap_or(0)
        };
        out.sort_by_key(|v| std::cmp::Reverse(len(v)));
        out.truncate(n);
        Ok(json!({"lines": out, "box": p.meta().text_box}))
    }

    fn lines(&self, q: &str, flt: &str, page: usize, per: usize) -> Result<Value> {
        let Some(p) = self.project() else { return Ok(json!({"rows": [], "total": 0})) };
        let pipe = self.pipeline()?;
        let eng = pipe.engine().ok();
        let names = p.glossary().names;
        let gloss =
            if matches!(flt, "glossary" | "all" | "problems") { pipe.glossary_issues() } else { HashMap::new() };
        let q = q.to_lowercase();
        let lines = p.lines();
        let (total, rows) = p.with_results(|res| {
            let mut hits: Vec<(&Line, Option<&Arc<ResultRow>>)> = Vec::new();
            for l in lines.iter() {
                let r = res.get(&l.id);
                let tr = r.is_some_and(|r| r.translated());
                let keep = match flt {
                    "untranslated" => !tr && l.skip.is_empty(),
                    "problems" => r.is_some_and(|r| !r.problems.is_empty()) || gloss.contains_key(&l.id),
                    "skipped" => !l.skip.is_empty(),
                    "translated" => tr,
                    "glossary" => gloss.contains_key(&l.id),
                    "reviewed" => r.is_some_and(|r| r.before.is_some()),
                    _ => true,
                };
                if !keep {
                    continue;
                }
                if !q.is_empty() {
                    let en = r.map(|r| r.en_ph.to_lowercase()).unwrap_or_default();
                    let who = l.name.as_deref().unwrap_or("").to_lowercase();
                    let who_en =
                        l.name.as_ref().and_then(|n| names.get(n)).map(|s| s.to_lowercase()).unwrap_or_default();
                    if !l.jp.to_lowercase().contains(&q)
                        && !en.contains(&q)
                        && !who.contains(&q)
                        && !who_en.contains(&q)
                    {
                        continue;
                    }
                }
                hits.push((l, r));
            }
            let total = hits.len();
            let rows: Vec<Value> = hits
                .into_iter()
                .skip(page * per)
                .take(per)
                .map(|(l, r)| {
                    let jp = match &eng {
                        Some(e) => protect(&l.jp, e.line_break_for(&l.file)).text.clone(),
                        None => l.jp.clone(),
                    };
                    let who = l
                        .name
                        .as_ref()
                        .map(|n| names.get(n).filter(|e| !e.is_empty()).unwrap_or(n).clone())
                        .unwrap_or_default();
                    json!({"id": l.id, "file": l.file, "who": who, "jp": jp,
                           "en": r.map(|r| r.en_ph.clone()).unwrap_or_default(),
                           "stage": r.map(|r| r.stage.clone()),
                           "problems": r.map(|r| r.problems.clone()).filter(|p| !p.is_empty()),
                           "skip": l.skip, "before": r.and_then(|r| r.before.clone()),
                           "glossary": gloss.get(&l.id)})
                })
                .collect();
            (total, rows)
        });
        Ok(json!({"rows": rows, "total": total, "page": page, "per": per}))
    }

    fn edit_line(&self, a: &Value) -> Result<Value> {
        let p = self.need_project()?;
        let id = s_arg(a, "id");
        let lines = p.lines();
        let Some(pos) = lines.iter().position(|l| l.id == id) else { return user("Line not found.") };
        if let Some(skip) = a.get("skip").and_then(Value::as_bool) {
            let mut v: Vec<Line> = (*lines).clone();
            v[pos].skip = if skip { "skipped by you".into() } else { String::new() };
            p.set_lines(v)?;
            return Ok(json!({"ok": true}));
        }
        let line = &lines[pos];
        let en = s_arg(a, "en").trim();
        if en.is_empty() {
            // Clearing a line means "translate this again".
            p.add_results(vec![ResultRow {
                id: line.id.clone(),
                stage: "cleared".into(),
                model: "you".into(),
                src: Some(line.jp.clone()),
                ..Default::default()
            }])?;
            return Ok(json!({"ok": true, "problems": []}));
        }
        let eng = self.pipeline()?.engine()?;
        let pr = protect(&line.jp, eng.line_break_for(&line.file));
        let probs: Vec<String> = check(&pr, Some(en)).into_iter().filter(|x| x != "still has Japanese").collect();
        p.add_results(vec![ResultRow {
            id: line.id.clone(),
            en_ph: en.into(),
            stage: "manual".into(),
            model: "you".into(),
            src: Some(line.jp.clone()),
            problems: probs.clone(),
            ..Default::default()
        }])?;
        Ok(json!({"ok": true, "problems": probs}))
    }

    fn save_style(&self, a: &Value) -> Result<Value> {
        let p = self.need_project()?;
        if let Some(s) = a.get("style").and_then(Value::as_str) {
            p.set_style(s)?;
        }
        if let Some(g) = a.get("glossary") {
            let mut gl: crate::project::Glossary = serde_json::from_value(g.clone()).unwrap_or_default();
            gl.names = gl
                .names
                .into_iter()
                .filter(|(k, _)| !k.trim().is_empty())
                .map(|(k, v)| (k.trim().into(), v.trim().into()))
                .collect();
            gl.terms.retain(|t| !t.jp.trim().is_empty());
            p.set_glossary(gl)?;
        }
        Ok(json!({"ok": true}))
    }

    fn test_ids(&self, pipe: &Pipeline) -> (Vec<String>, Option<String>) {
        let live = pipe.live_lines();
        let mut f = pipe.p.meta().test_file.clone();
        if f.as_ref().is_none_or(|f| !live.iter().any(|l| &l.file == f)) {
            let mut counts: HashMap<&str, usize> = HashMap::new();
            for l in &live {
                *counts.entry(l.file.as_str()).or_default() += 1;
            }
            let mut files: Vec<&str> = counts.keys().copied().collect();
            files.sort_by_key(|x| x.to_lowercase());
            f = files.iter().find(|x| counts[*x] >= 30).or(files.first()).map(|s| s.to_string());
        }
        let chosen = live.iter().filter(|l| Some(&l.file) == f.as_ref()).take(150).map(|l| l.id.clone()).collect();
        (chosen, f)
    }

    fn run_action(self: &Arc<Self>, action: &str) -> Result<Value> {
        let pipe = self.pipeline()?;
        let app = self.clone();
        match action {
            "extract" => self.start(action, move || Ok(json!(pipe.extract()?))),
            "style" => self.start(action, move || pipe.draft_style()),
            "test" => {
                let (ids, f) = self.test_ids(&pipe);
                self.start(action, move || {
                    pipe.translate(Some(&ids), false)?;
                    pipe.build()?;
                    pipe.install()?;
                    app.log(&format!(
                        "Test patch installed: the first {} lines of {}. Launch the game to check them.",
                        ids.len(),
                        f.unwrap_or_default()
                    ));
                    Ok(json!({"test": ids.len()}))
                })
            }
            "refit" => self.start(action, move || {
                pipe.build()?;
                pipe.install()?;
                app.log("Rebuilt with the new text box settings.");
                Ok(json!({}))
            }),
            "full" => self.start(action, move || {
                pipe.translate(None, false)?;
                if pipe.models().1.is_some() {
                    pipe.review(None)?;
                }
                if !pipe.glossary_issues().is_empty() {
                    pipe.fix_glossary()?;
                }
                if !pipe.overflow_lines()?.is_empty() {
                    pipe.shorten(None)?;
                }
                let b = pipe.build()?;
                pipe.install()?;
                app.log("Done. The translation is installed. Launch the game!");
                Ok(json!({"applied": b.applied, "complete": true}))
            }),
            "translate" => self.start(action, move || Ok(json!(pipe.translate(None, false)?))),
            "review" => self.start(action, move || Ok(json!(pipe.review(None)?))),
            "fit" => self.start(action, move || Ok(json!(pipe.shorten(None)?))),
            "build" => self.start(action, move || {
                let b = pipe.build()?;
                pipe.install()?;
                Ok(json!(b))
            }),
            "restore" => self.start(action, move || {
                pipe.restore()?;
                Ok(json!({}))
            }),
            "glossary" => self.start(action, move || Ok(json!(pipe.fix_glossary()?))),
            _ => user(format!("Unknown action {action}")),
        }
    }

    fn retranslate(self: &Arc<Self>, ids: Vec<String>) -> Result<Value> {
        let pipe = self.pipeline()?;
        if ids.is_empty() {
            return user("Pick at least one line.");
        }
        let installed = pipe.installed();
        let app = self.clone();
        self.start("retranslate", move || {
            pipe.translate(Some(&ids), true)?;
            if pipe.models().1.is_some() {
                pipe.review(Some(&ids))?;
            }
            if installed {
                pipe.build()?;
                pipe.install()?;
            }
            app.log(&format!("Retranslated {} line{}.", ids.len(), if ids.len() == 1 { "" } else { "s" }));
            Ok(json!({}))
        })
    }

    fn estimate(&self, all: bool) -> Result<Value> {
        let Some(p) = self.project().filter(|p| p.meta().stats.is_some()) else { return Ok(json!({})) };
        if all {
            let base = self.settings();
            let mut out = Map::new();
            for pre in QUALITY_PRESETS {
                let mut s = base.clone();
                s.preset = pre.id.into();
                s.review = pre.review.is_some();
                out.insert(
                    pre.id.into(),
                    Pipeline::new(self.env.clone(), p.clone(), s, self.factory.clone()).estimate(),
                );
            }
            return Ok(Value::Object(out));
        }
        Ok(self.pipeline()?.estimate())
    }

    fn preflight(&self) -> Result<Value> {
        if !self.project().is_some_and(|p| p.meta().stats.is_some()) {
            return Ok(json!({"notes": [], "glossary_issues": 0, "overflow": 0}));
        }
        let pipe = self.pipeline()?;
        Ok(json!({"notes": pipe.preflight(), "glossary_issues": pipe.glossary_issues().len(),
                  "overflow": pipe.overflow_lines().map(|v| v.len()).unwrap_or(0)}))
    }

    fn export_csv(&self) -> Result<Value> {
        let p = self.need_project()?;
        let eng = self.pipeline()?.engine()?;
        let names = p.glossary().names;
        let mut w = csv::Writer::from_writer(Vec::new());
        let lines = p.lines();
        p.with_results(|res| -> Result<()> {
            w.write_record(["id", "speaker", "japanese", "english", "status"])
                .map_err(|e| Error::User(e.to_string()))?;
            for l in lines.iter().filter(|l| l.skip.is_empty()) {
                let r = res.get(&l.id);
                let who = l
                    .name
                    .as_ref()
                    .map(|n| names.get(n).filter(|e| !e.is_empty()).unwrap_or(n).clone())
                    .unwrap_or_default();
                let jp = protect(&l.jp, eng.line_break_for(&l.file)).text.clone();
                let en = r.map(|r| r.en_ph.clone()).unwrap_or_default();
                let st = r.map(|r| r.stage.clone()).unwrap_or_default();
                w.write_record([l.id.as_str(), &who, &jp, &en, &st]).map_err(|e| Error::User(e.to_string()))?;
            }
            Ok(())
        })?;
        let data = w.into_inner().map_err(|e| Error::User(e.to_string()))?;
        let folder = p.path("exports");
        std::fs::create_dir_all(&folder)?;
        let path = folder.join(chrono::Local::now().format("lines-%Y%m%d-%H%M%S.csv").to_string());
        let mut bytes = b"\xEF\xBB\xBF".to_vec(); // BOM so Excel reads Japanese
        bytes.extend(data);
        std::fs::write(&path, bytes)?;
        self.log(&format!("Exported {}", path.display()));
        let _ = (self.opener)(&folder);
        Ok(json!({"path": path}))
    }

    fn import_csv(&self, text: &str) -> Result<Value> {
        let p = self.need_project()?;
        let eng = self.pipeline()?.engine()?;
        let lines = p.lines();
        let by_id: HashMap<&str, &Line> = lines.iter().map(|l| (l.id.as_str(), l)).collect();
        let mut rdr =
            csv::ReaderBuilder::new().flexible(true).from_reader(text.trim_start_matches('\u{feff}').as_bytes());
        let headers = rdr.headers().map_err(|e| Error::User(format!("That CSV couldn't be read: {e}")))?.clone();
        let col = |name: &str| headers.iter().position(|h| h.trim() == name);
        let (Some(ci), Some(ce)) = (col("id"), col("english")) else {
            return user(
                "That CSV needs \"id\" and \"english\" columns. Export from Yakusoku first and edit that file.",
            );
        };
        let (mut rows, mut changed, mut problems, mut unknown) = (Vec::new(), 0, 0, 0);
        for rec in rdr.records().flatten() {
            let Some(l) = by_id.get(rec.get(ci).unwrap_or("").trim()) else {
                unknown += 1;
                continue;
            };
            let en = rec.get(ce).unwrap_or("").trim();
            let before = p.result(&l.id).map(|r| r.en_ph.clone());
            if en.is_empty() || Some(en) == before.as_deref() {
                continue;
            }
            let probs: Vec<String> = check(&protect(&l.jp, eng.line_break_for(&l.file)), Some(en))
                .into_iter()
                .filter(|x| x != "still has Japanese")
                .collect();
            if !probs.is_empty() {
                problems += 1;
            }
            rows.push(ResultRow {
                id: l.id.clone(),
                en_ph: en.into(),
                stage: "manual".into(),
                model: "import".into(),
                src: Some(l.jp.clone()),
                before,
                problems: probs,
                ..Default::default()
            });
            changed += 1;
        }
        p.add_results(rows)?;
        self.log(&format!("Imported {changed} edited lines from CSV."));
        Ok(json!({"changed": changed, "problems": problems, "unknown": unknown}))
    }

    fn launch(&self) -> Result<Value> {
        let p = self.need_project()?;
        let game = p.game_dir();
        let Some(exe) = engines::find_exe(&game) else { return user("Couldn't find the game's .exe in its folder.") };
        std::process::Command::new(&exe)
            .current_dir(&game)
            .spawn()
            .map_err(|e| Error::User(format!("Couldn't start the game: {e}")))?;
        Ok(json!({"launched": exe.file_name().map(|s| s.to_string_lossy().to_string())}))
    }

    fn reveal(&self, which: &str) -> Result<Value> {
        let p = self.need_project()?;
        let path = match which {
            "game" => Some(p.game_dir()),
            "project" => Some(p.root.clone()),
            "manual" => p.meta().engine.as_ref().and_then(|e| e.str("manual_output")).map(PathBuf::from),
            _ => None,
        };
        let Some(path) = path.filter(|p| p.exists()) else { return user("That folder doesn't exist yet.") };
        (self.opener)(&path).map_err(|e| Error::User(format!("Couldn't open the folder: {e}")))?;
        Ok(json!({"ok": true}))
    }

    /// Hide a game from the recent list. Its translations are kept and come
    /// back if the same folder is opened again.
    fn forget(&self, game_dir: &str) -> Result<Value> {
        if game_dir.is_empty() {
            return user("No game given.");
        }
        let p = Project::open(self.env.projects_dir().join(slugify(&abs_path(game_dir))))?;
        p.update_meta(|m| m.hidden = true)?;
        Ok(json!({"ok": true}))
    }
}
