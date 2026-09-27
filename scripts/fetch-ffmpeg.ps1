# Download the pinned FFmpeg LGPL shared build (headers, import libs, DLLs)
# into ..\..\third_party\ffmpeg. The bindings in nya-ffmpeg-sys match this version.
param(
    [string]$Dest = (Join-Path $PSScriptRoot '..\..\third_party'),
    [string]$Url = 'https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-n8.1-latest-win64-lgpl-shared-8.1.zip'
)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $Dest | Out-Null
$zip = Join-Path $Dest 'ffmpeg.zip'
Invoke-WebRequest -Uri $Url -OutFile $zip -UseBasicParsing
$tmp = Join-Path $Dest 'ffmpeg-extract'
Expand-Archive $zip -DestinationPath $tmp -Force
Remove-Item $zip
$target = Join-Path $Dest 'ffmpeg'
if (Test-Path $target) { Remove-Item $target -Recurse -Force }
Move-Item (Get-ChildItem $tmp -Directory | Select-Object -First 1).FullName $target
Remove-Item $tmp -Recurse -Force
Write-Host "FFmpeg installed to $target"
