//! `tuoen globals list` 的两种输出：给人看的表 + 给脚本的 JSON。
//!
//! # `--json` 的形状是**冻结**的
//!
//! ```jsonc
//! { "schemaVersion": 2, "command": "globals.list", "ok": true, "data": {
//!     "roots":    [ { "tool": "npm", "root": "C:\\…\\globals\\npm\\v24.19.0", "source": "tuoen" } ],
//!     "packages": [ { "tool": "npm", "name": "pnpm", "version": "11.21.0", "source": "machine",
//!                     "binNames": ["pn", "pnpm", "pnpx", "pnx"] } ] } }
//! ```
//!
//! 三条形状纪律：
//!
//! 1. **camelCase**（`binNames`），键与取值**不本地化**（决策 35）；
//! 2. `binNames` 拿不到时**整键消失**（`skip_serializing_if`）—— `[]` 会被读成
//!    "我查过了，它一个命令都没有"，而那是另一句话；
//! 3. `packages` 里**不带** `toolVersion`：它是"哪一份运行时"的答案，在人的表里
//!    有用（一行一个包，读者要知道这一行属于哪个版本），而在脚本那一侧它属于
//!    **行**而不属于**包** —— 塞进每个包里会让同一份数据出现 N 次。
//!
//! # 人看的表
//!
//! 六列：工具 · 运行时版本 · 来源 · 包名 · 版本 · bin 名。来源那一列是这一票的
//! 全部意义所在：**同一行里能看出这个包是谁管的**，否则两个来源的包会混成一张
//! 看不出两套安装的表。

use serde::Serialize;
use tuoen_core::{GlobalsListing, GlobalsSource};

/// `--json` 的 `data`。
#[derive(Debug, Serialize)]
pub struct GlobalsListView {
    /// 我们管着的两个根（**空根也在**）。
    pub roots: Vec<GlobalsRootView>,
    /// 两个来源的包。
    pub packages: Vec<GlobalsPackageView>,
}

/// 一个根。
///
/// **没有 `exists`**：它是给人看的那句话的下脚料，而脚本那一侧要的是
/// "根在哪、里面有什么" —— 空根在 `packages` 里自然表现为零个包。
/// 加一个 `exists` 会让"根不存在"与"根存在但空"在 JSON 里成为两个不同的形状，
/// 而冻结的形状里没有它（形状一旦发出去就改不动）。
#[derive(Debug, Serialize)]
pub struct GlobalsRootView {
    /// 哪个工具（`npm` / `pip`）。
    pub tool: String,
    /// 根在哪（绝对路径）。
    pub root: String,
    /// 这个根是谁的：**永远**是 `tuoen`。
    pub source: String,
}

/// 一个包。
#[derive(Debug, Serialize)]
pub struct GlobalsPackageView {
    /// 哪个工具。
    pub tool: String,
    /// 包名（含 scope）。
    pub name: String,
    /// 版本；工具没给版本时是 `"unknown"`（**键永远在**）。
    pub version: String,
    /// 这个包从哪来（`machine` / `tuoen`）。
    pub source: String,
    /// 它提供了哪些命令。**拿不到就整键消失**（不是 `[]`）。
    #[serde(rename = "binNames", skip_serializing_if = "Vec::is_empty")]
    pub bin_names: Vec<String>,
}

impl From<&GlobalsListing> for GlobalsListView {
    fn from(listing: &GlobalsListing) -> Self {
        Self {
            roots: listing
                .roots
                .iter()
                .map(|row| GlobalsRootView {
                    tool: row.tool.slug().to_owned(),
                    root: row.root.to_string_lossy().into_owned(),
                    source: row.source.slug().to_owned(),
                })
                .collect(),
            packages: listing
                .packages
                .iter()
                .map(|row| GlobalsPackageView {
                    tool: row.tool.slug().to_owned(),
                    name: row.name.clone(),
                    version: row.version.clone(),
                    source: row.source.slug().to_owned(),
                    bin_names: row.bin_names.clone(),
                })
                .collect(),
        }
    }
}

/// 人看的表。
pub fn print_human(listing: &GlobalsListing) {
    print_roots(listing);
    print_packages(listing);
    print_notes(listing);
}

