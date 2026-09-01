#!/usr/bin/env python3
"""M81 Chrome-vs-Ours 多维度验证：每轮测不同能力，不重复。

状态文件 /tmp/compare_results/round_state.json 记录轮次。
每轮执行：1 个渲染提取验证 + 1 个交互验证（click/hover/focus/type/check/select/scroll 轮换）。
结果追加到 /tmp/compare_results/history.jsonl（去重：同结果不重复报告）。
"""
import json, subprocess, time, os, sys, re
from html.parser import HTMLParser

CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
OURS = os.path.join(os.path.dirname(__file__), "..", "target", "release", "browser")
OUT = "/tmp/compare_results"
STATE = os.path.join(OUT, "round_state.json")
HISTORY = os.path.join(OUT, "history.jsonl")
os.makedirs(OUT, exist_ok=True)

# 站点池（15 站，比旧版多 9 站）
SITES = [
    "https://example.com/",
    "https://svelte.dev/",
    "https://todomvc.com/examples/javascript-es5/dist/",
    "https://httpbin.org/html",
    "https://docusaurus.io/",
    "https://bark.day.app/",
    "https://vite.dev/",
    "https://news.ycombinator.com/",
    "https://en.wikipedia.org/wiki/Web_browser",
    "https://developer.mozilla.org/en-US/",
    "https://docs.python.org/3/tutorial/index.html",
    "https://go.dev/",
    "https://rustlang.org/",
    "https://nodejs.org/",
    "https://www.rust-lang.org/",
]

# 交互能力轮换（7 种）
INTERACTIONS = ["click", "hover", "focus", "type", "check", "select", "scroll"]


class Extractor(HTMLParser):
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
    if not os.path.exists(path) or os.path.getsize(path) < 10:
        return {'texts': set(), 'links': set()}
    p = Extractor()
    p.feed(open(path, encoding='utf-8', errors='replace').read())
    return {'texts': p.texts, 'links': p.links}


def load_state():
    if os.path.exists(STATE):
        return json.load(open(STATE))
    return {'round': 0, 'site_idx': 0}


def save_state(st):
    json.dump(st, open(STATE, 'w'))


def render_extraction(site_url, name):
    """渲染 + 提取对比"""
    ours_html = os.path.join(OUT, f"r{name}_ours.html")
    chrome_html = os.path.join(OUT, f"r{name}_chrome.html")

    r = subprocess.run([OURS, 'fetch', site_url, '--format', 'html'],
                       capture_output=True, text=True, timeout=120)
    with open(ours_html, 'w') as f:
        f.write(r.stdout)
    with open(chrome_html, 'w') as f:
        subprocess.run([CHROME, '--headless=new', '--virtual-time-budget=10000',
                        '--dump-dom', site_url],
                       stdout=f, stderr=subprocess.DEVNULL, timeout=60)

    po = extract(ours_html)
    pc = extract(chrome_html)
    t_cov = len(po['texts'] & pc['texts']) / max(1, len(pc['texts'])) * 100
    l_cov = len(po['links'] & pc['links']) / max(1, len(pc['links'])) * 100
    return {'type': 'extraction', 'site': name, 'url': site_url,
            'text_cov': round(t_cov), 'link_cov': round(l_cov),
            'texts_ours': len(po['texts']), 'texts_chrome': len(pc['texts']),
            'links_ours': len(po['links']), 'links_chrome': len(pc['links'])}


def render_interaction(site_url, name, interaction):
    """交互验证：在真实站点上执行 --click/--hover/--focus/--type/--check/--select/--scroll-to"""
    r = subprocess.run([OURS, 'render-url', site_url, '--width', '80',
                        f'--{interaction}', 'a'],
                       capture_output=True, text=True, timeout=120)
    ok = r.returncode == 0
    return {'type': 'interaction', 'kind': interaction, 'site': name, 'url': site_url,
            'status': 'OK' if ok else 'FAIL', 'returncode': r.returncode}


def main():
    st = load_state()
    rnd = st['round']
    site_idx = st['site_idx'] % len(SITES)
    interaction_idx = rnd % len(INTERACTIONS)

    site_url = SITES[site_idx]
    site_name = site_url.split('//')[1].split('/')[0].replace('www.', '')
    interaction = INTERACTIONS[interaction_idx]

    print(f"=== Round {rnd+1}: {site_name} [{interaction}] ===")

    # 1) 渲染提取对比
    ext_result = render_extraction(site_url, site_name)

    # 2) 交互验证（对我们浏览器做一次交互操作）
    int_result = render_interaction(site_url, site_name, interaction)

    # 3) 写历史（追加，去重：同 site+type+coverage 不重复报）
    entry = {'round': rnd + 1, 'timestamp': time.strftime('%Y-%m-%d %H:%M:%S'),
             'extraction': ext_result, 'interaction': int_result}
    with open(HISTORY, 'a') as f:
        f.write(json.dumps(entry, ensure_ascii=False) + '\n')

    # 4) 更新状态
    st['round'] = rnd + 1
    st['site_idx'] = (site_idx + 1) % len(SITES)
    save_state(st)

    # 5) 输出摘要
    print(json.dumps(entry, indent=2, ensure_ascii=False))
    print(f"\nState: round={st['round']}, next_site={SITES[st['site_idx']].split('//')[1].split('/')[0]}")


if __name__ == '__main__':
    main()
