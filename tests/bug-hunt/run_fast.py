#!/usr/bin/env python3
"""
快速 bug 扫描模式
=================
不需要 Chrome baseline，fixture 自身有 PASS/FAIL 断言。
跑我们的 browser fetch，提取 #out 内容，发现 FAIL = bug。
"""
import subprocess, sys, re, glob, json, time, os
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
BINARY = str(ROOT / "target/release/browser")
TIMEOUT = 30

def run_cmd(cmd, timeout=TIMEOUT):
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return r.stdout, r.stderr, r.returncode
    except subprocess.TimeoutExpired:
        return "", "TIMEOUT", -1
    except Exception as e:
        return "", str(e), -1

def scan_html_file(path):
    """Scan a single fixture HTML file."""
    name = Path(path).stem
    rel = Path(path).relative_to(HERE)
    url = f"http://127.0.0.1:8772/{rel.as_posix()}"
    
    stdout, stderr, rc = run_cmd([BINARY, "fetch", url, "--format", "text"], TIMEOUT)
    
    # Collect JS errors from stderr
    js_errors = []
    for line in stderr.splitlines():
        if "[quickjs]" in line or "[js]" in line:
            js_errors.append(line.strip())
    
    # Filter stdout to get content (skip debug lines)
    content_lines = [l for l in stdout.splitlines() if not l.startswith("[") and not l.startswith("Loading")]
    content = "\n".join(content_lines)
    
    # Find all :PASS and :FAIL in content
    passes = re.findall(r'(\w[\w-]*):PASS', content)
    fails = re.findall(r'(\w[\w-]*):FAIL(?:\(([^)]*)\))?', content)
    script_throws = re.findall(r'__SCRIPT_THREW__:?(.*)', content)
    
    result = {
        "name": name,
        "passes": passes,
        "fails": [{"name": f[0], "detail": f[1]} for f in fails],
        "script_throws": script_throws,
        "js_errors": js_errors[:5],
        "content_preview": content[:200] if content else "(empty)",
        "has_error": bool(fails or script_throws or js_errors),
        "has_content": bool(content.strip()),
    }
    
    return result

def scan_category(cat_dir):
    """Scan all HTML files in a category directory."""
    fixtures = sorted(glob.glob(str(cat_dir / "*.html")))
    if not fixtures:
        return []
    
    cat_name = cat_dir.name
    print(f"\n{'='*60}")
    print(f"  [{cat_name}] {len(fixtures)} fixtures")
    print(f"{'='*60}")
    
    results = []
    for f in fixtures:
        r = scan_html_file(f)
        results.append(r)
        
        if r["script_throws"]:
            icon = "💥"
        elif r["fails"]:
            icon = "🐛"
        elif r["js_errors"]:
            icon = "⚠️"
        elif not r["has_content"]:
            icon = "❌"
        else:
            icon = "✅"
        
        fail_detail = ""
        if r["fails"]:
            fail_detail = f" ({len(r['fails'])} FAILs)"
        elif r["script_throws"]:
            fail_detail = f" (SCRIPT THREW: {r['script_throws'][0][:50]})"
        elif r["js_errors"]:
            fail_detail = f" ({r['js_errors'][0][:60]})"
        elif not r["has_content"]:
            fail_detail = " (no content)"
        
        print(f"  {icon} {r['name']:<30}{fail_detail}")
        
        # For bugs, show first failure detail
        if r["fails"]:
            for f_ in r["fails"][:2]:
                d = f"({f_['detail']})" if f_["detail"] else ""
                print(f"       FAIL: {f_['name']}{d}")
    
    return results

def main():
    categories_dir = HERE / "categories"
    cats = sys.argv[1:] if len(sys.argv) > 1 else sorted(
        d.name for d in categories_dir.iterdir() if d.is_dir()
    )
    
    all_results = {}
    total_bugs = 0
    total_pass = 0
    total_fixtures = 0
    
    for cat in cats:
        cat_path = categories_dir / cat
        if not cat_path.exists():
            print(f"  [WARN] category not found: {cat}")
            continue
        results = scan_category(cat_path)
        all_results[cat] = results
        bugs = sum(1 for r in results if r["has_error"])
        passes = sum(1 for r in results if not r["has_error"])
        total_bugs += bugs
        total_pass += passes
        total_fixtures += len(results)
    
    # Summary
    print(f"\n{'='*60}")
    print(f"  汇总")
    print(f"{'='*60}")
    for cat, results in all_results.items():
        if not results:
            continue
        bugs = sum(1 for r in results if r["has_error"])
        matches = sum(1 for r in results if not r["has_error"])
        print(f"  {cat:<25} ✅ {matches:>2} / 🐛 {bugs:>2} / total {len(results)}")
    print(f"\n  总计: ✅ {total_pass} passed | 🐛 {total_bugs} bugs | {total_fixtures} fixtures")
    
    # Save report
    report = HERE / "bug_report.json"
    with open(report, "w") as f:
        json.dump(all_results, f, indent=2, ensure_ascii=False)
    print(f"  报告: {report}")
    
    # Generate markdown bug list
    md_path = HERE / "BUGS.md"
    with open(md_path, "w") as f:
        f.write("# M72 Bug 狩猎报告\n\n")
        f.write(f"生成时间: {time.ctime()}\n\n")
        f.write(f"总览: ✅ {total_pass} passed / 🐛 {total_bugs} bugs / {total_fixtures} fixtures\n\n")
        f.write("| 类别 | 状态 | 详情 |\n")
        f.write("|------|------|------|\n")
        for cat, results in all_results.items():
            for r in results:
                icon = "💥" if r["script_throws"] else "🐛" if r["fails"] else "⚠️" if r["js_errors"] else "✅"
                if r["has_error"]:
                    detail = ""
                    if r["script_throws"]:
                        detail = f"SCRIPT_THREW: {r['script_throws'][0][:80]}"
                    elif r["fails"]:
                        fails = ", ".join([f"{f_['name']}" for f_ in r["fails"][:3]])
                        detail = f"FAIL: {fails}"
                    elif r["js_errors"]:
                        detail = f"JS_ERR: {r['js_errors'][0][:80]}"
                    f.write(f"| {cat}/{r['name']} | {icon} | {detail} |\n")
    
    print(f"  BUGS.md: {md_path}")
    
    return total_bugs

if __name__ == "__main__":
    sys.exit(1 if main() else 0)
