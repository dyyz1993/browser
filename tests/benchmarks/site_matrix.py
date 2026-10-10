#!/usr/bin/env python3
"""M97.26c 真实站点矩阵（夜间冒烟）。

对一组「基建稳定」的站点跑 browser fetch --json，分级断言：
- HARD（失败即 exit 1）：进程 rc!=0 / 超时 / 正文少于阈值
- SOFT（只报告不拦）：uncaught errors / console.error 计数——真实站点
  改版引入 JS 报错很常见，作为趋势信号而非门禁

站点选型原则：基建方稳定（GitHub Pages/Netlify/typicode），避开风控站
（juejin/cls 走 FEATURES 已知局限条目，不进矩阵）。

用法：
    python3 tests/benchmarks/site_matrix.py --browser target/release/browser
结果（含逐站 JSON 摘要）写到 tests/benchmarks/site-matrix-results.json，
并支持 GITHUB_STEP_SUMMARY 渲染（CI 内自动生效）。
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
RESULTS = HERE / "site-matrix-results.json"
FETCH_TIMEOUT_MS = 30000

# (名称, URL, 正文最小字节数, 类型)
# 选型实测记录（2026-10-10，本地快测）：jsonplaceholder 触发「response is
# not valid UTF-8」（疑似 zstd/编码解码缺口，后续单独修）；hnpwa-vue3 已 404；
# demo.realworld.io / ourworldindata 本地 0B——均不入选。
SITES = [
    ("example.com", "https://example.com/", 100, "static"),
    ("todomvc", "https://todomvc.com/", 500, "js"),
    ("learn-svelte", "https://learn.svelte.dev/", 800, "csr"),
    ("caniuse", "https://caniuse.com/", 800, "csr"),
    ("npmtrends", "https://npmtrends.com/", 300, "csr"),
]


def check_site(binary: str, name: str, url: str, min_bytes: int, kind: str) -> dict:
    row = {"name": name, "url": url, "kind": kind, "hard_fail": None, "errors": None,
           "console": None, "content_len": None, "elapsed_s": None}
    start = time.perf_counter()
    try:
        proc = subprocess.run(
            [binary, "fetch", url, "--json", "--timeout-ms", str(FETCH_TIMEOUT_MS)],
            capture_output=True, text=True, timeout=FETCH_TIMEOUT_MS / 1000 + 30,
        )
    except subprocess.TimeoutExpired:
        row["hard_fail"] = f"进程超时（>{FETCH_TIMEOUT_MS / 1000 + 30:.0f}s）"
        return row
    row["elapsed_s"] = round(time.perf_counter() - start, 1)
    if proc.returncode != 0:
        tail = (proc.stderr or "").strip().splitlines()[-1:] or ["(no stderr)"]
        row["hard_fail"] = f"rc={proc.returncode}: {tail[0][:200]}"
        return row
    try:
        data = json.loads(proc.stdout)
    except json.JSONDecodeError:
        row["hard_fail"] = "--json 输出不可解析（前 200 字节）: " + proc.stdout[:200]
        return row
    c = data.get("content")
    # M97.20 --json 的 content 是 {format, text} 投影对象
    content = (c.get("text") if isinstance(c, dict) else c) or ""
    content = content.strip()
    row["content_len"] = len(content)
    row["errors"] = len(data.get("errors") or [])
    row["console"] = len(data.get("console") or [])
    if len(content) < min_bytes:
        row["hard_fail"] = f"正文 {len(content)}B < 阈值 {min_bytes}B"
    return row


def summary_md(rows: list[dict]) -> str:
    lines = ["| 站点 | 类型 | 耗时 | 正文 | uncaught | console | 结果 |",
             "|------|------|------|------|----------|---------|------|"]
    for r in rows:
        verdict = "❌ " + r["hard_fail"] if r["hard_fail"] else "✅"
        lines.append(
            f"| {r['name']} | {r['kind']} | {r['elapsed_s']}s | "
            f"{r['content_len']}B | {r['errors']} | {r['console']} | {verdict} |"
        )
    return "\n".join(lines)


def main() -> int:
    binary = None
    for cand in ("target/release/browser", "target/debug/browser"):
        if Path(cand).exists():
            binary = cand
            break
    if "--browser" in sys.argv:
        binary = sys.argv[sys.argv.index("--browser") + 1]
    if not binary:
        sys.exit("[site-matrix] 找不到二进制：先 cargo build --release -p browser-cli")

    rows = [check_site(binary, *s) for s in SITES]
    hard = [r for r in rows if r["hard_fail"]]

    RESULTS.write_text(json.dumps(rows, indent=2, ensure_ascii=False) + "\n")
    print(summary_md(rows))
    summary_path = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary_path:
        with open(summary_path, "a") as f:
            f.write("## 站点矩阵（夜间冒烟）\n\n" + summary_md(rows) + "\n")

    print(f"\n[site-matrix] HARD 失败 {len(hard)}/{len(rows)}，结果已写 {RESULTS}")
    return 1 if hard else 0


if __name__ == "__main__":
    sys.exit(main())
