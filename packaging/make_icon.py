"""Writes the Juttusihti icon PNGs from the artwork in packaging/icon-source.png.

The artwork is a 1024 px rounded square with transparent corners. Needs
Pillow (`pip install pillow`). Run from the repository root:
    python packaging/make_icon.py
"""
import os

from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
SIZES = (32, 64, 128, 256, 512)


def main():
    art = Image.open(os.path.join(HERE, "icon-source.png")).convert("RGBA")
    for size in SIZES:
        path = os.path.join(HERE, "icons", f"juttusihti-{size}.png")
        art.resize((size, size), Image.LANCZOS).save(path, optimize=True)
        print(path)
    # The Windows .exe embeds this (crates/app/build.rs); the taskbar shows it.
    path = os.path.join(HERE, "icons", "juttusihti.ico")
    art.save(path, sizes=[(s, s) for s in (16, 24, 32, 48, 64, 128, 256)])
    print(path)


if __name__ == "__main__":
    main()
