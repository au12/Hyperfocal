#!/usr/bin/env python3
"""Regenerates the small HEIC/DNG test fixtures in this folder.

Needs heif-enc (libheif-examples + libheif-plugin-x265), exiftool and ImageMagick.
    python3 tests/fixtures/make_fixtures.py
"""
import os, struct, subprocess, tempfile, zlib

HERE = os.path.dirname(os.path.abspath(__file__))


def png(path, w, h, rows, bits=8):
    """Writes an RGB PNG; rows yields lists of (r, g, b) per row."""
    raw = bytearray()
    for row in rows:
        raw.append(0)
        for px in row:
            raw += struct.pack(">3H", *px) if bits == 16 else bytes(px)
    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    data = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, bits, 2, 0, 0, 0))
    data += chunk(b"IDAT", zlib.compress(bytes(raw), 9)) + chunk(b"IEND", b"")
    open(path, "wb").write(data)


def run(*args):
    subprocess.run(args, check=True, stdout=subprocess.DEVNULL)


def s15(v):
    return struct.pack(">i", round(v * 65536))


def display_p3_icc():
    xyz = lambda x, y, z: b"XYZ " + b"\0" * 4 + s15(x) + s15(y) + s15(z)
    para = b"para" + b"\0" * 4 + struct.pack(">H", 3) + b"\0\0" + b"".join(
        s15(v) for v in (2.4, 1 / 1.055, 0.055 / 1.055, 1 / 12.92, 0.04045))
    text = b"Display P3"
    desc = b"desc" + b"\0" * 4 + struct.pack(">I", len(text) + 1) + text + b"\0" + b"\0" * 79
    wtpt = xyz(0.9642, 1.0, 0.8249)
    tags = [(b"desc", desc), (b"wtpt", wtpt), (b"rXYZ", xyz(0.5151, 0.2412, -0.0011)),
            (b"gXYZ", xyz(0.2920, 0.6922, 0.0419)), (b"bXYZ", xyz(0.1571, 0.0666, 0.7841)),
            (b"rTRC", para), (b"gTRC", para), (b"bTRC", para)]
    base = 132 + 12 * len(tags)
    table, data = b"", b""
    for sig, d in tags:
        table += sig + struct.pack(">II", base + len(data), len(d))
        data += d + b"\0" * (-len(d) % 4)
    header = bytearray(128)
    header[8:12] = b"\x02\x10\0\0"
    header[12:16] = b"mntr"
    header[16:20] = b"RGB "
    header[20:24] = b"XYZ "
    header[36:40] = b"acsp"
    header[68:80] = s15(0.9642) + s15(1.0) + s15(0.8249)
    icc = bytes(header) + struct.pack(">I", len(tags)) + table + data
    return struct.pack(">I", len(icc)) + icc[4:]


def tiny_dng(path):
    """64x48 RGGB Bayer DNG, 16-bit uncompressed. Black 256, white 4095, as-shot white
    balance R x2, B x1.5, EXIF orientation 6. Left half a neutral grey at 25% of the
    range; right half four times brighter, so green sits at the white level (clipped)
    while red and blue do not; the top-right 8x8 corner is clipped in every channel."""
    w, h, black, white = 64, 48, 256, 4095
    rng = white - black
    gain = {0: 0.5, 1: 1.0, 2: 1 / 1.5}  # raw response of a neutral, per channel (R, G, B)
    data = bytearray()
    for y in range(h):
        for x in range(w):
            c = (0, 1)[x % 2] if y % 2 == 0 else (1, 2)[x % 2]
            level = 0.25 if x < w // 2 else 1.0
            v = black + rng * level * gain[c]
            if x >= w - 8 and y < 8:
                v = white
            data += struct.pack("<H", min(white, round(v)))

    def rat(n, d=1):
        return struct.pack("<II", n, d)

    def srat(v):
        return struct.pack("<ii", round(v * 10000), 10000)

    entries = []  # (tag, type, count, payload bytes)
    def add(tag, typ, count, payload):
        entries.append((tag, typ, count, payload))
    asc = lambda s: s.encode() + b"\0"
    SHORT, LONG, ASCII, RATIONAL, SRATIONAL, BYTE = 3, 4, 2, 5, 10, 1
    xyz_to_srgb = [3.2406, -1.5372, -0.4986, -0.9689, 1.8758, 0.0415, 0.0557, -0.2040, 1.0570]
    add(254, LONG, 1, struct.pack("<I", 0))
    add(256, LONG, 1, struct.pack("<I", w))
    add(257, LONG, 1, struct.pack("<I", h))
    add(258, SHORT, 1, struct.pack("<H", 16))
    add(259, SHORT, 1, struct.pack("<H", 1))
    add(262, SHORT, 1, struct.pack("<H", 32803))
    add(271, ASCII, len(asc("Hyperfocal")), asc("Hyperfocal"))
    add(272, ASCII, len(asc("Test DNG")), asc("Test DNG"))
    add(273, LONG, 1, b"STRP")
    add(274, SHORT, 1, struct.pack("<H", 6))
    add(277, SHORT, 1, struct.pack("<H", 1))
    add(278, LONG, 1, struct.pack("<I", h))
    add(279, LONG, 1, struct.pack("<I", len(data)))
    add(284, SHORT, 1, struct.pack("<H", 1))
    add(306, ASCII, 20, asc("2024:07:04 18:30:05"))
    add(33421, SHORT, 2, struct.pack("<HH", 2, 2))
    add(33422, BYTE, 4, bytes([0, 1, 1, 2]))
    add(34665, LONG, 1, b"EXIF")
    add(50706, BYTE, 4, bytes([1, 4, 0, 0]))
    add(50707, BYTE, 4, bytes([1, 1, 0, 0]))
    add(50708, ASCII, len(asc("Hyperfocal Test DNG")), asc("Hyperfocal Test DNG"))
    add(50714, SHORT, 1, struct.pack("<H", black))
    add(50717, SHORT, 1, struct.pack("<H", white))
    add(50721, SRATIONAL, 9, b"".join(srat(v) for v in xyz_to_srgb))
    add(50728, RATIONAL, 3, rat(1, 2) + rat(1, 1) + rat(2, 3))
    add(50778, SHORT, 1, struct.pack("<H", 21))
    exif = [(33434, RATIONAL, 1, rat(1, 250)), (33437, RATIONAL, 1, rat(28, 10)), (34855, SHORT, 1, struct.pack("<H", 200)),
            (36867, ASCII, 20, asc("2024:07:04 18:30:05")), (37386, RATIONAL, 1, rat(35, 1))]

    def ifd(entries, start):
        """Serialises an IFD at offset `start`; returns (bytes, offsets of placeholders)."""
        n = len(entries)
        body = struct.pack("<H", n)
        extra = b""
        extra_at = start + 2 + n * 12 + 4
        marks = {}
        for tag, typ, count, payload in sorted(entries):
            if payload in (b"STRP", b"EXIF"):
                marks[payload] = start + len(body) + 8
                body += struct.pack("<HHI", tag, typ, count) + b"\0\0\0\0"
            elif len(payload) <= 4:
                body += struct.pack("<HHI", tag, typ, count) + payload.ljust(4, b"\0")
            else:
                body += struct.pack("<HHII", tag, typ, count, extra_at + len(extra))
                extra += payload + b"\0" * (len(payload) % 2)
        return body + struct.pack("<I", 0) + extra, marks

    ifd0, marks = ifd(entries, 8)
    exif_at = 8 + len(ifd0)
    exif_ifd, _ = ifd(exif, exif_at)
    data_at = exif_at + len(exif_ifd)
    out = bytearray(b"II*\0" + struct.pack("<I", 8) + ifd0 + exif_ifd + data)
    struct.pack_into("<I", out, marks[b"STRP"], data_at)
    struct.pack_into("<I", out, marks[b"EXIF"], exif_at)
    open(path, "wb").write(out)


