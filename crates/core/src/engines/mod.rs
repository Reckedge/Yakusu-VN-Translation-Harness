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
use std::collections::BTreeMap;
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

/// `root/a/b.txt` for rel "a/b.txt", for paths Yakusu produced itself.
///
/// Never leaves `root`: "..", "." and drive-like parts are dropped. Names
/// that come from outside (archives, tools, the manifest) go through
/// [`safe_join`] instead, which refuses them rather than rewriting them.
pub fn join_rel(root: &Path, rel: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    for part in rel.split(['/', '\\']) {
        if part.is_empty() || part == "." || part == ".." || part.contains(':') {
            continue;
        }
        p.push(part);
    }
    p
}

/// Check a relative file name that came from outside Yakusu and return it
/// '/'-separated. Both slashes count as separators. Refused: empty names,
/// absolute paths, "." / "..", drive letters and streams (":"), Windows
/// device names (CON, NUL, COM1 ...), characters Windows can't store, and
/// parts ending in a dot or space (Windows silently drops those).
pub fn safe_rel(rel: &str) -> Result<String> {
    let bad = |why: &str| -> Result<String> {
        Err(Error::User(format!("Refusing the unsafe file name \"{}\" ({why}).", rel.escape_debug())))
    };
    if rel.chars().any(|c| (c as u32) < 0x20) {
        return bad("control characters");
    }
    let norm = rel.replace('\\', "/");
    if norm.starts_with('/') {
        return bad("absolute path");
    }
    let mut parts = Vec::new();
    for part in norm.split('/') {
        if part.is_empty() {
            continue;
        }
        if part == ".." {
            return bad("points to a parent folder");
        }
        if part == "." {
            return bad("a \".\" folder");
        }
        if part.contains(':') {
            return bad("drive letter or stream");
        }
        if part.chars().any(|c| "<>\"|?*".contains(c)) {
            return bad("characters Windows can't use");
        }
        if part.ends_with('.') || part.ends_with(' ') {
            return bad("ends with a dot or space");
        }
        let stem = part.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
        let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.len() == 4
                && stem.as_bytes()[3].is_ascii_digit());
        if device {
            return bad("Windows device name");
        }
        parts.push(part);
    }
    if parts.is_empty() {
        return bad("empty");
    }
    Ok(parts.join("/"))
}

/// `root` joined with an untrusted relative name, after [`safe_rel`].
///
/// Also refuses a path that runs through a symlink or junction under `root`
/// (including the file itself), so writes can't be redirected elsewhere.
pub fn safe_join(root: &Path, rel: &str) -> Result<PathBuf> {
    let rel = safe_rel(rel)?;
    let mut p = root.to_path_buf();
    for part in rel.split('/') {
        p.push(part);
        if fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(Error::User(format!(
                "Refusing to follow the link at {}: Yakusu only writes real files inside the folder.",
                p.display()
            )));
        }
    }
    Ok(p)
}

fn ext_of(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => name[i..].to_lowercase(),
        _ => String::new(),
    }
}

/// Files under root, as '/'-separated relative paths, sorted case-insensitively.
/// Symlinks are skipped (neither followed nor listed): everything listed is a
/// real file inside `root`.
pub fn list_files(root: &Path, exts: Option<&[&str]>) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let rel = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                stack.push((e.path(), rel));
            } else if ft.is_file() && exts.is_none_or(|x| x.contains(&ext_of(&name).as_str())) {
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
        let to = safe_join(dst, &rel)?;
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
            Error::User(format!("Missing tool: {}. Reinstall Yakusu or put the tool in the tools folder.", args[0]))
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
    /// SHA-1 of each original's backup, so Restore can tell a damaged backup.
    pub backup_sha1: BTreeMap<String, String>,
}

