# Yakusu 訳す

**Translate a Japanese visual novel into an English patch, from one window.**

Pick the game folder, paste an API key, press Translate. Yakusu pulls the script out of the game, translates it with an AI model using a style sheet and name list it drafts for you, makes the English fit the game's text box, and installs a patch. Your original files are backed up, and one click puts them back.

![Yakusu test patch view](docs/screenshot.png)

It grew out of a full English retranslation of *Sakura, Moyu. -as the Night's, Reincarnation-*: 53,699 lines, Haiku translating and Sonnet reviewing every line, about **$31** in API costs. Yakusu packages that pipeline so it works on other games.

## How it works

1. **Game.** Yakusu detects the engine and reads the scripts, decrypting archives where needed. It measures the text box from the original Japanese: how many characters fit on a line and how many lines fit in a box.
2. **Translator.** Claude (Haiku, Sonnet, Opus), any OpenAI-compatible API, or a free local model in LM Studio or Ollama. Yakusu estimates the cost for your game before spending anything, and you can set a spending limit.
3. **Style & names.** The model reads a sample of the script and drafts a style sheet (tone, narrator voice, honorifics) plus a table of character names and terms. Every batch follows it, which keeps names and voices consistent across tens of thousands of lines. You can edit both.
4. **Test patch.** Yakusu translates 150 lines, installs them, and shows them in a text box preview wrapped exactly as the game will draw them. If anything runs out of the box, move the slider and apply again. Re-applying doesn't re-translate, so it costs nothing.
5. **Translate & install.** Yakusu translates the rest in parallel batches, saving as it goes so you can stop and resume. An optional review pass with a stronger model edits weak lines. Lines that still overflow the box are rewritten shorter. Then it builds and installs the patch.

A **Lines** view lists every line with its translation. Edit anything, retranslate a line or a page of lines, see what the review changed, mark a line to skip, and rebuild. **Export for proofreading** writes a CSV (opens in Excel or Google Sheets) that a friend can edit; **Import** brings their edits back, with every line checked before it's used.

### Built to survive long runs

- **Stop and resume any time.** Every batch is saved the moment it comes back; a crash, a closed window or a rate limit loses at most the batches in flight.
- **Bad replies don't cost a re-run.** A reply that's cut off or unreadable splits its batch in half and tries again. Lines that come back with broken codes are retried together, then one by one. Lines that still fail are flagged in **Lines → Problems** and retried on the next run.
- **Rate limits and outages are waited out**, honoring the provider's `retry-after`. A bad key or an empty account stops the run with a clear message instead of retrying.
- **Repeated lines are translated once.** VNs repeat short lines constantly ("……", "Yeah.", menu choices); identical lines from the same speaker reuse one translation, which saves money and keeps them consistent.
- **Names stay consistent.** Speakers without an English name get one before translation starts, and a check flags lines that don't use the glossary's spelling, with a one-click fix.
- **A spending limit you set** stops the run before it goes over.

## Getting text back into the game

This is the hard part of any VN translation, and it's different for every engine. Yakusu handles it in layers:

- **Engine codes are protected.** Codes for click-waits, page breaks, choices, colours and embedded variables (`[l]`, `[p]`, `<link …>`, `\c[2]`, `%name;`) are swapped for numbered markers (`⟦1⟧`) before the model sees a line. Afterwards every marker is checked: a missing, duplicated or invented code sends the line back to be translated again on its own. Furigana markup is dropped, since it makes no sense over English.
- **Commands never move.** For Kirikiri `.ks` scripts, Yakusu rewrites only the text lines in place. Every `@bg`, `@chara` and voice command stays on its original line, so backgrounds, sprites and voices stay in sync.
- **Text fits the box.** Most engines don't word-wrap English: they either break exactly where the script says, or cut words in half. Yakusu wraps English to the box it measured, keeps line breaks that belong to codes, and sends lines that are still too long back to the model with a character limit.
- **Encoding is handled.** Most engines store text in Shift-JIS, which can't hold "é" or an em dash. Yakusu swaps in characters the game can display, so nothing turns into "?" in the text box. (For games that need a different font or proportional spacing, VNTextProxy from VNTranslationTools can be added by hand.)
- **Dialogue looks English.** 「」 become quotes, and 【名前】 name tags and name boxes use the English names from the glossary.
- **Installs are reversible.** Kirikiri games get a new `patchN.xp3` and the original archives are never touched. Encrypted Kirikiri games also get KirikiriTools' `version.dll`, so they accept the unencrypted patch. For other engines, every file Yakusu replaces is backed up first, and **Remove the patch** restores everything.

