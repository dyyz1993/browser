#!/usr/bin/env bash
# SPA 覆盖面批量测试脚本 —— 20 个真实 SPA（CSR）站点三方对比。
#
# 目的：诚实回答"browser fetch 能爬多少 SPA？跟 Chrome 差多少？"
# 与 coverage_survey.sh 的核心差异：
#   - 判定逻辑加 curl 维度：curl 抓不到（CSR 铁证）+ 我们抓到 = 真 SPA 成功
#   - 避免把 SSR 站误判为"持平"（curl 能抓到的从统计剔除）
#   - 分主测试组（轻量 SPA）+ 压力组（大 bundle/REPL，预期失败也是有效结论）
#   - ours_kw=0 时自动 --no-js 兜底重试
#
# 用法：./tests/benchmarks/spa_survey.sh
# 产物：${TMPDIR}/spa-survey-$$/ 下 spa_survey.tsv + 各方原始输出
#       docs/assessments/M-spa-survey.md（人类可读报告）

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BROWSER_BIN="$REPO_ROOT/target/release/browser"
CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
OUT_DIR="${TMPDIR:-/tmp}/spa-survey-$$"
REPORT_MD="$REPO_ROOT/docs/assessments/M-spa-survey.md"
mkdir -p "$OUT_DIR"

[[ -x "$BROWSER_BIN" ]] || { echo "❌ build browser first: cargo build --release -p browser-cli"; exit 1; }
[[ -x "$CHROME" ]] || { echo "⚠️ Chrome not found, skipping Chrome baseline"; CHROME=""; }

# ===========================================================================
# 站点矩阵：tag|url|预期关键词
# 关键词选取：必须是 JS 渲染后才出现的内容（curl 拿不到的），用于判定 JS 是否跑通
# ===========================================================================

# 主测试组（12 个）：轻量 SPA，预期 boa 能跑通
MAIN_SITES=(
  "todomvc-react|https://todomvc.com/examples/react/dist/#/|todo"
  "todomvc-vue|https://todomvc.com/examples/vue/dist/#/|todo"
  "todomvc-preact|https://todomvc.com/examples/preact/dist/|todo"
  "todomvc-angular|https://todomvc.com/examples/angularjs/#/|todo"
  "realworld-ng|https://demo.realworld.io/|Popular"
  "realworld-react|https://react-redux.realworld.io/|Popular"
  "hn-vue|https://hnpwa-vue3.netlify.app/|points"
  "caniuse|https://caniuse.com/|Usage"
  "bundlephobia|https://bundlephobia.com/package/react|Bundle"
  "bark|https://bark.day.app/|Bark"
  "jsonplaceholder|https://jsonplaceholder.typicode.com/|JSONPlaceholder"
  "npmtrends|https://npmtrends.com/react-vs-vue|downloads"
)

# 压力组（8 个）：大 bundle/REPL/复杂 SPA，预期 OOM 或 boa 跑不动
# 失败也是有效结论（验证内存护栏/超时/boa 兼容性边界）
STRESS_SITES=(
  "juejin|https://juejin.cn/|稀土"
  "cls|https://www.cls.cn/telegraph|财联社"
  "svelte-repl|https://learn.svelte.dev/|Svelte"
  "vue-playground|https://play.vuejs.org/|Vue"
  "solid-playground|https://playground.solidjs.com/|Solid"
  "firecrawl-docs|https://docs.firecrawl.dev/introduction|Firecrawl"
  "coingecko|https://www.coingecko.com/en/coins/bitcoin|Bitcoin"
  "owid-grapher|https://ourworldindata.org/grapher/life-expectancy|Expectancy"
)

# ===========================================================================
# 辅助函数（复用自 coverage_survey.sh）
# ===========================================================================

# 提取可见文本字节数（去标签 + 压缩空白）。
text_bytes() {
  sed -e 's/<[^>]*>//g' "$1" | tr -s '[:space:]' ' ' | wc -c | tr -d ' '
}

# 判断关键词是否命中（大小写无关）。
has_kw() { grep -qi "$2" "$1" && echo 1 || echo 0; }

# ===========================================================================
# 三路采集 + SPA 判定
# ===========================================================================

