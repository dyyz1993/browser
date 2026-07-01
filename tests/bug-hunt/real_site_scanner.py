#!/usr/bin/env python3
"""
M72 真实 SPA 站 Bug 狩猎
=========================
对每个真实 SPA 站点：
  1. Chrome headless (virtual-time-budget=10s) dump-dom → 提取正文文本
  2. 我们的 browser fetch --format text → 提取正文
  3. 逐项对比：文本内容 / JS 报错 / 内容完整性
  4. 输出 BUG 报告

TODO items from user:
- ✅ A15 Object.fromEntries
- Integration with CLI JSON output
- Compare: react.dev, vuejs.org, nuxt.com, svelte.dev, angular.io, remix.run,
  nextjs.org, astro.build, solidjs.com, qwik.dev

用法:
  python3 real_site_scanner.py              # 跑全部站点
  python3 real_site_scanner.py react.dev    # 跑特定站点
"""
import subprocess, sys, re, json, time, os
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
BINARY = str(ROOT / "target/release/browser")
CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
TIMEOUT = 45

SITES = [
    # 纯 CSR 站（无 SSR 兜底，完全依赖 JS）
    ("react.dev",        "https://react.dev/learn"),
    ("vuejs.org",        "https://vuejs.org/guide/introduction.html"),
    ("svelte.dev",       "https://svelte.dev/"),
    ("nuxt.com",         "https://nuxt.com/"),
    ("remix.run",        "https://remix.run/"),
    ("solidjs.com",      "https://solidjs.com/"),
    ("astro.build",      "https://astro.build/"),
    # 框架文档站（SSR + CSR hydrate）
    ("angular.io",       "https://angular.io/"),
    ("nextjs.org",       "https://nextjs.org/docs"),
    ("qwik.dev",         "https://qwik.dev/docs/"),
    ("preactjs.com",     "https://preactjs.com/"),
    ("alpinejs.dev",     "https://alpinejs.dev/start-here"),
    ("mithril.js.org",   "https://mithril.js.org/"),
    # 真实 SPA 应用
    ("cal.com",          "https://cal.com/"),
    ("linear.app",       "https://linear.app/"),
    ("netlify.com",      "https://www.netlify.com/"),
    ("vercel.com",       "https://vercel.com/"),
    ("supabase.com",     "https://supabase.com/"),
    ("prisma.io",        "https://www.prisma.io/"),
]

def run_cmd(cmd, timeout=TIMEOUT):
    """Run a command, return (stdout, stderr, success)."""
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return r.stdout, r.stderr, r.returncode == 0
    except subprocess.TimeoutExpired:
        return "", "TIMEOUT", False
    except Exception as e:
        return "", str(e), False

def chrome_dump_dom(url):
    """Chrome headless dump-dom with virtual-time-budget."""
    stdout, stderr, ok = run_cmd([
        CHROME, "--headless=new", "--disable-gpu", "--no-sandbox",
        "--virtual-time-budget=10000", "--dump-dom", url
    ], timeout=TIMEOUT)
    if not ok:
        return None, stderr[:200]
    # Simple cleanup: remove all script/style tags and their content
    cleaned = re.sub(r'<script[^>]*>.*?</script>', '', stdout, flags=re.DOTALL)
    cleaned = re.sub(r'<style[^>]*>.*?</style>', '', cleaned, flags=re.DOTALL)
    # Remove HTML tags for plain text comparison
    cleaned = re.sub(r'<[^>]+>', ' ', cleaned)
    cleaned = re.sub(r'\s+', ' ', cleaned).strip()
    if len(cleaned) < 50:
        return None, f"Chrome returned only {len(cleaned)} chars of text"
    return cleaned, None

def ours_fetch(url):
    """Our browser fetch --format text"""
    stdout, stderr, ok = run_cmd([
        BINARY, "fetch", url, "--format", "text"
    ], timeout=TIMEOUT+10)
    # Filter debug lines
    lines = []
    for line in stdout.splitlines():
        if line.startswith("[") or line.startswith("Loading"):
            continue
        lines.append(line)
    content = "\n".join(lines).strip()
    
    # Extract JS errors from stderr
    js_errors = []
    for line in stderr.splitlines():
        m = re.search(r'message=(.+?)(\||$)', line)
        if m:
            js_errors.append(m.group(1).strip())
        if 'JS likely broke' in line or 'THREW' in line:
            js_errors.append(line.strip())
    return content, js_errors, stderr

