//! `tuoen shim add` / `remove` / `list` / `path` 的**执行**部分。
//!
//! 参数形状在 [`crate::shim`]，`--json` 形状与人类输出在 [`crate::shim_view`]；
//! 这里只做编排：把"这个工具要在 `PATH` 上暴露哪些命令"变成磁盘上的真 `.exe`。
//!
//! ## 三条贯穿这一族的决定
//!
//! 1. **shim 目录只有一个来源**：`Store::home().join("shims")`（`docs/DESIGN.md` 决策 48：
//!    shim 与 `store/` 并列，因为 `store/` 可以被清空，而 `PATH` 上那批文件属于用户）。
//!    这里不再发明第二个位置。
//! 2. **逐条落盘，不回滚**：一条命令失败**不会**删掉已经成功的那几条。把刚生成好的东西
//!    删掉，只会让"现在磁盘上到底是什么"更难解释。失败会报出来、退出码非 0，
//!    而**已经发生的事实**照样出现在输出里（`--json` 里是一个**带 `data` 的失败信封**）。
//! 3. **`remove` 的退出码语义**：只要有**一个**名字没删成，退出码就是 1（`not-found`
//!    也算没删成 —— 你要删的东西不在，这条命令没有完成它被要求做的事）。
//!    删除同样是逐条的，前面删掉的那些不会因为后面失败而恢复。
//!
//! ## 三件这一层**不做**的事
//!
//! * **不改 `PATH`**（那是另一张票的事）。它只把文件放进 shim 目录。
//! * **不猜启动器**：要暴露哪些命令来自 [`tuoen_core::shim_commands`]，
//!   而它对没实测过的工具返回空表 —— 我们宁可拒绝，也不生成一个指不到东西的 shim。
//! * **不跟随、不穿透**：只读自己目录里的普通文件。

use std::fmt;
use std::path::{Path, PathBuf};

use tuoen_shim::{
    SLOT_MAGIC, ShimError, ShimSpec, decode_slot, find_template, prefix_target, write_shim,
};
use tuoen_store::Store;

use crate::envelope::Envelope;
use crate::exit;
use crate::shim::{ShimAddArgs, ShimListArgs, ShimPathArgs, ShimRemoveArgs};
use crate::shim_view::{
    ShimAddView, ShimCommandView, ShimEntryView, ShimListView, ShimPathView, ShimRemoveEntryView,
    ShimRemoveView, status, template_source_slug,
};

/// 编排层的一条错误。
///
/// 两个来源，**分开是因为它们的错误码来源不同**：
/// * [`ShimError`] —— 错误码直接取 `kind()`，中文消息直接取它的 `Display`。
///   那是一句信息量很足的完整句子（含"为什么"），这里**不再翻译一遍**：
///   翻译一次就多一份会漂移的副本。
/// * 这一层自己的判断（没有生效版本、没实测过启动器、模板找不到……）。
#[derive(Debug)]
pub enum ShimCliError {
    /// shim crate 报的错。
    Shim(ShimError),
    /// 这一层自己的错。
    Own {
        /// 稳定的小写 kebab ASCII 错误码（进 `--json`）。
        code: &'static str,
        /// 中文消息。
        message: String,
    },
}

impl ShimCliError {
    fn own(code: &'static str, message: impl Into<String>) -> Self {
        Self::Own {
            code,
            message: message.into(),
        }
    }

    /// 稳定错误码（进 `--json`）。
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Shim(error) => error.kind(),
            Self::Own { code, .. } => code,
        }
    }
}

impl From<ShimError> for ShimCliError {
    fn from(error: ShimError) -> Self {
        Self::Shim(error)
    }
}

impl fmt::Display for ShimCliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Shim(error) => write!(f, "{error}"),
            Self::Own { message, .. } => f.write_str(message),
        }
    }
}

/// shim 目录：`<家目录>/shims`。**整个仓库里只有这一处定义它。**
///
/// `pub(crate)` 是给 `tuoen path` 用的：它要拿这个目录做两件事 ——
/// 保护它不被 `path remove` 摘掉，以及判断我们发布的命令有没有被别的目录遮蔽。
/// **另一个调用方不是"再拼一遍这个路径"的理由**：第二份定义会漂移，
/// 而它漂移的后果是 `PATH` 报告里的遮蔽检测静默地看了一个不存在的目录。
#[must_use]
pub(crate) fn shim_dir(store: &Store) -> PathBuf {
    store.home().join("shims")
}

// ─────────────────────────────────────────────────────────────────────────────
// shim add
// ─────────────────────────────────────────────────────────────────────────────

