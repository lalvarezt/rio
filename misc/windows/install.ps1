[CmdletBinding()]
param(
    [ValidateSet('release', 'debug')]
    [string]$Profile = 'release',
    [string]$Destination = "$env:USERPROFILE/.local/bin",
    [ValidateSet('x64', 'arm64', 'x86')]
    [string]$Architecture = [System.Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString().ToLowerInvariant()
)

$ErrorActionPreference = 'Stop'
Push-Location (Join-Path $PSScriptRoot '../..')
try {
    # Check before building so a running installation cannot be partly updated.
    foreach ($name in @('rio.exe', 'conpty.dll', 'OpenConsole.exe')) {
        $installed = Join-Path $Destination $name
        if (Test-Path $installed) {
            try {
                $handle = [IO.File]::Open($installed, 'Open', 'ReadWrite', 'None')
                $handle.Dispose()
            } catch {
                throw "Cannot update $installed. Close Rio windows using this installation, then rerun just install. $($_.Exception.Message)"
            }
        }
    }

    $cargoProfile = if ($Profile -eq 'release') { 'release' } else { 'dev' }
    if ($Profile -eq 'debug') {
        $env:CARGO_PROFILE_DEV_DEBUG = '0'
        $env:CARGO_INCREMENTAL = '1'
    }
    cargo build --profile $cargoProfile -p rioterm --bin rio --locked
    if ($LASTEXITCODE -ne 0) { throw "Cargo build failed (exit $LASTEXITCODE)." }

    $build = Join-Path './target' $Profile
    & "$PSScriptRoot/prepare-conpty.ps1" -Architecture $Architecture -Destination $build
    New-Item -ItemType Directory -Force $Destination | Out-Null
    Copy-Item "$build/rio.exe", "$build/conpty.dll", "$build/OpenConsole.exe", "$build/ConPTY-LICENSE.txt" $Destination -Force
    Write-Output "Installed Rio ($Profile) and ConPTY to $Destination"
} finally {
    Pop-Location
}
