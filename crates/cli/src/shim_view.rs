//! `tuoen shim` 的 **`--json` 形状**与**人类输出**。
//!
//! 与 [`crate::manage_view`] 分开：那一族描述的是"装了什么、刚发生了什么"，
//! 这一族描述的是"`PATH` 上放了哪些文件、它们各自会跑什么"。
//! 两者的消费者不同（一个是安装流程，一个是 `PATH` 上的事实）。
//!
//! ## 两条硬规矩
//!
//! 1. **`--json` 的成功载荷里不出现中文**（决策 35）。所以：
//!    - 每条命令的 `status` 是稳定 slug（`created` / `replaced` / `planned` / `failed`）；
//!    - 核心层的 `note`（"为什么这条命令需要前缀参数"那句中文字）用
//!      `#[serde(skip)]` 挡在 JSON 外面，**只进人类输出**；
//!    - 失败时的 `code` 是小写 kebab ASCII，中文只在 `message` 里 ——
//!      而带 `message` 的结果一定是**失败信封**（`ok: false`）。
//! 2. **人类输出是中文**，而且要把"为什么"说出来，不只是"是什么"。

use serde::Serialize;

use tuoen_shim::TemplateSource;

use crate::display_width;

/// 每条命令 / 每个文件的**稳定结局**。取值进 `--json`，改它们要递增 `schemaVersion`。
pub mod status {
    /// `shim add`：这个文件是这次新建的。
    pub const CREATED: &str = "created";
    /// `shim add`：这个文件原来就有一个我们的 shim，被覆盖了。
    pub const REPLACED: &str = "replaced";
    /// `shim add --dry-run`：只是计划，什么都没有写。
    pub const PLANNED: &str = "planned";
    /// `shim add`：这一条没生成出来（`code` / `message` 里说明为什么）。
    pub const FAILED: &str = "failed";
    /// `shim list`：解出了烘在里面的目标 —— 它是我们的 shim。
    pub const OK: &str = "ok";
    /// `shim list` / `shim remove`：不是 tuoen 生成的 shim（或已损坏）。
    pub const NOT_A_SHIM: &str = "not-a-shim";
    /// `shim remove`：删掉了。
    pub const REMOVED: &str = "removed";
    /// `shim remove`：那个名字**本来就不在** —— 这不是失败，是幂等的成功。
    ///
    /// 取值与 `tuoen_platform::NoopReason::Absent` 的 slug 一致（`path remove` 那边
    /// 同一件事说的是同一个词）：**同一个工具里"本来就不在"只有一种含义**。
    /// 名字里的 `ABSENT` 而不是 `NOT_FOUND` 是刻意的 —— `not-found` 读起来像一个错误码，
    /// 而这件事的结论恰恰是"没有出错，你要的终态已经成立"。
    pub const ABSENT: &str = "absent";
    /// `shim remove`：名字本身不合法（分隔符 / 冒号 / 保留设备名……）。
    pub const BAD_NAME: &str = "bad-name";
}

/// `TemplateSource` → 稳定 slug（**不本地化**）。
#[must_use]
pub fn template_source_slug(source: &TemplateSource) -> &'static str {
    match source {
        TemplateSource::EnvVar => "env-var",
        TemplateSource::BesideExe => "beside-exe",
    }
}

/// 模板来源的人话。
#[must_use]
pub fn template_source_human(slug: &str) -> &'static str {
    match slug {
        "env-var" => "来自环境变量 TUOEN_SHIM_TEMPLATE",
        "beside-exe" => "与 tuoen.exe 同目录",
        _ => "来源不明",
    }
}

