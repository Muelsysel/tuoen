//! `tuoen shell` / `tuoen auto` 的执行：装配真实依赖 → 造计划 → 起 shell。
//!
//! # 决策 121：**同一个计划构造器，一道门**
//!
//! `auto` 与 `shell` 的差别**只有**那一道信任门（[`trust_gate`]）。
//! 两条路径各写一份构造逻辑必然漂移，而漂移的表现是"`auto` 切了、`shell` 没切"
//! —— 那种 bug 没有任何一条日志会指向它。所以这里的形状是：
//!
//! ```text
//! shell:                      build_plan() → dry_run? 打印 : spawn
//! auto:  trust_gate() → Trusted → build_plan() → dry_run? 打印 : spawn
//! ```
//!
//! # 锁文件：为什么"有锁就用锁"而不是"每次重新解析"
//!
//! 决策 116 的原文是"**没有**锁文件时 `shell` 现场解析并**写出**锁文件" ——
//! "现场解析"被单独点出来，说明它**只**发生在没有锁的时候。这不是省事，是安全：
//!
//! * `tuoen.lock` 里的 `path` 是**绝对路径**（决策 119），而锁文件是要进 git 的。
//!   一台机器上写出来的锁，在另一台机器上很可能指向一个**不存在的目录** ——
//!   按它前置 `PATH` 的结果是"前置了却没生效"（决策 120 的那个失效形态），
//!   而用户会以为版本切过去了。反过来，锁与声明一致时，按锁执行与按声明执行
//!   得到的是**同一个答案**（"一致"就是这个意思）。
//! * 副作用是 `shell` 的第二条快路径：第一次跑付一次检测的钱（要起进程问版本），
//!   之后每次都是纯读文件。
//!
//! # 为什么这里会启动**几十个进程**
//!
//! 没有锁时要跑一次检测（`probe_versions: true`）—— 那是"pin 的这个版本装了吗"
//! 唯一的输入。它给每个检测到的工具起一个进程问版本，而本机的行数是十几条。
//! 这是**既有行为**（`tuoen detect` 一直这么做），并且只在没有锁的时候发生。
//! 有锁之后 `shell` 一个进程都不起（除了子 shell 本身）。
//!
//! `AGENTS.md` 铁律 3 要求"跑之前先算最坏情况下的进程数"：这里的上限是
//! **检测到的工具条数**（每条一个探针，`DEFAULT_PROBE_TIMEOUT` 是 3 秒），
//! 加上**一个**子 shell。没有重试、没有 fork 自己、没有孙子进程。
//!
//! # 这一层写什么
//!
//! 只写 `tuoen.lock`（解析成功且非 `--dry-run` 时）。**绝不**翻转任何 junction、
//! **绝不**改父进程的环境、**绝不**动 store 里的东西 —— 这是决策 119 的硬要求，
//! `crates/cli/tests/pin_contract.rs` 用"跑前跑后逐项相同"证明它。

use std::path::{Path, PathBuf};

use tuoen_core::detect::{DetectedTool, KNOWN_TOOLS};
use tuoen_core::pin;
use tuoen_core::pin::{
    LOCK_FILE_NAME, LockFile, MAX_SHELL_DEPTH, MissingTool, PIN_FILE_NAME, PinError, PinFile,
    Resolution, ResolveContext, ResolvedTool, SHELL_DEPTH_VAR, ShellKind, ShellPlan, ShellSpec,
    TrustState,
};
use tuoen_store::Store;

use crate::envelope::Envelope;
use crate::exit;
use crate::pin_view::{
    FINGERPRINT_MISMATCH, Failure, ShellPlanView, UNTRUSTED, print_plan_human, print_warnings,
    report,
};
use crate::shell::{AutoArgs, ShellArgs};
use crate::trust_cmd;

// 三个 CLI 自己拥有的错误码（`untrusted` / `fingerprint-mismatch` / `unwired-root`）
// 定义在 `pin_view` —— 失败形状只有一个落点，码也一样。

// ─────────────────────────────────────────────────────────────────────────────
// 入口
// ─────────────────────────────────────────────────────────────────────────────

