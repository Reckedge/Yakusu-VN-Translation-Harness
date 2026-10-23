//! Regressions for the red-team audit of the Rust rewrite (findings R01–R14).
//!
//! Adapted from the audit's own test file. Every finding has a synthetic
//! test that runs everywhere. Tests that need the real msg-tool run when
//! YAKUSU_REAL_MSG_TOOL is set; tests on actual games run when
//! AUDIT_REAL_GAMES (and AUDIT_DLL, KirikiriTools' version.dll) are set.
//! No game assets are part of this repository.

mod common;
use common::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use yakusu_core::engines::{self, generic, ks, xp3, Installer};
use yakusu_core::error::Error;
use yakusu_core::project::{Project, ResultRow};
use yakusu_core::protect::{check, protect};

/// A Kirikiri game: data.xp3 holding a.ks (UTF-16) with `script`.
fn single_game(root: &Path, script: &str) -> PathBuf {
    let game = root.join("game");
    let src = root.join("src");
    fs::create_dir_all(&game).unwrap();
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("a.ks"), ks::encode(script, "utf-16")).unwrap();
    xp3::write_archive(&src, &game.join("data.xp3"), &["a.ks".into()]).unwrap();
    game
}

fn err_text<T: std::fmt::Debug>(r: Result<T, Error>) -> String {
    r.expect_err("expected an error").to_string()
}

// ------------------------------------------------------------------ R01 paths

#[test]
fn archive_parent_path_cannot_escape_extraction_root() {
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path());
    let game = single_game(tmp.path(), "朝だ。[p]\n");
    let src = tmp.path().join("src");
    fs::write(tmp.path().join("escaped.ks"), ks::encode("外に書いた。[p]\n", "utf-16")).unwrap();
    xp3::write_archive(&src, &game.join("data.xp3"), &["../escaped.ks".into()]).unwrap();
    let pipe = pipeline(&e, &game, settings("quick", 10, 1), fake().0);
    let target = pipe.p.work("escaped.ks");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, b"SENTINEL").unwrap();
    let r = pipe.extract();
    assert!(err_text(r).contains("unsafe file name"));
    assert_eq!(fs::read(target).unwrap(), b"SENTINEL", "archive entry escaped work/scripts");
}

#[test]
fn install_parent_path_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let game = tmp.path().join("game");
    fs::create_dir_all(&game).unwrap();
    let p = Project::open(tmp.path().join("project")).unwrap();
    let src = tmp.path().join("patch");
    fs::write(&src, b"patched").unwrap();
    let outside = tmp.path().join("outside.txt");
    fs::write(&outside, b"original").unwrap();
    for rel in ["../outside.txt", "..\\outside.txt", "sub/../../outside.txt", "/etc/x", "\\\\server\\share\\x", "C:/x"]
    {
        let r = Installer::new(&p).put(&src, rel, &game);
        assert!(r.is_err(), "installer accepted {rel}");
    }
    assert_eq!(fs::read(&outside).unwrap(), b"original");
    assert!(!Installer::new(&p).installed());
}

#[test]
fn unsafe_relative_names_are_refused() {
    for bad in [
        "",
        "..",
        "../a",
        "a/../b",
        "a\\..\\b",
        ".",
        "/abs",
        "\\abs",
        "C:x",
        "C:\\x",
        "a/b:stream",
        "\\\\?\\C:\\x",
        "CON",
        "nul.txt",
        "a/COM1.ks",
        "lpt9",
        "a?b",
        "a<b",
        "a|b",
        "dir./x",
        "x. ",
        "a\nb",
        "a\0b",
    ] {
        assert!(engines::safe_rel(bad).is_err(), "{bad:?} accepted");
    }
    assert_eq!(engines::safe_rel("scenario\\first.ks").unwrap(), "scenario/first.ks");
    assert_eq!(engines::safe_rel("a//b.ks").unwrap(), "a/b.ks");
    assert_eq!(engines::safe_rel("console.ks").unwrap(), "console.ks"); // CON only as a whole stem
    assert_eq!(engines::safe_rel("シナリオ/第1話.ks").unwrap(), "シナリオ/第1話.ks");
}

#[cfg(unix)]
#[test]
fn writes_never_follow_links_out_of_the_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
    assert!(engines::safe_join(&root, "link/x.ks").is_err());
    // Listing doesn't follow links either.
    fs::write(outside.join("secret.ks"), b"x").unwrap();
    assert!(engines::list_files(&root, None).is_empty());
}

// ------------------------------------------------------------------ R02 temp files

#[cfg(unix)]
#[test]
fn install_temporary_symlink_cannot_overwrite_outside_file() {
    let tmp = tempfile::tempdir().unwrap();
    let game = tmp.path().join("game");
    fs::create_dir_all(&game).unwrap();
    let src = tmp.path().join("patch");
    fs::write(&src, b"patched").unwrap();
    let outside = tmp.path().join("outside");
    fs::write(&outside, b"ORIGINAL").unwrap();
    std::os::unix::fs::symlink(&outside, game.join("new.bin.yakusu-tmp")).unwrap();
    let p = Project::open(tmp.path().join("p")).unwrap();
    Installer::new(&p).put(&src, "new.bin", &game).unwrap();
    assert_eq!(fs::read(&outside).unwrap(), b"ORIGINAL");
    assert_eq!(fs::read(game.join("new.bin")).unwrap(), b"patched");
}