/// 根那一段。**空根也印**，并且把"它今天在不在"说出来 ——
/// 那是 `--json` 的形状里没有的东西（那里空根与不存在的根都是零个包）。
fn print_roots(listing: &GlobalsListing) {
    println!("tuoen 管着的根（`%LOCALAPPDATA%\\tuoen\\globals`）：");
    if listing.roots.is_empty() {
        println!("  （一个都算不出来 —— 见下面的说明。）");
        println!();
        return;
    }
    for row in &listing.roots {
        let state = if row.exists { "在" } else { "还没有" };
        let count = listing.package_count(row.tool, row.source);
        println!(
            "  · {} {} —— {}，{} 个包",
            row.tool.slug(),
            row.root.display(),
            state,
            count
        );
    }
    println!();
}

/// 包那一段：六列。
fn print_packages(listing: &GlobalsListing) {
    if listing.packages.is_empty() {
        println!("全局包：**一个都没有**。");
        println!("  （两个来源都是空的 —— 这台机器上没有全局包，或者两个工具都不在 `PATH` 上。）");
        println!();
        return;
    }

    // 列宽按内容算（CJK 宽度走 `display_width`：中文与全角字符占两格）。
    let headers = ["工具", "运行时版本", "来源", "包名", "版本", "bin 名"];
    let rows: Vec<[String; 6]> = listing
        .packages
        .iter()
        .map(|row| {
            [
                row.tool.slug().to_owned(),
                row.tool_version.clone(),
                row.source.slug().to_owned(),
                row.name.clone(),
                row.version.clone(),
                // 拿不到 bin 名时印 `?` —— 那是"不知道"，不是"没有"。
                // `-` 会被读成"这个包没有命令"，而 `?` 逼读者去看 `--json` 里
                // 那个**不存在的键**（决策：三态不许合并）。
                if row.bin_names.is_empty() {
                    "?".to_owned()
                } else {
                    row.bin_names.join(", ")
                },
            ]
        })
        .collect();

    let mut widths = headers.map(crate::display_width);
    for row in &rows {
        for (index, cell) in row.iter().enumerate() {
            widths[index] = widths[index].max(crate::display_width(cell));
        }
    }

    print_row(&headers.map(str::to_owned), &widths);
    for row in &rows {
        print_row(row, &widths);
    }

    let machine = listing
        .packages
        .iter()
        .filter(|row| row.source == GlobalsSource::Machine)
        .count();
    let tuoen = listing.packages.len() - machine;
    println!();
    println!(
        "  共 {} 个包：机器自己的 {} 个 / tuoen 管的 {} 个。",
        listing.packages.len(),
        machine,
        tuoen
    );
    println!(
        "  （`?` = 拿不到命令名，不是「它没有命令」；那种包在 `--json` 里 `binNames` 键整个不出现。）"
    );
    println!();
}

/// 印一行（表头用同样一套宽度：列数不齐的表在 zh-CN 下特别难看）。
fn print_row(cells: &[String; 6], widths: &[usize; 6]) {
    let mut line = String::from("  ");
    for (index, cell) in cells.iter().enumerate() {
        line.push_str(cell);
        if index + 1 < cells.len() {
            let pad = widths[index].saturating_sub(crate::display_width(cell)) + 2;
            line.push_str(&" ".repeat(pad));
        }
    }
    println!("{}", line.trim_end());
}

/// "为什么这里什么都没有"那几句。
fn print_notes(listing: &GlobalsListing) {
    for note in &listing.notes {
        match note.code {
            tuoen_core::GlobalsNoteCode::ToolNotOnPath => {
                println!(
                    "· {} 不在 `PATH` 上 —— 它一行都没有（决策 171：找不到可执行文件的工具不产生行）。",
                    note.tool.slug()
                );
            }
            tuoen_core::GlobalsNoteCode::RuntimeVersionUnknown => {
                println!(
                    "· npm 的根**算不出来**：它的版本目录名是 `node -v` 的原样输出，而这次没答上来。"
                );
                println!("  （装上 node，或去掉 `--no-version` 让它去问一次。）");
            }
        }
    }
}
