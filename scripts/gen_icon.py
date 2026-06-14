"""Generate tn.ico — the Tn application icon (磷光 Phosphor language).

Design (replaces the old Tokyo-Night blue→purple gradient):
  • Chassis: an opaque, rounded-square instrument plate. Depth comes from a
    subtle vertical elevation (L1 #141926 → L0 #0B0E16) plus a 1px cool hairline
    edge — structure, not light pollution. No glow, no flashy gradient.
  • Mark: a viewfinder — four phosphor corner brackets (the 磷光 design system's
    signature "focus = 取景器角标" motif) framing a solid block cursor, all in the
    single life color #5BE7C4. The brackets give Tn an instrument identity; the
    block cursor is the live thing the phosphor color is reserved for.

Output sizes: 16/24/32/48/64/128/256 px, BMP/DIB entries (traditional ICO,
works with LoadImageW and as an embedded Win32 resource on every Windows
version). Run: python scripts/gen_icon.py
"""

import struct
from pathlib import Path
from PIL import Image, ImageDraw

# ── 磷光 palette (design/phosphor.css :root) ──
PHOSPHOR = (0x5B, 0xE7, 0xC4)      # #5BE7C4  the single life color (cursor)
PLATE_TOP = (0x16, 0x1B, 0x29)     # slightly lifted L1 — top of the chassis
PLATE_BOT = (0x0B, 0x0E, 0x16)     # #0B0E16  L0 chassis floor — bottom
HAIRLINE = (0x34, 0x3E, 0x52)      # cool seam edge (h1-ish), opaque-ish
CORNER_RADIUS_RATIO = 0.235        # rounded-square chassis
SS = 4                             # supersampling for anti-aliasing


def _lerp(a, b, t):
    return tuple(round(a[i] + (b[i] - a[i]) * t) for i in range(3))


def render(size: int) -> Image.Image:
    """Render the icon at `size`×`size` (RGBA), supersampled then downscaled."""
    s = size * SS
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))

    # Rounded-rect chassis mask.
    mask = Image.new("L", (s, s), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        [0, 0, s - 1, s - 1], radius=int(CORNER_RADIUS_RATIO * s), fill=255
    )

    # Vertical elevation fill (opaque, dark — depth, not glow).
    grad = Image.new("RGBA", (s, s))
    gpx = grad.load()
    for y in range(s):
        r, g, b = _lerp(PLATE_TOP, PLATE_BOT, y / (s - 1))
        for x in range(s):
            gpx[x, y] = (r, g, b, 255)
    img.paste(grad, (0, 0), mask)

    # 1px cool hairline edge (precision-instrument seam).
    border = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    ImageDraw.Draw(border).rounded_rectangle(
        [SS // 2, SS // 2, s - 1 - SS // 2, s - 1 - SS // 2],
        radius=int(CORNER_RADIUS_RATIO * s) - SS // 2,
        outline=HAIRLINE + (235,),
        width=SS,
    )
    img = Image.alpha_composite(img, border)

    # Phosphor mark: a viewfinder — four corner brackets (the 磷光 focus motif)
    # framing a block cursor (the live element).
    mark = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    md = ImageDraw.Draw(mark)
    ph = PHOSPHOR + (255,)

    w = max(1, round(0.060 * s))      # bracket stroke
    inset, arm = 0.190 * s, 0.160 * s  # corner distance from edge, arm length
    cap = w / 2.0
    corners = [
        (inset, inset, 1, 1),
        (s - inset, inset, -1, 1),
        (inset, s - inset, 1, -1),
        (s - inset, s - inset, -1, -1),
    ]
    for cx, cy, sx, sy in corners:
        md.line([(cx, cy), (cx + sx * arm, cy)], fill=ph, width=w)
        md.line([(cx, cy), (cx, cy + sy * arm)], fill=ph, width=w)
        # round the exposed ends (outer corner + the two arm tips)
        for ex, ey in ((cx, cy), (cx + sx * arm, cy), (cx, cy + sy * arm)):
            md.ellipse([ex - cap, ey - cap, ex + cap, ey + cap], fill=ph)

    # Center block cursor — the single live element being framed.
    md.rounded_rectangle(
        [0.430 * s, 0.410 * s, 0.570 * s, 0.590 * s], radius=0.028 * s, fill=ph
    )

    img = Image.alpha_composite(img, mark)
    return img.resize((size, size), Image.LANCZOS)


# ── ICO packing (BMP / DIB entries — the traditional, maximally compatible form) ──

def write_ico(images: dict[int, Image.Image], dest: Path):
    entries, image_data = [], []
    for sz, img in sorted(images.items()):
        w, h = img.size
        px = list(img.getdata())
        xor = bytearray()
        for y in range(h - 1, -1, -1):
            for x in range(w):
                r, g, b, a = px[y * w + x]
                xor.extend([b, g, r, a])
        and_mask = bytearray(((w + 31) // 32) * 4 * h)
        bih = struct.pack(
            "<IiiHHIIiiII", 40, w, h * 2, 1, 32, 0, 0, 0, 0, 0, 0
        )
        dib = bytes(bih) + bytes(xor) + bytes(and_mask)
        image_data.append(dib)
        entries.append(struct.pack(
            "<BBBBHHII",
            sz if sz < 256 else 0, sz if sz < 256 else 0, 0, 0, 1, 32, len(dib), 0,
        ))

    header = struct.pack("<HHH", 0, 1, len(entries))
    offset = 6 + 16 * len(entries)
    final = b""
    for i, entry in enumerate(entries):
        bw, bh, bc, br, wp, wbc, sz, _ = struct.unpack("<BBBBHHII", entry)
        final += struct.pack("<BBBBHHII", bw, bh, bc, br, wp, wbc, sz, offset)
        offset += len(image_data[i])

    with open(dest, "wb") as f:
        f.write(header)
        f.write(final)
        for dib in image_data:
            f.write(dib)
    print(f"  wrote {dest} — {offset} bytes, sizes {sorted(images)}")


def main():
    root = Path(__file__).resolve().parent.parent
    dest = root / "crates" / "tn-ui" / "assets" / "tn.ico"
    dest.parent.mkdir(parents=True, exist_ok=True)

    sizes = [16, 24, 32, 48, 64, 128, 256]
    images = {sz: render(sz) for sz in sizes}
    for sz in sizes:
        print(f"  rendered {sz}×{sz}")
    write_ico(images, dest)

    # Preview: the 256 icon + the actual 16px pixels (nearest-scaled ×8) so small-
    # size legibility can be eyeballed. Written to a temp PNG, not shipped.
    prev = Image.new("RGBA", (256 + 16 * 8 + 24, 256), (24, 28, 38, 255))
    prev.alpha_composite(images[256], (0, 0))
    prev.alpha_composite(images[16].resize((128, 128), Image.NEAREST), (256 + 24, 64))
    import tempfile
    preview_path = Path(tempfile.gettempdir()) / "tn_icon_preview.png"
    prev.convert("RGB").save(preview_path)
    print("  preview →", preview_path)
    print("done →", dest)


if __name__ == "__main__":
    main()