#[test]
fn install_temporary_name_cannot_destroy_existing_game_asset() {
    let tmp = tempfile::tempdir().unwrap();
    let game = tmp.path().join("game");
    fs::create_dir_all(&game).unwrap();
    let src = tmp.path().join("patch");
    fs::write(&src, b"patched").unwrap();
    fs::write(game.join("new.bin.yakusu-tmp"), b"ORIGINAL ASSET").unwrap();
    let p = Project::open(tmp.path().join("p")).unwrap();
    Installer::new(&p).put(&src, "new.bin", &game).unwrap();
    Installer::new(&p).restore(&game, &|_| {}).unwrap();
    assert_eq!(fs::read(game.join("new.bin.yakusu-tmp")).ok(), Some(b"ORIGINAL ASSET".to_vec()));
    assert!(!game.join("new.bin").exists());
    // No temporary files left behind anywhere.
    assert!(engines::list_files(&game, None).iter().all(|f| f == "new.bin.yakusu-tmp"));
}

// ------------------------------------------------------------------ R03 / R04 install records

#[test]
fn corrupt_manifest_must_not_overwrite_original_backup() {
    let tmp = tempfile::tempdir().unwrap();
    let game = tmp.path().join("game");
    fs::create_dir_all(&game).unwrap();
    fs::write(game.join("data.bin"), b"original").unwrap();
    let src = tmp.path().join("patch");
    fs::write(&src, b"patched").unwrap();
    let p = Project::open(tmp.path().join("p")).unwrap();
    Installer::new(&p).put(&src, "data.bin", &game).unwrap();
    fs::write(p.path("install.json"), b"{broken").unwrap();
    let inst = Installer::new(&p);
    assert!(inst.damaged() && inst.installed());
    let r = Installer::new(&p).put(&src, "data.bin", &game);
    assert!(err_text(r).contains("damaged"));
    assert_eq!(fs::read(p.path("backup/data.bin")).unwrap(), b"original", "original backup overwritten");
    // Restore recovers from the backups themselves.
    Installer::new(&p).restore(&game, &|_| {}).unwrap();
    assert_eq!(fs::read(game.join("data.bin")).unwrap(), b"original");
    assert!(!Installer::new(&p).installed());
}

#[test]
fn stray_backup_is_never_overwritten() {
    // The record lost track of a file (deleted install.json), and the game
    // now has a different file than the backup: refuse, keep the backup.
    let tmp = tempfile::tempdir().unwrap();
    let game = tmp.path().join("game");
    fs::create_dir_all(&game).unwrap();
    fs::write(game.join("data.bin"), b"original").unwrap();
    let src = tmp.path().join("patch");
    fs::write(&src, b"patched").unwrap();
    let p = Project::open(tmp.path().join("p")).unwrap();
    Installer::new(&p).put(&src, "data.bin", &game).unwrap();
    fs::remove_file(p.path("install.json")).unwrap();
    assert!(Installer::new(&p).put(&src, "data.bin", &game).is_err());
    assert_eq!(fs::read(p.path("backup/data.bin")).unwrap(), b"original");
}

#[test]
fn missing_backup_must_not_report_successful_restore() {
    let tmp = tempfile::tempdir().unwrap();
    let game = tmp.path().join("game");
    fs::create_dir_all(&game).unwrap();
    fs::write(game.join("data.bin"), b"original").unwrap();
    fs::write(game.join("other.bin"), b"other original").unwrap();
    let src = tmp.path().join("patch");
    fs::write(&src, b"patched").unwrap();
    let p = Project::open(tmp.path().join("p")).unwrap();
    let mut inst = Installer::new(&p);
    inst.put(&src, "data.bin", &game).unwrap();
    inst.put(&src, "other.bin", &game).unwrap();
    inst.put(&src, "added.bin", &game).unwrap();
    fs::remove_file(p.path("backup/data.bin")).unwrap();
    let r = Installer::new(&p).restore(&game, &|_| {});
    assert!(err_text(r).contains("data.bin"));
    // Nothing was changed and nothing forgotten.
    let inst = Installer::new(&p);
    assert!(inst.installed());
    assert_eq!(inst.manifest.replaced.len(), 2);
    assert_eq!(fs::read(game.join("other.bin")).unwrap(), b"patched");
    assert!(game.join("added.bin").exists());
}

#[test]
fn changed_backup_is_detected() {
    let tmp = tempfile::tempdir().unwrap();
    let game = tmp.path().join("game");
    fs::create_dir_all(&game).unwrap();
    fs::write(game.join("data.bin"), b"original").unwrap();
    let src = tmp.path().join("patch");
    fs::write(&src, b"patched").unwrap();
    let p = Project::open(tmp.path().join("p")).unwrap();
    Installer::new(&p).put(&src, "data.bin", &game).unwrap();
    fs::write(p.path("backup/data.bin"), b"tampered").unwrap();
    assert!(Installer::new(&p).restore(&game, &|_| {}).is_err());
    assert!(Installer::new(&p).installed());
}

// ------------------------------------------------------------------ R05 / R06 / R07 codes

