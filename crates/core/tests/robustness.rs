//! Regression tests for the October 2026 stress report on the Python version:
//! crash recovery, install rollback, detection, code order, odd file names,
//! shared speaker strings, and large scripts.

mod common;
use common::*;
use std::sync::Arc;
use yakusu_core::engines::{self, generic, ks, manual, xp3, Entry, Installer};
use yakusu_core::pipeline::{Pipeline, SPEAKER_SKIP};
use yakusu_core::project::{Project, ResultRow};
use yakusu_core::protect::{check, protect};

fn row(id: &str, en: &str) -> ResultRow {
    ResultRow { id: id.into(), en_ph: en.into(), stage: "translate".into(), ..Default::default() }
}

#[test]
fn torn_results_tail_does_not_lose_the_next_batch() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("p");
    Project::open(root.clone()).unwrap().add_results(vec![row("before", "Before")]).unwrap();
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(root.join("results.jsonl"))
        .unwrap()
        .write_all(br#"{"id":"interrupted","en_ph":"par"#)
        .unwrap();
    let p = Project::open(root.clone()).unwrap();
    assert!(p.result("before").is_some());
    p.add_results(vec![row("after", "After")]).unwrap();
    let again = Project::open(root).unwrap();
    assert!(again.result("after").is_some(), "a batch saved after a crash must survive the next restart");
    assert!(again.result("before").is_some());
}

#[test]
fn torn_cost_tail_does_not_erase_new_spending() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("p");
    let u = Default::default();
    Project::open(root.clone()).unwrap().log_cost("translate", "mock", &u, 1.0).unwrap();
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(root.join("costs.jsonl"))
        .unwrap()
        .write_all(br#"{"stage":"translate","usd":"#)
        .unwrap();
    Project::open(root.clone()).unwrap().log_cost("translate", "mock", &u, 2.0).unwrap();
    assert_eq!(Project::open(root).unwrap().spent_total(), 3.0);
}

#[test]
fn failed_install_stays_recoverable() {
    let tmp = tempfile::tempdir().unwrap();
    let game = tmp.path().join("game");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::write(game.join("data.bin"), b"original").unwrap();
    let p = Project::open(tmp.path().join("project")).unwrap();
    // 1. The manifest can't be written (a folder is in the way): the game is never touched.
    std::fs::create_dir_all(p.path("install.json.tmp")).unwrap();
    let patch = tmp.path().join("patched.bin");
    std::fs::write(&patch, b"patched").unwrap();
    let mut inst = Installer::new(&p);
    assert!(inst.put(&patch, "data.bin", &game).is_err());
    assert_eq!(std::fs::read(game.join("data.bin")).unwrap(), b"original");
    std::fs::remove_dir_all(p.path("install.json.tmp")).unwrap();
    // 2. Copying the patch fails after the intent is recorded: Restore still works.
    let mut inst = Installer::new(&p);
    assert!(inst.put(&tmp.path().join("missing.bin"), "data.bin", &game).is_err());
    let mut fresh = Installer::new(&p);
    assert!(fresh.manifest.replaced.contains(&"data.bin".to_string()));
    fresh.restore(&game, &|_| {}).unwrap();
    assert_eq!(std::fs::read(game.join("data.bin")).unwrap(), b"original");
    // 3. A normal install and restore.
    let mut inst = Installer::new(&p);
    inst.put(&patch, "data.bin", &game).unwrap();
    inst.put(&patch, "new/added.bin", &game).unwrap();
    assert_eq!(std::fs::read(game.join("data.bin")).unwrap(), b"patched");
    Installer::new(&p).restore(&game, &|_| {}).unwrap();
    assert_eq!(std::fs::read(game.join("data.bin")).unwrap(), b"original");
    assert!(!game.join("new/added.bin").exists());
}

