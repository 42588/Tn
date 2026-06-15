"""Generate tn.ico — the Tn application icon (磷光 Phosphor language).

Design: macOS-style 3D / Glassmorphism
  • Chassis: rounded-square with a smooth diagonal gradient (3D body)
  • Overlay: a subtle frosted/glossy top reflection
  • Edge: inner bevel highlight/shadow
  • Shadow: soft drop shadow behind the icon
  • Mark: a clean ">" prompt with the signature #5BE7C4 Phosphor block cursor

Output sizes: 16/24/32/48/64/128/256 px, BMP/DIB entries.
Run: python scripts/gen_icon.py
"""

import struct
from pathlib import Path
from PIL import Image, ImageDraw, ImageFilter

# ── 磷光 palette (design/phosphor.css :root) ──
PHOSPHOR = (0x5B, 0xE7, 0xC4)      # #5BE7C4  the single life color (cursor)
BG_TOP = (0x3B, 0x43, 0x58)       # Lighter grayish blue for 3D top
BG_BOT = (0x16, 0x1B, 0x29)       # Darker bottom
BORDER_TOP = (0x7F, 0x8A, 0xA4)   # Highlight on top edge
BORDER_BOT = (0x0B, 0x0E, 0x16)   # Shadow on bottom edge
PROMPT_COLOR = (0xFF, 0xFF, 0xFF) # White for '>'
CORNER_RADIUS_RATIO = 0.225       # macOS style is roughly 22.5%
SS = 4                             # supersampling for anti-aliasing


def _lerp(a, b, t):
    return tuple(round(a[i] + (b[i] - a[i]) * t) for i in range(len(a)))


def _draw_rounded_rect_with_gradient(box, radius, color_top, color_bot, direction="vertical"):
    x0, y0, x1, y1 = box
    w = x1 - x0
    h = y1 - y0
    
    grad = Image.new("RGBA", (w, h))
    gpx = grad.load()
    for y in range(h):
        for x in range(w):
            if direction == "vertical":
                t = y / max(1, (h - 1))
            else:
                t = (x + y) / max(1, (w + h - 2))
            gpx[x, y] = _lerp(color_top, color_bot, t) + (255,)
            
    mask = Image.new("L", (w, h), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, w-1, h-1], radius=radius, fill=255)
    
    temp = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    temp.paste(grad, (0, 0), mask)
    return temp


