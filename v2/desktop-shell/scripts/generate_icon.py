#!/usr/bin/env python3
"""
Generate a classroom-operations themed icon for desktop-shell.

Outputs:
  - build/icon-master-1024.png
  - build/icon.ico (multi-size)
  - build/icon-preview-strip.png
"""

from __future__ import annotations

import argparse
from pathlib import Path
from typing import Iterable

from PIL import Image, ImageDraw, ImageFilter


BASE_SIZE = 1024
ICO_SIZES = (16, 24, 32, 48, 64, 128, 256)
SHORTCUT_SIZE = 1024


def _lerp(a: int, b: int, t: float) -> int:
    return int(round(a + (b - a) * t))


def _make_gradient(size: int, c1: tuple[int, int, int], c2: tuple[int, int, int]) -> Image.Image:
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    px = img.load()
    for y in range(size):
        ty = y / (size - 1)
        for x in range(size):
            tx = x / (size - 1)
            t = min(1.0, max(0.0, (tx * 0.58) + (ty * 0.42)))
            r = _lerp(c1[0], c2[0], t)
            g = _lerp(c1[1], c2[1], t)
            b = _lerp(c1[2], c2[2], t)
            px[x, y] = (r, g, b, 255)
    return img


def _draw_check(draw: ImageDraw.ImageDraw, box: tuple[int, int, int, int], color: tuple[int, int, int, int], width: int) -> None:
    x0, y0, x1, y1 = box
    w = x1 - x0
    h = y1 - y0
    p1 = (x0 + int(w * 0.22), y0 + int(h * 0.54))
    p2 = (x0 + int(w * 0.42), y0 + int(h * 0.73))
    p3 = (x0 + int(w * 0.78), y0 + int(h * 0.32))
    draw.line([p1, p2, p3], fill=color, width=width, joint="curve")


def _build_rounded_tile(size: int, c1: tuple[int, int, int], c2: tuple[int, int, int]) -> Image.Image:
    grad = _make_gradient(size, c1, c2)
    mask = Image.new("L", (size, size), 0)
    mask_draw = ImageDraw.Draw(mask)
    pad = int(size * 0.06)
    radius = int(size * 0.215)
    mask_draw.rounded_rectangle((pad, pad, size - pad, size - pad), radius=radius, fill=255)

    tile = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    tile.paste(grad, (0, 0), mask=mask)

    highlight = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    hdraw = ImageDraw.Draw(highlight)
    hdraw.ellipse(
        (
            int(size * 0.08),
            int(size * 0.02),
            int(size * 0.68),
            int(size * 0.56),
        ),
        fill=(255, 255, 255, 62),
    )
    highlight = highlight.filter(ImageFilter.GaussianBlur(radius=int(size * 0.035)))
    tile.alpha_composite(highlight)
    return tile


def build_master_icon(size: int = BASE_SIZE) -> Image.Image:
    # Blue -> teal theme for education/admin context.
    tile = _build_rounded_tile(size, (14, 71, 161), (20, 184, 166))

    draw = ImageDraw.Draw(tile)

    # Chalkboard frame.
    board_outer = (
        int(size * 0.19),
        int(size * 0.20),
        int(size * 0.81),
        int(size * 0.67),
    )
    board_inner = (
        int(size * 0.23),
        int(size * 0.24),
        int(size * 0.77),
        int(size * 0.63),
    )
    draw.rounded_rectangle(board_outer, radius=int(size * 0.075), fill=(245, 250, 255, 245))
    draw.rounded_rectangle(board_inner, radius=int(size * 0.055), fill=(11, 58, 94, 255))

    # Board writing guides (class management list feel).
    guide_color = (144, 202, 249, 188)
    for i in range(4):
        y = int(size * (0.285 + 0.067 * i))
        draw.line((int(size * 0.28), y, int(size * 0.72), y), fill=guide_color, width=int(size * 0.01))
        draw.ellipse(
            (
                int(size * 0.245),
                y - int(size * 0.012),
                int(size * 0.267),
                y + int(size * 0.012),
            ),
            fill=(226, 248, 255, 230),
        )

    # Teacher desk bar.
    desk_box = (
        int(size * 0.26),
        int(size * 0.70),
        int(size * 0.74),
        int(size * 0.77),
    )
    draw.rounded_rectangle(desk_box, radius=int(size * 0.02), fill=(236, 246, 255, 240))
    leg_w = int(size * 0.03)
    draw.rounded_rectangle(
        (
            int(size * 0.29),
            int(size * 0.76),
            int(size * 0.29) + leg_w,
            int(size * 0.84),
        ),
        radius=int(size * 0.01),
        fill=(236, 246, 255, 215),
    )
    draw.rounded_rectangle(
        (
            int(size * 0.68),
            int(size * 0.76),
            int(size * 0.68) + leg_w,
            int(size * 0.84),
        ),
        radius=int(size * 0.01),
        fill=(236, 246, 255, 215),
    )

    # Badge for "operations done/check".
    badge_outer = (
        int(size * 0.63),
        int(size * 0.63),
        int(size * 0.90),
        int(size * 0.90),
    )
    draw.ellipse(badge_outer, fill=(15, 23, 42, 200))
    badge_inner = (
        int(size * 0.65),
        int(size * 0.65),
        int(size * 0.88),
        int(size * 0.88),
    )
    draw.ellipse(badge_inner, fill=(16, 185, 129, 255))
    _draw_check(draw, badge_inner, (240, 255, 248, 255), width=int(size * 0.035))

    return tile


