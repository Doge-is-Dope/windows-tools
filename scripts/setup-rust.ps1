$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$root = Split-Path $PSScriptRoot -Parent
$tools = if ($env:WT_TOOLS_ROOT) { $env:WT_TOOLS_ROOT } else { Join-Path $root 'tools' }
New-Item -ItemType Directory -Force -Path $tools | Out-Null
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$msvc = if (Test-Path $vswhere) {
    & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
} else { $null }
if (-not $msvc) {
    $principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'MSVC is missing. Run this script in an administrator PowerShell on Windows to install Microsoft C++ Build Tools and the Windows SDK. No installation was started.'
    }
    $installer = Join-Path $tools 'vs_BuildTools.exe'
    Invoke-WebRequest -UseBasicParsing 'https://aka.ms/vs/17/release/vs_BuildTools.exe' -OutFile $installer
    $signature = Get-AuthenticodeSignature -FilePath $installer
    if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'Microsoft Corporation') {
        throw 'The Build Tools installer did not pass Microsoft signature verification.'
    }
    $arguments = @('--quiet','--wait','--norestart','--nocache','--installPath',('"' + (Join-Path $tools 'msvc') + '"'),
        '--add','Microsoft.VisualStudio.Component.VC.Tools.x86.x64',
        '--add','Microsoft.VisualStudio.Component.Windows11SDK.26100')
    $install = Start-Process $installer -ArgumentList $arguments -PassThru
    $null = $install.Handle # PowerShell 5.1 needs the handle retained for ExitCode.
    $install.WaitForExit()
    if ($install.ExitCode -eq 3010) { throw 'Microsoft Build Tools installed and requires a Windows restart; restart manually before continuing.' }
    if ($install.ExitCode -ne 0) { throw ('Build Tools installation failed with exit code ' + $install.ExitCode) }
}

$env:CARGO_HOME = Join-Path $tools 'cargo'
$env:RUSTUP_HOME = Join-Path $tools 'rustup'
$rustup = Join-Path $tools 'rustup-init.exe'
$rustupUrl = 'https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe'
Invoke-WebRequest -UseBasicParsing $rustupUrl -OutFile $rustup
$hashPath = $rustup + '.sha256'
Invoke-WebRequest -UseBasicParsing ($rustupUrl + '.sha256') -OutFile $hashPath
$expected = [IO.File]::ReadAllText($hashPath).Trim().Split(' ')[0]
if ($expected -notmatch '^[a-fA-F0-9]{64}$' -or (Get-FileHash $rustup -Algorithm SHA256).Hash -ne $expected) {
    throw 'Rustup installer SHA256 verification failed.'
}
$install = Start-Process $rustup -ArgumentList @('-y','--profile','default','--default-host','x86_64-pc-windows-msvc','--default-toolchain','stable','--no-modify-path') -PassThru -NoNewWindow
$null = $install.Handle
$install.WaitForExit()
if ($install.ExitCode -ne 0) { throw ('Rust installation failed with exit code ' + $install.ExitCode) }
& (Join-Path $env:CARGO_HOME 'bin\rustc.exe') --version
if ($LASTEXITCODE -ne 0) { throw 'Installed rustc did not pass version verification.' }
& (Join-Path $env:CARGO_HOME 'bin\cargo.exe') --version
if ($LASTEXITCODE -ne 0) { throw 'Installed cargo did not pass version verification.' }