def main():
    tmp = tempfile.mkdtemp()
    tiny_dng(os.path.join(HERE, "tiny.dng"))
    # 1. Stored 96x64 (left half red, right half blue), shown rotated 90° clockwise: an
    #    iPhone-style file with a container rotation *and* EXIF orientation 6, EXIF, GPS, XMP.
    src = os.path.join(tmp, "rot.png")
    png(src, 96, 64, ([(230, 20, 20)] * 48 + [(20, 20, 230)] * 48 for _ in range(64)))
    jpg = os.path.join(tmp, "rot.jpg")
    run("convert", src, "-quality", "95", jpg)
    run("exiftool", "-q", "-overwrite_original", "-Make=Apple", "-Model=iPhone 15 Pro",
        "-LensModel=iPhone 15 Pro back camera 6.765mm f/1.78", "-FNumber=1.78", "-ExposureTime=1/120",
        "-ISO=80", "-FocalLength=6.765", "-DateTimeOriginal=2024:07:04 18:30:05", "-OffsetTimeOriginal=+01:00",
        "-GPSLatitude=38.7223", "-GPSLatitudeRef=N", "-GPSLongitude=9.1393", "-GPSLongitudeRef=W",
        "-Orientation=6", "-n", "-XMP-xmp:Rating=4", "-XMP-dc:Subject=fixture", jpg)
    run("heif-enc", "-q", "90", jpg, "-o", os.path.join(HERE, "rotated.heic"))

    # 2. Display P3 ICC profile, one flat colour (P3 values 200, 100, 50).
    src = os.path.join(tmp, "p3.png")
    png(src, 32, 32, ([(200, 100, 50)] * 32 for _ in range(32)))
    icc = os.path.join(tmp, "p3.icc")
    open(icc, "wb").write(display_p3_icc())
    jpg = os.path.join(tmp, "p3.jpg")
    run("convert", src, "-quality", "100", "-profile", icc, jpg)
    run("heif-enc", "-L", "-p", "chroma=444", jpg, "-o", os.path.join(HERE, "display-p3.heic"))

    # 3. 10-bit BT.2020 PQ (HDR): top half at the 203-nit reference white, bottom 1000 nits.
    def pq(nits):
        m1, m2, c1, c2, c3 = 2610 / 16384, 2523 / 4096 * 128, 3424 / 4096, 2413 / 4096 * 32, 2392 / 4096 * 32
        y = (nits / 10000) ** m1
        return round(((c1 + c2 * y) / (1 + c3 * y)) ** m2 * 65535)
    src = os.path.join(tmp, "pq.png")
    png(src, 32, 32, ([(pq(203),) * 3] * 32 if r < 16 else [(pq(1000),) * 3] * 32 for r in range(32)), bits=16)
    run("heif-enc", "-L", "-p", "chroma=444", "-b", "10", "--matrix_coefficients", "9", "--colour_primaries", "9",
        "--transfer_characteristic", "16", "--full_range_flag", "1", src, "-o", os.path.join(HERE, "pq-hdr.heic"))
    for f in ("rotated.heic", "display-p3.heic", "pq-hdr.heic", "tiny.dng"):
        print(f, os.path.getsize(os.path.join(HERE, f)), "bytes")


if __name__ == "__main__":
    main()
