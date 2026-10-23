# Third-party tools, code and fonts

Yakusoku runs these programs as separate tools. They ship inside the Windows installer, in the `tools` folder.

| Tool | What Yakusoku uses it for | License | Source |
|---|---|---|---|
| msg-tool by lifegpc | Reading and writing scripts and archives for ~20 engines; decrypting ~500 encrypted Kirikiri games. Pinned to commit `64856108` (0.4.0-alpha.3). | GPL-3.0 | https://github.com/lifegpc/msg-tool |
| VNTextPatch by arcusmaximus | Script files for engines msg-tool doesn't cover | MIT | https://github.com/arcusmaximus/VNTranslationTools |
| KirikiriUnencryptedArchive (version.dll) by arcusmaximus | Lets encrypted Kirikiri games load Yakusoku's unencrypted patch.xp3 | MIT | https://github.com/arcusmaximus/KirikiriTools |

Code ported into Yakusoku:

- `crates/core/src/engines/xp3.rs`: the xp3 writer is a port of Xp3Pack and the descrambler a port of KirikiriDescrambler, both from KirikiriTools (MIT, © arcusmaximus).
- `crates/core/data/xp3_games.json`: game names, titles and plugin file names from msg-tool's `msg_tool_xp3data/crypt.json` (GPL-3.0), which comes from GARbro's database. This is one reason Yakusoku is licensed under GPL-3.0.

Fonts in `ui/fonts` (SIL Open Font License 1.1, license texts alongside):

- Shippori Mincho, © The Shippori Mincho Project Authors. Latin subset, plus a subset of the few kanji the interface itself uses.
- Zen Kaku Gothic New, © The Zen Project Authors. Latin subset.

The desktop window is built with Tauri (MIT / Apache-2.0).
