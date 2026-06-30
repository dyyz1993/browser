#!/usr/bin/env python3
"""用我们的浏览器跑 8 档，逐行对比 Chrome baseline，列出所有差异。
这是"一个个覆盖"的核心工具：每个不一致行 = 一个要修的缺口。"""
import subprocess, re, os, difflib, sys

BIN = "/Users/xuyingzhou/Project/study-rust/browser-work-m71.1/target/release/browser"
BASE = "http://127.0.0.1:8765"
DIR = os.path.dirname(__file__)
BLDIR = os.path.join(DIR, "baseline")

LEVELS = ["07-iframe", "08-fragment", "09-css-selectors", "10-css3",
          "11-canvas", "12-webgl", "13-vdom", "14-performance"]

def ours_out(url):
    """我们: fetch --format html -> 提取 <pre id=out> 文本"""
    out = subprocess.run(
        [BIN, "fetch", url, "--format", "html"],
        capture_output=True, timeout=30
    )
    html = out.stdout.decode("utf-8", errors="replace")
    err = out.stderr.decode("utf-8", errors="replace")
    m = re.search(r'<pre[^>]*id="out"[^>]*>(.*?)</pre>', html, re.DOTALL)
    text = re.sub(r'<[^>]+>', '', m.group(1)).strip() if m else ""
    # 收集 JS 错误
    jserr = []
    for line in err.split('\n'):
        if 'message=' in line or 'is not defined' in line or 'THREW' in line:
            mm = re.search(r'(?:message=|Error: )([^\|]+)', line)
            if mm:
                jserr.append(mm.group(1).strip())
    return text, list(dict.fromkeys(jserr))  # 去重保序

def main():
    print("=" * 70)
    print(" 我们(QuickJS) vs Chrome baseline 逐行对比")
    print("=" * 70)
    summary = []
    all_gaps = []
    for lvl in LEVELS:
        blpath = os.path.join(BLDIR, f"{lvl}.txt")
        with open(blpath) as f:
            baseline = f.read().strip()
        bl_lines = [l for l in baseline.split('\n') if l.strip()]

        url = f"{BASE}/{lvl}.html"
        ours, jserr = ours_out(url)
        o_lines = [l for l in ours.split('\n') if l.strip()]

        ratio = difflib.SequenceMatcher(None, baseline, ours).ratio()
        summary.append((lvl, ratio, len(bl_lines), len(o_lines), len(jserr)))

        print(f"\n{'═'*68}")
        print(f"{lvl}   相似度 {ratio*100:.0f}%  (baseline {len(bl_lines)}行 / ours {len(o_lines)}行 / JS错误 {len(jserr)})")
        print(f"{'─'*68}")
        # 逐行对比：baseline 每行，我们有没有匹配
        ours_set = set(o_lines)
        gaps = []
        for bl in bl_lines:
            if bl in ours_set:
                print(f"  ✓ {bl[:64]}")
            else:
                # 模糊匹配找近似
                best = max(o_lines, key=lambda o: difflib.SequenceMatcher(None, bl, o).ratio()) if o_lines else ""
                br = difflib.SequenceMatcher(None, bl, best).ratio() if best else 0
                if br > 0.5:
                    print(f"  ≈ C|{bl[:60]}")
                    print(f"    O|{best[:60]}")
                    gaps.append((bl, best))
                else:
                    print(f"  ✗ MISSING: {bl[:60]}")
                    gaps.append((bl, None))
        # 我们多出来的行
        bl_set = set(bl_lines)
        extra = [o for o in o_lines if o not in bl_set]
        if extra:
            print(f"  --- ours 多出 ({len(extra)}行) ---")
            for e in extra[:5]:
                print(f"  + {e[:60]}")
        if jserr:
            print(f"  --- JS 错误 ({len(jserr)}) ---")
            for e in jserr[:5]:
                print(f"  ! {e[:60]}")
        all_gaps.append((lvl, gaps, jserr))

    # 汇总
    print(f"\n{'='*70}")
    print(f"{'档位':<20}{'相似度':>8}{'base':>7}{'ours':>7}{'JSerr':>7}")
    print(f"{'-'*70}")
    tot = 0
    for lvl, r, b, o, je in summary:
        print(f"{lvl:<20}{r*100:>7.0f}%{b:>7}{o:>7}{je:>7}")
        tot += r
    print(f"{'-'*70}")
    print(f"{'平均':<20}{tot/len(summary)*100:>7.0f}%")
    print(f"{'='*70}")

    # 缺口汇总（MISSING 的）
    print(f"\n缺口汇总（MISSING 行 = 必修）:")
    for lvl, gaps, jserr in all_gaps:
        missing = [g[0] for g in gaps if g[1] is None]
        if missing or jserr:
            print(f"\n  [{lvl}]")
            for m in missing:
                print(f"    - {m}")
            for e in jserr:
                print(f"    ! JS: {e}")

if __name__ == "__main__":
    main()
