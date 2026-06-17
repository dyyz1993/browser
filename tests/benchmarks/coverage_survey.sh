#!/usr/bin/env bash
# M59: browser fetch 覆盖面调研 —— 三方对比（curl SSR / 我们 / Chrome headless 真渲染）。
#
# 目的：诚实回答"我们能爬出多少页面？跟 Chrome 差多少？"
# 不挑容易的，挑真实市面页面，看真实差距。
#
# 用法：./tests/benchmarks/coverage_survey.sh
# 产物：${TMPDIR}/m59-cov/ 下 coverage.tsv + 各方原始输出

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BROWSER_BIN="$REPO_ROOT/target/release/browser"
CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
OUT_DIR="${TMPDIR:-/tmp}/m59-cov-$$"
mkdir -p "$OUT_DIR"

[[ -x "$BROWSER_BIN" ]] || { echo "❌ build browser first"; exit 1; }
[[ -x "$CHROME" ]] || { echo "⚠️ Chrome not found, skipping Chrome baseline"; CHROME=""; }

# 调研站点矩阵：tag|url|预期关键词（用于判断"抓到关键内容"）
# 覆盖：文档站/新闻/博客/技术社区/CSR数据站/电商首页/政府/学术
SITES=(
  "doc-mdn|https://developer.mozilla.org/en-US/docs/Web/JavaScript|JavaScript"
  "doc-rust|https://doc.rust-lang.org/book/ch01-01-installation.html|install"
  "news-hn|https://news.ycombinator.com/|points"
  "blog-github|https://github.blog/engineering/|engineering"
  "so-question|https://stackoverflow.com/questions/231761/the-definitive-guide-to-form-based-website-authentication|authentication"
  "csr-seo-box|https://seo.box/referring/|Domain"
  "github-readme|https://github.com/tokio-rs/tokio|Tokio"
  "wiki-static|https://en.wikipedia.org/wiki/Rust_(programming_language)|Rust"
  "md-static|https://daringfireball.net/projects/markdown/|Markdown"
  "gov-cn|https://www.gov.cn/|中国政府"
  "tech-juejin|https://juejin.cn/|稀土"
  "docs-firecrawl|https://docs.firecrawl.dev/introduction|Firecrawl"
)

# 提取可见文本字节数（去标签后的近似）。
text_bytes() {
  # 粗暴去 HTML 标签 + 压缩空白，算可见文本字节数。
  sed -e 's/<[^>]*>//g' "$1" | tr -s '[:space:]' ' ' | wc -c | tr -d ' '
}

# 判断关键词是否命中。
has_kw() { grep -qi "$2" "$1" && echo 1 || echo 0; }

COV_TSV="$OUT_DIR/coverage.tsv"
echo -e "tag\turl\tcurl_bytes\tcurl_kw\tours_bytes\tours_kw\tchrome_bytes\tchrome_kw\tverdict" > "$COV_TSV"

echo "=========================================="
echo "M59 覆盖面调研（curl / browser fetch / Chrome）"
echo "=========================================="

for entry in "${SITES[@]}"; do
  IFS='|' read -r tag url kw <<< "$entry"
  echo ""
  echo "--- $tag: $url (关键词: $kw) ---"

  # 1. curl SSR 基线（拿原始 HTML 的可见文本）。
  curl_html="$OUT_DIR/$tag.curl.html"
  curl -sL --compressed -A "Mozilla/5.0" --max-time 20 "$url" > "$curl_html" 2>/dev/null
  curl_bytes=$(text_bytes "$curl_html")
  curl_kw=$(has_kw "$curl_html" "$kw")

  # 2. browser fetch（我们）。
  ours_md="$OUT_DIR/$tag.ours.md"
  ours_timefile="$OUT_DIR/$tag.ours.time"
  /usr/bin/time -lp "$BROWSER_BIN" fetch "$url" --format markdown > "$ours_md" 2> "$ours_timefile"
  ours_rc=$?
  ours_bytes=0; ours_kw=0; ours_rss_mb=0
  if [[ $ours_rc -eq 0 ]]; then
    ours_bytes=$(wc -c < "$ours_md" | tr -d ' ')
    ours_kw=$(has_kw "$ours_md" "$kw")
    ours_rss_mb=$(( $(grep -i "maximum resident set size" "$ours_timefile" | awk '{print $1}') / 1048576 ))
  else
    ours_bytes="FAIL"
  fi

  # 3. Chrome headless 真渲染基线。
  chrome_html="$OUT_DIR/$tag.chrome.html"
  chrome_bytes=0; chrome_kw=0
  if [[ -n "$CHROME" ]]; then
    "$CHROME" --headless=new --disable-gpu --dump-dom --virtual-time-budget=8000 "$url" > "$chrome_html" 2>/dev/null
    chrome_rc=$?
    if [[ $chrome_rc -eq 0 && -s "$chrome_html" ]]; then
      chrome_bytes=$(text_bytes "$chrome_html")
      chrome_kw=$(has_kw "$chrome_html" "$kw")
    else
      chrome_bytes="FAIL"
    fi
  fi

  # 判定：以 Chrome 为"能抓到"的真相。
  # ours_kw=1 & chrome_kw=1 → 持平 Chrome
  # ours_kw=1 & chrome_kw=0 → 我们反而抓到了（少见）
  # ours_kw=0 & chrome_kw=1 → 输给 Chrome（JS 渲染差距）
  # ours_kw=0 & chrome_kw=0 → 都没抓到（可能反爬/需登录）
  if [[ "$ours_kw" == "1" && "$chrome_kw" == "1" ]]; then
    verdict="✅ 持平Chrome"
  elif [[ "$ours_kw" == "1" && "$chrome_kw" == "0" ]]; then
    verdict="🟰 我们抓到Chrome没抓到"
  elif [[ "$ours_kw" == "0" && "$chrome_kw" == "1" ]]; then
    verdict="❌ 输给Chrome"
  else
    verdict="⚪ 都没抓到"
  fi

  echo "  curl:    ${curl_bytes}B  kw=$curl_kw"
  echo "  ours:    ${ours_bytes}B  kw=$ours_kw  RSS=${ours_rss_mb}MB"
  echo "  chrome:  ${chrome_bytes}B  kw=$chrome_kw"
  echo "  → $verdict"
  echo -e "$tag\t$url\t$curl_bytes\t$curl_kw\t$ours_bytes\t$ours_kw\t$chrome_bytes\t$chrome_kw\t$verdict" >> "$COV_TSV"
done

echo ""
echo "=========================================="
echo "汇总 TSV:"
column -t -s $'\t' "$COV_TSV"
echo ""
echo "文件: $COV_TSV"
