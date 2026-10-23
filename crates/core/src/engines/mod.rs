//! Engine adapters: how to get text out of a game and back in.
//!
//! Every adapter does the same four things:
//!
//!   detect(game_dir)  -> a Match, or nothing
//!   prepare(cx)       -> unpack archives if needed, export scripts to JSON
//!                        files in work/json (msg-tool / VNTextPatch format:
//!                        `[{"name": ..., "message": ...}, ...]`)
//!   build(cx)         -> import translated JSON (work/json_tl) back into the
//!                        scripts and package them, staged in work/out
//!   install(cx)       -> copy the staged files into the game folder, backing
//!                        up anything they replace; restore() undoes it.

pub mod generic;
pub mod kirikiri;
pub mod ks;
pub mod manual;
pub mod msgtool;
pub mod xp3;

use crate::config::{write_json_atomic, Env};
use crate::error::{user, Error, Result};
use crate::project::{now, Project};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

pub use ks::Entry;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Match {
    pub engine_id: String,
    pub name: String,
    #[serde(default)]
    pub confidence: f64,
    #[serde(default)]
    pub details: Map<String, Value>,
    /// auto | experimental | manual
    #[serde(default = "auto")]
    pub support: String,
    #[serde(default)]
    pub note: String,
}

fn auto() -> String {
    "auto".into()
}

impl Match {
    pub fn new(
        engine_id: &str,
        name: &str,
        confidence: f64,
        details: Map<String, Value>,
        support: &str,
        note: &str,
    ) -> Match {
        Match {
            engine_id: engine_id.into(),
            name: name.into(),
            confidence,
            details,
            support: support.into(),
            note: note.into(),
        }
    }
    pub fn str(&self, k: &str) -> Option<&str> {
        self.details.get(k).and_then(Value::as_str).filter(|s| !s.is_empty())
    }
    pub fn flag(&self, k: &str) -> bool {
        self.details.get(k).and_then(Value::as_bool).unwrap_or(false)
    }
    pub fn strings(&self, k: &str) -> Vec<String> {
        self.details
            .get(k)
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default()
    }
    pub fn set(&mut self, k: &str, v: impl Into<Value>) {
        self.details.insert(k.into(), v.into());
    }
}

/// What an engine needs while it works.
pub struct Ctx<'a> {
    pub env: &'a Env,
    pub project: &'a Project,
    pub log: &'a (dyn Fn(&str) + Sync),
}

impl Ctx<'_> {
    pub fn log(&self, msg: impl AsRef<str>) {
        (self.log)(msg.as_ref())
    }
    pub fn game(&self) -> PathBuf {
        self.project.game_dir()
    }
}

pub trait Engine: Send + Sync {
    fn m(&self) -> &Match;
    fn m_mut(&mut self) -> &mut Match;
    /// Encoding the game's text must fit in ("cp932", "utf-8", "utf-16").
    fn encoding(&self) -> String {
        "cp932".into()
    }
    /// How a line break is written in messages of this exported file.
    fn line_break_for(&self, _rel: &str) -> &'static str {
        "\n"
    }
    /// Curly quotes look wide in most Shift-JIS fonts.
    fn straight_quotes(&self) -> bool {
        true
    }
    fn prepare(&mut self, cx: &Ctx) -> Result<PathBuf>;
    fn build(&mut self, cx: &Ctx) -> Result<PathBuf>;

    fn install(&mut self, cx: &Ctx) -> Result<Vec<String>> {
        let mut inst = Installer::new(cx.project);
        let out = cx.project.work("out");
        let files = list_files(&out, None);
        if files.is_empty() {
            return user("Nothing to install yet. Build the patch first.");
        }
        for rel in &files {
            inst.put(&join_rel(&out, rel), rel, &cx.game())?;
            cx.log(format!("Installed {rel}"));
        }
        Ok(files)
    }

    fn restore(&mut self, cx: &Ctx) -> Result<()> {
        Installer::new(cx.project).restore(&cx.game(), cx.log)
    }
}

// ---------------------------------------------------------------- registry

pub fn detect(game_dir: &Path) -> Result<Vec<Match>> {
    if !game_dir.is_dir() {
        return user("That folder doesn't exist.");
    }
    let mut matches: Vec<Match> = Vec::new();
    if let Some(m) = kirikiri::detect(game_dir) {
        matches.push(m);
    }
    for prof in generic::PROFILES {
        if let Some(m) = generic::detect_profile(game_dir, prof) {
            matches.push(m);
        }
    }
    matches.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal));
    if matches.first().is_none_or(|m| m.confidence < 0.9) {
        if let Some(u) = generic::detect_unsupported_strong(game_dir) {
            matches.insert(0, u);
            return Ok(matches);
        }
    }
    if matches.is_empty() {
        if let Some(u) = generic::detect_unsupported(game_dir) {
            matches.push(u);
        }
    }
    Ok(matches)
}

