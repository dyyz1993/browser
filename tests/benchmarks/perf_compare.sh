#!/usr/bin/env bash
# M64: 性能基准 —— 我们的 browser vs Chrome headless
# 测：渲染时间(秒) + 峰值 RSS(MB)
# macOS: 用 /usr/bin/time -l 拿 RSS（"maximum resident set size"）
set -uo pipefail

CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
BROWSER="./target/release/browser"

SITES=(
  "https://bark.day.app/"
  "https://vuejs.org/"
  "https://vite.dev/"
  "https://nuxt.com/"
  "https://react.dev/"
)

# macOS /usr/bin/time 输出的 RSS 字段是 bytes
bytes_to_mb() {
  python3 -c "print(f'{$1/1024/1024:.0f}')"
}

printf "%-28s | %-22s | %-22s | %-14s | %-14s\n" \
  "站点" "我们(时间/RSS)" "Chrome(时间/RSS)" "内存比" "速度比"
printf '%.0s-' {1..110}; echo

for url in "${SITES[@]}"; do
  # ===== 我们的 browser =====
  # 渲染时间（3 次取最快）
  our_best_time=999
  our_rss=0
  for i in 1 2 3; do
    start=$(python3 -c "import time; print(time.time())")
    /usr/bin/time -l $BROWSER fetch "$url" --format text >/dev/null 2>/tmp/our_time.txt
    end=$(python3 -c "import time; print(time.time())")
    elapsed=$(python3 -c "print(f'{$end-$start:.2f}')")
    rss=$(grep "maximum resident set size" /tmp/our_time.txt | awk '{print $1}')
    if (( $(echo "$elapsed < $our_best_time" | bc -l 2>/dev/null || echo 0) )); then
      our_best_time=$elapsed
      our_rss=$rss
    fi
  done
  our_time=$our_best_time
  our_rss_mb=$(bytes_to_mb "${our_rss:-0}")

  # ===== Chrome headless =====
  chr_best_time=999
  chr_rss=0
  for i in 1 2; do
    start=$(python3 -c "import time; print(time.time())")
    /usr/bin/time -l "$CHROME" --headless=new --disable-gpu --no-sandbox \
      --virtual-time-budget=8000 --dump-dom "$url" >/dev/null 2>/tmp/chr_time.txt
    end=$(python3 -c "import time; print(time.time())")
    elapsed=$(python3 -c "print(f'{$end-$start:.2f}')")
    rss=$(grep "maximum resident set size" /tmp/chr_time.txt | awk '{print $1}')
    if (( $(echo "$elapsed < $chr_best_time" | bc -l 2>/dev/null || echo 0) )); then
      chr_best_time=$elapsed
      chr_rss=$rss
    fi
  done
  chr_time=$chr_best_time
  chr_rss_mb=$(bytes_to_mb "${chr_rss:-0}")

  # 比率
  if [ "$our_rss_mb" -gt 0 ] && [ "$chr_rss_mb" -gt 0 ] 2>/dev/null; then
    mem_ratio=$(python3 -c "print(f'1/{chr_rss_mb/$our_rss_mb:.1f}')")
  else
    mem_ratio="N/A"
  fi
  if [ "$our_time" != "999" ] && [ "$chr_time" != "999" ] 2>/dev/null; then
    speed_ratio=$(python3 -c "print(f'{chr_time/$our_time:.1f}x')")
  else
    speed_ratio="N/A"
  fi

  short=$(echo "$url" | sed 's|https://||; s|/$||')
  printf "%-28s | %5ss / %5sMB       | %5ss / %5sMB       | %-14s | %-14s\n" \
    "$short" "$our_time" "$our_rss_mb" "$chr_time" "$chr_rss_mb" "$mem_ratio" "$speed_ratio"
done

echo ""
echo "内存比 = Chrome内存/我们内存（1/N 表示我们只用 1/N）"
echo "速度比 = Chrome时间/我们时间（Nx 表示我们快 N 倍）"
echo "时间取 3 次最快，内存取峰值 RSS"
