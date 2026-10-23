//! Keep engine control codes intact through translation.
//!
//! Exported scripts still contain inline codes: Kirikiri tags like `[l]` or
//! `<link target="*a">`, escape sequences, `%name;` variables, colour codes.
//! Models happily translate or drop those, which crashes or garbles the game.
//! Before a line goes out, every code becomes a numbered placeholder (⟦1⟧,
//! ⟦2⟧ …); afterwards the originals go back and every placeholder is checked.
//!
//! Presentation is handled here too:
//! - Line breaks between plain text are layout: removed, then re-wrapped for
//!   English. A break right after a code, or at the very start or end of a
//!   message, is structure and is kept with the codes.
//! - Furigana markup is dropped.
//! - A leading 【name】 and 「」/『』 around dialogue are lifted off and put back
//!   at build time, in English.

use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};

pub const PH_OPEN: char = '\u{27E6}';
pub const PH_CLOSE: char = '\u{27E7}';

pub static PH_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new("\u{27E6}(\\d+)\u{27E7}").unwrap());

const CODE_PATTERNS: &[&str] = &[
    r"\[[A-Za-z_@][^\]\n]{0,160}\]",                                     // KAG tags
    r"<[A-Za-z/][^<>\n]{0,160}>",                                        // msg-tool XML-ish tags
    r"\\[A-Za-z](?:\[[^\]\n]{0,20}\]|\{[^}\n]{0,20}\}|<[^>\n]{0,20}>)?", // \c[2] escapes
    r"%[A-Za-z][A-Za-z0-9_]{0,15};?",                                    // %name; variables
    r"\$[A-Za-z_][A-Za-z0-9_]{0,30}",                                    // $var
    r"\{[\x20-\x7E]{1,40}\}",                                            // {var}
    r"@[A-Za-z][A-Za-z0-9_]{0,20}",                                      // @commands
    r"#[0-9A-Fa-f]{6}(?:[0-9A-Fa-f]{2})?\b",                             // colour codes
    r"&(?:[a-z]{2,6}|#\d{1,6});",                                        // XML entities
];

pub static CODE_SRC: LazyLock<String> =
    LazyLock::new(|| CODE_PATTERNS.iter().map(|p| format!("(?:{p})")).collect::<Vec<_>>().join("|"));
pub static CODE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(&CODE_SRC).unwrap());

static RUBY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\[ruby\b[^\]]*\]|<ruby\b[^>]*>|</ruby>|\[/ruby\]|<rt>[^<]*</rt>|</?rb>|</?rt>").unwrap()
});
static AOZORA_RUBY_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[｜|]?([^｜|《》\s]+)《[^》]{1,20}》").unwrap());
static NAME_PREFIX_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(【|［)([^】］]{1,24})(】|］)").unwrap());
pub static JP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\u{3040}-\u{30ff}\u{3400}-\u{4dbf}\u{4e00}-\u{9fff}]").unwrap());
static INDENT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[\u{3000} \t]+").unwrap());
static LEAD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new("^(?:\u{27E6}\\d+\u{27E7})+").unwrap());
static TRAIL_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new("(?:\u{27E6}\\d+\u{27E7})+$").unwrap());

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Protected {
    /// What the model sees.
    pub text: String,
    pub codes: Vec<String>,
    pub open_quote: String,
    pub close_quote: String,
    pub indent: String,
    /// (open bracket, Japanese name, close bracket)
    pub name_prefix: Option<(String, String, String)>,
    /// Placeholders before the opening quote / after the closing quote.
    pub lead: String,
    pub trail: String,
}

pub fn has_japanese(s: &str) -> bool {
    JP_RE.is_match(&PH_RE.replace_all(s, ""))
}

pub fn placeholder(i: usize) -> String {
    format!("{PH_OPEN}{i}{PH_CLOSE}")
}

static CACHE: LazyLock<Mutex<HashMap<(String, String), Arc<Protected>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static TOKEN_RES: LazyLock<Mutex<HashMap<String, Arc<Regex>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn token_re(line_break: &str) -> Arc<Regex> {
    let mut map = TOKEN_RES.lock().unwrap();
    map.entry(line_break.to_string())
        .or_insert_with(|| {
            let mut breaks: Vec<&str> = vec![line_break, "\r\n", "\n"];
            breaks.retain(|b| !b.is_empty());
            breaks.sort_by_key(|b| std::cmp::Reverse(b.len()));
            breaks.dedup();
            let alt = breaks.iter().map(|b| regex::escape(b)).collect::<Vec<_>>().join("|");
            Arc::new(Regex::new(&format!("({alt})|({})", *CODE_SRC)).unwrap())
        })
        .clone()
}

