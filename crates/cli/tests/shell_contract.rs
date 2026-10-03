//! `tuoen shell` 的**全局包重定向契约**（票据 #26）。
//!
//! 这一票要钉住的是三句话：
//!
//! 1. `NPM_CONFIG_PREFIX` / `PYTHONUSERBASE` / `PIP_USER=1` **只进子进程的环境块**
//!    （决策 27），而且值是 tuoen 自己的根（`…\globals\npm\<node -v 原样>` /
//!    `…\globals\pip`，决策 26）；
//! 2. 说不出根时**不设**那个变量，并且**把原因说出来**（决策 172）——
//!    两件事都不许静默；
//! 3. 跑完之后 `HKCU\Environment` **逐字未变**（决策 3 / 12 / 154 一脉相承：
//!    用户自己终端里的 `npm ls -g` 必须仍然说真话）。
//!
//! # 为什么这些用例不看这台机器上装了什么
//!
//! 判据是决策 116：**有锁就不跑检测、不解析**。所以每个用例都在项目目录里手写一份
//! 与声明一致的锁（[`write_lock`]，用 core 自己的写盘函数），于是计划完全由测试决定，
//! 一次版本探测都不发生、一次网络访问都没有。唯一真的起进程的地方是 `--exec`
//! 那几条，而它们跑的是 `cmd.exe /C echo …`（`cmd` 不查 `PATH`，它只展开环境变量）。
//!
//! # 为什么子进程的**环境**是这里唯一能看见的判据
//!
//! `spawn_inherit` 是一个自由函数（不是 `ProcessRunner` 的实现），所以固定装置
//! （`FixtureProcess`）**记录不到它** —— 那条 seam 不存在，也不该为了测试而造：
//! 交互式 shell 需要真正的控制台，它和"跑一条命令把输出收回来"不是同一件事。
//! 于是这里用的是**比固定装置更强**的判据：让真的子进程把它的环境**印出来**。
//! 构造那一份环境的**纯函数**则由 `src/shell_cmd.rs` 的 `mod tests` 逐字断言
//! （键与值），两条一起覆盖"构造得对"与"真的送到了子进程"。
//!
//! # 关于"跑完前后没变"
//!
//! `AGENTS.md` 规矩五：断言不许依赖机器状态，只能断言"跑完前后没变"，而且要**往被
//! 测代码会写的那个目录里看一层**（[`list_tree`] 连目录与重解析点都算进清单）。
//! 注册表那一条同理：只读快照两遍、逐字比较，**绝不写**。

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use common::{IsolatedHome, TempDir, list_tree, stderr, stdout};
use tuoen_core::detect::KNOWN_TOOLS;
use tuoen_core::pin::{LOCK_FILE_NAME, LockFile, LockTool, PIN_FILE_NAME};
use tuoen_platform::{RealRegistry, RegHive, RegValue, Registry};

/// 子 shell 的超时。这台机器上 `cmd.exe /C echo` 是几毫秒的事，30 秒只可能是真的挂住了
/// （`AGENTS.md` 铁律 3：一条不能失败的测量不是测量）。
const CHILD_TIMEOUT: Duration = Duration::from_secs(30);

/// 三个变量各印一次，用 `[…]` 包起来。
///
/// **括号是判据的一部分**：变量没设时 `cmd` 把 `%VAR%` **原样**印出来，于是
/// "没设"与"设成了空串"这两件事在输出里长得不一样。
const ECHO_TRIPLE: &str = "echo [%NPM_CONFIG_PREFIX%] [%PYTHONUSERBASE%] [%PIP_USER%]";

// ---------------------------------------------------------------------------
// 装置
// ---------------------------------------------------------------------------

fn project(label: &str) -> TempDir {
    TempDir::new(&format!("shell-{label}"))
}

