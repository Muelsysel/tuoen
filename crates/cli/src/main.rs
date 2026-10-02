//! `tuoen` 命令行入口。
//!
//! 子命令按票据逐张加入，**已有输出形状不改**。
//!
//! 输出契约见 `docs/specs/L1-dev-state.md`：
//! - `--json` 必须**稳定且不本地化**（决策 35），否则脚本化与未来的 GUI 会被中文界面绑死
//! - 人类输出**默认中文**
//! - 错误码是稳定的机器可读字符串
//!
//! **`--json` 的载荷全部来自 `view` 模块，不直接序列化磁盘模型。**
//! 磁盘模型（`tuoen_manifest::Catalog`）的字段名是 TOML 的形状，把它当接口会让
//! TOML 字段名变成事实上的公开 API。

mod catalog;
mod cli;
mod detect;
mod envelope;
mod exit;
mod view;

use clap::Parser;
use tuoen_manifest::{GateVerdict, Redistribution, ResolvedRecipe};

use catalog::{CatalogCommand, CheckArgs, JsonFlag, ShowArgs};
use cli::{Cli, Command};
use envelope::Envelope;
use view::{CatalogListView, CheckView, DetectView, ToolDetailView};

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
        Command::Catalog(command) => match command {
            CatalogCommand::List(JsonFlag { json }) => run_catalog_list(json),
            CatalogCommand::Show(args) => run_catalog_show(&args),
            CatalogCommand::Check(args) => run_catalog_check(&args),
        },
        Command::Detect(args) => run_detect(&args),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// `tuoen detect`
// ─────────────────────────────────────────────────────────────────────────────

/// 只读地检测这台机器。
///
/// **六个依赖全是真实实现，没有一个是写死的** —— 这也是为什么固定装置
/// （`tuoen_core::fixture`）必须是公开 API：测试能注入假实现，真机跑真实现。
fn run_detect(args: &detect::DetectArgs) -> i32 {
    let fs = tuoen_platform::RealFileSystem;
    let registry = tuoen_platform::RealRegistry;
    // 环境变量里 `target_exists` 要真的去看磁盘，所以这里把 fs 也传进去。
    let env = tuoen_platform::RealEnvBlock::new(registry, fs);
    let process_env = tuoen_platform::RealProcessEnv::new();
    let runner = tuoen_platform::SystemProcessRunner;
    let managed = tuoen_platform::RealManagedStore::default();
    let scan_roots = tuoen_core::detect::engine::default_scan_roots(&process_env);

    let ctx = tuoen_core::detect::DetectContext {
        fs: &fs,
        registry: &registry,
        env: &env,
        process_env: &process_env,
        runner: &runner,
        managed: &managed,
        probe_timeout: tuoen_platform::DEFAULT_PROBE_TIMEOUT,
        probe_versions: !args.no_version,
        scan_roots,
    };

    let summary = tuoen_core::detect::detect_all(&ctx);

    if args.json {
        print_json(&Envelope::ok("detect", &DetectView::new(&summary)));
    } else {
        print_human_detect(&summary, args.no_version);
    }
    exit::SUCCESS
}

fn print_human_detect(summary: &tuoen_core::detect::DetectionSummary, no_version: bool) {
    if summary.tools.is_empty() {
        println!("（没有检测到任何开发工具）");
        println!();
        println!("这不太可能是真的 —— 请把这个输出报告给我们。");
        return;
    }

    // 列宽按实际内容算，中文与长路径都不会把表挤歪。
    let name_w = summary
        .tools
        .iter()
        .map(|t| display_width(&t.name))
        .max()
        .unwrap_or(4)
        .max(4);
    let version_w = summary
        .tools
        .iter()
        .map(|t| display_width(t.version.as_deref().unwrap_or("?")))
        .max()
        .unwrap_or(1)
        .max(7);

    println!(
        "{:<name_w$}  {:<version_w$}  {:<17}  路径",
        "工具",
        "版本",
        "置信度",
        name_w = name_w,
        version_w = version_w
    );
    for tool in &summary.tools {
        let mark = if view::is_reproducible(tool.confidence) {
            " "
        } else {
            "✗"
        };
        println!(
            "{:<name_w$}  {:<version_w$}  {:<17}  {mark} {}",
            tool.name,
            tool.version.as_deref().unwrap_or("?"),
            tool.confidence.as_str(),
            tool.path,
            name_w = name_w,
            version_w = version_w
        );
    }

    println!();
    println!("共 {} 条记录。", summary.tools.len());

    // 按来源与置信度各给一行计数 —— 这是"检测有多可信"的概览。
    let by_source = summary
        .count_by_source()
        .into_iter()
        .filter(|(_, count)| *count > 0)
        .map(|(source, count)| format!("{} {}", source.as_str(), count))
        .collect::<Vec<_>>()
        .join(" · ");
    println!("来源：{by_source}");

    let by_confidence = summary
        .count_by_confidence()
        .into_iter()
        .filter(|(_, count)| *count > 0)
        .map(|(level, count)| format!("{} {}", level.as_str(), count))
        .collect::<Vec<_>>()
        .join(" · ");
    println!("置信度：{by_confidence}");

    // 值得单独点名的两类：它们最容易让人误判。
    let ghosts = summary
        .tools
        .iter()
        .filter(|t| t.confidence == tuoen_core::detect::Confidence::RegisteredMissing)
        .count();
    let aliases = summary
        .tools
        .iter()
        .filter(|t| t.confidence == tuoen_core::detect::Confidence::AliasGhost)
        .count();
    if ghosts > 0 || aliases > 0 {
        println!();
        println!("注意：");
        if ghosts > 0 {
            println!("  · {ghosts} 条是**幽灵条目** —— 注册表声称已安装，但文件不存在。");
            println!("    tuoen 只报告它们，不会去清理（那属于你的决定）。");
        }
        if aliases > 0 {
            println!("  · {aliases} 条是**App Execution Alias** —— 0 字节，`Test-Path` 通过，");
            println!("    但它不是文件。`Get-Command` 会成功，实际执行会打开应用商店。");
        }
    }

    let not_reproducible = summary
        .tools
        .iter()
        .filter(|t| !view::is_reproducible(t.confidence))
        .count();
    if not_reproducible > 0 {
        println!();
        println!("标 ✗ 的 {not_reproducible} 条**不能由 tuoen 自动重建**（它们是别人的安装、");
        println!("只剩目录、或根本不存在）。其余条目是 `capture` 的捕获对象。");
    }

    if no_version {
        println!();
        println!("（本次没有探测版本 —— 加了 `--no-version`）");
    }

    println!();
    println!("用 `tuoen detect --json` 拿机器可读的输出。");
}

