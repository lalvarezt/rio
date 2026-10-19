set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command"]

default:
    @just --list

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
