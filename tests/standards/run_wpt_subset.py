#!/usr/bin/env python3
"""M97.26d WPT 子集 runner（报告制 v1，非门禁）。

对 tests/standards/wpt-subset.txt 里的每个 wpt harness 测试：
1. 本地 http.server 以 tests/standards/wpt/ 为根起服务
   （测试引用 /resources/testharness.js，必须是根）
2. browser fetch <url> --format html 拿**执行后**的序列化 DOM
3. 解析 testharness 结果表：`<tr class="overall-pass|fail">` 行
4. 汇总通过率 → stdout 表 + GITHUB_STEP_SUMMARY + JSON artifact

v1 明确非门禁（exit 恒 0）：先建立通过率基线趋势，门禁（不回退）
留 v2——第一轮的 fail 数本身就是 DOM 桥的 gap 清单。

用法：
    python3 tests/standards/run_wpt_subset.py --browser target/release/browser
"""
from __future__ import annotations

import http.server
import json
import os
import re
import socketserver
import subprocess
import sys
import threading
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
WPT_ROOT = HERE / "wpt"
MANIFEST = HERE / "wpt-subset.txt"
RESULTS = HERE / "wpt-subset-results.json"
PORT = 18901
PER_TEST_TIMEOUT_S = 45

ROW_RE = re.compile(
    r'<tr class="overall-(pass|fail)"><td class="(?:pass|fail)">[^<]*</td>'
    r'<td>(.{0,200}?)</td>',
    re.S,
)
TAG_RE = re.compile(r"<[^>]+>")


def quiet_handler():
    """静默 http.server 请求日志（favicon 404 等噪声）。"""
    base = http.server.SimpleHTTPRequestHandler

    class Quiet(base):
        def log_message(self, fmt, *args):
            pass

    return Quiet


def load_manifest() -> list[str]:
    paths = []
    for line in MANIFEST.read_text().splitlines():
        line = line.strip()
        if line and not line.startswith("#"):
            paths.append(line)
    return paths


def serve(root: Path, port: int):
    os.chdir(root)
    socketserver.TCPServer.allow_reuse_address = True
    httpd = socketserver.TCPServer(("", port), quiet_handler())
    thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    thread.start()
    return httpd


def run_one(binary: str, path: str) -> dict:
    out = {"path": path, "passed": 0, "failed": 0, "cases": [],
           "harness_error": None}
    try:
        proc = subprocess.run(
            [binary, "fetch", f"http://127.0.0.1:{PORT}/{path}",
             "--format", "html", "--timeout-ms", "30000"],
            capture_output=True, text=True, timeout=PER_TEST_TIMEOUT_S,
        )
    except subprocess.TimeoutExpired:
        out["harness_error"] = "browser 超时"
        return out
    if proc.returncode != 0:
        out["harness_error"] = f"rc={proc.returncode}"
        return out
    html = proc.stdout
    for status, name in ROW_RE.findall(html):
        # <details>「Asserts run」里的 assert 级行第二格是 <pre>，不是用例名
        if name.startswith("<pre"):
            continue
        name = TAG_RE.sub("", name).strip()
        out["cases"].append({"status": status, "name": name})
        if status == "pass":
            out["passed"] += 1
        else:
            out["failed"] += 1
    if out["passed"] + out["failed"] == 0:
        # 结果表为空 = harness 没跑起来或该页 0 用例
        real_err = [l for l in (proc.stderr or "").splitlines()
                    if "Error" in l or "error" in l]
        out["harness_error"] = (real_err[-1][:200] if real_err
                                else "无结果表（harness 未运行或 0 用例）")
    return out


def main() -> int:
    binary = "target/release/browser"
    if "--browser" in sys.argv:
        binary = sys.argv[sys.argv.index("--browser") + 1]
    if not Path(binary).exists():
        sys.exit(f"[wpt] 找不到二进制 {binary}")
    # serve() 会 chdir 到 wpt 根——二进制路径先转绝对
    binary = str(Path(binary).resolve())

    tests = load_manifest()
    missing = [t for t in tests if not (WPT_ROOT / t).exists()]
    if missing:
        sys.exit(f"[wpt] manifest 引用了不存在的文件: {missing}")

    httpd = serve(WPT_ROOT, PORT)
    time.sleep(0.5)
    rows = [run_one(binary, t) for t in tests]
    httpd.shutdown()

    total_pass = sum(r["passed"] for r in rows)
    total_fail = sum(r["failed"] for r in rows)
    errors = [r for r in rows if r["harness_error"]]

    lines = ["| 测试文件 | 用例 | 通过 | 失败 | 备注 |", "|---|---|---|---|---|"]
    for r in rows:
        note = r["harness_error"] or ""
        lines.append(f"| `{r['path']}` | {r['passed'] + r['failed']} | "
                     f"{r['passed']} | {r['failed']} | {note} |")
    rate = total_pass / (total_pass + total_fail) if total_pass + total_fail else 0.0
    table = "\n".join(lines)
    headline = (f"**WPT 子集通过率: {total_pass}/{total_pass + total_fail} "
                f"({rate:.1%})** — harness 加载失败 {len(errors)} 个文件")
    print(headline)
    print(table)

    summary_path = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary_path:
        with open(summary_path, "a") as f:
            f.write("## WPT 子集（报告制 v1）\n\n" + headline + "\n\n" + table + "\n")

    RESULTS.write_text(json.dumps(
        {"total_pass": total_pass, "total_fail": total_fail, "rate": rate,
         "files": rows}, indent=2, ensure_ascii=False) + "\n")
    print(f"\n[wpt] 结果已写 {RESULTS}")
    # v1 报告制：恒 exit 0（harness 加载失败也只上报，避免夜间噪声红）
    return 0


if __name__ == "__main__":
    sys.exit(main())
