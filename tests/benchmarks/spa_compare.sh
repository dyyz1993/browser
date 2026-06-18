#!/usr/bin/env bash
# M62: SPA 真实站点公平对比 —— browser fetch vs Chrome headless。
#
# 四维度：速度(wall) / 内存(peak RSS) / 内容质量(可见文本字节+关键词) / 判定。
# 公平性：
#   - 我们：browser fetch --format markdown（强制跑 JS）
#   - Chrome：headless --dump-dom 拿渲染后 HTML，再用通用过滤器去标签算可见文本
#   - 两边都提取「可见文本」对比，不是比原始 HTML
#
# 用法：./tests/benchmarks/spa_compare.sh [--rounds N] [--group main|stress|all]
# 产物：${TMPDIR}/spa-cmp/ 下 compare.tsv + 各方原始输出

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BROWSER_BIN="$REPO_ROOT/target/release/browser"
CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
OUT_DIR="${TMPDIR:-/tmp}/spa-cmp-$$"
mkdir -p "$OUT_DIR"

ROUNDS=1
GROUP=all
while [[ $# -gt 0 ]]; do
  case "$1" in
    --rounds) ROUNDS="$2"; shift 2;;
    --group) GROUP="$2"; shift 2;;
    *) echo "unknown: $1"; exit 1;;
  esac
done

[[ -x "$BROWSER_BIN" ]] || { echo "❌ build browser first"; exit 1; }
[[ -x "$CHROME" ]] || { echo "⚠️ Chrome not found"; CHROME=""; }

# 站点矩阵（从 M-spa-survey 改造，去掉已知 404 站）。
# 格式："tag|url|验证关键词（小写，内容命中才算真成功）"
MAIN_SITES=(
  "todomvc-react|https://todomvc.com/examples/react/dist/#/|todo"
  "todomvc-vue|https://todomvc.com/examples/vue/dist/#/|todo"
  "todomvc-preact|https://todomvc.com/examples/preact/dist/|todo"
  "caniuse|https://caniuse.com/|usage"
  "bundlephobia|https://bundlephobia.com/package/react|bundle"
  "bark|https://bark.day.app/|bark"
  "jsonplaceholder|https://jsonplaceholder.typicode.com/|jsonplaceholder"
  "npmtrends|https://npmtrends.com/react-vs-vue|downloads"
)
STRESS_SITES=(
  "juejin|https://juejin.cn/|稀土"
  "cls|https://www.cls.cn/telegraph|财联社"
  "svelte-repl|https://learn.svelte.dev/|svelte"
  "vue-playground|https://play.vuejs.org/|vue"
  "solid-playground|https://playground.solidjs.com/|solid"
  "firecrawl-docs|https://docs.firecrawl.dev/introduction|firecrawl"
  "owid-grapher|https://ourworldindata.org/grapher/life-expectancy|expectancy"
)

SITES=()
[[ "$GROUP" == "main" || "$GROUP" == "all" ]] && SITES+=("${MAIN_SITES[@]}")
[[ "$GROUP" == "stress" || "$GROUP" == "all" ]] && SITES+=("${STRESS_SITES[@]}")

# 可见文本字节数（去 HTML 标签 + 压缩空白）。
visible_text_bytes() {
  sed -e 's/<script[^>]*>.*<\/script>//g' \
      -e 's/<style[^>]*>.*<\/style>//g' \
      -e 's/<[^>]*>//g' "$1" | tr -s '[:space:]' ' ' | wc -c | tr -d ' '
}

# 提取 peak RSS（字节）。
peak_rss() {
  grep -i "maximum resident set size" "$1" 2>/dev/null | awk '{print $1}'
}

TSV="$OUT_DIR/compare.tsv"
echo -e "tag\turl\tkeyword\tours_ms\tours_rss_mb\tours_bytes\tours_kw\tchrome_ms\tchrome_rss_mb\tchrome_bytes\tchrome_kw\tverdict" > "$TSV"

