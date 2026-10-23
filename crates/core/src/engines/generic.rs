//! Engines handled by msg-tool's archive + script support.
//!
//! Each profile says how to find the game's script archive, which msg-tool
//! archive/script types to use, and the text encoding. Flow:
//!
//!   unpack archive -> export scripts -> (translate) -> import -> repack the
//!   archive with the patched scripts -> install over the original (backed up).
//!
//! Engines whose archives can be read but not rebuilt are "manual": Yakusoku
//! produces the patched script files and says where they go.

use super::{clean_dir, copy_tree, join_rel, list_files, msgtool, read_json_messages, Ctx, Engine, Entry, Match};
use crate::error::{user, Result};
use serde_json::{Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

pub struct Profile {
    pub id: &'static str,
    pub name: &'static str,
    pub archives: &'static [&'static str],
    pub loose: &'static [&'static str],
    pub markers: &'static [&'static str],
    pub archive_type: Option<&'static str>,
    pub script_type: Option<&'static str>,
    pub encoding: &'static str,
    pub support: &'static str,
    pub no_repack: bool,
    pub pack_extra: &'static [&'static str],
    pub note: &'static str,
}

const P: Profile = Profile {
    id: "",
    name: "",
    archives: &[],
    loose: &[],
    markers: &[],
    archive_type: None,
    script_type: None,
    encoding: "cp932",
    support: "experimental",
    no_repack: false,
    pack_extra: &[],
    note: "",
};

pub const PROFILES: &[Profile] = &[
    Profile {
        id: "favorite",
        name: "FAVORITE (FVP)",
        loose: &["*.hcb"],
        script_type: Some("favorite"),
        note: "Replaces the .hcb script in place. Keep the backup: old saves may not load with a rebuilt script.",
        ..P
    },
    Profile {
        id: "artemis",
        name: "Artemis",
        archives: &["*.pfs", "*.pfs.*"],
        archive_type: Some("artemis-arc"),
        encoding: "utf-8",
        ..P
    },
    Profile {
        id: "bgi",
        name: "BGI / Ethornell",
        archives: &["data0*.arc"],
        markers: &["BGI.exe", "sysprg.arc", "*.gdb"],
        archive_type: Some("bgi-arc"),
        script_type: Some("bgi"),
        ..P
    },
    Profile { id: "yuris", name: "YU-RIS", archives: &["*.ypf"], archive_type: Some("yuris-ypf"), ..P },
    Profile {
        id: "qlie",
        name: "QLiE",
        archives: &["GameData/*.pack", "*.pack"],
        archive_type: Some("qlie-pack"),
        pack_extra: &["--backslash"],
        ..P
    },
    Profile {
        id: "circus",
        name: "Circus",
        archives: &["Advdata/*.dat", "*.pck"],
        archive_type: Some("circus-pck"),
        script_type: Some("circus"),
        ..P
    },
    Profile { id: "escude", name: "Escu:de", archives: &["script.bin"], archive_type: Some("escude-arc"), ..P },
    Profile {
        id: "catsystem2",
        name: "CatSystem2",
        archives: &["scene.int", "*.int"],
        archive_type: Some("cat-system-int"),
        script_type: Some("cat-system"),
        support: "manual",
        no_repack: true,
        note: "Yakusoku can read CatSystem2 archives but can't rebuild them yet. You'll get patched .cst files to pack with CatSystem2 tools.",
        ..P
    },
    Profile {
        id: "softpal",
        name: "Softpal",
        archives: &["data.pac"],
        archive_type: Some("softpal-pac"),
        script_type: Some("softpal"),
        support: "manual",
        no_repack: true,
        note: "Yakusoku can read Softpal archives but can't rebuild them yet. You'll get patched script files.",
        ..P
    },
];

/// Engines we recognise but can't patch automatically, with what to do instead.
const KNOWN_UNSUPPORTED: &[(&[&str], &str, &str)] = &[
    (
        &["Scene.pck", "Gameexe.dat"],
        "SiglusEngine",
        "SiglusEngine isn't supported yet. No open tool can rebuild its Scene.pck reliably.",
    ),
    (
        &["Rio.arc", "Rio*.arc"],
        "WillPlus / AdvHD",
        "Extract the .ws2 scripts from Rio.arc with GARbro, then use \"Translate script files\" and point Yakusoku at that folder.",
    ),
    (
        &["renpy", "game/*.rpa"],
        "Ren'Py",
        "Unpack game/*.rpa with unrpa (or use the game's .rpy files), then use \"Translate script files\" on the game/ folder.",
    ),
    (
        &["Seen.txt", "Gameexe.ini"],
        "RealLive",
        "Use \"Translate script files\" on the folder with Seen.txt (VNTextPatch handles RealLive).",
    ),
];