// ─────────────────────────────────────────────────────────────────────────────
// `tuoen catalog`
// ─────────────────────────────────────────────────────────────────────────────

/// 加载种子 catalog。失败时把**全部**校验问题打印出来并返回运行期错误。
///
/// 种子目录是我们自己写的，但仍按外部输入处理：一个 panic 会让整个 CLI 不可用。
fn seed_or_report(command: &'static str) -> Result<tuoen_manifest::Catalog, i32> {
    match tuoen_manifest::load_seed() {
        Ok(catalog) => Ok(catalog),
        Err(err) => {
            eprintln!("tuoen: 内置种子目录无效 —— 这是一个 bug，请报告：");
            eprintln!("{err}");
            if command == "catalog" {
                print_json(&Envelope::err(
                    "catalog",
                    "seed-catalog-invalid",
                    err.to_string(),
                ));
            }
            Err(exit::RUNTIME_ERROR)
        }
    }
}

fn run_catalog_list(json: bool) -> i32 {
    let Ok(catalog) = seed_or_report("catalog") else {
        return exit::RUNTIME_ERROR;
    };

    if json {
        print_json(&Envelope::ok(
            "catalog.list",
            &CatalogListView::from(&catalog),
        ));
        return exit::SUCCESS;
    }

    let id_w = catalog.tools.iter().map(|t| t.id.len()).max().unwrap_or(4);
    let name_w = catalog
        .tools
        .iter()
        .map(|t| display_width(&t.display_name))
        .max()
        .unwrap_or(4);

    println!(
        "{:<id_w$}  {:<name_w$}  许可证结论",
        "id",
        "名称",
        id_w = id_w,
        name_w = name_w
    );
    for tool in &catalog.tools {
        println!(
            "{:<id_w$}  {:<name_w$}  {}",
            tool.id,
            tool.display_name,
            tool.licence.redistribution.as_str(),
            id_w = id_w,
            name_w = name_w
        );
    }
    println!();
    println!("共 {} 个工具。", catalog.tools.len());
    println!(
        "用 `tuoen catalog show <id>` 看详情，`tuoen catalog check <id> <版本>` 判断能不能装。"
    );

    let prohibited: Vec<_> = catalog
        .tools
        .iter()
        .filter(|t| t.licence.redistribution == Redistribution::Prohibited)
        .collect();
    if !prohibited.is_empty() {
        println!();
        println!(
            "注意：其中 {} 个不可再分发，tuoen 不会下载或安装它们（原因见 `catalog show`）。",
            prohibited.len()
        );
    }

    exit::SUCCESS
}

