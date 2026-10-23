//! The command surface the interface talks to: the main flow, job handling, Stop.

mod common;
use common::*;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use yakusu_core::app::App;

fn app(tmp: &std::path::Path) -> Arc<App> {
    App::with_opener(env(tmp), fake().0, Box::new(|_| Ok(())))
}

fn call(a: &Arc<App>, cmd: &str, args: Value) -> Value {
    a.call(cmd, &args).unwrap_or_else(|e| panic!("{cmd}: {e}"))
}

fn wait_job(a: &Arc<App>) -> Value {
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(20) {
        let st = call(a, "state", json!({}));
        if !st["job"]["running"].as_bool().unwrap_or(false) {
            return st;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("job did not finish");
}

#[test]
fn flow_through_commands() {
    let tmp = tempfile::tempdir().unwrap();
    let a = app(tmp.path());
    let game = make_game(&tmp.path().join("ゲーム"), SECOND_KS);
    let r = call(&a, "open", json!({"game_dir": game}));
    assert_eq!(r["matches"][0]["engine_id"], "kirikiri");
    call(&a, "settings", json!({"provider": "anthropic", "api_key": "sk-test-1234567890abcdef", "preset": "quick"}));
    let st = call(&a, "state", json!({}));
    assert!(!st.to_string().contains("sk-test-1234567890abcdef")); // key never sent back in full
    assert_eq!(st["settings"]["keys"]["anthropic"], "sk-test…cdef");

    call(&a, "run", json!({"action": "extract"}));
    let st = wait_job(&a);
    assert_eq!(st["project"]["total"], 8);
    assert!(st["job"]["error"].is_null(), "{}", st["job"]);
    let est = call(&a, "estimate", json!({"all": true}));
    let keys: Vec<&String> = est.as_object().unwrap().keys().collect();
    assert_eq!(keys.len(), 3);
    assert!(est["recommended"]["total"][0].as_f64().unwrap() > est["quick"]["total"][0].as_f64().unwrap());
    let pf = call(&a, "preflight", json!({}));
    assert!(pf["notes"].as_array().unwrap().iter().any(|n| n["text"].as_str().unwrap().contains("test patch")));

    call(&a, "run", json!({"action": "test"}));
    let st = wait_job(&a);
    assert!(st["project"]["installed"].as_bool().unwrap(), "{}", st["job"]);
    assert!(st["job"]["error"].is_null());
    let prev = call(&a, "preview", json!({}));
    assert!(prev["lines"][0]["lines"].as_array().unwrap().iter().all(|x| x.is_string()));

    let lines = call(&a, "lines", json!({"filter": "all"}));
    assert_eq!(lines["total"], 8);
    let lid = lines["rows"][0]["id"].as_str().unwrap().to_string();
    let r = call(&a, "line", json!({"id": lid, "en": "Edited by hand."}));
    assert_eq!(r["ok"], true);
    let r = call(&a, "lines", json!({"q": "edited by hand"}));
    assert_eq!(r["total"], 1);
    assert_eq!(r["rows"][0]["stage"], "manual");
    // Clearing a line queues it for translation again.
    let untranslated = call(&a, "lines", json!({"filter": "untranslated"}))["total"].as_u64().unwrap();
    call(&a, "line", json!({"id": lid, "en": ""}));
    assert_eq!(call(&a, "lines", json!({"filter": "untranslated"}))["total"].as_u64().unwrap(), untranslated + 1);
    // Skip / unskip
    call(&a, "line", json!({"id": lid, "skip": true}));
    assert_eq!(call(&a, "lines", json!({"filter": "skipped"}))["total"], 1);
    call(&a, "line", json!({"id": lid, "skip": false}));

    // CSV round trip
    let path = call(&a, "export_csv", json!({}))["path"].as_str().unwrap().to_string();
    let csv = std::fs::read_to_string(&path).unwrap();
    assert!(csv.starts_with('\u{feff}') && csv.contains("japanese"));
    let edited = csv
        .lines()
        .enumerate()
        .map(|(i, l)| {
            if i == 2 {
                let mut f: Vec<String> = l.split(',').map(String::from).collect();
                f[3] = "CSV line".into();
                f.join(",")
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let r = call(&a, "import_csv", json!({"text": edited}));
    assert_eq!(r["changed"], 1);
    assert!(a.call("import_csv", &json!({"text": "a,b\n1,2"})).is_err());

    // Style and glossary
    call(
        &a,
        "save_style",
        json!({"style": "Past tense.", "glossary": {"names": {"桜": "Sakura", "": "x"}, "terms": [{"jp": "", "en": "y"}]}}),
    );
    let g = call(&a, "get_style", json!({}));
    assert_eq!(g["style"], "Past tense.");
    assert_eq!(g["glossary"]["names"].as_object().unwrap().len(), 1);

    // Box settings
    call(&a, "box", json!({"width": 500, "lines": 0, "quotes": "straight"}));
    let st = call(&a, "state", json!({}));
    assert_eq!((st["project"]["box"]["width"].as_u64(), st["project"]["box"]["lines"].as_u64()), (Some(200), Some(1)));
    assert_eq!(st["project"]["settings"]["quotes"], "straight");

    call(&a, "run", json!({"action": "restore"}));
    let st = wait_job(&a);
    assert!(!st["project"]["installed"].as_bool().unwrap());
    assert!(!game.join("patch.xp3").exists());
}

#[test]
fn second_job_is_refused_while_running() {
    let tmp = tempfile::tempdir().unwrap();
    let a = app(tmp.path());
    let game = make_game(&tmp.path().join("g"), SECOND_KS);
    call(&a, "open", json!({"game_dir": game}));
    let gate = Arc::new(AtomicBool::new(false));
    let g2 = gate.clone();
    a.start("slow", move || {
        while !g2.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(json!({}))
    })
    .unwrap();
    let e = a.call("run", &json!({"action": "extract"})).unwrap_err();
    assert!(e.contains("already running"));
    gate.store(true, Ordering::SeqCst);
    wait_job(&a);
}

#[test]
fn stop_reaches_the_running_pipeline() {
    let tmp = tempfile::tempdir().unwrap();
    let a = app(tmp.path());
    let game = make_game(&tmp.path().join("g"), SECOND_KS);
    call(&a, "open", json!({"game_dir": game}));
    call(&a, "settings", json!({"api_key": "k"}));
    call(&a, "run", json!({"action": "extract"}));
    wait_job(&a);
    let pipe = a.pipeline().unwrap();
    let seen = Arc::new(AtomicBool::new(false));
    let s2 = seen.clone();
    a.start("probe", move || {
        for _ in 0..200 {
            if pipe.cancel.load(Ordering::SeqCst) {
                s2.store(true, Ordering::SeqCst);
                return Err(yakusu_core::Error::Cancelled);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(json!({}))
    })
    .unwrap();
    call(&a, "cancel", json!({}));
    let st = wait_job(&a);
    assert!(seen.load(Ordering::SeqCst));
    assert_eq!(st["job"]["stopped"], true);
    // The next job starts with Stop cleared.
    call(&a, "run", json!({"action": "extract"}));
    let st = wait_job(&a);
    assert!(st["job"].get("stopped").is_none() && st["job"]["error"].is_null(), "{}", st["job"]);
}

#[test]
fn errors_are_user_facing_and_recent_games_listed() {
    let tmp = tempfile::tempdir().unwrap();
    let a = app(tmp.path());
    assert!(a.call("open", &json!({"game_dir": tmp.path().join("missing")})).unwrap_err().contains("doesn't exist"));
    assert!(a.call("run", &json!({"action": "extract"})).unwrap_err().contains("game folder"));
    assert!(a.call("nope", &json!({})).is_err());
    let g1 = make_game(&tmp.path().join("one"), SECOND_KS);
    let g2 = make_game(&tmp.path().join("two"), SECOND_KS);
    call(&a, "open", json!({"game_dir": g1}));
    call(&a, "open", json!({"game_dir": g2}));
    let recent = call(&a, "recent", json!({}));
    assert_eq!(recent.as_array().unwrap().len(), 1);
    assert_eq!(recent[0]["name"], "one");
    call(&a, "forget", json!({"game_dir": g1}));
    assert_eq!(call(&a, "recent", json!({})).as_array().unwrap().len(), 0);
    // Settings and the last game persist across restarts.
    drop(a);
    let b = app(tmp.path());
    let st = call(&b, "state", json!({}));
    assert_eq!(st["project"]["name"], "two");
    // Test the key with the stand-in model.
    call(&b, "settings", json!({"api_key": "k"}));
    assert_eq!(call(&b, "test_key", json!({}))["reply"], "OK");
    let games = call(&b, "xp3_games", json!({"q": "11eyes"}));
    assert!(games["games"].as_array().unwrap().len() >= 2);
}
