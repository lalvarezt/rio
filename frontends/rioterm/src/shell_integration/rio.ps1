# Reports the working directory to rio (OSC 7, kitty-shell-cwd flavor:
# the path travels verbatim, so spaces, `#` and `%` need no encoding),
# which powers the title path variables and opening new tabs in the
# current directory. UNC paths are skipped: the verbatim flavor has no
# host field to mark them as belonging to another machine. Loading
# twice is a no-op.
if ($Global:__RioShellIntegration) { return }
$Global:__RioShellIntegration = $true

$Global:__RioOriginalPrompt = $Function:Prompt

function Global:__RioReportApp([string]$App) {
    $remote = if ($env:SSH_CONNECTION -or $env:SSH_CLIENT -or $env:SSH_TTY) { '1' } else { '0' }
    $program = [IO.Path]::GetFileNameWithoutExtension($App)
    $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($program))
    $transport = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($remote))
    [Console]::Write("$([char]27)]1337;SetUserVar=RIO_SSH=$transport$([char]7)$([char]27)]1337;SetUserVar=RIO_APP=$encoded$([char]7)")
}

if (Get-Command Set-PSReadLineOption -ErrorAction SilentlyContinue) {
    $Global:__RioOriginalHistoryHandler = (Get-PSReadLineOption).AddToHistoryHandler
    Set-PSReadLineOption -AddToHistoryHandler {
        param([string]$Line)
        $tokens = $null
        $parseErrors = $null
        $ast = [System.Management.Automation.Language.Parser]::ParseInput($Line, [ref]$tokens, [ref]$parseErrors)
        $command = $ast.Find({ param($node) $node -is [System.Management.Automation.Language.CommandAst] }, $true)
        if ($command) {
            $name = $command.GetCommandName()
            if ($name) { __RioReportApp $name }
        }
        if ($Global:__RioOriginalHistoryHandler) {
            return $Global:__RioOriginalHistoryHandler.Invoke($Line)
        }
        return $true
    }
}

function Global:Prompt {
    # Preserve the state the user's prompt inspects: $LASTEXITCODE is
    # restored directly, and since $? is not assignable, a deliberately
    # failing statement re-arms it when the last command failed.
    $Global:__RioLastSuccess = $?
    $Global:__RioLastExitCode = $Global:LASTEXITCODE
    $shell = if ($PSVersionTable.PSEdition -eq 'Core') { 'pwsh' } else { 'powershell' }
    __RioReportApp $shell
    $location = $ExecutionContext.SessionState.Path.CurrentLocation
    if ($location.Provider.Name -eq 'FileSystem') {
        $path = $location.ProviderPath
        if (-not $path.StartsWith('\\')) {
            if (-not $path.StartsWith('/')) { $path = "/$path" }
            [Console]::Write("$([char]27)]7;kitty-shell-cwd://$path$([char]7)")
        }
    }
    $Global:LASTEXITCODE = $Global:__RioLastExitCode
    if ($Global:__RioLastSuccess) {
        & $Global:__RioOriginalPrompt
    } else {
        Microsoft.PowerShell.Utility\Write-Error '' -ErrorAction SilentlyContinue
        & $Global:__RioOriginalPrompt
    }
}
