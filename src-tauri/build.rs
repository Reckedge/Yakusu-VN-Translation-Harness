//! Packs the third-party tools (tools/, filled by packaging/fetch_tools.ps1)
//! into the executable, so the app is a single file you double-click. They are
//! unpacked into the data folder on first launch.

use std::io::Write;
use std::path::{Path, PathBuf};

fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, base, out);
        } else {
            let rel = p.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/");
            if rel != "README.md" {
                out.push((rel, p));
            }
        }
    }
}

fn main() {
    let tools = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tools");
    println!("cargo:rerun-if-changed={}", tools.display());
    let mut files = Vec::new();
    walk(&tools, &tools, &mut files);
    files.sort();
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o755);
        for (rel, path) in &files {
            println!("cargo:rerun-if-changed={}", path.display());
            z.start_file(rel.as_str(), opts).unwrap();
            z.write_all(&std::fs::read(path).unwrap()).unwrap();
        }
        z.finish().unwrap();
    }
    let data = buf.into_inner();
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let hash = if files.is_empty() {
        String::new()
    } else {
        sha1_smol::Sha1::from(&data).digest().to_string()[..12].to_string()
    };
    std::fs::write(out.join("tools.zip"), if files.is_empty() { Vec::new() } else { data }).unwrap();
    std::fs::write(out.join("tools.hash"), hash).unwrap();
    tauri_build::build()
}
