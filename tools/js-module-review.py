#!/usr/bin/env python3
"""
JS Code Module Mapper
Maps JS shim code locations to logical modules.
Run during weekly reviews to detect drift.
"""
import re, subprocess, os
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FILES = {
    "js-runtime/scripts.rs": ROOT / "crates/js-runtime/src/scripts.rs",
    "js-runtime/bridge.rs": ROOT / "crates/js-runtime/src/bridge.rs",
    "js-runtime/engine_quickjs.rs": ROOT / "crates/js-runtime/src/engine_quickjs.rs",
    "cli/main.rs": ROOT / "crates/cli/src/main.rs",
}

MODULES = {
    "JS Engine": {
        "desc": "QuickJS/boa 引擎初始化、eval、module loader",
        "files": ["js-runtime/engine_quickjs.rs"],
        "markers": ["QuickJsEngine", "HttpLoader", "HttpResolver", "eval_module", "eval_user_script"],
    },
    "Global Shim": {
        "desc": "全局对象（window/console/Event/Blob/TextEncoder 等）",
        "files": ["js-runtime/scripts.rs"],
        "markers": ["QUICKJS_GLOBAL_SHIM", "QUICKJS_GLOBAL_SHIM"],
        "line_range": (1344, 2202),
    },
    "Element Shim": {
        "desc": "Element.prototype 方法、DOM 操作",
        "files": ["js-runtime/scripts.rs"],
        "markers": ["QUICKJS_ELEMENT_SHIM"],
        "line_range": (2202, 2849),
    },
    "Document Shim": {
        "desc": "document 对象方法",
        "files": ["js-runtime/scripts.rs"],
        "markers": ["QUICKJS_DOCUMENT_SHIM"],
        "line_range": (2849, 2965),
    },
    "XHR Shim": {
        "desc": "XMLHttpRequest 实现",
        "files": ["js-runtime/scripts.rs"],
        "markers": ["QUICKJS_XHR_SHIM"],
        "line_range": (2965, None),
    },
    "Bridge (shared)": {
        "desc": "Rust ↔ JS bridge 函数（DOM 读写、fetch、network）",
        "files": ["js-runtime/bridge.rs"],
        "markers": ["fn.*bridge", "pub fn"],
    },
    "CLI": {
        "desc": "命令行解析、fetch 命令、输出格式",
        "files": ["cli/main.rs"],
        "markers": ["Fetch {", "render_html_to_string"],
    },
}

def line_count(filepath):
    with open(filepath) as f:
        return sum(1 for _ in f)

def get_git_changes(filepath):
    """Get lines changed in last 7 days."""
    try:
        r = subprocess.run(
            ["git", "log", "--since=7.days", "--oneline", "--", str(filepath)],
            capture_output=True, text=True, cwd=ROOT
        )
        return r.stdout.strip()
    except:
        return ""

def analyze_module(mod_name, mod_info):
    """Check module for drift."""
    total_lines = 0
    markers_found = []
    recent_changes = []
    
    for fname in mod_info["files"]:
        fpath = FILES.get(fname)
        if fpath and fpath.exists():
            total_lines += line_count(fpath)
            changes = get_git_changes(fpath)
            if changes:
                recent_changes.append(f"{fname}: {changes}")
    
    for marker in mod_info["markers"]:
        for fname in mod_info["files"]:
            fpath = FILES.get(fname)
            if fpath and fpath.exists():
                with open(fpath) as f:
                    content = f.read()
                    count = len(re.findall(marker, content))
                    if count > 0:
                        markers_found.append(f"  {marker}: {count} hits")
    
    return {
        "total_lines": total_lines,
        "markers": markers_found,
        "recent_changes": recent_changes,
    }

def main():
    print("=" * 60)
    print("  JS Code Module Review")
    print("=" * 60)
    print()
    
    all_ok = True
    for mod_name, mod_info in MODULES.items():
        result = analyze_module(mod_name, mod_info)
        
        print(f"\n## {mod_name}")
        print(f"   {mod_info['desc']}")
        print(f"   Lines: {result['total_lines']}")
        
        if result["markers"]:
            print("   Markers found:")
            for m in result["markers"]:
                print(f"    {m}")
        else:
            print("   ⚠️  No module markers found!")
            all_ok = False
        
        if result["recent_changes"]:
            print("   Recent changes (7d):")
            for c in result["recent_changes"]:
                print(f"    📝 {c}")
        
        # Check line range drift
        if "line_range" in mod_info:
            lo, hi = mod_info["line_range"]
            fpath = FILES.get(mod_info["files"][0])
            if fpath and fpath.exists():
                total = line_count(fpath)
                if hi and total > hi + 50:
                    print(f"   ⚠️  Module exceeds expected line range! ({total} > {hi})")
                    print(f"       Consider splitting: {mod_info['files'][0]} lines {hi}-{total} → new module")
                    all_ok = False
    
    print()
    print("=" * 60)
    if all_ok:
        print("  ✅ All modules within expected boundaries")
    else:
        print("  ⚠️  Action items identified above")
    print("=" * 60)
    
    return 0 if all_ok else 1

if __name__ == "__main__":
    exit(main())
