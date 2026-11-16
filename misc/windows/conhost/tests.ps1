[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$work = Join-Path ([IO.Path]::GetTempPath()) ("rio-conhost-test-" + [Guid]::NewGuid())
$engine = [Diagnostics.Process]::GetCurrentProcess().MainModule.FileName

try {
    $conhost = Join-Path $work 'conhost'
    [void][IO.Directory]::CreateDirectory($conhost)
    Copy-Item -LiteralPath "$PSScriptRoot/fetch.ps1" -Destination $conhost
    Copy-Item -LiteralPath "$PSScriptRoot/../runtime-utils.ps1" -Destination $work
    $driver = Join-Path $work 'driver.ps1'
    @'
param([string]$FetchPath, [string]$Hash, [string]$Version, [switch]$SkipHash)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
# Load utility commands before masking one; Windows PowerShell loads them lazily.
Import-Module Microsoft.PowerShell.Utility
function Get-FileHash {
    throw (New-Object Management.Automation.CommandNotFoundException "The term 'Get-FileHash' is not recognized")
}
function Invoke-WebRequest {
    param($Uri, $OutFile, [switch]$UseBasicParsing)
    [IO.File]::AppendAllText((Join-Path (Split-Path $FetchPath) 'requests.txt'), "download`n")
    [IO.File]::WriteAllBytes($OutFile, [Text.Encoding]::UTF8.GetBytes('Rio hash fixture'))
}
# Stop after hashing so these tests need neither Windows signatures nor a download.
$env:OS = 'ConhostFetchTest'
& $FetchPath -Version $Version -Sha256 $Hash -SkipHash:$SkipHash
'@ | Set-Content -LiteralPath $driver

    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        $expected = [BitConverter]::ToString(
            $sha.ComputeHash([Text.Encoding]::UTF8.GetBytes('Rio hash fixture'))
        ).Replace('-', '').ToLowerInvariant()
    } finally { $sha.Dispose() }

    $fetch = Join-Path $conhost 'fetch.ps1'
    $downloads = @{ valid = 1; cached = 1; mismatch = 2; skip = 3; corrupt = 4; version = 5 }
    foreach ($case in @('valid', 'cached', 'mismatch', 'skip', 'corrupt', 'version')) {
        $hash = if ($case -in @('mismatch', 'skip')) { '0' * 64 } else { $expected }
        $version = if ($case -eq 'version') { 'fixture-v2' } else { 'fixture-v1' }
        if ($case -eq 'corrupt') {
            [IO.File]::WriteAllText((Join-Path $conhost 'nupkg/fixture-v1/conpty.zip'), 'corrupt')
        }
        $arguments = @('-NoProfile', '-NonInteractive', '-File', $driver, $fetch, $hash, $version)
        if ($case -eq 'skip') { $arguments += '-SkipHash' }
        $previousPreference = $ErrorActionPreference
        try {
            # Windows PowerShell reports native stderr as error records.
            $ErrorActionPreference = 'Continue'
            $output = & $engine @arguments 2>&1
            $code = $LASTEXITCODE
        } finally { $ErrorActionPreference = $previousPreference }
        $text = $output -join "`n"
        if ($case -eq 'mismatch') {
            if ($code -eq 0 -or $text -notmatch 'SHA-256 of .* expected') {
                throw "Checksum mismatch was not rejected: $text"
            }
        } elseif ($code -ne 0) {
            throw "ConPTY fetch failed without Get-FileHash ($case): $text"
        } elseif ($case -eq 'skip' -and !$text.Contains("SHA-256 $expected")) {
            throw "SkipHash did not report the computed checksum: $text"
        }
        $requests = @(Get-Content -LiteralPath (Join-Path $conhost 'requests.txt')).Count
        if ($requests -ne $downloads[$case]) {
            throw "Unexpected download count for ${case}: $requests, expected $($downloads[$case])"
        }
        Write-Output "PASS: $case without Get-FileHash"
    }
} finally {
    if (Test-Path -LiteralPath $work) { Remove-Item -LiteralPath $work -Recurse -Force }
}
