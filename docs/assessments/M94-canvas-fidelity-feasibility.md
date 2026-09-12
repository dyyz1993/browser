# M94 可行性评估：canvas 真像素渲染能否让 xcancel verify 过线

> 决策问题：剩余 5 项 fp diff 中 canvasFingerprint + hasModifiedCanvas 两项，
> 是否值得立项「canvas 真像素渲染」（路线 B 像素渲染器的超集）去冲 100%？
> 本文全部结论带实测证据（2026-09-13，Chrome 153 / macOS 26 / M2 Max）。

## 一、VM 的 canvas 检测序列（望远镜实测全参数）

M93.19 望远镜（ctx 属性 setter + 方法 wrap）抓到 VM 完整序列：

- canvas **400×200**，`textBaseline='alphabetic'`
- 第一行文字：`fillStyle='#f60'`、`fillRect(125,1,62,20)`、
  `font='11pt no-real-font-123'`（故意不存在的字体名）、
  `fillText('Cwm fjordbank glyphs vext quiz, 😃', 2, 15)`
- 第二行：`fillStyle='#069'`、`fillText(同文, 4, 45)`
- 三圆：`fillStyle='rgba(102,204,0,0.2)'`、`font='18pt Arial'`、
  arc(50,50,50)/(100,50,50)/(75,100,50) 各 closePath+fill
- 终段：`globalCompositeOperation='multiply'`、
  fillStyle 轮换 rgb(255,0,255)/(0,255,255)/(255,255,0)、
  `rect(0,0,10,10)+rect(2,2,6,6)`（残留路径）+ arc(75,75,75)+arc(75,75,25)，
  `fill('evenodd')`
- 读取：`toDataURL()`（Chrome 输出 20522 字符）+ getImageData →
  canvasFingerprint 哈希 / hasModifiedCanvas 篡改检测

这是标准 FingerprintJS 式序列：文字（含 **emoji**）+ 几何 + 半透明 + 混合模式。

## 二、匹配 Chrome toDataURL 需要过的五层（全部实测量化）

### 层 1：字体选择（已实测，可解但需喂表）

Chrome 对未知 family `'no-real-font-123'` 的 canvas 回退 =
`-apple-system`（**宽度指纹铁证**：measureText 全句 217.54px，
与 `-apple-system` 完全一致、Helvetica 206.14、Arial 206.14、
system-ui 214.12）。即 **SF Pro（CoreText 系统字体）**。

系统盘上只有 `SFNS.ttf`（**可变字体**）；fontdue 不支持实例选择，
取默认实例 advance 总宽 197.09 ≠ 217.54（差 10%）。用 Chrome 逐字符
advance 表喂入后布局可对齐（bbox x[2,218]≈[2,219]）。
→ 可解，但每平台/字号要采表，或引入可变字体实例支持。

### 层 2：字形光栅化 AA（**本质算法差，实测死刑级**）

布局完全对齐后（控制变量：Chrome advance + SFNS 字形）逐像素对比
（400×200，单通道 alpha，墨迹 1596 像素）：

| 指标 | 值 |
|------|-----|
| alpha 完全相等 | **60/1596 = 3.8%** |
| \|diff\|≤8 | 13.6% |
| \|diff\|≥64 | **54.5%** |
| 平均差 | **92.9 / 255** |
| 最大差 | 255 |

AA 边缘灰度分布**定性相反**：
- Chrome（Skia gamma-corrected AA）：边缘值集中 **243–251**（对比度增强，
  边缘接近实心）+ 47
- fontdue（纯几何 coverage）：边缘值集中 **1–63**（均匀覆盖）

这是 Skia 文本 AA 的 gamma/contrast LUT（按背景亮度查表）与 fontdue
scanline coverage 的**算法族差异**，不是精度参数可调的噪声。
匹配需复刻 Skia gamma LUT（SkGamma 表硬编码在 Skia 源码，随版本漂移）。

### 层 3：emoji 彩色字形（fontdue 零支持）

fillText 文本含 `😃`（U+1F603）。Chrome 渲染走 Apple Color Emoji
（**sbix PNG 位图字形**，RGBA 直接合成、不吸收 fillStyle）。
fontdue 不支持任何彩色字形格式（CBDT/sbix/COLR）。需要：emoji 字体
回退链 + sbix PNG 解码 + 位图缩放到字号 + 直通合成。

### 层 4：几何 AA 与合成模式

arc 填充的抗锯齿 = Skia **analytic coverage**（精确面积覆盖）；
半透明圆 rgba(102,204,0,0.2) 叠加 + `globalCompositeOperation='multiply'`
+ `fill('evenodd')`。multiply 的结果还依赖色彩空间（canvas sRGB vs
Skia 内部 linearized 合成路径，macOS Chrome 的实现特定）。
需自研路径 AA（解析几何积分）+ 色彩管理一致的合成器。

### 层 5：PNG 编码器字节一致

即使像素 100% 一致，toDataURL 输出取决于编码器实现：Skia PNG encoder
的 zlib 压缩参数/策略、per-row filter 选择、chunk 布局（iCCP/sRGB），
与 png crate 默认行为不同。**dataURL 字节一致要求编码器路径等价**，
这是 zlib 级别的对齐工程。

