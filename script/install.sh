#!/usr/bin/env sh
set -eu

# Downloads a release tarball from GitHub Releases
# (https://github.com/shenghsi/flint/releases) and unpacks it into ~/.local/.
# Set ZED_VERSION to a tag (e.g. v0.3.7) to pin a version; it defaults to the
# latest release for the selected channel.
# Set ZED_CHANNEL=preview to install the latest preview build instead of
# stable (preview releases are published to GitHub as prereleases, so
# GitHub's "latest" redirect never returns them and has to be resolved
# separately).

main() {
    platform="$(uname -s)"
    arch="$(uname -m)"
    channel="${ZED_CHANNEL:-stable}"
    ZED_VERSION="${ZED_VERSION:-latest}"
    # Use TMPDIR if available (for environments with non-standard temp directories)
    if [ -n "${TMPDIR:-}" ] && [ -d "${TMPDIR}" ]; then
        temp="$(mktemp -d "$TMPDIR/flint-XXXXXX")"
    else
        temp="$(mktemp -d "/tmp/flint-XXXXXX")"
    fi

    if [ "$platform" = "Darwin" ]; then
        platform="macos"
    elif [ "$platform" = "Linux" ]; then
        platform="linux"
    else
        echo "Unsupported platform $platform"
        exit 1
    fi

    case "$platform-$arch" in
        macos-arm64* | linux-arm64* | linux-armhf | linux-aarch64)
            arch="aarch64"
            ;;
        macos-x86* | linux-x86* | linux-i686*)
            arch="x86_64"
            ;;
        *)
            echo "Unsupported platform or architecture"
            exit 1
            ;;
    esac

    if command -v curl >/dev/null 2>&1; then
        curl () {
            command curl -fL "$@"
        }
    elif command -v wget >/dev/null 2>&1; then
        curl () {
            wget -O- "$@"
        }
    else
        echo "Could not find 'curl' or 'wget' in your path"
        exit 1
    fi

    "$platform" "$@"

    if [ "$(command -v flint)" = "$HOME/.local/bin/flint" ]; then
        echo "Flint has been installed. Run with 'flint'"
    else
        echo "To run Flint from your terminal, you must add ~/.local/bin to your PATH"
        echo "Run:"

        case "$SHELL" in
            *zsh)
                echo "   echo 'export PATH=\$HOME/.local/bin:\$PATH' >> ~/.zshrc"
                echo "   source ~/.zshrc"
                ;;
            *fish)
                echo "   fish_add_path -U $HOME/.local/bin"
                ;;
            *)
                echo "   echo 'export PATH=\$HOME/.local/bin:\$PATH' >> ~/.bashrc"
                echo "   source ~/.bashrc"
                ;;
        esac

        echo "To run Flint now, '~/.local/bin/flint'"
    fi
}

# Builds the GitHub Releases download URL for a given asset filename. Uses the
# "latest" redirect unless ZED_VERSION pins a specific tag (with or without the
# leading "v"), or the preview channel is selected (see latest_preview_tag).
github_release_url() {
    asset="$1"
    if [ "$ZED_VERSION" = "latest" ]; then
        if [ "$channel" = "preview" ]; then
            tag="$(latest_preview_tag)"
            if [ -z "$tag" ]; then
                echo "Could not find a published preview release" >&2
                exit 1
            fi
            echo "https://github.com/shenghsi/flint/releases/download/$tag/$asset"
        else
            echo "https://github.com/shenghsi/flint/releases/latest/download/$asset"
        fi
    else
        case "$ZED_VERSION" in
            v*) tag="$ZED_VERSION" ;;
            *) tag="v$ZED_VERSION" ;;
        esac
        echo "https://github.com/shenghsi/flint/releases/download/$tag/$asset"
    fi
}

