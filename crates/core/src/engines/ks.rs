//! Kirikiri KAG `.ks` scripts: read and write text in place.
//!
//! KAG is line based: `*label`, `@command`, `; comment`, `[iscript]` blocks,
//! and everything else is text with inline `[tags]`. A physical newline inside
//! text is not a line break in KAG; only `[r]` is. So consecutive text lines
//! are joined into one unit (ending at a page break, a command, a label or a
//! speaker line), translated, and written back onto the unit's first line with
//! the rest blanked. Command lines never move, so backgrounds, sprites and
//! voices stay in sync.

use crate::protect::JP_RE;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

static NAME_LINE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*([【［])([^】］]{1,24})([】］])\s*$").unwrap());
static NAME_PREFIX_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(【)([^】]{1,24})(】)").unwrap());
static TAG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[[^\]]*\]").unwrap());
static PAGE_END_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[(p|pg|np|cm|er|s)\b[^\]]*\]\s*$").unwrap());
static STANDALONE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\[(link|sel|select|button|glink|choice)\b").unwrap());
static SCRIPT_START: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\s*(@iscript|\[iscript\]|@macro|\[macro\b)").unwrap());
static SCRIPT_END: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\s*(@endscript|\[endscript\]|@endmacro|\[endmacro\])").unwrap());
static SPEAKER_TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[([^\s\]=\[]{1,16})(?:\s[^\]]*)?\]\s*$").unwrap());
static SPEAKER_CMD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^[@\[](?:nm|name|cn|talk|Talk|speaker|名前)\b[^\]]*?(?:t|text|name|n)\s*=\s*"?([^"\s\]]+)"#).unwrap()
});
static LEADING_WS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*").unwrap());

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub names: Option<Vec<String>>,
    pub message: String,
}

#[derive(Clone, Debug, Default)]
pub struct Unit {
    pub lines: Vec<usize>,
    pub message: String,
    pub name: Option<String>,
    /// A separate 【name】 line that belongs to this unit.
    pub name_line: Option<usize>,
    pub name_brackets: Option<(String, String)>,
    standalone: bool,
}

/// Returns (text, encoding label). Kirikiri accepts UTF-16LE with BOM, UTF-8
/// with BOM, or Shift-JIS.
pub fn decode(data: &[u8]) -> (String, &'static str) {
    if data.starts_with(&[0xFF, 0xFE]) {
        let (t, _, _) = encoding_rs::UTF_16LE.decode(&data[2..]);
        return (t.into_owned(), "utf-16");
    }
    if data.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return (String::from_utf8_lossy(&data[3..]).into_owned(), "utf-8-sig");
    }
    // No BOM: UTF-8 is very unlikely to decode by accident, so try it first.
    if let Ok(t) = std::str::from_utf8(data) {
        if !t.is_ascii() {
            return (t.to_string(), "utf-8");
        }
    }
    let (t, _, _) = encoding_rs::SHIFT_JIS.decode(data);
    (t.into_owned(), "cp932")
}

pub fn encode(text: &str, label: &str) -> Vec<u8> {
    match label {
        "utf-16" => {
            let mut out = vec![0xFF, 0xFE];
            for u in text.encode_utf16() {
                out.extend_from_slice(&u.to_le_bytes());
            }
            out
        }
        "utf-8-sig" => {
            let mut out = vec![0xEF, 0xBB, 0xBF];
            out.extend_from_slice(text.as_bytes());
            out
        }
        "utf-8" => text.as_bytes().to_vec(),
        _ => crate::textfit::encode_sjis(text),
    }
}

fn is_text_line(stripped: &str) -> bool {
    match stripped.chars().next() {
        None | Some('*') | Some('@') | Some(';') => false,
        // Lines made only of tags ("[cm]", "[wait time=200]") are commands.
        _ => !TAG_RE.replace_all(stripped, "").trim().is_empty(),
    }
}

/// Speaker named by a command line, e.g. `[桜 voice=sak001]` or `@nm t="桜"`.
fn speaker_from_command(s: &str) -> Option<String> {
    if let Some(c) = SPEAKER_CMD_RE.captures(s) {
        if JP_RE.is_match(&c[1]) {
            return Some(c[1].to_string());
        }
    }
    if let Some(c) = SPEAKER_TAG_RE.captures(s) {
        if JP_RE.is_match(&c[1]) {
            return Some(c[1].to_string());
        }
    }
    None
}

fn clean(line: &str) -> &str {
    line.strip_suffix('\r').unwrap_or(line).trim()
}

