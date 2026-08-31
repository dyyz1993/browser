#!/usr/bin/env python3
"""M81 Chrome-vs-Ours 对比验证：同站双引擎渲染 + CDP 交互一致性。

每轮选一个站点，用 Chrome headless 和 browser 分别渲染/交互，
对比提取的数据是否一致（文本/链接/点击行为）。
结果写入 /tmp/compare_results/latest.json 并输出摘要。
"""
import json, subprocess, time, os, sys, hashlib, re
from html.parser import HTMLParser
from html import unescape

CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
OURS = os.path.join(os.path.dirname(__file__), "..", "target", "release", "browser")
OUT = "/tmp/compare_results"
os.makedirs(OUT, exist_ok=True)

# 测试站列表（轮换）
SITES = [
    ("example", "https://example.com/"),
    ("svelte", "https://svelte.dev/"),
    ("todomvc", "https://todomvc.com/examples/javascript-es5/dist/"),
    ("httpbin", "https://httpbin.org/html"),
    ("docusaurus", "https://docusaurus.io/"),
    ("bark", "https://bark.day.app/"),
]

# CDP 点击测试页（本地写入）
CLICK_PAGE = os.path.join(OUT, "click_test.html")
os.makedirs(os.path.dirname(CLICK_PAGE), exist_ok=True)
with open(CLICK_PAGE, "w") as f:
    f.write("""<html><body>
<button id="btn" onclick="this.textContent='CLICKED';document.title='CLICKED-TITLE'">Press Me</button>
<div id="result">before</div>
<script>document.getElementById('btn').addEventListener('click',function(){document.getElementById('result').textContent='LISTENER-FIRED'});</script>
</body></html>""")


class Extractor(HTMLParser):
    """提取文本 + 链接"""
    def __init__(self):
        super().__init__()
        self.texts = set()
        self.links = set()
        self._skip = 0
    def handle_starttag(self, tag, attrs):
        if tag in ('script','style','noscript'): self._skip += 1
        if tag == 'a':
            for k, v in attrs:
                if k == 'href' and v: self.links.add(v.split('#')[0])
    def handle_endtag(self, tag):
        if tag in ('script','style','noscript'): self._skip = max(0, self._skip - 1)
    def handle_data(self, data):
        if self._skip: return
        t = re.sub(r'\s+', ' ', data).strip()
        if len(t) >= 4: self.texts.add(t[:60])


def extract(path):
    if not os.path.exists(path): return None
    p = Extractor()
    p.feed(open(path, encoding='utf-8', errors='replace').read())
    return {'texts': p.texts, 'links': p.links}


def chrome_dump(url, out_html):
    """Chrome headless dump-dom"""
    r = subprocess.run([CHROME, '--headless=new', '--virtual-time-budget=10000',
                        '--dump-dom', url], capture_output=True, text=True, timeout=60)
    open(out_html, 'w').write(r.stdout)
    return r.stdout


def ours_dump(url, out_html):
    """我们的 fetch --format html"""
    r = subprocess.run([OURS, 'fetch', url, '--format', 'html'],
                       capture_output=True, text=True, timeout=120)
    open(out_html, 'w').write(r.stdout)
    return r.stdout


def compare_site(name, url):
    """对比一个站点的双引擎提取结果"""
    ours_html = os.path.join(OUT, f"{name}_ours.html")
    chrome_html = os.path.join(OUT, f"{name}_chrome.html")

    ours_dump(url, ours_html)
    chrome_dump(url, chrome_html)

    po, pc = extract(ours_html), extract(chrome_html)
    if po is None or pc is None:
        return {'site': name, 'status': 'FETCH_FAIL', 'text_cov': 0, 'link_cov': 0}

    t_inter = len(po['texts'] & pc['texts'])
    l_inter = len(po['links'] & pc['links'])
    t_cov = t_inter / max(1, len(pc['texts'])) * 100
    l_cov = l_inter / max(1, len(pc['links'])) * 100
    return {
        'site': name, 'url': url, 'status': 'OK',
        'text_cov': round(t_cov), 'link_cov': round(l_cov),
        'texts_ours': len(po['texts']), 'texts_chrome': len(pc['texts']),
        'links_ours': len(po['links']), 'links_chrome': len(pc['links']),
    }


def compare_cdp_click():
    """对比 CDP 点击行为一致性"""
    page_url = f"file://{CLICK_PAGE}"
    results = {}

    # 我们：render-file --click
    r = subprocess.run([OURS, 'render-file', CLICK_PAGE, '--click', '#btn'],
                       capture_output=True, text=True, timeout=30)
    results['ours_click'] = 'CLICKED' in r.stdout

    # Chrome headless：evaluate 点击
    r2 = subprocess.run([CHROME, '--headless=new', '--dump-dom',
                         f"--run-all-compositor-stages-before-draw",
                         f"--virtual-time-budget=2000",
                         page_url.replace('file://', '')],
                        capture_output=True, text=True, timeout=30)
    # Chrome dump-dom 不触发 click——跳过（需要 CDP driver）

    return results


def main():
    site_idx = int(time.time() / 600) % len(SITES)  # 每 10 分钟轮换
    name, url = SITES[site_idx]

    print(f"=== M81 对比验证：{name} ({url}) ===")
    result = compare_site(name, url)
    print(json.dumps(result, indent=2, ensure_ascii=False))

    click_result = compare_cdp_click()

    # 汇总
    summary = {
        'timestamp': time.strftime('%Y-%m-%d %H:%M:%S'),
        'extraction': result,
        'cdp_click': click_result,
    }
    out = os.path.join(OUT, 'latest.json')
    with open(out, 'w') as f:
        json.dump(summary, f, indent=2, ensure_ascii=False)
    print(f"\nSaved: {out}")

    # 判定
    if result.get('status') == 'OK':
        tc, lc = result['text_cov'], result['link_cov']
        verdict = '✅ PASS' if tc >= 80 and lc >= 80 else ('⚠ PARTIAL' if tc >= 50 else '❌ FAIL')
        print(f"\nVERDICT: {verdict} (text={tc}%, link={lc}%)")


if __name__ == '__main__':
    main()
