#!/usr/bin/env python3
"""生成 jpeg_fixtures.txt 的渐进式（SOF2）夹具矩阵（车道AJ2）。

格式与车道AA 一致：每行 `key:jpg_b64,ref_b64`。
- jpg = PIL 编码的 JPEG 字节
- ref = PIL 解码同一文件的 RGBA8（逐像素预言机，预算 ±1/±2 LSB）

夹具矩阵（渐进式覆盖面）：
- pg8   灰度 16×16 q75        —— DC(Al=1) + AC 分带(1-5/6-63) + DC refine + AC refine
- pc444 彩色 16×16 q80 4:4:4  —— 全 10 扫描脚本
- pc420 彩色 32×32 q60 4:2:0  —— 交织 DC + 单分量 AC（EOB 游程）
- pc422 彩色 32×16 q60 4:2:2  —— h2v1 + 多 MCU 行
- podd  彩色 13×7  q70 4:2:0  —— 奇数尺寸 MCU 填充
- pdri  灰度 24×24 q75 + DRI  —— 渐进式 × 重启间隔（RST 重同步 + EOBRUN 复位）
- plq   彩色 64×48 q10 4:2:0  —— 低质量长 EOB 游程 + 粗 Al 位面

幂等：已存在的 key 跳过；`--ref key` 只回填某行的 ref（jpg 字节不动）。
基线夹具（g8/c444/c420/c422/dri/odd53/px1/prog）由车道AA ad-hoc 生成，
本脚本不重生成——PIL 版本差异会改变字节，破坏既有基线锚定。
"""

import base64
import io
import sys

from PIL import Image, ImageDraw

FIXTURES = "jpeg_fixtures.txt"


def make_rgb(w, h):
    im = Image.new("RGB", (w, h))
    dr = ImageDraw.Draw(im)
    for y in range(h):
        for x in range(w):
            dr.point((x, y), fill=(((x * 255) // max(w - 1, 1)) % 256,
                                   ((y * 255) // max(h - 1, 1)) % 256,
                                   (((x + y) * 255) // max(w + h - 2, 1)) % 256))
    dr.ellipse((w // 6, h // 6, w - w // 6, h - h // 6), fill=(255, 128, 0))
    dr.ellipse((w // 3, h // 3, w - w // 3, h - h // 3), fill=(0, 200, 80))
    return im


def make_gray(w, h):
    im = Image.new("L", (w, h))
    dr = ImageDraw.Draw(im)
    for y in range(h):
        for x in range(w):
            dr.point((x, y), fill=(x * 7 + y * 11) % 256)
    dr.rectangle((w // 4, h // 4, w - w // 4, h - h // 4), fill=250)
    return im


def encode(im, **kw):
    buf = io.BytesIO()
    im.save(buf, "JPEG", **kw)
    return buf.getvalue()


def ref_of(jpg):
    im = Image.open(io.BytesIO(jpg)).convert("RGBA")
    return base64.b64encode(im.tobytes()).decode()


PROG_MATRIX = [
    ("pg8", make_gray(16, 16), dict(quality=75, progressive=True)),
    ("pc444", make_rgb(16, 16), dict(quality=80, progressive=True, subsampling=0)),
    ("pc420", make_rgb(32, 32), dict(quality=60, progressive=True, subsampling=2)),
    ("pc422", make_rgb(32, 16), dict(quality=60, progressive=True, subsampling=1)),
    ("podd", make_rgb(13, 7), dict(quality=70, progressive=True, subsampling=2)),
    ("pdri", make_gray(24, 24),
     dict(quality=75, progressive=True, restart_marker_rows=1)),
    ("plq", make_rgb(64, 48), dict(quality=10, progressive=True, subsampling=2)),
]


def main():
    lines = open(FIXTURES).read().splitlines()
    have = {ln.split(":", 1)[0] for ln in lines if ":" in ln}
    for key, im, kw in PROG_MATRIX:
        if key in have:
            print(f"skip {key} (exists)")
            continue
        jpg = encode(im, **kw)
        lines.append(f"{key}:{base64.b64encode(jpg).decode()},{ref_of(jpg)}")
        print(f"append {key} ({len(jpg)} bytes jpg)")
    open(FIXTURES, "w").write("\n".join(lines) + "\n")

    # 回填模式：--ref key —— jpg 不动，仅补 ref（渐进式解码落地前的占位行）
    if len(sys.argv) == 3 and sys.argv[1] == "--ref":
        key = sys.argv[2]
        out = []
        for ln in lines:
            if ln.startswith(key + ":") and ln.endswith(",-"):
                jpg = base64.b64decode(ln.split(":", 1)[1].rsplit(",", 1)[0])
                out.append(f"{ln.rsplit(',', 1)[0]},{ref_of(jpg)}")
                print(f"backfill ref {key}")
            else:
                out.append(ln)
        open(FIXTURES, "w").write("\n".join(out) + "\n")


if __name__ == "__main__":
    main()
