#!/usr/bin/env bash
# HTML 对标 Chrome Benchmark

PROJECT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPORT="$PROJECT_DIR/benchmark_report.md"

echo "# HTML 对标 Chrome 报告" > "$REPORT"
echo "## $(date '+%Y-%m-%d %H:%M')" >> "$REPORT"
echo "" >> "$REPORT"
echo "| 站点 | 类型 | Chrome(B) | ours(B) | grade | diff行 | JS报错 |" >> "$REPORT"
echo "|------|------|---------:|-------:|:----:|:-----:|:-----:|" >> "$REPORT"

bench() {
  local url="$1" name="$2" stype="$3"
  local our_html chrome_html our_size chrome_size js_err=0
  
  # Ours
  our_html=$(timeout 30 "$PROJECT_DIR/target/release/browser" fetch "$url" --format html 2>/dev/null || true)
  our_size=$(echo "${our_html:-}" | wc -c | tr -d ' ')
  
  # JS errors
  local se="/tmp/se_$$.txt"
  timeout 30 "$PROJECT_DIR/target/release/browser" fetch "$url" --format text >/dev/null 2>"$se" || true
  js_err=$(grep -c '\[js\]' "$se" 2>/dev/null; true)
  rm -f "$se"
  
  # Chrome
  chrome_html=$(timeout 25 /Applications/Google\ Chrome.app/Contents/MacOS/Google\ Chrome \
    --headless=new --no-sandbox --disable-gpu --virtual-time-budget=10000 \
    --dump-dom "$url" 2>/dev/null || true)
  chrome_size=$(echo "${chrome_html:-}" | wc -c | tr -d ' ')
  
	  local score dl grade
	  score=""
	  dl="-"
	  grade="-"
	  if [ "$chrome_size" -gt 100 ] && [ "$our_size" -gt 100 ]; then
    echo "$our_html" > /tmp/_ox.html
    echo "$chrome_html" > /tmp/_cx.html
    score=$(python3 "$PROJECT_DIR/tests/benchmarks/completeness.py" \
      --ours /tmp/_ox.html --theirs /tmp/_cx.html --grade 2>/dev/null | tr -d '\n')
    dl=$(diff /tmp/_ox.html /tmp/_cx.html 2>/dev/null | wc -l | tr -d ' ')
    grade=$(echo "$score" | awk '{print $NF}')
    echo "  ✅ $name: ours=${our_size}B chrome=${chrome_size}B grade=$grade diff=$dl js=$js_err"
  elif [ "$our_size" -gt 100 ]; then
    echo "  ⚠️ $name: ours=${our_size}B chrome=0B js=$js_err"
  else
    echo "  ❌ $name: 双端不可达"
  fi
  echo "| $name | $stype | $chrome_size | $our_size | $grade | $dl | $js_err |" >> "$REPORT"
}

echo "--- 10 站 Benchmark ---"
bench "https://example.com/" "example.com" "静态"
bench "https://vuejs.org/" "vuejs.org" "框架文档"
bench "https://react.dev/" "react.dev" "框架文档"
bench "https://svelte.dev/" "svelte.dev" "框架文档"
bench "https://baidu.com/" "baidu.com" "门户"
bench "https://github.com/" "github.com" "代码托管"
bench "https://www.bing.com/" "bing.com" "搜索"
bench "https://www.zhihu.com/" "zhihu.com" "中文问答"
bench "https://www.wikipedia.org/" "wikipedia" "百科"
bench "https://news.ycombinator.com/" "hackernews" "技术"

echo ""
echo "✅ 报告: $REPORT"
head -18 "$REPORT"
echo "..."
tail -5 "$REPORT"
