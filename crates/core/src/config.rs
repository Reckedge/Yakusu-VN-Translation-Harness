//! App-wide settings and folders.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

pub const APP_NAME: &str = "Yakusoku";

/// Where things live: the user's data folder and the bundled tools.
#[derive(Clone, Debug)]
pub struct Env {
    pub home: PathBuf,
    pub tools: PathBuf,
}

impl Env {
    pub fn new(home: impl Into<PathBuf>, tools: impl Into<PathBuf>) -> Env {
        let e = Env { home: home.into(), tools: tools.into() };
        let _ = std::fs::create_dir_all(e.projects_dir());
        e
    }

    /// The standard data folder (`YAKUSOKU_HOME` overrides it).
    pub fn system(tools: impl Into<PathBuf>) -> Env {
        let home = match std::env::var_os("YAKUSOKU_HOME") {
            Some(h) if !h.is_empty() => PathBuf::from(h),
            _ if cfg!(windows) => dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")).join(APP_NAME),
            _ => dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".yakusoku"),
        };
        Env::new(home, tools)
    }

    pub fn projects_dir(&self) -> PathBuf {
        self.home.join("projects")
    }

    pub fn settings_path(&self) -> PathBuf {
        self.home.join("settings.json")
    }

    /// Find a bundled tool (msg-tool, VNTextPatch, ...).
    /// `YAKUSOKU_MSG_TOOL`-style variables override the search.
    pub fn tool_path(&self, name: &str) -> Option<PathBuf> {
        let exe = if cfg!(windows) && !name.ends_with(".exe") { format!("{name}.exe") } else { name.to_string() };
        let var = format!("YAKUSOKU_{}", name.to_uppercase().replace('-', "_"));
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(v) = std::env::var_os(&var) {
            if !v.is_empty() {
                candidates.push(PathBuf::from(v));
            }
        }
        candidates.push(self.tools.join(&exe));
        candidates.push(self.tools.join(name).join(&exe));
        candidates.push(self.home.join("tools").join(&exe));
        if let Some(found) = candidates.into_iter().find(|c| c.is_file()) {
            return Some(found);
        }
        which_in_path(&exe)
    }

    pub fn load_settings(&self) -> Settings {
        std::fs::read_to_string(self.settings_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save_settings(&self, s: &Settings) -> std::io::Result<()> {
        write_json_atomic(&self.settings_path(), &serde_json::to_value(s)?)
    }
}

fn which_in_path(exe: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(exe)).find(|p| p.is_file())
}

pub fn write_json_atomic(path: &Path, v: &Value) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(v)?)?;
    std::fs::rename(&tmp, path)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub provider: String,
    /// provider -> API key
    pub keys: IndexMap<String, String>,
    /// provider -> custom base URL
    pub base_urls: IndexMap<String, String>,
    /// quick | recommended | best | custom
    pub preset: String,
    pub translate_model: IndexMap<String, String>,
    pub review_model: IndexMap<String, String>,
    pub review: bool,
    /// 0 = no cap
    pub budget_usd: f64,
    pub batch_size: usize,
    pub context_lines: usize,
    pub parallel: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_game: Option<String>,
    /// Anything else the interface stores.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            provider: "anthropic".into(),
            keys: IndexMap::new(),
            base_urls: IndexMap::new(),
            preset: "recommended".into(),
            translate_model: IndexMap::new(),
            review_model: IndexMap::new(),
            review: true,
            budget_usd: 0.0,
            batch_size: 40,
            context_lines: 10,
            parallel: 4,
            last_game: None,
            extra: Map::new(),
        }
    }
}

impl Settings {
    pub fn key(&self) -> &str {
        self.keys.get(&self.provider).map(String::as_str).unwrap_or("")
    }
}
