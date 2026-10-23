# Third-party tools and data

Yakusu runs these programs as separate tools. They ship inside the Windows build, in the `tools` folder.

| Tool | What Yakusu uses it for | License | Source |
|---|---|---|---|
| msg-tool by lifegpc | Reading and writing scripts and archives for ~20 engines; decrypting ~500 encrypted Kirikiri games | GPL-3.0 | https://github.com/lifegpc/msg-tool |
| VNTextPatch / VNTextProxy by arcusmaximus | Script files for engines msg-tool doesn't cover; Shift-JIS tunneling and font fixes | MIT | https://github.com/arcusmaximus/VNTranslationTools |
| KirikiriUnencryptedArchive (version.dll) by arcusmaximus | Lets encrypted Kirikiri games load Yakusu's unencrypted patch.xp3 | MIT | https://github.com/arcusmaximus/KirikiriTools |

Code ported into Yakusu:

- `yakusu/engines/xp3.py`: the xp3 writer is a port of Xp3Pack and the descrambler a port of KirikiriDescrambler, both from KirikiriTools (MIT, © arcusmaximus).
- `yakusu/data/xp3_games.json`: game names, titles and plugin file names taken from msg-tool's `msg_tool_xp3data/crypt.json` (GPL-3.0), which in turn comes from GARbro's database. This is why Yakusu itself is licensed under GPL-3.0.
