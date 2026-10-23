//! Kirikiri / KiriKiri Z (KAG). The most common VN engine, fully automatic.
//!
//! Out: read the .xp3 archives (msg-tool decrypts ~500 known encrypted games),
//!      descramble .ks files, parse KAG ourselves (.scn goes through msg-tool).
//! In:  write translations into the scripts, pack only the changed ones into a
//!      new patchN.xp3 next to the game. The originals are never touched.
//!      For encrypted games we also drop in KirikiriUnencryptedArchive's
//!      version.dll so the game accepts our unencrypted patch.

use super::{
    clean_dir, join_rel, ks, list_files, msgtool, read_json_messages, write_json_messages, xp3, Ctx, Engine, Installer,
    Match,
};
use crate::error::{user, Error, Result};
use regex::Regex;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

const SCRIPT_EXTS: &[&str] = &[".ks", ".scn"];
/// A small file inside every patch.xp3 Yakusu builds.
pub const PATCH_MARKER: &str = "yakusu-patch.json";
/// Archives that only ever hold images, audio or video: never worth decrypting.
static MEDIA_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(voice|bgm|se|sound|sysse|image|bgimage|fgimage|evimage|video|movie|fg|bg|ev|chara|face|thum)\d*\.xp3$",
    )
    .unwrap()
});
static PATCH_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^patch(\d*)\.xp3$").unwrap());

fn is_script(name: &str) -> bool {
    let n = name.to_lowercase();
    SCRIPT_EXTS.iter().any(|e| n.ends_with(e))
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
pub struct Xp3Game {
    pub name: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub tpm: String,
}

pub static XP3_GAMES: LazyLock<Vec<Xp3Game>> =
    LazyLock::new(|| serde_json::from_str(include_str!("../../data/xp3_games.json")).unwrap_or_default());

/// difflib.SequenceMatcher-style similarity (Ratcliff/Obershelp), 0..1.
pub fn similarity(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    fn matched(a: &[char], b: &[char]) -> usize {
        if a.is_empty() || b.is_empty() {
            return 0;
        }
        // Longest common substring.
        let (mut best, mut bi, mut bj) = (0usize, 0usize, 0usize);
        let mut prev = vec![0usize; b.len() + 1];
        for i in 1..=a.len() {
            let mut cur = vec![0usize; b.len() + 1];
            for j in 1..=b.len() {
                if a[i - 1] == b[j - 1] {
                    cur[j] = prev[j - 1] + 1;
                    if cur[j] > best {
                        best = cur[j];
                        bi = i - best;
                        bj = j - best;
                    }
                }
            }
            prev = cur;
        }
        if best == 0 {
            return 0;
        }
        best + matched(&a[..bi], &b[..bj]) + matched(&a[bi + best..], &b[bj + best..])
    }
    2.0 * matched(&a, &b) as f64 / (a.len() + b.len()) as f64
}

/// Pick the msg-tool decryption profile for an encrypted game.
///
/// Best signal: the game ships the .tpm plugin named in the profile.
/// Otherwise fuzzy-match the folder and exe names against known titles.
pub fn guess_game_title(game_dir: &Path) -> Option<(String, &'static str)> {
    for g in XP3_GAMES.iter() {
        if !g.tpm.is_empty() && join_rel(game_dir, &g.tpm).exists() {
            return Some((g.name.clone(), "plugin"));
        }
    }
    let mut words = vec![game_dir.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()];
    if let Ok(rd) = fs::read_dir(game_dir) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.to_lowercase().ends_with(".exe") {
                words.push(n[..n.len() - 4].to_string());
            }
        }
    }
    let (mut best, mut score) = (None, 0.0);
    for g in XP3_GAMES.iter() {
        let labels =
            std::iter::once(g.name.as_str()).chain(g.title.split('|').map(str::trim).filter(|t| !t.is_empty()));
        for label in labels {
            for w in &words {
                let s = similarity(&w.to_lowercase(), &label.to_lowercase());
                if s > score {
                    best = Some(g.name.clone());
                    score = s;
                }
            }
        }
    }
    if score >= 0.72 {
        best.map(|b| (b, "name"))
    } else {
        None
    }
}

