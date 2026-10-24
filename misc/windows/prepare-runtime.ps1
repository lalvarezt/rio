[CmdletBinding()]
param(
    [ValidateSet('x64', 'arm64', 'x86')]
    [string[]]$Architecture = @('x64', 'arm64'),
    [string]$Destination
)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/runtime-utils.ps1"
if ($Destination -and $Architecture.Count -ne 1) {
    throw 'Choose one architecture when specifying a destination.'
}
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$licenses = @{
    'Font-LICENSE.txt' = 'vendor/fonts/LICENSE.txt'
}
foreach ($path in $licenses.Values) {
    if (!(Test-Path (Join-Path $root $path))) { throw "Runtime license not found: $path" }
}
foreach ($arch in $Architecture) {
    $output = if ($Destination) { $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Destination) } else { Join-Path $root "target/runtime/$arch" }
    & "$PSScriptRoot/prepare-conpty.ps1" -Architecture $arch -Destination $output
    foreach ($name in $licenses.Keys) {
        [void](Copy-FileIfChanged (Join-Path $root $licenses[$name]) (Join-Path $output $name))
    }
    Write-Output "Windows runtime ($arch): $output"
}
