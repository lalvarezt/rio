# SSH tabs and splits

Rio can open remote shells with OpenSSH.
New Tab Here, Split Right, and Split Down use the focused pane's SSH destination.
The original shell and its programs keep running.

Open the command palette and choose **Connect via SSH…**. Enter an SSH config
alias, `user@host`, or an `ssh://` address. The picker also lists saved connections
and literal aliases from `~/.ssh/config`. OpenSSH handles authentication, host keys,
agents, and jump hosts. On Unix clients, Rio enables connection sharing and keeps
an idle master for up to ten minutes after its last session closes.

On Windows clients, Rio uses native Windows OpenSSH. Each tab or split opens its
own connection, using OpenSSH's configuration, host-key verification and Windows
agent directly. Authentication prompts appear in the terminal. Rio does not
require a Plink executable or agent bridge. SSH configuration failures appear in
the picker with OpenSSH's diagnostic text, and the destination stays available
for retry.

For a Windows server, choose **Connect via SSH with PowerShell…**. This starts
PowerShell even when the server's default shell is cmd.exe. Rio sends its prompt
integration inline so directory changes reach the terminal. It installs no files
or services on the server. Comments and indentation are omitted from the encoded
script to leave room for the directory within Windows command-line limits.

New tabs and splits use the last reported remote directory when available and
when `navigation.current-working-directory` is enabled. Shells can report it with
OSC 7. A remote hostname is checked against the SSH destination's resolved host.
Remote paths stay separate from local paths. Without a directory report, a fresh
remote shell starts in its usual directory. POSIX remote shells need their own
directory-reporting integration; the default SSH launcher preserves the server's
normal shell startup.

**New Local Tab** opens a local shell even from an SSH pane. **Save SSH Connection**
saves the focused connection's destination, shell choice, and supported identity
options. Connections opened through the picker are saved automatically in
`rio/ssh-connections.toml` under the user's configuration directory. This file
contains connection recipes, not passwords or MFA codes. Saved SSH `-o` values
are limited to identity settings and configuration references. Environment values
and proxy commands are excluded because they can contain credentials.
Configuration remains owned by OpenSSH; use an SSH alias for complex settings.

On Unix clients, Rio also recognizes foreground OpenSSH commands launched manually
inside a pane. Such a connection must already have sharing enabled, for example
through `ControlMaster`, `ControlPath`, and `ControlPersist` in SSH configuration.
Rio cannot retrofit sharing onto an existing non-shared connection. On Windows
clients, foreground command discovery is unavailable. Rio can preserve SSH when
it launches the client itself, through the picker or its `-e ssh ...` option.
An explicitly selected POSIX shell, such as `ssh host /path/to/fish`, is retained
in sibling panes. Arbitrary remote commands are not replayed.

On Unix clients, Rio checks the shared connection before creating a sibling. A
missing connection produces an error instead of a credential prompt. If the master disappears during
creation, fresh authentication is disabled. Forwarding requests and remote commands
are not replayed in sibling panes. Shell variables and running programs are not
copied. Saved recipes can establish a new login after a disconnect; they do not
recover a previous process.

The local tests run with `cargo test -p rioterm` and
`cargo test -p rio-vt current_directory`. The optional Windows SSH integration test
requires an authenticated master and reads its target from `RIO_TEST_SSH_TARGET`:

```sh
RIO_TEST_SSH_TARGET=user@windows-host cargo test -p rioterm live_ssh_tabs_and_split_launches -- --ignored --nocapture
```

That test uses real PTYs and Rio's parser. It verifies directory inheritance,
independent shell state, the same server-side SSH transport, foreground SSH
discovery, and survival of a sibling after the original shell closes. It exercises
the shared launch path used by both split directions without creating a GPU window.

On a Windows client, `live_windows_ssh_connection_uses_conpty` verifies a real
SSH login through Rio's ConPTY backend. It requires working authentication and
remote shell integration that reports `RIO_SSH`. `RIO_TEST_SSH_TARGET` selects
the destination. Optional `RIO_TEST_SSH_COMMAND` selects the remote shell:

```powershell
$env:RIO_TEST_SSH_TARGET = 'user@linux-host'
$env:RIO_TEST_SSH_COMMAND = '/path/to/fish'
cargo test -p rioterm live_windows_ssh_connection_uses_conpty -- --ignored --nocapture
```

This test verifies independent Windows SSH transports, remote directory
inheritance, Unicode, resize, Ctrl+C and survival after the parent pane closes.
`windows_fish_arrow_editing` additionally checks arrows, history, word movement
and autosuggestions through ConPTY. `windows_local_and_ssh_performance` compares
local, direct native SSH and Rio-generated SSH command round trips. Both use
`RIO_PERF_SSH_TARGET`; the Fish check accepts `RIO_TEST_SSH_COMMAND` for its shell
executable.