/// Split a .ks file into translation units.
pub fn parse(text: &str) -> Vec<Unit> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut units: Vec<Unit> = Vec::new();
    let mut cur: Option<Unit> = None;
    // (line index, name, open bracket, close bracket); line None = context only.
    let mut pending: Option<(Option<usize>, String, Option<(String, String)>)> = None;
    let mut in_script = false;

    let close = |cur: &mut Option<Unit>, units: &mut Vec<Unit>| {
        if let Some(mut u) = cur.take() {
            u.message = u.lines.iter().map(|&i| clean(lines[i])).collect::<String>();
            if JP_RE.is_match(&TAG_RE.replace_all(&u.message, "")) || u.name.is_some() {
                units.push(u);
            }
        }
    };

    for (i, raw) in lines.iter().enumerate() {
        let s = clean(raw);
        if in_script {
            if SCRIPT_END.is_match(s) {
                in_script = false;
            }
            continue;
        }
        if SCRIPT_START.is_match(s) {
            close(&mut cur, &mut units);
            in_script = true;
            continue;
        }
        if !is_text_line(s) {
            close(&mut cur, &mut units);
            if let Some(spk) = speaker_from_command(s) {
                pending = Some((None, spk, None)); // context only: never rewritten
            }
            continue;
        }
        if let Some(c) = NAME_LINE_RE.captures(s) {
            if JP_RE.is_match(&c[2]) && !STANDALONE_RE.is_match(s) {
                close(&mut cur, &mut units);
                pending = Some((Some(i), c[2].to_string(), Some((c[1].to_string(), c[3].to_string()))));
                continue;
            }
        }
        let standalone = STANDALONE_RE.is_match(s);
        if cur.is_none() || standalone || cur.as_ref().map(|u| u.standalone).unwrap_or(false) {
            close(&mut cur, &mut units);
            let mut u = Unit { standalone, ..Default::default() };
            if let Some((line, name, brackets)) = pending.take() {
                u.name_line = line;
                u.name = Some(name);
                u.name_brackets = brackets;
            }
            cur = Some(u);
        }
        cur.as_mut().unwrap().lines.push(i);
        if PAGE_END_RE.is_match(s) {
            close(&mut cur, &mut units);
        }
    }
    close(&mut cur, &mut units);
    units
}

/// msg-tool/VNTextPatch-style entries for a .ks file.
pub fn export_entries(text: &str) -> Vec<Entry> {
    parse(text)
        .into_iter()
        .map(|u| {
            let mut name = u.name.clone();
            if name.is_none() {
                if let Some(c) = NAME_PREFIX_RE.captures(&u.message) {
                    if JP_RE.is_match(&c[2]) {
                        name = Some(c[2].to_string());
                    }
                }
            }
            Entry { name, names: None, message: u.message }
        })
        .collect()
}

/// Write translated entries back. Entry order must match `export_entries`.
pub fn apply_entries(text: &str, entries: &[Entry]) -> crate::Result<String> {
    let mut lines: Vec<String> = text.split('\n').map(String::from).collect();
    let units = parse(text);
    if units.len() != entries.len() {
        return crate::error::user(format!(
            "The script changed since it was read ({} vs {} text blocks). Read the scripts again.",
            units.len(),
            entries.len()
        ));
    }
    for (u, e) in units.iter().zip(entries) {
        let mut msg = e.message.clone();
        let name = e.name.as_deref();
        if let (Some(nl), Some(name)) = (u.name_line, name) {
            let (o, c) = u.name_brackets.clone().unwrap_or(("【".into(), "】".into()));
            let indent = LEADING_WS.find(&lines[nl]).map(|m| m.as_str().to_string()).unwrap_or_default();
            let cr = if lines[nl].ends_with('\r') { "\r" } else { "" };
            lines[nl] = format!("{indent}{o}{name}{c}{cr}");
        } else if let Some(name) = name {
            if NAME_PREFIX_RE.is_match(&msg) {
                msg = NAME_PREFIX_RE
                    .replace(&msg, |c: &regex::Captures| format!("{}{}{}", &c[1], name, &c[3]))
                    .into_owned();
            }
        }
        let first = u.lines[0];
        let cr = if lines[first].ends_with('\r') { "\r" } else { "" };
        let indent = LEADING_WS.find(&lines[first]).map(|m| m.as_str().to_string()).unwrap_or_default();
        lines[first] = format!("{indent}{msg}{cr}");
        for &i in &u.lines[1..] {
            lines[i] = if lines[i].ends_with('\r') { "\r".into() } else { String::new() };
        }
    }
    Ok(lines.join("\n"))
}