/// 跑 `tuoen shell`。返回退出码。
///
/// **永远可用，不需要信任**（决策 15）：`shell` 是用户**显式**敲的那条命令，
/// 所以"陌生仓库自动改我的工具链"这个漏洞在它身上不存在。
pub fn run_shell(args: &ShellArgs) -> i32 {
    let spec = ShellSpec {
        kind: args.shell.kind(),
        exec: args.exec.clone(),
    };
    let cwd = match current_dir() {
        Ok(cwd) => cwd,
        Err(failure) => return report("shell", args.json, &failure, false),
    };
    run_plan_at("shell", &cwd, &spec, args.dry_run, args.json, None)
}

/// 跑 `tuoen auto`。返回退出码。
///
/// 与 [`run_shell`] 共用同一个计划构造器，差别只有 [`trust_gate`]。
pub fn run_auto(args: &AutoArgs) -> i32 {
    let spec = ShellSpec {
        kind: args.shell.kind(),
        exec: args.exec.clone(),
    };
    let cwd = match current_dir() {
        Ok(cwd) => cwd,
        Err(failure) => return report("auto", args.json, &failure, false),
    };

    let trust = match trust_gate(&cwd) {
        Ok(state) => state,
        // **这道门的失败要打到 stderr**（决策 121 的原文："未信任 → 不启动 +
        // stderr 提示"）。`--json` 的那条路上信封在 stdout、提示在 stderr，
        // 两者互不污染 —— 而"这个目录为什么没自动切"正是用户唯一需要读的那句话。
        Err(failure) => return report("auto", args.json, &failure, true),
    };

    run_plan_at("auto", &cwd, &spec, args.dry_run, args.json, Some(trust))
}

/// 信任门。**`auto` 与 `shell` 唯一的差别。**
///
/// 四态各有各的下场（决策 121）：
///
/// * `Trusted` → 放行。
/// * `NotTrusted` → 拒绝 + 提示"跑 `tuoen trust` 可启用自动切换"。
/// * `Stale` → 拒绝 + 提示重新 `trust`。**不是**静默信任（那等于指纹白算了），
///   **也不是**永久拉黑（用户改回去就该恢复）。
/// * `MissingFile` → **放行到共用构造器**，由它报 `missing-pin`。
///   在这里另编一句"这个目录没有 `tuoen.toml`"的结果是两处各说一句，
///   而某一天它们说的不是同一件事 —— core 的 `PinFile::load` 已经有那一句。
fn trust_gate(cwd: &Path) -> Result<TrustState, Failure> {
    let pin_path = cwd.join(PIN_FILE_NAME);
    if !pin_path.is_file() {
        // **没有声明就没有"要不要自动应用"这个问题**，信任门在这里不该发言：
        // 它的 `NotTrusted` 消息里写着"这个目录有 `tuoen.toml`"，而那句话在
        // 这个目录里是错的 —— 一句看起来完全合理的错话，比一次崩溃更难被发现。
        // 判据与消息都走 `PinFile::load`（`missing-pin`）。
        if let Err(error) = PinFile::load(&pin_path) {
            return Err(error.into());
        }
    }

    let state = trust_state(cwd)?;
    match state {
        TrustState::Trusted | TrustState::MissingFile => Ok(state),
        TrustState::NotTrusted => Err(Failure::cli(
            UNTRUSTED,
            format!(
                "`{}` 不在信任清单里，所以 `tuoen auto` **不会**自动应用它的 pin。\n\
                 进入陌生仓库就自动改工具链版本是安全漏洞（恶意仓库可以 pin 一个\
                 带后门的「Node 版本」），所以自动切换默认关闭。\n\
                 这个目录有 `tuoen.toml`，跑 `tuoen trust` 可启用自动切换。",
                cwd.display()
            ),
        )),
        TrustState::Stale { .. } => Err(Failure::cli(
            FINGERPRINT_MISMATCH,
            format!(
                "`{}` 被信任过，但 `tuoen.toml` 的内容变了（指纹对不上）。\n\
                 指纹是为了防「删掉目录再 clone 一个同名目录」（决策 16），\
                 所以内容一变就必须重新确认 —— 不是静默信任，也不是永久拉黑。\n\
                 确认内容之后重新跑一次 `tuoen trust` 就能恢复自动切换。",
                cwd.display()
            ),
        )),
    }
}