/// "这个文件为什么不是我们的 shim"的人话。
#[must_use]
pub fn problem_human(problem: Option<&str>) -> &'static str {
    match problem {
        Some("no-slot-magic") => "文件里没有 shim 的槽位魔数",
        Some("not-a-file") => "它不是一个普通文件",
        Some("read-failed") => "读不了这个文件",
        _ => "原因不明",
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// shim add
// ─────────────────────────────────────────────────────────────────────────────

/// `tuoen shim add --json` 的载荷。
///
/// **`--dry-run` 与真装是同一个形状**，区别只在每条命令的 `status`（`planned` vs
/// `created` / `replaced`）与计数（演练时全是 0）。两套形状会让消费者写两套解析，
/// 而"我只想看会做什么"与"我真做了"要读的字段是同一批（决策 20）。
#[derive(Debug, Serialize)]
pub struct ShimAddView {
    #[serde(rename = "dryRun")]
    pub dry_run: bool,
    pub tool: String,
    /// 这次用的是哪个版本（不带版本号时就是当前生效版本）。
    pub version: String,
    /// **当前生效版本**。与 `version` 不同时，`notes` 里会有一条 `version-not-active`。
    #[serde(rename = "activeVersion")]
    pub active_version: Option<String>,
    #[serde(rename = "shimDir")]
    pub shim_dir: String,
    /// 载荷根：`<store>/<工具>/current`。**这是 shim 真正指向的东西。**
    #[serde(rename = "payloadRoot")]
    pub payload_root: String,
    pub template: String,
    #[serde(rename = "templateSource")]
    pub template_source: &'static str,
    pub commands: Vec<ShimCommandView>,
    pub created: usize,
    pub replaced: usize,
    pub planned: usize,
    pub failed: usize,
    /// 结论性的提醒，**稳定 slug**（人话在人类输出里，JSON 里不出现中文）。
    ///
    /// 现在只有一条：`version-not-active`（你指定的版本不是当前生效版本 ——
    /// 而 shim 指向 `current`，所以它跑的不是你指定的那个）。
    pub notes: Vec<&'static str>,
}

impl ShimAddView {
    /// 第一条失败的命令的错误码 —— 部分失败时它就是整个信封的错误码。
    #[must_use]
    pub fn first_failure_code(&self) -> Option<&'static str> {
        self.commands
            .iter()
            .find(|command| command.status == status::FAILED)
            .and_then(|command| command.code)
    }

    /// 部分失败时的中文汇总（进失败信封的 `error.message`）。
    #[must_use]
    pub fn failure_summary(&self) -> String {
        let failed: Vec<&str> = self
            .commands
            .iter()
            .filter(|command| command.status == status::FAILED)
            .map(|command| command.file.as_str())
            .collect();
        let succeeded = self.commands.len() - failed.len();
        let mut message = format!(
            "{} 条命令里有 {} 条没生成出来：{}。",
            self.commands.len(),
            failed.len(),
            failed.join("、")
        );
        if let Some(first) = self
            .commands
            .iter()
            .find(|command| command.status == status::FAILED)
            .and_then(|command| command.message.as_deref())
        {
            message.push_str("\n第一条的原因：");
            message.push_str(first);
        }
        message.push_str(&format!(
            "\n**已经成功的那 {succeeded} 条不会回滚** —— shim 是逐条落盘的，\
             把它们删掉只会让状态更难解释。"
        ));
        message
    }
}