def build_teacher_dashboard_shortcut_icon(size: int = SHORTCUT_SIZE) -> Image.Image:
    tile = _build_rounded_tile(size, (25, 78, 191), (8, 145, 178))
    draw = ImageDraw.Draw(tile)

    panel = (
        int(size * 0.18),
        int(size * 0.20),
        int(size * 0.82),
        int(size * 0.78),
    )
    draw.rounded_rectangle(panel, radius=int(size * 0.06), fill=(246, 250, 255, 246))
    draw.rounded_rectangle(
        (
            int(size * 0.22),
            int(size * 0.24),
            int(size * 0.78),
            int(size * 0.31),
        ),
        radius=int(size * 0.02),
        fill=(21, 101, 192, 255),
    )

    cards = [
        ((0.25, 0.36, 0.47, 0.54), (37, 99, 235, 255)),
        ((0.53, 0.36, 0.75, 0.54), (14, 165, 233, 255)),
        ((0.25, 0.58, 0.47, 0.73), (16, 185, 129, 255)),
        ((0.53, 0.58, 0.75, 0.73), (99, 102, 241, 255)),
    ]
    for ratio_box, color in cards:
        x0, y0, x1, y1 = ratio_box
        draw.rounded_rectangle(
            (int(size * x0), int(size * y0), int(size * x1), int(size * y1)),
            radius=int(size * 0.022),
            fill=color,
        )

    badge = (int(size * 0.66), int(size * 0.63), int(size * 0.90), int(size * 0.90))
    draw.ellipse(badge, fill=(255, 255, 255, 240))
    _draw_check(draw, badge, (14, 165, 133, 255), width=int(size * 0.03))
    return tile


def build_team_hub_shortcut_icon(size: int = SHORTCUT_SIZE) -> Image.Image:
    tile = _build_rounded_tile(size, (17, 94, 89), (13, 148, 136))
    draw = ImageDraw.Draw(tile)

    nodes = [
        (0.5, 0.30, 0.12, (255, 255, 255, 244)),
        (0.30, 0.66, 0.11, (226, 248, 255, 240)),
        (0.70, 0.66, 0.11, (226, 248, 255, 240)),
    ]
    center = nodes[0]
    for node in nodes[1:]:
        draw.line(
            (
                int(size * center[0]),
                int(size * center[1]),
                int(size * node[0]),
                int(size * node[1]),
            ),
            fill=(214, 250, 240, 235),
            width=int(size * 0.032),
        )
    draw.line(
        (
            int(size * nodes[1][0]),
            int(size * nodes[1][1]),
            int(size * nodes[2][0]),
            int(size * nodes[2][1]),
        ),
        fill=(190, 242, 238, 210),
        width=int(size * 0.024),
    )

    for cx, cy, rr, color in nodes:
        r = int(size * rr)
        draw.ellipse(
            (int(size * cx) - r, int(size * cy) - r, int(size * cx) + r, int(size * cy) + r),
            fill=color,
        )

    hub_ring = (
        int(size * 0.38),
        int(size * 0.18),
        int(size * 0.62),
        int(size * 0.42),
    )
    draw.ellipse(hub_ring, outline=(13, 148, 136, 255), width=int(size * 0.016))
    return tile


def build_yearbook_shortcut_icon(size: int = SHORTCUT_SIZE) -> Image.Image:
    tile = _build_rounded_tile(size, (180, 83, 9), (245, 158, 11))
    draw = ImageDraw.Draw(tile)

    left_page = (
        int(size * 0.20),
        int(size * 0.24),
        int(size * 0.50),
        int(size * 0.80),
    )
    right_page = (
        int(size * 0.50),
        int(size * 0.24),
        int(size * 0.80),
        int(size * 0.80),
    )
    draw.rounded_rectangle(left_page, radius=int(size * 0.03), fill=(255, 251, 235, 247))
    draw.rounded_rectangle(right_page, radius=int(size * 0.03), fill=(255, 248, 230, 247))
    draw.line(
        (int(size * 0.50), int(size * 0.24), int(size * 0.50), int(size * 0.80)),
        fill=(217, 119, 6, 180),
        width=int(size * 0.01),
    )

    top_bar = (int(size * 0.22), int(size * 0.28), int(size * 0.78), int(size * 0.37))
    draw.rounded_rectangle(top_bar, radius=int(size * 0.016), fill=(234, 88, 12, 235))

    for i in range(3):
        y = int(size * (0.46 + 0.1 * i))
        draw.line((int(size * 0.27), y, int(size * 0.44), y), fill=(180, 83, 9, 210), width=int(size * 0.012))
        draw.line((int(size * 0.56), y, int(size * 0.73), y), fill=(180, 83, 9, 210), width=int(size * 0.012))

    bookmark = (
        int(size * 0.66),
        int(size * 0.24),
        int(size * 0.73),
        int(size * 0.47),
    )
    draw.rounded_rectangle(bookmark, radius=int(size * 0.01), fill=(59, 130, 246, 235))
    draw.polygon(
        [
            (int(size * 0.66), int(size * 0.47)),
            (int(size * 0.695), int(size * 0.42)),
            (int(size * 0.73), int(size * 0.47)),
        ],
        fill=(59, 130, 246, 235),
    )
    return tile


