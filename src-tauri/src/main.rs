//! The desktop window. Everything happens in yakusu-core; this shell only
//! forwards `call(command, args)` from the interface and supplies the folder
//! picker, drag and drop, and the bundled tools.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::Manager;
use yakusu_core::app::App;
use yakusu_core::config::Env;
use yakusu_core::llm;

/// msg-tool, VNTextPatch and version.dll, zipped into the executable by build.rs.
static TOOLS_ZIP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tools.zip"));
static TOOLS_HASH: &str = include_str!(concat!(env!("OUT_DIR"), "/tools.hash"));

struct Core(Arc<App>);

#[tauri::command]
async fn call(core: tauri::State<'_, Core>, cmd: String, args: Value) -> Result<Value, String> {
    let app = core.0.clone();
    // Core calls block (file work, the occasional API test): keep them off the UI thread.
    tauri::async_runtime::spawn_blocking(move || app.call(&cmd, &args))
        .await
        .map_err(|e| format!("Unexpected error: {e}"))?
}

/// Unpack the embedded tools once per version into the data folder.
fn unpack_tools(home: &Path) -> Option<PathBuf> {
    if TOOLS_ZIP.is_empty() {
        return None;
    }
    let dir = home.join(format!("tools-{TOOLS_HASH}"));
    if dir.join(".complete").exists() {
        return Some(dir);
    }
    let tmp = home.join(format!("tools-{TOOLS_HASH}.part"));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(TOOLS_ZIP)).ok()?;
    z.extract(&tmp).ok()?;
    std::fs::write(tmp.join(".complete"), TOOLS_HASH).ok()?;
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::rename(&tmp, &dir).ok()?;
    // Older unpacked versions are no longer needed.
    if let Ok(rd) = std::fs::read_dir(home) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.starts_with("tools-") && e.path() != dir {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    Some(dir)
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let home = Env::system(PathBuf::new()).home;
            let tools = std::env::var_os("YAKUSU_TOOLS")
                .map(PathBuf::from)
                .or_else(|| unpack_tools(&home))
                .or_else(|| app.path().resource_dir().ok().map(|d| d.join("tools")))
                .unwrap_or_default();
            let factory = if std::env::var("YAKUSU_FAKE_LLM").as_deref() == Ok("1") {
                llm::fake::factory(Default::default(), None, std::time::Duration::from_millis(300))
            } else {
                llm::http_factory()
            };
            app.manage(Core(App::new(Env::new(home, tools), factory)));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![call])
        .run(tauri::generate_context!())
        .expect("error while running Yakusu");
}
