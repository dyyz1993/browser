# WPT 子集溯源（vendored）

本目录是 [web-platform-tests](https://github.com/web-platform-tests/wpt) 的
**锁定 revision 精选子集**（AGENTS.md 兼容性评分闭环「标准正确性主线」的
CI 落地 v1），不是本仓原创代码。

| 项 | 值 |
|----|-----|
| 上游 revision | `3d9fd1ffe7c3c7c01ca87874b2c8675a224a33b8`（master，2026-10-10 抓取） |
| 拷贝范围 | `resources/testharness.js`、`resources/testharnessreport.js` + `dom/nodes/*.html` ×11、`dom/events/*.html` ×3（全部 `.html` 型 harness 测试，`.js`/`.window.js` 型需 wpt 多全局包装器，v1 不收） |
| 入选标准 | 对应本仓 DOM 桥已声明的 API 面（Node/CharacterData/Document/Event 基础）|
| 许可 | W3C 3-clause BSD（上游 LICENSE，见 https://github.com/web-platform-tests/wpt/blob/master/LICENSE ) |

## 为什么 vendor 而不是 CI 夜间克隆

wpt 全仓 ~3GB，夜间克隆纯浪费；子集 v1 的目标是**确定性**（固定文件、
固定断言面、零网络），跑分趋势对齐同一快照才有意义。上游升级 = 换一个
revision 重新拷贝（手动，频率低）。

## 如何更新子集

```bash
git clone --filter=blob:none --no-checkout --sparse https://github.com/web-platform-tests/wpt /tmp/wpt
cd /tmp/wpt && git sparse-checkout set dom resources && git checkout <rev>
# 按需拷贝测试文件 → tests/standards/wpt/，并更新本文件 revision
```

## 运行

```bash
python3 tests/standards/run_wpt_subset.py --browser target/release/browser
```

报告制（v1 非门禁）：通过率写 job summary + artifact；基线对比（不回退
门禁）留待 v2。
