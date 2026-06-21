#!/usr/bin/env python3
"""完整性度量工具 —— 对比两份 HTML/文本产物，输出 4 个量化指标。

用法：
  python3 completeness.py --ours qjs.html --theirs chrome.html
  python3 completeness.py --ours qjs.html --theirs chrome.html --text   # 输入是纯文本而非 HTML
  cat ours.txt | python3 completeness.py --theirs chrome.html            # ours 从 stdin
  python3 completeness.py --ours a.html --theirs b.html --grade          # 加综合评级

输出：TSV 四列（block_cov  sim_ratio  struct_jaccard  word_cov），值域 [0,1]。
加 --grade 会额外输出一列综合评级（A-F）。

4 个指标：
  block_cov      关键内容块覆盖率（Chrome 的文本块里 ours 命中多少）
  sim_ratio      文本相似度（difflib SequenceMatcher.ratio）
  struct_jaccard 链接集 Jaccard 相似度（a[href] 归一化后 |交集|/|并集|）
  word_cov       词频覆盖（Chrome 正文高频词 ours 命中比例）

设计原则：
  - 纯标准库（difflib / html.parser / collections / re），不引第三方依赖
  - 去噪：nav/footer/script/style/aside/header 等噪声子树不计入，测的是正文完整性
    （而非页面总字节数——这是旧 wc -c 指标的根本缺陷）

详见 AGENTS.md 第三章「内容完整性度量」。
"""

import argparse
import re
import sys
from collections import Counter
from difflib import SequenceMatcher
from html.parser import HTMLParser

# ── 噪声标签：提取正文块/词频时跳过的子树 ──
# 复用 extractor clean.rs 的 Firecrawl 思路（nav/footer/script/style/aside/header）
NOISE_TAGS = {
    "script", "style", "noscript", "nav", "footer", "aside", "header",
    "svg", "iframe", "form", "button",
}
# 内容块标签：这些元素的直接文本算一个"块"
BLOCK_TAGS = {"p", "li", "h1", "h2", "h3", "h4", "h5", "h6", "td", "th", "blockquote", "figcaption", "dt", "dd"}
# 英文停用词（词频覆盖时过滤）
STOPWORDS = {
    "the", "and", "for", "are", "but", "not", "you", "all", "any", "can", "her",
    "was", "one", "our", "out", "has", "have", "from", "this", "that", "with",
    "will", "your", "they", "them", "their", "what", "when", "which", "who",
    "how", "where", "here", "there", "than", "then", "into", "over", "more",
    "such", "some", "about", "also", "been", "were", "its", "it's", "is", "be",
    "as", "at", "by", "an", "or", "on", "if", "so", "do", "no", "we", "he",
    "she", "my", "me", "us", "of", "to", "in", "a", "it",
}

WORD_RE = re.compile(r"[a-zA-Z0-9]{4,}")


class DOMExtractor(HTMLParser):
    """一次性提取：去噪后的纯文本、文本块、链接集。

    用栈跟踪当前是否在噪声子树内（depth 计数）和当前块标签。
    """

    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.text_parts = []          # 去噪后的纯文本片段
        self.blocks = []              # 文本块列表（归一化后的字符串）
        self.links = set()            # a[href] 归一化 URL 集合
        self._noise_depth = 0         # >0 表示在噪声子树内
        self._block_tag = None        # 当前块标签（或 None）
        self._block_buf = []          # 当前块文本缓冲
        self._cur_href = None

    def handle_starttag(self, tag, attrs):
        tag = tag.lower()
        if tag in NOISE_TAGS:
            self._noise_depth += 1
            return
        if self._noise_depth > 0:
            return
        if tag in BLOCK_TAGS:
            # 进入新块：先 flush 旧块
            self._flush_block()
            self._block_tag = tag
        if tag == "a":
            for k, v in attrs:
                if k == "href" and v:
                    self._cur_href = v

    def handle_endtag(self, tag):
        tag = tag.lower()
        if tag in NOISE_TAGS:
            if self._noise_depth > 0:
                self._noise_depth -= 1
            return
        if self._noise_depth > 0:
            return
        if tag in BLOCK_TAGS and self._block_tag == tag:
            self._flush_block()
            self._block_tag = None
        if tag == "a" and self._cur_href is not None:
            norm = normalize_url(self._cur_href)
            if norm:
                self.links.add(norm)
            self._cur_href = None

    def handle_data(self, data):
        if self._noise_depth > 0:
            return
        self.text_parts.append(data)
        if self._block_tag is not None:
            self._block_buf.append(data)

    def _flush_block(self):
        if self._block_tag is None:
            return
        text = normalize_ws("".join(self._block_buf))
        # 去重式归一：只保留长度 >= 2 的块（过滤纯标点/空白）
        if len(text) >= 2:
            self.blocks.append(text)
        self._block_buf = []

    def text(self):
        return normalize_ws("".join(self.text_parts))


def normalize_ws(s):
    """压缩空白：连续空白→单空格，去除首尾。"""
    return re.sub(r"\s+", " ", s).strip()


def normalize_url(u):
    """链接归一化：去 fragment/query，小写 scheme+host。"""
    u = u.strip()
    if not u or u.startswith(("javascript:", "mailto:", "tel:", "#")):
        return None
    # 去 fragment
    u = u.split("#", 1)[0]
    # 去 query（可选——这里去掉，因为 query 常含 tracking 参数）
    u = u.split("?", 1)[0]
    u = u.rstrip("/")
    return u.lower()


