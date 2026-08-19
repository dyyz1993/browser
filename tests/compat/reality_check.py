#!/usr/bin/env python3
"""M78.41: 真实站点 CSR 守护——对照基线字节量防回归（SPA 任务成功率主线）。

AGENTS.md：标准正确性看 WPT/Test262，SPA 价值看任务成功率。本脚本守护后者：
标杆站 fetch text 的字节量与基线（reality-baseline.json）对比，偏差超阈值即 FAIL。

用法：python3 tests/compat/reality_check.py [--update]
  --update: 用当前值刷新基线（仅在确认无回归后使用）
"""
import json
import os
import subprocess
import sys

BINARY = os.path.join(os.path.dirname(__file__), "..", "..", "target", "release", "browser")
BASELINE = os.path.join(os.path.dirname(__file__), "results", "reality-baseline.json")
# 站点 → (基线字节, 容差比例)。字节量仅粗粒度哨兵——内容存在性由关键词条目守护。
SITES = {
    "https://example.com/": {"min_bytes": 100, "keyword": "Example Domain"},
    "https://react.dev/": {"min_bytes": 5000, "keyword": "React"},
    "https://vuejs.org/": {"min_bytes": 1000, "keyword": "Vue"},
    "https://svelte.dev/": {"min_bytes": 1500, "keyword": "Svelte"},
}


def fetch_text(url: str) -> str:
    try:
        p = subprocess.run([BINARY, "fetch", url, "--format", "text"],
                           capture_output=True, text=True, timeout=60)
        return p.stdout
    except subprocess.TimeoutExpired:
        return ""


def main() -> int:
    update = "--update" in sys.argv
    baseline = {}
    if os.path.exists(BASELINE):
        baseline = json.load(open(BASELINE))
    failures = []
    print("%-30s %10s %10s %s" % ("站点", "字节", "基线", "关键字"))
    for url, spec in SITES.items():
        text = fetch_text(url)
        n = len(text.encode("utf-8", errors="replace"))
        kw_ok = spec["keyword"].lower() in text.lower()
        base_n = baseline.get(url, {}).get("bytes")
        ok = n >= spec["min_bytes"] and kw_ok
        if base_n and not update:
            # 基线容差 ±40%（站点内容自然波动大，只拦断崖）
            if n < base_n * 0.6:
                ok = False
        print("%-30s %10d %10s %s" % (url, n, base_n or "-", "✓" if kw_ok else "✗"))
        if not ok:
            failures.append(url)
        if update:
            baseline[url] = {"bytes": n}
    if update:
        os.makedirs(os.path.dirname(BASELINE), exist_ok=True)
        json.dump(baseline, open(BASELINE, "w"), indent=1)
        print("基线已更新 →", BASELINE)
        return 0
    if failures:
        print("\nREALITY: FAIL（%d 站回归）: %s" % (len(failures), ", ".join(failures)))
        return 1
    print("\nREALITY: PASS（真实站点 CSR 无回归）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
