#!/usr/bin/env bash
set -euo pipefail
cd "$(git rev-parse --show-toplevel 2>/dev/null || echo '/Users/xuyingzhou/Project/study-rust/browser')"

BROWSER="./target/release/browser"
CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
CHROME_FLAGS="--headless=new --no-sandbox --disable-gpu --virtual-time-budget=10000"
REPORT="benchmark_report.md"

echo "📊 HTML 对标 Chrome Benchmark"
echo "================================"
echo ""

# Ensure binary exists
if [ ! -f "$BROWSER" ]; then
    cargo build --release -p browser-cli 2>&1 | tail -1
fi

echo "# HTML 对标 Chrome 报告" > "$REPORT"
echo "## $(date '+%Y-%m-%d %H:%M')" >> "$REPORT"
echo "" >> "$REPORT"
echo "| 站点 | 类型 | Chrome(B) | ours(B) | block_cov | sim_ratio | struct | word_cov | diff行 | 评级 | JS报错 |" >> "$REPORT"
echo "|------|------|----------|--------|-----------|-----------|--------|----------|-------|-------|--------|" >> "$REPORT"

bench_site() {
    local url="$1" name="$2" stype="$3"
    
    # 我们的渲染后 HTML
    local our_html our_size
    our_html=$(timeout 40 "$BROWSER" fetch "$url" --format html 2>/dev/null) || our_html=""
    our_size=$(echo "$our_html" | wc -c | tr -d ' ')
    
    # JS 报错数
    local js_errors
    js_errors=$(timeout 40 "$BROWSER" fetch "$url" --format text 2>&1 >/dev/null | grep -c "\[js\]" || true)
    
    # Chrome dump-dom
    local chrome_size chrome_html
    chrome_html=$(timeout 30 "$CHROME" $CHROME_FLAGS --dump-dom "$url" 2>/dev/null) || chrome_html=""
    chrome_size=$(echo "$chrome_html" | wc -c | tr -d ' ')
    
    local score="" dl="" grade="—"
    
    if [ "$chrome_size" -gt 100 ] && [ "$our_size" -gt 100 ]; then
        echo "$our_html" > /tmp/ours_bench.html
        echo "$chrome_html" > /tmp/chrome_bench.html
        
        score=$(python3 tests/benchmarks/completeness.py --ours /tmp/ours_bench.html --theirs /tmp/chrome_bench.html --grade 2>/dev/null || echo "0.000 0.000 0.000 0.000 0.000 F")
        dl=$(diff /tmp/ours_bench.html /tmp/chrome_bench.html 2>/dev/null | wc -l | tr -d ' ')
        grade=$(echo "$score" | awk '{print $NF}')
    fi
    
    local block=$(echo "$score" | awk '{printf "%.3f", $1}')
    local sim=$(echo "$score" | awk '{printf "%.3f", $2}')
    local struct=$(echo "$score" | awk '{printf "%.3f", $3}')
    local word=$(echo "$score" | awk '{printf "%.3f", $4}')
    local comp=$(echo "$score" | awk '{printf "%.3f", $5}')
    
    printf "| %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %d |\n" \
        "$name" "$stype" "$chrome_size" "$our_size" \
        "$block" "$sim" "$struct" "$word" "$dl" "$grade" "$js_errors" >> "$REPORT"
    
    echo "  $name ($stype): ours=${our_size}B chrome=${chrome_size}B grade=$grade js_errors=$js_errors"
}

bench_site "https://example.com/" "example.com" "静态"
bench_site "https://vuejs.org/" "vuejs.org" "框架文档"
bench_site "https://react.dev/" "react.dev" "框架文档"
bench_site "https://svelte.dev/" "svelte.dev" "框架文档"
bench_site "https://baidu.com/" "baidu.com" "搜索门户"
bench_site "https://github.com/" "github.com" "代码托管"
bench_site "https://www.bing.com/" "bing.com" "搜索引擎"
bench_site "https://www.zhihu.com/" "zhihu.com" "中文问答"
bench_site "https://www.wikipedia.org/" "wikipedia.org" "百科"
bench_site "https://news.ycombinator.com/" "hackernews" "技术新闻"

echo ""
echo "✅ 报告已写入: $REPORT"
cat "$REPORT"
