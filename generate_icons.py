# Generate MediaDown app icons (32/128/256 png + multi-size ico)
# Run: python generate_icons.py  (outputs into src-tauri/icons/)
import os
from PIL import Image, ImageDraw

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "icons")
os.makedirs(OUT, exist_ok=True)


def draw_icon(size: int) -> Image.Image:
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    # rounded-square background with vertical gradient
    r = int(size * 0.22)
    top = (79, 142, 247, 255)      # #4f8ef7
    bottom = (124, 92, 255, 255)   # #7c5cff
    for y in range(size):
        t = y / size
        col = tuple(int(top[i] + (bottom[i] - top[i]) * t) for i in range(4))
        d.line([(0, y), (size, y)], fill=col)
    # mask rounded corners
    mask = Image.new("L", (size, size), 0)
    md = ImageDraw.Draw(mask)
    md.rounded_rectangle([0, 0, size - 1, size - 1], radius=r, fill=255)
    img.putalpha(mask)

    # white download arrow
    cx = size * 0.5
    shaft_w = size * 0.16
    head_w = size * 0.42
    head_h = size * 0.30
    shaft_top = size * 0.22
    shaft_bot = size * 0.52
    head_bot = size * 0.72

    # shaft (vertical bar)
    d.rounded_rectangle(
        [cx - shaft_w / 2, shaft_top, cx + shaft_w / 2, shaft_bot],
        radius=shaft_w / 4,
        fill=(255, 255, 255, 255),
    )
    # arrow head (triangle)
    d.polygon(
        [
            (cx - head_w / 2, head_bot - head_h),
            (cx + head_w / 2, head_bot - head_h),
            (cx, head_bot),
        ],
        fill=(255, 255, 255, 255),
    )
    # baseline bar
    d.rounded_rectangle(
        [cx - head_w * 0.55, head_bot + size * 0.03, cx + head_w * 0.55, head_bot + size * 0.11],
        radius=size * 0.03,
        fill=(255, 255, 255, 255),
    )
    return img


for size, name in [(32, "32x32.png"), (128, "128x128.png"), (256, "128x128@2x.png")]:
    draw_icon(size).save(os.path.join(OUT, name))
    print("wrote", name)

# multi-size .ico (16..256)
img256 = draw_icon(256)
img256.save(
    os.path.join(OUT, "icon.ico"),
    sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
)
print("wrote icon.ico")
