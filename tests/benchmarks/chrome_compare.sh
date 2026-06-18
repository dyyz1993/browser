#!/usr/bin/env bash
# M64: 我们的 browser vs Chrome headless 渲染对比
# 公平对比：两者都走完整 CSR（fetch + JS 执行 + 渲染），提取 body 文本内容对比。
set -euo pipefail

CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
BROWSER="./target/release/browser"

# 渲染站点列表（ESM 重点 + 之前有问题的）
SITES=(
  "https://vite.dev/"
  "https://vuejs.org/"
  "https://nuxt.com/"
  "https://bark.day.app/"
  "https://react.dev/"
)

TMPDIR=$(mktemp -d)
trap "rm -rf $TMPDIR" EXIT

printf "%-30s | %-12s | %-12s | %-10s | %-10s\n" "站点" "我们(字符)" "Chrome(字符)" "覆盖率" "重叠词"
printf '%.0s-' {1..100}; echo

for url in "${SITES[@]}"; do
  # 我们的渲染
  $BROWSER fetch "$url" --format text > "$TMPDIR/ours.txt" 2>/dev/null || true
  ours_chars=$(wc -c < "$TMPDIR/ours.txt" | tr -d ' ')

  # Chrome headless 渲染（等待 networkidle，dump DOM text）
  timeout 30 "$CHROME" --headless --disable-gpu --no-sandbox \
    --virtual-time-budget=8000 \
    --dump-dom "$url" > "$TMPDIR/chrome_dom.html" 2>/dev/null || true
  # 从 Chrome DOM 提取文本
  python3 -c "
import re, sys
html = open('$TMPDIR/chrome_dom.html', errors='replace').read()
# 去 script/style
html = re.sub(r'<script[^>]*>.*?</script>', '', html, flags=re.DOTALL)
html = re.sub(r'<style[^>]*>.*?</style>', '', html, flags=re.DOTALL)
text = re.sub(r'<[^>]+>', ' ', html)
text = re.sub(r'\s+', ' ', text).strip()
open('$TMPDIR/chrome.txt', 'w').write(text)
" 2>/dev/null || echo "" > "$TMPDIR/chrome.txt"
  chrome_chars=$(wc -c < "$TMPDIR/chrome.txt" | tr -d ' ')

  # 覆盖率 = 我们的内容长度 / Chrome 内容长度（粗略指标）
  if [ "$chrome_chars" -gt 0 ] 2>/dev/null; then
    coverage=$(python3 -c "print(f'{$ours_chars/$chrome_chars*100:.0f}%')")
  else
    coverage="N/A"
  fi

  # 词重叠率（更有意义）：取 Chrome 文本的前 30 个有意义词，看我们有多少
  overlap=$(python3 -c "
import re
ours = open('$TMPDIR/ours.txt', errors='replace').read().lower()
chrome = open('$TMPDIR/chrome.txt', errors='replace').read().lower()
# 提取 Chrome 的有意义词（长度>=4，去常见停用词）
words = [w for w in re.findall(r'[a-z]{4,}', chrome)]
# 取前 40 个不重复词
seen = set()
sample = []
for w in words:
    if w not in seen:
        seen.add(w)
        sample.append(w)
    if len(sample) >= 40:
        break
if not sample:
    print('N/A')
else:
    found = sum(1 for w in sample if w in ours)
    print(f'{found}/{len(sample)}')
" 2>/dev/null || echo "N/A")

  printf "%-30s | %-12s | %-12s | %-10s | %-10s\n" "$url" "$ours_chars" "$chrome_chars" "$coverage" "$overlap"
done

echo ""
echo "覆盖率 = 我们文本字符数 / Chrome 字符数"
echo "重叠词 = Chrome 前 40 个有意义词中有多少出现在我们的输出里"
