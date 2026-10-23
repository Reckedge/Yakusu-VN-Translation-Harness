//! The desktop window. Everything happens in yakusoku-core; this shell only
//! forwards `call(command, args)` from the interface and supplies the folder
//! picker, drag and drop, and the bundled tools.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde_json::Value;
use std::sync::Arc;
use tauri::Manager;
use yakusoku_core::app::App;
use yakusoku_core::config::Env;
use yakusoku_core::llm;

struct Core(Arc<App>);

#[tauri::command]
async fn call(core: tauri::State<'_, Core>, cmd: String, args: Value) -> Result<Value, String> {
    let app = core.0.clone();
    // Core calls block (file work, the occasional API test): keep them off the UI thread.
    tauri::async_runtime::spawn_blocking(move || app.call(&cmd, &args))
        .await
        .map_err(|e| format!("Unexpected error: {e}"))?
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let tools = std::env::var_os("YAKUSOKU_TOOLS")
                .map(Into::into)
                .or_else(|| app.path().resource_dir().ok().map(|d| d.join("tools")))
                .unwrap_or_default();
            let factory = if std::env::var("YAKUSOKU_FAKE_LLM").as_deref() == Ok("1") {
                llm::fake::factory(Default::default(), None, std::time::Duration::from_millis(300))
            } else {
                llm::http_factory()
            };
            app.manage(Core(App::new(Env::system(tools), factory)));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![call])
        .run(tauri::generate_context!())
        .expect("error while running Yakusoku");
}