pub fn engine_for(m: &Match) -> Result<Box<dyn Engine>> {
    match m.engine_id.as_str() {
        "kirikiri" => Ok(Box::new(kirikiri::Kirikiri { m: m.clone() })),
        "scripts" => Ok(Box::new(manual::ScriptFolder { m: m.clone() })),
        id if generic::profile(id).is_some() => Ok(Box::new(generic::MsgToolEngine { m: m.clone() })),
        _ => Err(Error::User(format!("{} isn't supported. {}", m.name, m.note).trim().to_string())),
    }
}

// ---------------------------------------------------------------- files

/// `root/a/b.txt` for rel "a/b.txt".
pub fn join_rel(root: &Path, rel: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    for part in rel.split(['/', '\\']).filter(|s| !s.is_empty()) {
        p.push(part);
    }
    p
}

fn ext_of(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => name[i..].to_lowercase(),
        _ => String::new(),
    }
}

/// Files under root, as '/'-separated relative paths, sorted case-insensitively.
pub fn list_files(root: &Path, exts: Option<&[&str]>) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let rel = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                stack.push((e.path(), rel));
            } else if exts.is_none_or(|x| x.contains(&ext_of(&name).as_str())) {
                out.push(rel);
            }
        }
    }
    out.sort_by_key(|p| p.to_lowercase());
    out
}

pub fn read_json_messages(path: &Path) -> Result<Vec<Entry>> {
    let data = fs::read(path)?;
    let data = data.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&data);
    let v: Value = serde_json::from_slice(data).map_err(|_| {
        Error::User(format!("Unexpected JSON in {}", path.file_name().unwrap_or_default().to_string_lossy()))
    })?;
    let Value::Array(items) = v else {
        return user(format!("Unexpected JSON in {}", path.file_name().unwrap_or_default().to_string_lossy()));
    };
    Ok(items
        .into_iter()
        .map(|it| Entry {
            name: it.get("name").and_then(Value::as_str).map(String::from),
            names: it
                .get("names")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(|n| n.as_str().unwrap_or("").to_string()).collect()),
            message: it.get("message").and_then(Value::as_str).unwrap_or("").to_string(),
        })
        .collect())
}

pub fn write_json_messages(path: &Path, entries: &[Entry]) -> Result<()> {
    if let Some(d) = path.parent() {
        fs::create_dir_all(d)?;
    }
    fs::write(path, serde_json::to_vec_pretty(entries)?)?;
    Ok(())
}

pub fn clean_dir(path: &Path) -> Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path)?;
    }
    fs::create_dir_all(path)?;
    Ok(())
}

/// Recursively copy `src` into `dst` (merging, overwriting files).
pub fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    for rel in list_files(src, None) {
        let to = join_rel(dst, &rel);
        if let Some(d) = to.parent() {
            fs::create_dir_all(d)?;
        }
        fs::copy(join_rel(src, &rel), to)?;
    }
    Ok(())
}

// ---------------------------------------------------------------- tools

/// Run a command-line tool. Returns (exit code, combined output).
pub fn run_tool(args: &[String], cx: &Ctx) -> Result<(i32, String)> {
    cx.log(format!(
        "$ {}",
        args.iter().map(|a| if a.contains(' ') { format!("\"{a}\"") } else { a.clone() }).collect::<Vec<_>>().join(" ")
    ));
    let mut cmd = std::process::Command::new(&args[0]);
    cmd.args(&args[1..]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::User(format!("Missing tool: {}. Reinstall Yakusoku or put the tool in the tools folder.", args[0]))
        } else {
            Error::User(format!("Couldn't run {}: {e}", args[0]))
        }
    })?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let lines: Vec<&str> = text.trim().lines().collect();
    for l in &lines[lines.len().saturating_sub(40)..] {
        cx.log(format!("  {l}"));
    }
    Ok((out.status.code().unwrap_or(-1), text))
}

// ---------------------------------------------------------------- install / restore

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Manifest {
    pub replaced: Vec<String>,
    pub added: Vec<String>,
    pub installed_at: Option<f64>,
}

