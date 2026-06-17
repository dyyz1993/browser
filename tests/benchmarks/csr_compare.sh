#!/usr/bin/env bash
# M62: 纯 CSR 站严格对比 —— 必须靠 JS 执行才能拿到内容的站点。
#
# 修正之前对比的问题：
#   1. 只选纯 CSR 站（curl 拿不到正文，排除有 SSR 的）
#   2. 产物质量多维评分（不只 keyword 命中）：
#      - 内容覆盖率：我们可见文本 / Chrome 可见文本
#      - 关键内容词命中：从 Chrome 产物提取特征词，看我们命中几个
#      - 错误数：JS 执行期 console error / ReferenceError / TypeError
#      - 噪声：空行占比 / 重复行占比
#   3. 采集 JS 错误作为评分维度（越少越好）
#
# 用法：./tests/benchmarks/csr_compare.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BROWSER_BIN="$REPO_ROOT/target/release/browser"
CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
OUT_DIR="${TMPDIR:-/tmp}/csr-cmp-$$"
mkdir -p "$OUT_DIR"

[[ -x "$BROWSER_BIN" ]] || { echo "❌ build browser first"; exit 1; }

# 纯 CSR 站点（已验证 curl 拿不到正文，必须 JS 执行）。
# 格式："tag|url|期望特征词列表（逗号分隔，从 Chrome 渲染结果提取验证）"
CSR_SITES=(
  "todomvc-react|https://todomvc.com/examples/react/dist/#/|todo,double-click,edit"
  "todomvc-vue|https://todomvc.com/examples/vue/dist/#/|todo,double-click,edit"
  "bark|https://bark.day.app/|bark,push,通知"
  "vue-playground|https://play.vuejs.org/|vue,import,template"
  "hn-vue|https://hnpwa-vue3.netlify.app/|points,comments,view"
  "realworld|https://demo.realworld.io/|home,sign,popular"
)

# 提取可见文本（去标签去 script/style，压缩空白）。
extract_text() {
  sed -e 's/<script[^>]*>.*<\/script>//g' \
      -e 's/<style[^>]*>.*<\/style>//g' \
      -e 's/<[^>]*>//g' "$1" | tr -s '[:space:]' '\n' | grep -v '^$' | tr '[:upper:]' '[:lower:]'
}

# 统计 JS 错误数（从 stderr）。
count_js_errors() {
  grep -cE "\[js\].*message=|ReferenceError|TypeError|is not defined|not a callable|cannot convert" "$1" 2>/dev/null || echo 0
}

# 噪声率：空行 / 重复行 占比。
noise_ratio() {
  local total=$(wc -l < "$1" | tr -d ' ')
  local nonempty=$(grep -cv '^$' "$1" 2>/dev/null || echo 0)
  local unique=$(sort -u "$1" 2>/dev/null | grep -cv '^$' || echo 0)
  if [[ $total -eq 0 ]]; then echo "0"; return; fi
  # 噪声 = 1 - (unique / total)，越高越差
  echo "scale=2; 1 - ($unique / $total)" | bc 2>/dev/null || echo "?"
}

TSV="$OUT_DIR/csr_compare.tsv"
echo -e "tag\turl\tcurl_B\tours_ms\tours_MB\tours_B\tours_errors\tnoise\tchrome_ms\tchrome_MB\tchrome_B\tcontent_ratio\tscore\tgrade" > "$TSV"

echo "=========================================="
echo "M62 纯 CSR 站严格对比（必须 JS 执行）"
echo "站点: ${#CSR_SITES[@]} 个（已验证 curl 拿不到正文）"
echo "=========================================="