/// 读一次信任状态。**`%APPDATA%` 取不到就是失败**，不是"当作没信任"。
fn trust_state(cwd: &Path) -> Result<TrustState, Failure> {
    // 清单的装载只有一处定义（[`crate::trust_cmd::load`]）：`auto` 的信任门与
    // `trust --list` 必须读**同一份清单**、用**同一套失败**。
    Ok(trust_cmd::load()?.state(cwd))
}

// ─────────────────────────────────────────────────────────────────────────────
// 计划
// ─────────────────────────────────────────────────────────────────────────────

/// 一次成功的计划构造。
#[derive(Debug)]
struct Planned {
    plan: ShellPlan,
    /// 这一次**真的写了** `tuoen.lock` 吗。
    lock_written: bool,
    /// 锁文件的位置（**不管写没写都报出来**：用户要知道下一次会读哪里）。
    lock_file: PathBuf,
}

/// **`shell` 与 `auto` 共用的计划构造器**（决策 121）。
///
/// 顺序是刻意的，每一步都排在后一步之前：
///
/// 1. **读 `tuoen.toml`** —— 没有它后面每一步都没有意义。
/// 2. **锁的闸门** —— 锁与声明不一致就**停下来**（决策 116）。排在第 3 步之前，
///    是因为"按锁执行"与"按声明执行"都会骗人，而两者都不该发生任何写盘。
/// 3. **解析**（只在没有锁时）—— 缺版本就报 `version-not-installed` 并**不写锁**
///    （决策 116 的"解析失败时不写"）：一份描述"半个工具链"的锁比没有锁更糟。
/// 4. **建计划** —— 前置目录、`pathAfter`、遮蔽警告都由 core 算。
/// 5. **深度闸门** —— 见下。
///
/// # 为什么深度闸门也拦 `--dry-run`
///
/// 深度是**计划的性质**，不是执行的性质：`plan.too_deep()` 为真时这份计划
/// 根本跑不了，而一份跑不了的计划的预览会让人以为它跑得了。拒绝比预览诚实。
fn build_plan(cwd: &Path, dry_run: bool) -> Result<Planned, Failure> {
    let pin_path = cwd.join(PIN_FILE_NAME);
    let pin = PinFile::load(&pin_path)?;
    let lock_path = cwd.join(LOCK_FILE_NAME);

    let (tools, lock_written) = match LockFile::load(&lock_path)? {
        Some(lock) => {
            let mismatches = lock.mismatches(&pin);
            if !mismatches.is_empty() {
                // 决策 116：不一致时**拒绝启动**，并给出下一条命令。
                // 不给 `--ignore-lock`：先有需求再加，加容易、去难。
                return Err(PinError::lock_mismatch(mismatches).into());
            }
            (tools_from_lock(&lock), false)
        }
        None => {
            let tools = resolve_now(&pin)?;
            if dry_run {
                // **`--dry-run` 不写任何文件**（决策 123 与票据的硬要求）：
                // 一次预览不该在仓库里留下一个未跟踪的文件。
                (tools, false)
            } else {
                // 锁的形状只有一处定义（core 的 `LockFile::from_resolved`）。
                let lock = LockFile::from_resolved(&tools);
                LockFile::write(&lock_path, &lock)?;
                (tools, true)
            }
        }
    };

    let depth = pin::shell::depth_from_env(std::env::var(SHELL_DEPTH_VAR).ok().as_deref());
    let plan = ShellPlan::build(cwd, &tools, &current_path(), KNOWN_TOOLS, depth);

    if plan.too_deep() {
        // 码与消息都取自 core（`shell-depth`）—— 这一条不是 CLI 的判据，
        // 是 `PinError::Depth` 的：上限住在 core，这里只把当前深度递过去。
        return Err(PinError::Depth {
            depth,
            max: MAX_SHELL_DEPTH,
        }
        .into());
    }

    Ok(Planned {
        plan,
        lock_written,
        lock_file: lock_path,
    })
}

