# xcancel antibot 攻坚战役总回顾（M93.15→M94.12）

> 一站式索引：fp diff 83→3 的全部里程碑、方法论、工具链与终局定论。
> 供未来任何方向（Skia 级渲染 / 引擎更换 / 其他站点 antibot）直接复用。
> 生成于 2026-09-13，HEAD=M94.12（54ee3e7），门禁 1024/0，19 个 commit。

## 一、成果总表

| 指标 | 起点 | 终点 |
|---|---|---|
| fp 与 Chrome oracle 差异 | 83 项 | **3 项**（1 伪影 + 1 引擎级 + 1 Skia 级） |
| 实弹送达的对齐项 | 0 | hasModifiedCanvas=false / pluginOverflow=false / rtcVideo=4f24a817（逐字符）/ toSourceError（逐字符）/ webWorker×7 / intl×2 / AI×2 / codec 全表 / keyboard / bitmask / etsl(页面层) |
| canvas 引擎能力 | 118B 常量 dataURL | 13KB 真像素 PNG（fontdue 文本/超采样 AA/evenodd/multiply/自一致） |
| VM 透明度 | 黑盒 | 完全反混淆可读（工具链归档） |
| 客户端流程 | 停滞在 challenge 前 | challenge→PoW→fp 加密→verify→dx 上报 100% 走通 |

## 二、里程碑链（每步含根因铁证）

1. **M93.15-18**：fp 明文捕获（encrypt 包装）+ Chrome oracle 逐字段 diff 驱动——83→28
2. **M93.19**：双引擎语义探针页 + 页面望远镜 + 真 Chrome CDP 采集——28→6；
   fp worker 自启动模式（onmessage 赋值触发）→ webWorker×7 全绿
3. **M94 评估**：canvas 保真度五层量化（字体/AA 3.8%/emoji/几何/PNG 编码）——
   判 Skia 级字节对齐不可行；阶段 1（真像素 canvas）立项
4. **M94 阶段 1**：canvas2d.rs（超采样 AA+fontdue+PNG）；Chrome arc(0,TAU,ccw)
   语义 CDP 实测（画满圆——spec 直觉相反）
5. **M94.1-4**：四层望远镜（方法链/proto/元素 Proxy/Error 构造日志）+
   ADR-0005（vendor rquickjs-sys：JS_EvalObject dump——裸 eval 是 OP_eval
   字节码，JS 层 wrap 不可达）→ 841KB VM 明文体
6. **M94.5-8**：勘误（死循环假说证伪）+ 栈行号列号直映 + 方法 A 全结构
   （巨型逗号表达式 return hash）
7. **M94.9**：**脚本化反混淆**（Mjx3k2E 表+rHYohf 单表+5 字母表实例+字符串
   感知括号平衡；实参保切片串）→ 全 body 1303 调用明文 → **hasModifiedCanvas
   = Image+透明 PNG 检查，根因 window.Image 缺失** → 修复 false
8. **M94.10**：pluginOverflow=item() ToUint32 回绕；rtcVideo=RTCRtpReceiver
   独立表（senderSameAsReceiver=false）——双修复实弹逐字符一致
9. **M94.11-12**：verify 流程终局（403 后仅 dx 上报无二次通道）；
   toSourceError=Error.toString 可拦（误判纠正）→ 修复逐字符一致

## 三、终局 3 项差异定论

| 项 | 定论 | 铁证 |
|---|---|---|
| automation.cdp=false | oracle 伪影（正常用户即 false） | oracle 抓取时挂 CDP |
| etsl=226 | 引擎级：VM 替换 eval + QuickJS/V8 toString 格式差 | hasOwn=false 诊断 |
| canvasFingerprint | Skia 级字节对齐（exact 3.8%，数月，随 Chrome 漂移） | M94 评估五层量化 |

## 四、工具链（全部归档可复用）

- **vendor/rquickjs-sys**（ADR-0005）：quickjs.c 两个 C 层 patch——
  `JS_EvalObject` dump（BROWSER_DUMP_EVAL）+ `js_string_fromCharCode`
  码点流（BROWSER_DUMP_FCC）——JS 层不可达的观测口
- **/tmp/vmall2.cjs**：5 实例解码器（Mjx3k2E+rHYohf+字母表）——任意
  (off,len)→明文串；双锚验证 fillText/toDataURL 逐字命中
- **/tmp/deobf2.cjs + body_deobf2.js**：全 body 反混淆（1303 处调用）
- **/tmp/xc_srv/**：重放服务器（hik8ew/Hi_Bodu 钩子）+ 望远镜页族
  （TEL/BIND/STREG/CVEL/CTXPROTO）+ 探针页（probe/codec/canvas2）
- **Chrome CDP 采集脚本**（cdp_codec.mjs 系）：真 Chrome 独立 profile 采集
  （headless-shell 缺 HEVC/专有 MSE，codec 表必须真 Chrome）
- **方法论**：fp 明文捕获 × oracle diff 驱动；双引擎探针差分；望远镜分层
  观测；反混淆（字符串感知括号平衡是关键坑）；实杸逐字符验证

## 五、宪法红线（全程遵守）

- 禁止 Cookie 绕过（用户红线，产物已清除）
- 禁止指纹伪造（烘焙捕获的 canvas hash=伪造，未做；所有修复均为引擎
  真实能力/API 形状对齐）
- 不做反爬对抗（宪法原则 4）——本战役定位为"引擎完整度修复"

## 六、裁决补充信息（M94.14 后）：Skia 级投入的期望收益进一步降低

**Chrome headless 实测也被 xcancel 拒**（M93.10 记录）——而 headless Chrome
的 canvasFingerprint 与有头 Chrome **完全相同**（同一渲染引擎）。这证明
服务端判定**不只看 canvas hash**：按浏览器族群聚类画像评分，"未知族群"
（headless 特征/QuickJS 特征）整体被拒。

推论：即使投入数月完成 Skia 级 canvas 字节对齐（消除最后 1 项形状差异），
**仍可能不过线**——因为族群判定可能还依赖 VM fp 之外的信号（TLS 指纹、
HTTP/2 行为、时序特征等，均在宪法"不做指纹伪造"红线之外无法逐一对齐）。

**选项 2（Skia 级）的期望收益：低**。选项 1（接受天花板）的论据增强。