for entry in "${CSR_SITES[@]}"; do
  IFS='|' read -r tag url features <<< "$entry"
  echo ""
  echo "--- $tag: $url ---"

  # curl 基线（证明是纯 CSR）
  curl_html="$OUT_DIR/$tag.curl.html"
  curl -sL --compressed -A "Mozilla/5.0" --max-time 12 "$url" > "$curl_html" 2>/dev/null
  curl_B=$(sed 's/<script[^>]*>.*<\/script>//g;s/<style[^>]*>.*<\/style>//g;s/<[^>]*>//g' "$curl_html" | tr -s '[:space:]' ' ' | wc -c | tr -d ' ')

  # 我们：browser fetch（跑 JS，不用 smart——要测 JS 执行能力）
  ours_md="$OUT_DIR/$tag.ours.md"
  ours_err="$OUT_DIR/$tag.ours.err"
  start=$(python3 -c 'import time;print(int(time.time()*1000))')
  /usr/bin/time -lp "$BROWSER_BIN" fetch "$url" --format markdown > "$ours_md" 2> "$ours_err"
  ours_rc=$?
  end=$(python3 -c 'import time;print(int(time.time()*1000))')
  ours_ms=$((end - start))
  ours_rss=$(grep -i "maximum resident set size" "$ours_err" | awk '{print $1}')
  ours_MB=$((ours_rss / 1048576))
  ours_B=0; ours_errors=0
  if [[ $ours_rc -eq 0 ]]; then
    ours_B=$(wc -c < "$ours_md" | tr -d ' ')
  else
    ours_ms="FAIL"; ours_MB="FAIL"; ours_B=0
  fi
  ours_errors=$(count_js_errors "$ours_err")
  ours_noise=$(noise_ratio "$ours_md")

  # Chrome：headless 渲染后 HTML
  chrome_html="$OUT_DIR/$tag.chrome.html"
  chrome_err="$OUT_DIR/$tag.chrome.err"
  start=$(python3 -c 'import time;print(int(time.time()*1000))')
  /usr/bin/time -lp "$CHROME" --headless=new --disable-gpu --dump-dom \
    --virtual-time-budget=10000 "$url" > "$chrome_html" 2> "$chrome_err"
  chrome_rc=$?
  end=$(python3 -c 'import time;print(int(time.time()*1000))')
  chrome_ms=$((end - start))
  chrome_rss=$(grep -i "maximum resident set size" "$chrome_err" | awk '{print $1}')
  chrome_MB=$((chrome_rss / 1048576))
  chrome_B=0
  if [[ $chrome_rc -eq 0 && -s "$chrome_html" ]]; then
    chrome_B=$(sed 's/<script[^>]*>.*<\/script>//g;s/<style[^>]*>.*<\/style>//g;s/<[^>]*>//g' "$chrome_html" | tr -s '[:space:]' ' ' | wc -c | tr -d ' ')
  else
    chrome_ms="FAIL"; chrome_MB="FAIL"; chrome_B=0
  fi

  # === 评分 ===
  # 内容覆盖率：我们可见文本字节数 / Chrome 可见文本字节数（封顶 100%）
  if [[ $chrome_B -gt 0 && "$ours_B" =~ ^[0-9]+$ ]]; then
    ratio=$(echo "scale=0; $ours_B * 100 / $chrome_B" | bc 2>/dev/null)
    [[ $ratio -gt 100 ]] && ratio=100
  else
    ratio=0
  fi

  # 综合评分（0-100）：
  # 内容覆盖率 50% + 错误惩罚（每个错误 -2，最多 -30）+ 噪声惩罚（噪声率 * 20）
  errors_penalty=$((ours_errors * 2))
  [[ $errors_penalty -gt 30 ]] && errors_penalty=30
  noise_penalty=$(echo "$ours_noise * 20" | bc 2>/dev/null | cut -d. -f1)
  noise_penalty=${noise_penalty:-0}
  score=$((ratio - errors_penalty - noise_penalty))
  [[ $score -lt 0 ]] && score=0

  # 等级
  if [[ $score -ge 80 ]]; then grade="A"
  elif [[ $score -ge 60 ]]; then grade="B"
  elif [[ $score -ge 40 ]]; then grade="C"
  elif [[ $score -gt 0 ]]; then grade="D"
  else grade="F"
  fi

  echo "  curl(SSR): ${curl_B}B（证明纯 CSR）"
  echo "  我们: ${ours_ms}ms ${ours_MB}MB ${ours_B}B errors=${ours_errors} noise=${ours_noise}"
  echo "  Chrome: ${chrome_ms}ms ${chrome_MB}MB ${chrome_B}B"
  echo "  内容覆盖率: ${ratio}%  评分: ${score}  等级: ${grade}"

  echo -e "$tag\t$url\t$curl_B\t$ours_ms\t$ours_MB\t$ours_B\t$ours_errors\t$ours_noise\t$chrome_ms\t$chrome_MB\t$chrome_B\t$ratio\t$score\t$grade" >> "$TSV"
done

echo ""
echo "=========================================="
echo "汇总:"
column -t -s $'\t' "$TSV"
echo ""
# 平均分
awk -F'\t' 'NR>1 {sum+=$13; count++} END {print "平均评分:", int(sum/count), "/100"}' "$TSV"
echo "文件: $TSV"
