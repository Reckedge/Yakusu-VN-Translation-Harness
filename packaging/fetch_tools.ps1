# Fills tools/ with the third-party programs Yakusoku drives. Run on Windows:
#   powershell -ExecutionPolicy Bypass -File packaging\fetch_tools.ps1
# Needs: Rust (cargo) for msg-tool, and the GitHub CLI (gh) for release downloads.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$tools = Join-Path $root 'tools'
New-Item -ItemType Directory -Force -Path $tools, "$tools\kirikiri", "$tools\VNTextPatch" | Out-Null
$tmp = Join-Path $env:TEMP 'yakusoku-tools'
New-Item -ItemType Directory -Force -Path $tmp | Out-Null

# 1. msg-tool (lifegpc/msg-tool, GPL-3.0): scripts + archives for ~20 engines.
#    Pinned to the commit Yakusoku's archive tests ran against (0.4.0-alpha.3).
$MSGTOOL_REF = '64856108ef5aa18d15f5debc67ff2ade2fda2c45'
if (-not (Test-Path "$tmp\msg-tool")) { git clone https://github.com/lifegpc/msg-tool "$tmp\msg-tool" }
Push-Location "$tmp\msg-tool"
git fetch origin; git checkout $MSGTOOL_REF
cargo build --release --no-default-features --features "all-script,all-arc"
Copy-Item target\release\msg_tool.exe "$tools\msg-tool.exe" -Force
Pop-Location

# 2. VNTextPatch (arcusmaximus/VNTranslationTools, MIT).
gh release download --repo arcusmaximus/VNTranslationTools --pattern '*.zip' --dir $tmp --clobber
Get-ChildItem $tmp -Filter '*.zip' | Where-Object { $_.Name -match 'VNTextPatch|VNTranslationTools' } | ForEach-Object {
  Expand-Archive $_.FullName -DestinationPath "$tools\VNTextPatch" -Force }

# 3. KirikiriUnencryptedArchive version.dll (arcusmaximus/KirikiriTools, MIT).
gh release download --repo arcusmaximus/KirikiriTools --pattern '*.zip' --dir "$tmp\krkr" --clobber
Get-ChildItem "$tmp\krkr" -Filter '*.zip' | ForEach-Object { Expand-Archive $_.FullName -DestinationPath "$tmp\krkr\x" -Force }
$dll = Get-ChildItem "$tmp\krkr\x" -Recurse -Filter 'version.dll' | Select-Object -First 1
if (-not $dll) { throw 'version.dll not found in the KirikiriTools release' }
Copy-Item $dll.FullName "$tools\kirikiri\version.dll" -Force

Write-Host "Tools ready in $tools"
Get-ChildItem $tools -Recurse | Select-Object FullName
