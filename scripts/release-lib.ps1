# Helpers shared by the server and client release scripts (dot-source it):
#   . (Join-Path $PSScriptRoot '..\..\common\scripts\release-lib.ps1')
#
# Versions: each released repository (server, client) has its own VERSION file
# (MAJOR.MINOR.PATCH or MAJOR.MINOR.PATCH-PRERELEASE) and is tagged v<VERSION>.
# The Cargo.toml package versions must match it (Test-NyaVersion checks,
# Set-NyaVersion writes). Server and client are versioned independently.
# Runs on Windows PowerShell 5.1 and PowerShell 7.

$ErrorActionPreference = 'Stop'

$script:SemVer = '^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(-[0-9A-Za-z.-]+)?$'

# The repository's VERSION.
function Get-NyaVersion([string]$Repo) {
    $v = (Get-Content -LiteralPath (Join-Path $Repo 'VERSION') -Raw).Trim()
    if ($v -notmatch $script:SemVer) { throw "VERSION '$v' is not MAJOR.MINOR.PATCH[-PRERELEASE]" }
    return $v
}

# The four-part numeric version Windows resources and installers need (prerelease dropped).
function Get-NyaNumericVersion([string]$Version) {
    return ($Version -replace '-.*$', '') + '.0'
}

# The `version = "..."` line of a Cargo.toml's [package] section.
function Get-CargoPackageVersion([string]$Toml) {
    $inPackage = $false
    foreach ($line in Get-Content -LiteralPath $Toml) {
        if ($line -match '^\s*\[(.+)\]\s*$') { $inPackage = ($Matches[1] -eq 'package') ; continue }
        if ($inPackage -and $line -match '^\s*version\s*=\s*"([^"]+)"') { return $Matches[1] }
    }
    throw "$Toml has no [package] version"
}

