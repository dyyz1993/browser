#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════════════════
# triple_compare.sh — serve vs Chrome vs Firecrawl 三方对比
#
# 对比维度：
#   1. 耗时（ms）
#   2. 内容字节数
#   3. 完整性（completeness.py 4 指标：serve vs Firecrawl，同 markdown 格式）
#
# 用法：
#   bash tests/benchmarks/triple_compare.sh
# ═══════════════════════════════════════════════════════════════════════

set -euo pipefail

CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
OUT_DIR="/tmp/triple_compare"
mkdir -p "$OUT_DIR"

FIRECRAWL_KEY="fc-a542d60e046c4e9baebf6831e6c576e0"
SERVE_URL="${SERVE_URL:-http://127.0.0.1:3021}"

SITES=(
  "https://example.com"
  "https://bark.day.app/"
  "https://react.dev"
  "https://nuxt.com"
  "https://vuejs.org"
  "https://svelte.dev"
  "https://docs.python.org"
  "https://rust-lang.org"
  "https://go.dev"
  "https://typescriptlang.org"
  "https://tailwindcss.com"
  "https://nodejs.org"
  "https://news.ycombinator.com"
  "https://github.com/rust-lang/rust"
  "https://stripe.com"
)

echo "═══════════════════════════════════════════════════════════════════════"
echo "  三方对比：serve vs Chrome vs Firecrawl（15 站）"
echo "═══════════════════════════════════════════════════════════════════════"
printf "%-22s | %7s %7s %7s | %7s %7s %7s | %5s %5s %5s %5s | %s\n" \
  "Site" "serve" "chrome" "fire" "serve" "chrome" "fire" "blk" "sim" "jacc" "word" "grade"
printf "%.0s─" {1..115}; echo ""

for site in "${SITES[@]}"; do
  name=$(echo "$site" | sed 's|https://||;s|/$||;s|/|_|g')
  short=$(echo "$name" | cut -c1-21)

  # serve (markdown)
  serve_json=$(curl -s --max-time 20 -X POST -H "Content-Type: application/json" \
    -d "{\"url\":\"$site\",\"format\":\"markdown\"}" "$SERVE_URL/" 2>/dev/null || echo '{}')
  echo "$serve_json" | python3 -c "import json,sys;d=json.load(sys.stdin);open('$OUT_DIR/serve_$name.md','w').write(d.get('content',''))" 2>/dev/null
  serve_ms=$(echo "$serve_json" | python3 -c "import json,sys;d=json.load(sys.stdin);print(d.get('_timing',{}).get('total_ms','?'))" 2>/dev/null || echo "?")
  serve_ch=$(wc -c < "$OUT_DIR/serve_$name.md" 2>/dev/null | tr -d ' ')

  # Chrome (dump-dom → text)
  chr_t0=$(python3 -c "import time;print(time.time())")
  timeout 12 "$CHROME" --headless=new --virtual-time-budget=5000 --dump-dom "$site" 2>/dev/null | \
    python3 -c "import sys,re;h=sys.stdin.read();h=re.sub(r'<script[^>]*>.*?</script>','',h,flags=re.DOTALL);h=re.sub(r'<style[^>]*>.*?</style>','',h,flags=re.DOTALL);t=re.sub(r'<[^>]+>',' ',h);t=re.sub(r'\s+',' ',t).strip();print(t)" > "$OUT_DIR/chrome_$name.txt" 2>/dev/null || echo "" > "$OUT_DIR/chrome_$name.txt"
  chr_t1=$(python3 -c "import time;print(time.time())")
  chr_ms=$(python3 -c "print(int(($chr_t1-$chr_t0)*1000))" 2>/dev/null || echo "?")
  chr_ch=$(wc -c < "$OUT_DIR/chrome_$name.txt" 2>/dev/null | tr -d ' ')

  # Firecrawl (markdown)
  fire_t0=$(python3 -c "import time;print(time.time())")
  curl -s --max-time 20 --request POST --url https://api.firecrawl.dev/v2/scrape \
    --header "Authorization: Bearer $FIRECRAWL_KEY" \
    --header "Content-Type: application/json" \
    -d "{\"url\":\"$site\",\"formats\":[\"markdown\"]}" 2>/dev/null | \
    python3 -c "import json,sys;d=json.load(sys.stdin);print(d.get('data',{}).get('markdown',''))" > "$OUT_DIR/fire_$name.md" 2>/dev/null || echo "" > "$OUT_DIR/fire_$name.md"
  fire_t1=$(python3 -c "import time;print(time.time())")
  fire_ms=$(python3 -c "print(int(($fire_t1-$fire_t0)*1000))" 2>/dev/null || echo "?")
  fire_ch=$(wc -c < "$OUT_DIR/fire_$name.md" 2>/dev/null | tr -d ' ')

  # completeness (serve vs firecrawl)
  if [ -s "$OUT_DIR/fire_$name.md" ] && [ "$serve_ch" -gt 10 ]; then
    comp=$(python3 "$SCRIPT_DIR/completeness.py" --ours "$OUT_DIR/serve_$name.md" --theirs "$OUT_DIR/fire_$name.md" --text --grade 2>/dev/null | tail -1 || echo "0 0 0 0 -")
  else
    comp="0 0 0 0 -"
  fi

  printf "%-22s | %5sms %5sms %5sms | %6s %6s %6s | %s\n" \
    "$short" "$serve_ms" "$chr_ms" "$fire_ms" "$serve_ch" "$chr_ch" "$fire_ch" "$comp"
done

echo ""
echo "═══════════════════════════════════════════════════════════════════════"
echo "  blk/sim/jacc/word = serve vs Firecrawl 完整性（同 markdown 格式）"
echo "  产物: $OUT_DIR/"
echo "═══════════════════════════════════════════════════════════════════════"