def save_preview_strip(master: Image.Image, out_path: Path, sizes: Iterable[int]) -> None:
    sizes = list(sizes)
    gap = 16
    thumb_pad = 18
    width = sum(s + (thumb_pad * 2) for s in sizes) + gap * (len(sizes) + 1)
    height = max(sizes) + (thumb_pad * 2) + 42
    strip = Image.new("RGBA", (width, height), (9, 18, 34, 255))
    draw = ImageDraw.Draw(strip)

    x = gap
    for s in sizes:
        thumb = master.resize((s, s), Image.Resampling.LANCZOS)
        frame = (x, 18, x + s + (thumb_pad * 2), 18 + s + (thumb_pad * 2))
        draw.rounded_rectangle(frame, radius=14, fill=(17, 24, 39, 255), outline=(71, 85, 105, 255), width=2)
        strip.alpha_composite(thumb, (x + thumb_pad, 18 + thumb_pad))
        draw.text((x + thumb_pad, 18 + s + thumb_pad + 6), f"{s}px", fill=(191, 219, 254, 255))
        x += s + (thumb_pad * 2) + gap

    out_path.parent.mkdir(parents=True, exist_ok=True)
    strip.save(out_path)


def save_icon_bundle(master: Image.Image, out_ico: Path, out_png: Path) -> None:
    out_png.parent.mkdir(parents=True, exist_ok=True)
    master.save(out_png, "PNG")

    out_ico.parent.mkdir(parents=True, exist_ok=True)
    master.save(out_ico, format="ICO", sizes=[(s, s) for s in ICO_SIZES])


def _resolve_output_path(raw: str, project_root: Path) -> Path:
    p = Path(raw)
    if p.is_absolute():
        return p
    return project_root / p


def main() -> int:
    parser = argparse.ArgumentParser(description="Generate desktop-shell classroom icon")
    parser.add_argument("--out-ico", default="build/icon.ico", help="Output ICO path")
    parser.add_argument("--out-master", default="build/icon-master-1024.png", help="Output master PNG path")
    parser.add_argument("--out-preview", default="build/icon-preview-strip.png", help="Output preview strip path")
    parser.add_argument("--out-shortcut-dir", default="build/shortcut-icons", help="Output directory for module shortcut icons")
    args = parser.parse_args()

    project_root = Path(__file__).resolve().parent.parent
    out_ico = _resolve_output_path(args.out_ico, project_root)
    out_master = _resolve_output_path(args.out_master, project_root)
    out_preview = _resolve_output_path(args.out_preview, project_root)
    out_shortcut_dir = _resolve_output_path(args.out_shortcut_dir, project_root)

    master = build_master_icon(BASE_SIZE)

    out_master.parent.mkdir(parents=True, exist_ok=True)
    master.save(out_master, "PNG")

    out_ico.parent.mkdir(parents=True, exist_ok=True)
    # Pillow writes ICO with embedded size set from provided list.
    master.save(out_ico, format="ICO", sizes=[(s, s) for s in ICO_SIZES])

    save_preview_strip(master, out_preview, ICO_SIZES)

    shortcut_icons = [
        ("teacher-dashboard", build_teacher_dashboard_shortcut_icon(SHORTCUT_SIZE)),
        ("team-hub", build_team_hub_shortcut_icon(SHORTCUT_SIZE)),
        ("yearbook-index", build_yearbook_shortcut_icon(SHORTCUT_SIZE)),
    ]
    for icon_id, icon_master in shortcut_icons:
        save_icon_bundle(
            icon_master,
            out_shortcut_dir / f"{icon_id}.ico",
            out_shortcut_dir / f"{icon_id}-1024.png",
        )

    print(f"[icon] master:  {out_master}")
    print(f"[icon] ico:     {out_ico}")
    print(f"[icon] preview: {out_preview}")
    for icon_id, _icon_master in shortcut_icons:
        print(f"[icon] shortcut ico: {out_shortcut_dir / f'{icon_id}.ico'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
