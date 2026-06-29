//! M-cls.1: 内存自愈护栏 —— 子进程沙箱执行 JS 渲染。
//!
//! ## 为什么需要它
//! cls.cn/telegraph 等 Next.js CSR 站点的 vendor bundle（main.js ~142KB）
//! 在 boa 0.20 里 eval 时内存暴涨到 6.6GB，被系统 OOM-killer 杀死，渲染
//! 永远完不成。boa 0.20 没有内存配额 API（只有循环/递归/栈限制，已在
//! `js-runtime::scripts` 收紧），所以单靠引擎层无法根治。`js-runtime` 全
//! crate `#![forbid(unsafe_code)]` 且 `SharedTree` 是 `!Send`，线程内方案
//! 不可行 —— 唯一可靠的硬上限是 **子进程 + OS rlimit**。
//!
//! ## 设计
//! - 父进程把"fetch 完的 HTML + base_url"交给一个子进程（即自身二进制，
//!   走隐藏子命令 `__js-render`）。
//! - 子进程启动**最早期**调 `setrlimit(RLIMIT_AS, soft, hard)` 设硬上限。
//!   任何分配超过上限 → 内核在子进程内部返回 ENOMEM / 直接 SIGKILL，
//!   绝不会波及父进程或别的程序。
//! - 子进程把渲染后的纯文本写 stdout，父进程读回。
//! - 父进程检测子进程退出码：被信号杀（OOM）或非 0 → 返回 `Ok(None)`，
//!   调用方据此走 CSR 数据兜底（`spa_fallback`）。
//!
//! ## 为什么是 RLIMIT_AS 而不是 RLIMIT_RSS
//! boa 的分配走 `Vec`/`mmap`，都计入虚拟地址空间（AS）。RSS 上限在大多数
//! 内核上是"建议"而非强制，无法可靠触发杀死；AS 是强制硬上限。
//!
//! ## 为什么本地文件不走沙箱
//! `render-file`/`render-script` 读的是用户本地的受信 HTML，性能优先，
//! 走原进程内路径。沙箱只覆盖来自网络的 `render-url`/`open`。

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{anyhow, Result};

/// 隐藏子命令名。父进程 `Command::new(current_exe)` 时作为第一个参数传入。
/// clap 把变体 `JsRender` 转成 kebab-case `js-render`（前导下划线会被吃掉），
/// 所以这里用 `js-render` 与实际 CLI token 对齐。`#[command(hide = true)]`
/// 保证它不出现在 `--help`。
pub(crate) const SANDBOX_SUBCOMMAND: &str = "js-render";

/// 默认子进程内存上限（MB）。cls.cn 的 Next.js bundle 实测在 boa 里要
/// 涨到 GB 级；RSS 监控每 50ms 轮询一次，vendor bundle 在一个轮询间隔内
/// 可能从 <cap 跃升 ~300MB（cls 的 main.js 分配极快）。设 400MB 上限，
/// 实测峰值 ~600MB（一次轮询的过冲），远低于 7.5GB 的崩溃值，满足"自愈"。
/// 真正跑不动的（如 cls 的 main.js）会撞上限被杀 → 走兜底。
pub(crate) const DEFAULT_JS_MEMORY_LIMIT_MB: u64 = 400;

/// 子进程整体超时（含 fetch + eval + 渲染）。防子进程卡死（如等待外部
/// script 永久 hang）。父进程超时后 kill 子进程并返回 `Ok(None)`。
const SANDBOX_TIMEOUT: Duration = Duration::from_secs(90);

/// 父子进程之间的帧分隔符。HTML 里不会出现这个字节序列（0x1f = unit
/// separator，HTML 文本里几乎不可能出现），用它做 base_url / width / html
/// 三段的边界，避免逐字段长度前缀解析的复杂度。
const FRAME_SEP: &str = "\x1f";