# GitHub's "/releases/latest" redirect only ever resolves to the newest
# non-prerelease release, so it can't be used to find the newest preview
# build. Preview releases are tagged "vX.Y.Z-pre" and published with GitHub's
# "prerelease" flag set (see script/create-draft-release), so instead we walk
# the releases list (newest first) and return the tag of the first entry
# marked as a prerelease.
latest_preview_tag() {
    curl "https://api.github.com/repos/shenghsi/flint/releases" | awk '
        /"tag_name":/ {
            tag = $0
            sub(/^[^:]*: *"/, "", tag)
            sub(/",?$/, "", tag)
        }
        /"prerelease":/ {
            if ($0 ~ /true/ && tag != "") {
                print tag
                exit
            }
        }
    '
}

linux() {
    if [ -n "${ZED_BUNDLE_PATH:-}" ]; then
        cp "$ZED_BUNDLE_PATH" "$temp/flint-linux-$arch.tar.gz"
    else
        echo "Downloading Flint version: $ZED_VERSION"
        curl "$(github_release_url "flint-linux-$arch.tar.gz")" > "$temp/flint-linux-$arch.tar.gz"
    fi

    suffix=""
    if [ "$channel" != "stable" ]; then
        suffix="-$channel"
    fi

    appid=""
    case "$channel" in
      stable)
        appid="dev.flint.Flint"
        ;;
      nightly)
        appid="dev.flint.Flint-Nightly"
        ;;
      preview)
        appid="dev.flint.Flint-Preview"
        ;;
      dev)
        appid="dev.flint.Flint-Dev"
        ;;
      *)
        echo "Unknown release channel: ${channel}. Using stable app ID."
        appid="dev.flint.Flint"
        ;;
    esac

    # Unpack
    rm -rf "$HOME/.local/flint$suffix.app"
    mkdir -p "$HOME/.local/flint$suffix.app"
    tar -xzf "$temp/flint-linux-$arch.tar.gz" -C "$HOME/.local/"

    # Setup ~/.local directories
    mkdir -p "$HOME/.local/bin" "$HOME/.local/share/applications"

    # Link the binary
    if [ -f "$HOME/.local/flint$suffix.app/bin/flint" ]; then
        ln -sf "$HOME/.local/flint$suffix.app/bin/flint" "$HOME/.local/bin/flint"
    else
        # support for versions before 0.139.x.
        ln -sf "$HOME/.local/flint$suffix.app/bin/cli" "$HOME/.local/bin/flint"
    fi

    # Copy .desktop file
    desktop_file_path="$HOME/.local/share/applications/${appid}.desktop"
    src_dir="$HOME/.local/flint$suffix.app/share/applications"
    if [ -f "$src_dir/${appid}.desktop" ]; then
        cp "$src_dir/${appid}.desktop" "${desktop_file_path}"
    else
        # Fallback for older tarballs
        cp "$src_dir/flint$suffix.desktop" "${desktop_file_path}"
    fi
    sed -i "s|Icon=flint|Icon=$HOME/.local/flint$suffix.app/share/icons/hicolor/512x512/apps/flint.png|g" "${desktop_file_path}"
    sed -i "s|Exec=flint|Exec=$HOME/.local/flint$suffix.app/bin/flint|g" "${desktop_file_path}"
}

macos() {
    echo "Downloading Flint version: $ZED_VERSION"
    curl "$(github_release_url "Flint-$arch.dmg")" > "$temp/Flint-$arch.dmg"
    hdiutil attach -quiet "$temp/Flint-$arch.dmg" -mountpoint "$temp/mount"
    app="$(cd "$temp/mount/"; echo *.app)"
    echo "Installing $app"
    if [ -d "/Applications/$app" ]; then
        echo "Removing existing $app"
        rm -rf "/Applications/$app"
    fi
    ditto "$temp/mount/$app" "/Applications/$app"
    hdiutil detach -quiet "$temp/mount"

    mkdir -p "$HOME/.local/bin"
    # Link the binary
    ln -sf "/Applications/$app/Contents/MacOS/cli" "$HOME/.local/bin/flint"
}

main "$@"
