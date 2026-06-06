#!/usr/bin/env python3
"""Generate side-by-side Safari vs our-browser comparison images.

Usage:
  cd <project-root>
  python3 tests/snapshots/compare.py

Outputs:
  tests/snapshots/demo-safari.png       Safari view (Quick Look = WebKit)
  tests/snapshots/demo-ours.txt         Our render-file ASCII
  tests/snapshots/demo-compare.png      Side-by-side composite
  tests/snapshots/example-safari.png
  tests/snapshots/example-ours.txt
  tests/snapshots/example-compare.png

This is the M6.0d acceptance artifact: proves the M6.0a/b/c fixes
narrowed the gap between our pure-Rust renderer and the system
WebKit renderer on the same HTML fixtures.

Dependencies: Pillow. The script degrades gracefully (with a clear
message) if Pillow is missing.
"""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

try:
    from PIL import Image, ImageDraw, ImageFont
except ImportError:
    sys.exit(
        "Pillow is required: pip3 install Pillow\n"
        "(This is a dev tool, not a runtime dependency of the browser.)"
    )


PROJECT_ROOT = Path(__file__).resolve().parent.parent.parent
SNAPSHOT_DIR = Path(__file__).resolve().parent


def run(cmd: list[str], **kw) -> subprocess.CompletedProcess[str]:
    """Run cmd, raise on failure with the captured output."""
    return subprocess.run(cmd, check=True, capture_output=True, text=True, **kw)


def get_font(size: int) -> ImageFont.FreeTypeFont | ImageFont.ImageFont:
    """Pick a system monospace font; fall back to PIL default."""
    candidates = [
        "/System/Library/Fonts/Menlo.ttc",
        "/System/Library/Fonts/Courier.ttc",
        "/System/Library/Fonts/SFNSMono.ttf",
        "/Library/Fonts/Menlo.ttc",
        "/System/Library/Fonts/Helvetica.ttc",
    ]
    for c in candidates:
        if Path(c).exists():
            try:
                return ImageFont.truetype(c, size)
            except Exception:
                continue
    return ImageFont.load_default()


def render_ascii_to_png(text: str, out_path: Path, font_size: int = 14,
                        padding: int = 24) -> None:
    """Render ASCII text to a PNG with a gray background."""
    lines = text.splitlines() if text else [""]
    longest = min(max((len(line) for line in lines), default=80), 200)
    font = get_font(font_size)
    try:
        bbox = font.getbbox("M")
        char_w = bbox[2] - bbox[0]
        char_h = bbox[3] - bbox[1] + int(font_size * 0.6)
    except Exception:
        char_w = font_size // 2 + 2
        char_h = font_size + 6

    line_h = char_h
    img_w = padding * 2 + longest * char_w
    img_h = padding * 2 + len(lines) * line_h
    img = Image.new("RGB", (img_w, img_h), (245, 245, 245))
    draw = ImageDraw.Draw(img)
    for i, line in enumerate(lines):
        y = padding + i * line_h
        draw.text((padding, y), line, font=font, fill=(20, 20, 20))
    img.save(out_path)


def make_comparison(label: str, safari_png: Path, our_text: str,
                    out_png: Path, title: str) -> None:
    """Side-by-side composite: safari left, our render right."""
    our_png = out_png.with_suffix(".tmp_ours.png")
    render_ascii_to_png(our_text, our_png)

    safari = Image.open(safari_png).convert("RGB")
    ours = Image.open(our_png).convert("RGB")

    target_h = 800

    def resize_to_h(im: Image.Image, h: int) -> Image.Image:
        w = int(im.width * h / im.height)
        return im.resize((w, h), Image.LANCZOS)

    safari_r = resize_to_h(safari, target_h)
    ours_r = resize_to_h(ours, target_h)

    margin = 30
    label_h = 60
    total_w = safari_r.width + ours_r.width + margin * 3
    total_h = target_h + label_h * 2 + margin
    canvas = Image.new("RGB", (total_w, total_h), (255, 255, 255))
    draw = ImageDraw.Draw(canvas)

    title_font = get_font(28)
    label_font = get_font(22)
    footer_font = get_font(16)

    draw.text((margin, 16), title, font=title_font, fill=(0, 0, 0))
    draw.text((margin, label_h + 8),
              "Safari (Quick Look — WebKit 同源)",
              font=label_font, fill=(0, 100, 0))
    canvas.paste(safari_r, (margin, label_h * 2))

    rx = margin + safari_r.width + margin
    draw.text((rx, label_h + 8),
              "我们的 browser CLI (纯 Rust 自研)",
              font=label_font, fill=(100, 0, 0))
    canvas.paste(ours_r, (rx, label_h * 2))

    div_x = margin + safari_r.width + margin // 2
    draw.line([(div_x, label_h * 2), (div_x, label_h * 2 + target_h)],
              fill=(180, 180, 180), width=2)

    footer_y = label_h * 2 + target_h + 8
    draw.text((margin, footer_y),
              "差异说明：Safari 用富文本排版 + 字体；我们输出 ASCII 文本流（爬虫可直接 grep）",
              font=footer_font, fill=(80, 80, 80))

    canvas.save(out_png)
    our_png.unlink(missing_ok=True)


