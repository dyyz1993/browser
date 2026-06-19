#!/usr/bin/env bash
# M65: CSR 全面对比基准 —— browser(rust) vs Chrome headless
# 维度：渲染时间 | 峰值内存(RSS) | 内容大小(字符) | DOM 节点数(标签数)
# 站点：10+ 常见 CSR 网站
#
# 用法：bash tests/benchmarks/csr_benchmark.sh [--quick]
# --quick: 每站只跑 1 次（默认 2 次取最快）

set -uo pipefail

CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
BROWSER="./target/release/browser"
QUICK=false
[[ "${1:-}" == "--quick" ]] && QUICK=true

# 10+ CSR 站点（ESM + 非 ESM + 框架覆盖）
SITES=(
  "https://bark.day.app/"
  "https://vuejs.org/"
  "https://vite.dev/"
  "https://nuxt.com/"
  "https://svelte.dev/"
  "https://react.dev/"
  "https://nextjs.org/"
  "https://www.solidjs.com/"
  "https://qwik.dev/"
  "https://astro.build/"
  "https://remix.run/"
  "https://docusaurus.io/"
)

# 时间测量函数
measure_time() {
  local t0=$(python3 -c "import time;print(time.time())")
  "$@" >/dev/null 2>&1
  python3 -c "import time;print(f'{time.time()-$t0:.2f}')"
}

# 内存测量（macOS /usr/bin/time -l → maximum resident set size in bytes）
measure_rss_mb() {
  /usr/bin/time -lp "$@" >/dev/null 2>&1
  # /usr/bin/time 输出到 stderr，但我们重定向了。改用包装。
  local out
  out=$(/usr/bin/time -lp "$@" 2>&1 >/dev/null)
  echo "$out" | grep "maximum resident" | awk '{printf "%.0f", $1/1024/1024}'
}

# 提取 DOM 节点数（HTML 标签数）
count_dom_nodes() {
  python3 -c "
import sys, re
html = sys.stdin.read()
# 去掉 script/style 内容
html = re.sub(r'<script[^>]*>.*?</script>', '', html, flags=re.DOTALL)
html = re.sub(r'<style[^>]*>.*?</style>', '', html, flags=re.DOTALL)
# 统计标签数（开标签 + 自闭合）
tags = re.findall(r'<[a-zA-Z][^>]*>', html)
print(len(tags))
"
}

# 提取纯文本字符数
count_text_chars() {
  python3 -c "
import sys, re
html = sys.stdin.read()
html = re.sub(r'<script[^>]*>.*?</script>', '', html, flags=re.DOTALL)
html = re.sub(r'<style[^>]*>.*?</style>', '', html, flags=re.DOTALL)
text = re.sub(r'<[^>]+>', ' ', html)
text = re.sub(r'\s+', ' ', text).strip()
print(len(text))
"
}

# 从 Chrome --dump-dom 输出提取 HTML
chrome_dump_html() {
  "$CHROME" --headless=new --disable-gpu --no-sandbox \
    --virtual-time-budget=8000 --dump-dom "$1" 2>/dev/null
}

echo "================================================================"
echo "  CSR 全面对比：browser(rust) vs Chrome headless"
echo "  维度：时间(s) | 内存(MB) | 内容字符数 | DOM节点数"
echo "  $([ "$QUICK" = true ] && echo '快速模式(1次)' || echo '标准模式(2次取最快)')"
echo "================================================================"
echo ""

# 表头
printf "%-22s | %8s %8s | %8s %8s | %10s %10s | %8s %8s | %s\n" \
  "站点" \
  "我们时间" "Chr时间" \
  "我们RSS" "ChrRSS" \
  "我们内容" "Chr内容" \
  "我们DOM" "ChrDOM" \
  "判定"
printf '%.0s-' {1..130}; echo

our_wins=0
chr_wins=0
total=0

for url in "${SITES[@]}"; do
  short=$(echo "$url" | sed 's|https://*||; s|https://||; s|/$||; s|^www\.||')
  total=$((total + 1))

  # === 我们 ===
  if [ "$QUICK" = true ]; then
    our_t=$(measure_time "$BROWSER" fetch "$url" --format text)
  else
    t1=$(measure_time "$BROWSER" fetch "$url" --format text)
    t2=$(measure_time "$BROWSER" fetch "$url" --format text)
    our_t=$(python3 -c "print(min($t1,$t2))")
  fi
  our_rss=$(/usr/bin/time -lp "$BROWSER" fetch "$url" --format text 2>&1 >/dev/null | grep "maximum resident" | awk '{printf "%.0f", $1/1024/1024}')
  our_html=$("$BROWSER" fetch "$url" --format html 2>/dev/null)
  our_content=$(echo "$our_html" | count_text_chars)
  our_dom=$(echo "$our_html" | count_dom_nodes)

  # === Chrome ===
  if [ "$QUICK" = true ]; then
    chr_t=$(measure_time "$CHROME" --headless=new --disable-gpu --no-sandbox --virtual-time-budget=8000 --dump-dom "$url")
  else
    c1=$(measure_time "$CHROME" --headless=new --disable-gpu --no-sandbox --virtual-time-budget=8000 --dump-dom "$url")
    c2=$(measure_time "$CHROME" --headless=new --disable-gpu --no-sandbox --virtual-time-budget=8000 --dump-dom "$url")
    chr_t=$(python3 -c "print(min($c1,$c2))")
  fi
  chr_rss=$(/usr/bin/time -lp "$CHROME" --headless=new --disable-gpu --no-sandbox --virtual-time-budget=8000 --dump-dom "$url" 2>&1 >/dev/null | grep "maximum resident" | awk '{printf "%.0f", $1/1024/1024}')
  chr_html=$(chrome_dump_html "$url")
  chr_content=$(echo "$chr_html" | count_text_chars)
  chr_dom=$(echo "$chr_html" | count_dom_nodes)

  # 判定（时间为主）
  verdict=$(python3 -c "
w_t=$our_t; c_t=$chr_t
w_m=${our_rss:-0}; c_m=${chr_rss:-0}
if w_t < c_t:
    print(f'⚡快{c_t/w_t:.1f}x')
else:
    print(f'慢{w_t/c_t:.1f}x')
" 2>/dev/null || echo "?")

  # 统计胜负
  if python3 -c "exit(0 if $our_t < $chr_t else 1)" 2>/dev/null; then
    our_wins=$((our_wins + 1))
  else
    chr_wins=$((chr_wins + 1))
  fi

  printf "%-22s | %7ss %7ss | %6sMB %6sMB | %9s %9s | %7s %7s | %s\n" \
    "$short" \
    "$our_t" "$chr_t" \
    "$our_rss" "$chr_rss" \
    "$our_content" "$chr_content" \
    "$our_dom" "$chr_dom" \
    "$verdict"
done

echo ""
echo "================================================================"
echo "  汇总：$total 站 | 我们更快 $our_wins 站 | Chrome 更快 $chr_wins 站"
echo "================================================================"
