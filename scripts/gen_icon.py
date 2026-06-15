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
from PIL import Image, ImageDraw, ImageFilter

# ── 磷光 palette (design/phosphor.css :root) ──
PHOSPHOR = (0x5B, 0xE7, 0xC4)      # #5BE7C4  the single life color (cursor)
PLATE_TOP = (0x16, 0x1B, 0x29)     # slightly lifted L1 — top of the chassis
PLATE_BOT = (0x0B, 0x0E, 0x16)     # #0B0E16  L0 chassis floor — bottom
HAIRLINE = (0x34, 0x3E, 0x52)      # cool seam edge (h1-ish), opaque-ish
CORNER_RADIUS_RATIO = 0.235        # rounded-square chassis
SS = 4                             # supersampling for anti-aliasing


def _lerp(a, b, t):
    return tuple(round(a[i] + (b[i] - a[i]) * t) for i in range(3))


def draw_gradient_polygon(img, pts, color_start, color_end, direction="vertical"):
    """Draw a polygon filled with a linear gradient from color_start to color_end."""
    # Create single-channel mask and draw polygon on it
    mask = Image.new("L", img.size, 0)
    ImageDraw.Draw(mask).polygon(pts, fill=255)
    
    # Get bounding box of the polygon
    xs = [p[0] for p in pts]
    ys = [p[1] for p in pts]
    min_x, max_x = int(min(xs)), int(max(xs))
    min_y, max_y = int(min(ys)), int(max(ys))
    
    w = max_x - min_x + 1
    h = max_y - min_y + 1
    if w <= 0 or h <= 0:
        return
        
    # Create gradient fill image
    grad = Image.new("RGBA", (w, h))
    gpx = grad.load()
    for y in range(h):
        for x in range(w):
            if direction == "vertical":
                t = y / (h - 1) if h > 1 else 0
            else: # diagonal
                t = (x + y) / (w + h - 2) if (w + h - 2) > 0 else 0
            
            r = round(color_start[0] + (color_end[0] - color_start[0]) * t)
            g = round(color_start[1] + (color_end[1] - color_start[1]) * t)
            b = round(color_start[2] + (color_end[2] - color_start[2]) * t)
            a = round(color_start[3] + (color_end[3] - color_start[3]) * t) if len(color_start) > 3 and len(color_end) > 3 else 255
            gpx[x, y] = (r, g, b, a)
            
    # Paste the cropped gradient to the target image using the mask
    img.paste(grad, (min_x, min_y), mask.crop((min_x, min_y, max_x + 1, max_y + 1)))