#[test]
fn invented_engine_commands_are_rejected_before_build() {
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path());
    let game = single_game(tmp.path(), "春の朝だ。[p]\n");
    let factory = fake_with(Box::new(|system, user| {
        let mut v: serde_json::Value = serde_json::from_str(&yakusu_core::llm::fake::reply(system, user)).ok()?;
        if user.contains("TRANSLATE") {
            for row in v.as_array_mut()? {
                row["en"] = serde_json::json!("Morning. [jump target=*audit_marker]");
            }
            Some(Ok(v.to_string()))
        } else {
            None
        }
    }));
    let pipe = pipeline(&e, &game, settings("quick", 10, 1), factory);
    pipe.extract().unwrap();
    pipe.translate(None, false).unwrap();
    let l = &pipe.live_lines()[0];
    let r = pipe.p.result(&l.id).unwrap();
    assert!(r.problems.iter().any(|p| p.contains("adds engine code")), "{:?}", r.problems);
    assert!(pipe.build().is_err());
    assert!(!pipe.p.work("out").join("patch.xp3").exists());
}

#[test]
fn hand_edits_and_csv_get_the_same_code_checks() {
    let pr = protect("朝だ。[p]", "[r]");
    for bad in ["Morning. @jump storage=x", "Morning.\n*label", "Morning [l] now", "<color=red>Morning"] {
        assert!(!check(&pr, Some(bad)).is_empty(), "{bad}");
    }
    assert!(check(&pr, Some("Morning.")).is_empty());
}

#[test]
fn flagged_reversed_codes_must_not_be_built() {
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path());
    let game = single_game(tmp.path(), "朝だ。[font color=red]赤い[resetfont]文字だ。[p]\n");
    let pipe = pipeline(&e, &game, settings("quick", 10, 1), fake().0);
    pipe.extract().unwrap();
    let l = &pipe.live_lines()[0];
    let en = "Morning. ⟦2⟧Red⟦1⟧ text.";
    let problems = check(&protect(&l.jp, "[r]"), Some(en));
    assert!(!problems.is_empty());
    pipe.p
        .add_results(vec![ResultRow {
            id: l.id.clone(),
            en_ph: en.into(),
            src: Some(l.jp.clone()),
            problems,
            ..Default::default()
        }])
        .unwrap();
    assert!(err_text(pipe.build()).contains("can be written safely"));
}

#[test]
fn rows_stored_without_problems_are_still_checked_at_build() {
    // A row written by an older version (or by hand) with no problems
    // recorded: build checks again and leaves it in Japanese.
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path());
    let game = single_game(tmp.path(), "朝だ。[p]\n夜だ。[p]\n");
    let pipe = pipeline(&e, &game, settings("quick", 10, 1), fake().0);
    pipe.extract().unwrap();
    let lines = pipe.live_lines();
    pipe.p
        .add_results(vec![
            ResultRow {
                id: lines[0].id.clone(),
                en_ph: "Morning.".into(),
                src: Some(lines[0].jp.clone()),
                ..Default::default()
            },
            ResultRow {
                id: lines[1].id.clone(),
                en_ph: "Night. [call storage=x]".into(),
                src: Some(lines[1].jp.clone()),
                ..Default::default()
            },
        ])
        .unwrap();
    let b = pipe.build().unwrap();
    assert_eq!((b.applied, b.blocked), (1, 1));
    let rel = &engines::list_files(&pipe.p.work("patchroot"), Some(&[".ks"]))[0];
    let (text, _) = ks::decode(&fs::read(engines::join_rel(&pipe.p.work("patchroot"), rel)).unwrap());
    assert!(text.contains("Morning.") && text.contains("夜だ。") && !text.contains("[call"));
}

#[test]
fn glossary_names_cannot_inject_commands() {
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path());
    let game = single_game(tmp.path(), "【桜】「おはよう」[p]\n");
    let pipe = pipeline(&e, &game, settings("quick", 10, 1), fake().0);
    pipe.extract().unwrap();
    let mut g = pipe.p.glossary();
    g.names.insert("桜".into(), "Sakura[jump target=*x]".into());
    pipe.p.set_glossary(g).unwrap();
    pipe.translate(None, false).unwrap();
    pipe.build().unwrap();
    let (text, _) = ks::decode(&fs::read(pipe.p.work("patchroot/a.ks")).unwrap());
    assert!(!text.contains("[jump"), "{text}");
    assert!(text.contains("【桜】"));
}

#[test]
fn nested_pairs_cannot_cross() {
    let p = protect("朝<color=red>赤<b>太い</b>赤</color>だ。", "[r]");
    assert!(!check(&p, Some("Morning ⟦1⟧red⟦2⟧bold⟦4⟧red⟦3⟧.")).is_empty(), "crossed nesting accepted");
    assert!(check(&p, Some("Morning ⟦1⟧red⟦2⟧bold⟦3⟧red⟦4⟧.")).is_empty());
    // Independent spans may change order; that's normal in translation.
    let q = protect("朝<b>太い</b>と<i>細い</i>だ", "[r]");
    assert!(check(&q, Some("⟦3⟧thin⟦4⟧ and ⟦1⟧bold⟦2⟧")).is_empty());
    assert!(!check(&q, Some("⟦3⟧thin⟦1⟧ and ⟦4⟧bold⟦2⟧")).is_empty());
}

// ------------------------------------------------------------------ R08 parsing