#[test]
fn siglus_and_reallive_are_detected_by_their_own_markers() {
    let tmp = tempfile::tempdir().unwrap();
    let s = tmp.path().join("siglus");
    std::fs::create_dir_all(&s).unwrap();
    std::fs::write(s.join("Scene.pck"), b"").unwrap();
    std::fs::write(s.join("Gameexe.dat"), b"").unwrap();
    let m = engines::detect(&s).unwrap();
    assert_eq!((m[0].engine_id.as_str(), m[0].name.as_str()), ("unsupported", "SiglusEngine"));
    let r = tmp.path().join("reallive");
    std::fs::create_dir_all(&r).unwrap();
    std::fs::write(r.join("Seen.txt"), b"").unwrap();
    std::fs::write(r.join("Gameexe.ini"), b"").unwrap();
    assert_eq!(engines::detect(&r).unwrap()[0].name, "RealLive");
}

#[test]
fn script_folder_at_game_root_installs_in_place() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manual::for_folder(&tmp.path().to_string_lossy(), tmp.path());
    assert!(m.flag("inside_game"), "the game root itself is an in-game script folder");
    assert_eq!(m.support, "auto");
    let outside = manual::for_folder(&tmp.path().join("..").join("elsewhere").to_string_lossy(), tmp.path());
    assert!(!outside.flag("inside_game"));
}

#[test]
fn reversed_open_and_close_codes_are_rejected() {
    let p = protect("朝だ。[font color=red]赤い文字[resetfont]だった。", "[r]");
    assert!(check(&p, Some("Morning. ⟦1⟧Red text⟦2⟧.")).is_empty());
    assert!(check(&p, Some("Morning. ⟦2⟧Red text⟦1⟧.")).iter().any(|x| x.contains("out of order")));
    // Unpaired codes may move freely (word order differs between languages).
    let q = protect("朝、[emb exp=\"f.a\"]と[emb exp=\"f.b\"]が来た。", "[r]");
    assert!(check(&q, Some("⟦2⟧ and ⟦1⟧ came.")).is_empty());
    let x = protect("空は<color=red>赤</color>で<b>太字</b>だ", "\n");
    assert!(!check(&x, Some("⟦2⟧red⟦1⟧ and ⟦3⟧bold⟦4⟧")).is_empty());
    assert!(check(&x, Some("⟦3⟧bold⟦4⟧ and ⟦1⟧red⟦2⟧")).is_empty());
}

#[test]
fn xp3_names_outside_the_bmp_survive() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let name = "scene_🌸.ks";
    std::fs::write(src.join(name), b"test").unwrap();
    let arc = tmp.path().join("t.xp3");
    xp3::write_archive(&src, &arc, &[name.into()]).unwrap();
    assert_eq!(xp3::read_index(&arc).unwrap()[0].name, name);
}

#[test]
fn kirikiri_names_do_not_leak_into_narration() {
    let units = ks::parse("【桜】\n「おはよう」[p]\n地の文が続く。[p]\n");
    assert_eq!(units.iter().map(|u| u.name.clone()).collect::<Vec<_>>(), vec![Some("桜".into()), None]);
}

#[test]
fn work_folders_never_end_in_an_extension() {
    for rel in ["data.xp3", "GameData/data0.pack", "data.pfs", "x.pfs.000"] {
        assert!(!generic::dir_key(rel).contains('.'), "{rel}");
    }
    let tmp = tempfile::tempdir().unwrap();
    let ypf = tmp.path().join("a.ypf");
    std::fs::write(&ypf, [b"YPF\0".as_slice(), &494u32.to_le_bytes(), &[0u8; 24]].concat()).unwrap();
    assert_eq!(generic::ypf_version(&ypf), Some(494));
    std::fs::write(&ypf, b"PK\x03\x04garbage").unwrap();
    assert_eq!(generic::ypf_version(&ypf), None);
}

fn e(name: Option<&str>, msg: &str) -> Entry {
    Entry { name: name.map(String::from), names: None, message: msg.into() }
}

