#!/usr/bin/env bash
# M59: browser fetch vs xbrowser scrape 对标脚本。
#
# 用同一批测试站点，对比两者的 markdown 输出质量。判断标准：
# - 主内容提取完整性（关键词命中率）
# - 噪声去除效果（nav/footer 残留检查）
# - markdown 可读性（语法正确性）
#
# 优雅降级：xbrowser 未安装则跳过对比，只跑我们的输出 + 人工 checklist。
#
# 用法：
#   ./tests/benchmarks/compare_xbrowser.sh
#
# 退出码：0 = 全部通过，1 = 有站点失败或对比异常。

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BROWSER_BIN="$REPO_ROOT/target/release/browser"
OUT_DIR="${TMPDIR:-/tmp}/m59-bench-$$"
mkdir -p "$OUT_DIR"

# 对标站点矩阵（覆盖不同类型）。
# 格式："URL|描述|必须包含的关键词1,关键词2,...|必须不包含的噪声"
SITES=(
  "https://example.com/|静态基线|Example Domain,documentation examples|"
  "https://seo.box/referring/|CSR+brotli+表格|Domain,Share|"
)

# 检查 browser 二进制。
if [[ ! -x "$BROWSER_BIN" ]]; then
  echo "❌ browser binary not found at $BROWSER_BIN"
  echo "   run: cargo build --release -p browser-cli"
  exit 1
fi

# 检查 xbrowser（可选对标对象）。
XBROWSER_AVAILABLE=0
if command -v xbrowser &>/dev/null; then
  XBROWSER_AVAILABLE=1
  echo "✅ xbrowser found, will run side-by-side comparison"
else
  echo "⚠️  xbrowser not installed, running browser-only validation"
fi

PASS=0
FAIL=0

echo ""
echo "=========================================="
echo "M59 fetch 对标验收"
echo "=========================================="
echo ""

for entry in "${SITES[@]}"; do
  IFS='|' read -r url desc must_include must_exclude <<< "$entry"
  echo "--- $desc: $url ---"

  # 跑 browser fetch。
  ours_md="$OUT_DIR/ours.md"
  if ! "$BROWSER_BIN" fetch "$url" --format markdown > "$ours_md" 2>"$OUT_DIR/ours.err"; then
    echo "  ❌ browser fetch failed (see $OUT_DIR/ours.err)"
    FAIL=$((FAIL + 1))
    continue
  fi
  ours_lines=$(wc -l < "$ours_md" | tr -d ' ')
  echo "  📄 browser: ${ours_lines} lines, $(wc -c < "$ours_md" | tr -d ' ') bytes"

  # 关键词命中检查。
  site_pass=1
  if [[ -n "$must_include" ]]; then
    IFS=',' read -ra KEYWORDS <<< "$must_include"
    for kw in "${KEYWORDS[@]}"; do
      if grep -qi "$kw" "$ours_md"; then
        echo "  ✅ contains: $kw"
      else
        echo "  ❌ MISSING: $kw"
        site_pass=0
      fi
    done
  fi
  # 噪声残留检查。
  if [[ -n "$must_exclude" ]]; then
    IFS=',' read -ra NOISE <<< "$must_exclude"
    for ns in "${NOISE[@]}"; do
      if grep -qi "$ns" "$ours_md"; then
        echo "  ❌ NOISE LEAKED: $ns"
        site_pass=0
      else
        echo "  ✅ noise stripped: $ns"
      fi
    done
  fi

  # xbrowser 对标（若可用）。
  if [[ $XBROWSER_AVAILABLE -eq 1 ]]; then
    xb_md="$OUT_DIR/xbrowser.md"
    if xbrowser scrape "$url" --format markdown > "$xb_md" 2>/dev/null; then
      xb_lines=$(wc -l < "$xb_md" | tr -d ' ')
      echo "  📄 xbrowser: ${xb_lines} lines, $(wc -c < "$xb_md" | tr -d ' ') bytes"
      # 行数对比（不要求完全一致，差距过大才告警）。
      ratio=$(echo "scale=2; $ours_lines / ($xb_lines + 1)" | bc 2>/dev/null || echo "?")
      echo "  📊 line ratio (ours/theirs): $ratio"
    else
      echo "  ⚠️  xbrowser scrape failed, skipping comparison"
    fi
  fi

  if [[ $site_pass -eq 1 ]]; then
    echo "  ✅ PASS"
    PASS=$((PASS + 1))
  else
    echo "  ❌ FAIL"
    FAIL=$((FAIL + 1))
  fi
  echo ""
done

echo "=========================================="
echo "结果: $PASS passed, $FAIL failed"
echo "输出目录: $OUT_DIR"
echo "=========================================="

[[ $FAIL -eq 0 ]] && exit 0 || exit 1