/// Prepare a source message for the model. Cached: the same line is protected
/// many times over a run (translate, review, fit, build, preview).
pub fn protect(message: &str, line_break: &str) -> Arc<Protected> {
    let key = (message.to_string(), line_break.to_string());
    if let Some(p) = CACHE.lock().unwrap().get(&key) {
        return p.clone();
    }
    let p = Arc::new(protect_uncached(message, line_break));
    let mut c = CACHE.lock().unwrap();
    if c.len() > 250_000 {
        c.clear();
    }
    c.insert(key, p.clone());
    p
}

fn protect_uncached(message: &str, line_break: &str) -> Protected {
    let mut p = Protected::default();
    let mut s: String = message.to_string();
    if let Some(m) = INDENT_RE.find(&s) {
        p.indent = m.as_str().to_string();
        s = s[m.end()..].to_string();
    }
    s = RUBY_RE.replace_all(&s, "").into_owned();
    s = AOZORA_RUBY_RE.replace_all(&s, "$1").into_owned();

    let re = token_re(line_break);
    let mut codes: Vec<String> = Vec::new();
    let mut out = String::new();
    let mut pos = 0;
    let mut prev_code = false;
    for caps in re.captures_iter(&s) {
        let m = caps.get(0).unwrap();
        let before = &s[pos..m.start()];
        if !before.is_empty() {
            out.push_str(before);
            prev_code = false;
        }
        let tok = m.as_str();
        if caps.get(1).is_some() {
            // A line break.
            let rest = &s[m.end()..];
            let rest_text = CODE_RE.replace_all(rest, "");
            let rest_text =
                if line_break.is_empty() { rest_text.to_string() } else { rest_text.replace(line_break, "") };
            let at_edge = out.trim().is_empty() || rest_text.trim().is_empty();
            if prev_code && !codes.is_empty() {
                codes.last_mut().unwrap().push_str(tok); // structural: keep with the code
            } else if at_edge {
                // Separates this message from text around it in the script.
                codes.push(tok.to_string());
                out.push_str(&placeholder(codes.len()));
                prev_code = true;
            }
            // else: a layout break inside the text, dropped (we re-wrap).
        } else {
            codes.push(tok.to_string());
            out.push_str(&placeholder(codes.len()));
            prev_code = true;
        }
        pos = m.end();
    }
    out.push_str(&s[pos..]);
    let mut s = out.replace('\r', "");

    if let Some(m) = LEAD_RE.find(&s) {
        p.lead = m.as_str().to_string();
        s = s[m.end()..].to_string();
    }
    if let Some(c) = NAME_PREFIX_RE.captures(&s) {
        if JP_RE.is_match(&c[2]) {
            p.name_prefix = Some((c[1].to_string(), c[2].to_string(), c[3].to_string()));
            let end = c.get(0).unwrap().end();
            s = s[end..].to_string();
        }
    }
    if let Some(m) = TRAIL_RE.find(&s) {
        p.trail = m.as_str().to_string();
        s = s[..m.start()].to_string();
    }
    let mut st = s.trim().to_string();
    let chars: Vec<char> = st.chars().collect();
    if chars.len() >= 2 {
        let (first, last) = (chars[0], chars[chars.len() - 1]);
        let close = match first {
            '「' => Some('」'),
            '『' => Some('』'),
            _ => None,
        };
        if close == Some(last) && chars.iter().filter(|&&c| c == first).count() == 1 {
            p.open_quote = first.to_string();
            p.close_quote = last.to_string();
            st = chars[1..chars.len() - 1].iter().collect();
        }
    }
    p.text = st;
    p.codes = codes;
    p
}

fn placeholders_in(s: &str) -> Vec<usize> {
    PH_RE.captures_iter(s).filter_map(|c| c[1].parse().ok()).collect()
}

/// For a tag-like code, (is_closer, name): `[font size=2]` -> (false, "font"),
/// `[resetfont]` / `[endlink]` / `[/b]` / `</color>` -> (true, ...).
fn tag_role(code: &str) -> Option<(bool, String)> {
    let c = code.trim();
    let inner = c.strip_prefix('[').or_else(|| c.strip_prefix('<'))?;
    if inner.ends_with("/>") {
        return None; // self-closing
    }
    let tok: String = inner
        .chars()
        .take_while(|ch| !ch.is_whitespace() && *ch != ']' && *ch != '>' && *ch != '=')
        .collect::<String>()
        .to_lowercase();
    if tok.is_empty() {
        return None;
    }
    if let Some(n) = tok.strip_prefix('/') {
        return Some((true, n.to_string()));
    }
    for pre in ["reset", "end"] {
        if let Some(n) = tok.strip_prefix(pre) {
            if !n.is_empty() {
                return Some((true, n.to_string()));
            }
        }
    }
    Some((false, tok))
}

