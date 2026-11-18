@echo off
setlocal
title Install Windows Tools
set "WT_INSTALLER_DIRECTORY=%~dp0"
set "WT_ADDRESS=%~1"
"%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -Command ^
  "$ErrorActionPreference = 'Stop';" ^
  "$version = 'v0.3.0';" ^
  "$expected = 'SET-AT-RELEASE-TO-THE-SHA256-OF-THE-RELEASE-EXE';" ^
  "$name = 'windows-tools-host.exe';" ^
  "$url = 'https://github.com/Doge-is-Dope/windows-tools/releases/download/' + $version + '/' + $name;" ^
  "$directory = Join-Path $env:LOCALAPPDATA 'Programs\WindowsTools';" ^
  "$destination = Join-Path $directory $name;" ^
  "$offline = Join-Path $env:WT_INSTALLER_DIRECTORY $name;" ^
  "$temporary = Join-Path $directory ([guid]::NewGuid().ToString() + '.tmp');" ^
  "try {" ^
  "  [void][IO.Directory]::CreateDirectory($directory);" ^
  "  if ((Test-Path -LiteralPath $destination) -and (Get-FileHash -LiteralPath $destination).Hash -eq $expected) {" ^
  "    Write-Host ('Windows Tools ' + $version + ' is already installed.');" ^
  "  } else {" ^
  "    if (Test-Path -LiteralPath $offline -PathType Leaf) { Copy-Item -LiteralPath $offline -Destination $temporary } else {" ^
  "      Write-Host ('Downloading Windows Tools ' + $version + '...');" ^
  "      [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12;" ^
  "      $proxy = @{}; if ($env:HTTPS_PROXY) { $proxy.Proxy = $env:HTTPS_PROXY };" ^
  "      try { Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $temporary -TimeoutSec 60 @proxy } catch {" ^
  "        throw ('Download failed: ' + $_.Exception.Message + [Environment]::NewLine + 'Retry, or place the ' + $version + ' exe beside this installer and run it again.')" ^
  "      }" ^
  "    };" ^
  "    if ((Get-FileHash -LiteralPath $temporary).Hash -ne $expected) { throw ('SHA-256 mismatch. Download the official ' + $version + ' exe and try again.') };" ^
  "    Get-Process -Name 'windows-tools-host' -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $destination } | Stop-Process -Force;" ^
  "    Start-Sleep -Milliseconds 500;" ^
  "    Move-Item -LiteralPath $temporary -Destination $destination -Force;" ^
  "    Write-Host ('Windows Tools ' + $version + ' installed.');" ^
  "  };" ^
  "  if ($env:WT_NO_START) { exit 0 };" ^
  "  $address = $env:WT_ADDRESS;" ^
  "  if (-not $address) {" ^
  "    $found = @(Get-NetIPAddress -AddressFamily IPv4 | Where-Object { $_.PrefixOrigin -ne 'WellKnown' } | ForEach-Object { $_.IPAddress });" ^
  "    if ($found.Count -eq 0) { throw 'This computer has no IPv4 address a controller can reach.' };" ^
  "    if ($found.Count -eq 1) { $address = $found[0] } else {" ^
  "      for ($i = 0; $i -lt $found.Count; $i++) { Write-Host ('[' + ($i + 1) + '] ' + $found[$i]) };" ^
  "      $choice = (Read-Host 'Which address does the controller reach? Number') -as [int];" ^
  "      if ($choice -lt 1 -or $choice -gt $found.Count) { throw 'No address chosen.' };" ^
  "      $address = $found[$choice - 1]" ^
  "    }" ^
  "  };" ^
  "  Write-Host 'Give this line to the controller; it is the key to this computer:';" ^
  "  & $destination --install $address;" ^
  "  if ($LASTEXITCODE) { throw 'The host could not be started.' }" ^
  "} catch { [Console]::Error.WriteLine($_.Exception.Message); exit 1 } finally {" ^
  "  if (Test-Path -LiteralPath $temporary) { Remove-Item -LiteralPath $temporary }" ^
  "}"
set "result=%errorlevel%"
pause
exit /b %result%