fn write_pin(dir: &Path, tools: &[(&str, &str)]) {
    let mut text = String::from("[project]\nname = \"contract\"\n\n[tools]\n");
    for (name, spec) in tools {
        text.push_str(&format!("{name} = \"{spec}\"\n"));
    }
    std::fs::write(dir.join(PIN_FILE_NAME), text).expect("写 tuoen.toml");
}

/// 一份与声明一致的锁。**用 core 自己的写盘函数**，不手抄 TOML 形状 ——
/// 手抄一份的后果是"锁的形状"有了两个来源，它们漂移时红的会是这些用例，
/// 而它们指向的是一个根本没坏的东西（决策 50 的第一次教训）。
fn write_lock(dir: &Path, tools: &[(&str, &str, &str, PathBuf)]) {
    let entries = tools
        .iter()
        .map(|(name, spec, version, path)| LockTool {
            name: (*name).to_owned(),
            spec: (*spec).to_owned(),
            version: (*version).to_owned(),
            source: "tuoen".to_owned(),
            manager: None,
            path: path.display().to_string(),
            hash: None,
        })
        .collect();
    LockFile::write(&dir.join(LOCK_FILE_NAME), &LockFile::new(entries)).expect("写 tuoen.lock");
}

/// 造一个"这个工具就在这里"的版本目录（命令表取自 `KNOWN_TOOLS`，不手抄一份）。
/// 建出来的是**空文件**，永远不会被执行。
fn stub_tool(dir: &Path, tool_id: &str) {
    std::fs::create_dir_all(dir).expect("建版本目录");
    let spec = KNOWN_TOOLS
        .iter()
        .find(|spec| spec.id == tool_id)
        .unwrap_or_else(|| panic!("KNOWN_TOOLS 里没有 `{tool_id}`"));
    for executable in spec.executables {
        std::fs::write(dir.join(executable.file), b"").expect("建空的可执行文件");
    }
}

/// 一个项目：`tuoen.toml` + 与它一致的锁（`(工具, 规格, 锁里的版本)`）。
///
/// 版本由**测试**给：这一票要断言的正是"锁里的版本怎么变成环境变量里的那一段"，
/// 所以它绝不能来自这台机器上装了哪个 node。
fn project_with_lock(label: &str, tools: &[(&str, &str, &str)]) -> (TempDir, PathBuf) {
    let dir = project(label);
    let mut locked = Vec::new();
    for (name, spec, version) in tools {
        let tool_dir = dir.path().join("tools").join(name);
        stub_tool(&tool_dir, name);
        locked.push((*name, *spec, *version, tool_dir));
    }
    write_pin(
        dir.path(),
        &tools.iter().map(|(n, s, _)| (*n, *s)).collect::<Vec<_>>(),
    );
    write_lock(dir.path(), &locked);
    let first = locked
        .first()
        .map(|entry| entry.3.clone())
        .unwrap_or_else(|| dir.path().to_path_buf());
    (dir, first)
}

fn describe(output: &Output) -> String {
    format!(
        "退出码 {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status.code(),
        stdout(output),
        stderr(output)
    )
}

fn contains_cjk(text: &str) -> bool {
    text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
}

/// 跑一次 `tuoen`，**带超时**。超时就杀掉并让用例红。
fn run_with_timeout(mut command: Command, dir: &Path, args: &[&str], timeout: Duration) -> Output {
    let mut child = command
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("起 tuoen 应当成功");

    let deadline = Instant::now() + timeout;
    while child.try_wait().expect("try_wait").is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("`{args:?}` 超过 {timeout:?} 还没结束 —— 子 shell 挂住了");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    child.wait_with_output().expect("收输出")
}

/// 在隔离家目录里跑一次 `tuoen`。
fn run_in(home: &IsolatedHome, dir: &Path, args: &[&str]) -> Output {
    run_with_timeout(home.command(), dir, args, CHILD_TIMEOUT)
}

/// tuoen 的全局包根（**测试自己拼**，不向产品问 —— 问它等于让断言跟着实现走）。
fn globals_root(home: &IsolatedHome) -> PathBuf {
    home.local_app_data().join("tuoen").join("globals")
}

