"""Generates every Hyperfocal icon size from assets/icon-source.png.

    python3 assets/gen-icon.py

icon-source.png is the 1024px artwork exactly as generated: the lens and beams
on a dark tile, with the tile's own rounded edge and the black around it. The
icons never show that tile edge. Everything is cut from inside the tile
(TILE_CROP) and given its own rounded corners. Nothing else is done to the
artwork: no pixels are painted, filled or extended.

  assets/icon-512.png              README + Unraid template (rounded corners)
  web/public/icon-192.png          PWA "any" icons (rounded corners)
  web/public/icon-512.png
  web/public/icon-maskable-512.png PWA maskable (rounded icon on black, lens inside the 80% safe circle)
  web/public/apple-touch-icon.png  iOS (rounded icon on black; iOS rounds it again)
  web/public/favicon.png           browser tab (rounded)
  web/public/logo.png              in-app header / login logo (rounded)
"""
import os
from PIL import Image, ImageDraw

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
PUB = os.path.join(ROOT, 'web', 'public')
RADIUS = 0.225  # corner radius as a fraction of size

# Square inside the tile of icon-source.png, clear of its bevelled edge
# (tile ~x 110..913, y 102..909; bevel ~12 px), centred on the lens
# (x ~337..690, y ~166..840). The rounded corners cut away the tile's own
# corners, which would otherwise show inside this square.
TILE_CROP = (136, 126, 890, 880)
LENS_HALF_H = 345 / 754  # half the lens height (with its glow), as a fraction of the crop


def rounded_mask(out):
    ss = 4  # supersample the mask for clean edges
    mask = Image.new('L', (out * ss, out * ss), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, out * ss - 1, out * ss - 1), radius=out * ss * RADIUS, fill=255)
    return mask.resize((out, out), Image.LANCZOS)


def rounded(img, out):
    rgba = img.resize((out, out), Image.LANCZOS).convert('RGBA')
    rgba.putalpha(rounded_mask(out))
    return rgba


def on_black(img, out, scale=1.0):
    """The rounded icon on plain black, for icons that must be opaque.

    Nothing is added to the artwork: the corners the rounding cuts away (and,
    for the maskable icon, the margin around it) are just black. iOS rounds
    apple-touch-icon itself; Android crops the maskable icon to its own shape,
    and the lens sits inside the 80% safe circle.
    """
    inner = int(out * scale) // 2 * 2
    bg = Image.new('RGB', (out, out), (0, 0, 0))
    off = (out - inner) // 2
    bg.paste(rounded(img, inner), (off, off), rounded_mask(inner))
    return bg


def save(im, *parts):
    path = os.path.join(*parts)
    im.save(path, optimize=True)
    print(os.path.relpath(path, ROOT), im.size)


if __name__ == '__main__':
    src = Image.open(os.path.join(HERE, 'icon-source.png')).convert('RGB')
    tile = src.crop(TILE_CROP)
    save(rounded(tile, 512), HERE, 'icon-512.png')
    save(rounded(tile, 512), PUB, 'icon-512.png')
    save(rounded(tile, 192), PUB, 'icon-192.png')
    save(on_black(tile, 512, 0.40 / LENS_HALF_H), PUB, 'icon-maskable-512.png')
    save(on_black(tile, 180), PUB, 'apple-touch-icon.png')
    save(rounded(tile, 64), PUB, 'favicon.png')
    save(rounded(tile, 256), PUB, 'logo.png')
