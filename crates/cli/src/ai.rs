//! M70.8: AI 摘要/问答功能（通过 pi CLI 调用）。
//!
//! 对标 Firecrawl `formats: ["summary", "question"]`。
//! Firecrawl 用内置 AI，我们通过外部 `pi` 命令行工具实现。
//!
//! 配置优先级：CLI 参数 > 环境变量 > 默认值
//!
//! ```bash
//! # 环境变量
//! export AI_PROVIDER=opencode-go
//! export AI_MODEL=deepseek-v4-flash
//! export AI_COMMAND=pi  # 自定义命令路径
//! ```

use std::io::Write;
use std::process::{Command, Stdio};

const DEFAULT_PROVIDER: &str = "opencode-go";
const DEFAULT_MODEL: &str = "deepseek-v4-flash";

/// 获取 AI 命令配置。
fn ai_command() -> String {
    std::env::var("AI_COMMAND").unwrap_or_else(|_| "pi".to_string())
}

fn ai_provider(cli_provider: Option<&str>) -> String {
    cli_provider
        .map(|s| s.to_string())
        .or_else(|| std::env::var("AI_PROVIDER").ok())
        .unwrap_or_else(|| DEFAULT_PROVIDER.to_string())
}

fn ai_model(cli_model: Option<&str>) -> String {
    cli_model
        .map(|s| s.to_string())
        .or_else(|| std::env::var("AI_MODEL").ok())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

/// 调用 pi CLI 生成摘要。
///
/// 通过 stdin 传输大段文本（避免 shell 参数长度限制）。
/// 遵循 sandbox.rs 的模式 A（子进程 + stdin 管道 + stdout 读取）。
pub fn summarize(
    content: &str,
    cli_provider: Option<&str>,
    cli_model: Option<&str>,
) -> Result<String, String> {
    let cmd = ai_command();
    let provider = ai_provider(cli_provider);
    let model = ai_model(cli_model);
    let prompt = format!(
        "You are a web page summarizer. Summarize the following page content in 3-5 sentences, \
         covering the main topic and key points. Return ONLY the summary, no preamble.\n\n{}",
        truncate(content, 8000)
    );

    call_pi(&cmd, &prompt, &provider, &model)
}

/// 调用 pi CLI 回答问题。
pub fn ask_question(
    content: &str,
    question: &str,
    cli_provider: Option<&str>,
    cli_model: Option<&str>,
) -> Result<String, String> {
    let cmd = ai_command();
    let provider = ai_provider(cli_provider);
    let model = ai_model(cli_model);
    let prompt = format!(
        "Context from a web page:\n\n{}\n\n---\n\nBased on the context above, answer: {}\n\nReturn ONLY the answer, no preamble.",
        truncate(content, 8000),
        question
    );

    call_pi(&cmd, &prompt, &provider, &model)
}

/// 核心调用函数：spawn pi 子进程，写 stdin，读 stdout。
fn call_pi(cmd: &str, prompt: &str, provider: &str, model: &str) -> Result<String, String> {
    let mut child = Command::new(cmd)
        .args(["-p", prompt, "--provider", provider, "--model", model])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to spawn {cmd}: {e}"))?;

    // 写入 prompt 到 stdin
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(prompt.as_bytes());
    }

    let output = child
        .wait_with_output()
        .map_err(|e| format!("wait {cmd} failed: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "{cmd} exited with {}: {}",
            output.status,
            stderr.trim()
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(stdout)
}

/// 截断超长文本（token 限制保守 8000 字符）。
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}... [truncated {} bytes]", &s[..max], s.len() - max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_text_stays() {
        assert_eq!(truncate("hello", 100), "hello");
    }

    #[test]
    fn truncate_long_text_cuts() {
        let long = "a".repeat(100);
        let t = truncate(&long, 10);
        assert!(t.len() < 100);
        assert!(t.contains("truncated"));
    }
}
