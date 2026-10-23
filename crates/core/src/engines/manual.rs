//! "Translate script files" mode, for engines Yakusu can't unpack itself.
//!
//! You point Yakusu at a folder of already-extracted script files (from
//! GARbro, unrpa, or the game folder itself for Ren'Py .rpy). msg-tool is
//! tried first, then VNTextPatch (arcusmaximus, MIT), which covers ~25
//! engines and handles Shift-JIS "tunneling" for characters SJIS can't show.
//!
//! If the folder is inside the game, the patched files are installed back in
//! place (with backups). Otherwise they're written to an output folder.

use super::{clean_dir, copy_tree, join_rel, list_files, msgtool, run_tool, Ctx, Engine, Match};
use crate::error::{user, Result};
use crate::project::{abs_path, path_within};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// extension -> engine label (VNTextPatch detects the format itself).
const VNTP_FORMATS: &[(&str, &str)] = &[
    (".ws2", "AdvHD"),
    (".bin", "ArcGameEngine"),
    (".asb", "Artemis"),
    (".ast", "Artemis"),
    (".cst", "CatSystem2"),
    (".a0", "Cyberworks"),
    (".ks", "Kirikiri"),
    (".scn", "Kirikiri"),
    (".mjo", "Majiro"),
    (".sc", "Musica"),
    (".nut", "Mware"),
    (".msc", "Propeller"),
    (".rpy", "Ren'Py"),
    (".hst", "ShSystem"),
    (".mes", "Silky's"),
    (".s", "QLiE"),
    (".nnn", "SystemNNN"),
    (".spt", "SystemNNN"),
    (".srp", "TmrHiroAdvSystem"),
    (".ybn", "YU-RIS"),
    (".src", "Softpal"),
];

pub fn for_folder(folder: &str, game_dir: &Path) -> Match {
    let folder = abs_path(folder);
    let mut exts: HashMap<String, usize> = HashMap::new();
    for rel in list_files(&folder, None) {
        if let Some(i) = rel.rfind('.') {
            *exts.entry(rel[i..].to_lowercase()).or_default() += 1;
        }
    }
    let mut by_count: Vec<(String, usize)> = exts.into_iter().collect();
    by_count.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let guess = by_count.iter().find_map(|(e, _)| VNTP_FORMATS.iter().find(|(x, _)| x == e).map(|(_, l)| *l));
    // The game root itself counts: Ren'Py .rpy files can sit right there.
    let inside = path_within(&folder, game_dir);
    let mut d = Map::new();
    d.insert("folder".into(), Value::from(folder.to_string_lossy().to_string()));
    d.insert("guess".into(), guess.map(Value::from).unwrap_or(Value::Null));
    d.insert("inside_game".into(), Value::from(inside));
    d.insert("vntp_format".into(), Value::Null);
    let name = match guess {
        Some(g) => format!("Script files ({g})"),
        None => "Script files".into(),
    };
    Match::new("scripts", &name, 0.5, d, if inside { "auto" } else { "manual" }, "")
}

pub struct ScriptFolder {
    pub m: Match,
}

impl ScriptFolder {
    fn folder(&self) -> Option<PathBuf> {
        self.m.str("folder").map(PathBuf::from)
    }
}

impl Engine for ScriptFolder {
    fn m(&self) -> &Match {
        &self.m
    }
    fn m_mut(&mut self) -> &mut Match {
        &mut self.m
    }
    fn straight_quotes(&self) -> bool {
        !matches!(self.m.str("guess"), Some("Ren'Py" | "Kirikiri" | "Artemis"))
    }

    fn prepare(&mut self, cx: &Ctx) -> Result<PathBuf> {
        let Some(folder) = self.folder().filter(|f| f.is_dir()) else {
            return user("Choose the folder that holds the extracted script files.");
        };
        let files = list_files(&folder, None);
        let size: u64 = files.iter().filter_map(|r| fs::metadata(join_rel(&folder, r)).ok()).map(|m| m.len()).sum();
        if files.len() > 5000 || size > 400 * 1024 * 1024 {
            return user(format!(
                "That folder has {} files ({} MB). Pick the folder that holds only the extracted script files.",
                files.len(),
                size / 1_048_576
            ));
        }
        let src = cx.project.work("scripts");
        clean_dir(&src)?;
        copy_tree(&folder, &src)?;
        let json_dir = cx.project.work("json");
        clean_dir(&json_dir)?;
        let mut backend = None;
        match msgtool::export(cx, &src, &json_dir, None) {
            Ok(()) if !list_files(&json_dir, Some(&[".json"])).is_empty() => backend = Some("msg-tool"),
            Ok(()) => {}
            Err(e) => cx.log(format!("msg-tool: {e}")),
        }
        if backend.is_none() {
            let Some(vntp) = cx.env.tool_path("VNTextPatch") else {
                return user(
                    "msg-tool couldn't read these files, and VNTextPatch isn't available (it only runs on Windows). \
                     Check that the folder holds only script files.",
                );
            };
            clean_dir(&json_dir)?;
            let mut args = vec![
                vntp.to_string_lossy().to_string(),
                "extractlocal".into(),
                src.to_string_lossy().to_string(),
                json_dir.to_string_lossy().to_string(),
            ];
            if let Some(f) = self.m.str("vntp_format") {
                args.push(format!("--format={f}"));
            }
            let (code, _) = run_tool(&args, cx)?;
            if code != 0 || list_files(&json_dir, Some(&[".json"])).is_empty() {
                return user(
                    "Neither msg-tool nor VNTextPatch could read these files. The folder should contain only the scenario scripts.",
                );
            }
            backend = Some("vntextpatch");
        }
        let backend = backend.unwrap();
        self.m.set("backend", backend);
        cx.log(format!("Read scripts with {backend}"));
        Ok(json_dir)
    }

    fn build(&mut self, cx: &Ctx) -> Result<PathBuf> {
        let src = cx.project.work("scripts");
        let patched = cx.project.work("patched");
        clean_dir(&patched)?;
        let tl = cx.project.work("json_tl");
        if self.m.str("backend") == Some("vntextpatch") {
            let Some(vntp) = cx.env.tool_path("VNTextPatch") else {
                return user("VNTextPatch is missing from the tools folder.");
            };
            let mut args = vec![vntp.to_string_lossy().to_string(), "insertlocal".into()];
            args.extend([&src, &tl, &patched].iter().map(|p| p.to_string_lossy().to_string()));
            if let Some(f) = self.m.str("vntp_format") {
                args.push(format!("--format={f}"));
            }
            let (code, _) = run_tool(&args, cx)?;
            if code != 0 {
                return user("VNTextPatch could not write the translated scripts. See the log for details.");
            }
        } else {
            msgtool::import(cx, &src, &tl, &patched, None)?;
        }
        let out = cx.project.work("out");
        clean_dir(&out)?;
        let folder = self.folder().unwrap_or_default();
        if self.m.flag("inside_game") {
            let game = cx.game();
            let rel_base = folder.strip_prefix(&game).map(Path::to_path_buf).unwrap_or_else(|_| {
                // Same folder spelled differently (case on Windows): compare by components.
                let n = game.components().count();
                folder.components().skip(n).collect()
            });
            copy_tree(&patched, &out.join(rel_base))?;
        } else {
            self.m.set("manual_output", patched.to_string_lossy().to_string());
        }
        Ok(out)
    }
}
