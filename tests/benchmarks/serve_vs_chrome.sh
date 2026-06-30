#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════════════════
# serve_vs_chrome.sh — browser serve vs Chrome headless 全维度对比
#
# 对比维度：
#   1. 提取内容字节数（markdown/text）
#   2. 内容完整性（completeness.py 4 指标）
#   3. 渲染耗时（serve timing vs Chrome wall clock）
#   4. HTML 结构一致性（DOM 序列化对比）
#
# 用法：
#   bash tests/benchmarks/serve_vs_chrome.sh [serve_url] [site1 site2 ...]
#   bash tests/benchmarks/serve_vs_chrome.sh  # 默认测 10 站
# ═══════════════════════════════════════════════════════════════════════

set -euo pipefail

SERVE_URL="${1:-http://127.0.0.1:3021}"
shift || true
CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
OUT_DIR="/tmp/serve_vs_chrome"
mkdir -p "$OUT_DIR"

# 默认测试站点（SSR + SPA 混合，覆盖主流框架/场景）
DEFAULT_SITES=(
  "https://example.com"
  "https://bark.day.app/"
  "https://docsify.js.org/"
  "https://react.dev"
  "https://nuxt.com"
  "https://vuejs.org"
  "https://svelte.dev"
  "https://go.dev"
  "https://todomvc.com/examples/vue/dist/"
  "https://todomvc.com/examples/backbone/"
  "https://docs.python.org/"
  "https://excalidraw.com/"
)

SITES=("${@:-${DEFAULT_SITES[@]}}")

echo "═══════════════════════════════════════════════════════════════════════"
echo "  browser serve vs Chrome headless 全维度对比"
echo "  serve: $SERVE_URL"
echo "  站点数: ${#SITES[@]}"
echo "═══════════════════════════════════════════════════════════════════════"
echo ""

# 输出表头
printf "%-25s | %-8s %-8s | %-8s %-8s | %-6s %-6s %-6s %-6s | %s\n" \
  "Site" "serve_ms" "chrome_s" "serve_ch" "chrome_ch" "blk" "sim" "jacc" "word" "grade"
printf "%s\n" "$(printf '%.0s─' {1..120})"

for site in "${SITES[@]}"; do
  name=$(echo "$site" | sed 's|https://||' | sed 's|/$||' | sed 's|/|_|g')
  short=$(echo "$name" | cut -c1-24)

  # ── 1. serve 提取（HTML 格式——和 Chrome dump-dom 同格式，apples-to-apples 对比）──
  serve_start=$(python3 -c "import time;print(time.time())")
  serve_json=$(curl -s --max-time 30 -X POST -H "Content-Type: application/json" \
    -d "{\"url\":\"$site\",\"format\":\"html\"}" "$SERVE_URL/" 2>/dev/null || echo '{}')
  serve_end=$(python3 -c "import time;print(time.time())")
  # 用 stdin 读 JSON（避免三引号嵌入被内容里的引号/特殊字符破坏）
  serve_ms=$(echo "$serve_json" | python3 -c "
import json,sys
try:
    d=json.load(sys.stdin)
    t=d.get('_timing',{})
    print(t.get('total_ms') or int(($serve_end-$serve_start)*1000))
except Exception:
    print(int(($serve_end-$serve_start)*1000))
" 2>/dev/null || echo "?")
  # 提取 content 字段并写文件（python 用 json.load 安全处理转义）
  echo "$serve_json" | python3 -c "import json,sys;d=json.load(sys.stdin);print(d.get('content',''),end='')" \
    > "$OUT_DIR/serve_${name}.html" 2>/dev/null || echo "" > "$OUT_DIR/serve_${name}.html"
  serve_chars=$(wc -c < "$OUT_DIR/serve_${name}.html" 2>/dev/null | tr -d ' ' || echo "0")

  # ── 2. Chrome 渲染 ──
  chrome_start=$(python3 -c "import time;print(time.time())")
  "$CHROME" --headless=new --virtual-time-budget=8000 --dump-dom "$site" \
    > "$OUT_DIR/chrome_${name}.html" 2>/dev/null || true
  chrome_end=$(python3 -c "import time;print(time.time())")
  chrome_s=$(python3 -c "print(f'{$chrome_end-$chrome_start:.1f}')")
  chrome_chars=$(wc -c < "$OUT_DIR/chrome_${name}.html" 2>/dev/null | tr -d ' ' || echo "0")

  # ── 3. 内容完整性对比（HTML vs HTML）──
  if [ -f "$OUT_DIR/chrome_${name}.html" ] && [ -s "$OUT_DIR/chrome_${name}.html" ] \
     && [ -s "$OUT_DIR/serve_${name}.html" ]; then
    completeness=$(python3 "$SCRIPT_DIR/completeness.py" \
      --ours "$OUT_DIR/serve_${name}.html" \
      --theirs "$OUT_DIR/chrome_${name}.html" \
      --grade 2>/dev/null | tail -1 || echo "0 0 0 0 ?")
    blk=$(echo "$completeness" | awk '{print $1}')
    sim=$(echo "$completeness" | awk '{print $2}')
    jacc=$(echo "$completeness" | awk '{print $3}')
    word=$(echo "$completeness" | awk '{print $4}')
    grade=$(echo "$completeness" | awk '{print $5}')
  else
    blk="-"; sim="-"; jacc="-"; word="-"; grade="-"
  fi

  # ── 输出行 ──
  printf "%-25s | %5sms  %6ss  | %6s   %6s   | %s\n" \
    "$short" "$serve_ms" "$chrome_s" "$serve_chars" "$chrome_chars" \
    "$blk $sim $jacc $word $grade"

done

echo ""
echo "═══════════════════════════════════════════════════════════════════════"
echo "  指标说明:"
echo "    serve_ms  = serve 后端耗时（含网络+JS+提取）"
echo "    chrome_s  = Chrome headless 总耗时"
echo "    serve_ch  = serve 提取的 HTML 字节数"
echo "    chrome_ch = Chrome dump-dom 的 HTML 字节数"
echo "    blk/sim/jacc/word = completeness.py 4 指标（值域 [0,1]，越高越好）"
echo "    grade = 综合评级（A≥0.85 B≥0.70 C≥0.55 D≥0.35 F<0.35）"
echo ""
echo "  产物保存在: $OUT_DIR/"
echo "═══════════════════════════════════════════════════════════════════════"
