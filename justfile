set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command"]

default:
    @just --list

# Build and install Rio with its Windows console runtime.
[windows]
install:
    & ./misc/windows/install.ps1

# Use the fast debug build for local iteration.
[windows]
install-debug:
    & ./misc/windows/install.ps1 -Profile debug