#[test]
fn malformed_descramble_length_returns_none_not_panic() {
    let mut data = vec![0xfe, 0xfe, 2, 0xff, 0xfe];
    data.extend((-1i64).to_le_bytes());
    data.extend(0u64.to_le_bytes());
    assert_eq!(std::panic::catch_unwind(|| xp3::descramble(&data)).ok(), Some(None));
    // Declared original length that doesn't match the data.
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut z, &[0x41, 0, 0x42, 0]).unwrap();
    let body = z.finish().unwrap();
    let mut data = vec![0xfe, 0xfe, 2, 0xff, 0xfe];
    data.extend((body.len() as u64).to_le_bytes());
    data.extend(400u64.to_le_bytes());
    data.extend(&body);
    assert_eq!(xp3::descramble(&data), None);
}

#[test]
fn malicious_xp3_size_returns_error_not_capacity_panic() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("empty");
    fs::write(&p, b"").unwrap();
    let e = xp3::Entry { name: "a.ks".into(), size: u64::MAX, segments: vec![], encrypted_flag: 0, adler: 0 };
    assert!(matches!(std::panic::catch_unwind(|| xp3::read_file(&p, &e)), Ok(Err(_))));
    let e = xp3::Entry {
        name: "a.ks".into(),
        size: 10,
        segments: vec![(0, u64::MAX - 2, 10, 10)],
        encrypted_flag: 0,
        adler: 0,
    };
    assert!(matches!(std::panic::catch_unwind(|| xp3::read_file(&p, &e)), Ok(Err(_))));
}

#[test]
fn corrupted_file_size_must_be_detected() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("data");
    fs::write(&p, b"hello").unwrap();
    let e = xp3::Entry { name: "a.ks".into(), size: 9, segments: vec![(0, 0, 9, 5)], encrypted_flag: 0, adler: 0 };
    assert!(xp3::read_file(&p, &e).is_err(), "claimed 9 bytes silently accepted as 5");
    let e = xp3::Entry { name: "a.ks".into(), size: 5, segments: vec![(0, 0, 5, 5)], encrypted_flag: 0, adler: 0 };
    assert_eq!(xp3::read_file(&p, &e).unwrap(), b"hello");
}

#[test]
fn damaged_archives_never_panic() {
    // Every truncation and a sweep of byte flips of a valid archive: errors
    // are fine, panics are not.
    let tmp = tempfile::tempdir().unwrap();
    let game = single_game(tmp.path(), "*start\n朝だ。[p]\n夜だ。[p]\n");
    let good = fs::read(game.join("data.xp3")).unwrap();
    let path = tmp.path().join("bad.xp3");
    let mut cases: Vec<Vec<u8>> = (0..good.len()).step_by(3).map(|n| good[..n].to_vec()).collect();
    for i in 0..good.len() {
        for x in [0xFFu8, 0x80, 0x01] {
            let mut d = good.clone();
            d[i] ^= x;
            cases.push(d);
        }
    }
    for data in cases {
        fs::write(&path, &data).unwrap();
        let r = std::panic::catch_unwind(|| {
            if let Ok(entries) = xp3::read_index(&path) {
                for e in &entries {
                    if let Ok(d) = xp3::read_file(&path, e) {
                        let _ = xp3::descramble(&d);
                        let _ = ks::garbled(&ks::decode(&d).0);
                    }
                }
            }
        });
        assert!(r.is_ok(), "panic on a damaged archive");
    }
}

#[test]
fn shift_jis_and_utf16_bom_roundtrip_control() {
    for enc in ["utf-16", "utf-8-sig", "cp932"] {
        let s = "*start\r\n日本語だ。[p]\r\n";
        let (decoded, label) = ks::decode(&ks::encode(s, enc));
        assert_eq!(decoded, s);
        assert_eq!(label, enc);
    }
}

// ------------------------------------------------------------------ R09 decryption

fn noise_utf16(n: usize) -> Vec<u8> {
    let mut v = vec![0xFF, 0xFE];
    let mut x: u32 = 0x1234_5678;
    for _ in 0..n {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        v.extend(((x >> 16) as u16).to_le_bytes());
    }
    v
}

#[test]
fn garbled_scripts_are_rejected_before_translation() {
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path());
    let game = single_game(tmp.path(), "*start\n朝だ。[p]\n");
    let (calls_factory, calls) = fake();
    let pipe = pipeline(&e, &game, settings("quick", 10, 1), calls_factory);
    let first = pipe.extract().unwrap();
    // The archive is replaced by one whose second script is noise (what a
    // wrong decryption profile produces).
    let src = tmp.path().join("src");
    fs::write(src.join("b.ks"), noise_utf16(3000)).unwrap();
    xp3::write_archive(&src, &game.join("data.xp3"), &["a.ks".into(), "b.ks".into()]).unwrap();
    let msg = err_text(pipe.extract());
    assert!(msg.contains("aren't readable text") && msg.contains("b.ks"), "{msg}");
    // The previous read is intact and nothing was sent to a model.
    assert_eq!(pipe.live_lines().len(), first.lines);
    assert!(engines::list_files(&pipe.p.work("json"), Some(&[".json"])).len() == 1);
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn garbled_detector_controls() {
    assert!(!ks::garbled(""));
    assert!(!ks::garbled(super_long_script().as_str()));
    assert!(ks::garbled(&ks::decode(&noise_utf16(2000)).0));
    let ctrl: String = "朝だ。\u{1}\u{2}\u{3}\u{4}\u{5}\u{6}[p]\n".repeat(20);
    assert!(ks::garbled(&ctrl));
}

fn super_long_script() -> String {
    let mut s = String::from("*start|プロローグ\r\n[cm]\r\n@bg storage=\"bg_room\"\r\n");
    for i in 0..300 {
        s += &format!("【桜】「おはよう、大雅くん！{i}回目だね♪」[p]\r\n今日もいい天気だ……。〜ー・！？[l][r]\r\n");
    }
    s
}

