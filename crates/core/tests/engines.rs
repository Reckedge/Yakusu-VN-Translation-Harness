//! msg-tool engines and script-folder mode, driven through a fake msg-tool.

mod common;
use common::*;
use std::io::{Read, Write};
use std::sync::Arc;
use yakusu_core::engines::{self, manual};
use yakusu_core::pipeline::Pipeline;
use yakusu_core::project::Project;

const SCRIPT: &str = "桜|「おはよう、大雅くん！」\n|春の朝。窓の外では、桜が散っていた。\\n今日から新しい学校だ。\n|bg_room.png\n大雅|「ああ、おはよう」\n";

fn zip_write(path: &std::path::Path, files: &[(&str, &[u8])]) {
    let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    for (name, data) in files {
        z.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
        z.write_all(data).unwrap();
    }
    z.finish().unwrap();
}

fn zip_read(path: &std::path::Path, name: &str) -> Vec<u8> {
    let mut z = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut out = Vec::new();
    z.by_name(name).unwrap().read_to_end(&mut out).unwrap();
    out
}

macro_rules! need_tool {
    ($tmp:expr) => {
        if fake_msgtool($tmp).is_none() {
            eprintln!("python3 not available; skipping");
            return;
        }
    };
}

#[test]
fn archive_engine_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    need_tool!(tmp.path());
    let env = env(tmp.path());
    let game = tmp.path().join("ArtemisGame");
    std::fs::create_dir_all(&game).unwrap();
    zip_write(&game.join("data.pfs"), &[("ysbin/yst00001.scr", SCRIPT.as_bytes()), ("ysbin/image.bin", b"\x00\x01")]);
    std::fs::write(game.join("game.exe"), b"MZ").unwrap();
    let orig = sha(&game.join("data.pfs"));

    let m = engines::detect(&game).unwrap();
    assert_eq!((m[0].engine_id.as_str(), m[0].support.as_str()), ("artemis", "experimental"));
    let pipe = pipeline(&env, &game, settings("quick", 10, 2), fake().0);
    let st = pipe.extract().unwrap();
    assert_eq!((st.lines, st.skipped), (3, 1)); // the .png line is skipped
    pipe.translate(None, false).unwrap();
    pipe.build().unwrap();
    pipe.install().unwrap();
    assert_eq!(zip_read(&game.join("data.pfs"), "ysbin/image.bin"), b"\x00\x01"); // untouched content survives
    let text = String::from_utf8(zip_read(&game.join("data.pfs"), "ysbin/yst00001.scr")).unwrap();
    assert!(!text.contains("桜|") && !text.contains("おはよう"));
    assert!(text.contains("|bg_room.png")); // skipped line left alone
    assert_eq!(sha(&pipe.p.path("backup").join("data.pfs")), orig);
    pipe.restore().unwrap();
    assert_eq!(sha(&game.join("data.pfs")), orig);
}

#[test]
fn names_translated_in_name_field() {
    let tmp = tempfile::tempdir().unwrap();
    need_tool!(tmp.path());
    let env = env(tmp.path());
    let game = tmp.path().join("G2");
    std::fs::create_dir_all(&game).unwrap();
    zip_write(&game.join("data.pfs"), &[("a.scr", SCRIPT.as_bytes())]);
    let pipe = pipeline(&env, &game, settings("quick", 10, 2), fake().0);
    pipe.extract().unwrap();
    pipe.draft_style().unwrap();
    pipe.translate(None, false).unwrap();
    pipe.build().unwrap();
    let out: serde_json::Value =
        serde_json::from_slice(&std::fs::read(pipe.p.work("json_tl/data_pfs/a.json")).unwrap()).unwrap();
    assert_eq!(out[0]["name"], "Sakura");
    assert_eq!(out[3]["name"], "Taiga");
}

#[test]
fn script_folder_mode_inside_game() {
    let tmp = tempfile::tempdir().unwrap();
    need_tool!(tmp.path());
    let env = env(tmp.path());
    let game = tmp.path().join("RenGame");
    std::fs::create_dir_all(game.join("game")).unwrap();
    std::fs::write(game.join("game/script.scr"), SCRIPT).unwrap();
    let orig = sha(&game.join("game/script.scr"));
    let p = Arc::new(Project::for_game(&env, &game).unwrap());
    let m = manual::for_folder(&game.join("game").to_string_lossy(), &game);
    assert!(m.flag("inside_game") && m.support == "auto");
    p.update_meta(|meta| meta.engine = Some(m)).unwrap();
    let pipe = Pipeline::new(env.clone(), p, settings("quick", 10, 2), fake().0);
    pipe.extract().unwrap();
    pipe.translate(None, false).unwrap();
    pipe.build().unwrap();
    pipe.install().unwrap();
    assert!(!std::fs::read_to_string(game.join("game/script.scr")).unwrap().contains("おはよう"));
    pipe.restore().unwrap();
    assert_eq!(sha(&game.join("game/script.scr")), orig);
}

#[test]
fn script_folder_outside_game_is_manual() {
    let tmp = tempfile::tempdir().unwrap();
    need_tool!(tmp.path());
    let env = env(tmp.path());
    let game = tmp.path().join("G3");
    std::fs::create_dir_all(&game).unwrap();
    let ext = tmp.path().join("extracted");
    std::fs::create_dir_all(&ext).unwrap();
    std::fs::write(ext.join("s.scr"), SCRIPT).unwrap();
    let p = Arc::new(Project::for_game(&env, &game).unwrap());
    p.update_meta(|meta| meta.engine = Some(manual::for_folder(&ext.to_string_lossy(), &game))).unwrap();
    let pipe = Pipeline::new(env.clone(), p, settings("quick", 10, 2), fake().0);
    pipe.extract().unwrap();
    pipe.translate(None, false).unwrap();
    let res = pipe.build().unwrap();
    let manual_out = res.manual_output.expect("manual output");
    assert!(std::path::Path::new(&manual_out).join("s.scr").exists());
    assert!(pipe.install().is_err());
}

#[test]
fn unsupported_engines_are_explained() {
    let tmp = tempfile::tempdir().unwrap();
    let game = tmp.path().join("Siglus");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::write(game.join("Scene.pck"), b"").unwrap();
    std::fs::write(game.join("Gameexe.dat"), b"").unwrap();
    let m = engines::detect(&game).unwrap();
    assert_eq!(m[0].engine_id, "unsupported");
    assert!(m[0].note.contains("SiglusEngine"));
    assert!(engines::engine_for(&m[0]).is_err());
    assert!(engines::detect(&tmp.path().join("nope")).is_err());
}