#[test]
fn rebuild_check_aligns_filtered_and_full_exports() {
    // Full export has an ASCII engine string the normal export filters out.
    let original_all = vec![e(Some("桜"), "おはよう"), e(None, "sys_label"), e(None, "桜")];
    let original = vec![e(Some("桜"), "おはよう"), e(None, "桜")];
    let expected = vec![e(Some("Sakura"), "Morning"), e(None, "Sakura")];
    let good = vec![e(Some("Sakura"), "Morning"), e(None, "sys_label"), e(None, "Sakura")];
    assert!(generic::verify_rebuilt(&original_all, &good, &original, &expected).1.is_empty());
    // A shared string overwritten by the line's translation: the name box breaks.
    let bad = vec![e(Some("Cherry"), "Morning"), e(None, "sys_label"), e(None, "Cherry")];
    assert_eq!(generic::verify_rebuilt(&original_all, &bad, &original, &expected).1, vec![0, 1]);
}

#[test]
fn name_only_lines_use_the_glossary_name() {
    let tmp = tempfile::tempdir().unwrap();
    if fake_msgtool(tmp.path()).is_none() {
        return;
    }
    let env = env(tmp.path());
    let game = tmp.path().join("G");
    std::fs::create_dir_all(&game).unwrap();
    // A standalone "桜" string next to lines spoken by 桜 (as in FVP scripts).
    let script = "桜|「おはよう」\n|桜\n桜|「行こう」\n";
    let mut z = zip::ZipWriter::new(std::fs::File::create(game.join("data.pfs")).unwrap());
    z.start_file("a.scr", zip::write::SimpleFileOptions::default()).unwrap();
    std::io::Write::write_all(&mut z, script.as_bytes()).unwrap();
    z.finish().unwrap();
    let pipe = pipeline(&env, &game, settings("quick", 10, 1), fake().0);
    pipe.extract().unwrap();
    let lines = pipe.p.lines();
    assert_eq!(lines[1].skip, SPEAKER_SKIP);
    pipe.translate(None, false).unwrap();
    pipe.build().unwrap();
    let out: serde_json::Value =
        serde_json::from_slice(&std::fs::read(pipe.p.work("json_tl/data_pfs/a.json")).unwrap()).unwrap();
    assert_eq!(out[0]["name"], "Sakura");
    assert_eq!(out[1]["message"], "Sakura");
}

#[test]
fn an_old_patch_is_not_read_as_the_original_after_a_reset() {
    let tmp = tempfile::tempdir().unwrap();
    let game = make_game(&tmp.path().join("g"), SECOND_KS);
    {
        let env = env(tmp.path());
        let pipe = pipeline(&env, &game, settings("quick", 50, 2), fake().0);
        pipe.extract().unwrap();
        pipe.translate(None, false).unwrap();
        pipe.build().unwrap();
        pipe.install().unwrap();
    }
    assert!(game.join("patch.xp3").exists());
    // Yakusu's data folder is gone (reinstall, new PC): the patch is still in the game.
    let env2 = yakusu_core::config::Env::new(tmp.path().join("home2"), tmp.path().join("tools"));
    let pipe = pipeline(&env2, &game, settings("quick", 50, 2), fake().0);
    let st = pipe.extract().unwrap();
    assert_eq!(st.lines, 8);
    assert!(pipe.live_lines().iter().all(|l| yakusu_core::protect::has_japanese(&l.jp)));
    // Building again writes the next free patch name rather than clobbering silently.
    pipe.translate(None, false).unwrap();
    pipe.build().unwrap();
    assert!(pipe.p.work("out/patch2.xp3").exists());
}