/// Copies staged files into the game and remembers how to undo it.
///
/// The manifest (in the project folder) lists every file we added and every
/// original we replaced, with a backup copy of each original. Restore puts
/// originals back and deletes what we added. Originals are backed up once,
/// the first time they're replaced, so repeated installs never back up a
/// patched file by mistake.
pub struct Installer {
    path: PathBuf,
    backup: PathBuf,
    pub manifest: Manifest,
}

impl Installer {
    pub fn new(p: &Project) -> Installer {
        let path = p.path("install.json");
        let manifest = fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        Installer { path, backup: p.path("backup"), manifest }
    }

    fn save(&self) -> Result<()> {
        write_json_atomic(&self.path, &serde_json::to_value(&self.manifest)?)?;
        Ok(())
    }

    pub fn installed(&self) -> bool {
        !self.manifest.replaced.is_empty() || !self.manifest.added.is_empty()
    }

    /// Copy `src` into the game at `rel`.
    ///
    /// Order matters for recovery: the original is backed up and the
    /// manifest saved (atomically) *before* the game file is touched. If
    /// anything fails after that, Restore still knows about the file; if the
    /// manifest can't be saved, the game file is never replaced.
    pub fn put(&mut self, src: &Path, rel: &str, game: &Path) -> Result<()> {
        let dst = join_rel(game, rel);
        let known = self.manifest.replaced.iter().chain(&self.manifest.added).any(|r| r == rel);
        if !known {
            if dst.exists() {
                let bak = join_rel(&self.backup, rel);
                fs::create_dir_all(bak.parent().unwrap())?;
                let tmp = bak.with_file_name(format!("{}.part", bak.file_name().unwrap().to_string_lossy()));
                fs::copy(&dst, &tmp)?;
                fs::rename(&tmp, &bak)?;
                self.manifest.replaced.push(rel.to_string());
            } else {
                self.manifest.added.push(rel.to_string());
            }
            if let Err(e) = self.save() {
                // Nothing in the game has changed yet: forget the intent.
                self.manifest.replaced.retain(|r| r != rel);
                self.manifest.added.retain(|r| r != rel);
                return Err(e);
            }
        }
        fs::create_dir_all(dst.parent().unwrap())?;
        let tmp = dst.with_file_name(format!("{}.yakusoku-tmp", dst.file_name().unwrap().to_string_lossy()));
        let r = fs::copy(src, &tmp).and_then(|_| fs::rename(&tmp, &dst));
        if let Err(e) = r {
            let _ = fs::remove_file(&tmp);
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                return user(format!(
                    "Windows won't let Yakusoku replace {rel}. Close the game (and anything else using its files), then try again."
                ));
            }
            return Err(e.into());
        }
        self.manifest.installed_at = Some(now());
        self.save()
    }

    pub fn restore(&mut self, game: &Path, log: &(dyn Fn(&str) + Sync)) -> Result<()> {
        let denied = |e: std::io::Error| -> Error {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                Error::User(
                    "Windows won't let Yakusoku change the game's files. Close the game, then try again.".into(),
                )
            } else {
                e.into()
            }
        };
        for rel in self.manifest.added.clone() {
            let p = join_rel(game, &rel);
            if p.exists() {
                fs::remove_file(&p).map_err(denied)?;
                log(&format!("Removed {rel}"));
            }
        }
        for rel in self.manifest.replaced.clone() {
            let bak = join_rel(&self.backup, &rel);
            if bak.exists() {
                fs::copy(&bak, join_rel(game, &rel)).map_err(denied)?;
                log(&format!("Restored {rel}"));
            }
        }
        self.manifest = Manifest::default();
        self.save()
    }
}

/// The game executable to launch for testing.
pub fn find_exe(game_dir: &Path) -> Option<PathBuf> {
    let mut exes: Vec<(String, u64)> = fs::read_dir(game_dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if name.to_lowercase().ends_with(".exe") {
                Some((name, e.metadata().map(|m| m.len()).unwrap_or(0)))
            } else {
                None
            }
        })
        .collect();
    const SKIP: &[&str] = &["unins", "setup", "config", "launcher", "vcredist", "dxsetup", "install"];
    let good: Vec<(String, u64)> =
        exes.iter().filter(|(n, _)| !SKIP.iter().any(|s| n.to_lowercase().contains(s))).cloned().collect();
    if !good.is_empty() {
        exes = good;
    }
    exes.sort_by_key(|e| std::cmp::Reverse(e.1));
    exes.first().map(|(n, _)| game_dir.join(n))
}
