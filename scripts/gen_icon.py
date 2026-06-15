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
    """Render the premium vibrant sci-fi icon at `size`×`size` (RGBA), supersampled then downscaled."""
    s = size * SS
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))

    # ── Colors & Design Specs ──
    ph = PHOSPHOR + (255,)
    ph_glow = PHOSPHOR + (45,)    # glow opacity
    ph_border = PHOSPHOR + (200,)  # border opacity
    ph_bg_glow = PHOSPHOR + (60,)  # bezel glow opacity

    # ── 1. Outer Bezel (Chassis) ──
    bezel_radius = int(0.20 * s)
    bezel_mask = Image.new("L", (s, s), 0)
    ImageDraw.Draw(bezel_mask).rounded_rectangle(
        [0, 0, s - 1, s - 1], radius=bezel_radius, fill=255
    )

    # Diagonal bezel fill (rich violet-blue to dark-tech slate)
    bezel_grad = Image.new("RGBA", (s, s))
    bgpx = bezel_grad.load()
    for y in range(s):
        for x in range(s):
            t = (x + y) / (2 * (s - 1))
            r, g, b = _lerp((0x22, 0x26, 0x3F), (0x0E, 0x0F, 0x16), t)
            bgpx[x, y] = (r, g, b, 255)
    img.paste(bezel_grad, (0, 0), bezel_mask)

    # Bezel border glow pass (neon edge)
    bezel_glow = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    ImageDraw.Draw(bezel_glow).rounded_rectangle(
        [SS // 2, SS // 2, s - 1 - SS // 2, s - 1 - SS // 2],
        radius=bezel_radius - SS // 2,
        outline=ph_bg_glow,
        width=int(SS * 1.5),
    )
    img = Image.alpha_composite(img, bezel_glow)

    # Bezel border sharp pass
    bezel_sharp = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    ImageDraw.Draw(bezel_sharp).rounded_rectangle(
        [SS // 2, SS // 2, s - 1 - SS // 2, s - 1 - SS // 2],
        radius=bezel_radius - SS // 2,
        outline=ph_border,
        width=max(1, SS // 2),
    )
    img = Image.alpha_composite(img, bezel_sharp)

    # ── 2. Inner Screen ──
    screen_inset = round(0.06 * s)
    screen_radius = int((0.20 - 0.06) * s)
    screen_box = [screen_inset, screen_inset, s - 1 - screen_inset, s - 1 - screen_inset]

    screen_mask = Image.new("L", (s, s), 0)
    ImageDraw.Draw(screen_mask).rounded_rectangle(
        screen_box, radius=screen_radius, fill=255
    )

    # Deep recessed background gradient (rich dark blue to rich black)
    screen_bg = Image.new("RGBA", (s, s))
    sbpx = screen_bg.load()
    for y in range(s):
        for x in range(s):
            t = (x + y) / (2 * (s - 1))
            r, g, b = _lerp((0x12, 0x16, 0x25), (0x08, 0x0A, 0x10), t)
            sbpx[x, y] = (r, g, b, 255)
    img.paste(screen_bg, (0, 0), screen_mask)

    # Screen inner subtle border
    screen_border = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    ImageDraw.Draw(screen_border).rounded_rectangle(
        [screen_inset + SS // 2, screen_inset + SS // 2, s - 1 - screen_inset - SS // 2, s - 1 - screen_inset - SS // 2],
        radius=screen_radius - SS // 2,
        outline=(0x1B, 0x23, 0x34, 180),
        width=max(1, SS // 2),
    )
    img = Image.alpha_composite(img, screen_border)

    # ── 3. Glowing Viewfinder Corner Brackets ──
    mark = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    md = ImageDraw.Draw(mark)

    w = max(1, round(0.035 * s))       # sleeker bracket stroke
    inset = round(0.180 * s)           # bracket inset from outer edge
    arm = round(0.120 * s)             # arm length
    cap = w / 2.0
    glow_cap = (w * 2.2) / 2.0

    corners = [
        (inset, inset, 1, 1),
        (s - inset, inset, -1, 1),
        (inset, s - inset, 1, -1),
        (s - inset, s - inset, -1, -1),
    ]

    # Glow pass for brackets
    for cx, cy, sx, sy in corners:
        md.line([(cx, cy), (cx + sx * arm, cy)], fill=ph_glow, width=int(w * 2.2))
        md.line([(cx, cy), (cx, cy + sy * arm)], fill=ph_glow, width=int(w * 2.2))
        # Round the bracket ends for glow
        for ex, ey in ((cx, cy), (cx + sx * arm, cy), (cx, cy + sy * arm)):
            md.ellipse([ex - glow_cap, ey - glow_cap, ex + glow_cap, ey + glow_cap], fill=ph_glow)

    # Sharp pass for brackets
    for cx, cy, sx, sy in corners:
        md.line([(cx, cy), (cx + sx * arm, cy)], fill=ph, width=w)
        md.line([(cx, cy), (cx, cy + sy * arm)], fill=ph, width=w)
        # Round the bracket ends for sharp
        for ex, ey in ((cx, cy), (cx + sx * arm, cy), (cx, cy + sy * arm)):
            md.ellipse([ex - cap, ey - cap, ex + cap, ey + cap], fill=ph)

    # ── 4. Glowing Block Cursor ──
    cx = s / 2
    cy = s / 2
    cur_left = round(0.44 * s)
    cur_right = round(0.56 * s)
    cur_top = round(0.42 * s)
    cur_bot = round(0.58 * s)
    cur_radius = max(1, round(0.015 * s))

    # Glow pass for cursor
    glow_expand = round(0.02 * s)
    md.rounded_rectangle(
        [cur_left - glow_expand, cur_top - glow_expand, cur_right + glow_expand, cur_bot + glow_expand],
        radius=cur_radius + glow_expand,
        fill=ph_glow
    )

    # Sharp pass for cursor
    md.rounded_rectangle(
        [cur_left, cur_top, cur_right, cur_bot],
        radius=cur_radius,
        fill=ph
    )

    img = Image.alpha_composite(img, mark)
    return img.resize((size, size), Image.LANCZOS)


# ── ICO packing (BMP / DIB entries — the traditional, maximally compatible form) ──

def write_ico(images: dict[int, Image.Image], dest: Path):
    entries, image_data = [], []
    for sz, img in sorted(images.items()):
        w, h = img.size
        px = img.load()
        xor = bytearray()
        for y in range(h - 1, -1, -1):
            for x in range(w):
                r, g, b, a = px[x, y]
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