**五层全部字节级一致才可能让 canvasFingerprint hash == Chrome**。

## 三、业界先例与服务器判定的合理性推断

- **无任何非 Chromium 引擎公开做到与 Chrome canvas 字节级一致**
  （Firefox/Safari 的 canvas hash 与 Chrome 系统性不同——antibot 正是
  用 canvas hash 做浏览器聚类而非单一白名单值）。
- 推论：xcancel 服务端**不可能硬性要求 hash == 某个 Chrome 值**（否则
  真 Firefox/Safari 用户全被拒）。canvas hash 更可能作为聚类/异常特征：
  **「ERROR/常量」比「不同但稳定」更可疑**。M93.19 的 fp 里
  hasModifiedCanvas=ERROR + 恒定 canvasFingerprint=214d9f3c 正是最差形态。

## 四、分阶段成本与收益

| 阶段 | 内容 | 工作量 | 收益 | 判决 |
|------|------|--------|------|------|
| 0 本评估 | 差距量化（本文） | 1 天 | 决策依据 | ✅ 已完成 |
| 1 真像素 canvas | RGBA framebuffer + fontdue 文本 + 解析几何 AA（arc/rect/evenodd）+ multiply 合成 + png crate toDataURL/getImageData；不追求字节一致 | **2–4 周**（render crate 已有 fontdue 资产；CanvasRenderingContext2D 接真实现替换 stub） | canvasFingerprint 变成真实稳定值（同 Firefox 的合法性形态）；hasModifiedCanvas 探测链路可完整应答 → 有实际过线概率 | **建议立项** |
| 2 字节级对齐 Chrome | Skia gamma LUT + CoreText 可变实例 + sbix emoji + Skia PNG 编码器 + 色彩管理合成 | **数月级持续工程**，随 Chrome 版本漂移重做；业界零先例 | hash 可能 == Chrome | **不立项**（宪法复杂度门槛；伪造边缘） |

## 五、判决

1. **阶段 2（100% 字节一致）不可行**：五层差距中 AA 分布（3.8% exact）
   与编码器字节是算法族/实现级差异，成本数月且不可维护，且服务端
   不可能以 Chrome hash 为唯一通过值（Firefox 论证）。
2. **阶段 1 值得做**：它是路线 B 像素渲染器（M80 已立项的 ASCII→画布
   路线）的自然扩展——同一套 framebuffer/光栅化资产服务两个目标
   （GUI 像素渲染 + canvas 2D API）。完成后 canvas 两项 fp 从
   「ERROR/常量」变为「真实稳定」，是诚实引擎能力的最大化。
3. **决策建议**：立项阶段 1（作为 M94），验收标准定为：
   (a) VM 序列在本引擎完整执行不 ERROR、toDataURL 输出真实 PNG；
   (b) hasModifiedCanvas 探测应答非 ERROR；
   (c) 同序列两次运行 hash 稳定（自一致）；
   (d) 实弹 xcancel verify 观察是否过线（经验证而非假定）。
   若阶段 1 后仍 403，则判定剩余分差在服务端不可见区域，100% 目标
   以当前技术栈不可达，记录天花板。

## 附：证据文件

- Chrome 像素基准：/tmp/canvas_chrome.json（CDP getImageData 320000B ×3 组）
- fontdue 原型：/tmp/canvasfid/（SFNS + Chrome advance 对齐后输出）
- Chrome 逐字符 advance：/tmp/chrome_advances.json
- 字体宽度指纹：Helvetica=206.14 / -apple-system=217.54 / 未知 family=217.54
- VM 序列望远镜：/tmp/ours_fp14.out（ctxset.* 记录）

## 六、M94.1 附录：hasModifiedCanvas 向量追猎记录（未破）

四层观测网络全部零命中（详见 PROGRESS M94.1）：方法调用链无异常、
C2D.prototype Proxy 零访问、canvas 元素 Proxy 仅读 4 个属性、Error 构造
日志证明 probe 异常不经 JS Error 构造器（QuickJS 引擎内部抛错）。

hik8ew 解码 dump（replay serve 侧钩子，393 词表 + 4000 序列归档）：
canvas 探测簇位于 webgpu 探测之后（序列 2185-2206），键名以碎片拼接
（'hasMod'+'ifiedC'+'anvas'）；fillText/getContext/toDataURL 等 API 名
**既不在 hik8ew 表也不在源码明文**——VM 用 charCode 拼名（词表含
fromCodePoint/charCodeAt），静态 dump 不可达。

已排除的假说（8 个）：prototype spy（零访问）、构造器 toString、
方法族 toString、toBlob null、convertToBlob 空 Blob、createImageBitmap
缺失、width/height 反射脱节、font 读回未规范化。其中 6 个假说对应的
**修复本身是正确的 Chrome 语义对齐**，已入库（门禁 1024/0）。

下轮候选路径：(a) rquickjs eval 命名支持（EvalOptions 无 name 字段，
需上游或 wrapper 层方案）→ stack 行号映射回 js-challenge.js 源码行 →
直接读探测函数混淆源码；(b) 源码级 charCode 拼名静态重构（AST 反混淆）。
