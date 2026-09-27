# Regenerate crates/nya-ffmpeg-sys/src/bindings.rs.
# Needs: FFmpeg dev package in ..\third_party\ffmpeg (scripts\fetch-ffmpeg.ps1),
#        MSVC build tools + Windows SDK, and LIBCLANG_PATH pointing at a directory
#        that contains libclang.dll (e.g. `pip install libclang` -> site-packages\clang\native).
param(
    [string]$FFmpegDir = (Join-Path $PSScriptRoot '..\..\third_party\ffmpeg')
)
$ErrorActionPreference = 'Stop'
if (-not $env:LIBCLANG_PATH) { throw 'Set LIBCLANG_PATH to the directory containing libclang.dll' }

$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
$msvc = Get-ChildItem "$vs\VC\Tools\MSVC" | Sort-Object Name -Descending | Select-Object -First 1
$kits = (Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows Kits\Installed Roots').KitsRoot10
$sdk = Get-ChildItem "$kits\Include" | Sort-Object Name -Descending | Select-Object -First 1

$includes = @(
    "-I$($msvc.FullName)\include",
    "-I$($sdk.FullName)\ucrt",
    "-I$($sdk.FullName)\shared",
    "-I$($sdk.FullName)\um"
)
$out = Join-Path $PSScriptRoot '..\crates\nya-ffmpeg-sys\src\bindings.rs'
Push-Location (Join-Path $PSScriptRoot '..')
try {
    $ErrorActionPreference = 'Continue'   # cargo progress goes to stderr
    cargo run -p gen-ffmpeg-bindings --release -- "$FFmpegDir\include" $out @includes
} finally {
    Pop-Location
}