/// 现场解析（没有锁文件时的那条路）。**`pub(crate)` 是给 `crate::lock_cmd` 的** ——
/// `tuoen lock` 与 `tuoen shell` 必须用**同一个**解析器，否则两份锁在某个边界
/// 条件下不是同一个东西，而那个边界条件只有用户会撞上。
///
/// **装配六个真实依赖，与 `doctor_cmd` / `main.rs` 的 `run_detect` 一模一样** ——
/// "读这台机器"的代码只有那一套，在这里另写一套会让两份事实慢慢漂移。
///
/// `probe_versions: true` 是必须的：pin 可能指向一个**第三方**装出来的版本
/// （决策 114：候选来自我们的 store **与**检测到的第三方），而第三方的版本号
/// 只能靠起进程问出来。这是本命令唯一会大面积启动第三方程序的地方，也是
/// 它只在**没有锁**的时候发生的原因。
pub fn resolve_now(pin: &PinFile) -> Result<Vec<ResolvedTool>, Failure> {
    let detected = detect_tools();
    let store = Store::at_default_location();
    let ctx = ResolveContext {
        store: &store,
        detected: &detected,
        known_tools: KNOWN_TOOLS,
    };

    match pin::resolve(&ctx, pin) {
        Resolution::All(tools) => Ok(tools),
        Resolution::Failed(missing, unknown) => {
            // **不认识的工具 id 先报**（决策 113）：那是**声明**本身的问题，
            // 修好它之前，其余缺口的 `next_command` 都建立在一条读不懂的声明上。
            if let Some(id) = unknown.first() {
                return Err(PinError::unknown_tool(id, KNOWN_TOOLS).into());
            }
            Err(missing_failure(&missing))
        }
    }
}

/// 把"有工具没装"翻译成一个错误。
///
/// **`next_command` 必须在消息里**（票据 story 47：pin 一个没装的版本时要说清
/// 下一条该跑什么）。core 的 [`PinError::version_not_installed`] 已经为第一个
/// 缺口写好了那句话；这里补上**其余**的缺口 —— 一次只报一个会让用户跑两遍，
/// 而"静默跳过是 bug"（`AGENTS.md` 的捕获规矩）在这里同样成立。
fn missing_failure(missing: &[MissingTool]) -> Failure {
    let Some(first) = missing.first() else {
        // `Resolution::Failed` 两个列表都空是不可达的（`unknown` 已经在上面
        // 先报掉了）；真到了这里就**如实说**"解析失败了但说不出缺什么"，
        // 而不是编一个"没装"（那可能是一句假话）。
        //
        // 码仍然取自 core（构造一个空的 `VersionNotInstalled` 只为读它的 slug），
        // 消息换成我们这一句 —— 一个手写的 `"version-not-installed"` 字面量
        // 会在 core 改 slug 的那天变成一个永远不会出现的码。
        let mut failure = Failure::from(PinError::VersionNotInstalled {
            tool: String::new(),
            spec: String::new(),
            installed: Vec::new(),
        });
        failure.message = "解析 pin 失败，但解析器没有说是哪个工具缺版本 —— 这是一个 bug，\
             请把这个 `tuoen.toml` 报告给我们。"
            .to_owned();
        return failure;
    };

    let mut failure = Failure::from(PinError::version_not_installed(first));
    if missing.len() > 1 {
        failure
            .message
            .push_str(&format!("\n另外还有 {} 个工具也没装：", missing.len() - 1));
        for tool in &missing[1..] {
            failure.message.push_str(&format!(
                "\n  · {} {} —— 跑 `{}`",
                tool.name, tool.spec, tool.next_command
            ));
        }
    }
    failure
}

/// 跑一次检测，只要那一串工具行。
///
/// 与 `doctor` 的区别只有一处：`probe_versions: true`（理由见 [`resolve_now`]）。
fn detect_tools() -> Vec<DetectedTool> {
    let fs = tuoen_platform::RealFileSystem;
    let registry = tuoen_platform::RealRegistry;
    let env = tuoen_platform::RealEnvBlock::new(registry, fs);
    let process_env = tuoen_platform::RealProcessEnv::new();
    let runner = tuoen_platform::SystemProcessRunner;
    let managed = crate::managed::StoreManagedStore::at_default_location();
    let scan_roots = tuoen_core::detect::engine::default_scan_roots(&process_env);

    let ctx = tuoen_core::detect::DetectContext {
        fs: &fs,
        registry: &registry,
        env: &env,
        process_env: &process_env,
        runner: &runner,
        managed: &managed,
        probe_timeout: tuoen_platform::DEFAULT_PROBE_TIMEOUT,
        probe_versions: true,
        scan_roots,
    };

    tuoen_core::detect::detect_all(&ctx).tools
}

