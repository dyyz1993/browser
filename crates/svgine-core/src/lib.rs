//! svgine-core —— 可移植 SVG 渲染核心。
//!
//! 宪法（详见 docs/GOALS.md，冲突时按序优先）：
//!
//! 1. **no_std 纪律**：本 crate 永远 `#![no_std] + alloc`，CI 对裸机目标
//!    交叉 check，变红即拒绝。
//! 2. **纯函数渲染**：`render(doc, viewport, time_ms) -> 像素`，不读系统
//!    时钟、不做任何 I/O；时间与外部资源由宿主注入。
//! 3. **DisplayList 是唯一移植边界**：平台差异不进入本 crate 的上层抽象，
//!    平台后端只消费 [`dl`] 定义的命令流。
//!
//! 分层：`xml`（解析）→ `tree`/`style`（树+样式）→ `geom`（几何）→
//! `dl`（显示列表）→ `raster`（参考软光栅化）；`anim` 与渲染解耦，
//! 只产出 t 时刻的属性覆盖表。
#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

// 体积门控（ADR-0009）：可选能力模块按 feature 编译；dl.rs 的
// DisplayList 契约不受门控（池/命令无条件存在，生产/消费路径才门）。
// gradient 模块本体恒编译（GradRaw/StopRaw 是解析期裸属性袋，
// stash_or_len 签名依赖）；重活（collect/bake/sample）在模块内门控。
#[cfg(feature = "anim")]
pub mod anim;
pub mod builder;
#[cfg(feature = "css")]
pub mod css;
pub mod dl; // fnum：自写 f32 解析/超越函数（T1 体积车道AF2，ADR-0009 行为级欠账）。
            // 替代 std dec2flt 巨表（≈16KB）与 libm rem_pio2f f64 链（≈8KB），
            // 语义登记见模块注释与 CONFORMANCE「近似」。
pub mod fnum;
// font（TTF 解析）恒编译：build_display_list_at_with_font 的公开签名
// 引用 Font（Tier C 注入 API）。T1/T2 构建中无调用点 → gc-sections 剥离。
pub mod font;
pub mod geom;
pub mod gradient;
// hit_test（车道GE）挂 anim 门：命中检测与事件派发只服务交互动画
// （宿主事件注入），静态 MCU 构建（T1/T2）无消费点——门控保持体积
// 预算（ADR-0009 能力位降级语义）。
#[cfg(feature = "anim")]
pub mod hit_test;
// ael_trace（ADR-0027 后续）：跨行 active-edge list 离线盈利性验证的
// host-only 诊断。挂 host-perf 门：复用 raster 的边收集/行桶/谓词/
// 排序语义做 trace 与 R0/C1/C2 模拟，不进生产 fill_path——MCU 面与
// 无 host-perf 构建零编译（第五门禁证）。
#[cfg(feature = "host-perf")]
pub mod ael_trace;
#[cfg(feature = "image-decode")]
pub mod jpeg_decode;
// mipmap（车道BB）恒编译：ImgData 的 mips 槽在其公开字段上（dl.rs 契约
// 不门，ADR-0009）；无 image-decode 构建中无解码注入 → 无消费点 →
// gc-sections 剥离。
pub mod mipmap;
pub mod path;
#[cfg(feature = "image-decode")]
pub mod png_decode;
pub mod raster;
// retained（车道RL）恒编译：保留式帧缓存 + 脏带增量渲染。alloc-only
// 纯新增模块，无消费点构建（T1-T4）中 gc-sections 剥离，预算不变。
pub mod retained;
pub mod style;
#[cfg(feature = "text-lite")]
pub mod text;
pub mod tree;
pub mod xml;
