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
    """Render the premium vibrant sci-fi icon with Tn Monogram at `size`×`size` (RGBA)."""
    s = size * SS
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    scale = s / 256.0

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

    # Diagonal bezel fill
    bezel_grad = Image.new("RGBA", (s, s))
    bgpx = bezel_grad.load()
    for y in range(s):
        for x in range(s):
            t = (x + y) / (2.0 * (s - 1)) if s > 1 else 0
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

    # Deep recessed background gradient
    screen_bg = Image.new("RGBA", (s, s))
    sbpx = screen_bg.load()
    for y in range(s):
        for x in range(s):
            t = (x + y) / (2.0 * (s - 1)) if s > 1 else 0
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

    # ── 3. Screen Grid HUD (Radar and crosshairs) ──
    # Only draw when size >= 24 to avoid rendering messy lines at 16px
    if size >= 24:
        hud = Image.new("RGBA", (s, s), (0, 0, 0, 0))
        hd = ImageDraw.Draw(hud)
        hud_color = PHOSPHOR + (20,) # very faint
        
        # Radar circles
        # r = 80
        hd.ellipse([
            round(48 * scale), round(48 * scale),
            round(208 * scale), round(208 * scale)
        ], fill=None, outline=PHOSPHOR + (15,))
        # r = 50
        hd.ellipse([
            round(78 * scale), round(78 * scale),
            round(178 * scale), round(178 * scale)
        ], fill=None, outline=PHOSPHOR + (10,))

        # Crosshairs
        hd.line([(round(128 * scale), round(20 * scale)), (round(128 * scale), round(40 * scale))], fill=hud_color, width=max(1, SS // 4))
        hd.line([(round(128 * scale), round(216 * scale)), (round(128 * scale), round(236 * scale))], fill=hud_color, width=max(1, SS // 4))
        hd.line([(round(20 * scale), round(128 * scale)), (round(40 * scale), round(128 * scale))], fill=hud_color, width=max(1, SS // 4))
        hd.line([(round(216 * scale), round(128 * scale)), (round(236 * scale), round(128 * scale))], fill=hud_color, width=max(1, SS // 4))
        
        # Center ticks
        hd.line([(round(120 * scale), round(128 * scale)), (round(136 * scale), round(128 * scale))], fill=PHOSPHOR + (40,), width=max(1, SS // 4))
        hd.line([(round(128 * scale), round(120 * scale)), (round(128 * scale), round(136 * scale))], fill=PHOSPHOR + (40,), width=max(1, SS // 4))
        
        img = Image.alpha_composite(img, hud)

    # ── 4. Drawing Paths (Monogram and Viewfinder Brackets) ──
    mark = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    md = ImageDraw.Draw(mark)

    w_sharp = max(1, round(3.2 * scale))
    w_glow = max(1, round(8.0 * scale))

    def draw_strip(points, width, color):
        scaled_pts = [(round(p[0] * scale), round(p[1] * scale)) for p in points]
        md.line(scaled_pts, fill=color, width=width, joint="round")
        cap = width / 2.0
        for pt in scaled_pts:
            md.ellipse([pt[0] - cap, pt[1] - cap, pt[0] + cap, pt[1] + cap], fill=color)

    def draw_n_arc(width, color):
        # Bounding box of the arc: [135, 110, 155, 130]
        arc_box = [round(135 * scale), round(110 * scale), round(155 * scale), round(130 * scale)]
        md.arc(arc_box, start=180, end=360, fill=color, width=width)
        cap = width / 2.0
        # Round caps at the arc endpoints
        for pt in [(round(135 * scale), round(120 * scale)), (round(155 * scale), round(120 * scale))]:
            md.ellipse([pt[0] - cap, pt[1] - cap, pt[0] + cap, pt[1] + cap], fill=color)

    # ── Viewfinder brackets ──
    brackets = [
        [(60, 80), (60, 60), (80, 60)],
        [(176, 60), (196, 60), (196, 80)],
        [(60, 176), (60, 196), (80, 196)],
        [(176, 196), (196, 196), (196, 176)]
    ]

    # Glow pass for brackets & logo
    if size >= 24:  # skip glow at 16px to avoid blur mess
        for pts in brackets:
            draw_strip(pts, w_glow, ph_glow)
        
        # T glow
        draw_strip([(85, 105), (125, 105)], w_glow, ph_glow) # T bar
        draw_strip([(105, 105), (105, 155)], w_glow, ph_glow) # T stem
        # n glow
        draw_strip([(135, 155), (135, 120)], w_glow, ph_glow) # n left leg
        draw_n_arc(w_glow, ph_glow)                          # n arc
        draw_strip([(155, 120), (155, 155)], w_glow, ph_glow) # n right leg

    # Sharp pass for brackets & logo
    # For very small size (16px), scale line width up slightly to keep it sharp
    w_draw_sharp = max(SS, w_sharp)
    for pts in brackets:
        draw_strip(pts, w_draw_sharp, ph)
    
    # T sharp
    draw_strip([(85, 105), (125, 105)], w_draw_sharp, ph)
    draw_strip([(105, 105), (105, 155)], w_draw_sharp, ph)
    # n sharp
    draw_strip([(135, 155), (135, 120)], w_draw_sharp, ph)
    draw_n_arc(w_draw_sharp, ph)
    draw_strip([(155, 120), (155, 155)], w_draw_sharp, ph)

    # ── 5. HUD Text details (only draw if size >= 64) ──
    if size >= 64:
        try:
            from PIL import ImageFont
            font = ImageFont.load_default()
            text_color = PHOSPHOR + (75,) # faint text
            md.text((round(32 * scale), round(38 * scale)), "SYS_ON", fill=text_color, font=font)
            md.text((round(180 * scale), round(38 * scale)), "CH.01", fill=text_color, font=font)
            md.text((round(32 * scale), round(208 * scale)), "LNK: OK", fill=text_color, font=font)
            md.text((round(176 * scale), round(208 * scale)), "v2.0.7", fill=text_color, font=font)
        except Exception:
            pass

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