/// 三个变量在**子进程里**的取值，形如 `[a] [b] [c]`。
fn child_triple(home: &IsolatedHome, dir: &Path) -> Output {
    run_in(home, dir, &["shell", "--exec", ECHO_TRIPLE])
}

// ---------------------------------------------------------------------------
// 1：三个变量的确切值（决策 26 / 27）
// ---------------------------------------------------------------------------

#[test]
fn the_child_shell_gets_the_frozen_redirect_triple() {
    let home = IsolatedHome::new("shell-redirect");
    let (dir, _) = project_with_lock("redirect", &[("node", "24", "24.19.0")]);

    // 跑前跑后各列一遍：这一票**不许**在磁盘上留下任何东西
    // （不写 `.npmrc`、不写 `pip.ini`、连 `tuoen\globals` 这个目录都不该被创建）。
    let home_before = list_tree(&home.local_app_data());
    let project_before = list_tree(dir.path());

    let output = child_triple(&home, dir.path());
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));

    // **`v24.19.0` 而不是 `24.19.0`**：目录名是 `node -v` 的**原样**输出（决策 172）。
    // 少了那个 `v`，用户在 `tuoen shell` 里装的每一个全局包都会落进一个
    // `capture` / `globals list` 永远不去看的目录 —— 一个不报错的"我装好了"。
    let expected = format!(
        "[{}] [{}] [1]",
        globals_root(&home).join("npm").join("v24.19.0").display(),
        globals_root(&home).join("pip").display()
    );
    assert_eq!(stdout(&output).trim(), expected, "{}", describe(&output));

    // 三个变量都设上了 → 没有任何"为什么没设"要说。
    assert!(
        stderr(&output).is_empty(),
        "什么都不缺的时候 stderr 该是干净的：{}",
        describe(&output)
    );

    assert_eq!(
        home_before,
        list_tree(&home.local_app_data()),
        "隔离家目录里多出/少掉了东西（这一票只许改子进程的环境块）：{}",
        describe(&output)
    );
    assert_eq!(
        project_before,
        list_tree(dir.path()),
        "项目目录里多出/少掉了东西（`.npmrc` 会落在这里 —— 决策 27 明令禁止）：{}",
        describe(&output)
    );
}

// ---------------------------------------------------------------------------
// 2：说不出根时不设，而且要说出来（决策 172）
// ---------------------------------------------------------------------------

/// `unknown` 的版本 → 不设 `NPM_CONFIG_PREFIX`，pip 那两个照设，stderr 说明原因。
///
/// 锁里写 `unknown` 是**驱动这一条分支**的固定装置：判据是
/// `ResolvedTool::version`，而 `unknown` 同时表示"这台机器上没有这个运行时"与
/// "这一次没有去问"（决策 172）。
#[test]
fn an_unknown_runtime_version_keeps_pip_and_says_why_npm_is_skipped() {
    let home = IsolatedHome::new("shell-unknown");
    let (dir, _) = project_with_lock("unknown", &[("node", "24", "unknown")]);

    let output = child_triple(&home, dir.path());
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));

    let expected = format!(
        "[%NPM_CONFIG_PREFIX%] [{}] [1]",
        globals_root(&home).join("pip").display()
    );
    assert_eq!(
        stdout(&output).trim(),
        expected,
        "`unknown` 之下 npm 那一个必须**原样**留着 `%…%`（= 没设），pip 的两个照设：{}",
        describe(&output)
    );

    let err = stderr(&output);
    assert!(err.contains("unknown"), "要说出版本是 `unknown`：{err}");
    assert!(
        err.contains("NPM_CONFIG_PREFIX"),
        "要说清没设的是哪一个：{err}"
    );
    assert!(
        err.contains("PYTHONUSERBASE") && err.contains("PIP_USER"),
        "也要说清照设的是哪两个：{err}"
    );
    assert!(contains_cjk(&err), "给人看的话是中文：{err}");
}

