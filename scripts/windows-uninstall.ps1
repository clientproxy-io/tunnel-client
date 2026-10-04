#Requires -RunAsAdministrator
<#
.SYNOPSIS
    Removes the tunnel-client Windows service and optionally its files.
#>

$ErrorActionPreference = 'Stop'

$InstallDir  = "$env:ProgramFiles\clientproxy\tunnel-client"
$ServiceName = 'tunnel-client'

Write-Host "=== clientproxy.io Tunnel Client Uninstaller ===" -ForegroundColor Cyan

$svc = "$InstallDir\tunnel-client-svc.exe"

if (Get-Service $ServiceName -ErrorAction SilentlyContinue) {
    Write-Host "Stopping and removing service..."
    & $svc stop      | Out-Null
    & $svc uninstall
    Write-Host "Service removed." -ForegroundColor Green
} else {
    Write-Host "Service not found — nothing to remove."
}

# Remove machine-level env vars
[System.Environment]::SetEnvironmentVariable('TUNNEL_API_URL', $null, 'Machine')
[System.Environment]::SetEnvironmentVariable('TUNNEL_ID',      $null, 'Machine')
[System.Environment]::SetEnvironmentVariable('TUNNEL_API_KEY', $null, 'Machine')

$removeFiles = Read-Host "Remove program files from $InstallDir? [y/N]"
if ($removeFiles -eq 'y' -or $removeFiles -eq 'Y') {
    Remove-Item -Recurse -Force $InstallDir -ErrorAction SilentlyContinue
    Write-Host "Program files removed." -ForegroundColor Green
    Write-Host "Note: config in $env:ProgramData\clientproxy\tunnel-client\ was kept."
}