def assert_tool(name: str) -> None:
    if shutil.which(name) is None:
        sys.exit(f"required tool not found: {name}")


def main() -> None:
    assert_tool("qlmanage")
    assert_tool("cargo")

    # 1. demo.html (static)
    demo_html = PROJECT_ROOT / "tests/fixtures/demo.html"
    if not demo_html.exists():
        sys.exit(f"missing fixture: {demo_html}")

    safari_demo = SNAPSHOT_DIR / "demo-safari.png"
    if safari_demo.exists():
        safari_demo.unlink()
    run(["qlmanage", "-t", "-s", "1280", "-o", str(SNAPSHOT_DIR), str(demo_html)])
    # qlmanage writes demo.html.png next to the input; rename.
    produced = SNAPSHOT_DIR / "demo.html.png"
    produced.rename(safari_demo)

    our_demo_txt = SNAPSHOT_DIR / "demo-ours.txt"
    cargo_bin = PROJECT_ROOT / "target/debug/browser"
    run(["cargo", "build", "-p", "browser-cli"],
        cwd=str(PROJECT_ROOT))
    with our_demo_txt.open("w") as f:
        subprocess.run(
            [str(cargo_bin), "render-file", str(demo_html), "--width", "80"],
            check=True,
            stdout=f,
            cwd=str(PROJECT_ROOT),
        )

    make_comparison(
        label="demo.html",
        safari_png=safari_demo,
        our_text=our_demo_txt.read_text(),
        out_png=SNAPSHOT_DIR / "demo-compare.png",
        title="对比 1: tests/fixtures/demo.html (静态 + M6.0a/b/c 修复后)",
    )

    # 2. example.com (real site)
    example_html = SNAPSHOT_DIR / "example.com.html"
    run(["curl", "-s", "-o", str(example_html),
         "https://example.com/"])

    safari_example = SNAPSHOT_DIR / "example-safari.png"
    if safari_example.exists():
        safari_example.unlink()
    run(["qlmanage", "-t", "-s", "1280", "-o", str(SNAPSHOT_DIR),
         str(example_html)])
    produced = SNAPSHOT_DIR / "example.com.html.png"
    produced.rename(safari_example)

    our_example_txt = SNAPSHOT_DIR / "example-ours.txt"
    with our_example_txt.open("w") as f:
        subprocess.run(
            [str(cargo_bin), "render-url",
             "https://example.com/", "--width", "100", "--no-js"],
            check=True,
            stdout=f,
            cwd=str(PROJECT_ROOT),
        )

    make_comparison(
        label="example.com",
        safari_png=safari_example,
        our_text=our_example_txt.read_text(),
        out_png=SNAPSHOT_DIR / "example-compare.png",
        title="对比 2: https://example.com/ (真实站点 + M6.0a/b/c 修复后)",
    )

    example_html.unlink()  # don't commit a downloaded HTML

    print("✅ Snapshot artifacts regenerated:")
    for f in sorted(SNAPSHOT_DIR.iterdir()):
        if f.is_file() and not f.name.startswith("."):
            print(f"   {f.relative_to(PROJECT_ROOT)}")


if __name__ == "__main__":
    main()
