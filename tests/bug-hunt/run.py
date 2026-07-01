#!/usr/bin/env python3
"""
M72 Bug 狩猎 harness
=====================
对每个 fixture：
  1. 本地 HTTP server 提供访问
  2. Chrome headless dump-dom → 提取 #out（正确答案 baseline）
  3. 我们的 browser fetch → 提取文本（我们结果）
  4. 逐行对比，标出差异 = BUG

fixture 约定：JS 执行后往 <div id="out"> 写结果，格式：
  TESTNAME:PASS            ← Chrome 也 PASS 则对齐
  TESTNAME:FAIL(原因)       ← Chrome PASS 我们 FAIL = BUG
  （空或异常）              ← 我们没跑出结果 = BUG

用法：
  python3 run.py categories/A-js-core/   # 跑一个类别
  python3 run.py                         # 跑全部
"""
import http.server, socketserver, threading
import subprocess, sys, os, re, glob, json, time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent  # browser/
BINARY = str(ROOT / "target/release/browser")
CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
PORT = 8771

# ── 本地 HTTP server（线程化，避免单请求阻塞）──
class Handler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *a, **k):
        super().__init__(*a, directory=str(HERE), **k)
    def log_message(self, *a): pass  # 静默

_server = None
def start_server():
    global _server
    _server = socketserver.ThreadingTCPServer(("127.0.0.1", PORT), Handler)
    _server.daemon_threads = True
    threading.Thread(target=_server.serve_forever, daemon=True).start()

def server_url(rel_path):
    """categories/A-js-core/x.html → http://127.0.0.1:PORT/categories/A-js-core/x.html"""
    rel = Path(rel_path).relative_to(HERE)
    return f"http://127.0.0.1:{PORT}/{rel.as_posix()}"

# ── 提取 #out 文本 ──
def extract_out(text):
    """从输出文本里提取 #out 内容（我们的 fetch 输出已是纯文本，Chrome 需 dump-dom 解析）"""
    return text.strip()

def chrome_baseline(url, timeout=30):
    """Chrome dump-dom → 提取 #out 内文本"""
    try:
        r = subprocess.run(
            [CHROME, "--headless=new", "--disable-gpu", "--no-sandbox",
             "--virtual-time-budget=8000", "--dump-dom", url],
            capture_output=True, text=True, timeout=timeout
        )
        dom = r.stdout
        # 提取 <div id="out">...</div> 内容
        m = re.search(r'<div[^>]*id="out"[^>]*>(.*?)</div>', dom, re.DOTALL)
        if not m:
            return None, f"Chrome: no #out found"
        raw = m.group(1)
        # 转纯文本：标签去壳、<br>→换行、decode entity
        raw = raw.replace("<br>", "\n").replace("<br/>", "\n")
        raw = re.sub(r"<[^>]+>", "", raw)
        entities = {"&lt;":"<","&gt;":">","&amp;":"&","&quot;":'"',"&#39;":"'","&nbsp;":" "}
        for k,v in entities.items():
            raw = raw.replace(k, v)
        return raw.strip(), None
    except subprocess.TimeoutExpired:
        return None, "Chrome timeout"
    except Exception as e:
        return None, f"Chrome error: {e}"

def ours_fetch(url, timeout=20):
    """我们的 browser fetch → 纯文本输出"""
    try:
        r = subprocess.run(
            [BINARY, "fetch", url, "--format", "text"],
            capture_output=True, text=True, timeout=timeout
        )
        # stdout 含 [js-runtime] 等调试行，过滤掉只留正文
        lines = []
        for line in r.stdout.splitlines():
            if line.startswith("[") or line.startswith("Loading"):
                continue
            lines.append(line)
        out = "\n".join(lines).strip()
        return out, None, r.stderr
    except subprocess.TimeoutExpired:
        return None, "our timeout", ""
    except Exception as e:
        return None, f"our error: {e}", ""

