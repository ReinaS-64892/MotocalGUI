#!/bin/bash
set -e

cd "$(dirname "$0")/flatpak"

echo "Building flatpak..."
flatpak-builder --repo=repo --force-clean build io.github.ReinaS_64892.MotocalGUI.yml

echo "Creating bundle..."
flatpak build-bundle repo motocalgui.flatpak io.github.ReinaS_64892.MotocalGUI \
    --runtime-repo=https://flathub.org/repo/flathub.flatpakrepo

mv motocalgui.flatpak ..
echo "Done: motocalgui.flatpak"
