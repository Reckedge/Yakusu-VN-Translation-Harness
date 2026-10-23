//! End to end: fake Kirikiri game -> detect -> extract -> style -> translate ->
//! review -> fit -> build -> install -> restore, with the stand-in model.

mod common;
use common::*;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use yakusu_core::engines::{ks, xp3};
use yakusu_core::llm::LlmError;
use yakusu_core::Error;

#[test]
fn full_flow() {
    let tmp = tempfile::tempdir().unwrap();
    let env = env(tmp.path());
    let game = make_game(&tmp.path().join("My Game"), SECOND_KS);
    let before: Vec<(String, String)> = {
        let mut v: Vec<_> = std::fs::read_dir(&game)
            .unwrap()
            .flatten()
            .map(|e| (e.file_name().to_string_lossy().to_string(), sha(&e.path())))
            .collect();
        v.sort();
        v
    };
    let (factory, _) = fake();
    let pipe = pipeline(&env, &game, settings("recommended", 3, 2), factory);
    assert_eq!(pipe.p.meta().engine.as_ref().unwrap().engine_id, "kirikiri");

    let stats = pipe.extract().unwrap();
    let live = pipe.live_lines();
    // Name lines are attached as speakers, not translated; script blocks skipped.
    assert!(!live.iter().any(|l| l.jp.contains("未使用")));
    assert!(live.iter().any(|l| l.name.as_deref() == Some("桜")));
    assert_eq!(stats.lines, 8);
    assert_eq!(live.len(), 8);
    assert!(pipe.p.meta().text_box.as_ref().unwrap().width >= 24);

    let est = pipe.estimate();
    assert!(est["total"][0].as_f64().unwrap() > 0.0);

    pipe.draft_style().unwrap();
    assert_eq!(pipe.p.glossary().names["桜"], "Sakura");

    pipe.translate(None, false).unwrap();
    pipe.p.with_results(|res| {
        assert!(live.iter().all(|l| res.contains_key(&l.id)));
        assert!(res.values().all(|r| r.problems.is_empty()));
    });

    pipe.review(None).unwrap();
    assert!(pipe.p.with_results(|r| r.values().any(|r| r.stage == "review")));

    let out = pipe.build().unwrap();
    assert_eq!(out.applied, 8);
    let patch = pipe.p.work("out/patch.xp3");
    assert!(patch.exists());

    // The patch archive holds the translated scripts, readable by our reader.
    let entries = xp3::read_index(&patch).unwrap();
    let get = |n: &str| entries.iter().find(|e| e.name == n).unwrap();
    assert!(entries.iter().any(|e| e.name == "first.ks"));
    assert!(entries.iter().all(|e| e.adler == 0));
    let (text, enc) = ks::decode(&xp3::read_file(&patch, get("scenario/first.ks")).unwrap());
    assert_eq!(enc, "utf-16");
    assert!(text.contains("【Sakura】") && text.contains("【Taiga】"));
    assert!(text.contains("@bg storage=\"bg_room\"") && text.contains("@chara name=\"sakura\""));
    assert!(text.contains("f.flag = \"未使用\";")); // TJS untouched
    assert!(text.contains("[link target=*a]") && text.contains("[endlink][r]"));
    assert!(!text.contains("ruby"));
    let lines: Vec<&str> = text.split('\n').collect();
    let orig: Vec<&str> = FIRST_KS.split('\n').collect();
    assert_eq!(lines.len(), orig.len()); // nothing moved
    for (a, b) in lines.iter().zip(&orig) {
        if ["@", "*", "[iscript", "[endscript", "f.flag"].iter().any(|p| b.starts_with(p)) {
            assert_eq!(a, b);
        }
    }
    let (second, enc2) = ks::decode(&xp3::read_file(&patch, get("scenario/second.ks")).unwrap());
    assert_eq!(enc2, "cp932");
    assert!(second.contains("【Sakura】\u{201c}"), "{second}");

    // Install adds patch.xp3 only (game unencrypted), restore removes it.
    pipe.install().unwrap();
    assert!(game.join("patch.xp3").exists());
    for (f, h) in &before {
        assert_eq!(&sha(&game.join(f)), h); // originals untouched
    }
    // Re-extract after install must ignore our own patch.xp3.
    pipe.extract().unwrap();
    assert_eq!(pipe.live_lines().len(), 8);
    pipe.build().unwrap();
    pipe.install().unwrap();
    assert!(!game.join("patch2.xp3").exists());
    pipe.restore().unwrap();
    let mut after: Vec<String> =
        std::fs::read_dir(&game).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    after.sort();
    assert_eq!(after, before.iter().map(|b| b.0.clone()).collect::<Vec<_>>());
    assert!(pipe.p.spent_total() > 0.0);
}