// ------------------------------------------------------------------ R10 mixed archives

#[test]
fn mixed_archive_rebuild_imports_only_scripts() {
    let tmp = tempfile::tempdir().unwrap();
    if fake_msgtool(tmp.path()).is_none() {
        eprintln!("python3 not available; skipping");
        return;
    }
    let e = env(tmp.path());
    let game = tmp.path().join("ArtemisGame");
    fs::create_dir_all(&game).unwrap();
    let script = "桜|「おはよう」\n|春の朝だ。\n";
    zip_write(&game.join("data.pfs"), &[("a.scr", script.as_bytes()), ("asset.bin", b"UNCHANGED ASSET")]);
    fs::write(game.join("game.exe"), b"MZ").unwrap();
    let pipe = pipeline(&e, &game, settings("quick", 10, 1), fake().0);
    pipe.extract().unwrap();
    pipe.translate(None, false).unwrap();
    assert_eq!(pipe.build().unwrap().applied, 2);
    pipe.install().unwrap();
    assert_eq!(zip_read(&game.join("data.pfs"), "asset.bin"), b"UNCHANGED ASSET");
    assert!(!String::from_utf8(zip_read(&game.join("data.pfs"), "a.scr")).unwrap().contains("春"));
    pipe.restore().unwrap();
}

#[test]
fn scripts_pair_with_their_json() {
    let tmp = tempfile::tempdir().unwrap();
    let (src, json) = (tmp.path().join("s"), tmp.path().join("j"));
    for f in ["a.ast", "sub/b.ast", "sub/c.ast", "img.png", "x.y.ast"] {
        let p = engines::join_rel(&src, f);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b"").unwrap();
    }
    for f in ["a.json", "sub/b.json", "x.y.json"] {
        let p = engines::join_rel(&json, f);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b"[]").unwrap();
    }
    assert_eq!(generic::scripts_with_json(&src, &json), vec!["a.ast", "sub/b.ast", "x.y.ast"]);
}

// ------------------------------------------------------------------ R11 changed sources

#[test]
fn changed_source_is_retranslated_without_explicit_redo() {
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path());
    let game = single_game(tmp.path(), "朝が来た。[p]\n変わらない行。[p]\n");
    let pipe = pipeline(&e, &game, settings("quick", 10, 1), fake().0);
    pipe.extract().unwrap();
    assert_eq!(pipe.translate(None, false).unwrap(), 2);
    single_game(tmp.path(), "夜になった。[p]\n変わらない行。[p]\n");
    pipe.extract().unwrap();
    assert_eq!(pipe.translate(None, false).unwrap(), 1, "a result for the old Japanese skipped the new line");
    assert_eq!(pipe.build().unwrap().applied, 2);
    // Also after reopening the project from disk.
    let p2 = Project::open(pipe.p.root.clone()).unwrap();
    let lines = p2.live_lines();
    assert!(lines.iter().all(|l| p2.result(&l.id).is_some_and(|r| r.src.as_deref() == Some(l.jp.as_str()))));
}

// ------------------------------------------------------------------ R12 re-reading

#[test]
fn reextract_while_installed_reads_the_backed_up_original() {
    let tmp = tempfile::tempdir().unwrap();
    if fake_msgtool(tmp.path()).is_none() {
        eprintln!("python3 not available; skipping");
        return;
    }
    let e = env(tmp.path());
    let game = tmp.path().join("ArtemisGame");
    fs::create_dir_all(&game).unwrap();
    zip_write(&game.join("data.pfs"), &[("a.scr", "桜|「おはよう」\n|春の朝だ。\n|学校へ行く。\n".as_bytes())]);
    fs::write(game.join("game.exe"), b"MZ").unwrap();
    let pipe = pipeline(&e, &game, settings("quick", 10, 1), fake().0);
    let before: Vec<String> = {
        pipe.extract().unwrap();
        pipe.live_lines().iter().map(|l| l.jp.clone()).collect()
    };
    pipe.translate(None, false).unwrap();
    pipe.build().unwrap();
    pipe.install().unwrap();
    let st = pipe.extract().unwrap();
    let after: Vec<String> = pipe.live_lines().iter().map(|l| l.jp.clone()).collect();
    assert_eq!(after, before, "re-read the installed English instead of the original");
    assert_eq!(st.lines, 3);
    // Translations still apply: nothing to redo.
    assert_eq!(pipe.translate(None, false).unwrap(), 0);
    assert_eq!(pipe.build().unwrap().applied, 3);
    pipe.restore().unwrap();
}

#[test]
fn failed_reread_keeps_the_previous_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path());
    let game = single_game(tmp.path(), "朝だ。[p]\n");
    let pipe = pipeline(&e, &game, settings("quick", 10, 1), fake().0);
    pipe.extract().unwrap();
    pipe.translate(None, false).unwrap();
    // Now the game's script has no Japanese (an English patch from elsewhere).
    single_game(tmp.path(), "Morning.[p]\n");
    assert!(pipe.extract().is_err());
    assert_eq!(pipe.live_lines().len(), 1);
    assert_eq!(engines::list_files(&pipe.p.work("scripts"), None), vec!["a.ks"]);
    let (text, _) = ks::decode(&fs::read(pipe.p.work("scripts/a.ks")).unwrap());
    assert!(text.contains("朝だ"));
    assert!(!pipe.p.work("previous-read").exists());
}

