"""Pre-render the tray-state emoji to raw RGBA for `src/icons.rs`.

The tray icon is repainted from these buffers at runtime (see `src/icons.rs`), so the
emoji are rasterised here, once, with Segoe UI Emoji in colour rather than at run time.

Run:  python tools/gen_icons.py
Writes: assets/icon_cold.rgba, assets/icon_warm.rgba, assets/icon_hot.rgba
        artifacts/icon_preview.png (4x montage for eyeballing)
"""

import os
from PIL import Image, ImageDraw, ImageFont

SIZE = 32
FONT = "C:/Windows/Fonts/seguiemj.ttf"

# state -> (emoji, label).  VS16 (U+FE0F) forces the colour presentation on the snowflake.
STATES = [
    ("cold", "\u2744\uFE0F", "snowflake"),
    ("warm", "\U0001F4A8", "steam"),
    ("hot", "\U0001F525", "fire"),
]

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ASSETS = os.path.join(ROOT, "assets")
ARTIFACTS = os.path.join(ROOT, "artifacts")


def render(glyph):
    """Rasterise one emoji, then fit the *drawn* pixels into the tile.

    Neither `textbbox` nor `anchor` can be trusted for a COLR colour glyph: `anchor="mm"`
    centred the snowflake so far left that it was drawn past the canvas edge and clipped.
    So: draw large, well inside a roomy canvas with no anchor, then crop to the actual
    non-transparent pixels and scale that to the tile.
    """
    canvas = SIZE * 16
    img = Image.new("RGBA", (canvas, canvas), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)
    font = ImageFont.truetype(FONT, SIZE * 4)
    origin = canvas // 4
    try:
        draw.text((origin, origin), glyph, font=font, embedded_color=True)
    except TypeError:
        draw.text((origin, origin), glyph, font=font)

    bbox = img.getbbox()
    if bbox:
        img = img.crop(bbox)

    w, h = img.size
    scale = SIZE / max(w, h)
    nw, nh = max(1, round(w * scale)), max(1, round(h * scale))
    img = img.resize((nw, nh), Image.LANCZOS)

    tile = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    tile.paste(img, ((SIZE - nw) // 2, (SIZE - nh) // 2), img)
    return tile


def render_app_icon(size=256):
    """The application icon: a flat tile with a three-blade fan.

    Deliberately in the Windows flat/system idiom rather than an emoji, so the window and
    taskbar entry are recognisable next to other apps. Three bold blades and no housing:
    a ring and finer blades disappear into mush at the 16-32 px the shell actually draws.
    """
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)
    draw.rounded_rectangle(
        [0, 0, size - 1, size - 1], radius=int(size * 0.22), fill=(38, 92, 140, 255)
    )

    c = size / 2
    ink = (236, 243, 249, 255)

    # One bold blade, then the same canvas rotated a third of a turn at a time.
    blade = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    rx, ry = size * 0.16, size * 0.28
    bcy = c - size * 0.235
    ImageDraw.Draw(blade).ellipse([c - rx, bcy - ry, c + rx, bcy + ry], fill=ink)
    for angle in (30, 150, 270):
        img.alpha_composite(blade.rotate(angle, resample=Image.BICUBIC, center=(c, c)))

    hub = size * 0.115
    draw.ellipse([c - hub, c - hub, c + hub, c + hub], fill=ink)
    return img


def main():
    os.makedirs(ASSETS, exist_ok=True)
    os.makedirs(ARTIFACTS, exist_ok=True)
    tiles = []
    for name, glyph, label in STATES:
        img = render(glyph)
        raw = img.tobytes()  # RGBA, row-major, top-down
        path = os.path.join(ASSETS, f"icon_{name}.rgba")
        with open(path, "wb") as fh:
            fh.write(raw)
        print(f"{name:5} {label:10} -> {path}  {len(raw)} bytes")
        tiles.append((name, img))

    # 4x montage on a checkerboard so transparency and colour are both visible.
    scale = 4
    pad = 8
    tile = SIZE * scale
    width = len(tiles) * (tile + pad) + pad
    height = tile + 2 * pad
    prev = Image.new("RGB", (width, height), (255, 255, 255))
    d = ImageDraw.Draw(prev)
    for i in range(0, width, 8):
        for j in range(0, height, 8):
            if (i // 8 + j // 8) % 2 == 0:
                d.rectangle([i, j, i + 7, j + 7], fill=(220, 220, 220))
    for idx, (name, img) in enumerate(tiles):
        big = img.resize((tile, tile), Image.NEAREST)
        prev.paste(big, (pad + idx * (tile + pad), pad), big)
    out = os.path.join(ARTIFACTS, "icon_preview.png")
    prev.save(out)
    print("preview ->", out)

    # Application icon: raw RGBA for the live window, and a multi-size .ico embedded in
    # the executable as its resource.
    app = render_app_icon()
    app_32 = app.resize((SIZE, SIZE), Image.LANCZOS)
    app_path = os.path.join(ASSETS, "app.rgba")
    with open(app_path, "wb") as fh:
        fh.write(app_32.tobytes())
    print(f"app   fan-tile   -> {app_path}  {SIZE * SIZE * 4} bytes")

    ico_path = os.path.join(ASSETS, "paveconel.ico")
    app.save(
        ico_path,
        sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
    )
    print("app   icon       ->", ico_path)

    app.resize((256, 256), Image.LANCZOS).save(
        os.path.join(ARTIFACTS, "app_icon_preview.png")
    )


if __name__ == "__main__":
    main()
