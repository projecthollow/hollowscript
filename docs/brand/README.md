# HollowScript logo

The master files for the HollowScript mark, its icon tile and its
horizontal lockup. Use these files as they are; do not redraw the mark.

The name HollowScript and this logo are trade marks of the project.
**The files in this directory are not licensed under CC BY 4.0**, unlike
the rest of `docs/`, **nor under Apache-2.0 or MIT**. Their use is
governed by [TRADEMARKS.md](../../TRADEMARKS.md) alone.

## The mark

Seven squares on a three by three grid, with the top and bottom cells of
the middle column left out, so the grid reads as an H. The centre cell is
the only one in the accent colour.

- **The H** is the name.
- **The grid** is the runtime: a fixed, bounded space the program runs in.
- **The single green cell** is the capability the host grants. A
  HollowScript program can do nothing outside itself unless its host hands
  it a capability; the mark shows one granted cell inside an otherwise
  neutral grid.

The mark belongs to the language alone. It shares no shape, asset or
symbol with any other product's branding, and it is not a ghost or a
hologram.

## Construction

The mark is drawn on a 48 by 48 unit square.

- Nine cells of 12 by 12 units, corner radius 1.5 units.
- Cells start at 3, 18 and 33 on each axis: a 3 unit margin on every side
  and a 3 unit gap between cells.
- The cells at (18, 3) and (18, 33) are removed.
- The cell at (18, 18) is the core, filled with the accent colour. The
  other six are the foreground colour.

```
        x = 3     x = 18    x = 33
y = 3   [ fg ]    ( none )  [ fg ]
y = 18  [ fg ]    [ core ]  [ fg ]
y = 33  [ fg ]    ( none )  [ fg ]
```

Every coordinate in the master files is a whole or half unit, so the mark
stays sharp at 16, 32 and 48 pixels.

**Icon tile.** A rounded square in `#1c1c1c` with a corner radius of 0.22
of its width, and the mark scaled to 70 percent of the tile, centred, in
the dark surface colours. `icon-tile.svg` is drawn on a 480 unit square
so that the scaled mark keeps whole and half unit coordinates (cells of 84
units at 93, 198 and 303, corner radius 10.5). Use the tile wherever a
platform expects an app icon: editor extensions, touch icons, web app
manifests.

**Lockup.** The mark followed by the wordmark "HollowScript" set in
Lexend at weight 600 with letter spacing of -0.04 em, in the foreground
colour only. At a mark height of 48 units the text is 36 units, the gap
between the mark and the H is 13 units (about 0.27 of the mark's height)
and the cap height is centred on the mark. The unregistered trade mark
symbol follows the name, at 0.6 of the text size in Lexend 400, with its
top level with the cap height, matching how the website shows the name in
its header. Green appears only in the core cell. The letters are outlines,
so the lockup needs no font to render. Lexend is licensed under the SIL
Open Font License 1.1; the outlines here are artwork made with it, not a
copy of the font.

## Colours

| Surface | Foreground | Core     | Background |
| ------- | ---------- | -------- | ---------- |
| Light   | `#1a1b20`  | `#1f7a5c` | `#ffffff`, or the page colour `#eceff5` |
| Dark    | `#f4f4f5`  | `#3fb27f` | `#1c1c1c`  |

These are the website's interface colours. The one-colour versions fill
all seven cells with a single colour: black `#1a1b20` or white `#ffffff`.
Use them where only one ink is available, such as a stamp, an embossed
surface or a single-colour print run.

## Clear space and size

- Keep clear space of at least 0.25 of the mark's height on every side
  (12 units at the 48 unit master), free of text, edges and other marks.
  The lockup takes the same clear space, measured from the mark's height.
- The smallest size for the mark and for the icon tile is 16 pixels. The
  lockup should not be used with a mark shorter than 16 pixels; below
  that, use the mark on its own.

## Do and do not

Do:

- use the light files on light surfaces and the dark files on dark ones;
- use a one-colour version when the medium has only one colour;
- scale the files uniformly.

Do not:

- recolour any cell other than the core, or give the core any colour but
  the accent of its surface;
- rotate, mirror, skew or stretch the mark, or rearrange, add or remove
  cells;
- add shadows, glows, gradients, outlines, bevels or other effects;
- set the wordmark in another typeface, weight or spacing, or colour it
  green;
- combine the mark with the Hollow ghost or any other product's logo, or
  place it inside another badge system.

## Files

| File | What it is |
| ---- | ---------- |
| `mark-light.svg` | The mark for light surfaces |
| `mark-dark.svg` | The mark for dark surfaces |
| `mark-mono-black.svg` | One colour, black |
| `mark-mono-white.svg` | One colour, white |
| `icon-tile.svg` | The app and editor icon tile |
| `lockup-light.svg` | Mark and wordmark for light surfaces |
| `lockup-dark.svg` | Mark and wordmark for dark surfaces |
| `png/icon-tile-16.png` to `png/icon-tile-512.png` | The tile at 16, 32, 48, 128, 256 and 512 pixels |
| `png/mark-light-512.png`, `png/mark-dark-512.png` | The mark at 512 pixels, transparent background |
| `png/mark-mono-black-512.png`, `png/mark-mono-white-512.png` | The one-colour marks at 512 pixels |

The PNG files are rendered from the SVG masters with `rsvg-convert`, for
example `rsvg-convert -w 512 -h 512 icon-tile.svg -o png/icon-tile-512.png`.
When a master changes, render its PNG files again.