// ------------------------------------------------------------------ R13 work keys

#[test]
fn distinct_archive_names_need_distinct_work_keys() {
    let names = ["data.0.pfs", "data_0.pfs", "Data.0.pfs", "a/b.pfs", "a__b.pfs", "a\\b_c.pfs"];
    let keys: std::collections::HashSet<String> = names.iter().map(|n| generic::dir_key(n)).collect();
    assert_eq!(keys.len(), names.len());
    assert!(keys.iter().all(|k| !k.contains('.') && !k.contains('/')));
    assert_eq!(generic::dir_key("a/b.pfs"), generic::dir_key("a\\b.pfs"));
}

#[test]
fn projects_read_before_keep_their_line_ids() {
    let files = vec!["data.pfs".to_string(), "sub/x.pfs".to_string()];
    let legacy = generic::work_keys(&files, None, true);
    assert_eq!(legacy["data.pfs"], "data_pfs");
    let fresh = generic::work_keys(&files, None, false);
    assert_eq!(fresh["data.pfs"], generic::dir_key("data.pfs").as_str());
    // Colliding legacy keys are never reused.
    let clash = vec!["data.0.pfs".to_string(), "data_0.pfs".to_string()];
    let k = generic::work_keys(&clash, None, true);
    assert_ne!(k["data.0.pfs"], k["data_0.pfs"]);
}

// ------------------------------------------------------------------ R14 spending limit

fn openai_settings(budget: f64) -> yakusu_core::config::Settings {
    let mut s = settings("quick", 1, 2);
    s.provider = "openai".into();
    s.keys.insert("openai".into(), "test".into());
    s.translate_model.insert("openai".into(), "unpriced-paid-model".into());
    s.budget_usd = budget;
    s
}

#[test]
fn unpriced_paid_model_must_not_silently_disable_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path());
    let game = single_game(tmp.path(), "朝だ。[p]\n夜だ。[p]\n");
    let (factory, calls) = fake();
    let pipe = pipeline(&e, &game, openai_settings(1.0), factory);
    pipe.extract().unwrap();
    let r = pipe.translate(None, false);
    assert!(matches!(&r, Err(Error::Budget(m)) if m.contains("doesn't know what")), "{r:?}");
    assert!(calls.lock().unwrap().is_empty(), "a call was made without a way to count its cost");
    assert!(pipe.preflight().iter().any(|n| n.text.contains("doesn't know what")));
}

#[test]
fn entered_prices_are_charged_and_capped() {
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path());
    let game = single_game(tmp.path(), "朝だ。[p]\n夜だ。[p]\n");
    let mut s = openai_settings(5.0);
    s.prices.insert("unpriced-paid-model".into(), [1.0, 2.0]);
    let pipe = pipeline(&e, &game, s.clone(), fake().0);
    pipe.extract().unwrap();
    assert_eq!(pipe.translate(None, false).unwrap(), 2);
    assert!(pipe.p.spent_total() > 0.0);
    // A cap smaller than one request's worst case stops before calling.
    let tmp2 = tempfile::tempdir().unwrap();
    let game2 = single_game(tmp2.path(), "朝だ。[p]\n");
    s.budget_usd = 0.000_01;
    let (factory, calls) = fake();
    let pipe2 = pipeline(&env(tmp2.path()), &game2, s, factory);
    pipe2.extract().unwrap();
    assert!(matches!(pipe2.translate(None, false), Err(Error::Budget(_))));
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn without_a_limit_unpriced_models_still_run() {
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path());
    let game = single_game(tmp.path(), "朝だ。[p]\n");
    let pipe = pipeline(&e, &game, openai_settings(0.0), fake().0);
    pipe.extract().unwrap();
    assert_eq!(pipe.translate(None, false).unwrap(), 1);
}

#[test]
fn parallel_calls_cannot_overshoot_the_cap() {
    // Eight batches in parallel against a cap that covers only a few of them.
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path());
    let script: String = (0..8).map(|i| format!("{i}番目の朝だ。[p]\n")).collect();
    let game = single_game(tmp.path(), &script);
    let mut s = settings("quick", 1, 8);
    s.budget_usd = 0.006; // Haiku: a 1-line batch reserves ~$0.00075
    let pipe = pipeline(&e, &game, s.clone(), fake().0);
    pipe.extract().unwrap();
    let _ = pipe.translate(None, false);
    assert!(pipe.p.spent_total() <= s.budget_usd, "spent {} over a {} cap", pipe.p.spent_total(), s.budget_usd);
}

// ------------------------------------------------------------------ native msg-tool

fn real_tool() -> Option<PathBuf> {
    std::env::var_os("YAKUSU_REAL_MSG_TOOL").map(PathBuf::from).filter(|p| p.is_file())
}

fn real_env(root: &Path) -> Option<yakusu_core::config::Env> {
    let tool = real_tool()?;
    let e = env(root);
    fs::create_dir_all(&e.tools).unwrap();
    fs::copy(tool, e.tools.join("msg-tool")).unwrap();
    if let Some(dll) = std::env::var_os("AUDIT_DLL") {
        fs::create_dir_all(e.tools.join("kirikiri")).unwrap();
        fs::copy(dll, e.tools.join("kirikiri/version.dll")).unwrap();
    }
    Some(e)
}