/// 跑 `tuoen shim add`。返回退出码。
pub fn run_add(args: &ShimAddArgs) -> i32 {
    match add(args) {
        Ok(view) if view.failed == 0 => {
            if args.json {
                crate::print_json(&Envelope::ok("shim.add", &view));
            } else {
                crate::shim_view::print_add_human(&view);
            }
            exit::SUCCESS
        }
        Ok(view) => {
            // **部分成功**：既有错误，也有已经发生的事实。
            // 报成纯失败会让消费者看不到已经生成了什么；报成 `ok: true` 又会让
            // `ok` 与退出码互相矛盾。
            let code = view.first_failure_code().unwrap_or("shim-failed");
            let message = view.failure_summary();
            if args.json {
                crate::print_json(&Envelope::partial("shim.add", code, message, &view));
            } else {
                crate::shim_view::print_add_human(&view);
            }
            exit::RUNTIME_ERROR
        }
        Err(error) => {
            if args.json {
                crate::print_json(&Envelope::err("shim.add", error.code(), error.to_string()));
            } else {
                eprintln!("tuoen: {error}");
            }
            exit::RUNTIME_ERROR
        }
    }
}

/// `shim add` 的全部逻辑，与输出方式无关 —— 这样测试能直接看返回值。
pub fn add(args: &ShimAddArgs) -> Result<ShimAddView, ShimCliError> {
    let (raw_tool, version_arg) = crate::manage::split_spec(&args.spec);
    if raw_tool.is_empty() {
        return Err(ShimCliError::own(
            "empty-tool",
            "没有给工具名。用法：`tuoen shim add node` 或 `tuoen shim add node@24.19.0`。",
        ));
    }
    if matches!(&version_arg, Some(version) if version.is_empty()) {
        return Err(ShimCliError::own(
            "empty-version",
            format!(
                "`{raw_tool}@` 后面没有版本号。想去掉 `@` 用当前生效版本，\
                 或者补上版本：`{raw_tool}@24`。"
            ),
        ));
    }
    let explicit_version = version_arg.is_some();

    let catalog = crate::manage_cmd::load_catalog()
        .map_err(|error| ShimCliError::own(error.code, error.message))?;
    let Some(tool) = tuoen_manifest::find_tool(&catalog, &raw_tool) else {
        return Err(ShimCliError::own(
            "unknown-tool",
            format!(
                "目录里没有工具 `{raw_tool}`；已知：{}",
                catalog
                    .tools
                    .iter()
                    .map(|tool| tool.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    };
    let tool_id = tool.id.clone();

    let store = Store::at_default_location();
    let active = tuoen_store::active_version(&store, &tool_id);

    // 不带版本号 = "当前生效版本"；带版本号 = "确认这个版本装过"。
    // **两者都不是"把 shim 钉到某个版本"** —— shim 指向的永远是 `current`。
    let version = match version_arg {
        Some(version) => {
            if tuoen_store::find_version(&store, &tool_id, &version).is_none() {
                let installed: Vec<String> = tuoen_store::installed_versions(&store, &tool_id)
                    .into_iter()
                    .map(|found| found.version)
                    .collect();
                let hint = if installed.is_empty() {
                    format!(
                        "`{tool_id}` 还没有装过任何版本。先 `tuoen install {tool_id}@{version}`。"
                    )
                } else {
                    format!("已安装的版本：{}", installed.join(", "))
                };
                return Err(ShimCliError::own(
                    "not-installed",
                    format!("`{tool_id}` 没有装过版本 `{version}`。{hint}"),
                ));
            }
            version
        }
        None => active.clone().ok_or_else(|| {
            ShimCliError::own(
                "no-active-version",
                format!(
                    "`{tool_id}` 现在没有生效版本，而不带版本号时用的是**当前激活版本**。\n\
                     先装一个并激活：`tuoen install {tool_id}@<版本> --use`\n\
                     或者指定一个已经装过的版本：`tuoen shim add {tool_id}@<版本>`"
                ),
            )
        })?,
    };

    // **没有实测过启动器的工具在这里被挡住。** 空表的意思是"我们不知道"，
    // 不是"这个工具没有命令" —— 猜一条相对路径的代价是生成一个敲了就报错的 shim。
    let commands = tuoen_core::shim_commands(&tool_id);
    if commands.is_empty() {
        return Err(ShimCliError::own(
            "no-shim-commands",
            format!(
                "`{tool_id}` 要在 PATH 上暴露哪些命令，我们**还没有实测过**，所以不猜。\n\
                 目前只有 `node` 一家是逐条实测过的（node / npm / npx / corepack）。\n\
                 一条猜出来的相对路径的症状是「shim 生成成功、敲命令时才发现目标不存在」—— \
                 那种失败被推迟到了最不该出现它的地方。"
            ),
        ));
    }

    let payload_root = store.current_link(&tool_id);
    if !payload_root.is_dir() {
        // 注意这条守卫的**位置**：它在写任何东西之前。如果放过去，
        // 每一条 write_shim 都会以 `target-unreadable` 失败 —— 而那句消息说的是
        // "那个版本可能已经被卸载"，与真实原因（根本没有生效版本）不是一回事。
        return Err(ShimCliError::own(
            "payload-missing",
            format!(
                "`{}` 不是一个能打开的目录 —— shim 指向它，所以现在生成的每一条 shim \
                 都会在运行时失败。\n先让它生效：`tuoen use {tool_id} {version}`\
                 （或 `tuoen install {tool_id}@{version} --use`）。",
                payload_root.display()
            ),
        ));
    }

    let template_dir = exe_dir()?;
    let (template, template_source) = find_template(&template_dir)?;

    let shim_dir = shim_dir(&store);
    let mut views: Vec<ShimCommandView> = Vec::with_capacity(commands.len());

    for command in &commands {
        let target = join_relative(&payload_root, &command.relative);
        let prefix_args: Vec<String> = command
            .prefix_args
            .iter()
            .map(|relative| {
                join_relative(&payload_root, relative)
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        let spec = ShimSpec {
            name: command.command.clone(),
            target,
            prefix_args,
        };
        let dest = shim_dir.join(spec.file_name());
        let file_exists = dest.exists();
        let target_text = spec.target.display().to_string();
        let prefix = spec.render_prefix();

        if args.dry_run {
            // 演练走的是与真装**同一套解析**，只差最后不落盘（决策 20）。
            views.push(ShimCommandView {
                command: command.command.clone(),
                file: spec.file_name(),
                target: target_text,
                prefix,
                status: status::PLANNED,
                bytes: None,
                slot_count: None,
                file_exists,
                code: None,
                message: None,
                user_error: None,
                note: command.note.clone(),
            });
            continue;
        }

        match write_shim(&template, &dest, &spec) {
            Ok(outcome) => views.push(ShimCommandView {
                command: command.command.clone(),
                file: spec.file_name(),
                target: target_text,
                prefix: outcome.prefix,
                status: if outcome.replaced {
                    status::REPLACED
                } else {
                    status::CREATED
                },
                bytes: Some(outcome.bytes),
                slot_count: Some(outcome.slot_count),
                file_exists,
                code: None,
                message: None,
                user_error: None,
                note: command.note.clone(),
            }),
            Err(error) => views.push(ShimCommandView {
                command: command.command.clone(),
                file: spec.file_name(),
                target: target_text,
                prefix,
                status: status::FAILED,
                bytes: None,
                slot_count: None,
                file_exists,
                code: Some(error.kind()),
                message: Some(error.to_string()),
                user_error: Some(error.is_user_error()),
                note: command.note.clone(),
            }),
        }
    }

    let count = |wanted: &str| views.iter().filter(|view| view.status == wanted).count();
    let mut notes = Vec::new();
    if explicit_version && active.as_deref() != Some(version.as_str()) {
        // shim 指向 `current`：指定一个**不是**当前生效版本时，
        // 生成出来的 shim 跑的不是那个版本。这件事必须说出来 —— 它是决策 11 的另一面。
        notes.push("version-not-active");
    }

    Ok(ShimAddView {
        dry_run: args.dry_run,
        tool: tool_id,
        version,
        active_version: active,
        shim_dir: shim_dir.display().to_string(),
        payload_root: payload_root.display().to_string(),
        template: template.display().to_string(),
        template_source: template_source_slug(&template_source),
        created: count(status::CREATED),
        replaced: count(status::REPLACED),
        planned: count(status::PLANNED),
        failed: count(status::FAILED),
        commands: views,
        notes,
    })
}

/// 当前可执行文件所在的目录 —— 找模板的起点。
fn exe_dir() -> Result<PathBuf, ShimCliError> {
    let exe = std::env::current_exe().map_err(|source| {
        ShimCliError::own(
            "no-exe-dir",
            format!("读不到当前可执行文件的位置（{source}），于是不知道该去哪找 shim 模板。"),
        )
    })?;
    exe.parent().map(Path::to_path_buf).ok_or_else(|| {
        ShimCliError::own(
            "no-exe-dir",
            format!(
                "`{}` 没有父目录，于是不知道该去哪找 shim 模板。",
                exe.display()
            ),
        )
    })
}

/// 把载荷根下的相对路径拼成绝对路径。
///
/// `relative` 里写的是 `/`（它是"归档里的形状"，见 `tuoen_core::ShimCommand::relative`），
/// 而拼出来的必须是 Windows 路径：**`PathBuf::push` 不规范化分隔符**
/// （`Path::new(r"C:\a").join("b/c")` 得到的是 `C:\a\b/c`），而一个混着正斜杠的路径
/// 在写进重解析点数据块、或者交给别的工具时都会出问题（决策 47 在 junction 上踩过
/// 同一个坑：所有单独的检查都说它对，但打不开）。所以这里逐段 push。
fn join_relative(root: &Path, relative: &str) -> PathBuf {
    let mut out = root.to_path_buf();
    for part in relative.split(['/', '\\']).filter(|part| !part.is_empty()) {
        out.push(part);
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// shim list
// ─────────────────────────────────────────────────────────────────────────────

/// 跑 `tuoen shim list`。返回退出码。
pub fn run_list(args: &ShimListArgs) -> i32 {
    match list(args) {
        Ok(view) => {
            if args.json {
                crate::print_json(&Envelope::ok("shim.list", &view));
            } else {
                crate::shim_view::print_list_human(&view);
            }
            exit::SUCCESS
        }
        Err(error) => {
            if args.json {
                crate::print_json(&Envelope::err("shim.list", error.code(), error.to_string()));
            } else {
                eprintln!("tuoen: {error}");
            }
            exit::RUNTIME_ERROR
        }
    }
}

/// 列出 shim 目录里的 `.exe`。
///
/// **一个坏文件不会让这条命令失败**，它会被标成 `not-a-shim` —— 那是**发现**，不是错误。
/// 这也是为什么这条命令的退出码不随"有多少条不是我们的"变化。
pub fn list(_args: &ShimListArgs) -> Result<ShimListView, ShimCliError> {
    let store = Store::at_default_location();
    let shim_dir = shim_dir(&store);
    let mut shims = Vec::new();

    match std::fs::read_dir(&shim_dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(file) = path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                // 只列 `.exe`：shim 目录里可能有别的东西（比如编辑器留下的临时文件），
                // 而"我们把什么放进了 PATH"只由 `.exe` 回答。
                if !file.to_ascii_lowercase().ends_with(".exe") {
                    continue;
                }
                shims.push(inspect(&path, file));
            }
        }
        // 目录不存在 = 还没有任何 shim。**这不是错误**：一台新机器就是这样。
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(ShimCliError::own(
                "shim-dir-unreadable",
                format!("读不了 shim 目录 `{}`：{error}", shim_dir.display()),
            ));
        }
    }

    // **必须排序**：目录的枚举顺序是任意的，而 `--json` 必须逐字节稳定（决策 35）。
    shims.sort_by(|a, b| a.file.cmp(&b.file));

    Ok(ShimListView {
        shim_dir: shim_dir.display().to_string(),
        shims,
    })
}

/// 读一个文件，看它是不是我们的 shim、指向哪。
///
/// **任何一种"读不出来"都只是把这一条标成 `not-a-shim`**，而不是让整条命令失败：
/// shim 目录里出现一个不属于我们的文件是**发现**，不是错误。
fn inspect(path: &Path, file: &str) -> ShimEntryView {
    let command = file
        .len()
        .checked_sub(4)
        .map_or(file, |end| &file[..end])
        .to_owned();
    let mut view = ShimEntryView {
        command,
        file: file.to_owned(),
        path: path.display().to_string(),
        bytes: std::fs::metadata(path).map_or(0, |meta| meta.len()),
        status: status::OK,
        problem: None,
        target: None,
        prefix: None,
        target_exists: None,
    };

    if !path.is_file() {
        view.status = status::NOT_A_SHIM;
        view.problem = Some("not-a-file");
        return view;
    }
    let Ok(contents) = std::fs::read(path) else {
        view.status = status::NOT_A_SHIM;
        view.problem = Some("read-failed");
        return view;
    };
    match baked_prefix(&contents) {
        Some(prefix) => {
            view.target = prefix_target(&prefix).map(str::to_owned);
            // 指向一个已经不存在的目标 = 一条坏的 shim。它不会自己报警，
            // 所以 `list` 替它报警。
            view.target_exists = view
                .target
                .as_deref()
                .map(|target| Path::new(target).is_file());
            view.prefix = Some(prefix);
        }
        None => {
            view.status = status::NOT_A_SHIM;
            view.problem = Some("no-slot-magic");
        }
    }
    view
}

/// 文件里有没有槽位魔数。
///
/// 判据与 `tuoen_shim::write_shim` 一致（它也是"看魔数"，而不是"看有没有原始槽位"）：
/// 上一次生成的 shim 槽位是烘过的，**它仍然是我们的东西**，必须允许覆盖与删除。
/// `windows()` 的扫描在这里写一遍是因为 shim crate 里的那个辅助函数是私有的，
/// 而判据本身（[`SLOT_MAGIC`]）是公开的。
fn contains_slot_magic(bytes: &[u8]) -> bool {
    slot_magic_offsets(bytes).next().is_some()
}

/// 文件里**烘着的**前缀（第一个能解出来的槽位）。
///
/// 不能只找魔数就下手：模板里那个"还没烘过"的槽位也有魔数，而它解出来是 `None`；
/// 已经生成的 shim 里，编译器可能把同一个常量数组复制到多处，只有其中一部分是
/// 真的槽位（`tuoen_shim::pristine_slot_offsets` 的文档里有实测数字）。
/// 所以逐处试 `decode_slot`，取第一个成功的。
fn baked_prefix(bytes: &[u8]) -> Option<String> {
    slot_magic_offsets(bytes).find_map(|at| decode_slot(&bytes[at..]))
}

/// 文件里所有魔数出现的位置。
fn slot_magic_offsets(bytes: &[u8]) -> impl Iterator<Item = usize> + '_ {
    bytes
        .windows(SLOT_MAGIC.len())
        .enumerate()
        .filter_map(|(at, window)| (window == SLOT_MAGIC).then_some(at))
}

// ─────────────────────────────────────────────────────────────────────────────
// shim remove
// ─────────────────────────────────────────────────────────────────────────────

/// 跑 `tuoen shim remove`。返回退出码。
pub fn run_remove(args: &ShimRemoveArgs) -> i32 {
    match remove(args) {
        Ok(view)
            if view
                .results
                .iter()
                .all(|entry| entry.status == status::REMOVED) =>
        {
            if args.json {
                crate::print_json(&Envelope::ok("shim.remove", &view));
            } else {
                crate::shim_view::print_remove_human(&view);
            }
            exit::SUCCESS
        }
        Ok(view) => {
            let code = view.first_failure_code().unwrap_or("remove-failed");
            let message = view.failure_summary();
            if args.json {
                crate::print_json(&Envelope::partial("shim.remove", code, message, &view));
            } else {
                crate::shim_view::print_remove_human(&view);
            }
            exit::RUNTIME_ERROR
        }
        Err(error) => {
            if args.json {
                crate::print_json(&Envelope::err(
                    "shim.remove",
                    error.code(),
                    error.to_string(),
                ));
            } else {
                eprintln!("tuoen: {error}");
            }
            exit::RUNTIME_ERROR
        }
    }
}

/// 删掉命令行给的每一个名字。
pub fn remove(args: &ShimRemoveArgs) -> Result<ShimRemoveView, ShimCliError> {
    let store = Store::at_default_location();
    let shim_dir = shim_dir(&store);
    let results: Vec<ShimRemoveEntryView> = args
        .names
        .iter()
        .map(|raw| remove_one(&shim_dir, raw))
        .collect();
    let (siblings_left, tool_id) = siblings_left_behind(&shim_dir, &results);
    Ok(ShimRemoveView {
        shim_dir: shim_dir.display().to_string(),
        results,
        siblings_left,
        tool_id,
    })
}

/// 删完之后，**同一个工具**还有哪几条留在盘上。
///
/// 不对称的来源：`tuoen shim add node` 一次生成 4 条（`node` / `npm` / `npx` /
/// `corepack`），而 `remove` 收的是**命令名**，所以 `remove node` 只删掉 `node.exe`。
/// 这个不对称本身是对的（`remove npm` 必须能只删 npm），但**不说出来**就会让人
/// 以为已经删干净了 —— 那是一个静默的半途状态，正是最该避免的一种。
///
/// 判据有三条，缺一不可：①这个名字属于某个已知工具；②那个工具**有别的**命令；
/// ③那些命令**真的还在盘上**。只按表推断而不管盘上有没有，会在删一个从未生成过的
/// 名字时报出一堆"漏下的兄弟"。
fn siblings_left_behind(
    shim_dir: &Path,
    results: &[ShimRemoveEntryView],
) -> (Vec<String>, Option<String>) {
    let mut tool_id: Option<String> = None;
    let mut left: Vec<String> = Vec::new();
    for entry in results.iter().filter(|e| e.status == status::REMOVED) {
        let name = strip_exe_suffix(entry.name.trim());
        let Some(tool) = tuoen_core::tool_for_command(name) else {
            continue;
        };
        tool_id = Some(tool.to_owned());
        for sibling in tuoen_core::command_names(tool) {
            if sibling.eq_ignore_ascii_case(name) {
                continue;
            }
            // **盘上真的还在**才算。表里有、但没生成过的命令不能报。
            if shim_dir.join(format!("{sibling}.exe")).is_file() {
                left.push(sibling.to_owned());
            }
        }
    }
    left.sort();
    left.dedup();
    let tool_id = if left.is_empty() { None } else { tool_id };
    (left, tool_id)
}

/// 删一个名字。
///
/// 四种拒绝，每一种都**什么都不删**：
/// * 名字不合法（分隔符 / 冒号 / 控制字符 / 保留设备名）—— 它不是 shim 目录里的一个文件名；
/// * 文件不存在；
/// * 文件存在但**不是我们的 shim**（没有槽位魔数）；
/// * 读不了它（**读不了就等于确认不了它是我们的**，于是不删）。
fn remove_one(shim_dir: &Path, raw: &str) -> ShimRemoveEntryView {
    let name = strip_exe_suffix(raw.trim());
    if let Err(error) = check_shim_name(name) {
        return ShimRemoveEntryView {
            name: raw.to_owned(),
            file: None,
            path: None,
            status: status::BAD_NAME,
            bytes: None,
            code: Some(error.kind()),
            message: Some(error.to_string()),
            user_error: Some(error.is_user_error()),
        };
    }

    let file = format!("{name}.exe");
    let path = shim_dir.join(&file);
    let path_text = path.display().to_string();
    if !path.exists() {
        return ShimRemoveEntryView {
            name: raw.to_owned(),
            file: Some(file),
            path: Some(path_text.clone()),
            status: status::NOT_FOUND,
            bytes: None,
            code: Some("not-found"),
            message: Some(format!(
                "shim `{name}` 不存在（找的是 `{path_text}`）。用 `tuoen shim list` 看现在有哪些。"
            )),
            user_error: Some(true),
        };
    }

    let Ok(contents) = std::fs::read(&path) else {
        return ShimRemoveEntryView {
            name: raw.to_owned(),
            file: Some(file),
            path: Some(path_text),
            status: status::FAILED,
            bytes: None,
            code: Some("unreadable"),
            message: Some(format!(
                "读不了 `{}` —— 在确认它是 tuoen 的 shim 之前不会删它。",
                path.display()
            )),
            user_error: Some(false),
        };
    };
    if !contains_slot_magic(&contents) {
        return ShimRemoveEntryView {
            name: raw.to_owned(),
            file: Some(file),
            path: Some(path_text),
            status: status::NOT_A_SHIM,
            bytes: Some(contents.len() as u64),
            code: Some("not-a-shim"),
            message: Some(format!(
                "`{}` 不是 tuoen 生成的 shim（文件里没有槽位魔数），**没有删除**。\n\
                 这个命令只会删自己生成的东西 —— 一个不属于我们的文件出现在 shim 目录里，\
                 多半意味着别的东西也在这条 PATH 上放文件。",
                path.display()
            )),
            user_error: Some(true),
        };
    }

    match std::fs::remove_file(&path) {
        Ok(()) => ShimRemoveEntryView {
            name: raw.to_owned(),
            file: Some(file),
            path: Some(path_text),
            status: status::REMOVED,
            bytes: Some(contents.len() as u64),
            code: None,
            message: None,
            user_error: None,
        },
        Err(source) => ShimRemoveEntryView {
            name: raw.to_owned(),
            file: Some(file),
            path: Some(path_text),
            status: status::FAILED,
            bytes: Some(contents.len() as u64),
            code: Some("remove-failed"),
            message: Some(format!("删不掉 `{}`：{source}", path.display())),
            user_error: Some(false),
        },
    }
}

/// 把用户给的命令名规范成"不带扩展名"的名字。
///
/// 收 `node` 也收 `node.exe`：我们所有的 shim 都是 `.exe`，所以结尾的 `.exe`
/// **只可能是扩展名**，不可能是名字的一部分（那会是 `node.exe.exe`）。
fn strip_exe_suffix(name: &str) -> &str {
    let bytes = name.as_bytes();
    // `>= 4` 而不是 `> 4`：光一个 `.exe` 切完是空的，而空名字正好被下面的
    // 名字校验拒掉 —— 那本来也不是一个名字。留成 `> 4` 的话它会变成一个
    // 要找 `.exe.exe` 的怪查询，报出来的错跟用户敲的东西对不上。
    if bytes.len() >= 4 && bytes[bytes.len() - 4..].eq_ignore_ascii_case(b".exe") {
        // 被切掉的四个字节是 ASCII，所以这里一定是字符边界。
        &name[..name.len() - 4]
    } else {
        name
    }
}

/// 校验一个名字能不能当作 shim 的文件名。
///
/// **名字安全性全仓只有一份定义**（`tuoen_shim::validate_name`）。这里直接调用它，
/// 而不是自己抄一份规则、也不是构造一个假 [`ShimSpec`] 去侧面触发校验 ——
/// 两份定义必然漂移，而侧面触发让"这条规则是什么"变得看不出来。
///
/// `remove` 用它挡在删除之前：`..\..\Windows\System32\calc` 这样的名字不该被拼进路径。
fn check_shim_name(name: &str) -> Result<(), ShimError> {
    tuoen_shim::validate_name(name)
}

// ─────────────────────────────────────────────────────────────────────────────
// shim path
// ─────────────────────────────────────────────────────────────────────────────

/// 跑 `tuoen shim path`。返回退出码。
///
/// **这条命令一字节都不改，也不做任何 I/O** —— 它只推一个路径。
/// 目录还不存在时照样打印它（脚本要的是"应该把东西放哪"，而那时 `shim add` 会创建它）。
pub fn run_path(args: &ShimPathArgs) -> i32 {
    let view = path(args);
    if args.json {
        crate::print_json(&Envelope::ok("shim.path", &view));
    } else {
        crate::shim_view::print_path_human(&view);
    }
    exit::SUCCESS
}

/// shim 目录的绝对路径。
#[must_use]
pub fn path(_args: &ShimPathArgs) -> ShimPathView {
    let store = Store::at_default_location();
    ShimPathView {
        path: shim_dir(&store).display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_relative_normalises_separators_to_the_platform_ones() {
        // **这条断言是决策 47 的教训**：`PathBuf::push` 不规范化分隔符，
        // 于是 `C:\a` + `b/c` 会得到 `C:\a\b/c` —— 一个所有检查都说它对、
        // 但交给内核时打不开的路径。
        let root = Path::new(r"C:\store\node\current");
        assert_eq!(
            join_relative(root, "node_modules/npm/bin/npm-cli.js"),
            root.join("node_modules")
                .join("npm")
                .join("bin")
                .join("npm-cli.js")
        );
        assert_eq!(join_relative(root, "node.exe"), root.join("node.exe"));
        // 反斜杠也认（两张表里写的是 `/`，但拼接函数不该只认一种）。
        assert_eq!(
            join_relative(root, r"node_modules\npm\bin\npm-cli.js"),
            root.join("node_modules")
                .join("npm")
                .join("bin")
                .join("npm-cli.js")
        );
        // 空段被跳掉，不会产生 `current\\node.exe`。
        assert_eq!(join_relative(root, "/node.exe"), root.join("node.exe"));
    }

    #[test]
    fn the_exe_suffix_is_only_stripped_when_it_is_really_the_extension() {
        assert_eq!(strip_exe_suffix("node"), "node");
        assert_eq!(strip_exe_suffix("node.exe"), "node");
        assert_eq!(strip_exe_suffix("node.EXE"), "node");
        assert_eq!(strip_exe_suffix("pip3.12"), "pip3.12");
        // 光一个 `.exe` 不是名字 —— 切完是空的，交给名字校验去拒。
        assert_eq!(strip_exe_suffix(".exe"), "");
        // 短名字不能被切坏（`len() >= 4` 这条守卫）。
        assert_eq!(strip_exe_suffix("exe"), "exe");
    }

    #[test]
    fn unsafe_names_are_refused_by_the_shim_crates_own_rules() {
        // 这里钉的是"我们**真的**问了 shim crate"，而不是自己写了一份校验：
        // 错误码必须是它那一套 `BadName` 的 `kind()`。
        for name in [
            "", "..", "../evil", r"a\b", "a/b", "a:b", "CON", "COM1", "name.",
        ] {
            let error = check_shim_name(name).expect_err(&format!("`{name}` 应当被拒"));
            assert_eq!(
                error.kind(),
                "bad-name",
                "`{name}` 的拒绝理由来自 shim crate"
            );
        }
    }

    #[test]
    fn safe_names_pass() {
        for name in ["node", "npm", "npx", "corepack", "pip3.12", "console"] {
            assert!(check_shim_name(name).is_ok(), "`{name}` 应当通过");
        }
    }

    #[test]
    fn the_slot_scan_finds_a_baked_prefix_and_ignores_the_pristine_one() {
        // 造一个"前半段是模板、后半段烘过"的文件：**模板里的那个槽位也有魔数**，
        // 但它解不出来（长度字段是 0）。扫描必须跳过它、找到真正烘过的那一个。
        let mut bytes = tuoen_shim::baked_slot().to_vec();
        let baked =
            tuoen_shim::encode_slot(r#""C:\x\node.exe" "C:\x\npm-cli.js""#).expect("encode");
        bytes.extend_from_slice(&baked);

        assert!(contains_slot_magic(&bytes), "两处魔数都应当被看见");
        assert_eq!(
            baked_prefix(&bytes).as_deref(),
            Some(r#""C:\x\node.exe" "C:\x\npm-cli.js""#),
            "必须跳过那个解不出来的原始槽位"
        );
        assert_eq!(
            prefix_target(&baked_prefix(&bytes).expect("prefix")),
            Some(r"C:\x\node.exe")
        );
    }

    #[test]
    fn a_file_without_the_magic_is_not_ours() {
        assert!(!contains_slot_magic(b"just some bytes"));
        assert_eq!(baked_prefix(b"just some bytes"), None);
        assert_eq!(baked_prefix(&[]), None);
        assert_eq!(baked_prefix(&SLOT_MAGIC), None, "半截魔数不算");
    }

    /// 一条"删成功了"的结果，用来喂 [`siblings_left_behind`]。
    fn removed(name: &str) -> ShimRemoveEntryView {
        ShimRemoveEntryView {
            name: name.to_owned(),
            file: Some(format!("{name}.exe")),
            path: None,
            status: status::REMOVED,
            bytes: Some(1),
            code: None,
            message: None,
            user_error: None,
        }
    }

    /// 一条"没删成"的结果。
    fn not_found(name: &str) -> ShimRemoveEntryView {
        ShimRemoveEntryView {
            name: name.to_owned(),
            file: Some(format!("{name}.exe")),
            path: None,
            status: status::NOT_FOUND,
            bytes: None,
            code: Some("not-found"),
            message: None,
            user_error: Some(true),
        }
    }

    #[test]
    fn removing_a_tool_name_reports_the_siblings_it_left_behind() {
        // **这条是实测抓出来的**：`tuoen shim add node` 生成 4 条，而
        // `tuoen shim remove node` 只删 `node.exe` —— 因为 remove 收的是**命令名**。
        // 不对称本身合理，静默才是问题：删完必须说清还剩哪几条。
        let dir = tuoen_platform::test_support::TempDir::new("shim-siblings");
        for name in ["node", "npm", "npx", "corepack"] {
            std::fs::write(dir.path().join(format!("{name}.exe")), b"x").expect("write");
        }
        // 删掉 `node.exe`，模拟 remove 已经跑完的那一刻。
        std::fs::remove_file(dir.path().join("node.exe")).expect("remove");

        let (left, tool) = siblings_left_behind(dir.path(), &[removed("node")]);
        assert_eq!(left, vec!["corepack", "npm", "npx"], "漏下的兄弟按名字排序");
        assert_eq!(tool.as_deref(), Some("node"));
    }

    #[test]
    fn siblings_are_reported_only_when_they_are_really_on_disk() {
        // 判据里最容易漏的一条：**表里有、盘上没有**的命令不算"漏下的兄弟"。
        // 只按表推断的话，删掉一个从没生成过全集的工具会报出一堆不存在的东西。
        let dir = tuoen_platform::test_support::TempDir::new("shim-siblings-absent");
        std::fs::write(dir.path().join("npm.exe"), b"x").expect("write");

        let (left, tool) = siblings_left_behind(dir.path(), &[removed("node")]);
        assert_eq!(left, vec!["npm"], "盘上只有 npm");
        assert_eq!(tool.as_deref(), Some("node"));

        // 一条都不剩的时候**不给提示**（`None`），否则删干净了还要挨一句注意。
        std::fs::remove_file(dir.path().join("npm.exe")).expect("remove");
        let (left, tool) = siblings_left_behind(dir.path(), &[removed("node")]);
        assert!(left.is_empty());
        assert!(tool.is_none());
    }

    #[test]
    fn a_name_we_did_not_remove_never_produces_a_hint() {
        // **只对真的删成功的那几个名字说话。** 没删成的（不存在、名字不合法……）
        // 一条兄弟都不该报 —— 否则"什么都没删掉"也会跟一句"还剩 N 条"。
        let dir = tuoen_platform::test_support::TempDir::new("shim-siblings-failed");
        std::fs::write(dir.path().join("npm.exe"), b"x").expect("write");

        let (left, tool) = siblings_left_behind(dir.path(), &[not_found("node")]);
        assert!(left.is_empty(), "没删成的名字不该触发兄弟提示");
        assert!(tool.is_none());
    }

    #[test]
    fn a_command_name_that_belongs_to_no_tool_produces_no_hint() {
        // 我们自己的工具之外的命令名（将来别的工具也会往这个目录放东西）
        // 反查不到工具，于是无从知道"兄弟"是什么 —— 那就什么都不说。
        let dir = tuoen_platform::test_support::TempDir::new("shim-siblings-unknown");
        std::fs::write(dir.path().join("node.exe"), b"x").expect("write");

        let (left, tool) = siblings_left_behind(dir.path(), &[removed("somethingelse")]);
        assert!(left.is_empty());
        assert!(tool.is_none());
    }
}
