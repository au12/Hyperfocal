"""Generates every Hyperfocal icon size from assets/icon-source.png.

    python3 assets/gen-icon.py

icon-source.png is the 1024px master (cleaned: denoised, black point set so
the background is true #000). Everything else is derived from it:

  assets/icon-512.png              README + Unraid template (rounded corners)
  web/public/icon-192.png          PWA "any" icons (rounded corners)
  web/public/icon-512.png
  web/public/icon-maskable-512.png PWA maskable (full bleed, lens inside safe zone)
  web/public/apple-touch-icon.png  iOS (full bleed square; iOS rounds it)
  web/public/favicon.png           browser tab (tight crop, rounded)
  web/public/logo.png              in-app header / login logo (tight crop, rounded)
"""
import os
from PIL import Image, ImageDraw

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
PUB = os.path.join(ROOT, 'web', 'public')
RADIUS = 0.225  # corner radius as a fraction of size


def crop(img, size):
    """Centered square crop of `size` px from the 1024 master."""
    m = (img.width - size) // 2
    return img.crop((m, m, m + size, m + size))


def rounded(img, out):
    ss = 4  # supersample the mask for clean edges
    mask = Image.new('L', (out * ss, out * ss), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, out * ss - 1, out * ss - 1), radius=out * ss * RADIUS, fill=255)
    rgba = img.resize((out, out), Image.LANCZOS).convert('RGBA')
    rgba.putalpha(mask.resize((out, out), Image.LANCZOS))
    return rgba


def square(img, out):
    return img.resize((out, out), Image.LANCZOS)


def save(im, *parts):
    path = os.path.join(*parts)
    im.save(path, optimize=True)
    print(os.path.relpath(path, ROOT), im.size)


if __name__ == '__main__':
    src = Image.open(os.path.join(HERE, 'icon-source.png')).convert('RGB')
    main = crop(src, 832)   # lens ~70% of height, beams still reach the edges
    tight = crop(src, 640)  # lens fills the frame for tiny sizes
    save(rounded(main, 512), HERE, 'icon-512.png')
    save(rounded(main, 512), PUB, 'icon-512.png')
    save(rounded(main, 192), PUB, 'icon-192.png')
    save(square(src, 512), PUB, 'icon-maskable-512.png')  # full frame keeps the lens in the 80% safe circle
    save(square(main, 180), PUB, 'apple-touch-icon.png')
    save(rounded(tight, 64), PUB, 'favicon.png')
    save(rounded(tight, 256), PUB, 'logo.png')
