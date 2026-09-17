#!/usr/bin/env bash
# bdiag — browser diagnose（M97.22）
#
# 一行命令快速诊断任意页面：console 日志、未捕获 JS 异常（带调用栈）、
# 网络事件、内容告警。封装 `browser fetch --json --only ...`，输出着色分组。
#
# 用法:
#   bdiag <url>                     # 快速诊断：console + errors + warnings
#   bdiag <url> --network           # 附带 JS 请求表（方法/状态码/URL）
#   bdiag <url> --full              # 全量：加正文提取（title + content 预览）
#   bdiag <url> --raw               # 原始 JSON 输出（自己接 jq）
#   bdiag <url> --engine boa        # 换 JS 引擎对照（默认 quickjs）
#
# 退出码: 0 = 健康；1 = 有未捕获异常或 console.error；2 = 抓取失败。
# 适合 CI / 脚本里做 SPA 渲染健康检查。
#
# 安装: cp tools/bdiag.sh /usr/local/bin/bdiag && chmod +x /usr/local/bin/bdiag
# 二进制定位: $BROWSER_BIN > PATH 里的 browser > 默认仓库 release 路径

set -euo pipefail

BROWSER_BIN="${BROWSER_BIN:-}"
if [[ -z "$BROWSER_BIN" ]]; then
    if command -v browser >/dev/null 2>&1; then
        BROWSER_BIN="$(command -v browser)"
    elif [[ -x "$HOME/Project/study-rust/browser/target/release/browser" ]]; then
        BROWSER_BIN="$HOME/Project/study-rust/browser/target/release/browser"
    else
        echo "bdiag: 找不到 browser 二进制（设置 BROWSER_BIN 或把 browser 加入 PATH）" >&2
        exit 2
    fi
fi

FIELDS="console,errors,warnings"
ENGINE="quickjs"
TIMEOUT_MS=30000
RAW=0
EXTRA_ARGS=()
URL=""

usage() {
    sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --network) FIELDS="$FIELDS,network"; shift ;;
        --full)    FIELDS="url,title,$FIELDS,network,content"; shift ;;
        --content) FIELDS="url,title,$FIELDS,content"; shift ;;
        --raw)     RAW=1; shift ;;
        --engine)  ENGINE="${2:?--engine 需要参数}"; shift 2 ;;
        --timeout) TIMEOUT_MS="${2:?--timeout 需要参数}"; shift 2 ;;
        --lines)   EXTRA_ARGS+=("--bdiag-lines" "${2:?--lines 需要参数}"); shift 2 ;;
        -h|--help) usage ;;
        -*)        echo "bdiag: 未知选项 $1" >&2; usage ;;
        *)         URL="$1"; shift ;;
    esac
done

[[ -n "$URL" ]] || { echo "bdiag: 缺少 URL 参数（bdiag <url>）" >&2; usage; }

START=$(date +%s)
# set -e 下 grep 无匹配会以 1 退出并杀掉脚本，这里手动捕获退出码
set +e
JSON="$("$BROWSER_BIN" fetch "$URL" --json --only "$FIELDS" \
        --js-engine "$ENGINE" --timeout-ms "$TIMEOUT_MS" 2>/tmp/bdiag.err.$$)"
RC=$?
set -e
if [[ $RC -ne 0 ]]; then
    echo "bdiag: 抓取失败（browser exit ${RC}）" >&2
    sed 's/^/  stderr: /' /tmp/bdiag.err.$$ >&2 | head -20
    rm -f /tmp/bdiag.err.$$
    exit 2
fi
ELAPSED=$(( $(date +%s) - START ))
ERRTAIL=$(grep '^\[js\]' /tmp/bdiag.err.$$ | head -5 || true)
rm -f /tmp/bdiag.err.$$

if [[ $RAW -eq 1 ]]; then
    printf '%s\n' "$JSON"
    exit 0
fi

# python3 格式化输出（macOS 自带）；退出码 0/1 由诊断结果决定。
# 注意：程序必须放临时文件——heredoc 会占用 stdin，管道数据进不去。
PYTMP="$(mktemp /tmp/bdiag.py.XXXXXX)"
trap 'rm -f "$PYTMP" /tmp/bdiag.err.* 2>/dev/null' EXIT
cat > "$PYTMP" <<'PYEOF'
import json, sys

