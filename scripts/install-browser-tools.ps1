<#
.SYNOPSIS
    Installs the browser tools beside the host: Node and Chrome DevTools MCP, pinned and
    hash-checked, without npm. Google Chrome is installed separately.

    powershell -NoProfile -ExecutionPolicy Bypass -File install-browser-tools.ps1

    Run it as the session's user, then restart the host (sign out and in, or run the host
    installer again). An archive placed beside this script is used instead of downloading.
#>
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$directory = Join-Path $env:LOCALAPPDATA 'Programs\WindowsTools'
$node = @{ Version = '24.22.3'; Sha256 = 'e612af99f626ab415ea225af6bce5c971feb448cb5cae250bb1c3fa50c5f7d9f' }
$server = @{ Version = '1.10.1'; Sha256 = '012cbcf6e832d4f6709dad0c21d7bef17089e94adee9cf33179d15ea0a9adf2b' }

function Get-Archive([string]$Name, [string]$Url, [string]$Sha256) {
    $path = Join-Path $directory $Name
    $offline = Join-Path $PSScriptRoot $Name
    if (Test-Path -LiteralPath $offline) { Copy-Item -LiteralPath $offline -Destination $path -Force }
    elseif (-not (Test-Path -LiteralPath $path)) {
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
        Invoke-WebRequest -UseBasicParsing -Uri $Url -OutFile $path -TimeoutSec 120
    }
    if ((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ne $Sha256) {
        Remove-Item -LiteralPath $path
        throw "SHA-256 mismatch: $Name. Nothing was installed; try again."
    }
    $path
}

[void][IO.Directory]::CreateDirectory($directory)
$nodeExe = Join-Path $directory 'node\node.exe'
if ((Test-Path -LiteralPath $nodeExe) -and (& $nodeExe --version) -eq "v$($node.Version)") {
    Write-Host "Node $($node.Version) is already installed."
} else {
    $name = "node-v$($node.Version)-win-x64"
    $zip = Get-Archive "$name.zip" "https://nodejs.org/dist/v$($node.Version)/$name.zip" $node.Sha256
    Expand-Archive -LiteralPath $zip -DestinationPath $directory -Force
    $target = Join-Path $directory 'node'
    if (Test-Path -LiteralPath $target) { Remove-Item -LiteralPath $target -Recurse -Force }
    Move-Item -LiteralPath (Join-Path $directory $name) -Destination $target
    Remove-Item -LiteralPath $zip
    Write-Host "Node $($node.Version) installed."
}

$browser = Join-Path $directory 'browser'
$manifest = Join-Path $browser 'package\package.json'
$installed = if (Test-Path -LiteralPath $manifest) { (Get-Content -LiteralPath $manifest -Raw | ConvertFrom-Json).version }
if ($installed -eq $server.Version) {
    Write-Host "Chrome DevTools MCP $($server.Version) is already installed."
} else {
    $name = "chrome-devtools-mcp-$($server.Version).tgz"
    $tgz = Get-Archive $name "https://registry.npmjs.org/chrome-devtools-mcp/-/$name" $server.Sha256
    if (Test-Path -LiteralPath $browser) { Remove-Item -LiteralPath $browser -Recurse -Force }
    [void][IO.Directory]::CreateDirectory($browser)
    & tar -xzf $tgz -C $browser
    if ($LASTEXITCODE) { throw "Could not extract $name." }
    Remove-Item -LiteralPath $tgz
    Write-Host "Chrome DevTools MCP $($server.Version) installed."
}

$env:CHROME_DEVTOOLS_MCP_NO_UPDATE_CHECKS = '1'
& $nodeExe (Join-Path $browser 'package\build\src\bin\chrome-devtools-mcp.js') --version
if ($LASTEXITCODE) { throw 'The browser tools could not start. Check that this computer allows node.exe beside the host to run.' }
$chrome = 'Google\Chrome\Application\chrome.exe'
if (-not (@($env:ProgramFiles, ${env:ProgramFiles(x86)}, $env:LOCALAPPDATA) | Where-Object { $_ -and (Test-Path -LiteralPath (Join-Path $_ $chrome)) })) {
    Write-Warning 'Google Chrome is not installed; the browser tools need it.'
}
Write-Host 'Restart the host to offer the browser tools.'
