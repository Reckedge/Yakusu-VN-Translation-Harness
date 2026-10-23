//! Unit tests: placeholders, wrapping, KAG parsing, xp3, descrambling.

use yakusu_core::engines::{ks, xp3};
use yakusu_core::protect::{check, protect, restore};
use yakusu_core::textfit::{self, measure_box, wrap};

fn e(name: Option<&str>, msg: &str) -> ks::Entry {
    ks::Entry { name: name.map(String::from), names: None, message: msg.into() }
}

#[test]
fn protect_roundtrip_keeps_codes_and_lifts_quotes() {
    let p = protect("[cm]【桜】「おはよう！[l]いい天気だね」[p]", "[r]");
    assert_eq!(p.text, "おはよう！⟦2⟧いい天気だね");
    assert_eq!(p.name_prefix.as_ref().unwrap().1, "桜");
    let out = restore(&p, "Morning!⟦2⟧ Nice weather.", Some(("\"", "\"")), Some("Sakura"));
    assert_eq!(out, "[cm]【Sakura】\"Morning![l] Nice weather.\"[p]");
    // quotes None keeps the Japanese brackets
    assert_eq!(restore(&p, "Hi⟦2⟧", None, None), "[cm]【桜】「Hi[l]」[p]");
}

#[test]
fn check_flags_missing_and_extra_codes_and_japanese() {
    let p = protect("あ[l]い[l]う", "[r]");
    assert!(check(&p, Some("A ⟦1⟧ B ⟦2⟧ C")).is_empty());
    assert!(check(&p, Some("A ⟦1⟧ B C"))[0].contains("missing codes [2]"));
    assert!(check(&p, Some("A ⟦1⟧⟦1⟧ ⟦2⟧")).iter().any(|x| x.contains("unexpected")));
    assert!(check(&p, Some("A ⟦1⟧ B ⟦2⟧ うん")).contains(&"still has Japanese".to_string()));
    assert_eq!(check(&p, None), vec!["no answer".to_string()]);
}

#[test]
fn dropped_codes_are_reappended_and_invented_ones_dropped() {
    let p = protect("あ[l]い", "[r]");
    assert!(restore(&p, "A B", None, None).ends_with("[l]"));
    assert_eq!(restore(&p, "A⟦1⟧ B⟦9⟧⟦1⟧", None, None), "A[l] B");
}

#[test]
fn layout_breaks_removed_structural_breaks_kept() {
    let p = protect("一行目\n二行目[l]\n三行目", "\n");
    assert!(!p.text.contains('\n'));
    assert_eq!(p.codes, vec!["[l]\n".to_string()]);
}

#[test]
fn ruby_is_dropped() {
    assert_eq!(protect("僕は[ruby text=\"ねむ\"]眠い", "[r]").text, "僕は眠い");
    assert_eq!(protect("｜漢字《かんじ》だ", "\n").text, "漢字だ");
}

#[test]
fn wrap_respects_width_codes_and_hard_breaks() {
    let (text, n) = wrap("one two three four five six seven", 10, "\n");
    assert!(text.split('\n').all(|l| l.len() <= 10));
    assert_eq!(n, 4);
    let (text, n) = wrap("aaa[l][r]bbb ccc", 5, "[r]");
    assert_eq!(text, "aaa[l][r]bbb[r]ccc");
    assert_eq!(n, 3);
}

#[test]
fn measure_box_explicit_and_auto() {
    let msgs: Vec<String> = vec!["あいうえおかきくけこさしすせそ\nたちつてと".into(); 30];
    let b = measure_box(&msgs);
    assert_eq!((b.mode.as_str(), b.width, b.lines), ("explicit", 30, 2));
    assert_eq!(measure_box(&vec!["あ".repeat(50); 30]).mode, "auto");
}

#[test]
fn sjis_normalization() {
    assert_eq!(textfit::normalize_encoding("café — “hi”", "cp932", true), "cafe ― \"hi\"");
    assert_eq!(textfit::normalize_encoding("“hi”", "utf-16", false), "“hi”");
}

#[test]
fn ks_name_lines_and_blank_continuations() {
    let src = "【桜】\n「おはよう」\n「元気？」[p]\n@bg storage=x\n地の文。\n続き。[p]\n";
    let units = ks::parse(src);
    assert_eq!(units.iter().map(|u| u.name.clone()).collect::<Vec<_>>(), vec![Some("桜".to_string()), None]);
    let out =
        ks::apply_entries(src, &[e(Some("Sakura"), "\"Morning.\" \"You okay?\"[p]"), e(None, "Narration. More.[p]")])
            .unwrap();
    assert_eq!(
        out.split('\n').collect::<Vec<_>>(),
        vec!["【Sakura】", "\"Morning.\" \"You okay?\"[p]", "", "@bg storage=x", "Narration. More.[p]", "", ""]
    );
}

