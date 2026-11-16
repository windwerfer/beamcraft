#!/bin/bash
set -e

echo "==> Running post-setup for mirrorwire"

# =====================
# OWNERSHIP (safe to run again)
# =====================
echo "==> Fixing volume permissions..."
mkdir -p "$HOME/.cargo" "$HOME/android-sdk"
sudo chown -R vscode:vscode \
    "$HOME/.cargo" \
    "$HOME/android-sdk" \
    /opt/backups

# Make shell scripts executable.
# Git does not reliably preserve +x after clone (especially cross-platform or via some UIs).
# This runs after the workspace is mounted, so it actually affects what you see.
chmod +x .devcontainer_rs/*.sh 2>/dev/null || true


# VAAPI H.264 encode for the presenter (--encoder vaapi) on the host's Intel GPU.
# Debian's free intel-media-va-driver only has the low-power entrypoint
# (EncSliceLP); the full encoder (EncSlice) is in the non-free package, so
# enable the non-free component first (idempotent). Check with: vainfo | grep -i enc
if ! grep -q "non-free" /etc/apt/sources.list.d/debian.sources 2>/dev/null; then
    echo "==> Enabling Debian non-free (Intel VAAPI encoder driver)..."
    sudo sed -i 's/^Components: main$/Components: main non-free/' /etc/apt/sources.list.d/debian.sources
fi

sudo apt update
sudo apt install -y  libclang-dev libavcodec-dev libavformat-dev libavutil-dev libswscale-dev libavfilter-dev libavdevice-dev libpipewire-0.3-dev libva-dev
sudo apt install -y  intel-media-va-driver-non-free vainfo

echo ""
echo "==> Post-setup complete."
echo "    Next:"
echo "      bash .devcontainer_rs/rust-setup.sh"
echo "      bash .devcontainer_rs/android-setup.sh"
echo "      bash .devcontainer_rs/verify-setup.sh"
echo ""
