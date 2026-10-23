//! Archive plumbing against the real msg-tool binary (skipped unless
//! YAKUSU_REAL_MSG_TOOL points at one). These caught the dotted-folder and
//! YU-RIS version bugs that a well-behaved stand-in hides.

mod common;
use common::*;
use std::path::{Path, PathBuf};
use yakusu_core::engines::{generic, msgtool, xp3, Ctx};
use yakusu_core::project::Project;

fn real_tool(tmp: &Path) -> Option<PathBuf> {
    let src = PathBuf::from(std::env::var_os("YAKUSU_REAL_MSG_TOOL")?);
    let tools = tmp.join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    let dst = tools.join(if cfg!(windows) { "msg-tool.exe" } else { "msg-tool" });
    std::fs::copy(src, &dst).unwrap();
    Some(dst)
}

fn files(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut v: Vec<_> = yakusu_core::engines::list_files(dir, None)
        .into_iter()
        .map(|r| (r.replace('\\', "/"), std::fs::read(yakusu_core::engines::join_rel(dir, &r)).unwrap()))
        .collect();
    v.sort();
    v
}

#[test]
fn archives_roundtrip_through_the_real_tool() {
    let tmp = tempfile::tempdir().unwrap();
    if real_tool(tmp.path()).is_none() {
        eprintln!("YAKUSU_REAL_MSG_TOOL not set; skipping");
        return;
    }
    let env = env(tmp.path());
    let p = Project::open(tmp.path().join("proj")).unwrap();
    let log = |m: &str| eprintln!("{m}");
    let cx = Ctx { env: &env, project: &p, log: &log };
    let source = tmp.path().join("source");
    std::fs::create_dir_all(source.join("nested")).unwrap();
    let payload: Vec<u8> = (0..=255u8).cycle().take(256 * 1024).collect();
    std::fs::write(source.join("nested/payload.bin"), &payload).unwrap();
    std::fs::write(source.join("config.txt"), b"unchanged configuration\r\n").unwrap();
    let want = files(&source);
    for prof in generic::PROFILES.iter().filter(|p| p.archive_type.is_some() && !p.no_repack) {
        if prof.id == "qlie" {
            continue; // needs a game-specific key file inside the archive
        }
        let at = prof.archive_type.unwrap();
        let rel = match prof.id {
            "yuris" => "ysbin.ypf",
            "artemis" => "data.pfs",
            "bgi" => "data01.arc",
            "circus" => "data.pck",
            _ => "script.bin",
        };
        let arc = tmp.path().join(prof.id).join(rel);
        let mut extra: Vec<String> = prof.pack_extra.iter().map(|s| s.to_string()).collect();
        if prof.id == "yuris" {
            extra.extend(["--yuris-ypf-version".into(), "494".into()]);
        }
        msgtool::pack(&cx, &source, &arc, at, &extra).unwrap_or_else(|e| panic!("{}: {e}", prof.id));
        if prof.id == "yuris" {
            assert_eq!(generic::ypf_version(&arc), Some(494), "the version we read back is what rebuilding needs");
        }
        // Unpack into the folder name the engines use: no dot, so the files land there.
        let out = tmp.path().join(prof.id).join(generic::dir_key(rel));
        msgtool::unpack(&cx, &arc, &out, Some(at), &[]).unwrap_or_else(|e| panic!("{}: {e}", prof.id));
        assert_eq!(files(&out), want, "{}", prof.id);
    }
}

#[test]
fn a_dotted_output_folder_is_not_where_the_tool_writes() {
    // Documents the tool behaviour the engines work around.
    let tmp = tempfile::tempdir().unwrap();
    let Some(tool) = real_tool(tmp.path()) else { return };
    let source = tmp.path().join("source");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("test.bin"), b"original payload").unwrap();
    let arc = tmp.path().join("data.pfs");
    let run = |args: &[&str]| std::process::Command::new(&tool).args(args).output().unwrap();
    run(&["pack", "-x", "2", "-r", "-t", "artemis-arc", source.to_str().unwrap(), arc.to_str().unwrap()]);
    let dotted = tmp.path().join("requested.pfs");
    run(&["unpack", "-x", "2", "-t", "artemis-arc", arc.to_str().unwrap(), dotted.to_str().unwrap()]);
    assert!(!dotted.join("test.bin").exists());
    assert!(tmp.path().join("requested").join("test.bin").exists());
}

#[test]
fn kirikiri_archives_unpack_into_the_folder_we_scan() {
    let tmp = tempfile::tempdir().unwrap();
    if real_tool(tmp.path()).is_none() {
        return;
    }
    let env = env(tmp.path());
    let p = Project::open(tmp.path().join("proj")).unwrap();
    let log = |m: &str| eprintln!("{m}");
    let cx = Ctx { env: &env, project: &p, log: &log };
    let src = tmp.path().join("src");
    std::fs::create_dir_all(src.join("scenario")).unwrap();
    std::fs::write(src.join("scenario/a.ks"), "*start\n「テスト」[p]\n").unwrap();
    let arc = tmp.path().join("data.xp3");
    xp3::write_archive(&src, &arc, &["scenario/a.ks".into()]).unwrap();
    let out = p.work(&format!("unpack/{}", generic::dir_key("data.xp3")));
    // Same call the engine makes for encrypted archives (type auto-detected).
    msgtool::unpack(&cx, &arc, &out, None, &[]).unwrap();
    let got = files(&out);
    assert!(got.iter().any(|(n, _)| n.ends_with("a.ks")), "{got:?}");
}