fn run_catalog_show(args: &ShowArgs) -> i32 {
    let Ok(catalog) = seed_or_report("catalog") else {
        return exit::RUNTIME_ERROR;
    };

    let Some(tool) = tuoen_manifest::find_tool(&catalog, &args.tool) else {
        return report_unknown_tool(&args.tool, args.json, &catalog);
    };

    let versions = tuoen_manifest::installable_versions(&catalog, &tool.id, "windows-x64");

    if args.json {
        print_json(&Envelope::ok(
            "catalog.show",
            &ToolDetailView::new(&catalog, tool, "windows-x64"),
        ));
        return exit::SUCCESS;
    }

    println!("{}  ({})", tool.display_name, tool.id);
    if !tool.aliases.is_empty() {
        println!("别名：{}", tool.aliases.join(", "));
    }
    println!("{}", tool.description);
    if let Some(home) = &tool.homepage {
        println!("上游：{home}");
    }
    println!();
    println!(
        "许可证：{} —— {}",
        tool.licence.spdx_or_name,
        tool.licence.redistribution.as_str()
    );
    if !tool.licence.notes.is_empty() {
        println!("  {}", tool.licence.notes);
    }
    println!();

    if tool.recipes.is_empty() {
        println!("没有可安装的版本。");
        if tool.licence.redistribution == Redistribution::Prohibited {
            println!("原因：不可再分发。请从上游自行获取。");
        }
    } else {
        println!("版本（windows-x64）：");
        for (version, redistribution) in &versions {
            let mark = if redistribution.installable() {
                " "
            } else {
                "✗"
            };
            println!("  {mark} {version}  [{}]", redistribution.as_str());
        }
    }

    exit::SUCCESS
}

fn run_catalog_check(args: &CheckArgs) -> i32 {
    let Ok(catalog) = seed_or_report("catalog") else {
        return exit::RUNTIME_ERROR;
    };

    // 先查工具是否存在 —— 否则"工具不存在"会被误报成"没有适用的 recipe"。
    let Some(tool) = tuoen_manifest::find_tool(&catalog, &args.tool) else {
        return report_unknown_tool(&args.tool, args.json, &catalog);
    };

    // 再看许可证门禁。**必须在解析 recipe 之前看**：不可再分发的条目
    // 本来就（正确地）没有 recipe，如果先解析就会报"没有适用的 recipe" ——
    // 那是把"我们不许可你装"说成了"我们不知道去哪装"，是完全错误的回答。
    if tool.licence.redistribution == Redistribution::Prohibited {
        let payload = CheckView::from_prohibited_tool(tool, &args.version, &args.platform);
        if args.json {
            print_json(&Envelope::ok("catalog.check", &payload));
        } else {
            println!("{} {}", tool.display_name, args.version);
            println!("平台：{}", args.platform);
            println!(
                "许可证：{} —— {}",
                tool.licence.spdx_or_name,
                tool.licence.redistribution.as_str()
            );
            println!();
            println!("不能安装。");
            println!("原因：{}", payload.reason);
        }
        // 门禁拒绝是**成功的回答**（我们确实回答了"能不能装"），所以退出码是 0。
        // 这与"工具不存在 / 版本不存在"（退出码 1）是不同的结果。
        return exit::SUCCESS;
    }

    match tuoen_manifest::resolve(&catalog, &args.tool, &args.version, &args.platform) {
        Ok(recipe) => {
            let verdict = tuoen_manifest::gate(&recipe);
            if args.json {
                print_json(&Envelope::ok(
                    "catalog.check",
                    &CheckView::from_resolved(&recipe, &verdict),
                ));
            } else {
                print_check_human(&recipe, &verdict);
            }
            exit::SUCCESS
        }
        Err(err) => {
            // 解析失败（平台不匹配 / 版本不存在）是**正常回答**，不是崩溃 ——
            // 但它与"许可证拒绝"是两件不同的事，所以退出码不同。
            if args.json {
                print_json(&Envelope::err(
                    "catalog.check",
                    "unresolved",
                    err.to_string(),
                ));
            } else {
                eprintln!("tuoen: {err}");
            }
            exit::RUNTIME_ERROR
        }
    }
}

fn print_check_human(recipe: &ResolvedRecipe, verdict: &GateVerdict) {
    println!("{} {}", recipe.display_name, recipe.version);
    println!("平台：{}", recipe.platform);
    println!(
        "许可证：{} —— {}",
        recipe.licence_name,
        recipe.redistribution.as_str()
    );
    println!();
    match verdict {
        GateVerdict::Allowed { notes } => {
            println!("可以安装。");
            if !notes.is_empty() {
                println!("注意：{notes}");
            }
            println!();
            println!("下载地址：{}", recipe.url);
            println!("sha256：{}", recipe.checksum.value);
            println!("归档：{}", recipe.archive.as_str());
        }
        GateVerdict::Rejected { reason } => {
            println!("不能安装。");
            println!("原因：{reason}");
            // 不给下载地址 —— 拒绝安装却顺手给出地址是自相矛盾的。
        }
    }
}

fn report_unknown_tool(tool: &str, json: bool, catalog: &tuoen_manifest::Catalog) -> i32 {
    let known = catalog
        .tools
        .iter()
        .map(|t| t.id.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let message = format!("catalog 里没有工具 `{tool}`；已知：{known}");
    if json {
        print_json(&Envelope::err("catalog.show", "unknown-tool", message));
    } else {
        eprintln!("tuoen: {message}");
    }
    exit::RUNTIME_ERROR
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