#[test]
fn ks_realistic_kag_patterns() {
    let src = [
        "*scene01|春",
        "\t[桜 voice=sak0001]",
        "\t「今日は[emb exp=\"f.name\"]くんと一緒だね」[p]",
        "@nm t=\"大雅\"",
        "「ああ」[p]",
        "[if exp=\"f.route==1\"]",
        "[font size=30]大きな声[resetfont]で叫んだ。[p]",
        "[endif]",
        "[macro name=foo]",
        "マクロの中の文字。",
        "[endmacro]",
        "地の文が[r]",
        "続いている。[p]",
    ]
    .join("\n");
    let units = ks::parse(&src);
    let names: Vec<Option<String>> = units.iter().map(|u| u.name.clone()).collect();
    assert_eq!(names, vec![Some("桜".into()), Some("大雅".into()), None, None]);
    assert!(units.iter().all(|u| !u.message.contains("マクロ")));
    let p = protect(&units[0].message, "[r]");
    assert!(p.codes.contains(&"[emb exp=\"f.name\"]".to_string()));
    let out = ks::apply_entries(
        &src,
        &[
            e(Some("Sakura"), "Today I'm with [emb exp=\"f.name\"]![p]"),
            e(Some("Taiga"), "\"Yeah.\"[p]"),
            e(None, "[font size=30]Loudly[resetfont] I shouted.[p]"),
            e(None, "Narration goes on.[p]"),
        ],
    )
    .unwrap();
    let lines: Vec<&str> = out.split('\n').collect();
    assert_eq!(lines[1], "\t[桜 voice=sak0001]"); // speaker tag is never rewritten
    assert_eq!(lines[3], "@nm t=\"大雅\"");
    assert_eq!(lines[2], "\tToday I'm with [emb exp=\"f.name\"]![p]");
    assert_eq!((lines[5], lines[7]), ("[if exp=\"f.route==1\"]", "[endif]"));
    assert_eq!(lines[9], "マクロの中の文字。");
    assert_eq!((lines[11], lines[12]), ("Narration goes on.[p]", ""));
}

#[test]
fn ks_trailing_break_before_command_is_kept() {
    let src = "一行目の文章。[r]\n@wait time=100\n二行目の文章。[p]\n";
    let units = ks::parse(src);
    let p = protect(&units[0].message, "[r]");
    assert_eq!(p.codes, vec!["[r]".to_string()]);
    assert!(restore(&p, "First line.", None, None).ends_with("[r]"));
}

#[test]
fn ks_crlf_and_encodings_roundtrip() {
    let src = "【桜】\r\n「おはよう」[p]\r\n";
    for enc in ["utf-16", "cp932", "utf-8-sig"] {
        let bytes = ks::encode(src, enc);
        let (back, label) = ks::decode(&bytes);
        assert_eq!(back, src);
        assert_eq!(label, enc);
    }
    let out = ks::apply_entries(src, &[e(Some("Sakura"), "\"Morning.\"[p]")]).unwrap();
    assert_eq!(out, "【Sakura】\r\n\"Morning.\"[p]\r\n");
}

#[test]
fn descramble_modes() {
    let text = "*start\n[cm]こんにちは";
    let raw: Vec<u8> = text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let mut m1 = raw.clone();
    for c in m1.chunks_mut(2) {
        let v = u16::from_le_bytes([c[0], c[1]]);
        let v = ((v & 0xAAAA) >> 1) | ((v & 0x5555) << 1);
        c.copy_from_slice(&v.to_le_bytes());
    }
    let mut d1 = vec![0xFE, 0xFE, 0x01, 0xFF, 0xFE];
    d1.extend(m1);
    assert_eq!(xp3::descramble(&d1).as_deref(), Some(text));
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut z, &raw).unwrap();
    let z = z.finish().unwrap();
    let mut d2 = vec![0xFE, 0xFE, 0x02, 0xFF, 0xFE];
    d2.extend((z.len() as i64).to_le_bytes());
    d2.extend((raw.len() as i64).to_le_bytes());
    d2.extend(z);
    assert_eq!(xp3::descramble(&d2).as_deref(), Some(text));
    assert_eq!(xp3::descramble(b"plain"), None);
}

#[test]
fn xp3_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(src.join("dir")).unwrap();
    std::fs::write(src.join("dir/a.ks"), "hello".repeat(100)).unwrap();
    std::fs::write(src.join("b.png"), b"\x89PNG").unwrap();
    let out = tmp.path().join("t.xp3");
    xp3::write_archive(&src, &out, &["dir/a.ks".into(), "b.png".into()]).unwrap();
    let idx = xp3::read_index(&out).unwrap();
    assert_eq!(idx.len(), 2);
    assert!(idx.iter().all(|e| e.adler == 0));
    let a = idx.iter().find(|e| e.name == "dir/a.ks").unwrap();
    assert_eq!(xp3::read_file(&out, a).unwrap(), "hello".repeat(100).into_bytes());
    assert!(xp3::looks_like_text(b"*start\n[cm]"));
    assert!(!xp3::looks_like_text(&[0u8, 1, 2, 3, 0x80, 0xFF, 4, 5, 6]));
}

#[test]
fn fuzzy_title_similarity() {
    use yakusu_core::engines::kirikiri::similarity;
    assert!((similarity("abcd", "abcd") - 1.0).abs() < 1e-9);
    assert!(similarity("sakuramoyu", "sakura moyu") > 0.9);
    assert!(similarity("xyz", "abc") < 0.1);
}

#[test]
fn glob_and_fnmatch() {
    use yakusu_core::engines::generic::{fnmatch, glob};
    assert!(fnmatch("DATA01.arc", "data0*.arc"));
    assert!(fnmatch("x.pfs.000", "*.pfs.*"));
    assert!(!fnmatch("x.pfs", "*.pfs.*"));
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("GameData")).unwrap();
    std::fs::write(tmp.path().join("GameData/data0.pack"), b"").unwrap();
    assert_eq!(glob(tmp.path(), &["gamedata/*.pack"]), vec!["GameData/data0.pack".to_string()]);
}
