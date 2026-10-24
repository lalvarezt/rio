$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/../runtime-utils.ps1"
$root = [IO.Path]::GetFullPath("$PSScriptRoot/../../..")
$scratch = Join-Path $root "target/runtime-cache-$([Guid]::NewGuid().ToString('N'))"
$handles = @()
function Assert($condition, $message) { if (!$condition) { throw $message } }
function ReadLock($path) { [IO.File]::Open($path, 'Open', 'Read', 'Read') }
try {
    $build = Join-Path $scratch 'build'
    $installed = Join-Path $scratch 'installed'
    [void][IO.Directory]::CreateDirectory($build)
    $binary = Join-Path $build 'rio.exe'
    [IO.File]::WriteAllText($binary, 'test executable fixture')
    & "$PSScriptRoot/../install.ps1" -Binary $binary -Destination $installed -Architecture x64
    Assert (!(Test-Path (Join-Path $installed 'rio-ssh.exe'))) 'Native SSH installation copied legacy Plink'
    Assert (!(Test-Path (Join-Path $installed 'rio-ssh-agent.exe'))) 'Native SSH installation copied legacy agent bridge'
    foreach ($directory in @($build, $installed)) {
        foreach ($file in (Get-ChildItem $directory -File)) { $handles += ReadLock $file.FullName }
    }
    $output = & "$PSScriptRoot/../install.ps1" -Binary $binary -Destination $installed -Architecture x64
    Assert ($output[-1] -match '\(0 files updated\)') 'Matching locked installation was rewritten'
    foreach ($handle in $handles) { $handle.Dispose() }; $handles = @()

    $conpty = Join-Path $scratch 'conpty'
    & "$PSScriptRoot/../prepare-conpty.ps1" -Architecture x64 -Destination $conpty
    $archive = Get-ChildItem "$root/vendor/windows-console/*.nupkg" | Select-Object -First 1
    $handles += [IO.File]::Open($archive.FullName, 'Open', 'Read', 'None')
    foreach ($file in (Get-ChildItem $conpty -File)) { $handles += ReadLock $file.FullName }
    & "$PSScriptRoot/../prepare-conpty.ps1" -Architecture x64 -Destination $conpty
    foreach ($handle in $handles) { $handle.Dispose() }; $handles = @()
    $dll = Join-Path $conpty 'conpty.dll'
    $expected = Get-FileHashString $dll
    $handles += ReadLock (Join-Path $conpty 'OpenConsole.exe')
    foreach ($mode in @('missing', 'corrupt')) {
        if ($mode -eq 'missing') { [IO.File]::Delete($dll) }
        else { [IO.File]::WriteAllText($dll, 'corrupt') }
        & "$PSScriptRoot/../prepare-conpty.ps1" -Architecture x64 -Destination $conpty
        Assert (Test-FileHash $dll $expected) "Failed to repair $mode DLL"
    }
    foreach ($handle in $handles) { $handle.Dispose() }; $handles = @()

    $first = Join-Path $installed 'conpty.dll'
    $locked = Join-Path $installed 'rio.exe'
    [IO.File]::WriteAllText($first, 'first changed file')
    [IO.File]::WriteAllText($locked, 'locked changed file')
    $handles += ReadLock $locked
    $failed = $false
    try { & "$PSScriptRoot/../install.ps1" -Binary $binary -Destination $installed -Architecture x64 }
    catch { $failed = $_.Exception.Message -match 'Cannot update' }
    Assert $failed 'Changed locked target must reject installation'
    Assert ([IO.File]::ReadAllText($first) -eq 'first changed file') 'Installer partially updated destination'
    foreach ($handle in $handles) { $handle.Dispose() }; $handles = @()
    & "$PSScriptRoot/../install.ps1" -Binary $binary -Destination $installed -Architecture x64
    Assert ((Get-FileHashString $locked) -eq (Get-FileHashString $binary)) 'Unlocked retry failed'
    Write-Output 'PASS: locked matching install, cached archive, missing/corrupt repair, atomic lock preflight, retry'
} finally {
    foreach ($handle in $handles) { $handle.Dispose() }
    if (Test-Path $scratch) { Remove-Item $scratch -Recurse -Force }
}