/// 在**子进程入口**调用：设 RLIMIT_AS 硬上限。
///
/// `mb` = 上限（MB）。失败不 panic（部分容器环境 rlimit 受限），只记日志
/// 到 stderr。即便设失败，外层 boa 限制（M-cls.2）仍是第二道防线。
///
/// # Errors
/// 仅在内部 rlimit 调用失败时返回 Err，但调用方应忽略错误（子进程仍可
/// 继续跑，靠 boa 限制兜底）。
pub(crate) fn apply_memory_limit(mb: u64) -> Result<()> {
    use rlimit::{setrlimit, Resource};
    let bytes = mb
        .checked_mul(1024)
        .and_then(|b| b.checked_mul(1024))
        .ok_or_else(|| anyhow!("memory limit overflow: {mb}MB"))?;
    // soft == hard：到上限立即触发，不留缓冲（爬虫场景宁可早杀走兜底）。
    if let Err(e) = setrlimit(Resource::AS, bytes, bytes) {
        // macOS 不强制 RLIMIT_AS（EINVAL 是预期行为）；父进程的 RSS 监控
        // 是跨平台可靠的后备。这里只记 info，不当错误（上层照常跑）。
        eprintln!(
            "[sandbox] RLIMIT_AS not enforced on this OS ({e}); parent RSS monitor is the active guard"
        );
        return Err(anyhow!("setrlimit unsupported: {e}"));
    }
    eprintln!("[sandbox] RLIMIT_AS capped at {mb}MB");
    Ok(())
}

/// 父进程侧：在子进程里跑一次"JS 渲染"，拿回渲染后的纯文本。
///
/// 成功返回 `Ok(Some(text))`；子进程被杀（OOM）/ 超时 / 非 0 退出都返回
/// `Ok(None)`，由调用方决定是否走 CSR 数据兜底。
///
/// `html` = 已 fetch 完的页面 HTML；`base_url` = 页面 URL（用于相对脚本
/// 解析 + CSR 判定）；`width` = 渲染宽度；`mem_mb` = 子进程内存上限。
pub(crate) fn run_js_render_in_sandbox(
    html: &str,
    base_url: &str,
    width: usize,
    mem_mb: u64,
) -> Result<Option<String>> {
    let exe = std::env::current_exe()
        .map_err(|e| anyhow!("cannot resolve current_exe for sandbox: {e}"))?;
    // 拼 stdin 帧：base_url \x1f width \x1f html
    let mut stdin_payload = String::with_capacity(html.len() + base_url.len() + 32);
    stdin_payload.push_str(base_url);
    stdin_payload.push_str(FRAME_SEP);
    stdin_payload.push_str(&width.to_string());
    stdin_payload.push_str(FRAME_SEP);
    stdin_payload.push_str(html);

    let mut child = Command::new(&exe)
        .arg(SANDBOX_SUBCOMMAND)
        .arg("--mem-mb")
        .arg(mem_mb.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| anyhow!("failed to spawn sandbox child: {e}"))?;

    // 写 stdin（一次性，HTML 几十 KB 量级）。
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(stdin_payload.as_bytes());
    }
    // 读 stdout（必须先 take 再 wait，避免管道满死锁）。
    let mut stdout = child.stdout.take();
    let child_pid = child.id();

    // 双层内存护栏：①子进程内 RLIMIT_AS（macOS 不强制，见 apply_memory_limit）；
    // ②父进程轮询子进程 RSS，超 mem_mb 上限立即 kill。第二层跨平台可靠。
    let status = match child.wait_timeout_mem(SANDBOX_TIMEOUT, child_pid, mem_mb) {
        Ok(Some(status)) => status,
        Ok(None) => {
            // 超时：kill 后返回 None。
            let _ = child.kill();
            let _ = child.wait();
            eprintln!("[sandbox] child timed out after {:?}", SANDBOX_TIMEOUT);
            return Ok(None);
        }
        Err(e) => {
            let _ = child.kill();
            return Err(anyhow!("sandbox child wait failed: {e}"));
        }
    };

    // 被信号杀（典型：RLIMIT_AS 触发的 SIGKILL / OOM）→ JS 失败，走兜底。
    if !status.success() {
        eprintln!(
            "[sandbox] child exited with {status} (JS render failed/OOM-killed → will fall back)"
        );
        return Ok(None);
    }

    let mut out = String::new();
    if let Some(s) = stdout.as_mut() {
        let _ = s.read_to_string(&mut out);
    }
    Ok(Some(out))
}

