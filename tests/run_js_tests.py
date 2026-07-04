#!/usr/bin/env python3
"""
CI JS test runner for browser bug-hunt fixtures.
Starts HTTP server, runs all fixture HTML files through `browser fetch`,
reports PASS/FAIL results.

Usage:
    python3 tests/run_js_tests.py              # uses target/release/browser
    python3 tests/run_js_tests.py --debug      # uses target/debug/browser
"""
import subprocess, sys, re, glob, json, time, os
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
BINARY = str(ROOT / "target/release/browser")
TIMEOUT = 45  # per-fixture timeout (60s default, but CI runs are slower)

SKIP_PATTERNS = [
    # known issues that aren't engine bugs (e.g., requires DOM rendering)
    "canvas",
    "webgl",
]

def run_cmd(cmd, timeout=TIMEOUT):
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return r.stdout, r.stderr, r.returncode
    except subprocess.TimeoutExpired:
        return "", "TIMEOUT", -1
    except Exception as e:
        return "", str(e), -1

def scan_html_file(path, server_port):
    """Scan a single fixture HTML file via HTTP server."""
    name = path.stem
    rel = path.relative_to(HERE)
    url = f"http://127.0.0.1:{server_port}/{rel.as_posix()}"

    stdout, stderr, rc = run_cmd([BINARY, "fetch", url, "--format", "text"], TIMEOUT)

    # Count JS errors in stderr
    js_errors = [l.strip() for l in stderr.splitlines() if "[quickjs]" in l or "[js]" in l]

    # Extract content (non-debug lines from stdout)
    content = "\n".join(l for l in stdout.splitlines()
                        if not l.startswith("[") and not l.startswith("Loading"))

    # Find PASS/FAIL markers
    passes = re.findall(r'(\w[\w-]*):PASS', content)
    fails = re.findall(r'(\w[\w-]*):FAIL(?:\(([^)]*)\))?', content)
    script_throws = re.findall(r'__SCRIPT_THREW__:?(.*)', content)

    return {
        "name": name,
        "passes": passes,
        "fails": [{"name": f[0], "detail": f[1]} for f in fails],
        "script_throws": script_throws[:3],
        "js_errors": js_errors[:5],
        "has_bug": bool(fails or script_throws or js_errors),
        "content_len": len(content.strip()),
        "rc": rc,
    }

def main():
    # Determine debug/release binary
    global BINARY
    if "--debug" in sys.argv:
        BINARY = str(ROOT / "target/debug/browser")

    if not os.path.exists(BINARY):
        print(f"[!] Binary not found: {BINARY}")
        print(f"    Build first: cargo build {'-p browser-cli' if '-p' in sys.argv else '--release -p browser-cli'}")
        sys.exit(1)

    # Collect fixture files
    categories_dir = HERE / "bug-hunt" / "categories"
    if not categories_dir.exists():
        print(f"[!] Fixtures not found at {categories_dir}")
        sys.exit(1)

    fixture_files = sorted(glob.glob(str(categories_dir / "*" / "*.html")))
    fixture_files = [Path(f) for f in fixture_files
                     if not any(p in Path(f).stem.lower() for p in SKIP_PATTERNS)]

    if not fixture_files:
        print(f"[!] No fixtures found in {categories_dir}")
        sys.exit(1)

    print(f"== JS Test Runner ==")
    print(f"  Binary: {BINARY}")
    print(f"  Fixtures: {len(fixture_files)}")
    print()

    # Start HTTP server
    import http.server, socketserver
    os.chdir(str(HERE))  # serve from tests/ so /bug-hunt/categories/ paths work
    server_port = 18877
    handler = http.server.SimpleHTTPRequestHandler

    # Suppress server log noise
    class QuietHandler(handler):
        def log_message(self, format, *args):
            pass

    httpd = socketserver.TCPServer(("", server_port), QuietHandler)
    httpd.timeout = 0.5
    server_thread = __import__("threading").Thread(target=httpd.serve_forever, daemon=True)
    server_thread.start()
    time.sleep(0.5)  # wait for server

    print(f"  HTTP Server: http://127.0.0.1:{server_port}/")
    print()

    # Run fixtures
    results = []
    total_passes = 0
    total_fails = 0

    for f in fixture_files:
        r = scan_html_file(f, server_port)

        if r["script_throws"]:
            icon = "💥 THREW"
        elif r["fails"]:
            icon = "🐛 FAIL"
        elif r["js_errors"]:
            icon = "⚠️  ERR"
        elif r["passes"]:
            icon = "✅ PASS"
        else:
            icon = "❓ ???"

        # Show per-fixture result
        summary = f"  {icon} {r['name']}"
        if r["passes"]:
            total_passes += len(r["passes"])
        if r["fails"]:
            total_fails += len(r["fails"])
        if r["has_bug"]:
            detail_parts = []
            if r["fails"]:
                detail_parts.append(f"{len(r['fails'])} asserts")
            if r["script_throws"]:
                detail_parts.append("threw")
            if r["js_errors"]:
                detail_parts.append(f"{len(r['js_errors'])} JS errs")
            summary += f" [{' '.join(detail_parts)}]"
        print(summary)
        results.append(r)

    httpd.shutdown()
    print()

    # Summary
    buggy = [r for r in results if r["has_bug"]]
    bug_free = [r for r in results if not r["has_bug"]]

    print(f"== Results ==")
    print(f"  Total fixtures: {len(results)}")
    print(f"  Bug-free:       {len(bug_free)}")
    print(f"  With bugs:      {len(buggy)}")
    print(f"  PASS assertions: {total_passes}")
    print(f"  FAIL assertions: {total_fails}")
    print()

    if buggy:
        print(f"== Bugs ({len(buggy)}) ==")
        for r in buggy:
            reasons = []
            if r["fails"]:
                reasons.append(f"FAIL({', '.join(f['name'] for f in r['fails'])})")
            if r["script_throws"]:
                reasons.append(f"THREW({', '.join(r['script_throws'])})")
            if r["js_errors"]:
                reasons.append(f"ERR({len(r['js_errors'])} JS)")

            print(f"  ❌ {r['name']}: {' '.join(reasons)}")

    # Exit code: 0 if no bugs, 1 if bugs found
    return 0 if not buggy else 1

if __name__ == "__main__":
    sys.exit(main())
