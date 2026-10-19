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
New-Item -ItemType Directory -Force $root | Out-Null
$download = "$archive.download-$PID"
try {
    $source = $archive
    if (-not [IO.File]::Exists($archive)) {
        $url = "https://api.nuget.org/v3-flatcontainer/microsoft.windows.console.conpty/$version/microsoft.windows.console.conpty.$version.nupkg"
        $client = [Net.WebClient]::new()
        $protocol = [Net.ServicePointManager]::SecurityProtocol
        try {
            [Net.ServicePointManager]::SecurityProtocol = $protocol -bor [Net.SecurityProtocolType]::Tls12
            $client.DownloadFile($url, $download)
        } finally {
            $client.Dispose()
            [Net.ServicePointManager]::SecurityProtocol = $protocol
        }
        $source = $download
    }

    # .NET works in both PowerShell hosts without optional hash/archive cmdlets.
    $stream = [IO.File]::OpenRead($source)
    $hash = [Security.Cryptography.SHA256]::Create()
    try {
        $actual = [BitConverter]::ToString($hash.ComputeHash($stream)).Replace('-', '')
    } finally {
        $hash.Dispose()
        $stream.Dispose()
    }
    if ($actual -ne $sha256) {
        throw "ConPTY package checksum mismatch: $source"
    }
    if ($source -eq $download) { [IO.File]::Move($download, $archive) }

    [void][Reflection.Assembly]::LoadWithPartialName('System.IO.Compression.FileSystem')
    $zip = [IO.Compression.ZipFile]::OpenRead($archive)
    try {
        foreach ($arch in $Architecture) {
            $output = if ($Destination) { $Destination } else { Join-Path $root $arch }
            $output = [IO.Path]::GetFullPath($output)
            [void][IO.Directory]::CreateDirectory($output)
            foreach ($entryPath in @("runtimes/win-$arch/native/conpty.dll", "build/native/runtimes/$arch/OpenConsole.exe")) {
                $entry = $zip.GetEntry($entryPath)
                if ($null -eq $entry) { throw "ConPTY package is missing $entryPath" }
                [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, (Join-Path $output $entry.Name), $true)
            }
            [IO.File]::Copy("$PSScriptRoot/ConPTY-LICENSE.txt", (Join-Path $output 'ConPTY-LICENSE.txt'), $true)
            Write-Output "ConPTY $version ($arch): $output"
        }
    } finally {
        $zip.Dispose()
    }
} finally {
    if ([IO.File]::Exists($download)) { [IO.File]::Delete($download) }
}
