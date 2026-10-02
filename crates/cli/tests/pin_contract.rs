//! `tuoen shell` / `auto` / `trust` / `lock` 的**进程边界契约测试**（票据 #14）。
//!
//! 这一族命令是本仓库里**唯一会启动子进程**的一族，所以它比别的命令多两类断言：
//! 一条是"该起的起了"（`--exec` 的退出码原样透传），另一条是"不该起的没起"
//! （深度超限、锁不一致、未信任时**一个子进程都不许有**）。
//!
//! # 为什么这些用例不依赖这台机器上装了什么
//!
//! 判据是 `docs/DESIGN.md` 决策 114：候选来自 **store + 检测到的第三方**。
//! 所以凡是要"解析成功"的用例都在隔离家目录的 store 里自己造一个版本目录
//! （[`stub_tool`]），凡是要"解析不参与"的用例都**手写一份与声明一致的锁**
//! （[`write_lock`]）—— 有锁就不跑检测、不解析（决策 116），于是这些用例
//! 在这台机器上装没装 node、装的是哪个版本，都不影响结果。
//!
//! 唯一一处真正起进程的地方是 [`exec_propagates_the_child_exit_code_and_does_not_hang`]，
//! 而它跑的是 `cmd.exe /C exit 7`：`cmd` 不查 `PATH`，所以它连"这台机器上有哪些工具"都不看。
//!
//! # 关于"跑完前后没变"
//!
//! `AGENTS.md` 规矩五：断言不许依赖机器状态，只能断言"跑完前后没变"，
//! 而且要**往被测代码会写的那个目录里看一层**。所以这里用的是 [`list_tree`]
//! （它把目录、重解析点、文件大小都算进清单）而不是"文件个数"这类会漏掉
//! "凭空多出一个 junction"的弱判据。

mod common;

use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use common::{IsolatedHome, TempDir, json, list_tree, stderr, stdout};
use serde_json::Value;
use tuoen_core::detect::KNOWN_TOOLS;
use tuoen_core::pin::{LOCK_FILE_NAME, LockFile, LockTool, PIN_FILE_NAME};

// ---------------------------------------------------------------------------
// 装置
// ---------------------------------------------------------------------------

/// 一个临时项目目录：`tuoen shell` 这一族读的是**当前目录**的 `tuoen.toml`。
fn project(label: &str) -> TempDir {
    TempDir::new(&format!("pin-{label}"))
}

/// `tuoen.toml` 的正文。
///
/// 单独一个函数是为了 CRLF 那条用例：它要的是**逐字相同**的两份文本、
/// 只差行尾 —— 于是正文必须只有一处来源。
fn pin_body(tools: &[(&str, &str)]) -> String {
    let mut text = String::from("[project]\nname = \"contract\"\n\n[tools]\n");
    for (name, spec) in tools {
        text.push_str(&format!("{name} = \"{spec}\"\n"));
    }
    text
}

fn write_pin(dir: &Path, tools: &[(&str, &str)]) {
    std::fs::write(dir.join(PIN_FILE_NAME), pin_body(tools)).expect("写 tuoen.toml");
}

/// 手写一份锁 —— **用 core 自己的写盘函数**，不手抄 TOML 形状。
///
/// 手抄一份的后果是"锁文件的形状"这件事有了两个来源：测试里那份和
/// `LockFile::to_toml()` 那份。它们漂移时红的会是这些用例，而它们指向的是
/// 一个根本没坏的东西（决策 50 的第一次教训）。
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

/// 造一个"这个工具就在这里"的版本目录：把它的**每一个**可执行文件都建出来。
///
/// 命令表取自 `KNOWN_TOOLS`，**不手抄一份**：抄一份的后果是 core 给某个工具
/// 加一条次要命令之后，这条用例会因为"那条命令没人抢到"而报出遮蔽警告，
/// 而它指向的是一个根本没坏的东西。
///
/// 建出来的是**空文件**，永远不会被执行：这一族测试跑的都是 `--dry-run`，
/// 唯一的例外是 `--exec` 那两条，而它们跑的是 `cmd.exe /C exit 7`（`cmd` 不查 `PATH`）。
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

/// 在隔离家目录的 store 里装一个"看起来像装好了"的版本。
fn install_stub(home: &IsolatedHome, tool_id: &str, version: &str) -> PathBuf {
    let dir = home
        .store_root()
        .join(tool_id)
        .join("versions")
        .join(version);
    stub_tool(&dir, tool_id);
    dir
}

/// 拼一份"有 `tuoen.toml`、也有与它一致的锁"的项目 —— 这一族用例最常见的起点。
///
/// 有锁就**不跑检测、不解析**（决策 116），于是计划完全由测试决定。
fn project_with_lock(label: &str, tools: &[(&str, &str)]) -> (TempDir, PathBuf) {
    let dir = project(label);
    let mut locked = Vec::new();
    for (name, spec) in tools {
        let tool_dir = dir.path().join("tools").join(name);
        stub_tool(&tool_dir, name);
        locked.push((*name, *spec, "24.19.0", tool_dir));
    }
    write_pin(dir.path(), tools);
    write_lock(dir.path(), &locked);
    let first = locked[0].3.clone();
    (dir, first)
}

/// 拼 stdout + stderr —— 失败断言里要看得见两边，否则一个把错误印到 stderr
/// 的实现会让失败信息看起来像"什么都没发生"。
fn describe(output: &Output) -> String {
    format!(
        "退出码 {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status.code(),
        stdout(output),
        stderr(output)
    )
}

/// 输出里有没有 CJK。
fn contains_cjk(text: &str) -> bool {
    text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
}

/// 帮助里有没有**以这个开关开头的一行** —— 也就是它真的被注册成了一个参数。
///
/// 不能只判 `contains("--ignore-lock")`：长帮助里**必须**写着"没有 `--ignore-lock`"
/// 这句承诺，而那正是这条用例要保护的东西。判"有没有这个参数"要判的是
/// `Options:` 段里那一行，不是全文里出现过这串字。
fn help_declares(help: &str, flag: &str) -> bool {
    help.lines().any(|line| line.trim_start().starts_with(flag))
}

/// 两个路径是不是同一个 —— 绝对化后的比较要忽略大小写与尾部分隔符（决策 118）。
fn same_path(left: &str, right: &Path) -> bool {
    let trim = |text: &str| {
        text.trim_end_matches(['\\', '/'])
            .replace('/', "\\")
            .to_lowercase()
    };
    trim(left) == trim(&right.to_string_lossy())
}

/// **测试进程真实的** `%APPDATA%\tuoen` 现在长什么样。
///
/// 隔离装置把 `APPDATA` 指向了临时家目录，所以被测代码**看不到**这个目录 ——
/// 而这一条正是判据：一次 `cargo test` 不许碰开发者真实的信任清单。
fn real_roaming_listing() -> Vec<String> {
    let Some(appdata) = std::env::var_os("APPDATA") else {
        return Vec::new();
    };
    list_tree(&PathBuf::from(appdata).join("tuoen"))
}