fn dir_names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

pub fn detect(game_dir: &Path) -> Option<Match> {
    let archives: Vec<String> =
        dir_names(game_dir).into_iter().filter(|f| f.to_lowercase().ends_with(".xp3")).collect();
    if archives.is_empty() {
        return None;
    }
    let mut d = Map::new();
    d.insert("archives".into(), Value::from(archives));
    Some(Match::new("kirikiri", "Kirikiri (KAG)", 0.95, d, "auto", ""))
}

pub struct Kirikiri {
    pub m: Match,
}

impl Kirikiri {
    fn source_archives(&self, cx: &Ctx) -> Vec<String> {
        let added: BTreeSet<String> = Installer::new(cx.project).manifest.added.into_iter().collect();
        let mut out: Vec<String> = dir_names(&cx.game())
            .into_iter()
            .filter(|f| f.to_lowercase().ends_with(".xp3") && !added.contains(f))
            .collect();
        // data.xp3 / scenario archives first; patches last so they override.
        out.sort_by_key(|name| {
            let n = name.to_lowercase();
            if let Some(rest) = n.strip_prefix("patch") {
                let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                (2, num.parse::<u32>().unwrap_or(1), n)
            } else {
                (if n.contains("scen") || n == "data.xp3" { 0 } else { 1 }, 0, n)
            }
        });
        out
    }

    fn save_script(root: &Path, rel: &str, data: Vec<u8>) -> Result<()> {
        let data = match xp3::descramble(&data) {
            // Descrambled scripts work as-is (UTF-16LE with BOM).
            Some(text) => ks::encode(&text, "utf-16"),
            None => data,
        };
        let dst = join_rel(root, rel);
        fs::create_dir_all(dst.parent().unwrap())?;
        fs::write(dst, data)?;
        Ok(())
    }

    fn patch_name(&self, cx: &Ctx) -> String {
        for a in Installer::new(cx.project).manifest.added {
            if PATCH_RE.is_match(&a) {
                return a;
            }
        }
        let existing: BTreeSet<String> = dir_names(&cx.game()).into_iter().map(|f| f.to_lowercase()).collect();
        if !existing.contains("patch.xp3") {
            return "patch.xp3".into();
        }
        let mut n = 2;
        while existing.contains(&format!("patch{n}.xp3")) {
            n += 1;
        }
        format!("patch{n}.xp3")
    }
}