url, engine, elapsed = sys.argv[1], sys.argv[2], sys.argv[3]
content_lines = 10
args = sys.argv[4:]
if "--bdiag-lines" in args:
    try:
        content_lines = int(args[args.index("--bdiag-lines") + 1])
    except (ValueError, IndexError):
        pass

d = json.load(sys.stdin)
tty = sys.stdout.isatty()
C = {
    "r": "\033[31m" if tty else "", "g": "\033[32m" if tty else "",
    "y": "\033[33m" if tty else "", "b": "\033[34m" if tty else "",
    "dim": "\033[2m" if tty else "", "bold": "\033[1m" if tty else "",
    "off": "\033[0m" if tty else "",
}

def sec(title):
    print(f"\n{C['bold']}── {title} {'─' * max(0, 58 - len(title))}{C['off']}")

print(f"{C['bold']}⚡ bdiag{C['off']} {C['dim']}· {engine} · {elapsed}s{C['off']}")
if "url" in d:
    print(f"   {C['dim']}{d.get('url') or url}{C['off']}")
if "title" in d and d.get("title"):
    print(f"   title: {d['title']}")

problems = False

# console
console = d.get("console", [])
if console or "console" in d:
    by = {}
    for e in console:
        by.setdefault(e["level"], []).append(e["text"])
    n_err, n_warn, n_log = len(by.get("error", [])), len(by.get("warn", [])), len(by.get("log", []))
    icon = f"{C['r']}✗{C['off']}" if n_err else f"{C['g']}✓{C['off']}"
    print(f"\n{icon} console: {len(console)} 条（error {n_err} / warn {n_warn} / log {n_log}）")
    if n_err:
        problems = True
        for t in by["error"][:8]:
            print(f"   {C['r']}[error]{C['off']} {t[:160]}")
        if len(by["error"]) > 8:
            print(f"   {C['dim']}… 还有 {len(by['error']) - 8} 条 error{C['off']}")
    for t in by.get("warn", [])[:5]:
        print(f"   {C['y']}[warn] {C['off']}{t[:160]}")
    if len(by.get("warn", [])) > 5:
        print(f"   {C['dim']}… 还有 {len(by['warn']) - 5} 条 warn{C['off']}")
    for t in by.get("log", [])[:3]:
        print(f"   {C['dim']}[log]  {t[:120]}{C['off']}")

# errors（未捕获异常）
errors = d.get("errors", [])
if errors or "errors" in d:
    sec("未捕获 JS 异常")
    if not errors:
        print(f"   {C['g']}无{C['off']}")
    else:
        problems = True
        for i, e in enumerate(errors, 1):
            print(f"   {C['r']}{i}.{C['off']} {C['bold']}{e['message'][:160]}{C['off']}")
            for line in (e.get("stack") or "").splitlines()[1:3]:
                if line.strip():
                    print(f"      {C['dim']}{line.strip()[:150]}{C['off']}")

# network
if "network" in d:
    net = d["network"]
    sec(f"JS 网络请求（{len(net)} 条）")
    if not net:
        print(f"   {C['dim']}无{C['off']}")
    for e in net[:15]:
        sc = e.get("status") or 0
        color = C["g"] if 200 <= sc < 300 else (C["y"] if sc == 304 else C["r"])
        print(f"   {color}{sc:>3}{C['off']} {e.get('method','?'):<4} {(e.get('url') or '')[:110]}")
    if len(net) > 15:
        print(f"   {C['dim']}… 还有 {len(net) - 15} 条{C['off']}")

# warnings
warns = d.get("warnings", [])
if warns:
    sec("内容告警")
    for w in warns:
        print(f"   {C['y']}⚠ {w}{C['off']}")

# content 预览
if "content" in d:
    text = (d["content"].get("text") or "").strip()
    sec(f"正文预览（{d['content'].get('format')}, {len(text)} 字符）")
    for line in [l for l in text.splitlines() if l.strip()][:content_lines]:
        print(f"   {line[:150]}")

print()
if problems:
    print(f"{C['r']}✗ 存在异常/console error —— 渲染不健康{C['off']}")
    sys.exit(1)
print(f"{C['g']}✓ 渲染健康：无未捕获异常，console 无 error{C['off']}")
PYEOF
printf '%s' "$JSON" | python3 "$PYTMP" "$URL" "$ENGINE" "$ELAPSED" ${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}
EXIT=$?
[[ -n "$ERRTAIL" && $EXIT -ne 2 ]] && printf '%s\n' "$ERRTAIL" | sed 's/^/  (stderr) /' >&2
exit $EXIT