def compare_site(name, url):
    """Compare Chrome vs our browser for a real SPA site."""
    print(f"\n{'='*60}")
    print(f"  {name} ({url})")
    print(f"{'='*60}")
    
    result = {"name": name, "url": url}
    
    # 1. Chrome baseline
    print(f"  [Chrome] dump-dom...", end=" ", flush=True)
    chrome_text, chrome_err = chrome_dump_dom(url)
    if chrome_text is None:
        print(f"❌ {chrome_err}")
        result["status"] = "SKIP"
        result["chrome_error"] = chrome_err
        return result
    print(f"✓ {len(chrome_text)} chars")
    result["chrome_length"] = len(chrome_text)
    result["chrome_preview"] = chrome_text[:200]
    
    # 2. Our fetch
    print(f"  [Ours]  fetch --format text...", end=" ", flush=True)
    our_text, js_errors, stderr = ours_fetch(url)
    if not our_text:
        print(f"❌ empty output")
        result["status"] = "BUG"
        result["our_length"] = 0
        result["js_errors"] = js_errors
        result["detail"] = "empty output"
        return result
    print(f"✓ {len(our_text)} chars")
    result["our_length"] = len(our_text)
    result["our_preview"] = our_text[:200]
    result["js_errors"] = js_errors[:5]  # top 5
    
    # 3. Compare
    content_ratio = len(our_text) / max(len(chrome_text), 1)
    result["content_ratio"] = round(content_ratio, 3)
    
    threshes = []
    if content_ratio < 0.1:
        threshes.append(f"content_critical({content_ratio:.0%})")
    elif content_ratio < 0.5:
        threshes.append(f"content_short({content_ratio:.0%})")
    else:
        threshes.append(f"content_ok({content_ratio:.0%})")
    
    if js_errors:
        threshes.append(f"js_errors({len(js_errors)})")
    else:
        threshes.append("js_errors(0)")
    
    # Check for our specific known error patterns
    error_patterns = []
    for err in js_errors:
        if "is not defined" in err.lower():
            error_patterns.append(f"UNDEF: {err[:60]}")
        elif "not a function" in err.lower() or "not a constructor" in err.lower():
            error_patterns.append(f"NOFN: {err[:60]}")
        elif "failed" in err.lower():
            error_patterns.append(f"FAIL: {err[:60]}")
    
    is_bug = content_ratio < 0.3 or len(js_errors) > 3 or (content_ratio < 0.5 and js_errors)
    
    result["status"] = "BUG" if is_bug else "OK"
    result["detail"] = " | ".join(threshes)
    if error_patterns:
        result["detail"] += " | " + " | ".join(error_patterns[:3])
    
    icon = "🐛" if is_bug else "✅"
    print(f"  {icon} {result['detail']}")
    if error_patterns:
        for ep in error_patterns[:3]:
            print(f"       {ep}")
    
    return result

def main():
    targets = sys.argv[1:] if len(sys.argv) > 1 else [s[1] for s in SITES]
    
    results = []
    for name, url in SITES:
        if any(t in url for t in targets) or name in targets:
            r = compare_site(name, url)
            results.append(r)
    
    # Summary report
    print(f"\n{'='*60}")
    print("  汇总")
    print(f"{'='*60}")
    
    bugs = [r for r in results if r.get("status") == "BUG"]
    skips = [r for r in results if r.get("status") == "SKIP"]
    oks = [r for r in results if r.get("status") == "OK"]
    
    for r in bugs:
        print(f"  🐛 {r['name']:<20} {r.get('detail','')}")
    for r in oks:
        print(f"  ✅ {r['name']:<20} {r.get('detail','')}")
    for r in skips:
        print(f"  ⏭️ {r['name']:<20} {r.get('chrome_error','')}")
    
    print(f"\n  总计: {len(results)} sites | ✅ {len(oks)} | 🐛 {len(bugs)} | ⏭️ {len(skips)}")
    
    # Save detailed report
    report = HERE / "real_site_report.json"
    with open(report, "w") as f:
        json.dump(results, f, indent=2, ensure_ascii=False)
    print(f"  详细报告: {report}")
    
    return bugs

if __name__ == "__main__":
    bugs = main()
    if bugs:
        print(f"\n  ⚠️ 发现 {len(bugs)} 个现实站点 BUG！")
        for b in bugs:
            print(f"    🐛 {b['name']}")
    sys.exit(1 if bugs else 0)