#[test]
fn fit_pass_shortens_overflowing_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let env = env(tmp.path());
    let game = make_game(&tmp.path().join("g"), SECOND_KS);
    let pipe = pipeline(&env, &game, settings("quick", 10, 2), fake().0);
    pipe.extract().unwrap();
    pipe.translate(None, false).unwrap();
    pipe.p
        .update_meta(|m| {
            let b = m.text_box.as_mut().unwrap();
            b.mode = "explicit".into();
            b.width = 24;
            b.lines = 2;
            b.user_set = true;
        })
        .unwrap();
    let over = pipe.overflow_lines().unwrap();
    assert!(!over.is_empty());
    pipe.shorten(Some(over.clone())).unwrap();
    assert!(pipe.overflow_lines().unwrap().len() < over.len());
    // A user-set box survives re-reading the scripts.
    pipe.extract().unwrap();
    assert_eq!(pipe.p.meta().text_box.as_ref().unwrap().width, 24);
}

#[test]
fn budget_cap_stops() {
    let tmp = tempfile::tempdir().unwrap();
    let env = env(tmp.path());
    let game = make_game(&tmp.path().join("g"), SECOND_KS);
    let mut s = settings("best", 2, 1);
    s.budget_usd = 0.000001;
    let pipe = pipeline(&env, &game, s, fake().0);
    pipe.extract().unwrap();
    assert!(matches!(pipe.translate(None, false), Err(Error::Budget(_))));
}

#[test]
fn repeated_lines_are_translated_once() {
    let tmp = tempfile::tempdir().unwrap();
    let env = env(tmp.path());
    let game =
        make_game(&tmp.path().join("g"), "*a\n「……うん」[p]\n「……うん」[p]\n【桜】「……うん」[p]\n「……うん」[p]\n");
    let pipe = pipeline(&env, &game, settings("quick", 50, 1), fake().0);
    pipe.extract().unwrap();
    pipe.translate(None, false).unwrap();
    let rows: Vec<_> =
        pipe.live_lines().iter().filter(|l| l.jp.contains("うん")).map(|l| pipe.p.result(&l.id).unwrap()).collect();
    assert_eq!(rows.len(), 4);
    assert!(rows.iter().all(|r| r.translated()));
    // narration x3 -> 1 + 2 copies; Sakura's own line separate
    assert_eq!(rows.iter().filter(|r| r.model == "memory").count(), 2);
}

#[test]
fn truncated_batches_are_split_until_they_fit() {
    let tmp = tempfile::tempdir().unwrap();
    let env = env(tmp.path());
    let game = make_game(&tmp.path().join("g"), SECOND_KS);
    // Cuts off any reply covering more than 2 lines.
    let factory = fake_with(Box::new(|_s, u| {
        let part = u.split_once("TRANSLATE").map(|x| x.1)?;
        if part.matches("\"i\"").count() > 2 {
            Some(Err(LlmError::Truncated(Default::default())))
        } else {
            None
        }
    }));
    let pipe = pipeline(&env, &game, settings("quick", 8, 1), factory);
    pipe.extract().unwrap();
    pipe.translate(None, false).unwrap();
    pipe.p.with_results(|res| {
        assert!(pipe.live_lines().iter().all(|l| res.get(&l.id).is_some_and(|r| r.good())));
    });
    assert!(pipe.p.spent_total() > 0.0 || pipe.p.path("costs.jsonl").exists()); // truncated calls still logged
}