/// 跑一次 `tuoen`，**带超时**。超时就杀掉并让用例红。
///
/// 存在的理由（`AGENTS.md` 铁律 3）：`shell --exec` 会起一个子进程，
/// 而"一条不能失败的测量不是测量" —— 一个挂住的子 shell 会让整次 `cargo test`
/// 停在那里，而不是给出一个失败。
///
/// `stdin` 接 `NUL`：不给它的话子进程会继承测试运行器的 stdin，
/// 于是"交互式"这件事在测试里变成一次真实的等待。
fn run_with_timeout(home: &IsolatedHome, dir: &Path, args: &[&str], timeout: Duration) -> Output {
    let mut child = home
        .command()
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

/// 断言信封里的失败码，并回显全部输出。
fn assert_code(output: &Output, expected: &str) -> String {
    let envelope = json(output);
    assert_eq!(
        output.status.code(),
        Some(1),
        "这一族命令的运行期失败统一是 1：{}",
        describe(output)
    );
    assert!(
        !envelope.ok,
        "失败时 `ok` 必须是 false：{}",
        describe(output)
    );
    let error = envelope
        .error
        .unwrap_or_else(|| panic!("失败时必须有 error 段：{}", describe(output)));
    assert_eq!(error.code, expected, "{}", describe(output));
    assert!(
        contains_cjk(&error.message),
        "错误消息是给人看的中文（`--json` 里也只有它可以是中文）：{}",
        describe(output)
    );
    error.message
}

// ---------------------------------------------------------------------------
// 1–3：计划的形状、用法闸门、确定性
// ---------------------------------------------------------------------------

#[test]
fn shell_dry_run_json_has_the_frozen_shape() {
    let home = IsolatedHome::new("pin-shape");
    let (dir, tool_dir) = project_with_lock("shape", &[("node", "24")]);

    let before = list_tree(dir.path());
    let output = home.run_in(dir.path(), ["shell", "--dry-run", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));

    // `--dry-run` 不写任何文件 —— 包括**不写锁**（决策 116 的 "不写"）。
    assert_eq!(
        before,
        list_tree(dir.path()),
        "`--dry-run` 动了项目目录：{}",
        describe(&output)
    );

    let text = stdout(&output);
    assert!(
        !contains_cjk(&text),
        "`--json` 的成功载荷里不许有 CJK：{text}"
    );
    assert!(
        !text.contains("\"message\""),
        "`message` 天生是中文，不许进成功载荷：{text}"
    );

    let envelope = json(&output);
    assert_eq!(envelope.schema_version, 1, "{}", describe(&output));
    assert_eq!(envelope.command, "shell");
    assert!(envelope.ok, "{}", describe(&output));
    let data = envelope.data.expect("成功必须有 data");
    for key in [
        "cwd",
        "shell",
        "exec",
        "depth",
        "pathBefore",
        "pathAfter",
        "prepend",
        "tools",
        "warnings",
        "lockWritten",
    ] {
        assert!(data.get(key).is_some(), "缺少顶层键 `{key}`：{data}");
    }

    assert!(same_path(data["cwd"].as_str().expect("cwd"), dir.path()));
    assert_eq!(data["shell"], "cmd");
    assert!(
        data["exec"].is_null(),
        "不带 `--exec` 时 `exec` 是 **null 而不是省略键**（见 `pin_view.rs` 的模块文档）：{data}"
    );
    assert_eq!(data["depth"], 0);
    assert_eq!(
        data["lockWritten"], false,
        "已经有锁了就不该再写一次：{data}"
    );

    let prepend: Vec<String> = data["prepend"]
        .as_array()
        .expect("prepend 是数组")
        .iter()
        .map(|item| item.as_str().expect("prepend 的每一项是字符串").to_owned())
        .collect();
    assert_eq!(prepend.len(), 1, "{data}");
    assert!(same_path(&prepend[0], &tool_dir), "{data}");
    let path_after = data["pathAfter"].as_str().expect("pathAfter");
    assert!(
        path_after.starts_with(&prepend[0]),
        "`pathAfter` 必须以 `prepend` 开头：{data}"
    );
    let path_before = data["pathBefore"].as_str().expect("pathBefore");
    // 决策 119：`pathAfter` = 前置目录 + **原 PATH 去掉重复**。
    //
    // 这里断言的是"原 PATH 的条目一个都没丢、次序也没被打乱"（重复项按**首次出现**
    // 算），而**不是** `ends_with(path_before)`：`ShellPlan` 会顺手丢掉空条目（`;;`，
    // 它在 Windows 上表示"当前目录"）以及与前置目录重复的条目 —— 那是 core 的判断，
    // 不该由这条用例替它决定。真实机器的 `PATH` 上几乎一定有 `;;`，
    // 所以一条 `ends_with` 的断言会红在一个根本没坏的地方。
    let after: Vec<&str> = path_after.split(';').collect();
    let mut expected: Vec<&str> = Vec::new();
    for entry in path_before.split(';').filter(|entry| !entry.is_empty()) {
        if !expected.iter().any(|seen| seen.eq_ignore_ascii_case(entry)) {
            expected.push(entry);
        }
    }
    assert!(
        !expected.is_empty(),
        "原 PATH 是空的，这条断言什么都不测：{data}"
    );
    let mut cursor = 0;
    for entry in &expected {
        let Some(offset) = after[cursor..]
            .iter()
            .position(|candidate| candidate.eq_ignore_ascii_case(entry))
        else {
            panic!("原 PATH 的条目 `{entry}` 在 `pathAfter` 里丢了、或者次序被打乱了：{data}");
        };
        cursor += offset + 1;
    }

    let tools = data["tools"].as_array().expect("tools 是数组");
    assert_eq!(tools.len(), 1, "{data}");
    for key in ["name", "spec", "version", "source", "manager", "path"] {
        assert!(tools[0].get(key).is_some(), "缺少工具字段 `{key}`：{data}");
    }
    assert_eq!(tools[0]["name"], "node");
    assert_eq!(tools[0]["spec"], "24");
    assert_eq!(tools[0]["version"], "24.19.0");
    assert_eq!(tools[0]["source"], "tuoen");
    assert_eq!(
        tools[0]["manager"],
        Value::Null,
        "我们自己装的没有管理器，`null` 是**答案**、不是「没问」：{data}"
    );
    assert!(same_path(
        tools[0]["path"].as_str().expect("path"),
        &tool_dir
    ));
    assert!(
        tools[0].get("hash").is_none(),
        "`hash` 不进 `--json`（机器要哈希去读 `tuoen.lock`）：{data}"
    );

    assert!(
        data["warnings"].as_array().expect("warnings").is_empty(),
        "命令都在前置目录里，不该报遮蔽：{data}"
    );
}

#[test]
fn json_without_dry_run_is_refused_by_clap_with_exit_code_two() {
    let home = IsolatedHome::new("pin-usage");
    let (dir, _) = project_with_lock("usage", &[("node", "24")]);

    // 决策 123：`--json` 只在 `--dry-run` 下有意义，被 clap 以**退出码 2** 拒掉。
    for args in [["shell", "--json"], ["auto", "--json"]] {
        let output = home.run_in(dir.path(), args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "`{}` 必须被 clap 当场拒掉：{}",
            args.join(" "),
            describe(&output)
        );
        assert!(
            stderr(&output).contains("--dry-run"),
            "拒绝的理由要写在 stderr 上：{}",
            describe(&output)
        );
    }

    let help = stdout(&home.run_in(dir.path(), ["shell", "--help"]));
    assert!(contains_cjk(&help), "帮助是中文优先的：{help}");
    assert!(help.contains("--dry-run"), "{help}");
    assert!(help.contains("--json"), "{help}");
    assert!(
        help.contains("用法错误"),
        "长帮助里要写清这条约束是**用法错误**（退出码 2），而不是一句运行时才发现的怪事：{help}"
    );
    assert!(
        !help_declares(&help, "--fix"),
        "pin 这一族绝不提供 `--fix`（票据 #14 的硬要求）：{help}"
    );
    assert!(
        !help_declares(&help, "--ignore-lock"),
        "决策 116：不给逃生门开关，锁不一致就是停下来：{help}"
    );
    assert!(
        help.contains("--ignore-lock"),
        "「没有 `--ignore-lock`」这句承诺本身要写进长帮助 —— \
         用户找不到这个开关时，得能看到为什么没有：{help}"
    );

    let top = stdout(&home.run_in(dir.path(), ["--help"]));
    for name in ["shell", "auto", "trust", "lock"] {
        assert!(top.contains(name), "顶层帮助里缺少 `{name}`：{top}");
    }

    // 逃生门开关必须被**当场拒掉**，而不是静默忽略 ——
    // "参数被接受了但没起作用"是这一族最坏的失败形态。
    for command in ["shell", "auto", "lock"] {
        let output = home.run_in(dir.path(), [command, "--ignore-lock"]);
        assert_eq!(
            output.status.code(),
            Some(2),
            "`{command} --ignore-lock` 必须被拒掉：{}",
            describe(&output)
        );
    }
}

#[test]
fn two_dry_runs_are_byte_identical() {
    let home = IsolatedHome::new("pin-deterministic");
    let (dir, _) = project_with_lock("deterministic", &[("node", "24")]);

    let first = stdout(&home.run_in(dir.path(), ["shell", "--dry-run", "--json"]));
    let second = stdout(&home.run_in(dir.path(), ["shell", "--dry-run", "--json"]));
    assert_eq!(
        first, second,
        "两次 `shell --dry-run --json` 必须逐字节相同"
    );
    // 反面对照：上面那条断言不能被"两份空串相同"满足。
    assert!(first.len() > 200, "载荷短得可疑，先确认它不是空的：{first}");
}

// ---------------------------------------------------------------------------
// 4–10：信任门
// ---------------------------------------------------------------------------

#[test]
fn every_success_payload_is_free_of_chinese() {
    // 决策 35：`--json` 的成功载荷里不出现中文，`message` 键也不出现
    // （它天生是一句中文）。这一条对**四个命令、六个出口**都成立，
    // 所以在这里一次过跑一遍 —— 每个命令各写一条的结果是
    // "新加的那个出口忘了这一条"变成一件没人会发现的事。
    let home = IsolatedHome::new("pin-no-cjk");
    let (dir, _) = project_with_lock("no-cjk", &[("node", "24")]);
    let trust = home.run_in(dir.path(), ["trust"]);
    assert_eq!(trust.status.code(), Some(0), "{}", describe(&trust));

    for args in [
        vec!["shell", "--dry-run", "--json"],
        vec!["auto", "--dry-run", "--json"],
        vec!["lock", "--json"],
        vec!["lock", "--dry-run", "--json"],
        vec!["trust", "--json"],
        vec!["trust", "--list", "--json"],
    ] {
        let label = args.join(" ");
        let output = home.run_in(dir.path(), &args);
        assert_eq!(
            output.status.code(),
            Some(0),
            "`{label}`：{}",
            describe(&output)
        );
        let text = stdout(&output);
        assert!(!contains_cjk(&text), "`{label}` 的成功载荷里有 CJK：{text}");
        assert!(
            !text.contains("\"message\""),
            "`{label}` 的成功载荷里有 `message` 键：{text}"
        );
        // 反面对照：`json()` 在非 JSON 上会 panic，所以这一行同时证明
        // 上面那两条断言不是在检查一段随便什么文本。
        let _ = json(&output);
    }
}

#[test]
fn a_directory_without_a_pin_is_missing_pin() {
    let home = IsolatedHome::new("pin-missing");
    let dir = project("missing");
    let before = list_tree(dir.path());

    for args in [
        ["shell", "--dry-run", "--json"],
        ["auto", "--dry-run", "--json"],
        ["lock", "--dry-run", "--json"],
    ] {
        let output = home.run_in(dir.path(), args);
        assert_code(&output, "missing-pin");
    }
    assert_eq!(
        before,
        list_tree(dir.path()),
        "没有 `tuoen.toml` 的目录不该被写任何东西"
    );
}

#[test]
fn auto_refuses_an_untrusted_directory_and_writes_nothing() {
    let home = IsolatedHome::new("pin-untrusted");
    let (dir, _) = project_with_lock("untrusted", &[("node", "24")]);

    let before = list_tree(dir.path());
    let output = home.run_in(dir.path(), ["auto", "--dry-run", "--json"]);
    let message = assert_code(&output, "untrusted");
    assert!(
        message.contains("tuoen trust"),
        "消息要给出下一步该跑什么：{message}"
    );
    assert!(
        stderr(&output).contains("tuoen trust"),
        "决策 121：未信任时**即使 `--json` 也要往 stderr 写一句**：{}",
        describe(&output)
    );
    assert_eq!(
        before,
        list_tree(dir.path()),
        "被拒掉的 `auto` 在项目目录里留了东西：{}",
        describe(&output)
    );

    // `shell` 永远可用、**不问这道门**（决策 14）。
    let shell = home.run_in(dir.path(), ["shell", "--dry-run", "--json"]);
    assert_eq!(shell.status.code(), Some(0), "{}", describe(&shell));
    let data = json(&shell).data.expect("data");
    assert!(
        data.get("trust").is_none(),
        "`shell` 根本不问这道门，`trust` 键必须**省略**而不是恒为 null（见 `pin_view.rs`）：{data}"
    );
}

#[test]
fn trust_writes_the_isolated_manifest_and_leaves_the_real_one_alone() {
    let home = IsolatedHome::new("pin-trust-write");
    let (dir, _) = project_with_lock("trust-write", &[("node", "24")]);

    assert!(
        std::env::var_os("APPDATA").is_some(),
        "测试进程自己没有 `%APPDATA%` 的话，「真实清单没被动过」这条断言是空的 —— \
         一条不能失败的测量不是测量"
    );
    let real_before = real_roaming_listing();
    let isolated_before = list_tree(&home.roaming_app_data().join("tuoen"));

    let output = home.run_in(dir.path(), ["trust", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    let data = json(&output).data.expect("成功必须有 data");
    let fingerprint = data["fingerprint"]
        .as_str()
        .expect("fingerprint 是字符串")
        .to_owned();
    assert!(
        fingerprint.starts_with("sha256:") && fingerprint.len() == 7 + 64,
        "指纹是 `sha256:` + 64 位 hex（决策 117）：{data}"
    );
    assert!(same_path(data["path"].as_str().expect("path"), dir.path()));
    assert_eq!(data["action"], "trusted");

    // 隔离家目录里的清单**被写了**（判据的左半边）。
    let isolated_after = list_tree(&home.roaming_app_data().join("tuoen"));
    assert_ne!(
        isolated_before, isolated_after,
        "`trust` 没有往隔离家目录的清单里写东西"
    );
    assert!(
        home.trust_file().is_file(),
        "清单该落在 `<APPDATA>\\tuoen\\trust.toml`（决策 16 / 118），实际清单：{isolated_after:?}"
    );
    let manifest = std::fs::read_to_string(home.trust_file()).expect("读清单");
    assert!(
        manifest.contains(&fingerprint),
        "清单里必须有这次算出来的指纹：{manifest}"
    );

    // 判据的右半边：**测试进程真实的** `%APPDATA%\tuoen` 逐项未变。
    assert_eq!(
        real_before,
        real_roaming_listing(),
        "一次 `cargo test` 碰了开发者真实的信任清单 —— 这是本仓库最容易造成真实伤害的地方"
    );
}

#[test]
fn a_trusted_directory_passes_the_auto_gate() {
    let home = IsolatedHome::new("pin-auto-ok");
    let (dir, tool_dir) = project_with_lock("auto-ok", &[("node", "24")]);

    let trust = home.run_in(dir.path(), ["trust"]);
    assert_eq!(trust.status.code(), Some(0), "{}", describe(&trust));

    let output = home.run_in(dir.path(), ["auto", "--dry-run", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    let data = json(&output).data.expect("data");
    assert_eq!(data["trust"], "trusted", "{data}");
    assert_eq!(data["shell"], "cmd");
    let prepend = data["prepend"].as_array().expect("prepend");
    assert_eq!(prepend.len(), 1, "`prepend` 不许是空的：{data}");
    assert!(same_path(prepend[0].as_str().expect("路径"), &tool_dir));

    // `auto` 与 `shell` 共用同一个计划构造器（决策 121）：
    // 除了那道门（`trust` 这一个键），两份载荷必须**逐字节相同**。
    let shell = home.run_in(dir.path(), ["shell", "--dry-run", "--json"]);
    let mut shell_data = json(&shell).data.expect("data");
    let mut auto_data = data;
    for view in [&mut shell_data, &mut auto_data] {
        view.as_object_mut().expect("data 是对象").remove("trust");
    }
    assert_eq!(
        shell_data, auto_data,
        "`auto` 与 `shell` 的计划不一致 —— 它们必须共用同一个构造器"
    );
}

#[test]
fn a_crlf_pin_has_the_same_fingerprint() {
    let home = IsolatedHome::new("pin-crlf");
    let dir = project("crlf");
    let tool_dir = dir.path().join("tools").join("node");
    stub_tool(&tool_dir, "node");
    let lf = pin_body(&[("node", "24")]);

    // LF 版先信任。
    std::fs::write(dir.path().join(PIN_FILE_NAME), &lf).expect("写 LF");
    write_lock(dir.path(), &[("node", "24", "24.19.0", tool_dir)]);
    let trust = home.run_in(dir.path(), ["trust"]);
    assert_eq!(trust.status.code(), Some(0), "{}", describe(&trust));
    let trusted = json(&home.run_in(dir.path(), ["trust", "--list", "--json"]));
    let lf_fingerprint = trusted.data.expect("data")["entries"][0]["fingerprint"]
        .as_str()
        .expect("fingerprint")
        .to_owned();

    // 换成 CRLF：**逐字相同**的两份文本，只差行尾（决策 117 的归一化只做这一件事）。
    let crlf = lf.replace('\n', "\r\n");
    assert_eq!(
        crlf.replace("\r\n", "\n"),
        lf,
        "CRLF 版与 LF 版必须逐字相同，只差行尾"
    );
    assert_ne!(crlf, lf, "这条用例必须有 CRLF 才算数");
    std::fs::write(dir.path().join(PIN_FILE_NAME), &crlf).expect("写 CRLF");

    let output = home.run_in(dir.path(), ["auto", "--dry-run", "--json"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "行尾不该让指纹变化：{}",
        describe(&output)
    );
    assert_eq!(json(&output).data.expect("data")["trust"], "trusted");

    let listed = json(&home.run_in(dir.path(), ["trust", "--list", "--json"]));
    let crlf_fingerprint = listed.data.expect("data")["entries"][0]["fingerprint"]
        .as_str()
        .expect("fingerprint")
        .to_owned();
    assert_eq!(
        lf_fingerprint, crlf_fingerprint,
        "CRLF 与 LF 的指纹必须相同"
    );
}

#[test]
fn one_changed_character_makes_the_trust_stale() {
    let home = IsolatedHome::new("pin-stale");
    let (dir, _) = project_with_lock("stale", &[("node", "24")]);

    let trust = home.run_in(dir.path(), ["trust"]);
    assert_eq!(trust.status.code(), Some(0), "{}", describe(&trust));

    // 改**一个字符**：`24` → `25`。
    write_pin(dir.path(), &[("node", "25")]);
    // 锁也要跟着改，否则先红的是 `lock-mismatch` 而不是指纹 —— 那样这条用例
    // 就变成在测另一件事了。
    let tool_dir = dir.path().join("tools").join("node");
    write_lock(dir.path(), &[("node", "25", "24.19.0", tool_dir)]);

    let output = home.run_in(dir.path(), ["auto", "--dry-run", "--json"]);
    let message = assert_code(&output, "fingerprint-mismatch");
    assert!(
        message.contains("tuoen trust"),
        "消息要给出下一步该跑什么：{message}"
    );

    // 而且它**没有**顺手把新指纹记下来 —— 那等于把这道门变成一次点击确认。
    let listed = json(&home.run_in(dir.path(), ["trust", "--list", "--json"]));
    assert_eq!(
        listed.data.expect("data")["entries"][0]["state"],
        "stale",
        "被拒掉的 `auto` 不许偷偷更新指纹"
    );
}

#[test]
fn revoke_takes_the_entry_away() {
    let home = IsolatedHome::new("pin-revoke");
    let (dir, _) = project_with_lock("revoke", &[("node", "24")]);

    let trust = home.run_in(dir.path(), ["trust"]);
    assert_eq!(trust.status.code(), Some(0), "{}", describe(&trust));

    let path_text = dir.path().display().to_string();
    let output = home.run_in(dir.path(), ["trust", "--revoke", &path_text, "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    let data = json(&output).data.expect("data");
    assert_eq!(data["action"], "revoked", "{data}");

    // 回到未信任。
    let auto = home.run_in(dir.path(), ["auto", "--dry-run", "--json"]);
    assert_code(&auto, "untrusted");

    // 清单里那条**消失**了（不是被标成 stale）。
    let listed = json(&home.run_in(dir.path(), ["trust", "--list", "--json"]));
    let data = listed.data.expect("data");
    assert!(
        data["entries"].as_array().expect("entries").is_empty(),
        "被摘掉的那条还在清单里：{data}"
    );

    // 再摘一次：报告 `absent`，而且**不许假装成功**。
    let again = home.run_in(dir.path(), ["trust", "--revoke", &path_text, "--json"]);
    assert_eq!(again.status.code(), Some(0), "{}", describe(&again));
    let data = json(&again).data.expect("data");
    assert_eq!(data["action"], "absent", "{data}");
}

// ---------------------------------------------------------------------------
// 11–12：锁与解析的失败
// ---------------------------------------------------------------------------

#[test]
fn a_lock_that_disagrees_with_the_pin_refuses_to_start() {
    let home = IsolatedHome::new("pin-lock-mismatch");
    let dir = project("lock-mismatch");
    let tool_dir = dir.path().join("tools").join("node");
    stub_tool(&tool_dir, "node");
    write_pin(dir.path(), &[("node", "24")]);
    // 声明 `24`、锁里却是 `20` —— 用户改了声明却忘了重算锁。
    write_lock(dir.path(), &[("node", "20", "20.19.0", tool_dir)]);

    let before = list_tree(dir.path());
    let output = home.run_in(dir.path(), ["shell", "--dry-run", "--json"]);
    let message = assert_code(&output, "lock-mismatch");
    assert!(
        message.contains("node"),
        "消息要点名哪一条不一致：{message}"
    );
    assert!(
        message.contains("tuoen lock"),
        "消息要给出下一条命令：{message}"
    );
    assert_eq!(
        before,
        list_tree(dir.path()),
        "被拒掉的 `shell` 不许改写锁文件"
    );
}

#[test]
fn a_version_that_is_not_installed_names_the_install_command() {
    let home = IsolatedHome::new("pin-not-installed");
    let dir = project("not-installed");
    // 一个几乎不可能装在这台机器上的版本：解析必然失败，而失败路径
    // **不依赖**这台机器上装了什么。
    write_pin(dir.path(), &[("node", "0.0.1")]);

    let output = home.run_in(dir.path(), ["shell", "--dry-run", "--json"]);
    let message = assert_code(&output, "version-not-installed");
    assert!(
        message.contains("tuoen install node@0.0.1"),
        "消息要给出下一步该跑什么（票据 #14 的硬要求）：{message}"
    );
    assert!(
        !dir.path().join(LOCK_FILE_NAME).exists(),
        "解析失败时**不许**留下锁（决策 116）：一份描述「半个工具链」的锁比没有锁更糟"
    );

    // 同一条缺口也要拦 `lock`（同一个解析器，同一个判断）。
    let lock = home.run_in(dir.path(), ["lock", "--dry-run", "--json"]);
    assert_code(&lock, "version-not-installed");
    assert!(!dir.path().join(LOCK_FILE_NAME).exists());
}

#[test]
fn an_unknown_tool_id_is_a_hard_error() {
    let home = IsolatedHome::new("pin-unknown");
    let dir = project("unknown");
    write_pin(dir.path(), &[("nosuchtool", "1")]);

    let output = home.run_in(dir.path(), ["shell", "--dry-run", "--json"]);
    let message = assert_code(&output, "unknown-tool");
    assert!(
        message.contains("nosuchtool"),
        "消息要点名那个不认识的 id：{message}"
    );
    assert!(
        message.contains("node"),
        "消息要列出认识的 id（决策 113 的硬要求）：{message}"
    );
}

// ---------------------------------------------------------------------------
// 13：只读
// ---------------------------------------------------------------------------

#[test]
fn the_dry_runs_touch_neither_the_store_nor_any_junction() {
    let home = IsolatedHome::new("pin-read-only");
    let (dir, _) = project_with_lock("read-only", &[("node", "24")]);

    // 先在隔离家目录里**造出非空的 store 与 shims**，否则"跑完前后相同"
    // 是一条空断言（两边的清单都是空的，什么都不会红）。
    install_stub(&home, "node", "24.19.0");
    std::fs::create_dir_all(home.tuoen_home().join("shims")).expect("建 shims");
    std::fs::write(home.tuoen_home().join("shims").join("node.cmd"), b"").expect("建 shim");

    // `trust` 会写清单，所以先做完它再拍快照。
    let trust = home.run_in(dir.path(), ["trust"]);
    assert_eq!(trust.status.code(), Some(0), "{}", describe(&trust));

    let local_before = list_tree(&home.tuoen_home());
    let roaming_before = list_tree(&home.roaming_app_data().join("tuoen"));
    let dir_before = list_tree(dir.path());
    assert!(
        local_before
            .iter()
            .any(|line| line.contains("store/node/versions")),
        "store 快照是空的，这条断言什么都不测：{local_before:?}"
    );
    assert!(
        local_before.iter().any(|line| line.contains("shims")),
        "shims 快照是空的：{local_before:?}"
    );

    for args in [
        vec!["shell", "--dry-run"],
        vec!["shell", "--dry-run", "--json"],
        vec!["auto", "--dry-run"],
        vec!["auto", "--dry-run", "--json"],
        vec!["lock", "--dry-run"],
    ] {
        let output = home.run_in(dir.path(), &args);
        assert_eq!(
            output.status.code(),
            Some(0),
            "`{}`：{}",
            args.join(" "),
            describe(&output)
        );
    }

    let local_after = list_tree(&home.tuoen_home());
    assert_eq!(
        local_before, local_after,
        "`--dry-run` 碰了隔离家目录里的 `tuoen`（含 store 与 shims）"
    );
    assert_eq!(
        roaming_before,
        list_tree(&home.roaming_app_data().join("tuoen")),
        "`--dry-run` 碰了信任清单"
    );
    assert!(
        !local_after.iter().any(|line| line.contains("[junction")),
        "跑完之后隔离家目录里多出了重解析点（决策 119：绝不翻转任何 junction）：{local_after:?}"
    );

    // 项目目录本身也不许多出东西：有锁就不重算锁，`--dry-run` 更不写锁。
    assert_eq!(
        dir_before,
        list_tree(dir.path()),
        "`--dry-run` 在项目目录里留了东西"
    );
}

// ---------------------------------------------------------------------------
// 14–15：子进程
// ---------------------------------------------------------------------------

#[test]
fn a_too_deep_nesting_is_refused_before_anything_starts() {
    let home = IsolatedHome::new("pin-depth");
    let (dir, _) = project_with_lock("depth", &[("node", "24")]);

    // 深度是**计划的性质**，所以连 `--dry-run` 也要拦：一份跑不了的计划的预览
    // 会让人以为它跑得了。
    let dry = home
        .command()
        .current_dir(dir.path())
        .env("TUOEN_SHELL_DEPTH", "9")
        .args(["shell", "--dry-run", "--json"])
        .output()
        .expect("跑 tuoen");
    let message = assert_code(&dry, "shell-depth");
    assert!(
        message.contains("TUOEN_SHELL_DEPTH"),
        "消息要说清判据是哪个环境变量：{message}"
    );
    assert!(
        json(&dry).data.is_none(),
        "被拒掉时不该给一份看起来能跑的计划：{}",
        describe(&dry)
    );

    // 不带 `--dry-run` 也一样拒绝，而且**一个子进程都没起** ——
    // 判据是 `--exec "exit 7"`：真起了的话退出码会是 7。
    let real = home
        .command()
        .current_dir(dir.path())
        .env("TUOEN_SHELL_DEPTH", "9")
        .args(["shell", "--exec", "exit 7"])
        .stdin(Stdio::null())
        .output()
        .expect("跑 tuoen");
    assert_eq!(
        real.status.code(),
        Some(1),
        "深度超限时拒绝必须发生在 spawn **之前**：{}",
        describe(&real)
    );

    // 深度是 `MAX_SHELL_DEPTH` 之内就不拦（决策：`too_deep` 是 `depth > 5`）。
    let ok = home
        .command()
        .current_dir(dir.path())
        .env("TUOEN_SHELL_DEPTH", "5")
        .args(["shell", "--dry-run", "--json"])
        .output()
        .expect("跑 tuoen");
    assert_eq!(ok.status.code(), Some(0), "{}", describe(&ok));
    let data = json(&ok).data.expect("data");
    assert_eq!(data["depth"], 5, "{data}");
}

#[test]
fn exec_propagates_the_child_exit_code_and_does_not_hang() {
    let home = IsolatedHome::new("pin-exec");
    let (dir, _) = project_with_lock("exec", &[("node", "24")]);

    // 20 秒：这台机器上 `cmd.exe /C exit 7` 的几千倍，所以超时只可能是真的挂住了。
    let timeout = Duration::from_secs(20);

    let seven = run_with_timeout(&home, dir.path(), &["shell", "--exec", "exit 7"], timeout);
    assert_eq!(
        seven.status.code(),
        Some(7),
        "子 shell 的退出码必须原样透传：{}",
        describe(&seven)
    );

    // 反面对照：上一条不能被"任何非零都算过"满足。
    let zero = run_with_timeout(&home, dir.path(), &["shell", "--exec", "exit 0"], timeout);
    assert_eq!(zero.status.code(), Some(0), "{}", describe(&zero));

    // `auto` 走的是同一个启动路径（决策 121），所以它也要能起来 ——
    // 前提是这道门开着。
    let trust = home.run_in(dir.path(), ["trust"]);
    assert_eq!(trust.status.code(), Some(0), "{}", describe(&trust));
    let auto = run_with_timeout(&home, dir.path(), &["auto", "--exec", "exit 3"], timeout);
    assert_eq!(auto.status.code(), Some(3), "{}", describe(&auto));

    // `--shell powershell` 也走同一条路：`powershell.exe` 不在 `System32` 里，
    // 所以这条用例真正在测的是"程序名取的是绝对路径、没被换掉的 `PATH` 影响"。
    let ps = run_with_timeout(
        &home,
        dir.path(),
        &["shell", "--shell", "powershell", "--exec", "exit 4"],
        timeout,
    );
    assert_eq!(ps.status.code(), Some(4), "{}", describe(&ps));
}

#[test]
fn the_child_shell_really_gets_the_prepended_path_and_the_depth() {
    // 这一条把决策 119 的核心承诺**端到端**钉住：子进程拿到的 `PATH` 真的被前置了。
    //
    // brief 原计划把"前置真的生效"交给真机验收，理由是"需要一个真装好的工具"。
    // 但判据其实不需要工具：`cmd.exe` 自己会把 `%PATH%` 展开成它**环境里**的那一条，
    // 而子进程的 stdout 是**继承**的（`spawn_inherit` 的存在理由），
    // 所以它会直接落在我们的 stdout 上。于是这条可以完全不依赖机器状态 ——
    // 那就该在契约测试里，而不是留在一个只有真机才跑得起来的脚本里。
    let home = IsolatedHome::new("pin-child-env");
    let (dir, tool_dir) = project_with_lock("child-env", &[("node", "24")]);

    let output = run_with_timeout(
        &home,
        dir.path(),
        &["shell", "--exec", "echo %PATH%"],
        Duration::from_secs(20),
    );
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));

    let echoed = stdout(&output);
    let child_path = echoed
        .lines()
        .find(|line| line.contains(&tool_dir.display().to_string()))
        .unwrap_or_else(|| panic!("子进程的 `PATH` 里根本没有我们前置的那个目录：{echoed}"));
    let first = child_path.split(';').next().expect("`PATH` 的第一条");
    assert!(
        same_path(first, &tool_dir),
        "前置目录必须是**第一条**（决策 119：前置，不是追加）：{child_path}"
    );

    // 深度也要传下去：子 shell 里的深度是"父 + 1"，否则嵌套永远拦不住
    // （拦的是 `plan.depth()`，而它读的正是这个变量）。
    let depth = run_with_timeout(
        &home,
        dir.path(),
        &["shell", "--exec", "echo %TUOEN_SHELL_DEPTH%"],
        Duration::from_secs(20),
    );
    assert_eq!(depth.status.code(), Some(0), "{}", describe(&depth));
    // 反面对照：变量没设时 `cmd` 会把 `%TUOEN_SHELL_DEPTH%` **原样**印出来，
    // 而那串字里没有 `1` —— 所以这条断言不是在"总能过"。
    assert_eq!(
        stdout(&depth).trim(),
        "1",
        "子 shell 里的深度必须是 1（父 0 + 1）：{}",
        describe(&depth)
    );
}

#[test]
fn exec_keeps_the_quotes_that_cmd_needs() {
    // **`cmd.exe /C` 的载荷必须原样交给 cmd**（`ShellArg::Raw` + `Command::raw_arg`）。
    //
    // 这条用例在旧实现下**必须红**：Rust 的 `Command::arg` 会把含引号的参数转义成
    // `\"`，而 `cmd.exe` 不认这种转义 —— 真机实测是"输出为空、退出码 0"，
    // 一句看起来完全合理的成功。`echo "hi"` 把这件事变得可判：
    // 原样交给 cmd → 印出 `"hi"`；被转义 → 印出 `\"hi\"`。
    let home = IsolatedHome::new("pin-exec-quotes");
    let (dir, _) = project_with_lock("exec-quotes", &[("node", "24")]);

    let output = run_with_timeout(
        &home,
        dir.path(),
        &["shell", "--exec", "echo \"hi\""],
        Duration::from_secs(20),
    );
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    let text = stdout(&output);
    assert!(
        text.contains("\"hi\""),
        "cmd 收到的引号必须是它认识的那一种：{}",
        describe(&output)
    );
    assert!(
        !text.contains("\\\""),
        "载荷被 MSVCRT 规则转义了（`\\\"` 是 cmd 不认的形状）：{}",
        describe(&output)
    );
}

#[test]
fn exec_prints_nothing_into_the_envelope() {
    // 子进程**继承** stdio（`spawn_inherit` 的存在理由），所以它的输出会直接
    // 落到我们的 stdout 上。这条用例钉住的是：我们**没有**把它捕获回来再包一层。
    let home = IsolatedHome::new("pin-exec-stdio");
    let (dir, _) = project_with_lock("exec-stdio", &[("node", "24")]);

    let output = run_with_timeout(
        &home,
        dir.path(),
        &["shell", "--exec", "echo tuoen-child-stdout"],
        Duration::from_secs(20),
    );
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    let text = stdout(&output);
    assert!(
        text.contains("tuoen-child-stdout"),
        "子进程的输出应当直接出现在 stdout 上：{}",
        describe(&output)
    );
    assert!(
        !text.trim_start().starts_with('{'),
        "`shell` 不带 `--dry-run` 时**不是** `--json`，不该有信封：{}",
        describe(&output)
    );
}

// ---------------------------------------------------------------------------
// 16–18：计划的人类输出、遮蔽警告、`trust --list` 的状态重算
// ---------------------------------------------------------------------------

#[test]
fn a_command_that_did_not_take_effect_is_warned_about_on_stderr() {
    let home = IsolatedHome::new("pin-shadow");
    let dir = project("shadow");
    // 前置目录**建出来但不放命令**：这就是决策 120 要检测的那个形态
    // ——"前置了却没生效"。
    let tool_dir = dir.path().join("tools").join("node");
    std::fs::create_dir_all(&tool_dir).expect("建空的前置目录");
    write_pin(dir.path(), &[("node", "24")]);
    write_lock(dir.path(), &[("node", "24", "24.19.0", tool_dir)]);

    let output = home.run_in(dir.path(), ["shell", "--dry-run", "--json"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "遮蔽是**警告**不是失败（决策 120）：{}",
        describe(&output)
    );

    let data = json(&output).data.expect("data");
    let warnings = data["warnings"].as_array().expect("warnings");
    assert!(
        !warnings.is_empty(),
        "前置目录里一个命令都没有，必须报出来：{data}"
    );
    assert!(
        warnings.iter().any(|item| item["command"] == "node"),
        "警告要含命令名：{data}"
    );
    for item in warnings {
        // `winner` / `entry` 用 `null` 而不是省略键：`null` 是一个**答案**
        // （"整条 PATH 上没有任何目录有这个命令"），决策 120 要求它单独可辨。
        assert!(
            item.get("winner").is_some() && item.get("entry").is_some(),
            "警告的形状不对：{item}"
        );
    }

    let err = stderr(&output);
    assert!(err.contains("node"), "stderr 上要出现命令名：{err}");
    assert!(
        contains_cjk(&err),
        "警告是给人看的中文（`--json` 只管 stdout）：{err}"
    );
    assert!(
        !contains_cjk(&stdout(&output)),
        "警告**不许**混进 stdout 的 JSON：{}",
        stdout(&output)
    );
}

#[test]
fn the_human_plan_output_is_chinese_and_mentions_the_decision_facts() {
    let home = IsolatedHome::new("pin-human");
    let (dir, tool_dir) = project_with_lock("human", &[("node", "24")]);

    let output = home.run_in(dir.path(), ["shell", "--dry-run"]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    let text = stdout(&output);
    assert!(contains_cjk(&text), "人话是中文优先的：{text}");
    assert!(text.contains("node"), "{text}");
    assert!(
        text.contains(&tool_dir.display().to_string()),
        "计划要写出它会前置哪个目录：{text}"
    );
    assert!(text.contains("24.19.0"), "计划要写出解析到的版本：{text}");
    assert!(
        !text.trim_start().starts_with('{'),
        "不带 `--json` 时不该有信封：{text}"
    );
    assert!(
        stderr(&output).is_empty(),
        "四个命令都在前置目录里、没有遮蔽，stderr 该是干净的：{}",
        describe(&output)
    );
}

#[test]
fn trust_list_recomputes_every_state_on_the_spot() {
    let home = IsolatedHome::new("pin-list");
    let dir = project("list");
    let tool_dir = dir.path().join("tools").join("node");
    stub_tool(&tool_dir, "node");
    write_pin(dir.path(), &[("node", "24")]);
    write_lock(dir.path(), &[("node", "24", "24.19.0", tool_dir)]);

    // 空清单也是合法状态。
    let empty = json(&home.run_in(dir.path(), ["trust", "--list", "--json"]));
    let data = empty.data.expect("data");
    assert!(same_path(
        data["trustFile"].as_str().expect("trustFile"),
        &home.trust_file()
    ));
    assert!(data["entries"].as_array().expect("entries").is_empty());

    let trust = home.run_in(dir.path(), ["trust"]);
    assert_eq!(trust.status.code(), Some(0), "{}", describe(&trust));

    let listed = json(&home.run_in(dir.path(), ["trust", "--list", "--json"]));
    let data = listed.data.expect("data");
    let entries = data["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 1, "{data}");
    for key in ["path", "fingerprint", "trustedAt", "state"] {
        assert!(entries[0].get(key).is_some(), "缺少字段 `{key}`：{data}");
    }
    assert!(same_path(
        entries[0]["path"].as_str().expect("path"),
        dir.path()
    ));
    assert_eq!(entries[0]["state"], "trusted");
    assert!(
        entries[0]["trustedAt"]
            .as_str()
            .expect("trustedAt")
            .ends_with('Z'),
        "时间戳是 RFC3339 UTC：{data}"
    );

    // `--list` 报的是**它现在**还有不有效，不是写盘那一刻的结论。
    write_pin(dir.path(), &[("node", "25")]);
    let listed = json(&home.run_in(dir.path(), ["trust", "--list", "--json"]));
    assert_eq!(
        listed.data.expect("data")["entries"][0]["state"],
        "stale",
        "`--list` 必须现场重算状态（决策 118）"
    );

    // 声明文件被删掉 → `missing-file`（不是 `stale`）。
    std::fs::remove_file(dir.path().join(PIN_FILE_NAME)).expect("删 tuoen.toml");
    let listed = json(&home.run_in(dir.path(), ["trust", "--list", "--json"]));
    assert_eq!(
        listed.data.expect("data")["entries"][0]["state"],
        "missing-file",
        "缺文件与内容变化是两件事，`state` 要分开报：{}",
        describe(&home.run_in(dir.path(), ["trust", "--list", "--json"]))
    );

    // 人话版也要印得出来。
    let human = home.run_in(dir.path(), ["trust", "--list"]);
    assert_eq!(human.status.code(), Some(0), "{}", describe(&human));
    assert!(contains_cjk(&stdout(&human)), "{}", stdout(&human));
}

#[test]
fn lock_writes_the_resolution_and_dry_run_does_not() {
    let home = IsolatedHome::new("pin-lock");
    let dir = project("lock");
    // 用**隔离家目录的 store** 造一个已装版本：候选来自 store + 检测到的第三方
    // （决策 114），而 store 那一半完全由测试决定，于是这条用例与这台机器上
    // 装了哪个第三方 node 无关。
    install_stub(&home, "node", "24.19.0");
    write_pin(dir.path(), &[("node", "24")]);

    let before = list_tree(dir.path());
    let dry = home.run_in(dir.path(), ["lock", "--dry-run", "--json"]);
    assert_eq!(dry.status.code(), Some(0), "{}", describe(&dry));
    assert_eq!(
        before,
        list_tree(dir.path()),
        "`lock --dry-run` 写了锁：{}",
        describe(&dry)
    );
    let data = json(&dry).data.expect("data");
    assert!(
        same_path(
            data["lockFile"].as_str().expect("lockFile"),
            &dir.path().join(LOCK_FILE_NAME)
        ),
        "{data}"
    );
    assert_eq!(data["written"], false, "{data}");
    let tools = data["tools"].as_array().expect("tools");
    assert_eq!(tools.len(), 1, "{data}");
    assert_eq!(tools[0]["name"], "node");
    assert_eq!(tools[0]["spec"], "24");
    assert_eq!(tools[0]["version"], "24.19.0");
    assert_eq!(tools[0]["source"], "tuoen");

    // 真的写一次。
    let real = home.run_in(dir.path(), ["lock", "--json"]);
    assert_eq!(real.status.code(), Some(0), "{}", describe(&real));
    assert_eq!(json(&real).data.expect("data")["written"], true);
    let lock_path = dir.path().join(LOCK_FILE_NAME);
    assert!(lock_path.is_file(), "`lock` 必须真的写出锁");
    let lock = LockFile::load(&lock_path)
        .expect("读锁")
        .expect("锁文件存在");
    assert_eq!(lock.tool.len(), 1, "{lock:?}");
    assert_eq!(lock.tool[0].name, "node");
    assert_eq!(lock.tool[0].spec, "24");
    assert_eq!(lock.tool[0].version, "24.19.0");

    // 锁里**没有时间戳**（决策 115）：同样的输入写两次必须逐字节相同。
    let first = std::fs::read(&lock_path).expect("读锁");
    let again = home.run_in(dir.path(), ["lock"]);
    assert_eq!(again.status.code(), Some(0), "{}", describe(&again));
    let second = std::fs::read(&lock_path).expect("读锁");
    assert_eq!(
        first, second,
        "锁里不许有时间戳 —— 它要进 git，两次逐字节相同才是判据"
    );

    // 有锁之后 `shell` 直接用锁，不再解析（于是也不需要 store 里有东西）。
    let shell = home.run_in(dir.path(), ["shell", "--dry-run", "--json"]);
    assert_eq!(shell.status.code(), Some(0), "{}", describe(&shell));
    assert_eq!(json(&shell).data.expect("data")["lockWritten"], false);
}

#[test]
fn the_first_shell_run_writes_the_lock_it_resolved() {
    let home = IsolatedHome::new("pin-first-run");
    let dir = project("first-run");
    let version_dir = install_stub(&home, "node", "24.19.0");
    write_pin(dir.path(), &[("node", "24")]);

    assert!(
        !dir.path().join(LOCK_FILE_NAME).exists(),
        "起点必须是没有锁"
    );

    // 真起一次子 shell：这条用例同时验证"没有锁时现场解析 + 写锁"与
    // "写出来的锁立刻可用"。超时给得比别处宽，因为这一次真的会跑检测
    // （每条工具一个探针，每个探针 3 秒上限）。
    let output = run_with_timeout(
        &home,
        dir.path(),
        &["shell", "--exec", "exit 0"],
        Duration::from_secs(60),
    );
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));

    let lock_path = dir.path().join(LOCK_FILE_NAME);
    assert!(
        lock_path.is_file(),
        "没有锁时 `shell` 必须写出锁（决策 116）"
    );
    let lock = LockFile::load(&lock_path)
        .expect("读锁")
        .expect("锁文件存在");
    assert_eq!(lock.tool.len(), 1, "{lock:?}");
    assert_eq!(lock.tool[0].name, "node");
    assert_eq!(lock.tool[0].spec, "24");
    assert_eq!(lock.tool[0].version, "24.19.0");
    assert_eq!(lock.tool[0].source, "tuoen");
    assert!(
        same_path(&lock.tool[0].path, &version_dir),
        "锁里的路径必须是解析出来的那个目录：{lock:?}"
    );
    // 没有 `.install.json` 记录 → 保守地前置版本目录本身，不猜一个 `bin` 出来。
    assert_eq!(lock.tool[0].hash, None, "{lock:?}");

    // 第二次跑：有锁就用锁，`lockWritten` 变成 false。
    let second = home.run_in(dir.path(), ["shell", "--dry-run", "--json"]);
    assert_eq!(second.status.code(), Some(0), "{}", describe(&second));
    let data = json(&second).data.expect("data");
    assert_eq!(data["lockWritten"], false, "{data}");
    assert!(same_path(
        data["prepend"][0].as_str().expect("prepend"),
        &version_dir
    ));
}
