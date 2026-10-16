# Windows console runtime

Rio loads `conpty.dll` and its matching `OpenConsole.exe` beside `rio.exe`.
Windows releases include both files. Keep them together when moving a portable
installation.

To build release and install Rio with its runtime to `%USERPROFILE%\.local\bin`:

```powershell
just install
```

Close Rio windows using that installation first. Run the command from another
terminal so Windows can replace the executable and runtime files.
Use `just install-debug` for a fast debug build during local iteration.

The recipes use the native architecture of PowerShell. For another architecture
or destination, invoke `./misc/windows/install.ps1` directly with `-Architecture`
and `-Destination`. These recipes expect Cargo's default target directory and a
native Windows build.

For a local x64 debug build, run this from the repository root in PowerShell:

```powershell
./misc/windows/prepare-conpty.ps1 -Architecture x64 -Destination ./target/debug
```

Use `arm64` for an ARM64 build. To update another installation, close its Rio
windows and set `-Destination` to the directory containing its `rio.exe`.
Windows locks runtime files while they are loaded. Start Rio again to load the
updated runtime.

The script downloads Microsoft's pinned ConPTY NuGet package, verifies its
SHA-256, and copies the matching runtime pair. It also prepares the x64 and
ARM64 release files when run without arguments.

ConPTY 1.25 fixes a cursor synchronization problem after resize. With the older
Windows built-in runtime or ConPTY 1.24, filling the screen, resizing during SSH,
exiting SSH, and resizing up and down can leave PowerShell typing over previous
output. Rio's terminal model and Windows' console cursor then disagree.
Microsoft tracks the underlying problem in
[terminal#18725](https://github.com/microsoft/terminal/issues/18725).

The Windows replay used 64 local lines, a resize, SSH to fish, 64 remote lines,
another resize, SSH exit, and four mouse drags alternating smaller and larger
window sizes before typing. ConPTY 1.24.261001001 placed the typed command on
row 11 while the prompt was on row 23. With ConPTY 1.25.260930003, both remained
on row 23. The same sequence also passed with the clean debug build after the
temporary capture code was removed.
