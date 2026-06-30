#!/usr/bin/env python3
"""公平对比：Chrome dump-dom(文本提取) vs 我们 fetch --format text。
两者都走 http://localhost:8765/<file>，都是爬虫真实路径。"""
import subprocess, re, difflib

BIN = "/Users/xuyingzhou/Project/study-rust/browser-work-m71.1/target/release/browser"
CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
BASE = "http://127.0.0.1:8765"

LEVELS = [
    ("01-basic",            "基础静态HTML（无JS无CSS）"),
    ("02-js-dom",           "JS改DOM（基础CSR，无框架）"),
    ("03-es6-advanced",     "ES6+/async/Web API（测引擎能力）"),
    ("04-spa-framework",    "SPA框架模式（组件/状态/事件）"),
    ("05-super-complex",    "超复杂（大数据表格+卡片网格）"),
    ("06-edge-malformed",   "边界畸形（未闭合/嵌套错误/emoji）"),
]

def clean_text(s):
    s = re.sub(r'&#\d+;', '', s)
    s = re.sub(r'\xa0', ' ', s)
    s = re.sub(r'[ \t]+', ' ', s)
    lines = [l.strip() for l in s.split('\n')]
    return '\n'.join(l for l in lines if l.strip())

def strip_html_to_text(html):
    """HTML 字符串 -> 可见纯文本（去 script/style/head/svg/标签）"""
    html = re.sub(r'<head[^>]*>.*?</head>', '', html, flags=re.DOTALL|re.IGNORECASE)
    html = re.sub(r'<script[^>]*>.*?</script>', '', html, flags=re.DOTALL|re.IGNORECASE)
    html = re.sub(r'<noscript[^>]*>.*?</noscript>', '', html, flags=re.DOTALL|re.IGNORECASE)
    html = re.sub(r'<style[^>]*>.*?</style>', '', html, flags=re.DOTALL|re.IGNORECASE)
    html = re.sub(r'<svg[^>]*>.*?</svg>', '', html, flags=re.DOTALL|re.IGNORECASE)
    html = re.sub(r'<!--.*?-->', '', html, flags=re.DOTALL)
    html = re.sub(r'</?(p|div|li|h[1-6]|tr|td|th|br|ul|ol|table|thead|tbody|header|footer|nav|main|section|article|span|option|select|form|button|label)\b[^>]*>', '\n', html, flags=re.IGNORECASE)
    html = re.sub(r'<[^>]+>', ' ', html)
    return clean_text(html)

def chrome_text(url):
    last = Exception("no attempt")
    for _ in range(2):
        try:
            raw = subprocess.run(
                [CHROME, "--headless=new", "--virtual-time-budget=5000", "--dump-dom", url],
                capture_output=True, timeout=45
            ).stdout.decode("utf-8", errors="replace")
            if raw.strip():
                return strip_html_to_text(raw)
        except Exception as e:
            last = e
    return f"[CHROME ERROR: {last}]"

def ours_text(url):
    raw = subprocess.run(
        [BIN, "fetch", url, "--format", "text"],
        capture_output=True, timeout=30
    ).stdout.decode("utf-8", errors="replace")
    raw = re.sub(r'\x1b\[[0-9;]*m', '', raw)  # 去 ANSI
    return clean_text(raw)

def main():
    print("=" * 64)
    print(" 渐进复杂度渲染对比: Chrome 149 vs 我们 (QuickJS 9.3M)")
    print(" 路径: 都走 http fetch | Chrome=dump-dom提取 | Ours=fetch text")
    print("=" * 64)

    results = []
    for lvl, desc in LEVELS:
        url = f"{BASE}/{lvl}.html"
        c = chrome_text(url)
        o = ours_text(url)
        ratio = difflib.SequenceMatcher(None, c, o).ratio()
        cl = [l for l in c.split('\n') if l.strip()]
        ol = [l for l in o.split('\n') if l.strip()]

        print(f"\n{'═'*62}")
        print(f"档位 {lvl}  ({desc})")
        print(f"相似度: {ratio*100:.0f}%   Chrome={len(cl)}行  Ours={len(ol)}行")
        print(f"{'─'*62}")
        maxn = min(max(len(cl), len(ol)), 10)
        for i in range(maxn):
            ci = cl[i] if i < len(cl) else ""
            oi = ol[i] if i < len(ol) else ""
            mark = "✓" if ci.strip() == oi.strip() else "✗"
            ci = (ci[:48] + '..') if len(ci) > 50 else ci
            oi = (oi[:48] + '..') if len(oi) > 50 else oi
            print(f" {mark} C|{ci}")
            if ci.strip() != oi.strip():
                print(f"   O|{oi}")
        results.append((lvl, ratio, len(cl), len(ol)))

    print(f"\n{'='*62}")
    print(f"{'档位':<22}{'相似度':>8}{'Chrome':>9}{'Ours':>7}")
    print(f"{'-'*62}")
    tot = 0
    for lvl, r, c, o in results:
        print(f"{lvl:<22}{r*100:>7.0f}%{c:>9}{o:>7}")
        tot += r
    print(f"{'-'*62}")
    print(f"{'平均':<22}{tot/len(results)*100:>7.0f}%")
    print(f"{'='*62}")

if __name__ == "__main__":
    main()