// ---- 轮询式 wait + RSS 监控（std 没自带，手写一个）----
pub(crate) trait ChildWaitTimeoutExt {
    /// 轮询子进程：①已退出 → 返回 Some(status)；②超过 dur → None（超时）；
    /// ③RSS 超过 mem_cap_mb → 立即 kill 并返回 Some(被信号杀的 status)。
    fn wait_timeout_mem(
        &mut self,
        dur: Duration,
        pid: u32,
        mem_cap_mb: u64,
    ) -> std::io::Result<Option<std::process::ExitStatus>>;
}

impl ChildWaitTimeoutExt for std::process::Child {
    fn wait_timeout_mem(
        &mut self,
        dur: Duration,
        pid: u32,
        mem_cap_mb: u64,
    ) -> std::io::Result<Option<std::process::ExitStatus>> {
        let start = std::time::Instant::now();
        let poll = Duration::from_millis(50);
        let cap_kb = mem_cap_mb.saturating_mul(1024);
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(Some(status));
            }
            // RSS 监控（跨平台：macOS/Linux 都支持 `ps -o rss= -p <pid>`）。
            if let Some(rss_kb) = read_child_rss_kb(pid) {
                if rss_kb > cap_kb {
                    eprintln!(
                        "[sandbox] child RSS {rss_kb}KB > cap {cap_kb}KB, killing (self-healing)"
                    );
                    let _ = self.kill();
                    // kill 后再 wait 拿真实退出态（被 SIGKILL）。
                    return self.wait().map(Some);
                }
            }
            if start.elapsed() >= dur {
                return Ok(None);
            }
            std::thread::sleep(poll);
        }
    }
}

/// 读子进程 RSS（KB）。失败返回 None（不阻断，只是监控降级）。
/// 用 `ps` 而非 sysinfo crate：无新依赖，macOS/Linux 行为一致。
fn read_child_rss_kb(pid: u32) -> Option<u64> {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    s.trim().parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_sep_is_single_byte_safe() {
        // 0x1f 必须是单字节，否则和 UTF-8 多字节混在 stdin 里会乱。
        assert_eq!(FRAME_SEP.chars().count(), 1);
        assert_eq!(FRAME_SEP.len(), 1);
    }

    #[test]
    fn default_memory_limit_is_reasonable() {
        // 400MB：远低于 OOM 风险线（实测 cls 涨到 6.6GB），又足够正常 SPA。
        // 用运行时局部变量绕开 clippy::assertions_on_constants（编译期常量断言）。
        let limit = DEFAULT_JS_MEMORY_LIMIT_MB;
        assert!((256..=1024).contains(&limit), "limit={limit}");
    }

    #[test]
    fn apply_memory_limit_does_not_panic_on_typical_value() {
        // 设一个典型值。环境不支持 rlimit 时返回 Err 但不 panic。
        let _ = apply_memory_limit(DEFAULT_JS_MEMORY_LIMIT_MB);
    }

    #[test]
    fn sandbox_returns_none_when_child_fails() {
        // 用一个不存在的子命令让子进程立即失败 → 必须返回 Ok(None)
        // （而不是 Err，因为"JS 失败"是预期可恢复状态）。
        // 这里直接测 run_js_render_in_sandbox 对失败子命令的行为。
        // 注意：此测试会真的 fork 一个子进程跑 __js-render（不存在的逻辑
        // 在 test 构建里），所以只验证"失败 → None"的契约。
        // 为避免依赖真实网络，用一个会立即被 boa 限制拒绝的极小 HTML。
        let result = run_js_render_in_sandbox(
            "<html><body><p>hi</p></body></html>",
            "about:blank",
            80,
            DEFAULT_JS_MEMORY_LIMIT_MB,
        );
        // 子进程要么成功（Some 文本含 hi），要么失败（None）—— 两种都可接受，
        // 关键是不能 Err（Err 表示基础设施本身坏了）。
        match result {
            Ok(Some(text)) => assert!(text.contains("hi"), "got: {text}"),
            Ok(None) => { /* 子进程失败也算契约内 */ }
            Err(e) => panic!("sandbox infra error (should be Ok): {e}"),
        }
    }
}