function Set-CargoPackageVersion([string]$Toml, [string]$Version) {
    $inPackage = $false
    $done = $false
    $out = foreach ($line in Get-Content -LiteralPath $Toml) {
        if ($line -match '^\s*\[(.+)\]\s*$') { $inPackage = ($Matches[1] -eq 'package') }
        elseif ($inPackage -and -not $done -and $line -match '^(\s*version\s*=\s*)"[^"]+"(.*)$') {
            $line = "$($Matches[1])`"$Version`"$($Matches[2])"
            $done = $true
        }
        $line
    }
    if (-not $done) { throw "$Toml has no [package] version" }
    [IO.File]::WriteAllText($Toml, (($out -join "`n") + "`n"))
}

# VERSION, every listed Cargo.toml and (if given) the tag agree.
function Test-NyaVersion([string]$Repo, [string[]]$Tomls, [string]$Tag = '') {
    $v = Get-NyaVersion $Repo
    foreach ($t in $Tomls) {
        $c = Get-CargoPackageVersion (Join-Path $Repo $t)
        if ($c -ne $v) { throw "$t has version $c but VERSION says $v (run scripts\release.ps1 to bump both)" }
    }
    if ($Tag -and $Tag -ne "v$v") { throw "tag $Tag does not match VERSION $v (expected v$v)" }
    return $v
}

function Invoke-Checked([string]$FilePath, [string[]]$Arguments = @()) {
    # cargo / npm write progress to stderr; Windows PowerShell 5.1 turns that
    # into errors under 'Stop', so success is decided by the exit code alone.
    $ErrorActionPreference = 'Continue'
    & $FilePath @Arguments
    $code = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    if ($code -ne 0) { throw "$FilePath $($Arguments -join ' ') failed with exit code $code" }
}

function Write-Sha256([string]$Path) {
    $hash = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
    Set-Content -LiteralPath "$Path.sha256" -Value "$hash  $(Split-Path -Leaf $Path)" -Encoding ascii
}

# Third-party pieces the build and the packages need, fetched when missing
# (into <workspace>\third_party, next to common / server / client).
function Initialize-NyaThirdParty([switch]$Vigem) {
    $common = Resolve-Path (Join-Path $PSScriptRoot '..')
    $tp = Join-Path $common '..\third_party'
    if (-not (Test-Path (Join-Path $tp 'ffmpeg\bin'))) { & (Join-Path $common 'scripts\fetch-ffmpeg.ps1') }
    & (Join-Path $common 'scripts\fetch-drivers.ps1')
    if ($Vigem -and -not (Test-Path (Join-Path $tp 'ViGEmClient\src\ViGEmClient.cpp'))) { & (Join-Path $common 'scripts\fetch-vigem.ps1') }
}

function Find-Makensis {
    $m = Get-Command makensis -ErrorAction SilentlyContinue
    if ($m) { return $m.Source }
    foreach ($pf in @(${env:ProgramFiles(x86)}, $env:ProgramFiles)) {
        if (-not $pf) { continue }
        foreach ($rel in @('NSIS\makensis.exe', 'NSIS\Bin\makensis.exe')) {
            $p = Join-Path $pf $rel
            if (Test-Path -LiteralPath $p) { return $p }
        }
    }
    throw 'makensis was not found. Install NSIS (winget install NSIS.NSIS, or choco install nsis) or pass -SkipInstaller.'
}

# Zip, NSIS installer and their .sha256 files for a packaged dist folder.
function New-NyaReleaseFiles {
    param(
        [string]$Repo, [string]$Name, [string]$Version, [string]$Stage,
        [string]$Nsi, [string]$Icon, [switch]$SkipInstaller
    )
    $out = Join-Path $Repo 'release'
    if (Test-Path $out) { Remove-Item $out -Recurse -Force }
    New-Item -ItemType Directory -Force $out | Out-Null

    $zip = Join-Path $out "${Name}_${Version}_windows_x64.zip"
    Compress-Archive -Path (Join-Path $Stage '*') -DestinationPath $zip -Force
    Write-Sha256 $zip

    if (-not $SkipInstaller) {
        $setup = Join-Path $out "${Name}_${Version}_x64-setup.exe"
        Invoke-Checked (Find-Makensis) @(
            '/V2', '/INPUTCHARSET', 'UTF8', "/DVERSION=$Version", "/DVI_VERSION=$(Get-NyaNumericVersion $Version)",
            "/DSOURCE_DIR=$Stage", "/DOUTFILE=$setup", "/DICON=$Icon", $Nsi
        )
        Write-Sha256 $setup
    }
    Write-Host "release files in ${out}:" -ForegroundColor Green
    Get-ChildItem -LiteralPath $out | ForEach-Object { "  $($_.Name)  ($([math]::Round($_.Length / 1MB, 1)) MB)" }
}

# Bump VERSION and the Cargo.toml versions, pin the common commit
# (COMMON_REF, used by the release build), commit and tag v<Version>.
function Publish-NyaVersion {
    param([string]$Repo, [string]$Product, [string[]]$Tomls, [string]$Version, [switch]$Push)
    if ($Version -notmatch $script:SemVer) { throw "'$Version' is not MAJOR.MINOR.PATCH[-PRERELEASE]" }
    $common = Resolve-Path (Join-Path $Repo '..\common')
    Push-Location $Repo
    try {
        if (git status --porcelain --untracked-files=no) { throw "$Product has uncommitted changes; commit them first" }
        if (git tag --list "v$Version") { throw "tag v$Version already exists" }
        if (git -C $common status --porcelain --untracked-files=no) { throw 'common has uncommitted changes; commit and push them first' }
        $commonSha = (git -C $common rev-parse HEAD).Trim()
        Invoke-Checked git @('-C', $common, 'fetch', '--quiet', 'origin')
        if (-not (git -C $common branch -r --contains $commonSha)) { throw "common $commonSha is not pushed; push common first (the release build checks it out)" }

        Set-Content -LiteralPath 'VERSION' -Value $Version -Encoding ascii
        foreach ($t in $Tomls) { Set-CargoPackageVersion (Join-Path $Repo $t) $Version }
        Set-Content -LiteralPath 'COMMON_REF' -Value $commonSha -Encoding ascii
        Invoke-Checked cargo @('update', '--workspace', '--quiet')
        Invoke-Checked git (@('add', 'VERSION', 'COMMON_REF', 'Cargo.lock') + $Tomls)
        Invoke-Checked git @('commit', '--quiet', '-m', "Release $Product $Version")
        Invoke-Checked git @('tag', '-a', "v$Version", '-m', "$Product $Version")
        Write-Host "tagged v$Version (common $($commonSha.Substring(0, 8)))" -ForegroundColor Green
        if ($Push) {
            Invoke-Checked git @('push', 'origin', 'HEAD', "v$Version")
            Write-Host 'pushed; GitHub Actions builds the installer and publishes the release' -ForegroundColor Green
        } else {
            Write-Host "push with: git push origin HEAD v$Version" -ForegroundColor Yellow
        }
    } finally { Pop-Location }
}