/// Copies staged files into the game and remembers how to undo it.
///
/// The manifest (in the project folder) lists every file we added and every
/// original we replaced, with a backup copy of each original. Restore puts
/// originals back and deletes what we added.
///
/// Rules that keep the originals safe:
/// - an existing backup is never overwritten: it's the only copy of the
///   original once the game file has been replaced;
/// - a manifest that exists but can't be read stops installs (fail closed);
///   Restore then recovers from the backups themselves;
/// - Restore checks every backup before touching the game and only forgets
///   entries it actually restored;
/// - files are written through a fresh, exclusively created temporary file
///   in the same folder, then renamed into place.
pub struct Installer {
    path: PathBuf,
    backup: PathBuf,
    pub manifest: Manifest,
    damaged: Option<String>,
}

fn denied(e: std::io::Error) -> Error {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        Error::User("Windows won't let Yakusu change the game's files. Close the game (and anything else using its files), then try again.".into())
    } else {
        e.into()
    }
}

/// Create a new, uniquely named temporary file next to `dst`. Exclusive
/// creation: an existing file (or symlink) with that name is never opened.
fn create_temp(dst: &Path) -> std::io::Result<(PathBuf, fs::File)> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = dst.parent().unwrap_or(Path::new("."));
    let name = dst.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    for _ in 0..64 {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let tmp = dir.join(format!(".{name}.{:x}-{nanos:x}-{n:x}.yakusu-tmp", std::process::id()));
        match fs::OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(f) => return Ok((tmp, f)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "couldn't create a temporary file"))
}

/// Replace `dst` with a copy of `src` atomically (through a fresh temp file).
pub fn atomic_copy(src: &Path, dst: &Path) -> std::io::Result<()> {
    let mut input = fs::File::open(src)?;
    let (tmp, mut out) = create_temp(dst)?;
    let r = std::io::copy(&mut input, &mut out).and_then(|_| out.sync_all());
    drop(out);
    let r = r.and_then(|_| fs::rename(&tmp, dst));
    if r.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    r
}

pub fn sha1_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut f = fs::File::open(path)?;
    let mut h = sha1_smol::Sha1::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.digest().to_string())
}

fn same_contents(a: &Path, b: &Path) -> std::io::Result<bool> {
    if fs::metadata(a)?.len() != fs::metadata(b)?.len() {
        return Ok(false);
    }
    Ok(sha1_file(a)? == sha1_file(b)?)
}

