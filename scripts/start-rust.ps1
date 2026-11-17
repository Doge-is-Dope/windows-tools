param([Parameter(Mandatory=$true)][string]$ListenAddress,[string]$StateDirectory)
# Prepares a new private state directory and serves from it, in this console. To run without a window:
#   conhost.exe --headless <host.exe> --serve <state directory>\connection.json
$ErrorActionPreference='Stop'
$root=Split-Path $PSScriptRoot -Parent
if(-not (Get-NetIPAddress -AddressFamily IPv4|Where-Object {$_.IPAddress -eq $ListenAddress})){throw 'Choose an IPv4 assigned to this Windows computer.'}
if(-not $StateDirectory){$StateDirectory=Join-Path $root ('.local\host-'+(Get-Date -Format 'yyyyMMdd-HHmmss'))}
$hostFile=Join-Path $root 'artifacts\rust\debug\windows-tools-host.exe'
& $hostFile --prepare $StateDirectory $ListenAddress
if($LASTEXITCODE -ne 0){throw 'Preparing the state directory failed.'}
& $hostFile --serve (Join-Path $StateDirectory 'connection.json')
exit $LASTEXITCODE
