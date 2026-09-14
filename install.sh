#!/bin/sh
# browser — 一键安装脚本
# 用法: curl -fsSL https://raw.githubusercontent.com/dyyz1993/browser/main/install.sh | sh
#
# 自动检测:
#   - OS (macOS / Linux)
#   - CPU 架构 (x86_64 / aarch64)
#   - glibc 版本 (老系统自动附带 portable-glibc)
#
# 安装到: ~/.local/bin/browser (或 /usr/local/bin 如果有 root)

set -e

# ─────────────────────────── 颜色输出 ───────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'
info()  { printf "${GREEN}[安装]${NC} %s\n" "$1"; }
warn()  { printf "${YELLOW}[警告]${NC} %s\n" "$1"; }
error() { printf "${RED}[错误]${NC} %s\n" "$1"; exit 1; }

# ─────────────────────────── GitHub 配置 ───────────────────────────
GITHUB_REPO="dyyz1993/browser"
GITHUB_BASE="https://github.com/${GITHUB_REPO}/releases/latest/download"

# ─────────────────────────── 环境检测 ───────────────────────────
info "检测系统环境..."

OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
    Darwin) PLATFORM="darwin" ;;
    Linux)  PLATFORM="linux" ;;
    *)      error "不支持的操作系统: $OS (仅支持 macOS/Linux)" ;;
esac

case "$ARCH" in
    x86_64|amd64)   ARCH_TAG="x86_64" ;;
    arm64|aarch64)  ARCH_TAG="arm64" ;;
    *)              error "不支持的 CPU 架构: $ARCH" ;;
esac

info "  系统: $OS ($ARCH_TAG)"

# ─────────────────────────── glibc 版本检测 (仅 Linux) ───────────────────────────
NEEDS_PORTABLE_GLIBC=0
if [ "$PLATFORM" = "linux" ] && [ "$ARCH_TAG" = "x86_64" ]; then
    GLIBC_VER="$(ldd --version 2>/dev/null | head -1 | grep -oE '[0-9]+\.[0-9]+' | tail -1 || echo '0.0')"
    GLIBC_MAJOR="${GLIBC_VER%%.*}"
    GLIBC_MINOR="${GLIBC_VER#*.}"
    info "  glibc: $GLIBC_VER"

    # V8/boring 需要 glibc >= 2.29
    if [ "$GLIBC_MAJOR" -lt 2 ] || ([ "$GLIBC_MAJOR" -eq 2 ] && [ "$GLIBC_MINOR" -lt 29 ]); then
        NEEDS_PORTABLE_GLIBC=1
        warn "  glibc < 2.29 → 将附带 portable-glibc"
    fi
fi

# ─────────────────────────── 确定下载文件 ───────────────────────────
BINARY_TARBALL="browser-${PLATFORM}-${ARCH_TAG}.tar.gz"
GLIBC_TARBALL="portable-glibc-x86_64.tar.gz"

info "下载: ${BINARY_TARBALL}"

# ─────────────────────────── 下载函数（带重试） ───────────────────────────
download() {
    _file="$1"; _dest="$2"
    _url="${GITHUB_BASE}/${_file}"
    for _try in 1 2 3 4 5; do
        info "  下载(${_try}/5): ${_file}"
        if command -v curl >/dev/null 2>&1; then
            curl -fsSL --connect-timeout 20 --max-time 600 \
                --retry 2 --retry-delay 3 \
                -o "$_dest" "$_url" && return 0
        elif command -v wget >/dev/null 2>&1; then
            wget -q --timeout=20 --tries=2 \
                -O "$_dest" "$_url" && return 0
        fi
        sleep 3
    done
    return 1
}

# ─────────────────────────── 下载 & 解压 ───────────────────────────
TMPDIR="$(mktemp -d /tmp/browser-install.XXXXXX)"
trap 'rm -rf "$TMPDIR"' EXIT

download "$BINARY_TARBALL" "$TMPDIR/browser.tar.gz" \
    || error "下载失败: ${BINARY_TARBALL} (请检查网络，或手动下载 ${GITHUB_BASE}/${BINARY_TARBALL})"

cd "$TMPDIR"
tar xzf browser.tar.gz

# 如果需要 portable-glibc
if [ "$NEEDS_PORTABLE_GLIBC" -eq 1 ]; then
    info "下载: ${GLIBC_TARBALL} (老系统 glibc 兼容包)"
    download "$GLIBC_TARBALL" "$TMPDIR/glibc.tar.gz" \
        || error "下载 portable-glibc 失败 (请检查网络)"
    tar xzf glibc.tar.gz
fi

# ─────────────────────────── 安装到 PATH ───────────────────────────
INSTALL_DIR="$HOME/.local/bin"
mkdir -p "$INSTALL_DIR"

if [ "$NEEDS_PORTABLE_GLIBC" -eq 1 ]; then
    # 老 Linux：安装 browser + portable-glibc + 启动包装脚本
    GLIBC_DIR="$INSTALL_DIR/browser-portable-glibc"
    mkdir -p "$GLIBC_DIR"
    cp -r portable-glibc/* "$GLIBC_DIR/"

    cat > "$INSTALL_DIR/browser" << WRAPPER
#!/bin/sh
# browser 启动包装（自动使用 portable-glibc）
LD="\$HOME/.local/bin/browser-portable-glibc/ld-linux-x86-64.so.2"
LP="\$HOME/.local/bin/browser-portable-glibc:\${LP_EXTRA}:/lib64:/usr/lib64:/lib:/usr/lib"
exec "\$LD" --library-path "\$LP" "\$HOME/.local/bin/browser-bin" "\$@"
WRAPPER
    chmod +x "$INSTALL_DIR/browser"
    cp browser "$INSTALL_DIR/browser-bin"
    chmod +x "$INSTALL_DIR/browser-bin"
    info "已安装: browser + portable-glibc → $INSTALL_DIR"
else
    # 新系统 / macOS：直接放二进制
    cp browser "$INSTALL_DIR/browser"
    chmod +x "$INSTALL_DIR/browser"
    info "已安装: browser → $INSTALL_DIR"
fi

# ─────────────────────────── 验证 ───────────────────────────
VERSION="$("$INSTALL_DIR/browser" --version 2>/dev/null || echo 'unknown')"
info "版本: $VERSION"
info ""

if ! echo "$PATH" | grep -q "$INSTALL_DIR"; then
    warn "PATH 中没有 $INSTALL_DIR，请手动添加:"
    warn "  echo 'export PATH=\$HOME/.local/bin:\$PATH' >> ~/.bashrc && source ~/.bashrc"
    warn ""
fi

info "安装完成！用法："
info "  browser fetch https://example.com/ --format text"
info "  browser fetch https://xcancel.com/xxx --js-engine v8 --format markdown"
info ""
