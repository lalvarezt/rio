# Vendored Windows runtime files

Windows `just install` and `just install-debug` use the local files here; runtime
preparation does not download dependencies. SSH uses native Windows OpenSSH,
including its configuration, host verification and agent. No SSH backend or agent
bridge is vendored.

## Console runtime

`windows-console/` contains the original Microsoft ConPTY NuGet package
`Microsoft.Windows.Console.ConPTY` version `1.25.260930003`, from
<https://www.nuget.org/packages/Microsoft.Windows.Console.ConPTY/1.25.260930003>.
Its SHA-256 is
`02b07b349af66d801159bdf9e440d4a1ce78bb951f37fc8609731665afdae7ee`.

The package supplies matching `conpty.dll` and `OpenConsole.exe` files for x64,
ARM64 and x86. They provide consistent console reflow and cursor behavior across
Windows hosts. Both files are unmodified; keep them together beside Rio. Rio
loads the DLL beside its executable and can fall back to the host ConPTY API.
The Microsoft redistribution license is installed beside the binaries.

`misc/windows/prepare-conpty.ps1` compares extracted files against
`windows-console/SHA256SUMS`. Matching outputs skip archive hashing and
decompression. Missing or changed files are extracted only after verifying the
pinned archive. When updating the package, update its version, archive hash and
per-architecture output hashes together.

## Embedded fonts

`fonts/` contains unchanged copies of the configured Windows terminal faces:
JetBrainsMono Nerd Font Mono, Maple Mono italics, and Symbols Nerd Font Mono.
They are embedded in Rio so a separate font installation is unnecessary.
[Font versions, hashes and provenance](fonts/README.md) and the original licenses
are retained with the files; the combined license is also installed beside Rio.

## Installation

`misc/windows/install.ps1` prepares the console runtime, builds Rio when needed,
and installs Rio, console files and licenses beside the Cargo executable and in
the chosen install directory. Matching hashes skip copying. Unchanged files may
remain in use; all changed targets are checked for locks before installation
copies begin. A source build still calls Cargo so Cargo can validate its inputs.
This workflow covers `just install*`; release packaging is separate.
