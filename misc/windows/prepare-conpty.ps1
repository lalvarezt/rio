[CmdletBinding()]
param(
    [ValidateSet('x64', 'arm64', 'x86')]
    [string[]]$Architecture = @('x64', 'arm64'),
    [string]$Destination
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
if ($Destination -and $Architecture.Count -ne 1) {
    throw 'Choose one architecture when specifying a destination.'
}

# 1.25 synchronizes the console cursor with the terminal after reflow.
# Older ConPTY versions can overwrite earlier output when PowerShell redraws.
$version = '1.25.260930003'
$sha256 = '02B07B349AF66D801159BDF9E440D4A1CE78BB951F37FC8609731665AFDAE7EE'
$root = Join-Path $PSScriptRoot '../../target/conpty'
$archive = Join-Path $root "$version.zip"
$package = Join-Path $root $version
New-Item -ItemType Directory -Force $root | Out-Null
if (-not (Test-Path $archive)) {
    $url = "https://api.nuget.org/v3-flatcontainer/microsoft.windows.console.conpty/$version/microsoft.windows.console.conpty.$version.nupkg"
    Invoke-WebRequest $url -OutFile $archive -UseBasicParsing
}
if ((Get-FileHash $archive -Algorithm SHA256).Hash -ne $sha256) {
    throw "ConPTY package checksum mismatch: $archive"
}
Expand-Archive $archive -DestinationPath $package -Force

foreach ($arch in $Architecture) {
    $output = if ($Destination) { $Destination } else { Join-Path $root $arch }
    New-Item -ItemType Directory -Force $output | Out-Null
    Copy-Item "$package/runtimes/win-$arch/native/conpty.dll" $output -Force
    Copy-Item "$package/build/native/runtimes/$arch/OpenConsole.exe" $output -Force
    Copy-Item "$PSScriptRoot/ConPTY-LICENSE.txt" $output -Force
    Write-Output "ConPTY $version ($arch): $output"
}