/// 把锁里的工具读回成解析结果。
///
/// 两个结构体的字段是**一一对应**的（决策 115 定的锁形状就是为了这件事）——
/// 所以这一层不重新解析、不起进程、不碰磁盘上的工具，只是换个类型。
fn tools_from_lock(lock: &LockFile) -> Vec<ResolvedTool> {
    lock.tool
        .iter()
        .map(|tool| ResolvedTool {
            name: tool.name.clone(),
            spec: tool.spec.clone(),
            version: tool.version.clone(),
            source: tool.source.clone(),
            manager: tool.manager.clone(),
            path: PathBuf::from(&tool.path),
            hash: tool.hash.clone(),
        })
        .collect()
}

/// 当前进程看到的 `PATH`。
///
/// **原样取，不 split 再 join**：`;` 串里的空条目是**真实存在**的（决策 92 的教训：
/// 空段落不是一个缺失的目录），split/join 会把它们吃掉，于是"前置之后到底有多少条"
/// 与用户机器上的事实对不上。
///
/// Windows 的环境变量查找不区分大小写（`std::env::var` 走 `GetEnvironmentVariableW`），
/// 所以两个拼法只要有一个在就行 —— 但**两个都试**，因为这条路径的失败模式是
/// "静默地拿到空串"，然后 `pathAfter` 变成只有前置目录。
fn current_path() -> String {
    std::env::var("PATH")
        .or_else(|_| std::env::var("Path"))
        .unwrap_or_default()
}

fn current_dir() -> Result<PathBuf, Failure> {
    std::env::current_dir().map_err(|error| Failure::io(None, format!("读不出当前目录：{error}")))
}

// ─────────────────────────────────────────────────────────────────────────────
// 执行
// ─────────────────────────────────────────────────────────────────────────────

/// 计划 →（预览 | 真起 shell）。**两个命令共用的后半段。**
fn run_plan_at(
    command: &'static str,
    cwd: &Path,
    spec: &ShellSpec,
    dry_run: bool,
    json: bool,
    trust: Option<TrustState>,
) -> i32 {
    let planned = match build_plan(cwd, dry_run) {
        Ok(planned) => planned,
        Err(failure) => return report(command, json, &failure, false),
    };

    // **遮蔽警告在两条路上都打**（决策 120）：`--dry-run` 也要 ——
    // 一份跑不了的计划的预览会让人以为它跑得了。
    print_warnings(planned.plan.warnings());

    if dry_run {
        let view = ShellPlanView::new(&planned.plan, spec, planned.lock_written, trust);
        if json {
            crate::print_json(&Envelope::ok(command, &view));
        } else {
            print_plan_human(&view, &planned.lock_file);
        }
        return exit::SUCCESS;
    }

    spawn_shell(&planned.plan, spec)
}

/// 子 shell 的一个 argv 元素。
///
/// `Quoted` 交给 `std::process::Command` 加引号（MSVCRT 规则）；`Raw` 原样交给子进程。
///
/// **为什么 `cmd.exe /C` 的载荷必须是 `Raw`**：Rust 会把含引号的参数转义成 `\"`，
/// 而 `cmd.exe` **不认**这种转义 —— 它看到的是字面的反斜杠。真机实测：
/// `tuoen shell --exec 'node -e "console.log(1)"'` 在旧写法下**输出为空、退出码 0**，
/// 是一句看起来完全合理的成功；而 `--exec "node -v"`（不含引号）照常工作，
/// 所以只看"能不能跑"是发现不了的。`raw_arg` 把整串交给 `cmd.exe` 自己的解析器。
#[derive(Debug, PartialEq, Eq)]
enum ShellArg {
    Quoted(String),
    Raw(String),
}

