//! Prompts for each stage. Kept in one place so they're easy to tune.

use crate::project::Glossary;

pub const STYLE_SYSTEM: &str = r#"You are the lead translator preparing an English fan translation of a Japanese visual novel.
You will see a sample of the script and the list of speaker names. Write the reference material
the translators will follow for the whole game.

Return ONLY a JSON object:
{
  "summary": "one or two sentences: genre, setting, tone",
  "style_sheet": "markdown. Cover: narrator voice and tense, tone and register, how each main character talks,
                  honorifics policy (default: drop -san/-kun/-chan unless they carry meaning, keep -senpai/-sensei),
                  how to handle sound effects, ellipses and stammering, and anything unusual in this script.
                  Concrete and short, 150-400 words.",
  "names": [{"jp": "speaker name as written", "en": "English name", "notes": "gender / role if clear"}],
  "terms": [{"jp": "recurring term, place or invented word", "en": "rendering"}]
}
Use the standard romanization (Hepburn, no macrons) for Japanese names unless the game itself
shows a different spelling. Include EVERY speaker name from the list in "names"."#;

const TRANSLATE_SYSTEM: &str = r#"You are translating a Japanese visual novel into English for a game patch.
Write natural, readable English that keeps the meaning, tone and each character's voice.
Avoid stiff literal phrasing, but don't add or remove story content.

{style}

{glossary}

Rules:
- Translate every line completely. Never leave Japanese in the output.
- Placeholders like ⟦1⟧ stand for game codes. Keep every one exactly once, in the
  matching position in the English sentence. Never translate or alter them.
- Don't wrap lines in quotation marks; the program adds dialogue quotes itself.
- Don't insert line breaks; the program wraps text to fit the text box.
- Use the glossary spellings for names and terms every time.
- Lines that are only noises, ellipses or stammers still get a natural English rendering
  ("......" -> "...", "えっ" -> "Huh?").

Return ONLY a JSON array, one object per line to translate, same order:
[{"i": 1, "en": "..."}, ...]"#;

const REVIEW_SYSTEM: &str = r#"You are the editor of an English visual novel translation. For each line you get the
Japanese original ("jp") and the current English ("en"). Fix real problems only:
mistranslations, missing meaning, wrong speaker gender or pronouns, tense or point-of-view slips,
glossary inconsistencies, awkward or unnatural English, and broken placeholders.
Don't rewrite lines that are already good, and don't add quotation marks or line breaks.

{style}

{glossary}

Placeholders like ⟦1⟧ are game codes: every one must appear exactly once.

Return ONLY a JSON array with the lines you changed:
[{"i": 3, "en": "improved line"}]
Return [] if nothing needs changing."#;

pub const SHORTEN_SYSTEM: &str = r#"You fit English visual novel lines into a fixed-size text box.
Each line comes with "max", the maximum number of characters it may use (spaces included).
Rewrite each line to fit within its limit while keeping the meaning, tone and character voice.
Prefer tighter wording over dropping content. Keep every placeholder like ⟦1⟧ exactly once.

Return ONLY a JSON array: [{"i": 1, "en": "shorter line"}]"#;

pub const NAMES_SYSTEM: &str = r#"You give English names for the speakers in a Japanese visual novel.
Use standard Hepburn romanization without macrons for personal names (e.g. 大雅 -> Taiga),
and translate descriptive labels (e.g. 女の子 -> Girl, 店員 -> Clerk, ？？？ -> ???).

Return ONLY a JSON array: [{"jp": "...", "en": "..."}]"#;

pub fn glossary_block(g: &Glossary) -> String {
    let mut out: Vec<String> = Vec::new();
    if !g.names.is_empty() {
        out.push("Character names (Japanese -> English):".into());
        out.extend(g.names.iter().filter(|(_, en)| !en.is_empty()).map(|(jp, en)| format!("- {jp} -> {en}")));
    }
    if !g.terms.is_empty() {
        out.push("Terms:".into());
        out.extend(
            g.terms.iter().filter(|t| !t.jp.is_empty() && !t.en.is_empty()).map(|t| format!("- {} -> {}", t.jp, t.en)),
        );
    }
    out.join("\n")
}

pub fn style_block(style: &str) -> String {
    let s = style.trim();
    if s.is_empty() {
        String::new()
    } else {
        format!("Style sheet:\n{s}")
    }
}

fn fill(template: &str, style: &str, g: &Glossary) -> String {
    template.replace("{style}", &style_block(style)).replace("{glossary}", &glossary_block(g))
}

pub fn translate_system(style: &str, g: &Glossary) -> String {
    fill(TRANSLATE_SYSTEM, style, g)
}

pub fn review_system(style: &str, g: &Glossary) -> String {
    fill(REVIEW_SYSTEM, style, g)
}
