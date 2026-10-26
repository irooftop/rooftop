#!/bin/sh
# Renders background.svg into background.tiff: a 1x + 2x (Retina) image that Finder picks from by
# screen density. Needs rsvg-convert (`brew install librsvg`) and macOS tiffutil.
set -eu
cd "$(dirname "$0")"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
rsvg-convert -w 660 -h 400 background.svg -o "$tmp/background.png"
rsvg-convert -w 1320 -h 800 background.svg -o "$tmp/background@2x.png"
tiffutil -cathidpicheck "$tmp/background.png" "$tmp/background@2x.png" -out background.tiff
