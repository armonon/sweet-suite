#!/usr/bin/env python3
"""Generate SWEET Visual's app icon → assets/AppIcon.icns.

A modern macOS-style rounded-square ("squircle") with a vertical brand-blue
gradient, a soft top-left highlight, a thin glass edge, and a bold white "S"
brand mark. Pure PIL + macOS iconutil — no design assets checked in beyond the
finished .icns. Re-run after tweaking; `scripts/package_macos.sh` copies the
result into the bundle.
"""
import math
import os
import subprocess
import sys
import tempfile

from PIL import Image, ImageDraw, ImageFont, ImageFilter

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT_ICNS = os.path.join(ROOT, "assets", "AppIcon.icns")
N = 1024  # master render size

# Brand palette (matches the app's ACCENT ramp).
TOP = (0x5B, 0x9C, 0xF8)     # light accent blue
BOTTOM = (0x1E, 0x3A, 0x8A)  # deep indigo


def squircle_mask(n, radius_frac=0.225):
    """A superellipse-ish rounded-square alpha mask (Apple's icon silhouette)."""
    m = Image.new("L", (n, n), 0)
    d = ImageDraw.Draw(m)
    r = int(n * radius_frac)
    d.rounded_rectangle([0, 0, n - 1, n - 1], radius=r, fill=255)
    return m


def vertical_gradient(n, top, bottom):
    g = Image.new("RGB", (n, n))
    px = g.load()
    for y in range(n):
        t = y / (n - 1)
        # ease-in-out so the middle band reads richer
        t = t * t * (3 - 2 * t)
        r = round(top[0] + (bottom[0] - top[0]) * t)
        gg = round(top[1] + (bottom[1] - top[1]) * t)
        b = round(top[2] + (bottom[2] - top[2]) * t)
        for x in range(n):
            px[x, y] = (r, gg, b)
    return g


def load_bold_font(size):
    for path in [
        "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
        "/System/Library/Fonts/HelveticaNeue.ttc",
        "/System/Library/Fonts/Supplemental/Helvetica.ttc",
    ]:
        if os.path.exists(path):
            try:
                return ImageFont.truetype(path, size)
            except Exception:
                continue
    return ImageFont.load_default()


def build():
    base = vertical_gradient(N, TOP, BOTTOM).convert("RGBA")

    # Soft top-left highlight for a little depth.
    hi = Image.new("L", (N, N), 0)
    hd = ImageDraw.Draw(hi)
    hd.ellipse([-N * 0.35, -N * 0.45, N * 0.75, N * 0.55], fill=70)
    hi = hi.filter(ImageFilter.GaussianBlur(N * 0.08))
    white = Image.new("RGBA", (N, N), (255, 255, 255, 255))
    base = Image.composite(white, base, hi)

    # Bold white "S" brand mark, optically centred.
    font = load_bold_font(int(N * 0.62))
    layer = Image.new("RGBA", (N, N), (0, 0, 0, 0))
    ld = ImageDraw.Draw(layer)
    bbox = ld.textbbox((0, 0), "S", font=font)
    tw, th = bbox[2] - bbox[0], bbox[3] - bbox[1]
    x = (N - tw) / 2 - bbox[0]
    y = (N - th) / 2 - bbox[1] - N * 0.01
    # subtle shadow under the letter
    sh = Image.new("RGBA", (N, N), (0, 0, 0, 0))
    ImageDraw.Draw(sh).text((x, y + N * 0.012), "S", font=font, fill=(10, 20, 60, 150))
    sh = sh.filter(ImageFilter.GaussianBlur(N * 0.012))
    base = Image.alpha_composite(base, sh)
    ld.text((x, y), "S", font=font, fill=(255, 255, 255, 255))
    base = Image.alpha_composite(base, layer)

    # Thin glass edge stroke.
    edge = Image.new("RGBA", (N, N), (0, 0, 0, 0))
    ed = ImageDraw.Draw(edge)
    r = int(N * 0.225)
    ed.rounded_rectangle([2, 2, N - 3, N - 3], radius=r, outline=(255, 255, 255, 40), width=3)
    base = Image.alpha_composite(base, edge)

    # Clip to the squircle.
    out = Image.new("RGBA", (N, N), (0, 0, 0, 0))
    out.paste(base, (0, 0), squircle_mask(N))
    return out


def main():
    icon = build()
    os.makedirs(os.path.dirname(OUT_ICNS), exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        master = os.path.join(tmp, "icon_1024.png")
        icon.save(master)
        iconset = os.path.join(tmp, "AppIcon.iconset")
        os.makedirs(iconset)
        # macOS iconset sizes (px, name).
        specs = [
            (16, "icon_16x16.png"), (32, "icon_16x16@2x.png"),
            (32, "icon_32x32.png"), (64, "icon_32x32@2x.png"),
            (128, "icon_128x128.png"), (256, "icon_128x128@2x.png"),
            (256, "icon_256x256.png"), (512, "icon_256x256@2x.png"),
            (512, "icon_512x512.png"), (1024, "icon_512x512@2x.png"),
        ]
        for px, name in specs:
            icon.resize((px, px), Image.LANCZOS).save(os.path.join(iconset, name))
        subprocess.run(["iconutil", "-c", "icns", iconset, "-o", OUT_ICNS], check=True)
    print(f"wrote {OUT_ICNS}")


if __name__ == "__main__":
    sys.exit(main())
