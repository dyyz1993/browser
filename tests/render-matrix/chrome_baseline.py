#!/usr/bin/env python3
"""跑 Chrome baseline：每档页面用 headless Chrome dump-dom，提取 <pre id="out"> 内容。
这是后续对比的"金标准"。结果存 tests/render-matrix/baseline/。"""
import subprocess, re, os, sys

CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
BASE = "http://127.0.0.1:8765"
OUTDIR = os.path.join(os.path.dirname(__file__), "baseline")

LEVELS = ["07-iframe", "08-fragment", "09-css-selectors", "10-css3",
          "11-canvas", "12-webgl", "13-vdom", "14-performance"]

def chrome_out_text(url):
    """Chrome dump-dom -> 提取 <pre id="out"> 的文本内容"""
    last = Exception("no attempt")
    for _ in range(2):
        try:
            html = subprocess.run(
                [CHROME, "--headless=new", "--virtual-time-budget=6000", "--dump-dom", url],
                capture_output=True, timeout=45
            ).stdout.decode("utf-8", errors="replace")
            if html.strip():
                # 提取 <pre id="out">...</pre> 里的文本（兼容 id 顺序）
                m = re.search(r'<pre[^>]*id="out"[^>]*>(.*?)</pre>', html, re.DOTALL)
                if m:
                    text = m.group(1)
                    text = re.sub(r'<[^>]+>', '', text)  # 去内部标签残留
                    return text.strip()
                # 某些页面 out 在最后，fallback: 找所有 pre
                pres = re.findall(r'<pre[^>]*>(.*?)</pre>', html, re.DOTALL)
                if pres:
                    return re.sub(r'<[^>]+>', '', pres[-1]).strip()
                return "[NO <pre id=out> FOUND]\n" + html[:500]
        except Exception as e:
            last = e
    return f"[CHROME ERROR: {last}]"

def main():
    os.makedirs(OUTDIR, exist_ok=True)
    print("=" * 60)
    print(" Chrome Baseline 抓取（8 档，每档提取 #out 文本）")
    print("=" * 60)
    all_ok = True
    for lvl in LEVELS:
        url = f"{BASE}/{lvl}.html"
        text = chrome_out_text(url)
        path = os.path.join(OUTDIR, f"{lvl}.txt")
        with open(path, "w") as f:
            f.write(text)
        lines = [l for l in text.split('\n') if l.strip()]
        ok = not text.startswith("[") and len(lines) > 0
        all_ok = all_ok and ok
        mark = "✅" if ok else "❌"
        print(f"{mark} {lvl:<20} {len(lines):>4} 行  -> baseline/{lvl}.txt")
    print("=" * 60)
    print("完成。" if all_ok else "⚠️ 有档位抓取失败，见上。")
    return 0 if all_ok else 1

if __name__ == "__main__":
    sys.exit(main())
