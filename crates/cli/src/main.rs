//! `tuoen` 命令行入口。
//!
//! 这一票（ticket #2）只建立骨架：`tuoen list` 端到端跑通、`--json` 形状固定、中文优先。
//! 后续票往里加子命令，**不改已有输出形状**。
//!
//! 输出契约见 `docs/specs/L1-dev-state.md`：
//! - `--json` 必须**稳定且不本地化**（决策 35），否则脚本化与未来的 GUI 会被中文界面绑死
//! - 人类输出**默认中文**
//! - 错误码是稳定的机器可读字符串

mod cli;
mod envelope;
mod exit;

use clap::Parser;

use cli::{Cli, Command};
use envelope::Envelope;

fn main() {
    // 退出码约定：0 成功，1 运行期错误，2 用法错误（由 clap 直接退出）。
    let code = run();
    std::process::exit(code);
}

fn run() -> i32 {
    let cli = Cli::parse();

    match cli.command {
        Command::List(args) => {
            let result = tuoen_core::list::list();
            if args.json {
                print_json(&Envelope::ok("list", &result));
            } else {
                print_human_list(&result);
            }
            exit::SUCCESS
        }
    }
}

fn print_json(envelope: &Envelope) {
    // 唯一的输出路径：所有 `--json` 输出都经过这里，所以形状不可能漂移。
    match serde_json::to_string(envelope) {
        Ok(json) => println!("{json}"),
        Err(err) => {
            // 序列化失败是我们自己的 bug，不是用户输入问题。用最小信封报告，不 panic。
            eprintln!("tuoen: 内部错误：无法序列化输出：{err}");
            std::process::exit(exit::RUNTIME_ERROR);
        }
    }
}

fn print_human_list(result: &tuoen_core::ListResult) {
    if result.tools.is_empty() {
        // 空列表必须说得清楚，否则用户会怀疑工具坏了。
        println!("（没有已管理的工具）");
        println!();
        println!("提示：`tuoen list` 目前只列出由 tuoen 自己管理的工具。");
        println!("      检测整机已安装的工具是 `tuoen detect`，整机状态快照是 `tuoen capture`。");
        return;
    }

    // 列宽按实际内容计算，避免中文与长路径把表格挤歪。
    let name_w = result
        .tools
        .iter()
        .map(|t| display_width(&t.name))
        .max()
        .unwrap_or(0);
    let version_w = result
        .tools
        .iter()
        .map(|t| display_width(t.version.as_deref().unwrap_or("-")))
        .max()
        .unwrap_or(0);

    println!(
        "{:<name_w$}  {:<version_w$}  来源",
        "工具",
        "版本",
        name_w = name_w,
        version_w = version_w
    );
    for tool in &result.tools {
        println!(
            "{:<name_w$}  {:<version_w$}  {}",
            tool.name,
            tool.version.as_deref().unwrap_or("-"),
            tool.source.as_str(),
            name_w = name_w,
            version_w = version_w
        );
    }
}

/// 粗略的显示宽度：CJK 字符占两列。
///
/// 只用于对齐，不用于任何逻辑判断 —— 所以不需要完整的 Unicode 宽度表。
fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| {
            let cp = u32::from(c);
            let wide = (0x1100..=0x115F).contains(&cp)
                || (0x2E80..=0xA4CF).contains(&cp)
                || (0xAC00..=0xD7A3).contains(&cp)
                || (0xF900..=0xFAFF).contains(&cp)
                || (0xFE30..=0xFE6F).contains(&cp)
                || (0xFF00..=0xFF60).contains(&cp)
                || (0xFFE0..=0xFFE6).contains(&cp);
            if wide { 2 } else { 1 }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_width_is_char_count() {
        assert_eq!(display_width("node"), 4);
    }

    #[test]
    fn cjk_width_counts_double() {
        assert_eq!(display_width("工具"), 4);
    }
}