/// 一条命令的结局。
#[derive(Debug, Serialize)]
pub struct ShimCommandView {
    /// 命令名（不带扩展名）。
    pub command: String,
    /// 落盘文件名（`<命令名>.exe`）。
    pub file: String,
    /// 目标可执行文件的绝对路径（前缀的第一个 token）。
    pub target: String,
    /// 烘进槽位的完整前缀（`"<目标>" <前缀参数…>`）。**不是**本次调用的命令行。
    pub prefix: String,
    /// 稳定 slug：`created` / `replaced` / `planned` / `failed`。
    pub status: &'static str,
    /// 文件字节数（`planned` 时是 `null`）。
    pub bytes: Option<usize>,
    /// 模板里改写了几个槽位（正常情况下是 1，见 `ShimOutcome`）。
    #[serde(rename = "slotCount")]
    pub slot_count: Option<usize>,
    /// 落盘位置上**之前**有没有东西（`planned` 时告诉你"这一条会覆盖"）。
    #[serde(rename = "fileExists")]
    pub file_exists: bool,
    /// 失败时的稳定错误码（就是 `ShimError::kind()`）。
    pub code: Option<&'static str>,
    /// 失败时的中文消息。
    pub message: Option<String>,
    /// 这个失败是不是"用户自己改得对"的那一类。
    #[serde(rename = "userError")]
    pub user_error: Option<bool>,
    /// 为什么这条命令需要前缀参数（中文）—— **只进人类输出**。
    ///
    /// `#[serde(skip)]` 不是省事，是纪律：`--json` 的成功载荷里不能有本地化文本。
    #[serde(skip)]
    pub note: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// shim list
// ─────────────────────────────────────────────────────────────────────────────

/// `tuoen shim list --json` 的载荷。
#[derive(Debug, Serialize)]
pub struct ShimListView {
    #[serde(rename = "shimDir")]
    pub shim_dir: String,
    /// **按文件名排序**：目录的枚举顺序是任意的，而 `--json` 必须逐字节稳定（决策 35）。
    pub shims: Vec<ShimEntryView>,
}

/// shim 目录里的一个文件。
#[derive(Debug, Serialize)]
pub struct ShimEntryView {
    /// 命令名（文件名去掉 `.exe`）。
    pub command: String,
    /// 文件名（`node.exe`）。
    pub file: String,
    pub path: String,
    pub bytes: u64,
    /// 稳定 slug：`ok`（是我们的 shim）/ `not-a-shim`（不是，或已损坏）。
    pub status: &'static str,
    /// 不是我们的 shim 时，具体是哪一种：`no-slot-magic` / `not-a-file` / `read-failed`。
    pub problem: Option<&'static str>,
    /// 解出来的目标（前缀的第一个 token）。解不出来是 `null`。
    pub target: Option<String>,
    /// 解出来的完整前缀。解不出来是 `null`。
    pub prefix: Option<String>,
    /// 目标现在还在不在。**一条指向已删版本的 shim 是坏的，而它不会自己报警。**
    #[serde(rename = "targetExists")]
    pub target_exists: Option<bool>,
}

// ─────────────────────────────────────────────────────────────────────────────
// shim remove
// ─────────────────────────────────────────────────────────────────────────────

/// `tuoen shim remove --json` 的载荷。
#[derive(Debug, Serialize)]
pub struct ShimRemoveView {
    #[serde(rename = "shimDir")]
    pub shim_dir: String,
    /// 用户给的每一个名字一条，**顺序与命令行一致**。
    pub results: Vec<ShimRemoveEntryView>,
    /// 真的删掉了几条。
    pub removed: usize,
    /// 有几个名字**本来就不在** —— 那是幂等的成功，不是失败（退出码仍然是 0）。
    ///
    /// 单独给一个计数，是为了让 `--json` 的消费者**不用扫 `results` 就能回答**
    /// "这次到底删掉了什么、哪些是本来就不在的"。三个计数与 `results` 由
    /// [`ShimRemoveView::new`] 一次算出，所以它们不可能互相矛盾。
    pub absent: usize,
    /// 有几个名字**没删成**（名字不合法 / 那是个不属于我们的文件 / 读不了 / 删不掉）。
    /// 它是退出码的唯一依据：`failed == 0` ⟺ 退出码 0。
    pub failed: usize,
    /// 删完之后，**同一个工具**还留在盘上的命令（命令名，已排序）。
    ///
    /// 存在的理由是 `add` 与 `remove` 的粒度不一样：`add node` 一次生成 4 条，
    /// 而 `remove node` 只删 `node.exe`。这个不对称本身合理（`remove npm`
    /// 必须能只删 npm），但**静默地只删一条**会让人以为删干净了 ——
    /// 所以这里把漏下的那几条报出来（决策 73 的同一条原则：报告，不替用户做决定）。
    #[serde(rename = "siblingsLeft")]
    pub siblings_left: Vec<String>,
    /// 这些"漏下的兄弟"属于哪个工具（`siblingsLeft` 为空时是 `null`）。
    #[serde(rename = "siblingsTool")]
    pub tool_id: Option<String>,
}

impl ShimRemoveView {
    /// 漏下的同工具命令的中文提示（**只进人类输出**）。
    ///
    /// 给出了**可以直接抄的完整命令**：只说"还剩 npm、npx、corepack"是在让用户
    /// 自己拼一条命令，那正是这一类不对称最容易出错的地方。
    #[must_use]
    pub fn siblings_hint(&self) -> Option<String> {
        if self.siblings_left.is_empty() {
            return None;
        }
        Some(format!(
            "注意：`{}` 这个工具还有 {} 条 shim 在盘上（{}）。\n\
             `remove` 收的是**命令名**，所以刚才只删了同名的那些；\
             要一起删：`tuoen shim remove {}`。",
            self.tool_id.as_deref().unwrap_or("?"),
            self.siblings_left.len(),
            self.siblings_left.join("、"),
            self.siblings_left.join(" ")
        ))
    }
}

impl ShimRemoveView {
    /// 从逐条结果装出视图，**顺手把三个计数算出来**。
    ///
    /// 让调用方自己数的话，`removed` / `absent` / `failed` 与 `results` 迟早会不一致 ——
    /// 而 `failed` 正是退出码的唯一依据，一个数错的 `failed` 会让退出码说反话。
    #[must_use]
    pub fn new(
        shim_dir: String,
        results: Vec<ShimRemoveEntryView>,
        siblings_left: Vec<String>,
        tool_id: Option<String>,
    ) -> Self {
        let count = |wanted: &str| {
            results
                .iter()
                .filter(|entry| entry.status == wanted)
                .count()
        };
        Self {
            shim_dir,
            removed: count(status::REMOVED),
            absent: count(status::ABSENT),
            failed: results.iter().filter(|entry| !entry.is_done()).count(),
            results,
            siblings_left,
            tool_id,
        }
    }

    /// 第一个**没删成**的名字的错误码 —— 部分失败时它就是整个信封的错误码。
    ///
    /// **`absent` 不算没删成**：它的 `code` 是 `None`，而"本来就不在"与"删不掉"
    /// 是两种结论完全相反的事（前者退出码 0，后者非 0）。
    #[must_use]
    pub fn first_failure_code(&self) -> Option<&'static str> {
        self.results
            .iter()
            .filter(|entry| !entry.is_done())
            .find_map(|entry| entry.code)
    }