# 采集单个站点，写一行 TSV。
# 参数：$1=group(main/stress) $2=tag $3=url $4=keyword
run_site() {
  local group="$1" tag="$2" url="$3" kw="$4"
  echo ""
  echo "--- [$group] $tag: $url (关键词: $kw) ---"

  # 1. curl SSR 基线 —— 拿原始 HTML 可见文本。
  #    curl_kw=0 是"CSR 铁证"：说明静态 HTML 没有目标内容，必须靠 JS 渲染。
  local curl_html curl_bytes curl_kw
  curl_html="$OUT_DIR/$tag.curl.html"
  curl -sL --compressed -A "Mozilla/5.0" --max-time 20 "$url" > "$curl_html" 2>/dev/null
  curl_bytes=$(text_bytes "$curl_html")
  curl_kw=$(has_kw "$curl_html" "$kw")

  # 2. browser fetch（我们）—— JS 渲染后输出 markdown。
  local ours_md ours_timefile ours_rc ours_bytes ours_kw ours_rss_mb ours_stderr scripts_n hint
  ours_md="$OUT_DIR/$tag.ours.md"
  ours_timefile="$OUT_DIR/$tag.ours.time"
  ours_stderr="$OUT_DIR/$tag.ours.stderr"
  /usr/bin/time -lp "$BROWSER_BIN" fetch "$url" --format markdown > "$ours_md" 2> "$ours_stderr"
  ours_rc=$?
  ours_rss_mb=0
  if [[ $ours_rc -eq 0 ]]; then
    ours_bytes=$(wc -c < "$ours_md" | tr -d ' ')
    ours_kw=$(has_kw "$ours_md" "$kw")
    ours_rss_mb=$(( $(grep -i "maximum resident set size" "$ours_stderr" | awk '{print $1}') / 1048576 ))
  else
    ours_bytes="FAIL($ours_rc)"; ours_kw=0
  fi
  # 捕获 stderr 诊断信号（script count / hint）
  scripts_n=$(grep -oE '\[[a-z]+\] [0-9]+ script\(s\) executed' "$ours_stderr" | grep -oE '[0-9]+' || echo "?")
  hint=$(grep -oE '\[hint\][^$]*' "$ours_stderr" | head -1 | sed 's/^ *//' || echo "")

  # 2b. --no-js 兜底重试：ours_kw=0 时试 SSR 静态壳（M59 gov.cn 场景）
  local nojs_kw="N/A"
  if [[ "$ours_kw" == "0" ]]; then
    local nojs_md="$OUT_DIR/$tag.nojs.md"
    "$BROWSER_BIN" fetch "$url" --format markdown --no-js > "$nojs_md" 2>/dev/null
    nojs_kw=$(has_kw "$nojs_md" "$kw")
  fi

  # 3. Chrome headless 真渲染基线。
  local chrome_html chrome_bytes chrome_kw
  chrome_html="$OUT_DIR/$tag.chrome.html"
  chrome_bytes=0; chrome_kw=0
  if [[ -n "$CHROME" ]]; then
    "$CHROME" --headless=new --disable-gpu --dump-dom --virtual-time-budget=8000 "$url" > "$chrome_html" 2>/dev/null
    local chrome_rc=$?
    if [[ $chrome_rc -eq 0 && -s "$chrome_html" ]]; then
      chrome_bytes=$(text_bytes "$chrome_html")
      chrome_kw=$(has_kw "$chrome_html" "$kw")
    else
      chrome_bytes="FAIL"
    fi
  fi

  # 4. SPA 专用判定（核心：加 curl_kw 维度，避免 SSR 站误判）
  #    curl=0 才是真 CSR；curl=1 说明有 SSR 兜底，不算纯 SPA 测试。
  local verdict
  if [[ "$curl_kw" == "1" && "$ours_kw" == "1" ]]; then
    verdict="⚪ 误入SSR(curl有内容)"
  elif [[ "$curl_kw" == "1" && "$ours_kw" == "0" ]]; then
    verdict="❌ SSR+JS报错"
  elif [[ "$curl_kw" == "0" && "$ours_kw" == "1" && "$chrome_kw" == "1" ]]; then
    verdict="✅ SPA成功"
  elif [[ "$curl_kw" == "0" && "$ours_kw" == "1" && "$chrome_kw" == "0" ]]; then
    verdict="🟢 SPA成功(超Chrome?)"
  elif [[ "$curl_kw" == "0" && "$ours_kw" == "0" && "$chrome_kw" == "1" ]]; then
    verdict="❌ JS失败(boa跑不动)"
  else
    verdict="⚠️ 三方都没抓到"
  fi

  # 内存标红（M-cls 护栏阈值 400MB）
  local rss_marker=""
  [[ "$ours_rss_mb" -gt 400 ]] && rss_marker=" 🔴OOM风险"

  echo "  curl:    ${curl_bytes}B  kw=$curl_kw"
  echo "  ours:    ${ours_bytes}B  kw=$ours_kw  RSS=${ours_rss_mb}MB$rss_marker  scripts=$scripts_n"
  [[ -n "$hint" ]] && echo "  hint:    $hint"
  [[ "$nojs_kw" != "N/A" ]] && echo "  no-js:   kw=$nojs_kw (SSR 兜底)"
  echo "  chrome:  ${chrome_bytes}B  kw=$chrome_kw"
  echo "  → $verdict"

  echo -e "$group\t$tag\t$url\t$kw\t$curl_bytes\t$curl_kw\t$ours_bytes\t$ours_kw\t$ours_rss_mb\t$chrome_bytes\t$chrome_kw\t$nojs_kw\t$verdict" >> "$TSV"
}