macro_rules! need_real_tool {
    ($tmp:expr) => {
        match real_env($tmp) {
            Some(e) => e,
            None => {
                eprintln!("YAKUSU_REAL_MSG_TOOL not set; skipping");
                return;
            }
        }
    };
}

fn native_artemis_roundtrip(with_asset: bool) {
    let tmp = tempfile::tempdir().unwrap();
    let e = need_real_tool!(tmp.path());
    let game = tmp.path().join("ArtemisSynthetic");
    let src = tmp.path().join("src");
    fs::create_dir_all(&game).unwrap();
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("a.ast"),
        r#"ast = { label = { top = { block = "b" } }, b = { text = { ja = { { "春の朝が来た。" }, { "学校へ向かった。" } } } } }"#,
    )
    .unwrap();
    if with_asset {
        fs::write(src.join("asset.bin"), b"UNCHANGED ASSET").unwrap();
    }
    let p = Project::open(tmp.path().join("pack")).unwrap();
    let log = |s: &str| eprintln!("{s}");
    let cx = engines::Ctx { env: &e, project: &p, log: &log };
    engines::msgtool::pack(&cx, &src, &game.join("data.pfs"), "artemis-arc", &[]).unwrap();
    let original = sha(&game.join("data.pfs"));
    let pipe = pipeline(&e, &game, settings("quick", 10, 1), fake().0);
    assert_eq!(pipe.extract().unwrap().lines, 2);
    pipe.translate(None, false).unwrap();
    assert_eq!(pipe.build().unwrap().applied, 2);
    pipe.install().unwrap();
    let unpack = tmp.path().join("rebuilt");
    engines::msgtool::unpack(&cx, &game.join("data.pfs"), &unpack, Some("artemis-arc"), &[]).unwrap();
    if with_asset {
        assert_eq!(fs::read(unpack.join("asset.bin")).unwrap(), b"UNCHANGED ASSET");
    }
    let export = tmp.path().join("exported");
    engines::msgtool::export(&cx, &unpack, &export, Some("artemis")).unwrap();
    let got = engines::read_json_messages(&export.join("a.json")).unwrap();
    let key = generic::dir_key("data.pfs");
    let wanted = engines::read_json_messages(&pipe.p.work(&format!("json_tl/{key}/a.json"))).unwrap();
    assert_eq!(got, wanted);
    pipe.restore().unwrap();
    assert_eq!(sha(&game.join("data.pfs")), original);
}

#[test]
fn native_artemis_script_only_roundtrip_control() {
    native_artemis_roundtrip(false);
}

#[test]
fn native_artemis_mixed_asset_rebuild_preserves_assets() {
    native_artemis_roundtrip(true);
}

#[test]
fn native_qlie_archive_control() {
    let tmp = tempfile::tempdir().unwrap();
    let e = need_real_tool!(tmp.path());
    let src = tmp.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("pack_keyfile_kfueheish15538fa9or.key"), vec![7u8; 1024]).unwrap();
    fs::write(src.join("payload.bin"), b"UNTOUCHED").unwrap();
    let p = Project::open(tmp.path().join("p")).unwrap();
    let log = |s: &str| eprintln!("{s}");
    let cx = engines::Ctx { env: &e, project: &p, log: &log };
    let arc = tmp.path().join("data.pack");
    let out = tmp.path().join("unpacked");
    engines::msgtool::pack(&cx, &src, &arc, "qlie-pack", &["--backslash".into()]).unwrap();
    engines::msgtool::unpack(&cx, &arc, &out, Some("qlie-pack"), &[]).unwrap();
    assert_eq!(fs::read(out.join("payload.bin")).unwrap(), b"UNTOUCHED");
}

#[test]
fn native_unpack_keeps_entries_inside_the_output_folder() {
    // Needs the msg-tool Yakusu ships (built with packaging/msg-tool-safe-paths.patch).
    let tmp = tempfile::tempdir().unwrap();
    let e = need_real_tool!(tmp.path());
    let src = tmp.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(tmp.path().join("escaped.ks"), b"ORIGINAL").unwrap();
    fs::write(src.join("ok.ks"), ks::encode("朝だ。[p]\n", "utf-16")).unwrap();
    let arc = tmp.path().join("data.xp3");
    xp3::write_archive(&src, &arc, &["ok.ks".into(), "../escaped.ks".into()]).unwrap();
    let p = Project::open(tmp.path().join("p")).unwrap();
    let log = |s: &str| eprintln!("{s}");
    let cx = engines::Ctx { env: &e, project: &p, log: &log };
    let out = tmp.path().join("work").join("unpacked");
    engines::msgtool::unpack(&cx, &arc, &out, Some("kirikiri-xp3"), &[]).unwrap();
    assert!(out.join("ok.ks").is_file(), "the archive wasn't unpacked, so this test proves nothing");
    assert_eq!(fs::read(tmp.path().join("escaped.ks")).unwrap(), b"ORIGINAL");
    assert!(!tmp.path().join("work").join("escaped.ks").exists());
    // The hostile entry was extracted, but inside the folder ("../" -> "__/").
    assert!(out.join("__").join("escaped.ks").is_file());
}

// ------------------------------------------------------------------ actual games (local only)

fn real_games() -> Option<PathBuf> {
    std::env::var_os("AUDIT_REAL_GAMES").map(PathBuf::from).filter(|p| p.is_dir())
}