/// **全仓唯一一处构造子 shell 命令行的函数**（`AGENTS.md` 铁律 2）。
///
/// 第二次进程事故的根因就是"半成品缓冲 + 自己补 NUL"，所以这里**不拼字符串**：
/// 返回的是 `(program, args)`，引号与转义全部交给 `std::process::Command`
/// （`cmd.exe /C` 的载荷除外，见 [`ShellArg`]）。调用方拿不到一个可以再加一个
/// 参数的半成品命令行。
///
/// # 为什么 `--exec` 的载荷是**一个** argv 元素
///
/// `cmd.exe /C "a && b"` 里的 `a && b` 是**一个**参数，由 `cmd.exe` 自己解析。
/// 我们在这一层把它拆开就等于实现了第二个命令行解析器 —— 而 `cmd` 的解析规则
/// （`&`、`|`、`%VAR%` 展开、引号删除）正是决策 52 记下来的那类坑。
///
/// # 为什么用绝对路径而不是裸命令名
///
/// 子进程的 `PATH` 会被换成 `plan.path_after()`，而 `powershell.exe` **不在**
/// `System32` 里（在 `System32\WindowsPowerShell\v1.0\`），所以"让 `CreateProcess`
/// 去 PATH 上找"这条路在 PowerShell 上是不可靠的。两个都取绝对路径，
/// 取不到才退回裸名字（那时至少还能靠 Windows 目录搜索）。
fn launch_command(spec: &ShellSpec) -> (PathBuf, Vec<ShellArg>) {
    match spec.kind {
        ShellKind::Cmd => {
            let mut args = Vec::new();
            if let Some(command) = &spec.exec {
                // `/C` 之后是**一条**命令；cmd 会把整串交给自己的解析器。
                args.push(ShellArg::Quoted("/C".to_owned()));
                args.push(ShellArg::Raw(command.clone()));
            }
            (system32(&["cmd.exe"]), args)
        }
        ShellKind::PowerShell => {
            // `-NoProfile` 是**必须的**：用户 profile 里可能有 `Set-Location`、
            // 可能有 `conda activate`，而我们要的是一个可预测的 shell。
            let mut args = vec![ShellArg::Quoted("-NoProfile".to_owned())];
            if let Some(command) = &spec.exec {
                args.push(ShellArg::Quoted("-Command".to_owned()));
                // PowerShell 是**正常程序**：`Command::arg` 的转义就是它期望的那套，
                // 所以这里**不能**用 `Raw`（那会把引号原样塞进去，反而拆坏）。
                args.push(ShellArg::Quoted(command.clone()));
            }
            (
                system32(&["WindowsPowerShell", "v1.0", "powershell.exe"]),
                args,
            )
        }
    }
}

/// `%SystemRoot%\System32\<…>`。取不到就退回裸文件名。
fn system32(relative: &[&str]) -> PathBuf {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_owned());
    let mut path = PathBuf::from(root).join("System32");
    for segment in relative {
        path.push(segment);
    }
    if path.is_file() {
        return path;
    }
    // 退回裸名字：`CreateProcess` 的搜索顺序里有 Windows 目录，
    // 而 `cmd.exe` 就在那里。**不**在这里报错 —— 报错的落点只有一个，
    // 在 `spawn_inherit` 失败的那一处（那里才有真实的 Win32 码）。
    PathBuf::from(relative.last().copied().unwrap_or("cmd.exe"))
}

