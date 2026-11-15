[CmdletBinding()]
param(
    [ValidateSet('x64', 'arm64')]
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
& pwsh -NoProfile -ExecutionPolicy Bypass -File "$PSScriptRoot/conhost/fetch.ps1"
if ($LASTEXITCODE -ne 0) { throw "ConPTY preparation failed (exit $LASTEXITCODE)." }
foreach ($arch in $Architecture) {
    $output = if ($Destination) { $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Destination) } else { Join-Path $root "target/runtime/$arch" }
    $packageArch = if ($arch -eq 'x64') { 'amd64' } else { $arch }
    $source = Join-Path $PSScriptRoot "conhost/$packageArch"
    foreach ($file in (Get-ChildItem -LiteralPath $source -File -Recurse)) {
        $relative = $file.FullName.Substring($source.Length + 1)
        [void](Copy-FileIfChanged $file.FullName (Join-Path $output $relative))
    }
    [void](Copy-FileIfChanged "$PSScriptRoot/conhost/LICENSE" (Join-Path $output 'LICENSE-ConPTY.txt'))
    foreach ($name in $licenses.Keys) {
        [void](Copy-FileIfChanged (Join-Path $root $licenses[$name]) (Join-Path $output $name))
    }
    Write-Output "Windows runtime ($arch): $output"
}
