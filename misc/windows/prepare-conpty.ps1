[CmdletBinding()]
param(
    [ValidateSet('x64', 'arm64', 'x86')]
    [string[]]$Architecture = @('x64', 'arm64'),
    [string]$Destination
)

$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/runtime-utils.ps1"
$ProgressPreference = 'SilentlyContinue'
if ($Destination -and $Architecture.Count -ne 1) {
    throw 'Choose one architecture when specifying a destination.'
}
if ($Destination) { $Destination = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Destination) }

# 1.25 synchronizes the console cursor with the terminal after reflow.
# Older ConPTY versions can overwrite earlier output when PowerShell redraws.
$version = '1.25.260930003'
$sha256 = '02B07B349AF66D801159BDF9E440D4A1CE78BB951F37FC8609731665AFDAE7EE'
$root = Join-Path $PSScriptRoot '../../target/conpty'
$archive = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "../../vendor/windows-console/microsoft.windows.console.conpty.$version.nupkg"))
$checksums = Read-HashManifest (Join-Path $PSScriptRoot '../../vendor/windows-console/SHA256SUMS')
$outputs = @{}
$extract = @(
    foreach ($arch in $Architecture) {
        $output = if ($Destination) { $Destination } else { Join-Path $root $arch }
        $outputs[$arch] = [IO.Path]::GetFullPath($output)
        foreach ($entry in @("runtimes/win-$arch/native/conpty.dll", "build/native/runtimes/$arch/OpenConsole.exe")) {
            $name = [IO.Path]::GetFileName($entry)
            $expected = $checksums["$arch/$name"]
            if (!$expected) { throw "Missing ConPTY checksum: $arch/$name" }
            $target = Join-Path $outputs[$arch] $name
            if (!(Test-FileHash $target $expected)) {
                [pscustomobject]@{ Entry = $entry; Target = $target; Hash = $expected }
            }
        }
    }
)
$licenses = @(foreach ($output in $outputs.Values) {
    Get-FileUpdate "$PSScriptRoot/ConPTY-LICENSE.txt" (Join-Path $output 'ConPTY-LICENSE.txt')
})
foreach ($update in @($extract) + @($licenses)) { Assert-FileCanChange $update.Target }

# A verified output needs neither archive hashing nor decompression. Verify the
# pinned archive only when at least one output must be repaired or extracted.
if ($extract.Count) {
    if (!(Test-FileHash $archive $sha256)) { throw "ConPTY package checksum mismatch: $archive" }
    [void][Reflection.Assembly]::LoadWithPartialName('System.IO.Compression.FileSystem')
    $zip = [IO.Compression.ZipFile]::OpenRead($archive)
    try {
        foreach ($update in $extract) {
            if (!$zip.GetEntry($update.Entry)) { throw "ConPTY package is missing $($update.Entry)" }
        }
        foreach ($update in $extract) {
            [void][IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($update.Target))
            [IO.Compression.ZipFileExtensions]::ExtractToFile($zip.GetEntry($update.Entry), $update.Target, $true)
            if (!(Test-FileHash $update.Target $update.Hash)) { throw "ConPTY extraction checksum mismatch: $($update.Target)" }
        }
    } finally { $zip.Dispose() }
}
foreach ($update in $licenses) {
    [void](Copy-FileIfChanged $update.Source $update.Target $update.Hash)
}
foreach ($arch in $Architecture) {
    Write-Output "ConPTY $version ($arch): $($outputs[$arch])"
}
