#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
profile=${1:-release}
if [ "$#" -gt 0 ]; then shift; fi
prefix=${RIO_INSTALL_PREFIX:-"$HOME/.local"}
binary=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --prefix|--binary)
            if [ "$#" -lt 2 ]; then echo "Missing value for $1" >&2; exit 2; fi
            case "$1" in
                --prefix) prefix=$2 ;;
                --binary) binary=$2 ;;
            esac
            shift 2 ;;
        *) echo "Unknown install option: $1" >&2; exit 2 ;;
    esac
done
case "$profile" in
    release|debug) ;;
    *) echo "Unknown build profile: $profile" >&2; exit 2 ;;
esac
os=$(uname -s)
case "$os" in
    Linux|Darwin) ;;
    *) echo "Unsupported install platform: $os" >&2; exit 2 ;;
esac

mkdir -p "$prefix/bin"
prefix=$(CDPATH= cd -- "$prefix" && pwd)
if [ -n "$binary" ]; then
    # Reuse a finished build without invoking Cargo.
    install -m 755 "$binary" "$prefix/bin/rio"
else
    cd "$root"
    # Keep Cargo's normal artifact cache for repeated local installs.
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/target}"
    if [ "$profile" = debug ]; then
        export CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=1
        cargo install --path frontends/rioterm --bin rio --locked --force --root "$prefix" --debug
    else
        cargo install --path frontends/rioterm --bin rio --locked --force --root "$prefix"
    fi
fi

if command -v tic >/dev/null 2>&1; then
    tic -x -e xterm-rio,rio -o "$HOME/.terminfo" "$root/misc/rio.terminfo"
fi
if [ "$os" = Darwin ]; then
    bundle="${RIO_APPLICATIONS_DIR:-$HOME/Applications}/Rio.app"
    mkdir -p "$bundle/Contents/MacOS"
    cp -R "$root/misc/osx/Rio.app/." "$bundle/"
    install -m 755 "$prefix/bin/rio" "$bundle/Contents/MacOS/rio"
    version=$(sed -n 's/^version = "\([^"]*\)".*/\1/p' "$root/Cargo.toml" | head -n 1)
    stamp=$(date +%Y%m%d%H%M%S)
    sed -e "s/{{\.Version}}/$version/g" \
        -e "s/{{\.Now\.Format \"20060102150405\"}}/$stamp/g" \
        "$root/misc/osx/Rio.app/Contents/Info.plist" > "$bundle/Contents/Info.plist"
    if command -v tic >/dev/null 2>&1; then
        tic -x -e xterm-rio,rio -o "$bundle/Contents/Resources" "$root/misc/rio.terminfo"
    fi
    codesign --force --deep --sign - "$bundle"
    echo "Installed Rio ($profile) to $bundle and $prefix/bin/rio"
else
    data=${XDG_DATA_HOME:-$prefix/share}
    mkdir -p "$data/applications" "$data/icons/hicolor/scalable/apps"
    install -m 644 "$root/misc/rio.desktop" "$data/applications/rio.desktop"
    install -m 644 "$root/misc/logo.svg" "$data/icons/hicolor/scalable/apps/rio.svg"
    echo "Installed Rio ($profile) to $prefix/bin/rio"
fi