def render(size: int) -> Image.Image:
    """Render the icon at `size`×`size` (RGBA), supersampled then downscaled."""
    s = size * SS
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    
    radius = int(CORNER_RADIUS_RATIO * s)
    margin = int(0.08 * s)  # Drop shadow margin
    
    body_box = [margin, margin, s - margin - 1, s - margin - 1]
    bw = body_box[2] - body_box[0]
    bh = body_box[3] - body_box[1]
    
    # 1. Drop Shadow
    shadow_offset = int(0.04 * s)
    shadow = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    ImageDraw.Draw(shadow).rounded_rectangle(
        [margin, margin + shadow_offset, s - margin - 1, s - margin - 1 + shadow_offset],
        radius=radius, fill=(0, 0, 0, 100)
    )
    shadow = shadow.filter(ImageFilter.GaussianBlur(radius=int(0.04 * s)))
    img.alpha_composite(shadow)
    
    # 2. Main Chassis Gradient (Diagonal)
    chassis = _draw_rounded_rect_with_gradient(
        [0, 0, bw, bh], radius, BG_TOP, BG_BOT, direction="diagonal"
    )
    img.alpha_composite(chassis, (margin, margin))
    
    # 3. Inner border highlight/shadow
    border_w = max(1, int(0.015 * s))
    border = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    bd = ImageDraw.Draw(border)
    bd.rounded_rectangle(body_box, radius=radius, outline=BORDER_TOP+(180,), width=border_w)
    
    border_mask = Image.new("L", (s, s))
    bmpx = border_mask.load()
    for y in range(s):
        t = y / max(1, (s - 1))
        for x in range(s):
            bmpx[x, y] = int((1 - t) * 255)
    border_top_layer = Image.new("RGBA", (s, s))
    border_top_layer.paste(border, (0, 0), border_mask)
    img.alpha_composite(border_top_layer)
    
    border_bot = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    ImageDraw.Draw(border_bot).rounded_rectangle(body_box, radius=radius, outline=BORDER_BOT+(200,), width=border_w)
    border_bot_mask = Image.new("L", (s, s))
    bbmpx = border_bot_mask.load()
    for y in range(s):
        t = y / max(1, (s - 1))
        for x in range(s):
            bbmpx[x, y] = int(t * 255)
    border_bot_layer = Image.new("RGBA", (s, s))
    border_bot_layer.paste(border_bot, (0, 0), border_bot_mask)
    img.alpha_composite(border_bot_layer)
    
    # 4. Glass reflection (Top glossy curve)
    gloss = Image.new("RGBA", (bw, bh), (0, 0, 0, 0))
    ImageDraw.Draw(gloss).ellipse([-bw*0.5, -bh*0.8, bw*1.5, bh*0.5], fill=(255, 255, 255, 12))
    gloss_mask = Image.new("L", (bw, bh), 0)
    ImageDraw.Draw(gloss_mask).rounded_rectangle([0, 0, bw-1, bh-1], radius=radius, fill=255)
    gloss_layer = Image.new("RGBA", (bw, bh), (0, 0, 0, 0))
    gloss_layer.paste(gloss, (0, 0), gloss_mask)
    img.alpha_composite(gloss_layer, (margin, margin))
    
    # 5. Terminal prompt ">_"
    content = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    cd = ImageDraw.Draw(content)
    
    stroke = max(1, int(0.06 * s))
    cx, cy = int(0.32 * s), int(0.48 * s)
    arr_w, arr_h = int(0.12 * s), int(0.12 * s)
    
    # ">"
    cd.line([(cx - arr_w/2, cy - arr_h), (cx + arr_w/2, cy), (cx - arr_w/2, cy + arr_h)], 
            fill=PROMPT_COLOR+(255,), width=stroke, joint="curve")
    # Cap the ends
    cd.ellipse([cx - arr_w/2 - stroke/2, cy - arr_h - stroke/2, cx - arr_w/2 + stroke/2, cy - arr_h + stroke/2], fill=PROMPT_COLOR+(255,))
    cd.ellipse([cx - arr_w/2 - stroke/2, cy + arr_h - stroke/2, cx - arr_w/2 + stroke/2, cy + arr_h + stroke/2], fill=PROMPT_COLOR+(255,))
    cd.ellipse([cx + arr_w/2 - stroke/2, cy - stroke/2, cx + arr_w/2 + stroke/2, cy + stroke/2], fill=PROMPT_COLOR+(255,))
            
    # "_" (Phosphor colored block)
    bx, by = int(0.50 * s), int(0.48 * s + arr_h - stroke/2)
    bw_cur, bh_cur = int(0.18 * s), stroke
    cd.rectangle([bx, by, bx + bw_cur, by + bh_cur], fill=PHOSPHOR+(255,))
    cd.ellipse([bx - stroke/2, by, bx + stroke/2, by + bh_cur], fill=PHOSPHOR+(255,))
    cd.ellipse([bx + bw_cur - stroke/2, by, bx + bw_cur + stroke/2, by + bh_cur], fill=PHOSPHOR+(255,))
    
    # Drop shadow for the prompt to enhance depth
    content_shadow = content.copy()
    content_shadow = content_shadow.filter(ImageFilter.GaussianBlur(radius=int(0.01 * s)))
    cs_data = content_shadow.load()
    for y in range(s):
        for x in range(s):
            r, g, b, a = cs_data[x, y]
            if a > 0:
                cs_data[x, y] = (0, 0, 0, int(a * 0.4))
                
    img.alpha_composite(content_shadow, (0, int(0.015 * s)))
    img.alpha_composite(content)

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
