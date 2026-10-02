//! `tuoen install` / `use` / `uninstall` 的**执行**部分。
//!
//! 参数形状在 [`crate::manage`]，`--json` 形状在 [`crate::manage_view`]；
//! 这里只做编排：把目录里的一条 recipe 变成一个真的能跑的版本目录。
//!
//! ## 安装链路（票据 #6 的出口）
//!
//! ```text
//! 内置目录 ──resolve──▶ ResolvedRecipe ──gate──▶ 允许？
//!                                              │
//!                     download（源优选 + 缓存 + SHA256 校验）
//!                                              │
//!                     archive（三层防线 + 同卷临时目录 + 一次 rename）
//!                                              │
//!                     store.adopt_payload（搬进存储 + 写记录）
//!                                              │
//!                     可选：store.activate（一次 IOCTL 翻转 current）
//! ```
//!
//! ## 三条必须守住的性质
//!
//! 1. **哈希来自内置目录，不来自下载源。** 从你正在下载的那台服务器上取哈希
//!    等于没有校验 —— 能改制品的人也能改哈希。所以这里调的是
//!    `recipe.checksum.value`，**任何"去上游读 SHASUMS256.txt"的想法都是错的**
//!    （真机验收例子 `crates/download/examples/fetch_probe.rs` 那么做是为了
//!    **证明**目录里的哈希与上游一致，那是取证，不是安装路径）。
//! 2. **解压先在存储根下的 `.incoming/` 里做，再一次 `rename` 进 `versions/`。**
//!    同一个卷 → 改名是原子的 → 用户永远不会看到一个装了一半的版本目录。
//! 3. **装完不激活**（决策 49）。`--use` 才翻 `current`，而且它是一次独立、可观察的
//!    IOCTL（决策 46）。

use std::time::Duration;

use tuoen_archive::{InstallRequest, TarCli, install_archive};
use tuoen_download::{Artifact, Cache, FetchOptions, HttpTransport, fetch};
use tuoen_manifest::{Catalog, Redistribution, gate, resolve};
use tuoen_platform::{RealFileSystem, SystemProcessRunner};
use tuoen_store::{InstallRecord, Store, UninstallOptions, UninstallOutcome};

use crate::manage::{InstallArgs, UninstallArgs, UseArgs};
use crate::manage_view::{
    InstallPlanView, InstallResultView, InstallView, RepointView, UninstallView, UseView,
};
use crate::{envelope::Envelope, exit};

/// 编排层的一条错误。
///
/// **`code` 是稳定的机器可读字符串**（进 `--json`），`message` 是中文。
/// 两者分开的理由与 `envelope` 那边一样：中文消息可以改，错误码不能。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManageError {
    pub code: &'static str,
    pub message: String,
}

impl ManageError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ManageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// install
// ─────────────────────────────────────────────────────────────────────────────

/// 跑 `tuoen install`。返回退出码。
pub fn run_install(args: &InstallArgs) -> i32 {
    match install(args) {
        Ok(view) => {
            if args.json {
                crate::print_json(&Envelope::ok("install", &view));
            } else {
                print_install_human(&view);
            }
            exit::SUCCESS
        }
        Err(error) => {
            if args.json {
                crate::print_json(&Envelope::err("install", error.code, error.message));
            } else {
                eprintln!("tuoen: {}", error.message);
            }
            exit::RUNTIME_ERROR
        }
    }
}