/// 计划里**没有 node** → 另一句话（两种"不知道"的修法完全不同）。
#[test]
fn a_plan_without_node_says_a_different_sentence() {
    let home = IsolatedHome::new("shell-no-node");
    let (dir, _) = project_with_lock("no-node", &[]);

    let output = child_triple(&home, dir.path());
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    assert_eq!(
        stdout(&output).trim(),
        format!(
            "[%NPM_CONFIG_PREFIX%] [{}] [1]",
            globals_root(&home).join("pip").display()
        ),
        "没有 node 就没有版本字符串，npm 那一个不许设：{}",
        describe(&output)
    );

    let err = stderr(&output);
    assert!(err.contains("node"), "要说清缺的是 node：{err}");
    assert!(
        !err.contains("unknown"),
        "两种'不知道'不许印同一句话（这一条要说的是'计划里没有 node'）：{err}"
    );
}

/// 连根都算不出来 → **三个都不设**，原因里要有那个变量名。
///
/// 这一条是"最贵的那类错"的护栏：静默不设的话，用户会以为包进了 tuoen 的根，
/// 而它们全在机器自己的位置上。
#[test]
fn without_a_global_root_no_variable_is_set_and_the_reason_names_the_variable() {
    let home = IsolatedHome::new("shell-no-root");
    let (dir, _) = project_with_lock("no-root", &[("node", "24", "24.19.0")]);

    // ① `LOCALAPPDATA` 不在（进程环境里没有它）。
    //    四个变量都先清掉：测试运行器自己的环境是**机器状态**，不许参与断言。
    let mut command = home.command();
    command
        .env_remove("LOCALAPPDATA")
        .env_remove("NPM_CONFIG_PREFIX")
        .env_remove("PYTHONUSERBASE")
        .env_remove("PIP_USER");
    let output = run_with_timeout(
        command,
        dir.path(),
        &["shell", "--exec", ECHO_TRIPLE],
        CHILD_TIMEOUT,
    );
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    assert_eq!(
        stdout(&output).trim(),
        "[%NPM_CONFIG_PREFIX%] [%PYTHONUSERBASE%] [%PIP_USER%]",
        "根算不出来时三个都不设：{}",
        describe(&output)
    );
    let err = stderr(&output);
    assert!(
        err.contains("LOCALAPPDATA"),
        "原因里要有变量名（人得知道去设什么）：{err}"
    );
    for name in ["NPM_CONFIG_PREFIX", "PYTHONUSERBASE", "PIP_USER"] {
        assert!(
            err.contains(name),
            "要说清没设的是哪几个（缺 `{name}`）：{err}"
        );
    }

    // ② `LOCALAPPDATA` 在，但它不是绝对路径 —— 另一条分支，另一句话。
    let mut command = home.command();
    command
        .env("LOCALAPPDATA", r"AppData\Local")
        .env_remove("NPM_CONFIG_PREFIX")
        .env_remove("PYTHONUSERBASE")
        .env_remove("PIP_USER");
    let output = run_with_timeout(
        command,
        dir.path(),
        &["shell", "--exec", ECHO_TRIPLE],
        CHILD_TIMEOUT,
    );
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    assert_eq!(
        stdout(&output).trim(),
        "[%NPM_CONFIG_PREFIX%] [%PYTHONUSERBASE%] [%PIP_USER%]",
        "相对路径拼出来的根会随 cwd 变，所以拒绝（而不是照用）：{}",
        describe(&output)
    );
    assert!(
        stderr(&output).contains("绝对路径"),
        "要说清拒绝的理由：{}",
        describe(&output)
    );
}

