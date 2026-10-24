function Get-FileHashString([string]$Path) {
    $stream = [IO.File]::OpenRead($Path)
    $hash = [Security.Cryptography.SHA256]::Create()
    try { [BitConverter]::ToString($hash.ComputeHash($stream)).Replace('-', '') }
    finally { $stream.Dispose(); $hash.Dispose() }
}

function Test-FileHash([string]$Path, [string]$Hash) {
    [IO.File]::Exists($Path) -and (Get-FileHashString $Path) -ieq $Hash
}

function Read-HashManifest([string]$Path) {
    $hashes = @{}
    foreach ($line in [IO.File]::ReadAllLines($Path)) {
        $parts = $line -split '\s+', 2
        if ($parts.Count -ne 2 -or $parts[0] -notmatch '^[a-fA-F0-9]{64}$') {
            throw "Invalid checksum manifest: $Path"
        }
        $hashes[$parts[1]] = $parts[0]
    }
    $hashes
}

function Get-FileUpdate([string]$Source, [string]$Target, [string]$Hash) {
    if ([IO.Path]::GetFullPath($Source) -ieq [IO.Path]::GetFullPath($Target)) { return }
    if (!$Hash) { $Hash = Get-FileHashString $Source }
    if (!(Test-FileHash $Target $Hash)) {
        [pscustomobject]@{ Source = $Source; Target = $Target; Hash = $Hash }
    }
}

function Assert-FileCanChange([string]$Path) {
    if ([IO.File]::Exists($Path)) {
        try {
            $handle = [IO.File]::Open($Path, 'Open', 'ReadWrite', 'None')
            $handle.Dispose()
        } catch {
            throw "Cannot update $Path. Close Rio windows using this file, then rerun just install. $($_.Exception.Message)"
        }
    }
}

function Copy-FileIfChanged([string]$Source, [string]$Target, [string]$Hash) {
    if (Get-FileUpdate $Source $Target $Hash) {
        Assert-FileCanChange $Target
        [void][IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($Target))
        [IO.File]::Copy($Source, $Target, $true)
        return $true
    }
    return $false
}
