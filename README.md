<p align="center"><img src="docs/icon.png" width="96" alt=""></p>

# Yakusoku 訳束

Translate Japanese visual novels into English patches. Pick the game folder, paste an API key, get an installable patch. Every change can be undone.

The name is a pun: 約束 (*yakusoku*) means "promise"; 訳束 swaps in 訳, "translation". A bundle of translations, and a promise to play the game you've been waiting for.

![The text box preview: translated lines wrapped exactly as the game will show them](docs/screenshot.png)

## What it does

1. **Game.** Point it at the game folder. It recognises the engine (Kirikiri, FVP, Artemis, BGI, YU-RIS, QLiE, Circus, Escu:de, CatSystem2, Softpal) and reads the scripts, decrypting about 500 known encrypted Kirikiri games.
2. **Translator.** Claude (Haiku, Sonnet, Opus), OpenAI, OpenRouter, or a local model in LM Studio / Ollama. You see a cost estimate for each quality level before anything is spent, and you can set a spending limit.
3. **Style and names.** The model reads a sample and writes a style sheet and a glossary of character names and terms. Every batch follows them. Edit anything.
4. **Test patch.** Translate 150 lines, install, check them in the game. The preview wraps lines exactly as the game will, using the text box measured from the original script; adjust it and apply again for free.
5. **Translate and install.** Batches run in parallel and save as they go: stop any time, pick up later. Then a review pass by a stronger model, a glossary consistency check, a pass that rewrites lines too long for the text box, and the patch is built and installed.

Originals are never edited in place. Kirikiri games get a new `patch.xp3`; other engines have their archive rebuilt with the original backed up. Restore puts every byte back.

## Why it's reliable

- **Game codes survive.** Inline codes (`[l]`, `<color>`, `%name;`, ...) become numbered placeholders. Every reply is checked: missing, invented or duplicated codes, leftover Japanese, and opening/closing pairs in the wrong order send the line back for another try.
- **Nothing moves.** Kirikiri `.ks` scripts are parsed by Yakusoku itself; command lines (backgrounds, sprites, voices) stay on their exact lines.
- **Crash-safe.** Results are an append-only log. A half-written line from a crash or power cut is trimmed on the next save, so nothing after it is lost.
- **Recoverable installs.** Before a game file is replaced, its backup and the install record are written. If anything fails mid-install, Restore still knows what to put back.
- **Speaker names stay consistent.** Where a name and a line share one string in the script (FVP), the line gets the glossary name, and the rebuilt script is re-read to check every record came out as written.
- **Never reads its own English.** Yakusoku marks the patches it builds, so reading a game again (even after a reinstall) skips them.
- **Repeated lines are translated once.** VNs repeat a lot ("……", "Yeah.", choices); identical lines from the same speaker reuse one translation.

## Install

Download the Windows installer from [Releases](../../releases). It bundles everything; there is nothing else to install.

## Build from source

Yakusoku is a Rust workspace: `crates/core` holds the whole engine (and its tests), `src-tauri` is a thin desktop window, and `ui/` is the interface.

```sh
cargo test -p yakusoku-core                     # unit, end-to-end and recovery tests, no network
cargo run -p yakusoku                           # the desktop app (needs Tauri's system deps on Linux)

# The interface in a browser, with a stand-in model (no key, no cost):
YAKUSOKU_FAKE_LLM=1 cargo run -p yakusoku-core --features dev-server --bin yakusoku-dev
```

Windows release build: `packaging\fetch_tools.ps1` builds and fetches the bundled tools into `tools/`, then `cargo tauri build` in `src-tauri` makes the NSIS installer. CI does both on every `v*` tag.

Archive tests against the real msg-tool run when `YAKUSOKU_REAL_MSG_TOOL` points at a build. The large test (`cargo test --release -- --ignored large`) pushes 100,000 lines through extract, translate, build, install and restore in about four seconds with the stand-in model.

## Data

Projects live in `%LOCALAPPDATA%\Yakusoku` (`~/.yakusoku` elsewhere), one folder per game: the extracted lines, translations, costs, style sheet and glossary. API keys are stored only on this computer. The **Lines** view exports a CSV for proofreading and imports it back.

## License

GPL-3.0-or-later. See [THIRD_PARTY.md](THIRD_PARTY.md) for the bundled tools and the fonts.

This tool is for translating games you own. Don't distribute game scripts.