# ===========================================================================
# 主流程
# ===========================================================================

TSV="$OUT_DIR/spa_survey.tsv"
echo -e "group\ttag\turl\tkw\tcurl_B\tcurl_kw\teurs_B\teurs_kw\teurs_RSS_MB\tchrome_B\tchrome_kw\tnojs_kw\tverdict" > "$TSV"

echo "=========================================="
echo "SPA 覆盖面批量测试（curl / browser fetch / Chrome）"
echo "=========================================="
echo "主测试组 12 个 + 压力组 8 个 = 共 20 个 SPA 站点"
echo "产物目录: $OUT_DIR"
echo ""

echo "########## 主测试组（预期 boa 能跑通） ##########"
for entry in "${MAIN_SITES[@]}"; do
  IFS='|' read -r tag url kw <<< "$entry"
  run_site "main" "$tag" "$url" "$kw"
done

echo ""
echo "########## 压力组（预期 OOM / boa 跑不动，失败也是结论） ##########"
for entry in "${STRESS_SITES[@]}"; do
  IFS='|' read -r tag url kw <<< "$entry"
  run_site "stress" "$tag" "$url" "$kw"
done

# ===========================================================================
# 汇总
# ===========================================================================
echo ""
echo "=========================================="
echo "汇总 TSV:"
column -t -s $'\t' "$TSV"
echo ""
echo "TSV 文件: $TSV"

# 统计：只数 curl_kw=0 的行（真 CSR 测试），避免 SSR 站污染
echo ""
echo "=========================================="
echo "统计（仅 curl_kw=0 的真 CSR 站点）"
echo "=========================================="
local_total=$(awk -F'\t' 'NR>1 && $1=="main"   && $6==0' "$TSV" | wc -l | tr -d ' ')
local_ok=$(awk    -F'\t' 'NR>1 && $1=="main"   && $6==0 && $8==1' "$TSV" | wc -l | tr -d ' ')
stress_total=$(awk -F'\t' 'NR>1 && $1=="stress" && $6==0' "$TSV" | wc -l | tr -d ' ')
stress_ok=$(awk    -F'\t' 'NR>1 && $1=="stress" && $6==0 && $8==1' "$TSV" | wc -l | tr -d ' ')
ssr_misclassified=$(awk -F'\t' 'NR>1 && $6==1' "$TSV" | wc -l | tr -d ' ')
echo "主测试组:   $local_ok / $local_total 站点 SPA 渲染成功"
echo "压力组:     $stress_ok / $stress_total 站点 SPA 渲染成功"
echo "误入 SSR:   $ssr_misclassified 个（curl 抓到，剔除出 SPA 统计）"
echo ""
echo "报告将写入: ${REPORT_MD}（请人工确认 TSV 后运行报告生成步骤）"