/// 起子 shell，把它的退出码**原样**带回来。
fn spawn_shell(plan: &ShellPlan, spec: &ShellSpec) -> i32 {
    let (program, args) = launch_command(spec);
    // 普通参数交给 `std::process::Command` 加引号；`Raw` 原样交给子进程
    // （只有 `cmd.exe /C` 的载荷走这一条 —— 它的解析规则不是 MSVCRT 的规则）。
    let argv: Vec<&str> = args
        .iter()
        .filter_map(|arg| match arg {
            ShellArg::Quoted(text) => Some(text.as_str()),
            ShellArg::Raw(_) => None,
        })
        .collect();
    let raw: Vec<&str> = args
        .iter()
        .filter_map(|arg| match arg {
            ShellArg::Quoted(_) => None,
            ShellArg::Raw(text) => Some(text.as_str()),
        })
        .collect();

    let path_after = plan.path_after().to_owned();
    // `PATH` 与 `Path` **两个键都要设**：Windows 的环境变量查找不区分大小写，
    // 但 `std::process::Command` 是按 `OsString` 存的 —— 只设一个的话，
    // 另一个会留着父进程的原值（本仓库既有做法见 `scripts/acceptance-L1-12.ps1`）。
    let env = vec![
        ("PATH".to_owned(), path_after.clone()),
        ("Path".to_owned(), path_after),
        (
            SHELL_DEPTH_VAR.to_owned(),
            plan.depth().saturating_add(1).to_string(),
        ),
    ];

    // `cwd` 显式传 `plan.cwd()`：它与当前目录是同一个值，但**把这件事说出来**
    // 比"靠继承恰好一致"强 —— 计划是为这个目录算的，子 shell 就该在这个目录里。
    match tuoen_platform::spawn_inherit(&program, &argv, &raw, &env, &[], Some(plan.cwd())) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("tuoen: 起不了子 shell `{}`：{error}", program.display());
            exit::RUNTIME_ERROR
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::ShellChoice;

    #[test]
    fn the_two_shells_build_the_documented_command_lines() {
        // 铁律 2 的落点：**只有这一个函数**在构造子 shell 的命令行。
        // 逐条钉住它，因为一个多出来的参数就是一个多出来的 argv 元素。
        let cmd = launch_command(&ShellSpec {
            kind: ShellKind::Cmd,
            exec: None,
        });
        assert!(cmd.0.ends_with("cmd.exe"), "{:?}", cmd.0);
        assert!(cmd.1.is_empty(), "交互式 cmd 不带参数：{:?}", cmd.1);

        let cmd_exec = launch_command(&ShellSpec {
            kind: ShellKind::Cmd,
            exec: Some("echo a && echo b".to_owned()),
        });
        assert_eq!(
            cmd_exec.1,
            vec![
                ShellArg::Quoted("/C".to_owned()),
                // **载荷是 `Raw`**：cmd.exe 的解析规则不是 MSVCRT 的规则。
                ShellArg::Raw("echo a && echo b".to_owned()),
            ]
        );

        let ps = launch_command(&ShellSpec {
            kind: ShellKind::PowerShell,
            exec: None,
        });
        assert!(ps.0.ends_with("powershell.exe"), "{:?}", ps.0);
        assert_eq!(ps.1, vec![ShellArg::Quoted("-NoProfile".to_owned())]);

        let ps_exec = launch_command(&ShellSpec {
            kind: ShellKind::PowerShell,
            exec: Some("node -v".to_owned()),
        });
        assert_eq!(
            ps_exec.1,
            vec![
                ShellArg::Quoted("-NoProfile".to_owned()),
                ShellArg::Quoted("-Command".to_owned()),
                // PowerShell 是正常程序：转义交给 `Command::arg`，**不能**用 `Raw`。
                ShellArg::Quoted("node -v".to_owned()),
            ]
        );
    }

    #[test]
    fn the_shell_choice_reaches_the_kind() {
        assert_eq!(ShellChoice::Cmd.kind(), ShellKind::Cmd);
        assert_eq!(ShellChoice::Powershell.kind(), ShellKind::PowerShell);
    }

    #[test]
    fn the_known_tools_table_has_one_definition_and_it_is_core_s() {
        // CLI **不抄一份** known tools：抄一份的后果是 core 加一个工具之后
        // `tuoen.toml` 里写它会被报成"不认识的工具 id"。
        assert!(!KNOWN_TOOLS.is_empty());
        assert!(KNOWN_TOOLS.iter().any(|tool| tool.id == "node"));
    }

    #[test]
    fn the_depth_code_comes_from_core() {
        // `shell-depth` **不是** CLI 自己拥有的码：它由 core 的 `PinError::Depth` 给。
        // 在这里再定义一份等于承认有两个来源 —— 而两个来源的下一步是它们不一致。
        assert_eq!(
            PinError::Depth {
                depth: MAX_SHELL_DEPTH + 1,
                max: MAX_SHELL_DEPTH,
            }
            .code(),
            "shell-depth"
        );
    }
}