/// `install` 的全部逻辑，与输出方式无关 —— 这样测试能直接看返回值。
pub fn install(args: &InstallArgs) -> Result<InstallView, ManageError> {
    let (tool_name, version) = crate::manage::split_spec(&args.spec);
    if tool_name.is_empty() {
        return Err(ManageError::new(
            "empty-tool",
            "没有给工具名。用法：`tuoen install node` 或 `tuoen install node@24.19.0`。",
        ));
    }
    // `node@` 与 `node` 是两件事：前者是"你写了 @ 但忘了版本"，后者是"给我最新的"。
    // 把两者混成一种会让一次手误静默装上一个最新版。
    if matches!(&version, Some(v) if v.is_empty()) {
        return Err(ManageError::new(
            "empty-version",
            format!(
                "`{tool_name}@` 后面没有版本号。想去掉 `@` 装最新版，或者补上版本：`{tool_name}@24`。"
            ),
        ));
    }

    let catalog = load_catalog()?;

    // **先查工具是否存在**，否则"工具不存在"会被报成"没有适用的 recipe"。
    let Some(tool) = tuoen_manifest::find_tool(&catalog, &tool_name) else {
        return Err(ManageError::new(
            "unknown-tool",
            format!(
                "目录里没有工具 `{tool_name}`；已知：{}",
                catalog
                    .tools
                    .iter()
                    .map(|t| t.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    };

    // **许可证门禁必须在解析 recipe 之前看**（与 `catalog check` 同一条教训）：
    // 不可再分发的条目本来就（正确地）没有 recipe，先解析会报成"没有适用的 recipe"，
    // 那是把"我们不许可你装"说成了"我们不知道去哪装"。
    if tool.licence.redistribution == Redistribution::Prohibited {
        return Err(ManageError::new(
            "licence-prohibited",
            format!(
                "{} 不可再分发，tuoen 不会下载或安装它。\n原因：{}",
                tool.display_name, tool.licence.notes
            ),
        ));
    }

    // 没给版本时取目录里**最新**的那个可安装版本（自然比较，不是字符串比较）。
    let constraint = match version {
        Some(v) => v,
        None => newest_installable(&catalog, &tool.id, &args.platform).ok_or_else(|| {
            ManageError::new(
                "no-installable-version",
                format!(
                    "{} 在 {} 上没有可安装的版本。用 `tuoen catalog show {}` 看它有什么。",
                    tool.id, args.platform, tool.id
                ),
            )
        })?,
    };

    let recipe = resolve(&catalog, &tool.id, &constraint, &args.platform).map_err(|err| {
        // **把平台包进去。** `ResolveError` 自己只说"没有匹配约束的 recipe"，
        // 而用户敲的是 `tuoen install node@21` —— 他需要知道我们是在
        // `windows-x64` 上找的，否则"版本不存在"与"这个平台不支持"就分不开。
        ManageError::new(
            "unresolved",
            format!(
                "在 {} 上找不出 `{}@{}`：{err}",
                args.platform, tool.id, constraint
            ),
        )
    })?;

    match gate(&recipe) {
        tuoen_manifest::GateVerdict::Allowed { .. } => {}
        tuoen_manifest::GateVerdict::Rejected { reason } => {
            return Err(ManageError::new("licence-rejected", reason));
        }
    }

    let store = Store::at_default_location();
    let installed_to = store.version_dir(&recipe.tool_id, &recipe.version);
    let current_link = store.current_link(&recipe.tool_id);
    let already_installed =
        tuoen_store::find_version(&store, &recipe.tool_id, &recipe.version).is_some();

    let plan = InstallPlanView::new(
        &recipe,
        installed_to.display().to_string(),
        current_link.display().to_string(),
        already_installed,
        args.r#use,
    );

    if args.dry_run {
        return Ok(InstallView {
            dry_run: true,
            plan,
            result: None,
        });
    }

    if already_installed {
        return Err(ManageError::new(
            "already-installed",
            format!(
                "{} {} 已经装过了（{}）。\n\
                 想让它生效：`tuoen use {} {}`\n\
                 想重装：先 `tuoen uninstall {} {}`",
                recipe.display_name,
                recipe.version,
                installed_to.display(),
                recipe.tool_id,
                recipe.version,
                recipe.tool_id,
                recipe.version
            ),
        ));
    }

    // ── 下载 ────────────────────────────────────────────────────────────────
    let artifact = Artifact::new(
        file_name_of(&recipe.url),
        &recipe.tool_id,
        &recipe.url,
        &recipe.checksum.algorithm,
        &recipe.checksum.value,
    );
    let options = FetchOptions {
        cache: Cache::at_default_location(),
        // 单制品 35 MB，600 秒够；比 archive 的默认超时短是刻意的：
        // 下载卡住时应该早点换源，而不是等到解压那份超时。
        timeout: Duration::from_secs(300),
        ..FetchOptions::default()
    };
    let transport = HttpTransport::probe();
    let report = fetch(&transport, &artifact, &options).map_err(|err| {
        ManageError::new(
            crate::manage_view::download_error_code(&err),
            format!(
                "下载 {} {} 失败：{err}",
                recipe.display_name, recipe.version
            ),
        )
    })?;

    // ── 解压（在存储根下的 .incoming 里，同卷 → 之后一次 rename 进存储） ──
    let incoming = store.root().join(".incoming").join(&recipe.tool_id);
    let tar = TarCli::probe().map_err(|err| {
        ManageError::new(
            err.kind(),
            format!(
                "找不到能解压的系统 `tar.exe`。{err}\n\
                 它随 Windows 10 1803+ 自带（`%SystemRoot%\\System32\\tar.exe`）。"
            ),
        )
    })?;
    let request = InstallRequest::new(&report.path, &incoming, &recipe.version)
        .stripping(recipe.layout.strip_components as usize);
    let outcome =
        install_archive(&SystemProcessRunner, &RealFileSystem, &tar, &request).map_err(|err| {
            ManageError::new(
                err.kind(),
                format!(
                    "解压 {} {} 失败：{err}",
                    recipe.display_name, recipe.version
                ),
            )
        })?;

    // ── 搬进存储（store 自己会再查一次"目标是否已存在"与"目标是不是重解析点"） ──
    let mut record = InstallRecord {
        schema_version: tuoen_store::RECORD_SCHEMA_VERSION,
        tool: recipe.tool_id.clone(),
        version: recipe.version.clone(),
        display_name: recipe.display_name.clone(),
        source_id: report.source_id.clone(),
        url: report.source_url.clone(),
        sha256: recipe.checksum.value.clone(),
        archive: recipe.archive.as_str().to_owned(),
        installed_at: tuoen_store::now_rfc3339(),
        // 这两个字段由 `adopt_payload` 按落盘结果**覆盖** —— 这里填 0 是诚实的：
        // 在搬家发生之前，谁也不知道真实数字。
        payload_files: 0,
        payload_bytes: 0,
        layout: recipe.layout.clone(),
    };
    let adopted = tuoen_store::adopt_payload(
        &store,
        &recipe.tool_id,
        &recipe.version,
        &outcome.installed_to,
        &mut record,
    )
    .map_err(|err| {
        // 搬家失败时把解压产物清掉 —— 否则 `.incoming` 会越积越多。
        // **清不掉也不掩盖原错误**：原错误才是用户要处理的。
        let _ = tuoen_archive::remove_tree(&outcome.installed_to, false);
        ManageError::new(err.kind(), err.to_string())
    })?;

    // ── 可选：激活 ──────────────────────────────────────────────────────────
    let repoint = if args.r#use {
        Some(
            tuoen_store::activate(&store, &recipe.tool_id, &recipe.version)
                .map_err(|err| ManageError::new(err.kind(), err.to_string()))?,
        )
    } else {
        None
    };

    Ok(InstallView {
        dry_run: false,
        plan,
        result: Some(InstallResultView::new(
            &report,
            outcome.listed_entries,
            &outcome.audit,
            &adopted,
            repoint.as_ref(),
        )),
    })
}

/// 目录里最新的可安装版本。
///
/// **按自然比较排序**，不是按目录里的书写顺序：书写顺序是人维护的，
/// 而"最新的版本"是用户能观察到的性质 —— 让它们一致靠的是约束人，
/// 而让它们一致靠**代码**只需要一个比较函数。
fn newest_installable(catalog: &Catalog, tool: &str, platform: &str) -> Option<String> {
    let mut installable: Vec<String> =
        tuoen_manifest::installable_versions(catalog, tool, platform)
            .into_iter()
            .filter(|(_, redistribution)| redistribution.installable())
            .map(|(version, _)| version)
            .collect();
    installable.sort_by(|a, b| tuoen_store::compare_versions(b, a));
    installable.into_iter().next()
}

/// 加载种子目录。失败时把**全部**校验问题打出来并返回一条运行期错误。
///
/// `pub(crate)` 是因为 `shim_cmd` 也要用它：**"内置目录无效是一个 bug"这句话
/// 只该有一份**，而两个模块各写一遍的结果是某一天其中一个开始说别的话。
pub(crate) fn load_catalog() -> Result<Catalog, ManageError> {
    tuoen_manifest::load_seed().map_err(|err| {
        ManageError::new(
            "seed-catalog-invalid",
            format!("内置种子目录无效 —— 这是一个 bug，请报告：\n{err}"),
        )
    })
}

/// 从一个 URL 里取文件名。取不到就用一个稳定的兜底名。
///
/// 只用它做缓存里的可读名字（真正的身份是哈希），所以这里**允许猜** ——
/// 但猜错不能导致崩溃或空名字。
fn file_name_of(url: &str) -> String {
    let without_query = url.split(['?', '#']).next().unwrap_or(url);
    let name = without_query
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(without_query);
    if name.is_empty() {
        "artifact".to_owned()
    } else {
        name.to_owned()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// use
// ─────────────────────────────────────────────────────────────────────────────

/// 跑 `tuoen use`。返回退出码。
pub fn run_use(args: &UseArgs) -> i32 {
    match use_version(args) {
        Ok(view) => {
            if args.json {
                crate::print_json(&Envelope::ok("use", &view));
            } else {
                print_use_human(&view);
            }
            exit::SUCCESS
        }
        Err(error) => {
            if args.json {
                crate::print_json(&Envelope::err("use", error.code, error.message));
            } else {
                eprintln!("tuoen: {}", error.message);
            }
            exit::RUNTIME_ERROR
        }
    }
}

/// 激活一个已安装的版本。
pub fn use_version(args: &UseArgs) -> Result<UseView, ManageError> {
    let catalog = load_catalog()?;
    // 名字要归一化到目录里的 id：用户可能敲的是别名（`java` / `nodejs`），
    // 而存储里的目录名必须是稳定的 id，否则同一个工具会有两个存储目录。
    let tool_id = match tuoen_manifest::find_tool(&catalog, &args.tool) {
        Some(tool) => tool.id.clone(),
        None => {
            // **已知工具但没装过** 与 **我们不认识的工具** 是两种错误。
            // 报成同一种会让用户去查文档，而其实他只需要 `tuoen install`。
            return Err(ManageError::new(
                "unknown-tool",
                format!("目录里没有工具 `{}`。", args.tool),
            ));
        }
    };

    let store = Store::at_default_location();
    let previous = tuoen_store::active_version(&store, &tool_id);

    // 先自己确认版本真的在 —— 这样"没装"这条错误的措辞能带上怎么办，
    // 而不是从 store 里冒出一句只有路径的话。
    if tuoen_store::find_version(&store, &tool_id, &args.version).is_none() {
        let installed: Vec<String> = tuoen_store::installed_versions(&store, &tool_id)
            .into_iter()
            .map(|v| v.version)
            .collect();
        let hint = if installed.is_empty() {
            format!(
                "`{tool_id}` 还没有装过任何版本。先 `tuoen install {tool_id}@{}`。",
                args.version
            )
        } else {
            format!("已安装的版本：{}", installed.join(", "))
        };
        return Err(ManageError::new(
            "not-installed",
            format!("`{tool_id}` 没有装过版本 `{}`。{hint}", args.version),
        ));
    }

    let repoint = tuoen_store::activate(&store, &tool_id, &args.version)
        .map_err(|err| ManageError::new(err.kind(), err.to_string()))?;

    let target = store
        .version_dir(&tool_id, &args.version)
        .display()
        .to_string();
    Ok(UseView {
        tool: tool_id,
        version: args.version.clone(),
        link: store.current_link(&args.tool).display().to_string(),
        target,
        previous,
        repoint: RepointView::from(&repoint),
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// uninstall
// ─────────────────────────────────────────────────────────────────────────────

/// 跑 `tuoen uninstall`。返回退出码。
pub fn run_uninstall(args: &UninstallArgs) -> i32 {
    match uninstall_version(args) {
        Ok(view) => {
            if args.json {
                crate::print_json(&Envelope::ok("uninstall", &view));
            } else {
                print_uninstall_human(&view);
            }
            exit::SUCCESS
        }
        Err(error) => {
            if args.json {
                crate::print_json(&Envelope::err("uninstall", error.code, error.message));
            } else {
                eprintln!("tuoen: {}", error.message);
            }
            exit::RUNTIME_ERROR
        }
    }
}

/// 删掉一个已安装的版本。
pub fn uninstall_version(args: &UninstallArgs) -> Result<UninstallView, ManageError> {
    let catalog = load_catalog()?;
    let tool_id = match tuoen_manifest::find_tool(&catalog, &args.tool) {
        Some(tool) => tool.id.clone(),
        None => {
            return Err(ManageError::new(
                "unknown-tool",
                format!("目录里没有工具 `{}`。", args.tool),
            ));
        }
    };

    let store = Store::at_default_location();
    let outcome: UninstallOutcome = tuoen_store::uninstall(
        &store,
        &tool_id,
        &args.version,
        UninstallOptions {
            force_active: args.force,
        },
    )
    .map_err(|err| ManageError::new(err.kind(), err.to_string()))?;

    Ok(UninstallView::new(&tool_id, &args.version, &outcome))
}

// ─────────────────────────────────────────────────────────────────────────────
// 人类输出（中文优先）
// ─────────────────────────────────────────────────────────────────────────────

fn print_install_human(view: &InstallView) {
    let plan = &view.plan;
    if view.dry_run {
        println!("（演练 —— 什么都没有下载、什么都没有写）");
        println!();
    }
    println!("{} {}", plan.display_name, plan.version);
    println!("平台：{}", plan.platform);
    println!("许可证：{} —— {}", plan.licence_name, plan.redistribution);
    println!();
    // **叫"上游地址"而不是"下载地址"。** 这是目录里记的**官方**地址，
    // 而真下载多半会走镜像（下面结果那一段会给出**实际服务了这次下载**的 URL）。
    // 两者都叫"下载地址"会让用户以为我们看到的是同一个东西 ——
    // 而在本机实测里它们一个是 `nodejs.org`、一个是 `cdn.npmmirror.com`。
    println!("上游地址：{}", plan.url);
    println!("sha256：{}", plan.sha256);
    println!("归档：{}", plan.archive);
    println!("装到：{}", plan.installed_to);
    println!("解压：剥 {} 层顶层目录", plan.layout.strip_components);
    if !plan.layout.bin.is_empty() {
        let commands: Vec<&str> = plan.layout.bin.keys().map(String::as_str).collect();
        println!("命令：{}", commands.join(", "));
    } else {
        println!("命令：**这个 recipe 没有暴露任何命令** —— 装完敲不到东西。");
    }
    println!("当前版本链接：{}", plan.current_link);

    if let Some(result) = &view.result {
        println!();
        println!(
            "实际下载：{}（{} 字节，源 `{}`）",
            result.url, result.bytes, result.source
        );
        if result.from_cache {
            println!("          来自缓存（没有产生网络流量）");
        }
        // 走过弯路时必须说 —— 否则"为什么这次慢"永远查不出来。
        if result.attempts.is_empty() {
            if result.from_cache {
                println!("          缓存命中，所以这次一个源都没碰。");
            }
        } else {
            println!("走过的源（按顺序）：");
            for attempt in &result.attempts {
                let mark = if attempt.ok { "✓" } else { "✗" };
                let why = match (&attempt.code, &attempt.message) {
                    (Some(code), Some(message)) => format!("  [{code}] {message}"),
                    (Some(code), None) => format!("  [{code}]"),
                    _ => String::new(),
                };
                println!("  {mark} {} {}ms{}", attempt.source, attempt.millis, why);
            }
        }
        println!(
            "解压：归档内 {} 个条目 → 落盘 {} 个文件、{} 字节",
            result.listed_entries, result.files, result.payload_bytes
        );
        println!("装到：{}", plan.installed_to);

        match &result.repoint {
            Some(repoint) => {
                println!();
                match repoint.outcome {
                    "created" => println!("✓ 已激活（{} 第一次指向这个版本）", plan.current_link),
                    "replaced" => println!(
                        "✓ 已激活（{} 的指向被就地替换 —— 一次操作，没有中间状态）",
                        plan.current_link
                    ),
                    _ => {
                        println!("⚠ 已激活，但用了降级手段：");
                        if let Some(reason) = &repoint.degraded_reason {
                            println!("  {reason}");
                        }
                    }
                }
            }
            None => {
                println!();
                println!("**装好了，但还没生效。**");
                if let Some(next) = &plan.next_command {
                    println!("让它生效：{next}");
                }
            }
        }
    } else if plan.already_installed {
        println!();
        println!("（这个版本已经装过了）");
    } else if let Some(next) = &plan.next_command {
        println!();
        println!("演练结束。真装之后要生效还得敲：{next}");
    }
}

fn print_use_human(view: &UseView) {
    println!(
        "{} {} {}",
        view.tool,
        view.version,
        if view.previous.as_deref() == Some(view.version.as_str()) {
            "（本来就是它）"
        } else {
            "已激活"
        }
    );
    if let Some(previous) = &view.previous
        && previous != &view.version
    {
        println!("从 {} 切过来。", previous);
    }
    println!("链接：{}", view.link);
    println!("指向：{}", view.target);
    match view.repoint.outcome {
        "created" => println!("方式：新建（这个工具此前没有生效版本）"),
        "replaced" => println!(
            "方式：就地替换重解析数据 —— **一次操作**，`{}` 在任何时刻都是可解析的。",
            view.link
        ),
        _ => {
            println!("方式：**降级**（不是原子的）");
            if let Some(reason) = &view.repoint.degraded_reason {
                println!("原因：{reason}");
            }
        }
    }
    println!();
    println!("注意：这条命令**没有改 PATH，也没有改任何 shim** —— 它只翻转了一个链接。");
}

fn print_uninstall_human(view: &UninstallView) {
    println!("已删除 {} {}。", view.tool, view.version);
    println!(
        "释放 {} 个文件、{} 字节。",
        view.freed_files, view.freed_bytes
    );
    if view.was_active {
        println!();
        println!(
            "注意：删掉的正是当时生效的版本，`current` 已经先摘掉了 —— \
             现在这个工具没有生效版本。"
        );
        println!(
            "要选一个：`tuoen list` 看还剩哪些，然后 `tuoen use {} <版本>`。",
            view.tool
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 测试
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_name_of_survives_query_strings_and_trailing_slashes() {
        assert_eq!(
            file_name_of("https://nodejs.org/dist/v24.19.0/node-v24.19.0-win-x64.zip"),
            "node-v24.19.0-win-x64.zip"
        );
        assert_eq!(file_name_of("https://api.adoptium.net/x?page=1#frag"), "x");
        // URL 以斜杠结尾时 rsplit 给出空串 —— 必须兜底，不能产生空文件名。
        assert_eq!(file_name_of("https://example.invalid/dir/"), "artifact");
        assert_eq!(file_name_of(""), "artifact");
    }

    #[test]
    fn file_name_of_keeps_dots_in_versions() {
        // Temurin 的真实文件名带 `+`，而 `+` 在 URL 里可能是 `%2B`。
        assert_eq!(
            file_name_of(
                "https://github.com/adoptium/temurin21-binaries/releases/download/jdk-21.0.12.1%2B1/OpenJDK21U-jdk_x64_windows_hotspot_21.0.12.1_1.zip"
            ),
            "OpenJDK21U-jdk_x64_windows_hotspot_21.0.12.1_1.zip"
        );
    }

    #[test]
    fn newest_installable_picks_by_natural_order_not_catalog_order() {
        // 目录里**故意**把 24.9.0 写在 24.21.0 前面 —— 如果实现按书写顺序取，
        // 它会装 24.9.0（一个更旧的版本），而用户说的是"给我最新的"。
        let text = r#"
schema_version = 1
name = "t"

[[tool]]
id = "x"
display_name = "X"
  [tool.licence]
  redistribution = "allowed"
  spdx_or_name = "MIT"

  [[tool.recipe]]
  version = "24.9.0"
  platform = "windows-x64"
  url = "https://example.invalid/x-{version}.zip"
  archive = "zip"
  checksum = { algorithm = "sha256", value = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
  layout = { bin = { x = "x.exe" } }

  [[tool.recipe]]
  version = "24.21.0"
  platform = "windows-x64"
  url = "https://example.invalid/x-{version}.zip"
  archive = "zip"
  checksum = { algorithm = "sha256", value = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" }
  layout = { bin = { x = "x.exe" } }
"#;
        let catalog = tuoen_manifest::load_str(text).expect("目录应当有效");
        assert_eq!(
            newest_installable(&catalog, "x", "windows-x64").as_deref(),
            Some("24.21.0"),
            "必须按自然比较取最新的，不是按书写顺序"
        );
    }

    #[test]
    fn newest_installable_ignores_a_prohibited_recipe() {
        // 工具层允许，但**某一条 recipe** 更严格地声明为不可再分发 ——
        // "最新"必须是"最新**可安装**"，否则不带版本号的安装会撞上许可证门禁。
        let text = r#"
schema_version = 1
name = "t"

[[tool]]
id = "x"
display_name = "X"
  [tool.licence]
  redistribution = "allowed"
  spdx_or_name = "MIT"

  [[tool.recipe]]
  version = "25.0.0"
  platform = "windows-x64"
  url = "https://example.invalid/x-{version}.zip"
  archive = "zip"
  checksum = { algorithm = "sha256", value = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
  layout = { bin = { x = "x.exe" } }
  licence = { redistribution = "prohibited", spdx_or_name = "专有" }

  [[tool.recipe]]
  version = "24.21.0"
  platform = "windows-x64"
  url = "https://example.invalid/x-{version}.zip"
  archive = "zip"
  checksum = { algorithm = "sha256", value = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" }
  layout = { bin = { x = "x.exe" } }
"#;
        let catalog = tuoen_manifest::load_str(text).expect("目录应当有效");
        assert_eq!(
            newest_installable(&catalog, "x", "windows-x64").as_deref(),
            Some("24.21.0"),
            "25.0.0 不可再分发，不该被选成'最新可安装版本'"
        );
    }

    #[test]
    fn newest_installable_is_none_for_a_tool_without_recipes() {
        let catalog = tuoen_manifest::load_str(
            r#"
schema_version = 1
name = "t"

[[tool]]
id = "oracle-jdk"
display_name = "不可再分发"
  [tool.licence]
  redistribution = "prohibited"
  spdx_or_name = "BCL"
  # 校验器**强制**要求 prohibited 必须写清原因 ——
  # "拒绝安装而不给出具体原因等于没给信息"。
  # 这条测试第一次跑就红了，正是因为漏了它。校验器是对的。
  notes = "许可不允许再分发。"
"#,
        )
        .expect("目录应当有效");
        assert_eq!(
            newest_installable(&catalog, "oracle-jdk", "windows-x64"),
            None
        );
    }
}
