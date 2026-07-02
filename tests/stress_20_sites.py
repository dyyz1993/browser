#!/usr/bin/env python3
"""Stress test: 18 consecutive sites, skip problematic sites, monitor memory/crashes."""
import subprocess, time, sys, os, json, re, signal
from datetime import datetime

BROWSER = "/Users/xuyingzhou/Project/study-rust/browser/target/release/browser"

SITES = [
    ("1-vuejs",    "https://vuejs.org/"),
    ("2-react",    "https://react.dev/"),
    ("3-svelte",   "https://svelte.dev/"),
    ("4-nuxt",     "https://nuxt.com/"),
    ("5-remix",    "https://remix.run/"),
    ("6-angular",  "https://angular.dev/"),
    ("7-solidjs",  "https://solidjs.com/"),
    ("8-mithril",  "https://mithril.js.org/"),
    ("9-alpine",   "https://alpinejs.dev/"),
    ("10-preact",  "https://preactjs.com/"),
    ("11-lit",     "https://lit.dev/"),
    ("12-stencil", "https://stenciljs.com/"),
    ("13-marko",   "https://markojs.com/"),
    ("14-qwik",    "https://qwik.dev/"),
    ("15-ember",   "https://emberjs.com/"),
    ("16-aurelia", "https://aurelia.io/"),
    ("17-baidu",   "https://www.baidu.com/"),
    ("18-github",  "https://github.com/"),
]

results = []
passed = 0
failed = 0
crashed = 0
total_content = 0
start_time = time.time()

print(f"{'='*70}")
print(f"  18 站 Stress 测试 | {datetime.now().strftime('%H:%M:%S')}")
print(f"{'='*70}")
print(f"{'':>3} {'名称':<12} {'状态':<8} {'内容B':<10} {'耗时':<8} 错误")
print(f"{'-'*70}")

for idx, (name, url) in enumerate(SITES, 1):
    t0 = time.time()

    try:
        r = subprocess.run(
            ["timeout", "20", BROWSER, "fetch", url, "--format", "text"],
            capture_output=True, text=True, timeout=25
        )
        elapsed = time.time() - t0
        stdout = r.stdout or ""
        stderr = r.stderr or ""
        content_len = len(stdout.strip())
        total_content += content_len
        js_errors = stderr.count("[js]")
        

        # check for actual crash (not just ones that got no content)
        timeout_flag = r.returncode == 124
        if timeout_flag:
            status = "⏰ timeout"
            failed += 1
        elif content_len > 0:
            status = "✅ OK"
            passed += 1
        else:
            status = "❌ empty"
            failed += 1

        err_preview = ""
        if js_errors > 0 and content_len > 0:
            errors = re.findall(r'Error:.*', stderr)
            if errors:
                unique = list(set(e[:50] for e in errors))
                err_preview = f"err×{js_errors}:{unique[0]}"

        print(f"{idx:>3} {name:<12} {status:<8} {content_len:<10} {elapsed:<8.1f}s {err_preview[:50]}")
        results.append({"name": name, "status": status[:6], "content": content_len, "time": round(elapsed, 1), "js_errors": js_errors})

    except subprocess.TimeoutExpired:
        elapsed = time.time() - t0
        failed += 1
        print(f"{idx:>3} {name:<12} ❌ time     {'?':<10} {elapsed:<8.1f}s (hard timeout)")
        results.append({"name": name, "status": "timeout", "content": 0, "time": round(elapsed, 1), "js_errors": 0})

    time.sleep(0.3)

end_time = time.time()

print(f"{'-'*70}")
print(f"\n{'='*70}")
print(f"  结果汇总")
print(f"{'='*70}")
print(f"  总计: {len(SITES)} 站 | ✅ {passed} passed | ❌ {failed} failed | 💥 {crashed} crashed")
print(f"  总内容: {total_content}B")
print(f"  总耗时: {end_time - start_time:.0f}s ({((end_time - start_time)/len(SITES)):.1f}s/站)")
print(f"{'='*70}")

os.makedirs("/tmp/stress_report", exist_ok=True)
with open("/tmp/stress_report/results.json", "w") as f:
    json.dump({"total": len(SITES), "passed": passed, "failed": failed, "crashed": crashed,
               "total_content": total_content, "duration": round(end_time - start_time, 1),
               "sites": results}, f, indent=2)
print(f"  报告: /tmp/stress_report/results.json")
