#!/usr/bin/env bash
# M66 完整测试套件：内存 + 请求 + 并发 + 完整性 + 错误
# 用法：bash tests/benchmarks/full_test_suite.sh [站点数]
set -uo pipefail

BROWSER="./target/release/browser"
SITES_FILE="${1:-10}"  # 默认 10 站

# 10 个 CSR 测试站
SITES=(
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

# 取前 N 站
if [ "$SITES_FILE" != "10" ]; then
  SITES=("${SITES[@]:0:$SITES_FILE}")
fi

TMPDIR=$(mktemp -d)
trap "rm -rf $TMPDIR" EXIT

echo ""
echo "================================================================"
echo "  M66 QuickJS 完整测试套件"
echo "  维度：内存 | 请求 | 并发 | 完整性 | 错误"
echo "  站点数：${#SITES[@]}"
echo "  时间：$(date '+%Y-%m-%d %H:%M:%S')"
echo "================================================================"

############################################################
# 1. 内存测试
############################################################
echo ""
echo "━━━ 1. 内存测试（峰值 RSS MB）━━━━━━━━━━━━━━━━━━━━━━━━━━"
printf "  %-18s | %8s %8s %8s | %s\n" "站点" "QJS" "boa" "Chr" "QJS vs boa"
printf '  %.0s-' {1..60}; echo

qjs_total_mem=0
boa_total_mem=0
count=0

for url in "${SITES[@]}"; do
  short=$(echo "$url" | sed 's|https://||;s|/$||')
  q_mem=$(timeout 30 /usr/bin/time -lp $BROWSER fetch "$url" --format text >/dev/null 2>&1 | grep "maximum resident" | awk '{printf "%.0f", $1/1024/1024}')
  b_mem=$(timeout 30 /usr/bin/time -lp $BROWSER fetch "$url" --format text --js-engine boa >/dev/null 2>&1 | grep "maximum resident" | awk '{printf "%.0f", $1/1024/1024}')
  c_mem=$(timeout 15 /usr/bin/time -lp "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --headless=new --disable-gpu --no-sandbox --virtual-time-budget=8000 --dump-dom "$url" >/dev/null 2>&1 | grep "maximum resident" | awk '{printf "%.0f", $1/1024/1024}')
  
  ratio=$(python3 -c "
q=${q_mem:-0};b=${b_mem:-0}
if q>0 and b>0: print(f'1/{b//q}')
else: print('—')
" 2>/dev/null)
  
  printf "  %-18s | %6sMB %6sMB %6sMB | %s\n" "$short" "${q_mem:-?}" "${b_mem:-?}" "${c_mem:-?}" "$ratio"
  
  qjs_total_mem=$((qjs_total_mem + ${q_mem:-0}))
  boa_total_mem=$((boa_total_mem + ${b_mem:-0}))
  count=$((count + 1))
done

qjs_avg=$((qjs_total_mem / count))
boa_avg=$((boa_total_mem / count))
printf "  %-18s | %6sMB %6sMB | avg\n" "平均" "$qjs_avg" "$boa_avg"

############################################################
# 2. 请求测试（响应时间 + 带宽）
############################################################
echo ""
echo "━━━ 2. 请求测试（渲染时间秒 + fetch 大小 KB）━━━━━━━━━━━"
printf "  %-18s | %8s %8s | %8s %8s | %s\n" "站点" "QJS秒" "boa秒" "QJS_KB" "boa_KB" "判定"
printf '  %.0s-' {1..65}; echo

qjs_wins=0
for url in "${SITES[@]}"; do
  short=$(echo "$url" | sed 's|https://||;s|/$||')
  
  q_t=$(bash -c "time $BROWSER fetch '$url' --format text >/dev/null 2>&1" 2>&1 | grep real | sed 's/.*m\([0-9.]*\)s.*/\1/')
  q_kb=$($BROWSER fetch "$url" --format html 2>/dev/null | wc -c | awk '{printf "%.0f", $1/1024}')
  
  b_t=$(bash -c "time $BROWSER fetch '$url' --format text --js-engine boa >/dev/null 2>&1" 2>&1 | grep real | sed 's/.*m\([0-9.]*\)s.*/\1/')
  b_kb=$($BROWSER fetch "$url" --format html --js-engine boa 2>/dev/null | wc -c | awk '{printf "%.0f", $1/1024}')
  
  verdict=$(python3 -c "
q=$q_t;b=$b_t
if q<b: print(f'QJS快{b/q:.1f}x')
else: print(f'boa快{q/b:.1f}x')
" 2>/dev/null)
  
  python3 -c "exit(0 if $q_t < $b_t else 1)" 2>/dev/null && qjs_wins=$((qjs_wins + 1))
  
  printf "  %-18s | %6ss %6ss | %6sKB %6sKB | %s\n" "$short" "$q_t" "$b_t" "${q_kb:-?}" "${b_kb:-?}" "$verdict"
done

printf "  %-18s | QJS 领先 %s/%s 站\n" "" "$qjs_wins" "$count"

############################################################
# 3. 并发测试（同时 3 个请求）
############################################################
echo ""
echo "━━━ 3. 并发测试（3 个并发请求，测总时间 + 是否崩溃）━━━━━━"
CONCURRENT_URLS=("https://svelte.dev/" "https://remix.run/" "https://qwik.dev/")

printf "  并发请求: %s\n" "${CONCURRENT_URLS[*]}"
echo "  启动 3 个并发 fetch..."

t0=$(python3 -c "import time;print(time.time())")

# 3 个并发
$BROWSER fetch "${CONCURRENT_URLS[0]}" --format text >/dev/null 2>&1 &
PID1=$!
$BROWSER fetch "${CONCURRENT_URLS[1]}" --format text >/dev/null 2>&1 &
PID2=$!
$BROWSER fetch "${CONCURRENT_URLS[2]}" --format text >/dev/null 2>&1 &
PID3=$!

# 等待全部完成
wait $PID1; R1=$?
wait $PID2; R2=$?
wait $PID3; R3=$?

t1=$(python3 -c "import time;print(time.time)")
total=$(python3 -c "print(f'{$t1-$t0:.1f}')")

echo "  总时间: ${total}s"
echo "  结果: svelte=$R1 remix=$R2 qwik=$R3 (0=成功)"
if [ "$R1" -eq 0 ] && [ "$R2" -eq 0 ] && [ "$R3" -eq 0 ]; then
  echo "  ✅ 3 个并发全部成功"
else
  echo "  ❌ 有并发请求失败"
fi

# 串行对比
echo "  串行对比..."
t2=$(python3 -c "import time;print(time.time())")
$BROWSER fetch "${CONCURRENT_URLS[0]}" --format text >/dev/null 2>&1
$BROWSER fetch "${CONCURRENT_URLS[1]}" --format text >/dev/null 2>&1
$BROWSER fetch "${CONCURRENT_URLS[2]}" --format text >/dev/null 2>&1
t3=$(python3 -c "import time;print(time.time())")
serial=$(python3 -c "print(f'{$t3-$t2:.1f}')")
echo "  串行总时间: ${serial}s | 并发总时间: ${total}s | 提速: $(python3 -c "print(f'{$serial/$total:.1f}x')")"

############################################################
# 4. 完整性测试（内容覆盖率 + 对标 boa）
############################################################
echo ""
echo "━━━ 4. 完整性测试（内容字符数 + DOM 节点数 + 对标 boa）━━━"
printf "  %-18s | %8s %8s | %8s %8s | %s\n" "站点" "QJS字符" "boa字符" "QJS_DOM" "boa_DOM" "一致性"
printf '  %.0s-' {1..65}; echo

consistent=0
for url in "${SITES[@]}"; do
  short=$(echo "$url" | sed 's|https://||;s|/$||')
  
  q_text=$($BROWSER fetch "$url" --format text 2>/dev/null | wc -c | tr -d ' ')
  q_dom=$($BROWSER fetch "$url" --format html 2>/dev/null | grep -c "<" | tr -d ' ')
  
  b_text=$($BROWSER fetch "$url" --format text --js-engine boa 2>/dev/null | wc -c | tr -d ' ')
  b_dom=$($BROWSER fetch "$url" --format html --js-engine boa 2>/dev/null | grep -c "<" | tr -d ' ')
  
  # 一致性：字符数差异 < 10%
  match=$(python3 -c "
q=$q_text;b=$b_text
if q>0 and b>0:
    diff=abs(q-b)/max(q,b)*100
    print(f'✅' if diff<10 else f'⚠️{diff:.0f}%差异')
elif q>0: print('QJS独有')
else: print('❌空')
" 2>/dev/null)
  
  python3 -c "
q=$q_text;b=$b_text
if q>0 and b>0 and abs(q-b)/max(q,b)<0.1: exit(0)
exit(1)
" 2>/dev/null && consistent=$((consistent + 1))
  
  printf "  %-18s | %7s %7s | %7s %7s | %s\n" "$short" "$q_text" "$b_text" "$q_dom" "$b_dom" "$match"
done

printf "  %-18s | %s/%s 站一致\n" "" "$consistent" "$count"

############################################################
# 5. 错误测试（JS 错误数 + GC assertion + 崩溃）
############################################################
echo ""
echo "━━━ 5. 错误测试（JS 错误数 + GC assertion + 崩溃检测）━━━━"
printf "  %-18s | %6s | %6s | %6s | %s\n" "站点" "JS错误" "Assert" "崩溃" "状态"
printf '  %.0s-' {1..60}; echo

zero_error=0
for url in "${SITES[@]}"; do
  short=$(echo "$url" | sed 's|https://||;s|/$||')
  
  err=$(timeout 30 $BROWSER fetch "$url" --format text 2>&1 >/dev/null | grep -c "quickjs.*Error" || echo 0)
  asrt=$(timeout 30 $BROWSER fetch "$url" --format text 2>&1 >/dev/null | grep -c "Assertion" || echo 0)
  
  # 崩溃检测：exit code 非 0
  timeout 30 $BROWSER fetch "$url" --format text >/dev/null 2>&1
  crash=$?
  crash_str=$([ $crash -eq 0 ] && echo "否" || echo "是($crash)")
  
  if [ "${err:-0}" -eq 0 ] && [ "${asrt:-0}" -eq 0 ] && [ $crash -eq 0 ]; then
    status="✅ 完美"
    zero_error=$((zero_error + 1))
  elif [ "${asrt:-0}" -gt 0 ] || [ $crash -ne 0 ]; then
    status="❌ 严重"
  else
    status="⚠️ 非致命"
  fi
  
  printf "  %-18s | %5s | %5s | %5s | %s\n" "$short" "${err:-0}" "${asrt:-0}" "$crash_str" "$status"
done

############################################################
# 汇总
############################################################
echo ""
echo "================================================================"
echo "  测试汇总"
echo "================================================================"
echo "  内存：QuickJS 平均 ${qjs_avg}MB | boa 平均 ${boa_avg}MB"
echo "  请求：QuickJS 领先 ${qjs_wins}/${count} 站"
echo "  并发：3 并发 ${total}s（串行 ${serial}s，提速 $(python3 -c "print(f'{$serial/$total:.1f}')" 2>/dev/null)x）"
echo "  完整性：${consistent}/${count} 站内容与 boa 一致"
echo "  错误：${zero_error}/${count} 站 0 错误"
echo ""
echo "  报告文件：docs/assessments/M66-quickjs-csr-comparison.md"
echo "  基准脚本：tests/benchmarks/full_test_suite.sh"
echo "================================================================"