pub fn profile(id: &str) -> Option<&'static Profile> {
    PROFILES.iter().find(|p| p.id == id)
}

/// Case-insensitive shell-style match (`*`, `?`).
pub fn fnmatch(name: &str, pat: &str) -> bool {
    let n: Vec<char> = name.to_lowercase().chars().collect();
    let p: Vec<char> = pat.to_lowercase().chars().collect();
    let (mut i, mut j, mut star, mut mark) = (0usize, 0usize, None::<usize>, 0usize);
    while i < n.len() {
        if j < p.len() && (p[j] == '?' || p[j] == n[i]) {
            i += 1;
            j += 1;
        } else if j < p.len() && p[j] == '*' {
            star = Some(j);
            mark = i;
            j += 1;
        } else if let Some(s) = star {
            j = s + 1;
            mark += 1;
            i = mark;
        } else {
            return false;
        }
    }
    while j < p.len() && p[j] == '*' {
        j += 1;
    }
    j == p.len()
}

fn entries(dir: &Path) -> Vec<(String, bool)> {
    let mut v: Vec<(String, bool)> = fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| {
                    (e.file_name().to_string_lossy().to_string(), e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Files in the game matching any pattern ("GameData/*.pack" walks one folder level).
pub fn glob(game_dir: &Path, patterns: &[&str]) -> Vec<String> {
    let mut hits = std::collections::BTreeSet::new();
    for pat in patterns {
        let parts: Vec<&str> = pat.split('/').collect();
        let mut base = game_dir.to_path_buf();
        let mut prefix = String::new();
        let mut ok = true;
        for part in &parts[..parts.len() - 1] {
            match entries(&base).into_iter().find(|(n, d)| *d && fnmatch(n, part)) {
                Some((n, _)) => {
                    base.push(&n);
                    prefix.push_str(&n);
                    prefix.push('/');
                }
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            continue;
        }
        for (n, is_dir) in entries(&base) {
            if !is_dir && fnmatch(&n, parts[parts.len() - 1]) {
                hits.insert(format!("{prefix}{n}"));
            }
        }
    }
    hits.into_iter().collect()
}

pub fn detect_profile(game_dir: &Path, prof: &Profile) -> Option<Match> {
    if !prof.markers.is_empty() && glob(game_dir, prof.markers).is_empty() {
        return None;
    }
    let pats = if prof.archives.is_empty() { prof.loose } else { prof.archives };
    let files = glob(game_dir, pats);
    if files.is_empty() {
        return None;
    }
    let mut d = Map::new();
    d.insert("files".into(), Value::from(files));
    d.insert("profile".into(), Value::from(prof.id));
    let conf = if prof.markers.is_empty() { 0.6 } else { 0.8 };
    Some(Match::new(prof.id, prof.name, conf, d, prof.support, prof.note))
}

/// An unsupported engine whose every marker is present (e.g. SiglusEngine's
/// Scene.pck *and* Gameexe.dat). Strong enough to outrank a loose archive glob.
pub fn detect_unsupported_strong(game_dir: &Path) -> Option<Match> {
    for (markers, name, note) in KNOWN_UNSUPPORTED {
        if markers.len() > 1 && markers.iter().all(|m| !glob(game_dir, &[m]).is_empty() || game_dir.join(m).is_dir()) {
            return Some(Match::new("unsupported", name, 0.5, Map::new(), "manual", note));
        }
    }
    None
}

pub fn detect_unsupported(game_dir: &Path) -> Option<Match> {
    for (markers, name, note) in KNOWN_UNSUPPORTED {
        if !glob(game_dir, markers).is_empty() || markers.iter().any(|m| game_dir.join(m).is_dir()) {
            return Some(Match::new("unsupported", name, 0.5, Map::new(), "manual", note));
        }
    }
    None
}

/// A folder name for one archive's work files. msg-tool strips the last
/// extension from output folders ("data.pfs" becomes "data"), so keys never
/// contain a dot: what we ask for is where the files land.
pub fn dir_key(rel: &str) -> String {
    rel.replace(['/', '\\'], "__").replace('.', "_")
}

/// The engine version stored in a YU-RIS .ypf header (needed to rebuild it).
pub fn ypf_version(path: &Path) -> Option<u32> {
    use std::io::Read;
    let mut head = [0u8; 8];
    fs::File::open(path).ok()?.read_exact(&mut head).ok()?;
    if &head[..4] != b"YPF\0" {
        return None;
    }
    let v = u32::from_le_bytes(head[4..8].try_into().ok()?);
    (234..=500).contains(&v).then_some(v)
}

/// Compare a re-export of the rebuilt scripts with what was asked for.
///
/// `original_all` / `rebuilt_all` are full exports (no ASCII filter);
/// `original` is the normal export the lines came from and `expected` what
/// was written for it. Returns (records checked, records that differ).
pub fn verify_rebuilt(
    original_all: &[Entry],
    rebuilt_all: &[Entry],
    original: &[Entry],
    expected: &[Entry],
) -> (usize, Vec<usize>) {
    let mut bad = Vec::new();
    if original_all.len() != rebuilt_all.len() {
        return (0, (0..expected.len()).collect());
    }
    let mut at = 0usize;
    for (k, (old, want)) in original.iter().zip(expected).enumerate() {
        while at < original_all.len() && original_all[at] != *old {
            at += 1;
        }
        if at >= original_all.len() {
            bad.push(k);
            continue;
        }
        let got = &rebuilt_all[at];
        if got.message != want.message || got.name != want.name {
            bad.push(k);
        }
        at += 1;
    }
    (original.len().min(expected.len()), bad)
}

pub struct MsgToolEngine {
    pub m: Match,
}

impl MsgToolEngine {
    fn prof(&self) -> &'static Profile {
        profile(self.m.str("profile").unwrap_or(&self.m.engine_id)).or_else(|| profile(&self.m.engine_id)).unwrap()
    }

    /// Extra arguments needed to rebuild `rel`.
    fn pack_args(&self, rel: &str) -> Result<Vec<String>> {
        let prof = self.prof();
        let mut extra: Vec<String> = prof.pack_extra.iter().map(|s| s.to_string()).collect();
        if prof.id == "yuris" {
            let v = self.m.details.get("ypf_versions").and_then(|m| m.get(rel)).and_then(Value::as_u64);
            match v {
                Some(v) => extra.extend(["--yuris-ypf-version".to_string(), v.to_string()]),
                None => {
                    return user(format!(
                    "Couldn't read the YU-RIS engine version from the header of {rel}, so it can't be rebuilt safely."
                ))
                }
            }
        }
        Ok(extra)
    }

    /// FVP: names and lines can share one string in the .hcb. Re-export the
    /// rebuilt script and check every record came out as written.
    fn verify_favorite(&mut self, cx: &Ctx, rel: &str, src: &Path, patched: &Path) -> Result<()> {
        let key = dir_key(rel);
        let tmp = cx.project.work("verify");
        clean_dir(&tmp)?;
        let all_flag = ["--favorite-hcb-no-filter-ascii"];
        let read_all = |dir: &Path, out: &Path| -> Result<Vec<Entry>> {
            msgtool::export_with(cx, dir, out, Some("favorite"), &all_flag)?;
            let mut v = Vec::new();
            for f in list_files(out, Some(&[".json"])) {
                v.extend(read_json_messages(&join_rel(out, &f))?);
            }
            Ok(v)
        };
        let collect = |dir: &Path| -> Result<Vec<Entry>> {
            let mut v = Vec::new();
            for f in list_files(dir, Some(&[".json"])) {
                v.extend(read_json_messages(&join_rel(dir, &f))?);
            }
            Ok(v)
        };
        let original_all = read_all(src, &tmp.join("original"))?;
        let rebuilt_all = read_all(patched, &tmp.join("rebuilt"))?;
        let original = collect(&cx.project.work("json").join(&key))?;
        let expected = collect(&cx.project.work("json_tl").join(&key))?;
        let (n, bad) = verify_rebuilt(&original_all, &rebuilt_all, &original, &expected);
        self.m.set("verify_mismatches", bad.len());
        if bad.is_empty() {
            cx.log(format!("Checked the rebuilt {rel}: all {n} text records came out as written."));
        } else {
            cx.log(format!(
                "Warning: in the rebuilt {rel}, {} of {n} text records differ from what was written (first at record {}). \
                 This usually means a name and a line share one string in the script.",
                bad.len(),
                bad[0]
            ));
        }
        let _ = fs::remove_dir_all(&tmp);
        Ok(())
    }
}

impl Engine for MsgToolEngine {
    fn m(&self) -> &Match {
        &self.m
    }
    fn m_mut(&mut self) -> &mut Match {
        &mut self.m
    }
    fn encoding(&self) -> String {
        self.prof().encoding.into()
    }
    fn straight_quotes(&self) -> bool {
        self.prof().encoding != "utf-8"
    }

    fn prepare(&mut self, cx: &Ctx) -> Result<PathBuf> {
        let prof = self.prof();
        let game = cx.game();
        let src_root = cx.project.work("scripts");
        clean_dir(&src_root)?;
        let json_root = cx.project.work("json");
        clean_dir(&json_root)?;
        let mut versions = Map::new();
        for rel in self.m.strings("files") {
            let path = join_rel(&game, &rel);
            let key = dir_key(&rel);
            let dst = src_root.join(&key);
            if prof.id == "yuris" {
                match ypf_version(&path) {
                    Some(v) => {
                        cx.log(format!("{rel}: YU-RIS archive version {v}"));
                        versions.insert(rel.clone(), Value::from(v));
                    }
                    None => cx.log(format!("{rel}: couldn't read the YU-RIS archive version; it may not rebuild.")),
                }
            }
            if let Some(at) = prof.archive_type {
                cx.log(format!("Unpacking {rel}"));
                msgtool::unpack(cx, &path, &dst, Some(at), &[])?;
            } else {
                fs::create_dir_all(&dst)?;
                fs::copy(&path, dst.join(path.file_name().unwrap()))?;
            }
            msgtool::export(cx, &dst, &json_root.join(&key), prof.script_type)?;
        }
        if prof.id == "yuris" {
            self.m.set("ypf_versions", Value::Object(versions));
        }
        if list_files(&json_root, Some(&[".json"])).is_empty() {
            return user(format!(
                "No text was found in the {} scripts. The game may use a variant msg-tool doesn't read yet.",
                prof.name
            ));
        }
        Ok(json_root)
    }

    fn build(&mut self, cx: &Ctx) -> Result<PathBuf> {
        let prof = self.prof();
        let src_root = cx.project.work("scripts");
        let out = cx.project.work("out");
        clean_dir(&out)?;
        let manual = cx.project.work("manual");
        for rel in self.m.strings("files") {
            let key = dir_key(&rel);
            let src = src_root.join(&key);
            let json_dir = cx.project.work("json_tl").join(&key);
            if !json_dir.is_dir() {
                continue;
            }
            let patched = cx.project.work("patched").join(&key);
            clean_dir(&patched)?;
            msgtool::import(cx, &src, &json_dir, &patched, prof.script_type)?;
            if prof.id == "favorite" {
                self.verify_favorite(cx, &rel, &src, &patched)?;
            }
            match prof.archive_type {
                Some(at) if !prof.no_repack => {
                    // Rebuild the archive: original contents with patched scripts on top.
                    let extra = self.pack_args(&rel)?;
                    let stage = cx.project.work("repack").join(&key);
                    clean_dir(&stage)?;
                    copy_tree(&src, &stage)?;
                    copy_tree(&patched, &stage)?;
                    msgtool::pack(cx, &stage, &join_rel(&out, &rel), at, &extra)?;
                    cx.log(format!("Rebuilt {rel}"));
                }
                Some(_) => {
                    let dst = manual.join(&key);
                    clean_dir(&dst)?;
                    copy_tree(&patched, &dst)?;
                    cx.log(format!("Patched scripts for {rel} are in {}", dst.display()));
                }
                None => {
                    let dir = rel.rsplit_once('/').map(|(d, _)| d.to_string());
                    for f in list_files(&patched, None) {
                        let base = f.rsplit('/').next().unwrap_or(&f);
                        let target = match &dir {
                            Some(d) => format!("{d}/{base}"),
                            None => f.clone(),
                        };
                        let dst = join_rel(&out, &target);
                        fs::create_dir_all(dst.parent().unwrap())?;
                        fs::copy(join_rel(&patched, &f), dst)?;
                    }
                }
            }
        }
        if prof.no_repack {
            self.m.set("manual_output", manual.to_string_lossy().to_string());
        }
        Ok(out)
    }
}
