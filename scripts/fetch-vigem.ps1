# Download ViGEmClient (MIT, https://github.com/nefarius/ViGEmClient) into
# ..\..\third_party\ViGEmClient. nya-server compiles it in for gamepad support;
# without it the server builds without gamepads.
param([string]$Dest = (Join-Path $PSScriptRoot '..\..\third_party'))
$ErrorActionPreference = 'Stop'
$zip = Join-Path $Dest 'vigemclient.zip'
Invoke-WebRequest -Uri 'https://github.com/nefarius/ViGEmClient/archive/refs/heads/master.zip' -OutFile $zip -UseBasicParsing
$tmp = Join-Path $Dest 'vigem-tmp'
Expand-Archive $zip -DestinationPath $tmp -Force
Remove-Item $zip
$target = Join-Path $Dest 'ViGEmClient'
if (Test-Path $target) { Remove-Item $target -Recurse -Force }
Move-Item (Get-ChildItem $tmp -Directory | Select-Object -First 1).FullName $target
Remove-Item $tmp -Recurse -Force
Write-Host "ViGEmClient installed to $target"
