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
. "$PSScriptRoot/runtime-utils.ps1"
$Destination = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Destination)
if ($Binary) { $Binary = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Binary) }
$oldDebug = $env:CARGO_PROFILE_DEV_DEBUG
$oldIncremental = $env:CARGO_INCREMENTAL
Push-Location (Join-Path $PSScriptRoot '../..')
try {
    $runtimeFiles = @('conpty.dll', 'OpenConsole.exe', 'ConPTY-LICENSE.txt', 'Font-LICENSE.txt')
    if ($Binary -and -not [IO.File]::Exists($Binary)) { throw "Rio executable not found: $Binary" }

    # Prepare the pinned local runtime before spending time on compilation.
    & "$PSScriptRoot/prepare-runtime.ps1" -Architecture $Architecture
    $runtime = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "../../target/runtime/$Architecture"))
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

    $build = [IO.Path]::GetDirectoryName($Binary)
    $updates = @(
        foreach ($name in $runtimeFiles) {
            $source = Join-Path $runtime $name
            $hash = Get-FileHashString $source
            foreach ($directory in (@($build, $Destination) | Select-Object -Unique)) {
                Get-FileUpdate $source (Join-Path $directory $name) $hash
            }
        }
        Get-FileUpdate $Binary (Join-Path $Destination 'rio.exe')
    )
    # Unchanged files may remain in use. Preflight every changed file before
    # copying any of them, including dependencies beside the Cargo executable.
    foreach ($update in $updates) { Assert-FileCanChange $update.Target }
    foreach ($update in $updates) {
        [void](Copy-FileIfChanged $update.Source $update.Target $update.Hash)
    }
    Write-Output "Installed Rio ($Profile) to $Destination ($($updates.Count) files updated)"
} finally {
    $env:CARGO_PROFILE_DEV_DEBUG = $oldDebug
    $env:CARGO_INCREMENTAL = $oldIncremental
    Pop-Location
}