/// 原因在**预览**那条路上也要说（决策 172 的"宁可不说，不许说错"里那半个"说"字）。
#[test]
fn the_dry_run_says_the_same_thing_on_stderr_and_keeps_stdout_clean() {
    let home = IsolatedHome::new("shell-dry-note");
    let (dir, _) = project_with_lock("dry-note", &[("node", "24", "unknown")]);

    let human = run_in(&home, dir.path(), &["shell", "--dry-run"]);
    assert_eq!(human.status.code(), Some(0), "{}", describe(&human));
    assert!(
        contains_cjk(&stdout(&human)),
        "人话版计划在 stdout 上：{}",
        describe(&human)
    );
    assert!(
        stderr(&human).contains("NPM_CONFIG_PREFIX"),
        "预览也要说清为什么没设：{}",
        describe(&human)
    );

    // `--json` 那条路上：载荷在 stdout、原因在 stderr，两者互不污染。
    let json_output = run_in(&home, dir.path(), &["shell", "--dry-run", "--json"]);
    assert_eq!(
        json_output.status.code(),
        Some(0),
        "{}",
        describe(&json_output)
    );
    let payload = stdout(&json_output);
    assert!(
        !contains_cjk(&payload),
        "`--json` 的成功载荷里不许有 CJK（原因只能进 stderr）：{payload}"
    );
    assert!(
        serde_json::from_str::<serde_json::Value>(payload.trim()).is_ok(),
        "stdout 必须仍然是完整的一份 JSON：{payload}"
    );
    assert!(
        contains_cjk(&stderr(&json_output)),
        "原因该在 stderr 上：{}",
        describe(&json_output)
    );
}

// ---------------------------------------------------------------------------
// 3：跑完之后用户自己的环境逐字未变（决策 3 / 12 / 154）
// ---------------------------------------------------------------------------

/// `HKCU\Environment` 的**全部值**（名字 → 值，不展开）。
///
/// 走 `RegEnumValueW`（[`RealRegistry`]）：它**从不**展开 `REG_EXPAND_SZ` ——
/// 展开是不可逆的信息损失（`AGENTS.md` 规矩二），拿展开后的值去比会漏掉
/// "类型被改了"这种事。
///
/// `"Environment"` 这个名字本身是**绝对路径**（`RegHive::resolve` 的特例）：
/// 它在 `HKCU\Environment`，**不在** `HKCU\SOFTWARE\Environment` —— 后者那个键不存在，
/// 而拼错的症状是"静默返回空表"（`crates/platform/src/registry.rs` 记着这次真 bug）。
/// 所以下面的护栏断言必须真的读到东西。
fn user_environment_snapshot() -> Vec<(String, RegValue)> {
    let mut values = RealRegistry
        .values(RegHive::Hkcu, "Environment")
        .expect("读 HKCU\\Environment（只读）");
    // 枚举顺序不是契约 —— 两边都排过再比（决策 94 的第三次）。
    values.sort_by(|left, right| left.0.cmp(&right.0));
    values
}

#[test]
fn the_user_environment_is_byte_identical_after_a_shell_run() {
    // **护栏**：一个读不到东西的助手会让每一条 `before == after` 都退化成
    // "两份空表相同"（`AGENTS.md` 规矩五："这条断言失败过吗？"）。
    let before = user_environment_snapshot();
    assert!(
        !before.is_empty(),
        "`HKCU\\Environment` 不该是空的 —— 读不到东西的助手什么都证明不了"
    );
    assert!(
        before
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("Path")),
        "本机的用户环境里必然有 `Path`：{before:?}"
    );

    let home = IsolatedHome::new("shell-registry");
    let (dir, _) = project_with_lock("registry", &[("node", "24", "24.19.0")]);
    let output = child_triple(&home, dir.path());
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    // 反面对照：这一条不许在"什么都没跑"的情况下通过。
    assert!(
        stdout(&output).contains("v24.19.0"),
        "子进程真的拿到了重定向（否则这条用例只是没跑而已）：{}",
        describe(&output)
    );

    let after = user_environment_snapshot();
    assert_eq!(
        after,
        before,
        "一次 `tuoen shell` 改动了 `HKCU\\Environment` 的 {} 个值里的内容 —— \
         决策 27 只允许改**子进程**的环境块",
        before.len()
    );
}
