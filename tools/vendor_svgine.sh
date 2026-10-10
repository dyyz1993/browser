#!/usr/bin/env bash
# vendor_svgine.sh — 从兄弟仓 svgine 同步 svgine-core/svgine-simd 快照到本仓。
#
# 背景：svgine-host（ADR-0019 阶段二）依赖 svgine-core；快照已入仓
# （M97.25），因 svgine 仓无远端、不能被 CI 拉取。上游有更新时跑本脚本
# 重新同步，同步后必须过三门禁再 commit：
#   cargo fmt --all -- --check
#   cargo clippy --workspace --all-targets -- -D warnings
#   cargo test --workspace --no-fail-fast
#
# 用法：tools/vendor_svgine.sh [svgine仓路径]   # 默认 ../svgine
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC_ROOT="${1:-$(cd "$ROOT/.." && pwd)/svgine}"
UPSTREAM_SHA="$(git -C "$SRC_ROOT" rev-parse HEAD)"
TODAY="$(date +%F)"

for crate in svgine-core svgine-simd; do
  test -d "$SRC_ROOT/crates/$crate" || { echo "✗ $SRC_ROOT/crates/$crate 不存在" >&2; exit 1; }
  # --delete 保持快照与上游一致；排除 Cargo.toml（本仓已做 workspace 内联化适配）
  rsync -a --delete --exclude Cargo.toml \
    "$SRC_ROOT/crates/$crate/" "$ROOT/crates/$crate/"
done

# 剔除依赖 svgine 仓根布局的契约测试：tests/*.rs 里引用 ../../（repo root
# 语料 tests/oracle、tests/corpus）的文件在本仓布局下路径失效（M97.25 实测
# panic）。src 单测 + 自包含集成测试保留，语料验收由上游仓自治。
PRUNED=$(grep -rl '\.\./\.\.' "$ROOT/crates/svgine-core/tests" --include='*.rs' 2>/dev/null || true)
if [ -n "$PRUNED" ]; then
  echo "$PRUNED" | xargs rm -v
fi

# 刷新两份 VENDOR.md 的溯源表（commit 行 + 快照日期行）
for md in "$ROOT/crates/svgine-core/VENDOR.md"; do
  python3 - "$md" "$UPSTREAM_SHA" "$TODAY" <<'PY'
import re, sys
path, sha, today = sys.argv[1], sys.argv[2], sys.argv[3]
text = open(path).read()
text = re.sub(r"(\| 上游 commit \| `)[0-9a-f]{40}(` \|)", r"\g<1>" + sha + r"\g<2>", text)
text = re.sub(r"(\| 快照日期 \| )\d{4}-\d{2}-\d{2}( \|)", r"\g<1>" + today + r"\g<2>", text)
open(path, "w").write(text)
PY
done

echo "✓ svgine-core / svgine-simd 已同步到 $UPSTREAM_SHA（$TODAY）"
echo "  下一步：过三门禁 → conventional commit（fix(svgine-host): vendor 同步）"