#[test]
fn garbage_replies_end_as_flagged_lines_not_crashes() {
    let tmp = tempfile::tempdir().unwrap();
    let env = env(tmp.path());
    let game = make_game(&tmp.path().join("g"), SECOND_KS);
    let factory =
        fake_with(Box::new(|_s, u| if u.contains("TRANSLATE") { Some(Ok("I can't do that.".into())) } else { None }));
    let pipe = pipeline(&env, &game, settings("quick", 4, 2), factory);
    pipe.extract().unwrap();
    pipe.translate(None, false).unwrap();
    pipe.p.with_results(|res| {
        assert!(pipe.live_lines().iter().all(|l| res.get(&l.id).is_some_and(|r| !r.problems.is_empty())));
    });
    // Flagged lines are retried on the next run.
    let pipe2 = yakusu_core::pipeline::Pipeline::new(env.clone(), pipe.p.clone(), settings("quick", 4, 2), fake().0);
    pipe2.translate(None, false).unwrap();
    pipe.p.with_results(|res| assert!(res.values().all(|r| r.good())));
}

#[test]
fn fatal_api_error_stops_the_run() {
    let tmp = tempfile::tempdir().unwrap();
    let env = env(tmp.path());
    let game = make_game(&tmp.path().join("g"), SECOND_KS);
    let factory = fake_with(Box::new(|_s, u| {
        if u.contains("TRANSLATE") {
            Some(Err(LlmError::Fatal("The API key was rejected (401).".into())))
        } else {
            None
        }
    }));
    let pipe = pipeline(&env, &game, settings("quick", 1, 3), factory);
    pipe.extract().unwrap();
    match pipe.translate(None, false) {
        Err(Error::Llm(m)) => assert!(m.contains("rejected")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn cancel_stops_and_keeps_finished_work() {
    let tmp = tempfile::tempdir().unwrap();
    let env = env(tmp.path());
    let game = make_game(&tmp.path().join("g"), SECOND_KS);
    let gate = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let first_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (g2, f2) = (gate.clone(), first_done.clone());
    let factory = fake_with(Box::new(move |s, u| {
        if u.contains("TRANSLATE") {
            f2.store(true, Ordering::SeqCst);
            let (m, c) = &*g2;
            let mut open = m.lock().unwrap();
            while !*open {
                open = c.wait(open).unwrap();
            }
            return Some(Ok(yakusu_core::llm::fake::reply(s, u)));
        }
        None
    }));
    let pipe = Arc::new(pipeline(&env, &game, settings("quick", 1, 1), factory));
    pipe.extract().unwrap();
    let p2 = pipe.clone();
    let t = std::thread::spawn(move || p2.translate(None, false));
    while !first_done.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    pipe.cancel.store(true, Ordering::SeqCst); // stop while the first batch is in flight
    {
        let (m, c) = &*gate;
        *m.lock().unwrap() = true;
        c.notify_all();
    }
    let r = t.join().unwrap();
    assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    assert_eq!(pipe.p.results_len(), 1); // the batch in flight is kept, nothing after it runs
}

#[test]
fn quote_modes_and_box_settings_apply_at_build() {
    let tmp = tempfile::tempdir().unwrap();
    let env = env(tmp.path());
    let game = make_game(&tmp.path().join("g"), SECOND_KS);
    let pipe = pipeline(&env, &game, settings("quick", 50, 1), fake().0);
    pipe.extract().unwrap();
    pipe.translate(None, false).unwrap();
    let line = pipe.live_lines().into_iter().find(|l| l.jp.contains("じゃあ")).unwrap();
    let r = pipe.p.result(&line.id).unwrap();
    for (mode, open) in [("keep", "「"), ("none", ""), ("straight", "\""), ("curly", "\u{201c}")] {
        pipe.p.update_meta(|m| m.settings.quotes = mode.into()).unwrap();
        let rc = pipe.render_ctx().unwrap();
        let (text, _) = pipe.render(&rc, &line, &r);
        assert!(
            text.starts_with(&format!("【Sakura】{open}T")) || text.starts_with(&format!("【桜】{open}T")),
            "{mode}: {text}"
        );
    }
}