/// 100,000 lines through extract, translate, build, install and restore.
/// `cargo test --release -- --ignored large` (takes a few seconds).
#[test]
#[ignore]
fn large_kirikiri_roundtrip() {
    let count = std::env::var("YAKUSU_LARGE").ok().and_then(|v| v.parse().ok()).unwrap_or(100_000usize);
    let tmp = tempfile::tempdir().unwrap();
    let env = env(tmp.path());
    let game = tmp.path().join("Load Game");
    let src = tmp.path().join("scripts");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::create_dir_all(&src).unwrap();
    let mut names = Vec::new();
    for n in (0..count).step_by(1000) {
        let mut text = String::from("*start\n");
        for i in n..(n + 1000).min(count) {
            text += &format!("@wait time={}\n春の朝、桜の下を歩いた。番号{i}。[l][p]\n", i % 100);
        }
        let name = format!("{n:06}.ks");
        std::fs::write(src.join(&name), ks::encode(&text, "utf-16")).unwrap();
        names.push(name);
    }
    xp3::write_archive(&src, &game.join("data.xp3"), &names).unwrap();
    let original = sha(&game.join("data.xp3"));
    let mut s = settings("quick", 40, 16);
    s.context_lines = 10;
    let t0 = std::time::Instant::now();
    let pipe = pipeline(&env, &game, s, fake().0);
    assert_eq!(pipe.extract().unwrap().lines, count);
    let t_extract = t0.elapsed();
    pipe.translate(None, false).unwrap();
    let t_translate = t0.elapsed();
    assert_eq!(pipe.p.results_len(), count);
    assert_eq!(pipe.build().unwrap().applied, count);
    let patch = pipe.p.work("out/patch.xp3");
    let entries = xp3::read_index(&patch).unwrap();
    for entry in entries.iter().filter(|e| !e.name.contains('/') && e.name.ends_with(".ks")) {
        let (text, _) = ks::decode(&xp3::read_file(&patch, entry).unwrap());
        let (old, _) = ks::decode(&std::fs::read(src.join(&entry.name)).unwrap());
        let cmds = |t: &str| t.lines().filter(|l| l.starts_with('@')).map(String::from).collect::<Vec<_>>();
        assert_eq!(cmds(&text), cmds(&old));
        assert_eq!(text.matches("[l]").count(), text.matches("[p]").count());
        assert_eq!(text.split('\n').count(), old.split('\n').count());
    }
    pipe.install().unwrap();
    assert_eq!(sha(&game.join("data.xp3")), original);
    pipe.restore().unwrap();
    let left: Vec<_> = std::fs::read_dir(&game).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(left.len(), 1);
    // The interface's heaviest calls stay fast at this size.
    let p2 = Arc::new(Project::for_game(&env, &game).unwrap());
    let probe = Pipeline::new(env.clone(), p2, settings("quick", 40, 4), fake().0);
    let t1 = std::time::Instant::now();
    let _ = probe.estimate();
    let _ = probe.glossary_issues();
    let _ = probe.overflow_lines().unwrap();
    eprintln!(
        "{count} lines: extract {:.1}s, translate {:.1}s, total {:.1}s; estimate+checks {:.2}s",
        t_extract.as_secs_f64(),
        t_translate.as_secs_f64(),
        t0.elapsed().as_secs_f64(),
        t1.elapsed().as_secs_f64()
    );
}

#[test]
fn data_from_the_yakusoku_builds_moves_over() {
    let tmp = tempfile::tempdir().unwrap();
    let old = tmp.path().join("Yakusoku");
    let new = tmp.path().join("Yakusu");
    std::fs::create_dir_all(old.join("projects/game-1234")).unwrap();
    std::fs::write(old.join("projects/game-1234/project.json"), "{}").unwrap();
    std::fs::write(old.join("settings.json"), r#"{"provider":"openai"}"#).unwrap();
    std::fs::create_dir_all(old.join("tools-abc")).unwrap();
    // A project the new folder already has is left alone.
    std::fs::create_dir_all(new.join("projects/kept-1")).unwrap();
    std::fs::create_dir_all(old.join("projects/kept-1")).unwrap();
    std::fs::write(old.join("projects/kept-1/project.json"), "old").unwrap();
    yakusu_core::config::migrate_home(&old, &new);
    assert!(new.join("projects/game-1234/project.json").exists());
    assert!(new.join("settings.json").exists());
    assert!(!new.join("projects/kept-1/project.json").exists());
    assert!(!old.join("projects/game-1234").exists());
}
