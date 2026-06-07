//! M28.1: 注入 `navigator` 全局对象（对齐 W3C/Chrome 基础子集）。
//!
//! 让 SPA 的 JS 直接用 Web 标准 API（**数据属性，无括号**，符合 W3C）：
//! ```js
//! navigator.userAgent      // "Mozilla/5.0 ... Chrome/..."
//! navigator.platform       // "MacIntel" / "Win32" / "Linux x86_64"
//! navigator.language       // "zh-CN"
//! navigator.languages      // ["zh-CN", "zh", "en"]
//! navigator.onLine         // true
//! navigator.cookieEnabled  // true
//! navigator.vendor         // "Google Inc."
//! ```
//!
//! 这些是 SPA 反爬/特性检测最常读的属性。缺失时 JS 报 ReferenceError。
//!
//! ## 设计决策：纯 JS 对象字面量（非 NativeFunction getter）
//!
//! navigator 与 location/history 不同：location 的值**依赖运行时状态**
//! （需调 `__locationHref()` 拿当前 URL），所以用方法模式。而 navigator
//! 的值**全是静态**的（UA/平台/语言不随 DOM 变化），用 JS 对象字面量
//! 一次构造即可——这是真正的**数据属性**，符合 W3C（`navigator.userAgent`
//! 无括号），且代码更简单、无 NativeFunction 跨 eval 的 this 绑定风险。
//!
//! 平台检测用 `cfg!(target_os)` 跨平台映射到 Chrome 的 platform 字符串，
//! 注入前 Rust 端拼接好。

use boa_engine::{Context, JsResult};

/// 当前进程的 Chrome UA 字符串（与 net::client.rs 一致，反爬必需）。
///
/// 注：UA 定义在 net crate 的 client.rs，这里复制一份避免 js-runtime 依赖 net
/// （net 依赖 reqwest，重）。M28.5 可考虑提取到共享常量。
const CHROME_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
    AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

/// 映射当前编译目标到 navigator.platform（Chrome 语义）。
fn platform_string() -> &'static str {
    if cfg!(target_os = "macos") {
        "MacIntel"
    } else if cfg!(target_os = "windows") {
        "Win32"
    } else {
        "Linux x86_64"
    }
}

/// 在 `ctx` 上注册 `navigator` 全局对象（数据属性）。
pub fn install_navigator(ctx: &mut Context) -> JsResult<()> {
    // 用 JS 对象字面量构造，保证属性是数据属性（无括号访问，符合 W3C）。
    // 所有值在 Rust 端拼接好，避免 JS 注入风险。
    let ua = CHROME_UA.replace('\\', "\\\\").replace('"', "\\\"");
    let platform = platform_string();
    let js = format!(
        r#"(function() {{
            globalThis.navigator = {{
                userAgent: "{ua}",
                platform: "{platform}",
                language: "zh-CN",
                languages: ["zh-CN", "zh", "en"],
                onLine: true,
                cookieEnabled: true,
                vendor: "Google Inc."
            }};
        }})();
        undefined;"#
    );
    ctx.eval(boa_engine::Source::from_bytes(&js))?;
    Ok(())
}

/// 返回当前 navigator UA 字符串（供其他 crate 获取，如需统一）。
#[must_use]
pub fn chrome_user_agent() -> &'static str {
    CHROME_UA
}

#[cfg(test)]
mod tests {
    use super::*;
    use boa_engine::JsValue;
    use crate::bridge::install;

    fn setup_ctx() -> Context {
        let mut ctx = Context::default();
        install(&mut ctx);
        install_navigator(&mut ctx).expect("install navigator");
        ctx
    }

    fn str_result(v: JsValue) -> String {
        v.as_string()
            .map(|s| s.to_std_string_escaped())
            .unwrap_or_default()
    }

    #[test]
    fn user_agent_is_chrome() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes("navigator.userAgent"))
            .unwrap();
        let ua = str_result(r);
        assert!(ua.contains("Chrome/"), "ua={ua}");
        assert!(ua.contains("Mozilla/5.0"), "ua={ua}");
    }

    #[test]
    fn platform_matches_target() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes("navigator.platform"))
            .unwrap();
        let p = str_result(r);
        if cfg!(target_os = "macos") {
            assert_eq!(p, "MacIntel");
        } else if cfg!(target_os = "windows") {
            assert_eq!(p, "Win32");
        } else {
            assert_eq!(p, "Linux x86_64");
        }
    }

    #[test]
    fn language_is_zh_cn() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes("navigator.language"))
            .unwrap();
        assert_eq!(str_result(r), "zh-CN");
    }

    #[test]
    fn languages_is_array_of_three() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes("navigator.languages.length"))
            .unwrap();
        assert_eq!(r.as_number().unwrap() as i64, 3);
    }

    #[test]
    fn online_and_cookie_enabled_are_true() {
        let mut ctx = setup_ctx();
        let online = ctx
            .eval(boa_engine::Source::from_bytes("navigator.onLine"))
            .unwrap();
        assert!(online.as_boolean().unwrap());
        let cookie = ctx
            .eval(boa_engine::Source::from_bytes("navigator.cookieEnabled"))
            .unwrap();
        assert!(cookie.as_boolean().unwrap());
    }

    #[test]
    fn vendor_is_google() {
        let mut ctx = setup_ctx();
        let r = ctx
            .eval(boa_engine::Source::from_bytes("navigator.vendor"))
            .unwrap();
        assert_eq!(str_result(r), "Google Inc.");
    }

    #[test]
    fn navigator_is_not_a_function() {
        // 关键：userAgent 是数据属性，不是方法。访问应返回字符串，
        // 不是函数对象（防 ObjectInitializer 方法模式的回归）。
        let mut ctx = setup_ctx();
        let ty = ctx
            .eval(boa_engine::Source::from_bytes("typeof navigator.userAgent"))
            .unwrap();
        assert_eq!(str_result(ty), "string");
    }
}