    /// 部分失败时的中文汇总（进失败信封的 `error.message`）。
    ///
    /// **必须带上每一个名字的**具体原因**，而不是只说"N 个没删成"：
    /// `--json` 的消费者读不到人类输出，而"哪一个、为什么"正是它要的东西。
    /// **本来就不在的那些不进这里** —— 它们不是失败，一句"没删成"会把结论说反。
    #[must_use]
    pub fn failure_summary(&self) -> String {
        let mut message = String::new();
        for entry in self.results.iter().filter(|entry| !entry.is_done()) {
            message.push_str(&format!(
                "`{}` 没删成：{}\n",
                entry.name,
                entry.message.as_deref().unwrap_or("（没有说明）")
            ));
        }
        let removed = self.removed;
        if removed > 0 {
            message.push_str(&format!(
                "**已经删掉的那 {removed} 条不会恢复**（删除是逐条的）。"
            ));
        }
        message
    }
}

/// 一个名字的处理结果。
#[derive(Debug, Serialize)]
pub struct ShimRemoveEntryView {
    /// 用户给的名字，**原样**（名字被拒时只有它是可靠的）。
    pub name: String,
    /// 对应的落盘文件名；名字不合法时是 `null`（我们没有为它构造过路径）。
    pub file: Option<String>,
    /// 实际看/删的路径；名字不合法时是 `null`。
    pub path: Option<String>,
    /// 稳定 slug：`removed` / `absent` / `not-a-shim` / `bad-name` / `failed`。
    ///
    /// 前两个是**成功**（`absent` 是"本来就不在"，幂等），后三个是**没删成**。
    pub status: &'static str,
    /// 删掉时的文件字节数。
    pub bytes: Option<u64>,
    /// 没删成时的稳定错误码。**成功的两种结局都是 `null`**（空操作没有错误码）。
    pub code: Option<&'static str>,
    /// 没删成时的中文消息。**`absent` 也是 `null`** —— 中文只在人类输出里
    /// （决策 35：成功载荷里不出现本地化文本）。
    pub message: Option<String>,
    /// 这个失败是不是"用户自己改得对"的那一类。
    #[serde(rename = "userError")]
    pub user_error: Option<bool>,
}