impl Engine for Kirikiri {
    fn m(&self) -> &Match {
        &self.m
    }
    fn m_mut(&mut self) -> &mut Match {
        &mut self.m
    }
    fn encoding(&self) -> String {
        if self.m.strings("ks_encodings").iter().any(|e| e == "cp932") {
            "cp932".into()
        } else {
            "utf-16".into()
        }
    }
    fn line_break_for(&self, rel: &str) -> &'static str {
        if rel.replace('\\', "/").starts_with("ks/") {
            "[r]"
        } else {
            "\n"
        }
    }
    fn straight_quotes(&self) -> bool {
        false
    }

    fn prepare(&mut self, cx: &Ctx) -> Result<PathBuf> {
        let game = cx.game();
        let scripts_dir = cx.project.work("scripts");
        clean_dir(&scripts_dir)?;
        let mut title = self.m.str("xp3_title").map(String::from);
        let mut encrypted = false;
        let mut found = 0usize;
        let mut origin: BTreeMap<String, String> = BTreeMap::new();
        for arc in self.source_archives(cx) {
            let path = game.join(&arc);
            let entries = xp3::read_index(&path).ok();
            if entries.is_none() && MEDIA_RE.is_match(&arc) {
                cx.log(format!("Skipping {arc} (media archive)"));
                continue;
            }
            if let Some(entries) = &entries {
                if entries.iter().any(|e| e.name == PATCH_MARKER) {
                    // A patch Yakusu made earlier (perhaps before its data folder
                    // was reset): never read our own English back as the source.
                    cx.log(format!("Skipping {arc} (a Yakusu patch, not the original game)"));
                    continue;
                }
                let has_scripts = entries.iter().any(|e| is_script(&e.name));
                let hashed = !entries.is_empty() && !entries.iter().take(50).any(|e| e.name.contains('.'));
                if !has_scripts && !hashed {
                    continue; // images, voices, ...
                }
                // Can we read the scripts ourselves? Then the archive isn't encrypted.
                if has_scripts {
                    let sample = entries.iter().find(|e| is_script(&e.name)).unwrap();
                    let readable = xp3::read_file(&path, sample)
                        .map(|d| xp3::looks_like_text(&d) || sample.name.to_lowercase().ends_with(".scn"))
                        .unwrap_or(false);
                    if readable && sample.encrypted_flag == 0 {
                        cx.log(format!("Reading {arc} ({} files)", entries.len()));
                        for e in entries.iter().filter(|e| is_script(&e.name)) {
                            let data = xp3::read_file(&path, e)?;
                            let rel = e.name.replace('\\', "/");
                            Self::save_script(&scripts_dir, &rel, data)?;
                            origin.insert(rel, arc.clone());
                            found += 1;
                        }
                        continue;
                    }
                }
            }
            // Encrypted (or unreadable index): let msg-tool decrypt it.
            encrypted = true;
            if title.is_none() {
                match guess_game_title(&game) {
                    Some((t, how)) => {
                        cx.log(format!(
                            "Encrypted archive. Using the decryption profile for \"{t}\" (matched by {how})."
                        ));
                        self.m.set("xp3_title", t.clone());
                        title = Some(t);
                    }
                    None => {
                        return user(format!(
                            "{arc} is encrypted and Yakusu couldn't tell which game this is. \
                             Pick the game from the list on the Game step and Yakusu will read the scripts again."
                        ))
                    }
                }
            }
            // No dot in the folder name: msg-tool strips the extension from it.
            let tmp = cx.project.work(&format!("unpack/{}", super::generic::dir_key(&arc)));
            clean_dir(&tmp)?;
            cx.log(format!("Decrypting {arc} with msg-tool"));
            msgtool::unpack(cx, &path, &tmp, None, &["--xp3-game-title".into(), title.clone().unwrap()])?;
            for rel in list_files(&tmp, None) {
                if is_script(&rel) {
                    let data = fs::read(join_rel(&tmp, &rel))?;
                    Self::save_script(&scripts_dir, &rel, data)?;
                    origin.insert(rel, arc.clone());
                    found += 1;
                }
            }
            let _ = fs::remove_dir_all(&tmp);
        }
        if found == 0 {
            return user("No Kirikiri scenario scripts (.ks / .scn) were found in this game's .xp3 archives.");
        }
        self.m.set("encrypted", encrypted);
        self.m.set("script_origin", serde_json::to_value(&origin)?);
        cx.log(format!("Found {found} script files"));
        let json_dir = cx.project.work("json");
        clean_dir(&json_dir)?;
        let mut encodings = BTreeSet::new();
        let mut scn = Vec::new();
        for rel in list_files(&scripts_dir, None) {
            if rel.to_lowercase().ends_with(".scn") {
                scn.push(rel);
                continue;
            }
            let (text, enc) = ks::decode(&fs::read(join_rel(&scripts_dir, &rel))?);
            encodings.insert(enc.to_string());
            let entries = ks::export_entries(&text);
            if !entries.is_empty() {
                write_json_messages(&join_rel(&json_dir, &format!("ks/{rel}.json")), &entries)?;
            }
        }
        if !scn.is_empty() {
            // Binary scene files (KiriKiri Z / E-mote): msg-tool handles these.
            let scn_src = cx.project.work("scripts_scn");
            clean_dir(&scn_src)?;
            for rel in &scn {
                let dst = join_rel(&scn_src, rel);
                fs::create_dir_all(dst.parent().unwrap())?;
                fs::copy(join_rel(&scripts_dir, rel), dst)?;
            }
            msgtool::export(cx, &scn_src, &json_dir.join("scn"), Some("kirikiri-scn"))?;
        }
        self.m.set("ks_encodings", encodings.into_iter().collect::<Vec<_>>());
        Ok(json_dir)
    }

    fn build(&mut self, cx: &Ctx) -> Result<PathBuf> {
        let scripts = cx.project.work("scripts");
        let patched = cx.project.work("patched");
        clean_dir(&patched)?;
        let tl = cx.project.work("json_tl");
        for jrel in list_files(&tl.join("ks"), Some(&[".json"])) {
            let rel = &jrel[..jrel.len() - 5];
            let (text, enc) = ks::decode(&fs::read(join_rel(&scripts, rel))?);
            let new = ks::apply_entries(&text, &read_json_messages(&join_rel(&tl.join("ks"), &jrel))?)?;
            let dst = join_rel(&patched, rel);
            fs::create_dir_all(dst.parent().unwrap())?;
            fs::write(dst, ks::encode(&new, enc))?;
        }
        if tl.join("scn").is_dir() {
            msgtool::import(cx, &cx.project.work("scripts_scn"), &tl.join("scn"), &patched, Some("kirikiri-scn"))?;
        }
        let mut changed = Vec::new();
        for rel in list_files(&patched, None) {
            if !is_script(&rel) {
                continue;
            }
            let a = fs::read(join_rel(&patched, &rel))?;
            if fs::read(join_rel(&scripts, &rel)).map(|b| b != a).unwrap_or(true) {
                changed.push(rel);
            }
        }
        if changed.is_empty() {
            return user("No translated lines to pack yet.");
        }
        // Stage every changed script at its archive path and by bare name, so
        // it overrides the original whichever way the game looks it up. Bare
        // names only when unique: two scripts with the same file name in
        // different folders must not overwrite each other.
        let stage = cx.project.work("patchroot");
        clean_dir(&stage)?;
        let base = |r: &str| r.rsplit('/').next().unwrap_or(r).to_string();
        let mut base_count: HashMap<String, usize> = HashMap::new();
        for rel in list_files(&scripts, None) {
            *base_count.entry(base(&rel).to_lowercase()).or_default() += 1;
        }
        let mut files = BTreeSet::new();
        for rel in &changed {
            let src = join_rel(&patched, rel);
            let mut names = vec![rel.clone()];
            let b = base(rel);
            if base_count.get(&b.to_lowercase()) == Some(&1) && b != *rel {
                names.push(b);
            }
            for name in names {
                let dst = join_rel(&stage, &name);
                fs::create_dir_all(dst.parent().unwrap())?;
                fs::copy(&src, dst)?;
                files.insert(name);
            }
        }
        let out = cx.project.work("out");
        clean_dir(&out)?;
        let patch_name = self.patch_name(cx);
        // Mark the archive as ours, so reading the game again never mistakes
        // it for the original scripts.
        let marker =
            serde_json::json!({"made_by": "Yakusu", "version": env!("CARGO_PKG_VERSION"), "scripts": changed.len()});
        fs::write(stage.join(PATCH_MARKER), serde_json::to_vec_pretty(&marker)?)?;
        files.insert(PATCH_MARKER.to_string());
        xp3::write_archive(&stage, &out.join(&patch_name), &files.into_iter().collect::<Vec<_>>())?;
        cx.log(format!("Built {patch_name} with {} translated scripts", changed.len()));
        if self.m.flag("encrypted") {
            let dll = cx.env.tools.join("kirikiri").join("version.dll");
            if !dll.is_file() {
                return Err(Error::User(
                    "This game is encrypted and needs version.dll from KirikiriTools, which is missing from the tools folder."
                        .into(),
                ));
            }
            let ours = Installer::new(cx.project).manifest.added.iter().any(|a| a == "version.dll");
            if cx.game().join("version.dll").exists() && !ours {
                cx.log("Warning: the game already has a version.dll; it will be backed up and replaced.");
            }
            fs::copy(dll, out.join("version.dll"))?;
        }
        Ok(out)
    }
}
