//! Thin wrapper around lifegpc/msg-tool (GPL-3.0, bundled as a separate program).
//!
//! <https://github.com/lifegpc/msg-tool>

use super::{list_files, run_tool, Ctx};
use crate::error::{user, Result};
use std::path::Path;

fn exe(cx: &Ctx) -> Result<String> {
    match cx.env.tool_path("msg-tool").or_else(|| cx.env.tool_path("msg_tool")) {
        Some(p) => Ok(p.to_string_lossy().to_string()),
        None => user("msg-tool is missing. Reinstall Yakusu (it ships inside the app)."),
    }
}

fn s(p: &Path) -> String {
    p.to_string_lossy().to_string()
}

fn tail(out: &str) -> String {
    let lines: Vec<&str> = out.trim().lines().filter(|l| !l.trim().is_empty()).collect();
    let t = lines[lines.len().saturating_sub(12)..].join("\n");
    if t.is_empty() {
        "(no output)".into()
    } else {
        t
    }
}

fn opt(args: &mut Vec<String>, flag: &str, v: Option<&str>) {
    if let Some(v) = v {
        args.push(flag.into());
        args.push(v.into());
    }
}

pub fn export(cx: &Ctx, src: &Path, out_dir: &Path, script_type: Option<&str>) -> Result<()> {
    export_with(cx, src, out_dir, script_type, &[])
}

pub fn export_with(cx: &Ctx, src: &Path, out_dir: &Path, script_type: Option<&str>, extra: &[&str]) -> Result<()> {
    std::fs::create_dir_all(out_dir)?;
    let mut args = vec![exe(cx)?, "export".into(), "-x".into(), "2".into(), "-T".into(), "json".into(), "-r".into()];
    opt(&mut args, "-t", script_type);
    args.extend(extra.iter().map(|e| e.to_string()));
    args.push(s(src));
    args.push(s(out_dir));
    let (code, out) = run_tool(&args, cx)?;
    let produced = !list_files(out_dir, Some(&[".json"])).is_empty();
    if code != 0 && !produced {
        return user(format!("msg-tool could not read the scripts:\n{}", tail(&out)));
    }
    if code != 0 {
        cx.log("Some files could not be read and were left untranslated (see above).");
    }
    Ok(())
}

pub fn import(cx: &Ctx, src: &Path, json_dir: &Path, out_dir: &Path, script_type: Option<&str>) -> Result<()> {
    std::fs::create_dir_all(out_dir)?;
    let mut args = vec![exe(cx)?, "import".into(), "-x".into(), "2".into(), "-T".into(), "json".into(), "-r".into()];
    opt(&mut args, "-t", script_type);
    args.extend([s(src), s(json_dir), s(out_dir)]);
    let (code, out) = run_tool(&args, cx)?;
    if code != 0 {
        return user(format!("msg-tool could not write the translated scripts:\n{}", tail(&out)));
    }
    Ok(())
}

/// Unpack an archive into `out_dir`.
///
/// msg-tool strips the last extension from the output path ("work/data.xp3"
/// becomes "work/data"), so `out_dir` must not contain a dot in its last
/// component. That is checked here rather than trusted.
pub fn unpack(cx: &Ctx, archive: &Path, out_dir: &Path, archive_type: Option<&str>, extra: &[String]) -> Result<()> {
    let last = out_dir.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    debug_assert!(!last.contains('.'), "msg-tool would write to a different folder than {last}");
    std::fs::create_dir_all(out_dir)?;
    let mut args = vec![exe(cx)?, "unpack".into(), "-x".into(), "2".into()];
    opt(&mut args, "-t", archive_type);
    args.extend(extra.iter().cloned());
    args.extend([s(archive), s(out_dir)]);
    let (code, out) = run_tool(&args, cx)?;
    if code != 0 {
        let name = archive.file_name().unwrap_or_default().to_string_lossy();
        return user(format!("msg-tool could not open {name}:\n{}", tail(&out)));
    }
    if list_files(out_dir, None).is_empty() {
        let name = archive.file_name().unwrap_or_default().to_string_lossy();
        return user(format!("msg-tool opened {name} but no files arrived in {}:\n{}", out_dir.display(), tail(&out)));
    }
    Ok(())
}

pub fn pack(cx: &Ctx, src_dir: &Path, archive: &Path, archive_type: &str, extra: &[String]) -> Result<()> {
    if let Some(d) = archive.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut args =
        vec![exe(cx)?, "pack".into(), "-x".into(), "2".into(), "-r".into(), "-t".into(), archive_type.into()];
    args.extend(extra.iter().cloned());
    args.extend([s(src_dir), s(archive)]);
    let (code, out) = run_tool(&args, cx)?;
    if code != 0 || !archive.exists() {
        let name = archive.file_name().unwrap_or_default().to_string_lossy();
        return user(format!("msg-tool could not build {name}:\n{}", tail(&out)));
    }
    Ok(())
}
