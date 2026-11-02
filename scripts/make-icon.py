"""Draws the OpenZap icon: a TV with antenna and a lightning bolt.

Usage: python3 scripts/make-icon.py [OUT_DIR] [PREVIEW.png]   (needs Pillow)
Writes icon.png (256 px, window icon) and icon.ico (exe icon).
"""
import sys
from PIL import Image, ImageDraw

out = sys.argv[1] if len(sys.argv) > 1 else "assets"
N = 1024                      # draw big, downscale for smooth edges
img = Image.new("RGBA", (N, N), (0, 0, 0, 0))

# Background: rounded square with a vertical blue gradient.
grad = Image.new("RGBA", (N, N))
top, bottom = (37, 99, 235), (30, 58, 138)
for y in range(N):
    t = y / (N - 1)
    c = tuple(round(top[i] + (bottom[i] - top[i]) * t) for i in range(3)) + (255,)
    ImageDraw.Draw(grad).line([(0, y), (N, y)], fill=c)
mask = Image.new("L", (N, N), 0)
ImageDraw.Draw(mask).rounded_rectangle([24, 24, N - 24, N - 24], radius=220, fill=255)
img.paste(grad, (0, 0), mask)

d = ImageDraw.Draw(img)
white = (255, 255, 255, 255)
# Antenna.
d.line([(512, 300), (360, 140)], fill=white, width=48)
d.line([(512, 300), (664, 140)], fill=white, width=48)
for x in (360, 664):
    d.ellipse([x - 40, 100, x + 40, 180], fill=white)
# Screen.
d.rounded_rectangle([170, 290, 854, 800], radius=90, outline=white, width=60)
# Stand.
d.rounded_rectangle([392, 840, 632, 900], radius=30, fill=white)
# Lightning bolt.
bolt = [(560, 360), (380, 580), (500, 580), (450, 740), (650, 500), (528, 500), (590, 360)]
d.polygon(bolt, fill=(250, 204, 21, 255))

big = img
big.resize((256, 256), Image.LANCZOS).save(f"{out}/icon.png")
big.save(f"{out}/icon.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
# Preview strip of small sizes to judge legibility.
strip = Image.new("RGBA", (16 + 32 + 48 + 64 + 256 + 60, 256), (40, 40, 40, 255))
x = 10
for s in (16, 32, 48, 64, 256):
    strip.paste(big.resize((s, s), Image.LANCZOS), (x, 256 - s), big.resize((s, s), Image.LANCZOS))
    x += s + 10
if len(sys.argv) > 2:
    strip.save(sys.argv[2])
