#!/usr/bin/env python3
"""M97.26b 性能/内存基线门禁。

- workload：本地确定性 HTML（零网络）——layout-heavy(解析+布局) /
  dom-heavy(DOM 桥) / js-heavy(QuickJS 计算)
- 每个 workload 跑 N 次：wall 中位数 + 子进程 peak RSS（独立 wrapper
  进程计量，getrusage(RUSAGE_CHILDREN) 只含单个子进程才准确）
- 与 tests/benchmarks/perf-baseline.json 对比：wall > WALL_TOLERANCE 倍
  或 RSS > RSS_TOLERANCE 倍 → exit 1。CI 共享 runner 噪声大，容差只拦
  「灾难级回退」（如误改引擎配置、内存泄漏），不做绝对性能比较
- --update 重写基线（供 CI 首跑自建 / 本地基准漂移后刷新，随后 commit）

用法：
    python3 tests/benchmarks/perf_bench.py                # 门禁模式
    python3 tests/benchmarks/perf_bench.py --update       # 生成/刷新基线
    python3 tests/benchmarks/perf_bench.py --browser PATH
"""
# 3.9 兼容：注解里的 tuple[...] / X | Y 不在运行期求值
from __future__ import annotations

import argparse
import json
import platform
import statistics
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
BASELINE = HERE / "perf-baseline.json"

# (名称, CLI 子命令, 文件) —— 顺序无关，输出按名称对齐基线键
WORKLOADS = [
    ("layout-heavy", "render-file",   HERE / "workloads/layout-heavy.html"),
    ("dom-heavy",    "render-script", HERE / "workloads/dom-heavy.html"),
    ("js-heavy",     "render-script", HERE / "workloads/js-heavy.html"),
]
N_RUNS = 5
WALL_TOLERANCE = 2.0   # 中位耗时倍数上限
RSS_TOLERANCE = 1.3    # 峰值 RSS 倍数上限


def one_run(binary: str, cmd: str, path: Path) -> tuple[float, float] | None:
    """跑一次，返回 (wall_ms, rss_kb)。独立 wrapper 进程保证 RSS 只含本子进程。"""
    code = (
        "import subprocess,sys,time,resource;"
        "t=time.perf_counter();"
        "subprocess.run(sys.argv[1:],capture_output=True);"
        "print(time.perf_counter()-t);"
        "print(resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss)"
    )
    try:
        out = subprocess.run(
            [sys.executable, "-c", code, binary, cmd, str(path)],
            capture_output=True, text=True, timeout=300,
        )
    except subprocess.TimeoutExpired:
        return None
    if out.returncode != 0:
        return None
    lines = out.stdout.split()
    if len(lines) < 2:
        return None
    wall_ms = float(lines[0]) * 1000.0
    rss = float(lines[1])
    # Linux ru_maxrss=KB，macOS=字节 → 统一 KB
    if platform.system() == "Darwin":
        rss /= 1024.0
    return wall_ms, rss


def measure(binary: str) -> dict:
    results = {}
    for name, cmd, path in WORKLOADS:
        runs = [one_run(binary, cmd, path) for _ in range(N_RUNS)]
        bad = sum(1 for r in runs if r is None)
        if bad == N_RUNS:
            sys.exit(f"[perf] workload {name} 全部失败——二进制不可用？({binary})")
        ok = [r for r in runs if r is not None]
        results[name] = {
            "median_ms": round(statistics.median(r[0] for r in ok), 1),
            "max_rss_kb": round(max(r[1] for r in ok), 0),
            "runs_ok": f"{len(ok)}/{N_RUNS}",
        }
    return results


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--update", action="store_true", help="重写基线文件")
    ap.add_argument("--browser", default=None, help="browser 二进制路径")
    ap.add_argument("--baseline", default=str(BASELINE))
    args = ap.parse_args()

    binary = args.browser
    if binary is None:
        cand = [ROOT / "target/release/browser", ROOT / "target/debug/browser"]
        binary = next((str(c) for c in cand if c.exists()), None)
        if binary is None:
            sys.exit("[perf] 找不到二进制：先 cargo build --release -p browser-cli")
    baseline_path = Path(args.baseline)

    results = measure(binary)
    print(f"{'workload':<14} {'median_ms':>10} {'max_rss_kb':>11}  runs")
    for name, r in results.items():
        print(f"{name:<14} {r['median_ms']:>10} {r['max_rss_kb']:>11}  {r['runs_ok']}")

    if args.update:
        payload = {
            "_meta": {
                "note": "CI 自动维护（perf.yml）；本地机器数字与 CI runner 不可比，勿手改",
                "updated": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                "platform": f"{platform.system()}-{platform.machine()}",
            },
            "workloads": results,
        }
        baseline_path.write_text(json.dumps(payload, indent=2, ensure_ascii=False) + "\n")
        print(f"[perf] 基线已写入 {baseline_path}")
        return 0

    if not baseline_path.exists():
        print(f"[perf] 基线不存在（{baseline_path}）——报告模式，本次不做门禁。")
        print("[perf] CI 首跑会自动建立基线并 commit；本地验证可加 --update。")
        return 0

    base = json.loads(baseline_path.read_text())["workloads"]
    violations = []
    for name, r in results.items():
        b = base.get(name)
        if b is None:
            violations.append(f"{name}: 基线缺 workload（跑 --update 重建）")
            continue
        if r["median_ms"] > b["median_ms"] * WALL_TOLERANCE:
            violations.append(
                f"{name}: wall {r['median_ms']}ms > {WALL_TOLERANCE}x 基线 {b['median_ms']}ms"
            )
        if r["max_rss_kb"] > b["max_rss_kb"] * RSS_TOLERANCE:
            violations.append(
                f"{name}: RSS {r['max_rss_kb']:.0f}KB > {RSS_TOLERANCE}x 基线 {b['max_rss_kb']:.0f}KB"
            )

    if violations:
        print("\n[perf] ❌ 性能回退（超过容差）：")
        for v in violations:
            print(f"  - {v}")
        return 1
    print("\n[perf] ✅ 全部 workload 在容差内")
    return 0


if __name__ == "__main__":
    sys.exit(main())
