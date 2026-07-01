#!/usr/bin/env python3
"""Scan real sites for 100% syntax/parse errors only."""
import subprocess, sys, os, time

BROWSER = os.path.expanduser("~/Project/study-rust/browser/target/release/browser")

# 百分之百语法错误的关键词（精确匹配，不是正则）
SYNTAX_SUBSTRINGS = [
    "syntaxerror",
    "syntax error",
    "redeclaration of",
    "unexpected token",
    "unexpected identifier",
    "strict mode",
    "invalid property name",
    "invalid regular expression",
    "invalid left-hand side",
    "bad character",
    "bad number",
    "bad escape",
    "bigint",
    "missing ] after element",
    "missing ) after argument",
    "missing ; before statement",
    "missing : after property id",
    "expected expression,",
    "expected identifier,",
    "expected ';'",
    "expected ':'",
    "expected ','",
    "unexpected end of input",
    "unexpected number",
    "unexpected string",
    "is not a valid identifier",
]

# 百分之百运行时（NOT 语法错误）
RUNTIME_SUBSTRINGS = [
    "is not defined",
    "not a function",
    "cannot read property",
    "cannot set property",
    "of undefined",
    "of null",
    "is undefined",
    "not a constructor",
    "failed to fetch",
    "fetch failed",
    "timeout",
    "networkerror",
    "typeerror",
    "referenceerror",
    "rangeerror",
    "urierror",
    "internalerror",
    "is not a valid url",
    "cannot find module",
]

SITES = [
    "https://vuejs.org/", "https://react.dev/", "https://svelte.dev/",
    "https://nuxt.com/", "https://remix.run/", "https://angular.dev/",
    "https://lit.dev/", "https://preactjs.com/", "https://alpinejs.dev/",
    "https://solidjs.com/", "https://qwik.dev/", "https://mithril.js.org/",
    "https://markojs.com/", "https://stenciljs.com/", "https://aurelia.io/",
    "https://emberjs.com/", "https://astro.build/", "https://nextjs.org/",
    "https://github.com/", "https://baidu.com/",
]

def is_syntax_error(msg):
    m = msg.lower().strip()
    # 先排除百分百运行时
    for r in RUNTIME_SUBSTRINGS:
        if r in m:
            return False
    # 匹配语法关键词
    for s in SYNTAX_SUBSTRINGS:
        if s in m:
            return True
    # 特殊识别：let/const redeclaration
    if "let " in m and "redefine" in m:
        return True
    return False

def run_once(url, timeout=25):
    try:
        r = subprocess.run([BROWSER, "fetch", url, "--format", "text"],
            capture_output=True, text=True, timeout=timeout)
        errs = []
        for line in r.stderr.split("\n"):
            if "[js]" in line or "Error:" in line:
                line = line.strip()
                if line: errs.append(line)
        for line in r.stdout.split("\n"):
            if "SyntaxError" in line or "redeclaration" in line:
                errs.append(line.strip())
        return {"url": url, "content_len": len(r.stdout.strip()), "errors": errs, "exit": r.returncode}
    except subprocess.TimeoutExpired:
        return {"url": url, "content_len": 0, "errors": ["[timeout]"], "exit": -1}

def main():
    all_syntax = []
    print("=" * 60)
    print(" 真实站点 — 语法错误扫描")
    print("=" * 60)
    for url in SITES:
        name = url.split("//")[1].split("/")[0]
        result = run_once(url)
        syntax = [e for e in result["errors"] if is_syntax_error(e)]
        runtime = [e for e in result["errors"] if not is_syntax_error(e) and "Error" in e]
        if syntax:
            all_syntax.append((name, url, syntax))
            print(f"\n  {name}: ❌ {len(syntax)} 语法错误")
            for e in syntax: print(f"    {e[:140]}")
        elif runtime:
            print(f"\n  {name}: ⚡ {len(runtime)} 运行时（非语法）")
        else:
            print(f"\n  {name}: ✅ 干净")

    print("\n" + "=" * 60)
    if all_syntax:
        print(f" 语法错误汇总 ({len(all_syntax)} 个站)：")
        for name, url, errors in all_syntax:
            print(f"\n  ❌ {name}")
            for e in errors: print(f"    {e[:150]}")
    else:
        print(" ✅ 无语法错误。全部通过！")

if __name__ == "__main__":
    main()