echo "=========================================="
echo "M62 SPA 公平对比（browser fetch（强制 JS）vs Chrome headless）"
echo "rounds=$ROUNDS group=$GROUP sites=${#SITES[@]}"
echo "=========================================="

for entry in "${SITES[@]}"; do
  IFS='|' read -r tag url kw <<< "$entry"
  echo ""
  echo "--- $tag: $url (kw=$kw) ---"

  # === 我们：browser fetch --format markdown ===
  ours_md="$OUT_DIR/$tag.ours.md"
  ours_time="$OUT_DIR/$tag.ours.time"
  start=$(python3 -c 'import time;print(int(time.time()*1000))')
  /usr/bin/time -lp "$BROWSER_BIN" fetch "$url" --format markdown > "$ours_md" 2> "$ours_time"
  ours_rc=$?
  end=$(python3 -c 'import time;print(int(time.time()*1000))')
  ours_ms=$((end - start))
  ours_rss=$(peak_rss "$ours_time")
  ours_rss_mb=$((ours_rss / 1048576))
  ours_bytes=0; ours_kw=0
  if [[ $ours_rc -eq 0 ]]; then
    ours_bytes=$(wc -c < "$ours_md" | tr -d ' ')
    grep -qi "$kw" "$ours_md" && ours_kw=1
  else
    ours_ms="FAIL"; ours_rss_mb="FAIL"; ours_bytes="FAIL"
  fi

  # === Chrome：headless --dump-dom（拿渲染后 HTML）===
  chrome_html="$OUT_DIR/$tag.chrome.html"
  chrome_time="$OUT_DIR/$tag.chrome.time"
  chrome_ms="-"; chrome_rss_mb="-"; chrome_bytes=0; chrome_kw=0
  if [[ -n "$CHROME" ]]; then
    start=$(python3 -c 'import time;print(int(time.time()*1000))')
    /usr/bin/time -lp "$CHROME" --headless=new --disable-gpu --dump-dom \
      --virtual-time-budget=8000 "$url" > "$chrome_html" 2> "$chrome_time"
    chrome_rc=$?
    end=$(python3 -c 'import time;print(int(time.time()*1000))')
    chrome_ms=$((end - start))
    chrome_rss=$(peak_rss "$chrome_time")
    chrome_rss_mb=$((chrome_rss / 1048576))
    if [[ $chrome_rc -eq 0 && -s "$chrome_html" ]]; then
      chrome_bytes=$(visible_text_bytes "$chrome_html")
      grep -qi "$kw" "$chrome_html" && chrome_kw=1
    else
      chrome_ms="FAIL"; chrome_rss_mb="FAIL"; chrome_bytes="FAIL"
    fi
  fi

  # === 判定 ===
  if [[ "$ours_kw" == "1" && "$chrome_kw" == "1" ]]; then
    # 都成功，比内容量
    verdict="✅ 持平(都拿到关键词)"
  elif [[ "$ours_kw" == "1" && "$chrome_kw" == "0" ]]; then
    verdict="🏆 我们赢(Chrome没拿到)"
  elif [[ "$ours_kw" == "0" && "$chrome_kw" == "1" ]]; then
    verdict="❌ Chrome赢(我们没拿到)"
  else
    verdict="⚪ 都没拿到"
  fi

  echo "  我们: ${ours_ms}ms ${ours_rss_mb}MB ${ours_bytes}B kw=$ours_kw"
  echo "  Chrome: ${chrome_ms}ms ${chrome_rss_mb}MB ${chrome_bytes}B kw=$chrome_kw"
  echo "  → $verdict"

  echo -e "$tag\t$url\t$kw\t$ours_ms\t$ours_rss_mb\t$ours_bytes\t$ours_kw\t$chrome_ms\t$chrome_rss_mb\t$chrome_bytes\t$chrome_kw\t$verdict" >> "$TSV"
done

echo ""
echo "=========================================="
echo "汇总:"
column -t -s $'\t' "$TSV"
echo ""
echo "文件: $TSV"