impl ShimRemoveEntryView {
    /// 这个名字处理**完了**吗 —— 删掉了，或者本来就不在。
    ///
    /// 这个判据只有一处定义：退出码、三个计数、失败汇总页都从这里出发。
    /// 分开写的话，"本来就不在"会在某一处被当成失败 —— 而那是本票要修的 bug 本身。
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.status == status::REMOVED || self.status == status::ABSENT
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// shim path
// ─────────────────────────────────────────────────────────────────────────────

/// `tuoen shim path --json` 的载荷。
#[derive(Debug, Serialize)]
pub struct ShimPathView {
    pub path: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// 人类输出（中文优先）
// ─────────────────────────────────────────────────────────────────────────────

/// `tuoen shim add` 的人类输出。
pub fn print_add_human(view: &ShimAddView) {
    if view.dry_run {
        println!("（演练 —— 什么都没有写）");
        println!();
    }
    println!("{} {}", view.tool, view.version);
    match &view.active_version {
        Some(active) if active == &view.version => println!("当前生效版本：{active}"),
        Some(active) => println!("当前生效版本：{active}（**不是你指定的这个**）"),
        None => println!("当前生效版本：（没有）"),
    }
    println!("shim 目录：{}", view.shim_dir);
    println!("载荷根：{}", view.payload_root);
    println!(
        "模板：{}（{}）",
        view.template,
        template_source_human(view.template_source)
    );
    println!();

    for command in &view.commands {
        let (mark, what) = match command.status {
            status::CREATED => ("✓", "新建"),
            status::REPLACED => ("↻", "覆盖（原来那个也是 tuoen 的 shim）"),
            status::PLANNED => (
                "·",
                if command.file_exists {
                    "计划：覆盖已存在的文件"
                } else {
                    "计划：新建"
                },
            ),
            _ => ("✗", "失败"),
        };
        println!("{mark} {} —— {what}", command.file);
        println!("    会执行：{}", command.prefix);
        if let Some(note) = &command.note {
            println!("    {note}");
        }
        if let Some(message) = &command.message {
            let code = command.code.unwrap_or("unknown");
            println!("    错误[{code}]：{message}");
            if command.user_error == Some(false) {
                println!("    这一条看起来不是用法问题 —— 请把上面的原文报告给我们。");
            }
        }
    }

    println!();
    if view.dry_run {
        println!(
            "演练结束：{} 条计划{}。真的跑一遍才会写文件。",
            view.planned,
            if view.commands.iter().any(|command| command.file_exists) {
                "（其中有的会覆盖已存在的文件）"
            } else {
                ""
            }
        );
    } else {
        println!(
            "{} 条：{} 新建、{} 覆盖、{} 失败。",
            view.commands.len(),
            view.created,
            view.replaced,
            view.failed
        );
    }

    if view.notes.contains(&"version-not-active") {
        println!();
        println!(
            "⚠ 你指定的是 {}，而当前生效版本是 {}。",
            view.version,
            view.active_version.as_deref().unwrap_or("（没有）")
        );
        println!(
            "  shim 指向 `current` 这个链接，所以这些 shim 现在跑的**不是**你指定的那个版本。"
        );
        if view.active_version.is_some() {
            println!("  要真的切过去：`tuoen use {} {}`", view.tool, view.version);
        } else {
            println!(
                "  先让它生效：`tuoen use {} {}`（或 `tuoen install {}@{} --use`）",
                view.tool, view.version, view.tool, view.version
            );
        }
    }

    println!();
    println!("shim 指向的是 `current`（一个链接），不是某个版本目录 ——");
    println!("所以**切版本不用重新生成 shim**（决策 11）：`tuoen use` 只翻转链接。");
    println!();
    println!("把这些命令放进当前会话的 PATH：");
    println!("  $env:Path = \"$(tuoen shim path);$env:Path\"");
    println!("（tuoen 现在还**不改 PATH** —— 它只把文件放进 shim 目录。）");
}

/// `tuoen shim list` 的人类输出。
pub fn print_list_human(view: &ShimListView) {
    println!("shim 目录：{}", view.shim_dir);
    if view.shims.is_empty() {
        println!();
        println!("（还没有任何 shim）");
        println!();
        println!("给一个工具生成：`tuoen shim add node` —— 前提是那个工具有生效版本。");
        return;
    }

    let name_w = view
        .shims
        .iter()
        .map(|entry| display_width(&entry.command))
        .max()
        .unwrap_or(4)
        .max(4);
    let byte_w = view
        .shims
        .iter()
        .map(|entry| entry.bytes.to_string().len())
        .max()
        .unwrap_or(4)
        .max(4);

    println!();
    println!(
        "{:<name_w$}  {:>byte_w$}  会执行什么",
        "名称",
        "字节",
        name_w = name_w,
        byte_w = byte_w
    );
    for entry in &view.shims {
        match entry.status {
            status::OK => {
                println!(
                    "{:<name_w$}  {:>byte_w$}  {}",
                    entry.command,
                    entry.bytes,
                    entry.prefix.as_deref().unwrap_or("?"),
                    name_w = name_w,
                    byte_w = byte_w
                );
                if entry.target_exists == Some(false) {
                    println!(
                        "{:width$}⚠ 目标不存在：{} —— 那个版本可能已经被删了",
                        "",
                        entry.target.as_deref().unwrap_or("?"),
                        width = name_w + byte_w + 4
                    );
                }
            }
            _ => {
                println!(
                    "{:<name_w$}  {:>byte_w$}  （不是 tuoen 的 shim 或已损坏：{}）",
                    entry.command,
                    entry.bytes,
                    problem_human(entry.problem),
                    name_w = name_w,
                    byte_w = byte_w
                );
            }
        }
    }

    println!();
    println!("共 {} 条。", view.shims.len());
    let foreign = view
        .shims
        .iter()
        .filter(|entry| entry.status != status::OK)
        .count();
    if foreign > 0 {
        println!("其中 {foreign} 条不是 tuoen 生成的 shim —— `tuoen shim remove` **不会**删它们。");
    }
    println!();
    println!("`会执行什么`是 shim 里烘着的完整前缀；你敲的参数会原样接在它后面。");
    println!("把这个目录放进当前会话的 PATH：");
    println!("  $env:Path = \"$(tuoen shim path);$env:Path\"");
}

/// `tuoen shim remove` 的人类输出。
///
/// **三种结局要长得不一样**：真的删了（`✓`）、本来就不在（`·`，而且明说"什么都没有改"）、
/// 没删成（`✗` + 原因）。第二类以前借用的是失败那一行的形状 —— 于是一句
/// "要删的东西本来就不在"看起来像出了错，而它恰恰是成功的终态。
///
/// 每一行都由一个**纯函数**产出来（[`removed_line`] / [`absent_line`] /
/// [`summary_line`]），测试断言的是那些函数本身，而不是重新拼一遍它们的格式串。
pub fn print_remove_human(view: &ShimRemoveView) {
    for entry in &view.results {
        if entry.status == status::REMOVED {
            println!("{}", removed_line(entry));
        } else if entry.status == status::ABSENT {
            println!("{}", absent_line(entry));
        } else {
            println!("✗ {}", entry.message.as_deref().unwrap_or("（没有说明）"));
            if entry.user_error == Some(false) {
                println!("  这一条看起来不是用法问题 —— 请把上面的原文报告给我们。");
            }
        }
    }

    println!();
    println!("{}", summary_line(view));
    if let Some(hint) = view.siblings_hint() {
        println!();
        println!("{hint}");
    }
}

/// 一条"真的删掉了"的正文。
#[must_use]
pub fn removed_line(entry: &ShimRemoveEntryView) -> String {
    format!(
        "✓ 已删除 {}（{} 字节）",
        entry.file.as_deref().unwrap_or(&entry.name),
        entry.bytes.unwrap_or(0)
    )
}

/// 一条"本来就不在"的正文。
///
/// **不是一个错误行**：要删的东西不在，就是删完之后的状态 —— 这句话必须说出来，
/// 否则"退出码 0 但输出里只有一个 ✗"会让人以为哪里坏了。
#[must_use]
pub fn absent_line(entry: &ShimRemoveEntryView) -> String {
    format!(
        "· `{}` 本来就不在{} —— **什么都没有改**。",
        entry.name,
        looked_at(entry)
    )
}

/// 收尾那一行（逐条结果之后、兄弟提示之前）。
#[must_use]
pub fn summary_line(view: &ShimRemoveView) -> String {
    if view.failed > 0 {
        let mut text = format!(
            "{} 个名字里有 {} 个没删成。",
            view.results.len(),
            view.failed
        );
        if view.removed > 0 {
            text.push_str(&format!(
                "\n已经删掉的那 {} 条**不会恢复**（删除是逐条的）。",
                view.removed
            ));
        }
        if view.absent > 0 {
            text.push_str(&format!(
                "\n另外 {} 个名字本来就不在 —— 那不算没删成，它们已经是终态。",
                view.absent
            ));
        }
        text.push_str("\n用 `tuoen shim list` 看现在还剩什么。");
        return text;
    }

    match (view.removed, view.absent) {
        (removed, 0) => format!("{removed} 条已删除。"),
        (0, absent) => format!(
            "{absent} 个名字本来就不在 —— **这不是错误**：\
             要删的东西不在，就是删完之后的状态（幂等）。\n用 `tuoen shim list` 看现在有哪些。"
        ),
        (removed, absent) => format!(
            "{removed} 条已删除，{absent} 个名字本来就不在（那也不是错误）。\
             \n用 `tuoen shim list` 看现在有哪些。"
        ),
    }
}

/// `（找的是 `…`）` —— 我们到底看了哪个文件。没有路径（名字被拒）时什么都不印。
fn looked_at(entry: &ShimRemoveEntryView) -> String {
    entry
        .path
        .as_deref()
        .map_or_else(String::new, |path| format!("（找的是 `{path}`）"))
}

/// `tuoen shim path` 的人类输出：**恰好一行，就是那个路径**。
///
/// 不加引号、不加前缀 —— 这条命令存在的理由就是脚本里的
/// `export PATH=$(tuoen shim path)`。
pub fn print_path_human(view: &ShimPathView) {
    println!("{}", view.path);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_command(note: Option<&str>) -> ShimCommandView {
        ShimCommandView {
            command: "npm".to_owned(),
            file: "npm.exe".to_owned(),
            target: r"C:\s\node\current\node.exe".to_owned(),
            prefix: r#""C:\s\node\current\node.exe" "C:\s\node\current\npm-cli.js""#.to_owned(),
            status: status::CREATED,
            bytes: Some(1024),
            slot_count: Some(1),
            file_exists: false,
            code: None,
            message: None,
            user_error: None,
            note: note.map(str::to_owned),
        }
    }

    fn a_view(commands: Vec<ShimCommandView>) -> ShimAddView {
        ShimAddView {
            dry_run: false,
            tool: "node".to_owned(),
            version: "24.19.0".to_owned(),
            active_version: Some("24.19.0".to_owned()),
            shim_dir: r"C:\s\shims".to_owned(),
            payload_root: r"C:\s\node\current".to_owned(),
            template: r"C:\s\tuoen-shim.exe".to_owned(),
            template_source: "beside-exe",
            commands,
            created: 1,
            replaced: 0,
            planned: 0,
            failed: 0,
            notes: Vec::new(),
        }
    }

    fn has_cjk(text: &str) -> bool {
        text.chars()
            .any(|c| (0x4E00..=0x9FFF).contains(&(u32::from(c))))
    }

    #[test]
    fn a_successful_payload_keeps_the_chinese_note_out_of_the_json() {
        // **这条是 `#[serde(skip)]` 的理由**：`note` 是给中国人看的一句话，
        // 而 `--json` 的成功载荷里出现中文就意味着界面语言把脚本绑死了（决策 35）。
        let json = serde_json::to_string(&a_view(vec![a_command(Some("中文说明：走 cli.js"))]))
            .expect("serialise");
        assert!(
            !has_cjk(&json),
            "成功载荷里不该有 CJK（note 必须只进人类输出）：{json}"
        );
        assert!(json.contains(r#""status":"created""#), "{json}");
        assert!(json.contains(r#""slotCount":1"#), "{json}");
    }

    #[test]
    fn every_status_slug_is_lowercase_kebab_ascii() {
        for slug in [
            status::CREATED,
            status::REPLACED,
            status::PLANNED,
            status::FAILED,
            status::OK,
            status::NOT_A_SHIM,
            status::REMOVED,
            status::ABSENT,
            status::BAD_NAME,
        ] {
            assert!(
                !slug.is_empty()
                    && slug
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "`{slug}` 不是小写 kebab ASCII"
            );
        }
    }

    #[test]
    fn a_dry_run_view_says_planned_and_has_null_sizes() {
        let mut command = a_command(None);
        command.status = status::PLANNED;
        command.bytes = None;
        command.slot_count = None;
        command.file_exists = true;
        let json = serde_json::to_string(&a_view(vec![command])).expect("serialise");
        assert!(json.contains(r#""status":"planned""#), "{json}");
        assert!(json.contains(r#""bytes":null"#), "{json}");
        assert!(json.contains(r#""fileExists":true"#), "{json}");
    }

    #[test]
    fn the_failure_summary_says_the_successful_ones_are_not_rolled_back() {
        let mut failed = a_command(None);
        failed.status = status::FAILED;
        failed.code = Some("target-unreadable");
        failed.message = Some("读不到 shim 目标".to_owned());
        let failed_file = failed.file.clone();
        let mut view = a_view(vec![a_command(None), failed]);
        view.created = 1;
        view.failed = 1;

        assert_eq!(view.first_failure_code(), Some("target-unreadable"));
        let summary = view.failure_summary();
        assert!(summary.contains("1 条没生成出来"), "{summary}");
        // 两个承诺：谁没成（点名）、成的那几条怎么样了（不回滚、还剩几条）。
        assert!(
            summary.contains(&failed_file),
            "要点出是哪个文件没生成出来：{summary}"
        );
        assert!(
            summary.contains("不会回滚"),
            "必须明说已经成功的那几条保留下来了：{summary}"
        );
        assert!(
            summary.contains("已经成功的那 1 条"),
            "要点出保住了几条：{summary}"
        );
        assert!(
            summary.contains("读不到 shim 目标"),
            "要带上第一条的具体原因：{summary}"
        );
    }

    #[test]
    fn the_remove_failure_summary_names_every_name_and_its_own_reason() {
        // `--json` 的消费者读不到人类输出，所以失败信封里的消息必须**逐个点名**，
        // 而不是只说"N 个没删成"。
        let view = ShimRemoveView::new(
            r"C:\s\shims".to_owned(),
            vec![
                removed_entry("node"),
                absent_entry("ghost"),
                ShimRemoveEntryView {
                    name: "broken".to_owned(),
                    file: Some("broken.exe".to_owned()),
                    path: Some(r"C:\s\shims\broken.exe".to_owned()),
                    status: status::NOT_A_SHIM,
                    bytes: Some(7),
                    code: Some("not-a-shim"),
                    message: Some("`broken.exe` 不是 tuoen 生成的 shim，**没有删除**。".to_owned()),
                    user_error: Some(true),
                },
            ],
            Vec::new(),
            None,
        );
        assert_eq!(view.failed, 1, "`absent` 不算没删成");
        assert_eq!(view.removed, 1);
        assert_eq!(view.absent, 1);
        assert_eq!(
            view.first_failure_code(),
            Some("not-a-shim"),
            "错误码必须来自**真正**没删成的那一个，而不是它前面那个「本来就不在」的"
        );
        let summary = view.failure_summary();
        assert!(summary.contains("`broken` 没删成"), "{summary}");
        assert!(summary.contains("不是 tuoen 生成的 shim"), "{summary}");
        assert!(summary.contains("不会恢复"), "要说明没有回滚：{summary}");
        assert!(
            !summary.contains("`ghost`"),
            "「本来就不在」不是失败，不能被写进失败汇总：{summary}"
        );
    }

    /// 一条"删成功了"的结果。
    fn removed_entry(name: &str) -> ShimRemoveEntryView {
        ShimRemoveEntryView {
            name: name.to_owned(),
            file: Some(format!("{name}.exe")),
            path: Some(format!(r"C:\s\shims\{name}.exe")),
            status: status::REMOVED,
            bytes: Some(1024),
            code: None,
            message: None,
            user_error: None,
        }
    }

    /// 一条"本来就不在"的结果 —— **成功的空操作**，所以没有错误码、没有中文消息。
    fn absent_entry(name: &str) -> ShimRemoveEntryView {
        ShimRemoveEntryView {
            name: name.to_owned(),
            file: Some(format!("{name}.exe")),
            path: Some(format!(r"C:\s\shims\{name}.exe")),
            status: status::ABSENT,
            bytes: None,
            code: None,
            message: None,
            user_error: None,
        }
    }

    #[test]
    fn an_absent_name_is_a_success_and_its_slug_is_visible_in_the_json() {
        // **本票的核心**：`shim remove <不存在的名字>` 是**幂等的成功**（退出码 0）。
        // 而"本来就不在"这件事必须在 `--json` 里看得见 —— 靠 `status` 与 `absent` 计数，
        // **不靠中文消息**（成功载荷里不许有 CJK，见上一个用例）。
        let view = ShimRemoveView::new(
            r"C:\s\shims".to_owned(),
            vec![absent_entry("ghost")],
            Vec::new(),
            None,
        );
        assert_eq!(view.failed, 0, "「本来就不在」不是失败");
        assert_eq!(view.absent, 1);
        assert_eq!(view.removed, 0);
        assert_eq!(view.first_failure_code(), None);

        let json = serde_json::to_string(&view).expect("serialise");
        assert!(json.contains(r#""status":"absent""#), "{json}");
        assert!(json.contains(r#""absent":1"#), "{json}");
        assert!(json.contains(r#""failed":0"#), "{json}");
        assert!(
            !has_cjk(&json),
            "成功载荷里不该有 CJK —— 所以「本来就不在」的中文只在人类输出里：{json}"
        );
    }

    #[test]
    fn the_human_lines_of_an_absent_name_and_of_a_removal_are_different() {
        // 逐字钉住人话。**断言的是产出那些行的那两个函数**，不是在这里重拼一遍格式串 ——
        // 重拼的断言在文案改了之后照样通过，而那正是"什么都没比"。
        let absent = absent_line(&absent_entry("ghost"));
        assert!(absent.contains("本来就不在"), "{absent}");
        assert!(
            absent.contains(r"C:\s\shims\ghost.exe"),
            "要点出我们看的是哪个文件：{absent}"
        );
        assert!(absent.contains("什么都没有改"), "{absent}");
        assert!(
            !absent.starts_with('✗'),
            "「本来就不在」不是失败，不能用失败的记号：{absent}"
        );

        let removed = removed_line(&removed_entry("node"));
        assert!(removed.starts_with('✓'), "{removed}");
        assert!(removed.contains("已删除"), "{removed}");

        // 收尾行：全部"本来就不在"时，**必须**明说这不是错误（本票要修的就是这句话）。
        let all_absent = ShimRemoveView::new(
            r"C:\s\shims".to_owned(),
            vec![absent_entry("ghost")],
            Vec::new(),
            None,
        );
        let summary = summary_line(&all_absent);
        assert!(summary.contains("这不是错误"), "{summary}");
        assert!(summary.contains("幂等"), "{summary}");
        assert!(!summary.contains("没删成"), "{summary}");
        assert!(!summary.contains("不会恢复"), "{summary}");

        // 真的删掉时不能说"本来就不在"。
        let all_removed = ShimRemoveView::new(
            r"C:\s\shims".to_owned(),
            vec![removed_entry("node")],
            Vec::new(),
            None,
        );
        assert_eq!(summary_line(&all_removed), "1 条已删除。");
    }

    /// 删完之后"还剩哪几条兄弟"的那句提示。
    ///
    /// **这是实测抓出来的不对称**：`tuoen shim add node` 一次生成 4 条，
    /// 而 `tuoen shim remove node` 只删 `node.exe`（remove 收的是**命令名**）。
    /// 不对称本身合理，但**静默**会让用户以为删干净了 —— 所以提示必须给出
    /// **可以直接抄的完整命令**，而不是只列出几个名字让他自己拼。
    #[test]
    fn the_sibling_hint_gives_a_command_that_can_be_copied_verbatim() {
        let view = ShimRemoveView::new(
            r"C:\s\shims".to_owned(),
            Vec::new(),
            vec!["corepack".to_owned(), "npm".to_owned(), "npx".to_owned()],
            Some("node".to_owned()),
        );
        let hint = view.siblings_hint().expect("有兄弟就必须有提示");
        assert!(hint.contains("3 条"), "要报条数：{hint}");
        assert!(hint.contains("corepack、npm、npx"), "要点名：{hint}");
        assert!(
            hint.contains("tuoen shim remove corepack npm npx"),
            "要给出能直接抄的命令：{hint}"
        );
        assert!(hint.contains("命令名"), "要说清为什么只删了一条：{hint}");

        // 一条不剩时不说话 —— 删干净了还跟一句"注意"是噪声。
        let clean = ShimRemoveView::new(r"C:\s\shims".to_owned(), Vec::new(), Vec::new(), None);
        assert_eq!(clean.siblings_hint(), None);
    }

    #[test]
    fn template_source_slugs_are_stable_and_translated_by_name() {
        assert_eq!(
            template_source_slug(&TemplateSource::EnvVar),
            "env-var",
            "环境变量与同目录必须在 JSON 里分得开 —— 排查'到底用了哪个模板'全靠它"
        );
        assert_eq!(
            template_source_slug(&TemplateSource::BesideExe),
            "beside-exe"
        );
        assert!(template_source_human("env-var").contains("TUOEN_SHIM_TEMPLATE"));
        assert!(template_source_human("beside-exe").contains("tuoen.exe"));
        assert!(has_cjk(template_source_human("env-var")), "人话要是中文");
    }
}
