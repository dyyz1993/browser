#!/usr/bin/env bash
# M66 完整对标 Chrome 测试套件
# 5 维度：内存 | 速度 | 内容覆盖 | 错误 | 并发
# 全部对标 Chrome headless
set -uo pipefail

BROWSER="./target/release/browser"
CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"

# 默认 5 站；可通过环境变量 SITES_COUNT 扩展（如 SITES_COUNT=10）
ALL_SITES=(
  "https://nuxt.com/"
  "https://svelte.dev/"
  "https://vite.dev/"
  "https://vuejs.org/"
  "https://react.dev/"
  "https://remix.run/"
  "https://astro.build/"
  "https://docusaurus.io/"
  "https://qwik.dev/"
  "https://nextjs.org/"
)

SITES_COUNT="${SITES_COUNT:-5}"
SITES=("${ALL_SITES[@]:0:$SITES_COUNT}")
count=${#SITES[@]}

echo ""
echo "================================================================"
echo "  QuickJS vs Chrome headless —— 完整对标测试"
echo "  维度：内存 | 速度 | 内容覆盖 | 错误 | 并发"
echo "  站点：${count} 个 CSR 网站"
echo "  时间：$(date '+%Y-%m-%d %H:%M:%S')"
echo "================================================================"

# 工具函数：提取 Chrome DOM 文本字符数
chrome_text_chars() {
  "$CHROME" --headless=new --disable-gpu --no-sandbox \
    --virtual-time-budget=8000 --dump-dom "$1" 2>/dev/null | \
    python3 -c "
import sys,re
h=sys.stdin.read()
if not h: print(0); exit()
h=re.sub(r'<script[^>]*>.*?</script>','',h,flags=re.DOTALL)
h=re.sub(r'<style[^>]*>.*?</style>','',h,flags=re.DOTALL)
t=re.sub(r'<[^>]+>',' ',h)
t=re.sub(r'\s+',' ',t).strip()
print(len(t))
" 2>/dev/null || echo 0
}

# 工具函数：测量时间
measure_time() {
  local t0=$(python3 -c "import time;print(time.time())")
  "$@" >/dev/null 2>&1
  local rc=$?
  local t1=$(python3 -c "import time;print(time.time())")
  python3 -c "print(f'{$t1-$t0:.1f}')"
  return $rc
}

############################################################
echo ""
echo "━━━ 1. 内存对标（峰值 RSS MB，越低越好）━━━━━━━━━━━━━━━━"
printf "  %-16s | %8s %8s | %s\n" "站点" "QuickJS" "Chrome" "内存比"
printf '  %.0s-' {1..55}; echo

# Chrome 内存：用 ps 采样峰值
chrome_peak_rss() {
  "$CHROME" --headless=new --disable-gpu --no-sandbox --virtual-time-budget=8000 --dump-dom "$1" >/dev/null 2>/dev/null &
  local _cpid=$!
  local _peak=0
  while kill -0 $_cpid 2>/dev/null; do
    local _rss=$(ps -o rss= -p $_cpid 2>/dev/null | awk '{printf "%.0f", $1/1024}')
    [ -n "$_rss" ] && [ "$_rss" -gt "$_peak" ] 2>/dev/null && _peak=$_rss
    sleep 0.3
  done
  echo "$_peak"
}

qjs_mem_sum=0
chr_mem_sum=0
valid=0

for url in "${SITES[@]}"; do
  short=$(echo "$url" | sed 's|https://||;s|/$||')

  # QuickJS 内存：用 --profile flag（内部 mach API，可靠）
  q_mem=$($BROWSER fetch "$url" --format text --profile 2>&1 >/dev/null | grep "TOTAL" | grep -oE '[0-9]+MB' | head -1 | tr -d 'MB')
  # Chrome 内存：用 ps 采样峰值
  c_mem=$(chrome_peak_rss "$url")
  
  ratio="—"
  if [ -n "$q_mem" ] && [ -n "$c_mem" ] && [ "$q_mem" -gt 0 ] 2>/dev/null && [ "$c_mem" -gt 0 ] 2>/dev/null; then
    ratio=$(python3 -c "print(f'1/{$c_mem//$q_mem}')")
    qjs_mem_sum=$((qjs_mem_sum + q_mem))
    chr_mem_sum=$((chr_mem_sum + c_mem))
    valid=$((valid + 1))
  fi
  
  printf "  %-16s | %6sMB %6sMB | %s\n" "$short" "${q_mem:-—}" "${c_mem:-—}" "$ratio"
done

if [ $valid -gt 0 ]; then
  printf "  %-16s | %6sMB %6sMB | 平均\n" "平均" $((qjs_mem_sum/valid)) $((chr_mem_sum/valid))
fi

############################################################
echo ""
echo "━━━ 2. 速度对标（渲染时间秒，越低越好）━━━━━━━━━━━━━━━━"
printf "  %-16s | %8s %8s | %s\n" "站点" "QuickJS" "Chrome" "速度比"
printf '  %.0s-' {1..55}; echo

qjs_speed_wins=0

for url in "${SITES[@]}"; do
  short=$(echo "$url" | sed 's|https://||;s|/$||')
  
  q_t=$(measure_time $BROWSER fetch "$url" --format text)
  c_t=$(measure_time "$CHROME" --headless=new --disable-gpu --no-sandbox --virtual-time-budget=8000 --dump-dom "$url")
  
  verdict="—"
  if python3 -c "exit(0 if $q_t < $c_t else 1)" 2>/dev/null; then
    verdict=$(python3 -c "print(f'QJS快{$c_t/$q_t:.1f}x')")
    qjs_speed_wins=$((qjs_speed_wins + 1))
  else
    verdict=$(python3 -c "print(f'Chrome快{$q_t/$c_t:.1f}x')" 2>/dev/null)
  fi
  
  printf "  %-16s | %6ss %6ss | %s\n" "$short" "$q_t" "$c_t" "$verdict"
done

echo "  QuickJS 速度领先: ${qjs_speed_wins}/${count} 站"

############################################################
echo ""
echo "━━━ 3. 内容覆盖对标（文本字符数，越接近 Chrome 越好）━━━━"
printf "  %-16s | %8s %8s | %s\n" "站点" "QuickJS" "Chrome" "覆盖率"
printf '  %.0s-' {1..55}; echo

coverage_sum=0
coverage_valid=0

for url in "${SITES[@]}"; do
  short=$(echo "$url" | sed 's|https://||;s|/$||')
  
  q_chars=$($BROWSER fetch "$url" --format text 2>/dev/null | wc -c | tr -d ' ')
  c_chars=$(chrome_text_chars "$url")
  
  cov="—"
  if [ "$c_chars" -gt 0 ] 2>/dev/null && [ "$q_chars" -gt 1 ] 2>/dev/null; then
    cov=$(python3 -c "print(f'{$q_chars/$c_chars*100:.0f}%')")
    coverage_sum=$(python3 -c "print($coverage_sum + $q_chars/$c_chars*100)")
    coverage_valid=$((coverage_valid + 1))
  elif [ "$q_chars" -gt 1 ] 2>/dev/null; then
    cov="有内容"
  else
    cov="❌空"
  fi
  
  printf "  %-16s | %7s %7s | %s\n" "$short" "$q_chars" "$c_chars" "$cov"
done

if [ $coverage_valid -gt 0 ]; then
  avg_cov=$(python3 -c "print(f'{$coverage_sum/$coverage_valid:.0f}%')")
  echo "  平均覆盖率: ${avg_cov}"
fi

############################################################
echo ""
echo "━━━ 4. 错误对标（JS 错误数 + 崩溃，越少越好）━━━━━━━━━━━"
printf "  %-16s | %12s | %12s | %s\n" "站点" "QuickJS" "Chrome" "状态"
printf '  %.0s-' {1..60}; echo

qjs_clean=0
chr_clean=0

for url in "${SITES[@]}"; do
  short=$(echo "$url" | sed 's|https://||;s|/$||')

  # QuickJS 错误（只抓 JS 引擎报错，过滤网络/进度噪声）
  q_err=$($BROWSER fetch "$url" --format text 2>&1 1>/dev/null | grep -oE '\[quickjs\].*Error|\[js\].*Error' | wc -l | tr -d ' ')
  [ -z "$q_err" ] && q_err=0
  q_crash=0
  $BROWSER fetch "$url" --format text >/dev/null 2>&1 || q_crash=$?

  # Chrome 错误（stderr 里 JS 相关的 Uncaught/error）
  c_err=$("$CHROME" --headless=new --disable-gpu --no-sandbox --virtual-time-budget=8000 --dump-dom "$url" 2>&1 1>/dev/null | grep -ciE "Uncaught|ReferenceError|TypeError|SyntaxError" || true)
  c_err=$(echo "$c_err" | tr -d ' \n')
  [ -z "$c_err" ] && c_err=0

  q_status="✅"
  [ "$q_err" != "0" ] && q_status="⚠️${q_err}错"
  [ "$q_crash" != "0" ] && q_status="❌崩溃"
  [ "$q_err" = "0" ] && [ "$q_crash" = "0" ] && qjs_clean=$((qjs_clean + 1))

  c_status="✅"
  [ "$c_err" != "0" ] && c_status="⚠️${c_err}行"
  [ "$c_err" = "0" ] && chr_clean=$((chr_clean + 1))
  
  printf "  %-16s | %10s | %10s | QJS:%s Chr:%s\n" "$short" "$q_status" "$c_status" "$q_status" "$c_status"
done

echo "  QuickJS 0错误: ${qjs_clean}/${count} | Chrome 0错误: ${chr_clean}/${count}"

############################################################
echo ""
echo "━━━ 5. 并发对标（3 个并发请求总时间）━━━━━━━━━━━━━━━━━━"
CONCURRENT_URLS=("https://svelte.dev/" "https://remix.run/" "https://qwik.dev/")

echo "  并发站点: ${CONCURRENT_URLS[*]}"

# QuickJS 并发
t0=$(python3 -c "import time;print(time.time())")
for u in "${CONCURRENT_URLS[@]}"; do
  $BROWSER fetch "$u" --format text >/dev/null 2>&1 &
done
wait
t1=$(python3 -c "import time;print(time.time())")
qjs_concurrent=$(python3 -c "print(f'{$t1-$t0:.1f}')")

# Chrome 并发
t2=$(python3 -c "import time;print(time.time())")
for u in "${CONCURRENT_URLS[@]}"; do
  "$CHROME" --headless=new --disable-gpu --no-sandbox --virtual-time-budget=8000 --dump-dom "$u" >/dev/null 2>&1 &
done
wait
t3=$(python3 -c "import time;print(time.time())")
chr_concurrent=$(python3 -c "print(f'{$t3-$t2:.1f}')")

# 串行对比
t4=$(python3 -c "import time;print(time.time())")
for u in "${CONCURRENT_URLS[@]}"; do
  $BROWSER fetch "$u" --format text >/dev/null 2>&1
done
t5=$(python3 -c "import time;print(time.time())")
qjs_serial=$(python3 -c "print(f'{$t5-$t4:.1f}')")

printf "  %-16s | %8s %8s %8s | %s\n" "" "QJS并发" "Chr并发" "QJS串行" "QJS并发提速"
printf "  %-16s | %7ss %7ss %7ss | %s\n" "3站" "$qjs_concurrent" "$chr_concurrent" "$qjs_serial" "$(python3 -c "print(f'{$qjs_serial/$qjs_concurrent:.1f}x')")"

############################################################
echo ""
echo "================================================================"
echo "  对标 Chrome 汇总"
echo "================================================================"
if [ $valid -gt 0 ]; then
  echo "  内存：QuickJS 平均 $((qjs_mem_sum/valid))MB | Chrome 平均 $((chr_mem_sum/valid))MB"
fi
echo "  速度：QuickJS ${qjs_speed_wins}/${count} 站比 Chrome 快"
if [ $coverage_valid -gt 0 ]; then
  echo "  覆盖率：平均 ${avg_cov}（对标 Chrome 文本内容）"
fi
echo "  错误：QuickJS ${qjs_clean}/${count} 站 0错误 | Chrome ${chr_clean}/${count} 站 0错误"
echo "  并发：QuickJS ${qjs_concurrent}s | Chrome ${chr_concurrent}s（3站并发）"
echo ""
echo "  测试脚本：tests/benchmarks/chrome_test_suite.sh"
echo "  评估报告：docs/assessments/M66-quickjs-csr-comparison.md"
echo "================================================================"
