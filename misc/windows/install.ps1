[CmdletBinding()]
param(
    [ValidateSet('release', 'debug')]
    [string]$Profile = 'release',
    [string]$Destination = "$env:USERPROFILE/.local/bin",
    # Install an existing executable without rebuilding it.
    [string]$Binary,
    [ValidateSet('x64', 'arm64', 'x86')]
    [string]$Architecture = [System.Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString().ToLowerInvariant()
)

$ErrorActionPreference = 'Stop'
$Destination = [IO.Path]::GetFullPath($Destination)
if ($Binary) { $Binary = [IO.Path]::GetFullPath($Binary) }
$oldDebug = $env:CARGO_PROFILE_DEV_DEBUG
$oldIncremental = $env:CARGO_INCREMENTAL
Push-Location (Join-Path $PSScriptRoot '../..')
try {
    # Check before building so a running installation cannot be partly updated.
    foreach ($name in @('rio.exe', 'conpty.dll', 'OpenConsole.exe', 'ConPTY-LICENSE.txt')) {
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

    if ($Binary -and -not [IO.File]::Exists($Binary)) { throw "Rio executable not found: $Binary" }

    # Resolve runtime/download errors before spending time on compilation.
    & "$PSScriptRoot/prepare-conpty.ps1" -Architecture $Architecture
    $runtime = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "../../target/conpty/$Architecture"))
    if (-not $Binary) {
        $cargoProfile = if ($Profile -eq 'release') { 'release' } else { 'dev' }
        if ($Profile -eq 'debug') {
            $env:CARGO_PROFILE_DEV_DEBUG = '0'
            $env:CARGO_INCREMENTAL = '1'
        }
        # Cargo reports the real output path, including custom target directories.
        cargo build --profile $cargoProfile -p rioterm --bin rio --locked --message-format=json-render-diagnostics | ForEach-Object {
            $artifact = $_ | ConvertFrom-Json
            if ($artifact.reason -eq 'compiler-artifact' -and $artifact.target.name -eq 'rio' -and $artifact.executable) {
                $Binary = $artifact.executable
            }
        }
        if ($LASTEXITCODE -ne 0) { throw "Cargo build failed (exit $LASTEXITCODE)." }
    }
    if (-not $Binary -or -not [IO.File]::Exists($Binary)) { throw 'Cargo did not produce a Rio executable.' }

    [void][IO.Directory]::CreateDirectory($Destination)
    $build = [IO.Path]::GetDirectoryName($Binary)
    foreach ($name in @('conpty.dll', 'OpenConsole.exe', 'ConPTY-LICENSE.txt')) {
        foreach ($directory in @($build, $Destination)) {
            $target = Join-Path $directory $name
            if ((Join-Path $runtime $name) -ine $target) { [IO.File]::Copy((Join-Path $runtime $name), $target, $true) }
        }
    }
    $installedBinary = Join-Path $Destination 'rio.exe'
    if ($Binary -ine $installedBinary) { [IO.File]::Copy($Binary, $installedBinary, $true) }
    Write-Output "Installed Rio ($Profile) and ConPTY to $Destination"
} finally {
    $env:CARGO_PROFILE_DEV_DEBUG = $oldDebug
    $env:CARGO_INCREMENTAL = $oldIncremental
    Pop-Location
}
