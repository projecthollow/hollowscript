# HollowScript logo

The master files for the Script Fold mark, icon tile and horizontal lockup.
Use these files unaltered; do not redraw the mark.

These files are **not licensed under CC BY 4.0, Apache-2.0 or MIT**.
Their use is governed by [TRADEMARKS.md](../../TRADEMARKS.md), which does
not claim registration or legal clearance.

## The mark

A solid source-file silhouette, a separated upper-right fold and a
transparent H that opens at the lower edge. The original reference's
proportions and openings are intentional. Do not close the lower H,
equalise its upper openings or replace it with a conventional letterform.

The mark is drawn on a square 1254-unit canvas. It was traced directly
from the selected reference, with subpixel contour simplification. Every
variant uses the same outline. The silhouette was compared against the
reference mask; overlap is 99.9984 percent. Flattening the fill removes
tonal variation without changing the selected design.

## Colours and variants

| File | Use |
| ---- | --- |
| `mark-light.svg` | Site green `#1f7a5c` on a light surface |
| `mark-dark.svg` | Site green `#3fb27f` on a dark surface |
| `mark-mono-black.svg` | The charcoal one-ink version |
| `mark-mono-white.svg` | The white one-ink version |
| `icon-tile.svg` | Green `#3fb27f` mark on a rounded `#1c1c1c` tile |
| `lockup-light.svg`, `lockup-dark.svg` | Mark with the outlined wordmark |
| `social-preview.svg` | Dark lockup on the tile colour, 1280 by 640, for the repository's social preview |

The tile preserves the mark's square canvas and its clear space. The
lockup retains the established Lexend 600 outlined wordmark and the
unregistered trademark symbol. No installed font is needed to render it.
Lexend is licensed under SIL Open Font License 1.1.

The coloured mark is the default across the sites and icons; the wordmark
stays neutral. Use monochrome files when the medium allows only one ink.
The two greens match the language site's accent tokens. Each clears 4.5:1
contrast against its intended background. Do not use the dark-surface green
on a light background.

## Generation

`symbol.svg` is the approved vector outline, and `palette.json` is the
colour source. Run `python3 scripts/render-brand.py` from the repository
root to rebuild all variants, lockups and PNGs with `rsvg-convert`. Run
`python3 scripts/render-brand.py --check` to verify the committed exports.
The renderer reads the vector directly; it does not retrace the reference
image or need the private provenance files.

It also keeps the bundled VS Code client's `images/icon.svg` and
`images/icon.png` aligned with the canonical tile, and writes `preview.svg`
and `preview.png` for a light/dark and small-size review. Store publication
is a separate release action; generating these assets does not publish it.

The language website imports these files with its `npm run brand:import`
command, then runs `npm run brand`. Its source/output fingerprint check
protects the SVG favicon, ICO, installation icons and sharing image from
stale exports. Consumers must import these masters rather than reconstruct
the mark from screenshots.

## Clear space and size

- Preserve the transparent margins in the square mark masters.
- Keep at least one quarter of the visible mark's height free around a
  mark or lockup when placing it beside other artwork.
- Use the mark or tile from 16 pixels. Use the horizontal lockup from
  a 24-pixel mark height; below that, use the mark alone.
- Scale uniformly. Do not stretch, rotate, mirror, rearrange the outline,
  add effects or combine it with another product's symbol.
- Use only the provided flat fills. No gradients, shadows or textures.

The PNG directory contains 512-pixel transparent marks, icon tiles at
16, 32, 48, 128, 256 and 512 pixels and the 1280 by 640 social preview, rendered from the SVG masters with
`rsvg-convert`. Regenerate those exports whenever a master changes.

These masters describe the current identity. Previously released source
archives remain immutable; their artwork reflects their release date.