def render(size: int) -> Image.Image:
    """Render the premium 3D Isometric Titanium Chassis and Suspended Core at `size`×`size` (RGBA)."""
    s = size * SS
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    scale = s / 256.0

    # Colors
    ph = PHOSPHOR + (255,)
    ph_glow = PHOSPHOR + (90,)
    ph_glow_soft = PHOSPHOR + (30,)

    # 1. Bottom Shadow (only if size >= 24 for clean results)
    if size >= 24:
        shadow = Image.new("RGBA", (s, s), (0, 0, 0, 0))
        sd = ImageDraw.Draw(shadow)
        shadow_pts = [
            (128 * scale, (65 + 15) * scale),
            (228 * scale, (120 + 15) * scale),
            (128 * scale, (190 + 15) * scale),
            (28 * scale, (120 + 15) * scale)
        ]
        sd.polygon(shadow_pts, fill=(0, 0, 0, 215))
        shadow_blur = shadow.filter(ImageFilter.GaussianBlur(max(1.0, 8.0 * scale)))
        img = Image.alpha_composite(img, shadow_blur)

    # 2. Chassis Base Left and Right Walls
    chassis_left_pts = [
        (36 * scale, (95 + 15) * scale),
        (128 * scale, (145 + 15) * scale),
        (128 * scale, (185 + 15) * scale),
        (36 * scale, (135 + 15) * scale)
    ]
    draw_gradient_polygon(img, chassis_left_pts, (0x2A, 0x30, 0x45), (0x0E, 0x10, 0x17), direction="vertical")

    chassis_right_pts = [
        (128 * scale, (145 + 15) * scale),
        (220 * scale, (95 + 15) * scale),
        (220 * scale, (135 + 15) * scale),
        (128 * scale, (185 + 15) * scale)
    ]
    draw_gradient_polygon(img, chassis_right_pts, (0x1B, 0x1E, 0x2A), (0x09, 0x0B, 0x10), direction="vertical")

    # 3. Side Grooves/Gratings (only if size >= 48 for clean detail)
    if size >= 48:
        md = ImageDraw.Draw(img)
        w_groove = max(1, round(1.0 * scale))
        
        # Left wall grooves
        md.polygon([
            (52 * scale, (118 + 15) * scale),
            (112 * scale, (151 + 15) * scale),
            (112 * scale, (153 + 15) * scale),
            (52 * scale, (120 + 15) * scale)
        ], fill=(6, 7, 10, 255))
        md.line([(52 * scale, (120 + 15) * scale), (112 * scale, (153 + 15) * scale)], fill=(0x37, 0x3E, 0x54, 150), width=w_groove)
        
        md.polygon([
            (52 * scale, (126 + 15) * scale),
            (112 * scale, (159 + 15) * scale),
            (112 * scale, (161 + 15) * scale),
            (52 * scale, (128 + 15) * scale)
        ], fill=(6, 7, 10, 255))
        md.line([(52 * scale, (128 + 15) * scale), (112 * scale, (161 + 15) * scale)], fill=(0x37, 0x3E, 0x54, 150), width=w_groove)

        # Right wall grooves
        md.polygon([
            (144 * scale, (151 + 15) * scale),
            (204 * scale, (118 + 15) * scale),
            (204 * scale, (120 + 15) * scale),
            (144 * scale, (153 + 15) * scale)
        ], fill=(6, 7, 10, 255))
        md.line([(144 * scale, (153 + 15) * scale), (204 * scale, (120 + 15) * scale)], fill=(0x26, 0x2B, 0x3A, 150), width=w_groove)
        
        md.polygon([
            (144 * scale, (159 + 15) * scale),
            (204 * scale, (126 + 15) * scale),
            (204 * scale, (128 + 15) * scale),
            (144 * scale, (161 + 15) * scale)
        ], fill=(6, 7, 10, 255))
        md.line([(144 * scale, (161 + 15) * scale), (204 * scale, (128 + 15) * scale)], fill=(0x26, 0x2B, 0x3A, 150), width=w_groove)

    # 4. Chassis Base Top Face
    chassis_top_pts = [
        (128 * scale, (45 + 15) * scale),
        (220 * scale, (95 + 15) * scale),
        (128 * scale, (145 + 15) * scale),
        (36 * scale, (95 + 15) * scale)
    ]
    draw_gradient_polygon(img, chassis_top_pts, (0x21, 0x25, 0x36), (0x0C, 0x0E, 0x14), direction="diagonal")

    # 5. CNC Chamfer Highlights
    md = ImageDraw.Draw(img)
    w_cnc = max(SS, round(1.5 * scale * SS) // SS)
    md.line([
        (36 * scale, (95 + 15) * scale),
        (128 * scale, (145 + 15) * scale),
        (220 * scale, (95 + 15) * scale)
    ], fill=(255, 255, 255, 46), width=w_cnc, joint="round")
    md.line([
        (128 * scale, (145 + 15) * scale),
        (128 * scale, (185 + 15) * scale)
    ], fill=(255, 255, 255, 20), width=max(1, SS // 2))

    # 6. Recessed Screen Basin (only if size >= 24 to avoid messy noise at 16px)
    if size >= 24:
        screen_basin_pts = [
            (128 * scale, (55 + 15) * scale),
            (208 * scale, (95 + 15) * scale),
            (128 * scale, (135 + 15) * scale),
            (48 * scale, (95 + 15) * scale)
        ]
        draw_gradient_polygon(img, screen_basin_pts, (0x0F, 0x11, 0x1C), (0x03, 0x04, 0x06), direction="diagonal")
        md.polygon(screen_basin_pts, fill=None, outline=(0x1A, 0x1D, 0x2E, 255))
        
        basin_glow = Image.new("RGBA", (s, s), (0, 0, 0, 0))
        ImageDraw.Draw(basin_glow).polygon(screen_basin_pts, fill=None, outline=PHOSPHOR + (30,), width=max(2, round(2.5 * scale)))
        basin_glow_blur = basin_glow.filter(ImageFilter.GaussianBlur(max(1.0, 4.0 * scale)))
        img = Image.alpha_composite(img, basin_glow_blur)

    # 7. Floor Glow from Crystal
    if size >= 24:
        floor_glow = Image.new("RGBA", (s, s), (0, 0, 0, 0))
        fgd = ImageDraw.Draw(floor_glow)
        cx, cy = 128 * scale, (100 + 15) * scale
        rx, ry = 28 * scale, 14 * scale
        fgd.ellipse([cx - rx, cy - ry, cx + rx, cy + ry], fill=PHOSPHOR + (56,))
        floor_glow_blur = floor_glow.filter(ImageFilter.GaussianBlur(max(1.0, 6.0 * scale)))
        img = Image.alpha_composite(img, floor_glow_blur)

    # 8. Suspended 3D Core (Crystal)
    c_yoff = -12 * scale
    
    if size >= 24:
        crystal_glow = Image.new("RGBA", (s, s), (0, 0, 0, 0))
        cgd = ImageDraw.Draw(crystal_glow)
        cgd.polygon([
            (128 * scale, (70 + 15) * scale + c_yoff),
            (148 * scale, (80 + 15) * scale + c_yoff),
            (128 * scale, (90 + 15) * scale + c_yoff),
            (108 * scale, (80 + 15) * scale + c_yoff)
        ], fill=PHOSPHOR + (90,))
        cgd.polygon([
            (108 * scale, (80 + 15) * scale + c_yoff),
            (128 * scale, (90 + 15) * scale + c_yoff),
            (128 * scale, (110 + 15) * scale + c_yoff),
            (108 * scale, (100 + 15) * scale + c_yoff)
        ], fill=PHOSPHOR + (64,))
        cgd.polygon([
            (128 * scale, (90 + 15) * scale + c_yoff),
            (148 * scale, (80 + 15) * scale + c_yoff),
            (148 * scale, (100 + 15) * scale + c_yoff),
            (128 * scale, (110 + 15) * scale + c_yoff)
        ], fill=PHOSPHOR + (64,))
        
        crystal_glow_blur = crystal_glow.filter(ImageFilter.GaussianBlur(max(1.0, 4.0 * scale)))
        img = Image.alpha_composite(img, crystal_glow_blur)

    # Crystal Faces
    crystal_top_pts = [
        (128 * scale, (70 + 15) * scale + c_yoff),
        (148 * scale, (80 + 15) * scale + c_yoff),
        (128 * scale, (90 + 15) * scale + c_yoff),
        (108 * scale, (80 + 15) * scale + c_yoff)
    ]
    draw_gradient_polygon(img, crystal_top_pts, (0xA3, 0xF7, 0xDF), (0x3C, 0xE2, 0xB6), direction="diagonal")

    crystal_left_pts = [
        (108 * scale, (80 + 15) * scale + c_yoff),
        (128 * scale, (90 + 15) * scale + c_yoff),
        (128 * scale, (110 + 15) * scale + c_yoff),
        (108 * scale, (100 + 15) * scale + c_yoff)
    ]
    draw_gradient_polygon(img, crystal_left_pts, (0x1E, 0xA8, 0x85), (0x09, 0x53, 0x41), direction="vertical")

    crystal_right_pts = [
        (128 * scale, (90 + 15) * scale + c_yoff),
        (148 * scale, (80 + 15) * scale + c_yoff),
        (148 * scale, (100 + 15) * scale + c_yoff),
        (128 * scale, (110 + 15) * scale + c_yoff)
    ]
    draw_gradient_polygon(img, crystal_right_pts, (0x12, 0x89, 0x6B), (0x05, 0x3A, 0x2D), direction="vertical")

    # Crystal Ridges
    md = ImageDraw.Draw(img)
    w_ridge = max(1, round(1.2 * scale))
    md.line([
        (108 * scale, (80 + 15) * scale + c_yoff),
        (128 * scale, (90 + 15) * scale + c_yoff),
        (148 * scale, (80 + 15) * scale + c_yoff)
    ], fill=(0xA3, 0xF7, 0xDF, 204), width=w_ridge, joint="round")
    md.line([
        (128 * scale, (90 + 15) * scale + c_yoff),
        (128 * scale, (110 + 15) * scale + c_yoff)
    ], fill=(0xA3, 0xF7, 0xDF, 204), width=w_ridge)

    # 9. Laser-Etched T/n (LOD: only draw if size >= 48)
    if size >= 48:
        # T (Left side)
        w_laser_glow = max(2, round(2.5 * scale))
        md.line([
            (112 * scale, (84.5 + 15) * scale + c_yoff),
            (124 * scale, (90.5 + 15) * scale + c_yoff)
        ], fill=PHOSPHOR + (120,), width=w_laser_glow, joint="round")
        md.line([
            (118 * scale, (87.5 + 15) * scale + c_yoff),
            (118 * scale, (102.5 + 15) * scale + c_yoff)
        ], fill=PHOSPHOR + (120,), width=w_laser_glow, joint="round")
        
        w_laser_core = max(1, round(1.0 * scale))
        md.line([
            (112 * scale, (84.5 + 15) * scale + c_yoff),
            (124 * scale, (90.5 + 15) * scale + c_yoff)
        ], fill=(255, 255, 255, 255), width=w_laser_core, joint="round")
        md.line([
            (118 * scale, (87.5 + 15) * scale + c_yoff),
            (118 * scale, (102.5 + 15) * scale + c_yoff)
        ], fill=(255, 255, 255, 255), width=w_laser_core, joint="round")

        # n (Right side)
        n_pts = [
            (134 * scale, (105.5 + 15) * scale + c_yoff),
            (134 * scale, (98.5 + 15) * scale + c_yoff),
            (138 * scale, (95.5 + 15) * scale + c_yoff),
            (142 * scale, (97.5 + 15) * scale + c_yoff),
            (142 * scale, (102.5 + 15) * scale + c_yoff)
        ]
        md.line(n_pts, fill=PHOSPHOR + (120,), width=w_laser_glow, joint="round")
        md.line(n_pts, fill=(255, 255, 255, 255), width=w_laser_core, joint="round")

    # 10. Floating Target Brackets (only if size >= 24)
    if size >= 24:
        brackets = [
            [(103, 62 + 15), (118, 55 + 15), (123, 57.5 + 15)],
            [(153, 62 + 15), (138, 55 + 15), (133, 57.5 + 15)],
            [(103, 128 + 15), (118, 135 + 15), (123, 132.5 + 15)],
            [(153, 128 + 15), (138, 135 + 15), (133, 132.5 + 15)]
        ]
        w_b_glow = max(2, round(2.0 * scale))
        w_b_sharp = max(SS, round(1.0 * scale * SS) // SS)
        
        for b_pts in brackets:
            scaled_pts = [(round(p[0] * scale), round(p[1] * scale)) for p in b_pts]
            md.line(scaled_pts, fill=PHOSPHOR + (60,), width=w_b_glow, joint="round")
        for b_pts in brackets:
            scaled_pts = [(round(p[0] * scale), round(p[1] * scale)) for p in b_pts]
            md.line(scaled_pts, fill=ph, width=w_b_sharp, joint="round")

    # 11. Tech Particles (only if size >= 64)
    if size >= 64:
        p1 = (round(80 * scale), round((95 + 15) * scale))
        p2 = (round(176 * scale), round((95 + 15) * scale))
        p3 = (round(128 * scale), round((155 + 15) * scale))
        r12 = max(1, round(1.5 * scale))
        r3 = max(1, round(1.2 * scale))
        
        md.ellipse([p1[0] - r12, p1[1] - r12, p1[0] + r12, p1[1] + r12], fill=PHOSPHOR + (150,))
        md.ellipse([p2[0] - r12, p2[1] - r12, p2[0] + r12, p2[1] + r12], fill=PHOSPHOR + (150,))
        md.ellipse([p3[0] - r3, p3[1] - r3, p3[0] + r3, p3[1] + r3], fill=PHOSPHOR + (100,))

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