def extract(html_or_text, is_html):
    """从输入提取 DOMExtractor 结果。纯文本模式只填充 text，不提取块/链接。"""
    if is_html:
        p = DOMExtractor()
        p.feed(html_or_text)
        p.close()
        return p
    # 纯文本：包一个 <p> 让它成为一个块
    p = DOMExtractor()
    p._block_tag = "p"
    p._block_buf = [html_or_text]
    p._flush_block()
    p._block_tag = None
    p.text_parts = [html_or_text]
    return p


def block_coverage(ours_blocks, theirs_blocks):
    """关键内容块覆盖率 = theirs 的块里 ours 命中多少（|交集|/|theirs|）。

    用子串匹配（theirs 的块是否作为 ours 某个块的子串出现），容许 ours 块更大。
    归一化大小写后再比。
    """
    if not theirs_blocks:
        return 1.0 if not ours_blocks else 0.0
    ours_lower = [b.lower() for b in ours_blocks]
    hit = 0
    for tb in theirs_blocks:
        tlb = tb.lower()
        if any(tlb and tlb in ob for ob in ours_lower):
            hit += 1
    return hit / len(theirs_blocks)


def text_similarity(ours_text, theirs_text):
    """文本相似度 = SequenceMatcher.ratio（0-1）。"""
    if not ours_text and not theirs_text:
        return 1.0
    return SequenceMatcher(None, ours_text.lower(), theirs_text.lower()).ratio()


def struct_jaccard(ours_links, theirs_links):
    """链接集 Jaccard 相似度 = |交集|/|并集|。"""
    union = ours_links | theirs_links
    if not union:
        return 1.0  # 两边都没链接，视为结构一致
    inter = ours_links & theirs_links
    return len(inter) / len(union)


def word_coverage(ours_text, theirs_text, top_n=40):
    """词频覆盖 = theirs 正文高频词 top_n 里 ours 命中多少。

    只算 len>=4 的词，去停用词。如果 theirs 词太少则降级用全部词。
    """
    theirs_words = [
        w for w in WORD_RE.findall(theirs_text.lower()) if w not in STOPWORDS
    ]
    if not theirs_words:
        return 1.0 if not ours_text else 0.0
    freq = Counter(theirs_words)
    top = [w for w, _ in freq.most_common(top_n)]
    ours_lower = ours_text.lower()
    hit = sum(1 for w in top if w in ours_lower)
    return hit / len(top) if top else 0.0


def grade(score):
    """综合评级：4 指标加权平均 → A-F。"""
    if score >= 0.85:
        return "A"
    if score >= 0.70:
        return "B"
    if score >= 0.55:
        return "C"
    if score >= 0.35:
        return "D"
    return "F"


def measure(ours, theirs, ours_is_html=True, theirs_is_html=True):
    """主度量函数，返回 dict。"""
    o = extract(ours, ours_is_html)
    t = extract(theirs, theirs_is_html)
    bc = block_coverage(o.blocks, t.blocks)
    sr = text_similarity(o.text(), t.text())
    sj = struct_jaccard(o.links, t.links)
    wc = word_coverage(o.text(), t.text())
    # 综合：块覆盖 0.4 + 词频 0.3 + 相似度 0.2 + 结构 0.1
    composite = bc * 0.4 + wc * 0.3 + sr * 0.2 + sj * 0.1
    return {
        "block_cov": bc,
        "sim_ratio": sr,
        "struct_jaccard": sj,
        "word_cov": wc,
        "composite": composite,
        "grade": grade(composite),
        "ours_blocks": len(o.blocks),
        "theirs_blocks": len(t.blocks),
        "ours_links": len(o.links),
        "theirs_links": len(t.links),
    }


def main():
    ap = argparse.ArgumentParser(description="内容完整性度量（4 指标）")
    ap.add_argument("--ours", help="我方产物文件（HTML 或文本，默认 HTML）")
    ap.add_argument("--theirs", required=True, help="对方产物文件（HTML 或文本）")
    ap.add_argument("--text", action="store_true", help="双方输入均为纯文本（非 HTML）")
    ap.add_argument("--grade", action="store_true", help="追加综合评级列")
    ap.add_argument("-v", "--verbose", action="store_true", help="输出明细")
    args = ap.parse_args()

    if args.ours:
        with open(args.ours, encoding="utf-8", errors="replace") as f:
            ours = f.read()
    else:
        ours = sys.stdin.read()
    with open(args.theirs, encoding="utf-8", errors="replace") as f:
        theirs = f.read()

    is_html = not args.text
    m = measure(ours, theirs, ours_is_html=is_html, theirs_is_html=is_html)

    if args.verbose:
        print(f"block_cov      = {m['block_cov']:.3f}  (ours={m['ours_blocks']} theirs={m['theirs_blocks']} blocks)")
        print(f"sim_ratio      = {m['sim_ratio']:.3f}")
        print(f"struct_jaccard = {m['struct_jaccard']:.3f}  (ours={m['ours_links']} theirs={m['theirs_links']} links)")
        print(f"word_cov       = {m['word_cov']:.3f}")
        print(f"composite      = {m['composite']:.3f}  grade={m['grade']}")
    else:
        cols = [f"{m['block_cov']:.3f}", f"{m['sim_ratio']:.3f}",
                f"{m['struct_jaccard']:.3f}", f"{m['word_cov']:.3f}"]
        if args.grade:
            cols.append(f"{m['composite']:.3f}\t{m['grade']}")
        print("\t".join(cols))


if __name__ == "__main__":
    main()
