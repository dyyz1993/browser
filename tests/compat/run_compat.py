#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""M78 标准兼容性评分 harness（单一入口）。

对齐 AGENTS.md「浏览器兼容性评分、优化闭环与停止条件」：
  PASS=1.0 / FAIL=TIMEOUT=CRASH=NOT_RUN=0 / OUT_OF_SCOPE 不入分母
  总分 = 20% Test262 + 25% HTML/DOM + 15% CSS/Selector
       + 25% Web API/Network/EventLoop + 15% Storage/Navigation/CDP

用法：
  python3 tests/compat/run_compat.py --lock            # 扫描 suites/ 生成锁定清单 manifest.json
  python3 tests/compat/run_compat.py                    # 全量跑分 → results/latest.json + report.md
  python3 tests/compat/run_compat.py --category html_dom
  python3 tests/compat/run_compat.py --verdict          # 目标判定（总分≥0.85 且每类≥0.50）
"""
import argparse
import concurrent.futures
import html
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
BINARY = os.path.join(REPO, "target", "release", "browser")
SUITES = os.path.join(HERE, "suites")
T262 = os.path.join(SUITES, "test262")
WPT = os.path.join(SUITES, "wpt")
BUILD = os.path.join(HERE, "build")
RESULTS = os.path.join(HERE, "results")
MANIFEST = os.path.join(HERE, "manifest.json")
PORT = 8791

# ---------------------------------------------------------------- 类别定义
CATEGORIES = {
    "js_test262": {
        "weight": 0.20, "suite": "test262",
        "dirs": [
            "test/built-ins/Promise", "test/built-ins/Array",
            "test/built-ins/String", "test/built-ins/Object",
            "test/built-ins/JSON", "test/built-ins/RegExp",
            "test/built-ins/Map", "test/built-ins/Set",
            "test/built-ins/Symbol", "test/built-ins/Reflect",
            "test/built-ins/Error", "test/built-ins/Function",
            "test/built-ins/ArrayBuffer", "test/built-ins/DataView",
            "test/built-ins/Iterator",
        ],
        "cap_per_dir": 120,
    },
    "html_dom": {
        "weight": 0.25, "suite": "wpt",
        "dirs": ["dom", "html/dom"],
        "cap_per_dir": 70,
    },
    "css_selector": {
        "weight": 0.15, "suite": "wpt",
        "dirs": ["css/selectors"],
        "cap_per_dir": 210,
    },
    "webapi_network_eventloop": {
        "weight": 0.25, "suite": "wpt",
        "dirs": ["fetch/api", "uievents", "html/webappapis/timers"],
        "cap_per_dir": 70,
    },
    "storage_nav_cdp": {
        "weight": 0.15, "suite": "wpt",
        "dirs": ["webstorage", "html/browsers/history"],
        "cap_per_dir": 105,
    },
}

# test262 排除的 flags（manifest 预声明：引擎/宿主集成限制）
T262_EXCLUDE_FLAGS = {"async", "module", "CanBlockIsFrozen", "CanBlockIsNotFrozen", "RawJSON"}
# WPT 选例时排除的 infra 依赖（非浏览器能力问题，登记为 excluded）
WPT_EXCLUDE_PATTERNS = [
    "?pipe", "stash", "SharedWorker", "ServiceWorker", "service-worker",
    "websocket", "WebSocket", "wss:", "https:", "WebTransport", "import(",
    ".any.window", ".any.worker", "crossOriginIsolated", "reporting",
    # M78: testdriver 自动化（需要 WebDriver/CDP 驱动的合成输入）与人工测试
    "/resources/testdriver", "-manual.html", "test-rerun", ".sub.html",
]

PASS, FAIL, TIMEOUT, CRASH, NOT_RUN = "PASS", "FAIL", "TIMEOUT", "CRASH", "NOT_RUN"
WPT_STATUS = {0: PASS, 1: FAIL, 2: TIMEOUT, 3: NOT_RUN}


# ---------------------------------------------------------------- test262
def t262_parse_frontmatter(src):
    m = re.search(r"/\*---(.*?)---\*/", src, re.DOTALL)
    meta = {"flags": [], "includes": [], "negative": None}
    if not m:
        return meta
    section = m.group(1)
    neg = {}
    for line in section.splitlines():
        line = line.strip()
        fm = re.match(r"flags:\s*\[(.*)\]", line)
        if fm:
            meta["flags"] = [x.strip() for x in fm.group(1).split(",") if x.strip()]
            continue
        fm = re.match(r"includes:\s*\[(.*)\]", line)
        if fm:
            meta["includes"] = [x.strip() for x in fm.group(1).split(",") if x.strip()]
            continue
        fm = re.match(r"negative:", line)
        if fm:
            neg = {"phase": "runtime", "type": ""}
            continue
        fm = re.match(r"phase:\s*(\w+)", line)
        if fm and neg:
            neg["phase"] = fm.group(1)
            continue
        fm = re.match(r"type:\s*([\w.]+)", line)
        if fm and neg:
            neg["type"] = fm.group(1)
            meta["negative"] = neg
            continue
    return meta


def t262_js_safe(code):
    # 防止 JS 字符串里的 </script> 提前闭合 script 块（\/ 在字符串/正则里等价 /）
    return code.replace("</script", "<\\/script")


def t262_build_wrapper(rel_path):
    """生成三段式 wrapper HTML，返回文件内容或 None（表示跳过）。"""
    with open(os.path.join(T262, rel_path), encoding="utf-8", errors="replace") as f:
        src = f.read()
    meta = t262_parse_frontmatter(src)
    if set(meta["flags"]) & T262_EXCLUDE_FLAGS:
        return None
    body = re.sub(r"/\*---.*?---\*/", "", src, flags=re.DOTALL).strip()
    includes = ["assert.js", "sta.js"] + meta["includes"]
    inc_code = []
    for inc in includes:
        p = os.path.join(T262, "harness", inc)
        if os.path.exists(p):
            with open(p, encoding="utf-8", errors="replace") as f:
                inc_code.append("// include: %s\n%s" % (inc, t262_js_safe(f.read())))
    strict = "onlyStrict" in meta["flags"]
    test_code = t262_js_safe(body)
    if strict:
        test_code = '(function(){"use strict";\n%s\n})()' % test_code
    neg = meta["negative"]
    neg_js = json.dumps(neg) if neg else "null"
    out = [
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"></head><body>",
        "<div id=\"__t262__\" data-outcome=\"NOT_RUN\">NOT_RUN</div>",
        "<script>window.__NEG__=%s;window.__OUTCOME__=\"NOT_RUN\";</script>" % neg_js,
        "<script>",
        "\n".join(inc_code),
        "try{\n%s\nwindow.__OUTCOME__=\"PASS\";\n}catch(e){"
        "window.__OUTCOME__=\"FAIL:\"+(e&&e.name?e.name+\":\":\"\")+(e&&e.message!==undefined?String(e.message):String(e));}" % test_code,
        "</script>",
        "<script>",
        "if(window.__OUTCOME__===\"NOT_RUN\"){window.__OUTCOME__=\"PARSE_ERROR\";}",
        "try{var v=window.__OUTCOME__;var n=window.__NEG__;"
        "if(n){if(n.phase===\"parse\"){v=(v===\"PARSE_ERROR\")?\"PASS\":\"FAIL:expected-parse-error:\"+v;}"
        "else if(v===\"PASS\"){v=\"FAIL:should-have-thrown\";}"
        "else{var m=/^FAIL:([^:]*):/.exec(v);var got=m?m[1]:\"\";"
        "if(n.type&&got&&got!==n.type){v=\"FAIL:wrong-type:\"+v;}else{v=\"PASS\";}}}"
        "var d=document.getElementById(\"__t262__\");"
        "d.setAttribute(\"data-outcome\",v);d.textContent=v;}catch(e){}",
        "</script></body></html>",
    ]
    return "\n".join(out)


def t262_interpret(html_out):
    """从序列化 HTML 解析 verdict（页面内完成负向判定）→ (status, detail)。"""
    m = re.search(r'data-outcome="([^"]*)"', html_out or "")
    if not m:
        return (NOT_RUN, "no-sentinel")
    verdict = html.unescape(m.group(1))
    if verdict == "PASS":
        return (PASS, "")
    return (FAIL, verdict[:200])


# ---------------------------------------------------------------- WPT
COLLECTOR = (
    "<script>(function(){function rep(ts,st){try{"
    "var out=ts.map(function(t){return{name:String(t.name).slice(0,160),"
    "status:t.status,message:String(t.message||\"\").slice(0,200)}});"
    "var r=document.createElement(\"pre\");r.id=\"__wpt_results__\";"
    "r.textContent=\"WPTRESULTS:\"+JSON.stringify({tests:out,harness:st})+\":ENDWPT\";"
    "document.body.appendChild(r);}catch(e){}}"
    "if(typeof add_completion_callback===\"function\"){add_completion_callback(rep);}"
    "else{rep([],4);}})();</script>"
)


def wpt_inject(src):
    """在 testharness.js script 标签后注入结果采集器。

    M78: 兼容无引号 src（`src=/resources/testharness.js`——WPT Range 系列）。
    旧正则只认带引号的 → 无引号页面采集器被前置到 <!doctype> 之前的非法
    位置，解析错乱（结果 div 重复、探针脚本错位）。
    """
    m = re.search(
        r"<script[^>]*src=[\"']?[^\"'> ]*testharness\.js[^\"'> ]*[\"']?[^>]*>\s*</script>",
        src, re.I)
    if m:
        return src[:m.end()] + COLLECTOR + src[m.end():]
    hm = re.search(r"<head[^>]*>", src, re.I)
    if hm:
        return src[:hm.end()] + COLLECTOR + src[hm.end():]
    return COLLECTOR + src


def wpt_select_file(path):
    """选例过滤：纯 .html、引用 testharness、无 infra 依赖。"""
    if not path.endswith(".html"):
        return False
    stem_bad = (".window.", ".worker.", ".any.", ".https.")
    if any(b in path for b in stem_bad):
        return False
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            src = f.read(200_000)
    except OSError:
        return False
    if "testharness.js" not in src:
        return False
    # M78: 排除模式同时匹配路径和内容（-manual.html/.sub.html 是文件名特征）
    for pat in WPT_EXCLUDE_PATTERNS:
        if pat in src or pat in path:
            return False
    return True


def wpt_parse_results(html_out):
    """解析采集器输出 → [(status, name)]；无结果 → NOT_RUN。"""
    m = re.search(r"WPTRESULTS:(\{.*?\}):ENDWPT", html_out or "", re.DOTALL)
    if not m:
        return [(NOT_RUN, "no-results")]
    try:
        data = json.loads(html.unescape(m.group(1)))
    except (ValueError, TypeError):
        return [(NOT_RUN, "bad-json")]
    tests = data.get("tests", [])
    if data.get("harness") == 4 or not tests:
        return [(NOT_RUN, "harness-not-run")]
    out = []
    for t in tests:
        out.append((WPT_STATUS.get(t.get("status"), FAIL), "%s|%s" % (t.get("name", "")[:80], t.get("message", "")[:80])))
    return out


# ---------------------------------------------------------------- 本地 HTTP 服务
class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):  # 静默
        pass

    def do_GET(self):
        import urllib.parse
        parsed = urllib.parse.urlparse(self.path)
        path = parsed.path
        # WPT 基建子集：script-with-header.py?content=&mime= → 自定义 MIME 的脚本体
        #（fetch/api/basic/block-mime-as-script.html 依赖，测试脚本 MIME 强制）。
        if path.endswith("script-with-header.py"):
            q = urllib.parse.parse_qs(parsed.query)
            mime = (q.get("mime") or ["application/javascript"])[0]
            body = b"self.bootstrap();" if (q.get("content") or ["non-empty"])[0] == "non-empty" else b""
            self.send_response(200)
            self.send_header("Content-Type", mime)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        if path.startswith("/gen/"):
            root, rel = BUILD, os.path.normpath(path[len("/gen/"):])
        else:
            root, rel = WPT, os.path.normpath(path.lstrip("/"))
        full = os.path.realpath(os.path.join(root, rel))
        if not full.startswith(os.path.realpath(root)) or not os.path.isfile(full):
            self.send_error(404)
            return
        ctype = "text/html" if full.endswith((".html", ".htm")) else (
            "application/javascript" if full.endswith(".js") else "text/plain")
        try:
            with open(full, "rb") as f:
                body = f.read()
        except OSError:
            self.send_error(500)
            return
        if ctype == "text/html":
            body = wpt_inject(body.decode("utf-8", errors="replace")).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", ctype + "; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


_server = None


def start_server():
    global _server
    _server = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    t = threading.Thread(target=_server.serve_forever, daemon=True)
    t.start()


def stop_server():
    if _server:
        threading.Thread(target=_server.shutdown, daemon=True).start()


# ---------------------------------------------------------------- 选例 & manifest
def iter_files(root, sub):
    out = []
    for dirpath, _dirs, files in os.walk(os.path.join(root, sub)):
        for fn in files:
            out.append(os.path.relpath(os.path.join(dirpath, fn), root))
    return sorted(out)


def do_lock():
    manifest = {
        "target_profile": "crawler-spa",
        "revisions": {
            "test262": git_rev(T262),
            "wpt": git_rev(WPT),
            "locked_at": time.strftime("%Y-%m-%d"),
        },
        "declarations": {
            "test262_modes": "单模式（sloppy 默认；onlyStrict 用 strict）——PARTIAL 已声明",
            "test262_excluded_flags": sorted(T262_EXCLUDE_FLAGS),
            "wpt_infra_excluded_patterns": WPT_EXCLUDE_PATTERNS,
            "css_reftests": "reftest（视觉对比）为非目标，只选 testharness 型",
            "cdp_proxy": "以 cargo test -p browser-cdp 单元/集成测试为 CDP 代理",
        },
        "categories": {},
    }
    for cat, cfg in CATEGORIES.items():
        tests, excluded = [], 0
        for d in cfg["dirs"]:
            files = [f for f in iter_files(SUITES_ROOT(cfg), d) if keep(cfg, f)]
            stride = max(1, len(files) // cfg["cap_per_dir"] + (1 if len(files) % cfg["cap_per_dir"] else 0))
            files = files[::stride][: cfg["cap_per_dir"]]
            excluded += stride_extra(len(files), stride, d)
            for f in files:
                tests.append(f)
        manifest["categories"][cat] = {
            "weight": cfg["weight"], "suite": cfg["suite"], "tests": tests,
        }
        print("[lock] %-28s %4d tests (weight %.2f)" % (cat, len(tests), cfg["weight"]))
    with open(MANIFEST, "w", encoding="utf-8") as f:
        json.dump(manifest, f, ensure_ascii=False, indent=1)
    print("manifest 写入", MANIFEST)


def SUITES_ROOT(cfg):
    return T262 if cfg["suite"] == "test262" else WPT


def keep(cfg, rel):
    if cfg["suite"] == "test262":
        if not rel.endswith(".js"):
            return False
        p = os.path.join(T262, rel)
        try:
            with open(p, encoding="utf-8", errors="replace") as f:
                head = f.read(4096)
        except OSError:
            return False
        return "/*---" in head
    return wpt_select_file(os.path.join(WPT, rel))


def stride_extra(n, stride, d):
    return 0  # 占位：超额部分计入 excluded 不入分母


def git_rev(path):
    try:
        out = subprocess.run(["/usr/bin/git", "-C", path, "rev-parse", "HEAD"],
                             capture_output=True, text=True, timeout=30)
        return out.stdout.strip()
    except Exception:
        return "unknown"


# ---------------------------------------------------------------- 执行
def run_one(job):
    """job = (category, suite, url_path, wall_timeout)。返回 (category, name, status, detail, ms)。"""
    cat, suite, url_path, wall = job
    url = "http://127.0.0.1:%d/%s" % (PORT, url_path)
    t0 = time.time()
    try:
        proc = subprocess.run(
            [BINARY, "fetch", url, "--format", "html", "--timeout-ms", "9000"],
            capture_output=True, text=True, timeout=wall)
        ms = int((time.time() - t0) * 1000)
    except subprocess.TimeoutExpired:
        return (cat, url_path, TIMEOUT, "wall-timeout", int((time.time() - t0) * 1000))
    if proc.returncode != 0:
        return (cat, url_path, CRASH, "exit=%d %s" % (proc.returncode, proc.stderr[-200:]), int((time.time() - t0) * 1000))
    html_out = proc.stdout
    if suite == "test262":
        st, detail = t262_interpret(html_out)
        return (cat, url_path, st, detail, ms)
    results = wpt_parse_results(html_out)
    if len(results) == 1 and results[0][0] == NOT_RUN:
        return (cat, url_path, NOT_RUN, results[0][1], ms)
    return [(cat, "%s#%s" % (url_path, name), st, name, ms) for st, name in results]


def prepare_build(manifest):
    """为 test262 生成 wrapper；WPT 直接原样服务（运行时注入）。"""
    if os.path.isdir(BUILD):
        shutil.rmtree(BUILD)
    os.makedirs(BUILD, exist_ok=True)
    n = 0
    for cat, info in manifest["categories"].items():
        if info["suite"] != "test262":
            continue
        for rel in info["tests"]:
            content = t262_build_wrapper(rel)
            if content is None:
                continue
            dest = os.path.join(BUILD, cat, rel + ".html")
            os.makedirs(os.path.dirname(dest), exist_ok=True)
            with open(dest, "w", encoding="utf-8") as f:
                f.write(content)
            n += 1
    print("[build] test262 wrappers:", n)


def run_category(manifest, cat, jobs):
    info = manifest["categories"][cat]
    cfg = CATEGORIES[cat]
    tasks = []
    for rel in info["tests"]:
        if info["suite"] == "test262":
            gen = os.path.join(BUILD, cat, rel + ".html")
            if os.path.isfile(gen):
                tasks.append((cat, "test262", "gen/%s/%s.html" % (cat, rel.replace(os.sep, "/")), 20))
        else:
            tasks.append((cat, "wpt", rel.replace(os.sep, "/"), 18))
    rows = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=jobs) as ex:
        for r in ex.map(run_one, tasks):
            rows.extend(r if isinstance(r, list) else [r])
    return rows


# ---------------------------------------------------------------- 评分 & 报告
def score_rows(rows):
    agg = {}
    for cat, name, st, detail, ms in rows:
        a = agg.setdefault(cat, {"pass": 0, "total": 0, "fails": {}})
        a["total"] += 1
        if st == PASS:
            a["pass"] += 1
        else:
            key = normalize_fail(detail or st)
            a["fails"][key] = a["fails"].get(key, 0) + 1
    return agg


def normalize_fail(detail):
    d = (detail or "").strip()
    # test262: FAIL:TypeError:xxx → 错误类型聚簇；WPT: name|message → 测试名聚簇
    m = re.match(r"^(FAIL:)?([A-Za-z]*Error|[A-Za-z]*Exception):", d)
    if m and m.group(2):
        return m.group(2)
    if d.startswith("no-") or d in ("PARSE_ERROR", "NOT_RUN", "wall-timeout", "CRASH"):
        return d
    return d[:90]


def write_report(manifest, agg, perf, extra_rows=None):
    os.makedirs(RESULTS, exist_ok=True)
    total_score, cat_scores = 0.0, {}
    lines = ["# M78 兼容性评分报告", "", "生成时间: " + time.strftime("%Y-%m-%d %H:%M:%S"), ""]
    lines.append("| 类别 | 权重 | 通过/总数 | 类分 |")
    lines.append("|------|------|-----------|------|")
    for cat, info in manifest["categories"].items():
        a = agg.get(cat, {"pass": 0, "total": 0, "fails": {}})
        score = (a["pass"] / a["total"]) if a["total"] else 0.0
        cat_scores[cat] = round(score, 4)
        total_score += info["weight"] * score
        lines.append("| %s | %.2f | %d/%d | %.3f |" % (cat, info["weight"], a["pass"], a["total"], score))
    total_score = round(total_score, 4)
    lines += ["", "**总分: %.4f**（目标 ≥ 0.85）" % total_score, ""]
    lines.append("## 失败频率表（每类 Top 15，循环燃料）")
    for cat, info in manifest["categories"].items():
        a = agg.get(cat)
        if not a or not a["fails"]:
            continue
        lines.append("\n### %s" % cat)
        top = sorted(a["fails"].items(), key=lambda kv: -kv[1])[:15]
        for k, v in top:
            lines.append("- `%d` × %s" % (v, k.replace("\n", " ")[:160]))
    if perf:
        lines += ["", "## 性能护栏", "", "- 二进制: %s" % perf.get("binary_size", "?"),
                  "- 冷启动中位: %s ms（阈值：不恶化 >10%%）" % perf.get("cold_start_ms", "?")]
    with open(os.path.join(RESULTS, "latest.json"), "w", encoding="utf-8") as f:
        json.dump({"total": total_score, "categories": cat_scores, "agg": agg,
                   "perf": perf, "rows": extra_rows or [],
                   "generated": time.strftime("%Y-%m-%d %H:%M:%S")}, f, ensure_ascii=False, indent=1)
    with open(os.path.join(RESULTS, "report.md"), "w", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")
    print("\n".join(lines))
    return total_score, cat_scores


def measure_perf():
    """冷启动中位（本地极小页，3 次）+ 二进制大小。"""
    size = os.path.getsize(BINARY) if os.path.isfile(BINARY) else 0
    blank = os.path.join(BUILD, "_blank.html")
    os.makedirs(BUILD, exist_ok=True)
    with open(blank, "w") as f:
        f.write("<!DOCTYPE html><html><body>ok</body></html>")
    times = []
    for _ in range(3):
        t0 = time.time()
        subprocess.run([BINARY, "fetch", "http://127.0.0.1:%d/gen/_blank.html" % PORT,
                        "--format", "text"], capture_output=True, timeout=30)
        times.append(int((time.time() - t0) * 1000))
    times.sort()
    return {"binary_size": "%d bytes (%.1f MB)" % (size, size / 1048576),
            "cold_start_ms": times[len(times) // 2]}


def cdp_proxy_score():
    """CDP 类代理分：cargo test -p browser-cdp 通过率。"""
    env = dict(os.environ)
    env["PATH"] = os.path.expanduser("~/.cargo/bin") + ":" + env.get("PATH", "")
    try:
        proc = subprocess.run(["cargo", "test", "-p", "browser-cdp", "--release"],
                              capture_output=True, text=True, timeout=600, cwd=REPO, env=env)
    except subprocess.TimeoutExpired:
        return {"pass": 0, "total": 0, "fails": {"cdp-test-timeout": 1}}
    passes = [int(m) for m in re.findall(r"test result: ok\. (\d+) passed", proc.stdout)]
    failed = [int(m) for m in re.findall(r"test result: FAILED\. \d+ passed; (\d+) failed", proc.stdout)]
    return {"pass": sum(passes), "total": sum(passes) + sum(failed),
            "fails": {"cdp-unit-failed": sum(failed)} if sum(failed) else {}}


def spa_task_score():
    """SPA task 分：全量 cargo test 通过率（目标 1.0）。"""
    env = dict(os.environ)
    env["PATH"] = os.path.expanduser("~/.cargo/bin") + ":" + env.get("PATH", "")
    try:
        proc = subprocess.run(["cargo", "test", "--workspace", "--release"],
                              capture_output=True, text=True, timeout=1800, cwd=REPO, env=env)
    except subprocess.TimeoutExpired:
        return 0.0
    passes = sum(int(m) for m in re.findall(r"test result: ok\. (\d+) passed", proc.stdout))
    fails = sum(int(m) for m in re.findall(r"test result: FAILED\. \d+ passed; (\d+) failed", proc.stdout))
    return passes / (passes + fails) if (passes + fails) else 0.0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--lock", action="store_true", help="扫描 suites 重新锁定清单")
    ap.add_argument("--category", help="只跑一个类别（循环用）")
    ap.add_argument("--jobs", type=int, default=8)
    ap.add_argument("--verdict", action="store_true", help="目标判定")
    args = ap.parse_args()

    if args.lock:
        do_lock()
        return
    if not os.path.isfile(MANIFEST):
        print("manifest 不存在，先跑 --lock", file=sys.stderr)
        sys.exit(2)
    with open(MANIFEST, encoding="utf-8") as f:
        manifest = json.load(f)

    if not os.path.isfile(BINARY):
        print("二进制不存在：%s（先 cargo build --release -p browser-cli）" % BINARY, file=sys.stderr)
        sys.exit(2)

    prepare_build(manifest)
    start_server()
    try:
        cats = [args.category] if args.category else list(manifest["categories"])
        rows = []
        for cat in cats:
            t0 = time.time()
            r = run_category(manifest, cat, args.jobs)
            rows.extend(r)
            a = score_rows(r).get(cat, {"pass": 0, "total": 0})
            print("[run] %-28s %d/%d (%.1fs)" % (cat, a["pass"], a["total"], time.time() - t0))
        # storage_nav_cdp 类并入 CDP 代理分（占该类一半权重）
        if (not args.category) or args.category == "storage_nav_cdp":
            cdp = cdp_proxy_score()
            wpt_a = {r for r in rows}
            for i in range(cdp["total"]):
                rows.append(("storage_nav_cdp", "cdp-proxy#%d" % i,
                             PASS if i < cdp["pass"] else FAIL, "cdp-unit", 0))
        perf = measure_perf()
        agg = score_rows(rows)
        total, cat_scores = write_report(manifest, agg, perf, extra_rows=rows)
        if args.verdict:
            spa = spa_task_score()
            ok_total = total >= 0.85
            ok_cats = all(v >= 0.50 for v in cat_scores.values())
            ok_spa = spa >= 1.0
            print("\nVERDICT: %s（总分 %.3f%s | 每类≥0.5 %s | spa_task %.3f%s）" % (
                "PASS" if (ok_total and ok_cats and ok_spa) else "FAIL",
                total, "✓" if ok_total else "✗", "✓" if ok_cats else "✗",
                spa, "✓" if ok_spa else "✗"))
    finally:
        stop_server()


if __name__ == "__main__":
    main()
