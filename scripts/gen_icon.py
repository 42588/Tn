"""Generate tn.ico — Tn application icon.

Replicates the title-bar brand mark: a rounded square with a 145° gradient
(accent #7AA2F7 → accent_alt #BB9AF7) and the "term" line-icon (>_ prompt) in
chrome_bg (#0E0F19) on top.

Output sizes: 16, 32, 48, 256 px — BMP/DIB entries (traditional ICO format,
guaranteed to work with CreateIconFromResourceEx regardless of Windows version).
"""

import math
import struct
from pathlib import Path
from PIL import Image, ImageDraw


# ── theme colours (Tn Dark) ──
ACCENT = (0x7A, 0xA2, 0xF7)       # #7AA2F7  blue
ACCENT_ALT = (0xBB, 0x9A, 0xF7)   # #BB9AF7  purple
CHROME_BG = (0x0E, 0x0F, 0x19)    # dark silhouette for the prompt glyph
CORNER_RADIUS_RATIO = 7.0 / 21.0   # 7px on a 21×21 block
GRADIENT_ANGLE = 145.0             # clockwise from 3-oʼclock


# ── helpers ──

def draw_brand(r: int) -> Image.Image:
    """Return an RGBA `r×r` image of the rounded gradient square."""
    img = Image.new("RGBA", (r, r), (0, 0, 0, 0))
    # Anti-aliased rounded-rect mask
    mask = Image.new("L", (r * 4, r * 4), 0)
    mask_draw = ImageDraw.Draw(mask)
    rad = int(CORNER_RADIUS_RATIO * r * 4)
    mask_draw.rounded_rectangle([0, 0, r * 4 - 1, r * 4 - 1], radius=rad, fill=255)
    mask = mask.resize((r, r), Image.LANCZOS)

    # Gradient layer (linear, 145°)
    grad = Image.new("RGBA", (r, r))
    angle_rad = math.radians(GRADIENT_ANGLE)
    dx, dy = math.cos(angle_rad), math.sin(angle_rad)
    proj_len = r * abs(dx) + r * abs(dy)

    for y in range(r):
        for x in range(r):
            t = ((x + 0.5) * dx + (y + 0.5) * dy) / max(proj_len, 1.0)
            t = max(0.0, min(1.0, t + 0.5))
            cr = int(ACCENT[0] + (ACCENT_ALT[0] - ACCENT[0]) * t)
            cg = int(ACCENT[1] + (ACCENT_ALT[1] - ACCENT[1]) * t)
            cb = int(ACCENT[2] + (ACCENT_ALT[2] - ACCENT[2]) * t)
            grad.putpixel((x, y), (cr, cg, cb, 255))

    img.paste(grad, (0, 0), mask)
    return img


def draw_term_icon(size: int) -> Image.Image:
    """Return an RGBA `size×size` image of the terminal prompt glyph.

    The SVG (viewBox 0 0 24 24):
      - chevron `<`:  M5 7.5 L 9.5 12 L 5 16.5
      - prompt line:  M12.5 16.5 h6.5
    """
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)

    def sx(x24: float) -> float:
        return x24 / 24.0 * size

    def sy(y24: float) -> float:
        return y24 / 24.0 * size

    sw = max(1.0, 2.2 / 24.0 * size)
    color = CHROME_BG + (255,)

    chev = [(sx(5.0), sy(7.5)), (sx(9.5), sy(12.0)), (sx(5.0), sy(16.5))]
    draw.line(chev, fill=color, width=max(1, round(sw)), joint="curve")

    draw.line(
        [(sx(12.5), sy(16.5)), (sx(19.0), sy(16.5))],
        fill=color, width=max(1, round(sw)),
    )
    return img


# ── ICO packing (BMP / DIB entries) ──

def write_ico(images: dict[int, Image.Image], dest: Path):
    """Write a .ico file with BMP/DIB entries — the traditional format.
    Each entry = BITMAPINFOHEADER (40 bytes) + XOR bitmap (BGRA, bottom-up)
    + AND mask (all zeros — we use 32-bit alpha transparency)."""

    entries = []       # ICONDIRENTRY structs (16 bytes each)
    image_data = []    # raw DIB per entry

    for sz, img in sorted(images.items()):
        w, h = img.size
        pixels = list(img.getdata())  # list of (R, G, B, A)

        # XOR bitmap: BGRA, bottom-up
        xor = bytearray()
        for y in range(h - 1, -1, -1):
            for x in range(w):
                r, g, b, a = pixels[y * w + x]
                xor.extend([b, g, r, a])

        # AND mask: each row is ((w + 31) // 32) * 4 bytes, all zeros
        and_row = ((w + 31) // 32) * 4
        and_mask = bytearray(and_row * h)

        # BITMAPINFOHEADER (biHeight = h * 2 = XOR + AND combined)
        bih = struct.pack(
            "<IiiHHIIiiII",
            40,       # biSize
            w,        # biWidth
            h * 2,    # biHeight (XOR rows + AND rows)
            1,        # biPlanes
            32,       # biBitCount (BGRA)
            0,        # biCompression (BI_RGB)
            0,        # biSizeImage (ok to be 0 for BI_RGB)
            0, 0,     # biXPelsPerMeter / biYPelsPerMeter
            0, 0,     # biClrUsed / biClrImportant
        )
        dib = bytes(bih) + bytes(xor) + bytes(and_mask)
        image_data.append(dib)

        entries.append(struct.pack(
            "<BBBBHHII",
            sz if sz < 256 else 0,   # bWidth  (0 = 256)
            sz if sz < 256 else 0,   # bHeight
            0,                        # bColorCount
            0,                        # bReserved
            1,                        # wPlanes
            32,                       # wBitCount
            len(dib),                 # dwBytesInRes
            0,                        # dwImageOffset (patched below)
        ))

    # ICO header: reserved (2) + type=ICO (2) + count (2)
    header = struct.pack("<HHH", 0, 1, len(entries))

    dir_size = 6 + 16 * len(entries)
    offset = dir_size
    final_entries = b""
    for i, entry in enumerate(entries):
        bw, bh, bc, br, wp, wbc, sz, _ = struct.unpack("<BBBBHHII", entry)
        final_entries += struct.pack("<BBBBHHII", bw, bh, bc, br, wp, wbc, sz, offset)
        offset += len(image_data[i])

    with open(dest, "wb") as f:
        f.write(header)
        f.write(final_entries)
        for dib in image_data:
            f.write(dib)

    total = offset
    print(f"  wrote {dest} — {total} bytes, {len(entries)} BMP entries: {sorted(images.keys())}")


# ── main ──

def main():
    root = Path(__file__).resolve().parent.parent
    dest = root / "crates" / "tn-ui" / "assets" / "tn.ico"
    dest.parent.mkdir(parents=True, exist_ok=True)

    sizes = [16, 32, 48, 256]
    images: dict[int, Image.Image] = {}

    for sz in sizes:
        bg = draw_brand(sz)
        fg = draw_term_icon(sz)
        img = Image.alpha_composite(bg, fg)
        images[sz] = img
        print(f"  rendered {sz}×{sz}")

    write_ico(images, dest)
    print("done →", dest)


if __name__ == "__main__":
    main()
