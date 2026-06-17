#!/usr/bin/env bash
# M59: browser fetch 性能基准测量脚本。
#
# 测量四个维度：内存（峰值 RSS）、速度（wall time）、输出大小、质量（关键词命中）。
# 每站点跑 N 次取中位数，消除网络抖动。结果输出为机器可读 TSV + 人类可读摘要。
#
# 用法：
#   ./tests/benchmarks/bench_fetch.sh [rounds]
#     rounds: 每站点测量次数，默认 3
#
# 产物：${TMPDIR}/m59-perf/ 下
#   - summary.tsv   机器可读（站点 \t 耗时中位数 \t RSS中位数 \t 输出字节）
#   - summary.txt   人类可读摘要
#   - <site>.md     每站点的 markdown 输出（供质量检查）

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BROWSER_BIN="$REPO_ROOT/target/release/browser"
ROUNDS="${1:-3}"
OUT_DIR="${TMPDIR:-/tmp}/m59-perf-$$"
mkdir -p "$OUT_DIR"

if [[ ! -x "$BROWSER_BIN" ]]; then
  echo "❌ browser binary not found at $BROWSER_BIN"
  echo "   run: cargo build --release -p browser-cli"
  exit 1
fi

# 对标站点矩阵（覆盖静态/CSR/文档站/SPA/中文站）。
# 格式："tag|url|format|extra-args|must-contain-keyword"
SITES=(
  "static-example|https://example.com/|markdown|--no-js|Example Domain"
  "csr-seo-box|https://seo.box/referring/|markdown|--selector table|Domain"
  "csr-seo-box-nosel|https://seo.box/referring/|markdown||Total Share"
  "static-rust-lang|https://www.rust-lang.org/|markdown|--no-js|Rust"
  "docs-firecrawl|https://docs.firecrawl.dev/|markdown||Firecrawl"
  "spa-bark|https://bark.day.app/|markdown||Bark"
)

# macOS / Linux 兼容的峰值 RSS 读取（单位：字节）。
# macOS /usr/bin/time -l 输出 "maximum resident set size" 单位是字节。
# Linux /usr/bin/time -v 单位是 KB——本脚本主要在 macOS 跑，按字节处理。
peak_rss_bytes() {
  local timefile="$1"
  local rss
  rss=$(grep -i "maximum resident set size" "$timefile" | awk '{print $1}')
  echo "${rss:-0}"
}

# 中位数（输入：空格分隔的数字列表）。
median() {
  local nums=($1)
  local sorted
  sorted=$(printf '%s\n' "${nums[@]}" | sort -n)
  local count=${#nums[@]}
  local mid=$(( (count + 1) / 2 ))
  echo "$sorted" | sed -n "${mid}p"
}

SUMMARY_TSV="$OUT_DIR/summary.tsv"
SUMMARY_TXT="$OUT_DIR/summary.txt"
echo -e "site\tformat\twall_ms_median\trss_bytes_median\toutput_bytes" > "$SUMMARY_TSV"

echo "=========================================="
echo "M59 fetch 性能基准（$ROUNDS 轮/站点）"
echo "binary: $BROWSER_BIN ($(du -h "$BROWSER_BIN" | cut -f1))"
echo "=========================================="

for entry in "${SITES[@]}"; do
  IFS='|' read -r tag url fmt extra kw <<< "$entry"
  echo ""
  echo "--- $tag: $url ---"
  echo "    format=$fmt extra=[$extra]"

  times_ms=()
  rsses_bytes=()
  last_output=""
  round=1
  while [[ $round -le $ROUNDS ]]; do
    timefile="$OUT_DIR/$tag.r${round}.time"
    outfile="$OUT_DIR/$tag.r${round}.out"
    # 拼参数：fetch <url> --format <fmt> [extra...]
    # shellcheck disable=SC2086
    start=$(date +%s%N 2>/dev/null || python3 -c 'import time;print(int(time.time()*1e9))')
    /usr/bin/time -lp "$BROWSER_BIN" fetch "$url" --format "$fmt" $extra \
      > "$outfile" 2> "$timefile"
    rc=$?
    end=$(date +%s%N 2>/dev/null || python3 -c 'import time;print(int(time.time()*1e9))')
    if [[ $rc -ne 0 ]]; then
      echo "  ⚠️ round $round failed (rc=$rc)"
      round=$((round + 1))
      continue
    fi
    wall_ms=$(( (end - start) / 1000000 ))
    rss_bytes=$(peak_rss_bytes "$timefile")
    times_ms+=("$wall_ms")
    rsses_bytes+=("$rss_bytes")
    last_output="$outfile"
    echo "  round $round: ${wall_ms}ms, RSS $((rss_bytes / 1048576))MB"
    round=$((round + 1))
  done

  if [[ ${#times_ms[@]} -eq 0 ]]; then
    echo "  ❌ all rounds failed"
    echo -e "$tag\t$fmt\tFAIL\tFAIL\t0" >> "$SUMMARY_TSV"
    continue
  fi

  t_med=$(median "${times_ms[*]}")
  r_med=$(median "${rsses_bytes[*]}")
  out_bytes=$(wc -c < "$last_output" | tr -d ' ')

  # 质量检查：关键词命中。
  if [[ -n "$kw" ]]; then
    if grep -qi "$kw" "$last_output"; then
      quality="✅ contains '$kw'"
    else
      quality="❌ MISSING '$kw'"
    fi
  else
    quality="(no keyword check)"
  fi

  echo "  📊 median: ${t_med}ms wall, $((r_med / 1048576))MB RSS, ${out_bytes}B output"
  echo "  🔍 quality: $quality"
  # 保存该站点最后一次输出供人工查看。
  cp "$last_output" "$OUT_DIR/$tag.md"
  echo -e "$tag\t$fmt\t$t_med\t$r_med\t$out_bytes" >> "$SUMMARY_TSV"
done

echo ""
echo "=========================================="
echo "TSV: $SUMMARY_TSV"
echo "输出目录: $OUT_DIR"
echo "=========================================="
cat "$SUMMARY_TSV"