/// (opener, closer) placeholder numbers for paired codes in the source text,
/// e.g. `[font color=red]` ... `[resetfont]`.
pub fn code_pairs(p: &Protected) -> Vec<(usize, usize)> {
    let mut open: HashMap<String, Vec<usize>> = HashMap::new();
    let mut pairs = Vec::new();
    for i in placeholders_in(&p.text) {
        let Some(code) = p.codes.get(i.wrapping_sub(1)) else { continue };
        match tag_role(code) {
            Some((false, name)) => open.entry(name).or_default().push(i),
            Some((true, name)) => {
                if let Some(o) = open.get_mut(&name).and_then(|v| v.pop()) {
                    pairs.push((o, i));
                }
            }
            None => {}
        }
    }
    pairs
}

/// Problems with a translation of `p` (empty = fine).
pub fn check(p: &Protected, translated: Option<&str>) -> Vec<String> {
    let t = translated.unwrap_or("").trim();
    let mut problems = Vec::new();
    if translated.is_none() {
        return vec!["no answer".into()];
    }
    if t.is_empty() && !p.text.trim().is_empty() {
        problems.push("empty".into());
    }
    let in_body: HashSet<usize> = placeholders_in(&p.text).into_iter().collect();
    let found = placeholders_in(t);
    let mut want: Vec<usize> = in_body.iter().copied().collect();
    want.sort();
    let mut got = found.clone();
    got.sort();
    if got != want {
        let found_set: HashSet<usize> = found.iter().copied().collect();
        let mut missing: Vec<usize> = in_body.difference(&found_set).copied().collect();
        missing.sort();
        if !missing.is_empty() {
            problems.push(format!("missing codes {missing:?}"));
        }
        let mut extra: Vec<usize> = found
            .iter()
            .copied()
            .filter(|n| !in_body.contains(n) || found.iter().filter(|&&x| x == *n).count() > 1)
            .collect();
        extra.sort();
        extra.dedup();
        if !extra.is_empty() {
            problems.push(format!("unexpected codes {extra:?}"));
        }
    }
    // Opening and closing commands (colour on/off, link start/end) must keep
    // their order, or the game renders the wrong span. Other codes may move.
    let pos = |n: usize| t.find(&placeholder(n));
    for (a, b) in code_pairs(p) {
        if let (Some(x), Some(y)) = (pos(a), pos(b)) {
            if x > y {
                problems.push(format!("codes out of order: ⟦{a}⟧ must come before ⟦{b}⟧"));
            }
        }
    }
    if has_japanese(t) {
        problems.push("still has Japanese".into());
    }
    problems
}

/// Rebuild a full message from the translated body.
///
/// `quotes`: (open, close) for dialogue that had 「」, or `None` to keep 「」.
/// `name`: English for a 【name】 prefix (falls back to the Japanese).
pub fn restore(p: &Protected, translated: &str, quotes: Option<(&str, &str)>, name: Option<&str>) -> String {
    let mut body = translated.trim().to_string();
    let found: HashSet<usize> = placeholders_in(&body).into_iter().collect();
    for i in placeholders_in(&p.text) {
        if !found.contains(&i) {
            body.push_str(&placeholder(i)); // re-append codes the model dropped
        }
    }
    if !p.open_quote.is_empty() {
        let (o, c) = quotes.unwrap_or((&p.open_quote, &p.close_quote));
        body = format!("{o}{body}{c}");
    }
    let prefix = match &p.name_prefix {
        Some((o, jp, c)) => format!("{o}{}{c}", name.unwrap_or(jp)),
        None => String::new(),
    };
    let full = format!("{}{}{}{}", p.lead, prefix, body, p.trail);
    let mut used = HashSet::new();
    PH_RE
        .replace_all(&full, |c: &regex::Captures| {
            let i: usize = c[1].parse().unwrap_or(0);
            if i == 0 || i > p.codes.len() || !used.insert(i) {
                String::new() // invented or repeated placeholder: drop it
            } else {
                p.codes[i - 1].clone()
            }
        })
        .into_owned()
}

pub fn strip_codes(s: &str) -> String {
    CODE_RE.replace_all(&PH_RE.replace_all(s, ""), "").into_owned()
}
