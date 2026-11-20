"""Generates the icon of the companion container, hyperfocal-ml:

    python3 assets/gen-ml-icon.py

The Hyperfocal icon (assets/icon-512.png) with a face-scan badge in the corner, so the
two containers sit together in the Unraid Docker list.
"""
import os
from PIL import Image, ImageDraw

HERE = os.path.dirname(os.path.abspath(__file__))
SS = 4  # supersampling


def badge(d: int) -> Image.Image:
    """A blue disc with the face-scan glyph (24-unit stroke icon geometry), d px."""
    D = d * SS
    im = Image.new('RGBA', (D, D), (0, 0, 0, 0))
    dr = ImageDraw.Draw(im)
    # Ring of the icon's black, then the disc.
    dr.ellipse((0, 0, D - 1, D - 1), fill=(0, 0, 0, 255))
    pad = int(D * 0.06)
    top, bottom = (77, 177, 255), (0, 100, 230)
    disc = Image.new('RGBA', (D, D), (0, 0, 0, 0))
    grad = Image.new('RGBA', (1, D))
    for y in range(D):
        t = y / (D - 1)
        grad.putpixel((0, y), tuple(int(top[i] + (bottom[i] - top[i]) * t) for i in range(3)) + (255,))
    grad = grad.resize((D, D))
    mask = Image.new('L', (D, D), 0)
    ImageDraw.Draw(mask).ellipse((pad, pad, D - 1 - pad, D - 1 - pad), fill=255)
    disc.paste(grad, (0, 0), mask)
    im = Image.alpha_composite(im, disc)
    dr = ImageDraw.Draw(im)
    # Glyph: 24 units mapped onto 56% of the disc.
    g = D * 0.56
    o = (D - g) / 2
    u = g / 24

    def p(x, y):
        return (o + x * u, o + y * u)

    w = max(1, int(2.1 * u))
    white = (255, 255, 255, 255)
    r = 2 * u
    for (cx, cy, a0) in ((5, 5, 180), (19, 5, 270), (19, 19, 0), (5, 19, 90)):
        # Rounded corner: a quarter arc and its two short legs.
        x, y = p(cx, cy)
        dr.arc((x - r, y - r, x + r, y + r), a0, a0 + 90, fill=white, width=w)
    for a, b in (((3, 5), (3, 7)), ((5, 3), (7, 3)), ((17, 3), (19, 3)), ((21, 5), (21, 7)),
                 ((21, 17), (21, 19)), ((17, 21), (19, 21)), ((3, 17), (3, 19)), ((5, 21), (7, 21))):
        dr.line((p(*a), p(*b)), fill=white, width=w)
    for (x, y) in ((3, 7), (7, 3), (17, 3), (21, 7), (21, 17), (17, 21), (7, 21), (3, 17)):
        cx, cy = p(x, y)
        dr.ellipse((cx - w / 2, cy - w / 2, cx + w / 2, cy + w / 2), fill=white)
    # Smile and eyes.
    x0, y0 = p(8, 11)
    x1, y1 = p(16, 16)
    dr.arc((x0, y0, x1, y1), 15, 165, fill=white, width=w)
    for ex in (9, 15):
        cx, cy = p(ex, 9)
        er = 1.35 * u
        dr.ellipse((cx - er, cy - er, cx + er, cy + er), fill=white)
    return im.resize((d, d), Image.LANCZOS)


if __name__ == '__main__':
    base = Image.open(os.path.join(HERE, 'icon-512.png')).convert('RGBA')
    b = badge(220)
    out = base.copy()
    out.alpha_composite(b, (512 - 220 - 18, 512 - 220 - 18))
    path = os.path.join(HERE, 'icon-ml-512.png')
    out.save(path, optimize=True)
    print(os.path.relpath(path, os.path.dirname(HERE)), out.size)