## Supported engines

| Engine | Level | How |
|---|---|---|
| Kirikiri / KiriKiri Z (`.xp3`, `.ks`, `.scn`) | **Automatic** | Reads `.xp3` directly (msg-tool decrypts ~500 known encrypted games), rewrites `.ks` in place, packs `patchN.xp3` |
| FAVORITE / FVP (`.hcb`) | Experimental | msg-tool reads and writes the script, which Yakusu replaces with a backup |
| Artemis, BGI/Ethornell, YU-RIS, QLiE, Circus, Escu:de | Experimental | msg-tool unpacks, patches and repacks the archive |
| CatSystem2, Softpal | Manual step | Yakusu reads the archive and writes patched scripts; you pack them back |
| AdvHD, Majiro, RealLive, Ren'Py, ShSystem, SystemNNN, Musica, … | Manual step | Extract the scripts (GARbro, unrpa), then use **Translate script files**: msg-tool or VNTextPatch reads and writes them |
| SiglusEngine | Not yet | No open tool rebuilds `Scene.pck` reliably |

"Experimental" means the code path exists and is built on msg-tool's tested format support, but hasn't yet been tried against a retail game for that engine. Please report what works.

## Running it

**Windows app:** download `Yakusu-windows.zip` from Releases, unzip it, and run `Yakusu.exe`. All the tools it needs are inside.

**From source** (any OS, Python 3.10+):

```bash
python -m yakusu            # app window (needs: pip install pywebview)
python -m yakusu --browser  # or in your browser
```

From source you also need msg-tool on your PATH or in `tools/` (`cargo install --git https://github.com/lifegpc/msg-tool`). Kirikiri `.ks` games work without it.

## Development

```bash
pip install pytest
python -m pytest -q tests
```

- `tests/test_llm.py` runs the API client against a mock server: request shape, cost math, rate limits with `retry-after`, bad keys, out-of-credit errors, truncated replies, and OpenAI parameter fallbacks.
- `tests/test_engines.py` drives the archive engines and script-folder mode through a stand-in msg-tool, and checks batch splitting on truncated replies and that Stop keeps finished work.
- `tests/test_server.py` checks the per-launch token, the whole flow over HTTP, refusing a second job, and that Stop reaches a running job.
- `tests/test_e2e.py` builds a small Kirikiri game (an `.xp3` with UTF-16 and Shift-JIS scripts, name lines, choices, ruby and TJS blocks). It then runs the whole pipeline with a fake model (extract, style, translate, review, shorten, build, install, re-install, restore) and checks that every command line is byte-identical and the original files are untouched.
- `tests/test_units.py` covers code protection, wrapping, box measurement, encoding and the `.ks` and `.xp3` formats.

To build the Windows app, run `packaging/fetch_tools.ps1` (builds msg-tool, downloads VNTextPatch and KirikiriTools), then `pyinstaller packaging/yakusu.spec`. `.github/workflows/build.yml` does both on every version tag.

```
yakusu/
  core/      pipeline, prompts, LLM clients, code protection, text fitting, project storage
  engines/   kirikiri (+ ks, xp3), generic msg-tool engines, script-folder mode
  web/       the interface
  server.py  local server (127.0.0.1 only, per-launch token)
```

## Notes

- Use it on games you own. A patch built by Yakusu contains the game's script, so don't redistribute the generated files. Share the tool, not the patch.
- Yakusu sends script text to the model provider you choose, and to nothing else. Keys and projects stay in `%LOCALAPPDATA%\Yakusu`.
- Third-party tools and their licenses are listed in [THIRD_PARTY.md](THIRD_PARTY.md). Yakusu is GPL-3.0 because it includes msg-tool's game database.

Made by [Reckedge](https://github.com/Reckedge).
