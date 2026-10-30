# Local shell only: remote integration must not refer to the local Rio binary.
if ($env:RIO_BIN -and -not (Get-Command ssh -CommandType Function,Alias -ErrorAction SilentlyContinue)) {
    function Global:ssh {
        $native = (Get-Command ssh -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
        if ($env:OS -eq 'Windows_NT') {
            # Rio is a GUI-subsystem executable. Wait explicitly and preserve
            # console handles; piping its output would break interactive SSH.
            $words = @('+ssh', '--ssh', $native, '--') + $args
            $quoted = foreach ($word in $words) {
                $word = [regex]::Replace([string]$word, '(\\*)"', '$1$1\"')
                '"' + [regex]::Replace($word, '(\\+)$', '$1$1') + '"'
            }
            $child = Start-Process -FilePath $env:RIO_BIN -ArgumentList ($quoted -join ' ') -NoNewWindow -PassThru
            $null = $child.Handle
            $child.WaitForExit()
            $global:LASTEXITCODE = $child.ExitCode
        } else {
            & $env:RIO_BIN +ssh --ssh $native -- @args
        }
    }
}
