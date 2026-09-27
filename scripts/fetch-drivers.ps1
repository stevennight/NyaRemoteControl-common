# Download the redistributable optional-component installers (pinned versions,
# SHA-256 checked) into ..\..\third_party\drivers. The package scripts copy
# them into dist\...\drivers so the one-click install works offline.
# VB-Cable is not redistributable and is always downloaded on demand.
param([string]$Dest = (Join-Path $PSScriptRoot '..\..\third_party\drivers'))
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$pkgs = @(
    @{ File = 'ViGEmBus_1.22.0_x64_x86_arm64.exe'; Url = 'https://github.com/nefarius/ViGEmBus/releases/download/v1.22.0/ViGEmBus_1.22.0_x64_x86_arm64.exe'; Sha = '89220a7865076b342892f98865f3499fb7c4cfd673159e89d352c360fd014c6a' },
    @{ File = 'USBip-0.9.8.1-x64.exe'; Url = 'https://github.com/vadimgrn/usbip-win2/releases/download/v.0.9.8.1/USBip-0.9.8.1-x64.exe'; Sha = '38cad6d4432b52d5bb9409d9ad03b72fdffc4ada4cd3a48fbeca1a2752a8518a' },
    @{ File = 'VirtualDisplayDriver-x86.Driver.Only.zip'; Url = 'https://github.com/VirtualDrivers/Virtual-Display-Driver/releases/download/25.7.23/VirtualDisplayDriver-x86.Driver.Only.zip'; Sha = 'e24210692b442b39af763536330ce78b423f19342b7a7792c26de3944e418b3a' },
    @{ File = 'usbipd-win_5.3.0_x64.msi'; Url = 'https://github.com/dorssel/usbipd-win/releases/download/v5.3.0/usbipd-win_5.3.0_x64.msi'; Sha = '1c984914aec944de19b64eff232421439629699f8138e3ddc29301175bc6d938' }
)
New-Item -ItemType Directory -Force $Dest | Out-Null
foreach ($p in $pkgs) {
    $path = Join-Path $Dest $p.File
    if ((Test-Path $path) -and ((Get-FileHash $path -Algorithm SHA256).Hash.ToLower() -eq $p.Sha)) {
        Write-Host "ok   $($p.File)"
        continue
    }
    Write-Host "get  $($p.File)"
    Invoke-WebRequest -Uri $p.Url -OutFile $path -UseBasicParsing
    $h = (Get-FileHash $path -Algorithm SHA256).Hash.ToLower()
    if ($h -ne $p.Sha) { Remove-Item $path; throw "$($p.File): SHA-256 mismatch ($h)" }
}
Write-Host "drivers in $Dest"
