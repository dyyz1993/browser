# ADR-0005: vendor rquickjs-sys + QuickJS eval 体 dump 钩子

日期：2026-09-13（M94.4）｜状态：已接受

## 背景
xcancel VM 的探测代码在运行时经 `Function("...")` 构造的混淆明文体执行
（QuickJS 栈名 `<input>`）。裸 `eval()`/`Function()` 在 QuickJS 是字节码级
OP_eval（quickjs.c:26821 无条件编译），JS 层 wrap globalThis.eval 不可达
（实测 wrap 生效但零调用）。

## 决策
- vendor rquickjs-sys 0.12.0 到 `vendor/rquickjs-sys`，patch quickjs.c 的
  `JS_EvalObject`：>40 字节代码体 + `BROWSER_DUMP_EVAL` 环境变量时写
  `/tmp/qjs_eval_dump_<n>.txt`。
- 根 Cargo.toml `[patch.crates-io]` 指向本地 vendor。
- 依赖白名单不变（rquickjs 本就白名单；这是同版本源码补丁，非新增依赖）。

## 影响
- 默认零行为变化（钩子环境变量门控）；诊断资产永久可用。
- 维护成本：rquickjs 升版本时需重新 vendor + 重打补丁（补丁仅 8 行）。
- unsafe 纪律：补丁在 rquickjs-sys 内部（workspace 仍 forbid unsafe）。

## 成果（M94.4）
dump 出 VM 完整明文体（841KB）→ 破译 hasModifiedCanvas 完整因果链：
`GgWjAt` 防篡改壳（piFTVKR 校验函数 toString 的正则清洗结果）失败 →
`while(true){}` 死循环 → 引擎 interrupt 打断 → VM `catch(n){return A5sPdo}`
→ ERROR。校验依赖引擎特定 toString 字节行为 → QuickJS 跨引擎天花板
（与 toSourceError 的 C 层文案同族）。Chrome 下校验通过故 fp=false。