# ── 单个 fixture 跑对比 ──
def run_fixture(html_path):
    """返回 dict: {name, chrome, ours, status: MATCH/BUG/SKIP, detail}"""
    name = Path(html_path).stem
    url = server_url(html_path)
    chrome_out, chrome_err = chrome_baseline(url)
    ours_out, ours_err, stderr = ours_fetch(url)
    # 收集我们的 JS 报错
    js_errors = []
    for line in stderr.splitlines():
        m = re.search(r'message=(.+?)(\||$)', line)
        if m: js_errors.append(m.group(1).strip())
    if chrome_out is None:
        return {"name": name, "status": "SKIP", "detail": chrome_err}
    if ours_out is None:
        return {"name": name, "status": "BUG", "detail": f"our={ours_err}", "chrome": chrome_out, "ours": ""}
    # 逐行对比
    clines = chrome_out.splitlines()
    olines = ours_out.splitlines()
    diffs = []
    maxn = max(len(clines), len(olines))
    mismatch = False
    for i in range(maxn):
        c = clines[i].strip() if i < len(clines) else ""
        o = olines[i].strip() if i < len(olines) else ""
        # Chrome baseline 里 TESTNAME:PASS 是预期
        # 我们如果 TESTNAME:FAIL 或不一致 = bug
        if c != o:
            mismatch = True
            diffs.append({"line": i+1, "chrome": c, "ours": o})
    status = "BUG" if mismatch else "MATCH"
    return {
        "name": name, "status": status,
        "chrome": chrome_out, "ours": ours_out,
        "diffs": diffs[:5],  # 前 5 个差异
        "js_errors": js_errors[:3],
        "detail": f"{len(diffs)} diff(s)" if mismatch else "aligned"
    }

# ── 跑一个目录 ──
def run_category(cat_dir):
    fixtures = sorted(glob.glob(str(HERE / cat_dir / "*.html")))
    if not fixtures:
        print(f"  [{cat_dir}] no fixtures")
        return []
    print(f"\n{'='*60}\n[{cat_dir}] {len(fixtures)} fixtures\n{'='*60}")
    results = []
    bugs = 0
    for f in fixtures:
        r = run_fixture(f)
        results.append(r)
        icon = "✅" if r["status"] == "MATCH" else "🐛" if r["status"] == "BUG" else "⏭️"
        detail = r.get("detail","")
        jserr = f" | js_err: {r['js_errors'][0]}" if r.get("js_errors") else ""
        print(f"  {icon} {r['name']:<30} {detail}{jserr}")
        if r["status"] == "BUG":
            bugs += 1
            for d in r.get("diffs",[])[:2]:
                print(f"       L{d['line']}: chrome={d['chrome'][:50]!r}")
                print(f"            ours ={d['ours'][:50]!r}")
    print(f"\n  [{cat_dir}] {bugs}/{len(fixtures)} bugs found")
    return results

def main():
    start_server()
    time.sleep(0.5)
    cats = sys.argv[1:] if len(sys.argv) > 1 else [
        "categories/A-js-core", "categories/B-dom-api",
        "categories/C-spa", "categories/D-network",
        "categories/E-console", "categories/F-edge"
    ]
    all_results = {}
    total_bugs = 0
    total_fixtures = 0
    for cat in cats:
        # 规范化路径
        cat_clean = cat.rstrip("/").replace(str(HERE)+"/","")
        results = run_category(cat_clean)
        all_results[cat_clean] = results
        bugs = sum(1 for r in results if r["status"]=="BUG")
        total_bugs += bugs
        total_fixtures += len(results)
    # 汇总报告
    print(f"\n{'='*60}\n汇总\n{'='*60}")
    for cat, results in all_results.items():
        bugs = sum(1 for r in results if r["status"]=="BUG")
        matches = sum(1 for r in results if r["status"]=="MATCH")
        print(f"  {cat:<25} {matches} match / {bugs} bug / {len(results)} total")
    print(f"\n  总计: {total_fixtures} fixtures, {total_bugs} bugs")
    # 保存详细报告
    report = HERE / "report.json"
    with open(report, "w") as f:
        json.dump(all_results, f, indent=2, ensure_ascii=False)
    print(f"  详细报告: {report}")
    return total_bugs

if __name__ == "__main__":
    sys.exit(0 if main() is not None else 1)