/// Full pipeline on a copy of an actual game. Decryption that yields noise
/// must stop at extraction with the game untouched.
fn real_roundtrip(fixture: &str, select: &[&str], title: Option<&str>) {
    let Some(root) = real_games() else {
        eprintln!("AUDIT_REAL_GAMES not set; skipping");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let e = need_real_tool!(tmp.path());
    let game = tmp.path().join("game");
    fs::create_dir_all(&game).unwrap();
    for file in select {
        fs::copy(root.join(fixture).join(file), game.join(file)).unwrap();
    }
    let original: Vec<_> = select.iter().map(|f| ((*f).to_string(), sha(&game.join(f)))).collect();
    let mut pipe = pipeline(&e, &game, settings("quick", 40, 16), fake().0);
    pipe.log = Arc::new(|s| eprintln!("{s}"));
    if let Some(title) = title {
        pipe.p.update_meta(|m| m.engine.as_mut().unwrap().set("xp3_title", title)).unwrap();
    }
    let st = match pipe.extract() {
        Ok(st) => st,
        Err(err) => {
            eprintln!("REAL {fixture}: extraction refused: {err}");
            assert!(err.to_string().contains("decrypt") || err.to_string().contains("readable"), "{err}");
            for (f, h) in &original {
                assert_eq!(sha(&game.join(f)), *h);
            }
            assert!(pipe.p.results_len() == 0 && !pipe.installed());
            return;
        }
    };
    eprintln!("REAL {fixture}: {} live, {} skipped, {} files", st.lines, st.skipped, st.files);
    assert!(st.lines > 0);
    for rel in engines::list_files(&pipe.p.work("scripts"), Some(&[".ks"])) {
        let (text, _) = ks::decode(&fs::read(engines::join_rel(&pipe.p.work("scripts"), &rel)).unwrap());
        assert!(!ks::garbled(&text), "{rel} accepted as text");
    }
    pipe.translate(None, false).unwrap();
    let build = pipe.build().unwrap();
    assert_eq!(build.applied + build.blocked, st.lines);
    if pipe.p.meta().engine.as_ref().unwrap().engine_id == "kirikiri" {
        for rel in engines::list_files(&pipe.p.work("json_tl/ks"), Some(&[".json"])) {
            let script = rel.strip_suffix(".json").unwrap();
            let (before, enc) = ks::decode(&fs::read(engines::join_rel(&pipe.p.work("scripts"), script)).unwrap());
            let entries = engines::read_json_messages(&engines::join_rel(&pipe.p.work("json_tl/ks"), &rel)).unwrap();
            let expected = ks::encode(&ks::apply_entries(&before, &entries).unwrap(), enc);
            assert_eq!(fs::read(engines::join_rel(&pipe.p.work("patched"), script)).unwrap(), expected);
        }
    }
    assert!(!pipe.install().unwrap().is_empty());
    pipe.install().unwrap();
    pipe.restore().unwrap();
    for (f, h) in &original {
        assert_eq!(sha(&game.join(f)), *h, "restore {f}");
    }
    assert_eq!(engines::list_files(&game, None).len(), select.len());
    if fixture.starts_with("Sakura") {
        assert_eq!(pipe.p.meta().engine.as_ref().unwrap().details["verify_mismatches"], 0);
    }
}

#[test]
fn real_sakura_moyu_fvp() {
    real_roundtrip("Sakura-Moyu-Trial", &["Sakura.hcb"], None);
}

#[test]
fn real_fate_trial_encrypted_kirikiri() {
    real_roundtrip("Fate-Trial/app", &["data.xp3"], Some("Fate/stay night"));
}

#[test]
fn real_true_remembrance_patch() {
    real_roundtrip("True-Remembrance-Patch", &["patch.xp3"], None);
}

#[test]
fn reextract_installed_favorite_keeps_original_source() {
    let Some(root) = real_games() else {
        eprintln!("AUDIT_REAL_GAMES not set; skipping");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let e = need_real_tool!(tmp.path());
    let game = tmp.path().join("game");
    fs::create_dir_all(&game).unwrap();
    fs::copy(root.join("Sakura-Moyu-Trial/Sakura.hcb"), game.join("Sakura.hcb")).unwrap();
    let pipe = pipeline(&e, &game, settings("quick", 40, 16), fake().0);
    pipe.extract().unwrap();
    let before = pipe.p.lines();
    pipe.translate(None, false).unwrap();
    pipe.build().unwrap();
    pipe.install().unwrap();
    pipe.extract().unwrap();
    let after = pipe.p.lines();
    pipe.restore().unwrap();
    assert_eq!(before.len(), after.len(), "re-extract read installed English instead of backed-up Japanese");
    assert!(before.iter().zip(after.iter()).all(|(a, b)| a.jp == b.jp));
}

// ------------------------------------------------------------------ helpers

fn zip_write(path: &Path, files: &[(&str, &[u8])]) {
    use std::io::Write;
    let mut z = zip::ZipWriter::new(fs::File::create(path).unwrap());
    for (name, data) in files {
        z.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
        z.write_all(data).unwrap();
    }
    z.finish().unwrap();
}

fn zip_read(path: &Path, name: &str) -> Vec<u8> {
    use std::io::Read;
    let mut z = zip::ZipArchive::new(fs::File::open(path).unwrap()).unwrap();
    let mut out = Vec::new();
    z.by_name(name).unwrap().read_to_end(&mut out).unwrap();
    out
}
