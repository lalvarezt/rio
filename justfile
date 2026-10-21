set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command"]

default:
    @just --list

# Compare committed upstream/river-raptor tips with freshly fetched Ghostty HEAD.
# Runs are retained in bench-results; use --help for sampling and filtering.
[unix]
bench *args:
    python3 misc/ghostty-bench/compare.py {{ args }}

# Build and install Rio, including the platform's runtime and desktop files.
# Reuse a build with --binary PATH on Unix or -Binary PATH on Windows.
[windows]
install *args:
    & ./misc/windows/install.ps1 {{ args }}

[unix]
install *args:
    sh ./misc/install.sh release {{ args }}

# Use the fast debug build for local iteration.
[windows]
install-debug *args:
    & ./misc/windows/install.ps1 -Profile debug {{ args }}

[unix]
install-debug *args:
    sh ./misc/install.sh debug {{ args }}