impl Installer {
    pub fn new(p: &Project) -> Installer {
        let path = p.path("install.json");
        let (manifest, damaged) = match fs::read(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Manifest::default(), None),
            Err(e) => (Manifest::default(), Some(e.to_string())),
            Ok(bytes) => match serde_json::from_slice::<Manifest>(&bytes) {
                Err(e) => (Manifest::default(), Some(e.to_string())),
                Ok(m) => match m.replaced.iter().chain(&m.added).find(|r| safe_rel(r).is_err()) {
                    Some(r) => (Manifest::default(), Some(format!("it lists the unsafe path {r:?}"))),
                    None => (m, None),
                },
            },
        };
        Installer { path, backup: p.path("backup"), manifest, damaged }
    }

    fn save(&self) -> Result<()> {
        write_json_atomic(&self.path, &serde_json::to_value(&self.manifest)?)?;
        Ok(())
    }

    /// The install record exists but couldn't be read.
    pub fn damaged(&self) -> bool {
        self.damaged.is_some()
    }

    /// Something may be installed (a damaged record counts: Restore recovers it).
    pub fn installed(&self) -> bool {
        self.damaged.is_some() || !self.manifest.replaced.is_empty() || !self.manifest.added.is_empty()
    }

    /// Copy `src` into the game at `rel`.
    ///
    /// Order matters for recovery: the original is backed up and the
    /// manifest saved (atomically) *before* the game file is touched. If
    /// anything fails after that, Restore still knows about the file; if the
    /// manifest can't be saved, the game file is never replaced.
    pub fn put(&mut self, src: &Path, rel: &str, game: &Path) -> Result<()> {
        if let Some(why) = &self.damaged {
            return user(format!(
                "Yakusu's install record for this game ({}) is damaged ({why}), so installing could overwrite \
                 the backup of an original file. Nothing was changed. Use Restore first: it puts the originals \
                 back from the backups.",
                self.path.display()
            ));
        }
        let rel = safe_rel(rel)?;
        let dst = safe_join(game, &rel)?;
        let known = self.manifest.replaced.iter().chain(&self.manifest.added).any(|r| *r == rel);
        if !known {
            if dst.exists() {
                let bak = safe_join(&self.backup, &rel)?;
                if bak.exists() {
                    // Left from an earlier install the record no longer lists.
                    // Reuse it only if it's exactly the game's current file.
                    if !same_contents(&bak, &dst)? {
                        return user(format!(
                            "There's already a backup of {rel} that doesn't match the game's current file, and \
                             Yakusu's install record doesn't mention it. Nothing was changed, so the backup stays \
                             safe. If the game was updated after an earlier restore, delete {} and try again.",
                            bak.display()
                        ));
                    }
                } else {
                    fs::create_dir_all(bak.parent().unwrap())?;
                    atomic_copy(&dst, &bak)?;
                }
                self.manifest.backup_sha1.insert(rel.clone(), sha1_file(&bak)?);
                self.manifest.replaced.push(rel.clone());
            } else {
                self.manifest.added.push(rel.clone());
            }
            if let Err(e) = self.save() {
                // Nothing in the game has changed yet: forget the intent.
                self.manifest.replaced.retain(|r| *r != rel);
                self.manifest.added.retain(|r| *r != rel);
                self.manifest.backup_sha1.remove(&rel);
                return Err(e);
            }
        }
        fs::create_dir_all(dst.parent().unwrap())?;
        atomic_copy(src, &dst).map_err(|e| {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                Error::User(format!(
                    "Windows won't let Yakusu replace {rel}. Close the game (and anything else using its files), then try again."
                ))
            } else {
                e.into()
            }
        })?;
        self.manifest.installed_at = Some(now());
        self.save()
    }

    /// Where the game's original `rel` is now: its backup while our version
    /// is installed over it, else the game's own file. Reading the game again
    /// while a patch is installed must never read our English as the source.
    pub fn original(&self, game: &Path, rel: &str) -> Result<PathBuf> {
        if let Some(why) = &self.damaged {
            return user(format!(
                "Yakusu's install record is damaged ({why}), so it can't tell which game files are originals. \
                 Use Restore first, then read the scripts again."
            ));
        }
        let norm = rel.replace('\\', "/");
        if self.manifest.replaced.contains(&norm) {
            if let Some(problem) = self.backup_problem(&norm) {
                return user(format!("{problem}. Restore or verify the game's files, then read the scripts again."));
            }
            return safe_join(&self.backup, &norm);
        }
        if self.manifest.added.contains(&norm) {
            return user(format!("{rel} was added by Yakusu's patch; it isn't one of the game's files."));
        }
        // Only read here, so links inside the game folder are fine.
        Ok(join_rel(game, &safe_rel(&norm)?))
    }

    /// Why a backup can't be used, if it can't.
    fn backup_problem(&self, rel: &str) -> Option<String> {
        let bak = match safe_join(&self.backup, rel) {
            Ok(b) => b,
            Err(e) => return Some(e.to_string()),
        };
        if !bak.is_file() {
            return Some(format!("{rel}: the backup of the original is missing ({})", bak.display()));
        }
        if let Some(want) = self.manifest.backup_sha1.get(rel) {
            match sha1_file(&bak) {
                Ok(got) if got == *want => {}
                Ok(_) => return Some(format!("{rel}: the backup of the original has changed since it was made")),
                Err(e) => return Some(format!("{rel}: the backup can't be read ({e})")),
            }
        }
        None
    }

    pub fn restore(&mut self, game: &Path, log: &(dyn Fn(&str) + Sync)) -> Result<()> {
        if self.damaged.is_some() {
            return self.recover(game, log);
        }
        // Check everything before touching the game.
        let problems: Vec<String> = self.manifest.replaced.iter().filter_map(|r| self.backup_problem(r)).collect();
        if !problems.is_empty() {
            return user(format!(
                "Restore stopped before changing anything:\n{}\nThe game still has Yakusu's files. To get the \
                 originals back, verify the game's files in its store (Steam: Properties > Installed Files > \
                 Verify integrity) or reinstall it.",
                problems.join("\n")
            ));
        }
        for rel in self.manifest.added.clone() {
            let p = safe_join(game, &rel)?;
            if p.exists() {
                fs::remove_file(&p).map_err(denied)?;
                log(&format!("Removed {rel}"));
            }
            self.manifest.added.retain(|r| *r != rel);
            self.save()?;
        }
        for rel in self.manifest.replaced.clone() {
            let bak = safe_join(&self.backup, &rel)?;
            let dst = safe_join(game, &rel)?;
            fs::create_dir_all(dst.parent().unwrap())?;
            atomic_copy(&bak, &dst).map_err(denied)?;
            if !same_contents(&bak, &dst)? {
                return user(format!(
                    "{rel} didn't come back identical to its backup. The backup is kept in {}.",
                    bak.display()
                ));
            }
            log(&format!("Restored {rel}"));
            self.manifest.replaced.retain(|r| *r != rel);
            self.manifest.backup_sha1.remove(&rel);
            self.save()?;
            // The game has the original again; an old backup would only get
            // in the way of the next install after a game update.
            let _ = fs::remove_file(&bak);
        }
        self.manifest = Manifest::default();
        self.save()
    }

    /// Restore without a readable record: every file in the backup folder is
    /// an original (backups are never overwritten), so put each one back.
    fn recover(&mut self, game: &Path, log: &(dyn Fn(&str) + Sync)) -> Result<()> {
        let why = self.damaged.clone().unwrap_or_default();
        log(&format!("The install record is damaged ({why}). Restoring every original from the backup folder."));
        let backups: Vec<String> = list_files(&self.backup, None)
            .into_iter()
            .filter(|r| !r.ends_with(".yakusu-tmp") && !r.ends_with(".part"))
            .collect();
        for rel in &backups {
            let bak = safe_join(&self.backup, rel)?;
            let dst = safe_join(game, rel)?;
            fs::create_dir_all(dst.parent().unwrap())?;
            atomic_copy(&bak, &dst).map_err(denied)?;
            if !same_contents(&bak, &dst)? {
                return user(format!(
                    "{rel} didn't come back identical to its backup. The backup is kept in {}.",
                    bak.display()
                ));
            }
            log(&format!("Restored {rel}"));
        }
        // Patches Yakusu built carry a marker file, so they can be found
        // without the record.
        let mut removed = 0;
        if let Ok(rd) = fs::read_dir(game) {
            for e in rd.flatten() {
                let p = e.path();
                let is_xp3 = p.extension().is_some_and(|x| x.eq_ignore_ascii_case("xp3"));
                if is_xp3 && xp3::read_index(&p).is_ok_and(|ix| ix.iter().any(|x| x.name == kirikiri::PATCH_MARKER)) {
                    fs::remove_file(&p).map_err(denied)?;
                    log(&format!("Removed {}", e.file_name().to_string_lossy()));
                    removed += 1;
                }
            }
        }
        let aside = self.path.with_file_name(format!("install.damaged-{}.json", now() as u64));
        fs::rename(&self.path, &aside)?;
        for rel in &backups {
            let _ = fs::remove_file(join_rel(&self.backup, rel));
        }
        self.damaged = None;
        self.manifest = Manifest::default();
        self.save()?;
        log(&format!(
            "Restored {} original file(s) and removed {removed} Yakusu patch archive(s). Other files Yakusu \
             added (for example version.dll) can't be identified without the record; the damaged record was \
             kept as {}.",
            backups.len(),
            aside.display()
        ));
        Ok(())
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
