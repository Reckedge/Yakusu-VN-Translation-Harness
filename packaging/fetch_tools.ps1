# Fills tools/ with the third-party programs Yakusoku drives. build.rs packs
# them into Yakusoku.exe. Run on Windows:
#   powershell -ExecutionPolicy Bypass -File packaging\fetch_tools.ps1
# Needs Rust (cargo) and git. Every version is pinned.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$root = Split-Path -Parent $PSScriptRoot
$tools = Join-Path $root 'tools'
New-Item -ItemType Directory -Force -Path $tools, "$tools\kirikiri", "$tools\VNTextPatch" | Out-Null
$tmp = Join-Path $env:TEMP 'yakusoku-tools'
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
function Check($what) { if ($LASTEXITCODE -ne 0) { throw "$what failed (exit $LASTEXITCODE)" } }

# 1. msg-tool (lifegpc/msg-tool, GPL-3.0): scripts + archives for ~20 engines.
$MSGTOOL_REF = '64856108ef5aa18d15f5debc67ff2ade2fda2c45'
if (-not (Test-Path "$tmp\msg-tool\.git")) { git clone https://github.com/lifegpc/msg-tool "$tmp\msg-tool"; Check 'git clone msg-tool' }
Push-Location "$tmp\msg-tool"
git fetch origin; Check 'git fetch'
git checkout --force $MSGTOOL_REF; Check 'git checkout'
cargo build --release --no-default-features --features "all-script,all-arc"; Check 'msg-tool build'
Copy-Item target\release\msg_tool.exe "$tools\msg-tool.exe" -Force
Pop-Location

# 2. VNTextPatch (arcusmaximus/VNTranslationTools v0.0.41, MIT).
$zip = "$tmp\VNTranslationTools.zip"
Invoke-WebRequest 'https://github.com/arcusmaximus/VNTranslationTools/releases/download/v0.0.41/VNTranslationTools.zip' -OutFile $zip
Expand-Archive $zip -DestinationPath "$tools\VNTextPatch" -Force
if (-not (Get-ChildItem "$tools\VNTextPatch" -Recurse -Filter 'VNTextPatch.exe')) { throw 'VNTextPatch.exe not found in VNTranslationTools.zip' }
# Yakusoku looks for tools\VNTextPatch\VNTextPatch.exe: flatten one level if the zip has a folder.
$exe = Get-ChildItem "$tools\VNTextPatch" -Recurse -Filter 'VNTextPatch.exe' | Select-Object -First 1
if ($exe.DirectoryName -ne "$tools\VNTextPatch") { Get-ChildItem $exe.DirectoryName | Move-Item -Destination "$tools\VNTextPatch" -Force }

# 3. KirikiriUnencryptedArchive version.dll (arcusmaximus/KirikiriTools 1.7, MIT).
Invoke-WebRequest 'https://github.com/arcusmaximus/KirikiriTools/releases/download/1.7/version.dll' -OutFile "$tools\kirikiri\version.dll"

Write-Host "Tools ready in $tools"
Get-ChildItem $tools -Recurse -File | Select-Object FullName, Length
