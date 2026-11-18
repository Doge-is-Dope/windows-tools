# Run on Windows; the real installer only writes inside the temporary profiles created by this test, and starts no host.
param([Parameter(Mandatory=$true)][string]$Installer, [string]$OfflineExe)

$ErrorActionPreference = 'Stop'
$base = Join-Path ([IO.Path]::GetTempPath()) ('windows-tools-test-' + [guid]::NewGuid())
$originalLocal = $env:LOCALAPPDATA
$originalProxy = $env:HTTPS_PROXY
$expected = [regex]::Match((Get-Content -Raw -LiteralPath $Installer), '\$expected = ''([0-9a-f]{64})''').Groups[1].Value
if (-not $expected) { throw 'No SHA-256 found in the installer.' }
$env:WT_NO_START = '1'
$name = 'windows-tools-host.exe'
$failures = 0

function Run-Installer([string]$case, [int]$exit, [string]$message) {
    $output = & $env:ComSpec /d /c 'install.cmd <nul 2>&1'
    if ($LASTEXITCODE -ne $exit -or ($output -join ' ') -notmatch [regex]::Escape($message)) {
        throw "$case failed (exit $LASTEXITCODE): $($output -join ' ')"
    }
    Write-Output "PASS $case"
}

New-Item -ItemType Directory -Path $base | Out-Null
Copy-Item -LiteralPath $Installer -Destination (Join-Path $base 'install.cmd')
Push-Location $base
try {
    $env:LOCALAPPDATA = Join-Path $base 'online profile'
    $dest = Join-Path $env:LOCALAPPDATA "Programs\WindowsTools\$name"
    try {
        Run-Installer 'fresh online install' 0 'installed.'
        if ((Get-FileHash $dest).Hash -ne $expected) { throw 'Wrong installed file' }
        Run-Installer 'already installed' 0 'already installed'
        Copy-Item $dest $name
    } catch {
        $failures++
        Write-Output "FAIL online: $_"
    }
    $env:LOCALAPPDATA = Join-Path $base 'offline profile'
    $env:HTTPS_PROXY = 'http://127.0.0.1:1'
    if (Test-Path $name) { Move-Item $name 'official.exe' }
    Run-Installer 'download blocked' 1 'Download failed:'
    $dest = Join-Path $env:LOCALAPPDATA "Programs\WindowsTools\$name"
    Set-Content -LiteralPath $name -Value 'corrupt'
    Run-Installer 'wrong offline digest' 1 'SHA-256 mismatch'
    if ((Test-Path $dest) -or (Get-ChildItem (Split-Path $dest) -Filter '*.tmp')) { throw 'Partial installation' }
    if ($OfflineExe) { Copy-Item -LiteralPath $OfflineExe -Destination $name -Force }
    else { Copy-Item -LiteralPath 'official.exe' -Destination $name -Force }
    Run-Installer 'offline recovery' 0 'installed.'
    if ((Get-FileHash $dest).Hash -ne $expected) { throw 'Wrong offline file' }
    Run-Installer 'offline repeat' 0 'already installed'
    Set-Content $dest 'older version'
    Run-Installer 'replace another version' 0 'installed.'
    if ((Get-FileHash $dest).Hash -ne $expected) { throw 'Another version was not replaced' }
} finally {
    Pop-Location
    $env:LOCALAPPDATA = $originalLocal
    $env:HTTPS_PROXY = $originalProxy
    Remove-Item Env:WT_NO_START
    Remove-Item -LiteralPath $base -Recurse -Force
}
if ($failures) { throw 'Online install failed; see its result above.' }
