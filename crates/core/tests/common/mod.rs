#![allow(dead_code)]
//! Test helpers: a fake Kirikiri game, a fake msg-tool, and the stand-in model.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use yakusoku_core::config::{Env, Settings};
use yakusoku_core::engines::{self, xp3};
use yakusoku_core::llm::{self, ClientFactory};
use yakusoku_core::pipeline::Pipeline;
use yakusoku_core::project::Project;

pub const FIRST_KS: &str = "*start|プロローグ
[cm]
@bg storage=\"bg_room\"
春の朝。窓の外では、桜が静かに散っていた。[l][r]
今日から新しい学校に通う。[p]
[cm]
@chara name=\"sakura\"
【桜】
「おはよう、大雅くん！[l]今日もいい天気だね」[p]
【大雅】
「……ああ、おはよう」[p]
僕は[ruby text=\"ねむ\"]眠い目をこすりながら、彼女の後を追った。そして駅に向かって長い坂道を歩いていった。[p]
[iscript]
f.flag = \"未使用\";
[endscript]
*choice
[link target=*a]一緒に行く[endlink][r]
[link target=*b]先に行く[endlink][s]
";

pub const SECOND_KS: &str = "*a
@playse storage=\"door\"
【桜】「じゃあ、一緒に行こう！」[p]
二人で歩く道は、いつもより短く感じた。[p]
";

/// A tiny Kirikiri game: data.xp3 with two scenario files, plus an exe.
pub fn make_game(root: &Path, second: &str) -> PathBuf {
    std::fs::create_dir_all(root).unwrap();
    let src = root.join("_src");
    std::fs::create_dir_all(src.join("scenario")).unwrap();
    std::fs::create_dir_all(src.join("image")).unwrap();
    let mut first = vec![0xFF, 0xFE];
    for u in FIRST_KS.encode_utf16() {
        first.extend_from_slice(&u.to_le_bytes());
    }
    std::fs::write(src.join("scenario/first.ks"), first).unwrap();
    std::fs::write(src.join("scenario/second.ks"), encoding_rs::SHIFT_JIS.encode(second).0).unwrap();
    std::fs::write(src.join("image/bg_room.png"), b"\x89PNG fake").unwrap();
    let files = engines::list_files(&src, None);
    xp3::write_archive(&src, &root.join("data.xp3"), &files).unwrap();
    std::fs::remove_dir_all(&src).unwrap();
    let mut exe = b"MZ".to_vec();
    exe.extend(vec![0u8; 2048]);
    std::fs::write(root.join("game.exe"), exe).unwrap();
    root.to_path_buf()
}

pub type Calls = Arc<Mutex<Vec<(String, String)>>>;

pub fn fake() -> (ClientFactory, Calls) {
    let calls: Calls = Default::default();
    (llm::fake::factory(calls.clone(), None, std::time::Duration::ZERO), calls)
}

pub fn fake_with(hook: llm::fake::Hook) -> ClientFactory {
    llm::fake::factory(Default::default(), Some(Arc::new(hook)), std::time::Duration::ZERO)
}

pub fn settings(preset: &str, batch: usize, parallel: usize) -> Settings {
    let mut s = Settings {
        provider: "anthropic".into(),
        preset: preset.into(),
        batch_size: batch,
        parallel,
        context_lines: 2,
        ..Default::default()
    };
    s.keys.insert("anthropic".into(), "test".into());
    s
}

pub fn env(tmp: &Path) -> Env {
    Env::new(tmp.join("home"), tmp.join("tools"))
}

/// A pipeline over `game`, with the first detected engine.
pub fn pipeline(env: &Env, game: &Path, s: Settings, factory: ClientFactory) -> Pipeline {
    let p = Arc::new(Project::for_game(env, game).unwrap());
    let m = engines::detect(game).unwrap().remove(0);
    p.update_meta(|meta| meta.engine = Some(m)).unwrap();
    Pipeline::new(env.clone(), p, s, factory)
}

pub fn sha(p: &Path) -> String {
    sha1_smol::Sha1::from(std::fs::read(p).unwrap()).digest().to_string()
}

/// A stand-in for msg-tool's command line (Python script + sh wrapper).
/// Returns None when python3 isn't available.
pub fn fake_msgtool(tmp: &Path) -> Option<PathBuf> {
    // The stand-in is a shell script; Windows can't run it as msg-tool.exe.
    if cfg!(windows) {
        return None;
    }
    if std::process::Command::new("python3").arg("--version").output().is_err() {
        return None;
    }
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_msgtool.py");
    // Env::tool_path looks in the tools folder first.
    let bin = tmp.join("tools");
    std::fs::create_dir_all(&bin).unwrap();
    let tool = bin.join("msg-tool");
    std::fs::write(&tool, format!("#!/bin/sh\nexec python3 \"{}\" \"$@\"\n", script.display())).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    Some(tool)
}
