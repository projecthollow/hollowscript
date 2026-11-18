#!/usr/bin/env python3
"""Render the brand palette and approved vector without retracing its outline.

Copyright (c) 2026 Soulstack. Requires rsvg-convert on PATH.
Run with --check to verify committed files instead of writing them.
"""

import argparse
import json
from pathlib import Path
import subprocess
import xml.etree.ElementTree as ET

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--check", action="store_true")
args = parser.parse_args()
root = Path(__file__).resolve().parents[1] / "docs/brand"
palette = json.loads((root / "palette.json").read_text())
ns = "{http://www.w3.org/2000/svg}"
source = ET.fromstring((root / "symbol.svg").read_text())
view_box = source.attrib["viewBox"]
width = float(view_box.split()[2])
shape = source.find(f"{ns}path").attrib["d"]
outputs = {}


def mark(colour):
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{view_box}" role="img">\n'
        '  <title>HollowScript</title>\n'
        f'  <path fill="{colour}" fill-rule="evenodd" d="{shape}"/>\n'
        '</svg>\n'
    ).encode()


def render(svg, size, height=None):
    return subprocess.run(
        ["rsvg-convert", "-w", str(size), "-h", str(height or size)],
        input=svg, capture_output=True, check=True,
    ).stdout


for name, colour in (("light", palette["light"]), ("dark", palette["dark"]),
                     ("mono-black", palette["ink"]), ("mono-white", palette["reverse"])):
    artwork = mark(colour)
    outputs[f"mark-{name}.svg"] = artwork
    outputs[f"png/mark-{name}-512.png"] = render(artwork, 512)

tile = (
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 480 480" role="img">\n'
    '  <title>HollowScript</title>\n'
    f'  <rect width="480" height="480" rx="105.6" fill="{palette["tile"]}"/>\n'
    f'  <path transform="scale({480 / width})" fill="{palette["dark"]}" fill-rule="evenodd" d="{shape}"/>\n'
    '</svg>\n'
).encode()
outputs["icon-tile.svg"] = tile
for size in (16, 32, 48, 128, 256, 512):
    outputs[f"png/icon-tile-{size}.png"] = render(tile, size)
outputs["../../editors/vscode/images/icon.svg"] = tile
outputs["../../editors/vscode/images/icon.png"] = outputs["png/icon-tile-128.png"]

preview = []
for offset, background, foreground in ((0, palette["reverse"], palette["light"]),
                                        (512, palette["tile"], palette["dark"])):
    preview.append(f'<rect x="{offset}" width="512" height="512" fill="{background}"/>')
    preview.append(f'<path transform="translate({offset + 64} 0) scale({384 / width})" fill="{foreground}" fill-rule="evenodd" d="{shape}"/>')
    for left, size in ((144, 16), (208, 24), (280, 32), (360, 48)):
        preview.append(f'<path transform="translate({offset + left} 428) scale({size / width})" fill="{foreground}" fill-rule="evenodd" d="{shape}"/>')
outputs["preview.svg"] = (
    '<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="512" viewBox="0 0 1024 512">\n'
    + '\n'.join(preview) + '\n</svg>\n'
).encode()
outputs["preview.png"] = render(outputs["preview.svg"], 1024, 512)

for theme in ("light", "dark"):
    # The two outlined wordmark paths remain neutral and unchanged.
    current = ET.fromstring((root / f"lockup-{theme}.svg").read_text())
    words = current.findall(f"{ns}path")[-2:]
    assert len(words) == 2
    ink = palette["ink"] if theme == "light" else palette["reverse"]
    word_paths = '\n'.join(f'  <path fill="{ink}" d="{word.attrib["d"]}"/>' for word in words)
    outputs[f"lockup-{theme}.svg"] = (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 289 48" role="img">\n'
        '  <title>HollowScript</title>\n'
        f'  <path transform="scale({48 / width})" fill="{palette[theme]}" fill-rule="evenodd" d="{shape}"/>\n'
        + word_paths + '\n</svg>\n'
    ).encode()

# The repository social preview: the dark lockup centred on the tile colour,
# at GitHub's recommended 1280 by 640 pixels.
lockup = ET.fromstring(outputs["lockup-dark.svg"].decode())
lockup_paths = '\n'.join(
    '    <path ' + ' '.join(f'{key}="{value}"' for key, value in path.attrib.items()) + '/>'
    for path in lockup.findall(f"{ns}path"))
social_width = 820
social_height = social_width * 48 / 289
outputs["social-preview.svg"] = (
    '<svg xmlns="http://www.w3.org/2000/svg" width="1280" height="640" viewBox="0 0 1280 640" role="img">\n'
    '  <title>HollowScript</title>\n'
    f'  <rect width="1280" height="640" fill="{palette["tile"]}"/>\n'
    f'  <svg x="{(1280 - social_width) / 2}" y="{(640 - social_height) / 2}" width="{social_width}" '
    f'height="{social_height}" viewBox="0 0 289 48">\n'
    + lockup_paths + '\n  </svg>\n</svg>\n'
).encode()
outputs["png/social-preview-1280x640.png"] = render(outputs["social-preview.svg"], 1280, 640)

stale = []
for name, data in outputs.items():
    destination = root / name
    if args.check:
        if not destination.exists() or destination.read_bytes() != data:
            stale.append(name)
    else:
        destination.write_bytes(data)
if stale:
    raise SystemExit("Stale brand exports; run python3 scripts/render-brand.py: " + ", ".join(stale))
print(f"Brand {'checked' if args.check else 'rendered'}: {len(outputs)} files; approved outline unchanged.")
