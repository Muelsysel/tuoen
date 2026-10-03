//! `tuoen shim add` / `remove` / `list` / `path` 的**进程边界**契约测试。
//!
//! ## 这一票为什么必须走进程边界
//!
//! shim 生成器本身在 `tuoen-shim` 里有单元测试（槽位布局、引号化、脚本目标被拒），
//! 但**只有跑真实二进制才能回答票据真正问的问题**："用户敲 `tuoen shim add node`
//! 之后，`PATH` 那个目录里到底多了什么、它们各自会跑什么"。参数解析、模板查找、
//! 逐条落盘、退出码、`--json` 形状，全都只有在这一层才看得见。
//!
//! ## 三条硬性约束
//!
//! 1. **绝不碰真实的 `%LOCALAPPDATA%`**：每个用例都用 [`IsolatedHome`]，
//!    它把 `LOCALAPPDATA` / `APPDATA` 指到临时目录。这一票比票据 #6 更要紧 ——
//!    `shim add` 放的是**可执行文件**，不隔离就等于往开发者的 `PATH` 目录里扔东西。
//! 2. **绝不执行生成出来的 shim。** 载荷里的 `node.exe` 是一个内容为
//!    `fake payload` 的普通文件，本文件**只读它、从不运行它**。
//!    票据 #7 期间曾经写过一个"shim 的目标是它自己"的测试，堆出 20580 个进程
//!    把机器内存吃干 —— 生成物是**数据**，不是可以运行的东西。
//! 3. **模板必须是真的 `tuoen-shim.exe`**：靠 `TUOEN_SHIM_TEMPLATE` 指到
//!    `target/<profile>/tuoen-shim.exe`（发行时它才与 `tuoen.exe` 并列）。
//!    `cargo test -p tuoen-cli` 不会构建另一个 crate 的 bin，所以
//!    [`template_path`] 会先断言它存在，不满足时给一句人能看懂的提示。

mod common;

use std::path::{Path, PathBuf};
use std::process::Output;

use common::{IsolatedHome, json, list_files, stderr, stdout};
use serde_json::json as value;
use tuoen_shim::{SLOT_MAGIC, TEMPLATE_ENV_VAR, TEMPLATE_FILE_NAME, decode_slot, prefix_target};
use tuoen_store::Store;

/// 假 node 的版本号。
const NODE_VERSION: &str = "24.19.0";

/// 一个真 node 安装目录里**该有的**四个载荷文件。
///
/// 与 `tuoen_core::shim_commands("node")` 的四条命令一一对应 ——
/// 少一个就会有一条 shim 以 `target-unreadable` 失败（那正是 corepack 在旧 Node 上的样子）。
const NODE_PAYLOAD: [&str; 4] = [
    "node.exe",
    "node_modules/npm/bin/npm-cli.js",
    "node_modules/npm/bin/npx-cli.js",
    "node_modules/corepack/dist/corepack.js",
];

// ─────────────────────────────────────────────────────────────────────────────
// 测试基础设施
// ─────────────────────────────────────────────────────────────────────────────

/// 真模板二进制的路径：`target/<profile>/tuoen-shim.exe`。
///
/// `env!("CARGO_BIN_EXE_tuoen-shim")` 在 **CLI** 的测试里拿不到另一个 crate 的 bin
/// （那个宏只对本包的 bin 有效），所以从测试可执行文件往上找两级：
/// `target/<profile>/deps/shim_contract-<hash>.exe` → `target/<profile>/`。
fn template_path() -> PathBuf {
    let exe = std::env::current_exe().expect("当前测试可执行文件的路径");
    let profile_dir = exe
        .parent()
        .and_then(Path::parent)
        .expect("测试可执行文件应当在 target/<profile>/deps/ 下")
        .to_path_buf();
    let template = profile_dir.join(TEMPLATE_FILE_NAME);
    assert!(
        template.is_file(),
        "找不到 shim 模板 `{}`。\n\
         它是 `tuoen-shim` 这个 bin 的产物，而 `cargo test -p tuoen-cli`\
         **不会**构建另一个 crate 的 bin。\n\
         先跑一次 `cargo test --workspace`（或 `cargo build --workspace`）再重试。",
        template.display()
    );
    template
}

/// 跑一次 `tuoen shim …`：环境指向隔离的家目录，并且告诉它模板在哪。
///
/// `IsolatedHome::run` 不带模板环境变量，所以这里自己拼 —— 但用的仍然是它给的路径，
/// **绝不碰真实的 `%LOCALAPPDATA%`**。
fn run_shim(home: &IsolatedHome, args: &[&str]) -> Output {
    common::tuoen()
        .args(args)
        .env("LOCALAPPDATA", home.local_app_data())
        .env("APPDATA", home.roaming_app_data())
        .env(TEMPLATE_ENV_VAR, template_path())
        .output()
        .expect("运行 tuoen 应当成功")
}

/// shim 目录：`<隔离的 LOCALAPPDATA>\tuoen\shims`。
///
/// **在测试里自己拼是刻意的**：测试要知道"东西应该落在哪"才能断言磁盘副作用，
/// 而向生产代码问路径等于让测试跟着实现走。
fn shims_dir(home: &IsolatedHome) -> PathBuf {
    home.local_app_data().join("tuoen").join("shims")
}

/// 把相对路径拼到根下（只认 `/`，与载荷里的写法一致）。
fn join_relative(root: &Path, relative: &str) -> PathBuf {
    let mut out = root.to_path_buf();
    for part in relative.split('/').filter(|part| !part.is_empty()) {
        out.push(part);
    }
    out
}

/// 在隔离的存储里造一个"装好的 node"：版本目录 + 四个载荷文件。
///
/// **不写记录文件**：目录是事实来源（决策 48）。
fn fake_installed_node(home: &IsolatedHome, version: &str) {
    let store = Store::new(home.store_root());
    let dir = store.version_dir("node", version);
    std::fs::create_dir_all(&dir).expect("造版本目录");
    std::fs::write(dir.join("marker.txt"), "installed by the test").expect("写 marker");
    for relative in NODE_PAYLOAD {
        let path = join_relative(&dir, relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("造载荷父目录");
        }
        std::fs::write(&path, b"fake payload, never executed").expect("写载荷文件");
    }
}

/// 造好 node 并**用真的 `tuoen use` 激活它** —— 于是 `current` 是一个真的 junction，
/// 而不是测试假装出来的普通目录（shim 指向它，所以这里越真越好）。
fn activate_node(home: &IsolatedHome, version: &str) {
    fake_installed_node(home, version);
    let output = home.run(["use", "node", version, "--json"]);
    assert!(
        output.status.success(),
        "`use` 应当成功：{}",
        describe(&output)
    );
}

/// 一条命令的**两路输出**都要看。
///
/// `--json` 的错误走 **stdout**（那是信封的规矩），所以"失败了但 stderr 是空的"
/// 完全正常 —— 只打印 stderr 会得到一句没有信息量的断言失败。
fn describe(output: &Output) -> String {
    format!("stdout：{} stderr：{}", stdout(output), stderr(output))
}

/// 从一个 shim 文件里解出烘着的完整前缀。
///
/// 规则与 `tuoen shim list` 一致：**扫 `SLOT_MAGIC` 的每一处，逐处试 `decode_slot`，
/// 取第一个成功的**。不能"找到第一处魔数就下手"—— 模板里那个还没烘过的槽位也有魔数，
/// 而编译器可能把同一个常量数组复制到多处（`pristine_slot_offsets` 的文档里有实测数字）。
fn read_prefix(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_else(|err| panic!("读 {}：{err}", path.display()));
    assert!(
        bytes.len() >= SLOT_MAGIC.len(),
        "`{}` 只有 {} 字节，比魔数还短",
        path.display(),
        bytes.len()
    );
    (0..=bytes.len() - SLOT_MAGIC.len())
        .filter(|at| bytes[*at..*at + SLOT_MAGIC.len()] == SLOT_MAGIC)
        .find_map(|at| decode_slot(&bytes[at..]))
        .unwrap_or_else(|| panic!("`{}` 里没有能解出来的槽位", path.display()))
}

fn contains_cjk(text: &str) -> bool {
    text.chars()
        .any(|c| (0x4E00..=0x9FFF).contains(&(u32::from(c))))
}

/// 真实的 `%LOCALAPPDATA%\tuoen` 下现在有哪些名字。
fn real_home_listing() -> Vec<String> {
    let Some(local) = std::env::var_os("LOCALAPPDATA") else {
        return Vec::new();
    };
    let home = PathBuf::from(local).join("tuoen");
    let Ok(entries) = std::fs::read_dir(&home) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    // **`shims` 要往里看一层。** 只列顶层的话，本票最严重的那一类事故看不见：
    // "在真实的 shim 目录里生成了一个 shim" —— 因为 `shims` 这个名字本来就在那儿，
    // 空目录和 4 条 shim 的顶层列表长得一模一样。
    let mut inner: Vec<String> = std::fs::read_dir(home.join("shims"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    inner.sort();
    names.push(format!("shims/{{{}}}", inner.join(", ")));
    names
}

// ─────────────────────────────────────────────────────────────────────────────
// add：拒绝的路径
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn add_without_an_active_version_says_to_install_with_use() {
    // 一台什么都没有的机器上，`shim add node` 必须给出**能照着敲**的下一步，
    // 而不是一句"没有生效版本"。
    let home = IsolatedHome::new("shim-add-no-active");
    let output = run_shim(&home, &["shim", "add", "node", "--json"]);
    assert_eq!(output.status.code(), Some(1), "拒绝必须是退出码 1");

    let envelope = json(&output);
    assert!(!envelope.ok);
    let error = envelope.error.expect("失败必须有 error");
    assert_eq!(error.code, "no-active-version", "错误码是稳定契约");
    assert!(
        error.message.contains("--use"),
        "必须给出 `tuoen install … --use` 这条出路：{}",
        error.message
    );
    assert!(
        error.message.contains("tuoen install node"),
        "要带上工具名，否则用户还得自己拼：{}",
        error.message
    );
    assert!(!shims_dir(&home).exists(), "拒绝路径上不该创建 shim 目录");
}

#[test]
fn add_with_a_version_that_was_never_installed_lists_what_is() {
    let home = IsolatedHome::new("shim-add-not-installed");
    fake_installed_node(&home, NODE_VERSION);

    let output = run_shim(&home, &["shim", "add", "node@24.21.0", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let error = json(&output).error.expect("失败必须有 error");
    assert_eq!(error.code, "not-installed");
    assert!(
        error.message.contains(NODE_VERSION),
        "要列出真正装过的版本：{}",
        error.message
    );
}

#[test]
fn add_for_a_tool_we_have_not_measured_refuses_instead_of_guessing() {
    // **这一条是"宁可不发，也不猜"的出口。**
    //
    // 用 `temurin`（内置目录里真的有的那个工具，别名 `java`）—— 它能装、能激活，
    // 但我们**还没实测过它的启动器**（`java.exe` 在 `bin/` 下？JRE 与 JDK 的差别？
    // 都得一条条验），所以拒绝，而不是生成一个指不到东西的 shim。
    let home = IsolatedHome::new("shim-add-unmeasured");
    let store = Store::new(home.store_root());
    let version = "21.0.12.1+1";
    std::fs::create_dir_all(store.version_dir("temurin", version)).expect("造 temurin 版本目录");
    let activated = home.run(["use", "temurin", version, "--json"]);
    assert!(activated.status.success(), "{}", describe(&activated));

    let output = run_shim(&home, &["shim", "add", "temurin", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let error = json(&output).error.expect("失败必须有 error");
    assert_eq!(error.code, "no-shim-commands");
    assert!(
        error.message.contains("还没有实测") || error.message.contains("没有实测"),
        "要说清是「我们还没量过」而不是「这个工具没有命令」：{}",
        error.message
    );
    assert!(
        error.message.contains("node"),
        "要指出目前只有 node 是实测过的：{}",
        error.message
    );
    assert!(!shims_dir(&home).exists());

    // **别名也要走同一条路**：`java` 是 `temurin` 的别名，归一化之后是同一个工具。
    let alias = run_shim(&home, &["shim", "add", "java", "--json"]);
    assert_eq!(alias.status.code(), Some(1));
    assert_eq!(
        json(&alias).error.expect("error").code,
        "no-shim-commands",
        "别名必须被归一化到目录里的工具 id，而不是当成另一个工具"
    );
}

#[test]
fn add_rejects_an_empty_trailing_at_and_an_unknown_tool() {
    let home = IsolatedHome::new("shim-add-args");
    let cases: [(&[&str], &str); 3] = [
        (&["shim", "add", "node@", "--json"], "empty-version"),
        (&["shim", "add", "", "--json"], "empty-tool"),
        (
            &["shim", "add", "definitely-not-a-tool", "--json"],
            "unknown-tool",
        ),
    ];
    for (args, expected) in cases {
        let output = run_shim(&home, args);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        let error = json(&output).error.expect("失败必须有 error");
        assert_eq!(error.code, expected, "{args:?}");
        assert!(!error.message.is_empty(), "{args:?} 的错误消息不该为空");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// add：生成
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn add_generates_one_exe_per_command_and_each_decodes_to_current() {
    // **这是本票最核心的断言**：四条命令各生成一个真 `.exe`，
    // 而每一个都能从文件里解出指向 `…/current/node.exe` 的前缀。
    let home = IsolatedHome::new("shim-add-node");
    activate_node(&home, NODE_VERSION);

    let output = run_shim(&home, &["shim", "add", "node", "--json"]);
    assert_eq!(output.status.code(), Some(0), "stderr：{}", stderr(&output));

    let envelope = json(&output);
    assert!(envelope.ok, "成功应当是成功信封：{:?}", envelope.error);
    let data = envelope.data.expect("成功必须有 data");
    assert_eq!(data["dryRun"], value!(false));
    assert_eq!(data["tool"], value!("node"));
    assert_eq!(data["version"], value!(NODE_VERSION));
    assert_eq!(data["activeVersion"], value!(NODE_VERSION));
    assert_eq!(data["created"], value!(4));
    assert_eq!(data["failed"], value!(0));
    assert_eq!(
        data["templateSource"],
        value!("env-var"),
        "测试必须靠 TUOEN_SHIM_TEMPLATE 指到真模板"
    );
    assert_eq!(data["notes"], value!([]));
    let commands = data["commands"].as_array().expect("commands 是数组");
    assert_eq!(commands.len(), 4, "node 暴露四条命令");
    for command in commands {
        assert_eq!(command["status"], value!("created"));
        assert!(
            command["bytes"].as_u64().unwrap_or(0) > 0,
            "生成出来的 shim 不该是空文件：{command}"
        );
        assert!(
            command["slotCount"].as_u64().unwrap_or(0) >= 1,
            "至少要改写一个槽位：{command}"
        );
    }

    // 磁盘上**恰好**这四个文件 —— 多一个也是 bug。
    assert_eq!(
        list_files(&shims_dir(&home)),
        vec!["corepack.exe", "node.exe", "npm.exe", "npx.exe"],
        "落盘的应当是这四条"
    );

    // 每一个都能解出前缀，而且前缀的第一个 token 都是 `current/node.exe`。
    let store = Store::new(home.store_root());
    let expected_target = store.current_link("node").join("node.exe");
    for name in ["node", "npm", "npx", "corepack"] {
        let path = shims_dir(&home).join(format!("{name}.exe"));
        let prefix = read_prefix(&path);
        let target = prefix_target(&prefix)
            .unwrap_or_else(|| panic!("`{name}` 的前缀里必须有目标：{prefix}"));
        assert!(
            Path::new(target)
                .to_string_lossy()
                .eq_ignore_ascii_case(&expected_target.to_string_lossy()),
            "`{name}` 应当指向 {}，实际 {target}",
            expected_target.display()
        );
    }
}

#[test]
fn the_npm_shim_goes_through_node_exe_and_a_js_entry_point() {
    // **"没有走 cmd"的判据就是这个**：目标是 `node.exe`，前缀参数是 `npm-cli.js`。
    // 如果哪天有人为了"支持 npm.cmd"改回去，这条会立刻红。
    let home = IsolatedHome::new("shim-add-npm-target");
    activate_node(&home, NODE_VERSION);
    let output = run_shim(&home, &["shim", "add", "node", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));

    let prefix = read_prefix(&shims_dir(&home).join("npm.exe"));
    assert!(
        prefix.contains("node.exe"),
        "npm 的目标必须是 node.exe：{prefix}"
    );
    assert!(
        prefix.contains("npm-cli.js"),
        "npm 必须走 cli.js（那就是它不需要 cmd 的原因）：{prefix}"
    );
    assert!(
        !prefix.to_ascii_lowercase().contains(".cmd"),
        "**绝不能**回退到 npm.cmd —— `.cmd` 会被 shim 拒绝，而且它是一条参数注入通道：{prefix}"
    );
    let target = prefix_target(&prefix).expect("前缀里必须有目标");
    assert!(
        target.ends_with("node.exe"),
        "第一个 token 必须是可执行映像本身，而不是那个 .js：{prefix}"
    );
    assert!(
        !target.contains("npm-cli.js"),
        "cli.js 是**前缀参数**，不是目标：{prefix}"
    );

    // npx / corepack 同理，各自的 cli.js 不同 —— 复制粘贴错一个文件名是最容易犯的错。
    //
    // 这里顺带钉住**分隔符是 Windows 的**：前缀是逐段 `push` 拼出来的，
    // 于是得到 `…\node_modules\npm\bin\npm-cli.js`，而不是 `…\node_modules/npm/bin/…`。
    // 混着正斜杠的路径在各处单独检查都"看起来对"，交给内核时才出问题（决策 47 的教训）。
    assert!(
        prefix.contains(r"node_modules\npm\bin\npm-cli.js"),
        "反斜杠是拼路径那一层的职责，不该漏出正斜杠：{prefix}"
    );
    let npx = read_prefix(&shims_dir(&home).join("npx.exe"));
    assert!(npx.contains("npx-cli.js"), "{npx}");
    let corepack = read_prefix(&shims_dir(&home).join("corepack.exe"));
    assert!(
        corepack.contains(r"corepack\dist\corepack.js"),
        "{corepack}"
    );
}

#[test]
fn a_generated_shim_really_launches_the_target_it_was_given() {
    // 前面那些用例证明的是"文件里的字节对"。这一条证明的是**它能跑** ——
    // 也就是 CLI 拼出来的那条 `current/…` 路径真的能被 `CreateProcessW` 启动。
    //
    // **为什么这里启动进程是安全的**：载荷里的 `node.exe` 是 System32 下的
    // `where.exe` 的一份拷贝，而它是**叶子**：shim → where.exe → 结束。
    // 递归需要"目标是另一个 shim"，本文件里从来没有那种东西 ——
    // 票据 #7 期间那个堆出 20580 个进程的测试正是因为目标绕回了自己。
    //
    // 用 `where.exe` 的**退出码**做判据（不是它的输出）：本机是 zh-CN，
    // 匹配错误文本在 AGENTS.md 里是被明令禁止的。
    let Some(system_root) = std::env::var_os("SystemRoot") else {
        panic!("没有 `SystemRoot` —— 这条用例需要 Windows 自带的 `where.exe`");
    };
    let where_exe = PathBuf::from(system_root)
        .join("System32")
        .join("where.exe");
    assert!(
        where_exe.is_file(),
        "`{}` 不存在 —— 它是 Windows 自带的，这条用例要靠它做叶子进程",
        where_exe.display()
    );

    let home = IsolatedHome::new("shim-runs");
    activate_node(&home, NODE_VERSION);
    std::fs::copy(
        &where_exe,
        Store::new(home.store_root())
            .version_dir("node", NODE_VERSION)
            .join("node.exe"),
    )
    .expect("把 where.exe 拷成载荷里的 node.exe");

    let added = run_shim(&home, &["shim", "add", "node", "--json"]);
    assert_eq!(added.status.code(), Some(0), "{}", describe(&added));
    let node_shim = shims_dir(&home).join("node.exe");

    let found = std::process::Command::new(&node_shim)
        .arg("cmd.exe")
        .output()
        .expect("启动生成出来的 shim");
    assert_eq!(
        found.status.code(),
        Some(0),
        "`where.exe cmd.exe` 应当找到东西 → 退出码 0 必须被原样透传。stderr：{}",
        String::from_utf8_lossy(&found.stderr)
    );

    let missing = std::process::Command::new(&node_shim)
        .arg("definitely-not-a-real-file-xyz")
        .output()
        .expect("启动生成出来的 shim");
    assert_eq!(
        missing.status.code(),
        Some(1),
        "找不到东西时 `where.exe` 退出 1 —— **退出码必须逐位透传**，\
         否则脚本里的 `if errorlevel` 会全部失效"
    );
}

#[test]
fn add_refuses_a_version_that_is_installed_but_not_active_yet() {
    // 决策 49：`install` **不**自动激活。所以「装了但没 `--use`」是一个常见状态，
    // 而这时 `current` 根本不存在 —— shim 指向的正是 `current`（决策 11），
    // 于是现在生成的每一条都会等到**运行时**才失败。
    //
    // 这条守卫必须在**写任何文件之前**：先落盘 4 个指不到东西的 `.exe`、再让用户去
    // `tuoen use`，等于把一次失败拆成两次，而且第一次是静默的。
    let home = IsolatedHome::new("shim-not-active");
    fake_installed_node(&home, NODE_VERSION);

    let output = run_shim(
        &home,
        &["shim", "add", &format!("node@{NODE_VERSION}"), "--json"],
    );
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    let error = json(&output).error.expect("失败必须有 error");
    assert_eq!(error.code, "payload-missing");
    assert!(
        error.message.contains("tuoen use node"),
        "要告诉用户下一步是激活：{}",
        error.message
    );
    assert!(
        !shims_dir(&home).exists(),
        "守卫在写任何东西之前 —— 连 shims 目录都不该被创建"
    );

    // 激活之后同一条命令就成功了：证明上面挡住的确实是「没激活」这一件事，
    // 而不是别的什么（否则这就成了一条永远为真的断言）。
    let activated = home.run(["use", "node", NODE_VERSION, "--json"]);
    assert!(activated.status.success(), "{}", describe(&activated));
    let output = run_shim(
        &home,
        &["shim", "add", &format!("node@{NODE_VERSION}"), "--json"],
    );
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
}

#[test]
fn a_dry_run_writes_nothing_but_still_answers_what_would_happen() {
    let home = IsolatedHome::new("shim-add-dry-run");
    activate_node(&home, NODE_VERSION);

    let output = run_shim(&home, &["shim", "add", "node", "--dry-run", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let data = json(&output).data.expect("成功必须有 data");
    assert_eq!(data["dryRun"], value!(true));
    assert_eq!(data["planned"], value!(4));
    assert_eq!(data["created"], value!(0));
    assert_eq!(data["replaced"], value!(0));
    for command in data["commands"].as_array().expect("commands 是数组") {
        assert_eq!(command["status"], value!("planned"));
        assert_eq!(command["bytes"], value!(null), "演练没有字节数");
        assert!(
            command["prefix"]
                .as_str()
                .unwrap_or("")
                .contains("node.exe"),
            "演练也要说清会烘进去什么：{command}"
        );
    }

    assert!(
        !shims_dir(&home).exists(),
        "演练**一个字节都不许写**，但 `{}` 出现了",
        shims_dir(&home).display()
    );
}

#[test]
fn a_second_add_replaces_the_existing_shims_and_says_so() {
    // `created` → `replaced` 与 `tuoen use` 那边是同一个道理：同一个命令在不同状态下
    // 有两种**都对**的输出。测试要钉的是那个转变。
    let home = IsolatedHome::new("shim-add-replace");
    activate_node(&home, NODE_VERSION);

    let first = json(&run_shim(&home, &["shim", "add", "node", "--json"]));
    assert_eq!(first.data.expect("data")["created"], value!(4));

    let second = json(&run_shim(&home, &["shim", "add", "node", "--json"]));
    let data = second.data.expect("data");
    assert_eq!(data["replaced"], value!(4), "第二次必须报覆盖");
    assert_eq!(data["created"], value!(0));
    assert_eq!(data["commands"][0]["status"], value!("replaced"));

    // 覆盖之后文件仍然可用 —— 覆盖的是槽位，不是把文件搞坏。
    let prefix = read_prefix(&shims_dir(&home).join("node.exe"));
    assert!(prefix.contains("current"), "{prefix}");
}

#[test]
fn a_version_that_is_not_active_is_reported_as_a_note_not_hidden() {
    // shim 指向 `current`：指定一个**不是**当前生效版本的版本时，
    // 生成出来的 shim 跑的不是那个版本。这件事必须出现在输出里（决策 11 的另一面）。
    let home = IsolatedHome::new("shim-add-not-active");
    fake_installed_node(&home, NODE_VERSION);
    fake_installed_node(&home, "24.21.0");
    let activated = home.run(["use", "node", NODE_VERSION, "--json"]);
    assert!(activated.status.success(), "{}", stderr(&activated));

    let output = run_shim(&home, &["shim", "add", "node@24.21.0", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let envelope = json(&output);
    assert!(envelope.ok);
    let data = envelope.data.expect("data");
    assert_eq!(data["version"], value!("24.21.0"));
    assert_eq!(data["activeVersion"], value!(NODE_VERSION));
    assert_eq!(
        data["notes"],
        value!(["version-not-active"]),
        "提醒必须是稳定 slug，而不是一句中文（决策 35）"
    );

    // 人话在人类输出里，而且要说清怎么真的切过去。
    let human = run_shim(&home, &["shim", "add", "node@24.21.0"]);
    let text = stdout(&human);
    assert!(
        text.contains("tuoen use node 24.21.0"),
        "要给出切换命令：{text}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// list
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn list_decodes_the_target_and_survives_a_foreign_file() {
    let home = IsolatedHome::new("shim-list");
    activate_node(&home, NODE_VERSION);
    let added = run_shim(&home, &["shim", "add", "node", "--json"]);
    assert_eq!(added.status.code(), Some(0), "{}", stderr(&added));

    // 一个**不是**我们生成的 `.exe`，以及一个不该被列出来的非 `.exe` 文件。
    let dir = shims_dir(&home);
    std::fs::write(dir.join("broken.exe"), b"not a tuoen shim").expect("写乱文件");
    std::fs::write(dir.join("notes.txt"), b"just a note").expect("写笔记");

    let output = run_shim(&home, &["shim", "list", "--json"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "一个坏文件是**发现**，不是错误：{}",
        stderr(&output)
    );

    let envelope = json(&output);
    assert!(envelope.ok);
    let data = envelope.data.expect("data");
    let shims = data["shims"].as_array().expect("shims 是数组");
    assert_eq!(
        shims.len(),
        5,
        "4 个我们的 + 1 个乱文件；notes.txt 不算：{data}"
    );
    assert!(
        !shims
            .iter()
            .any(|entry| entry["file"] == value!("notes.txt")),
        "只列 `.exe`：{data}"
    );

    for entry in shims {
        let file = entry["file"].as_str().expect("file 是字符串");
        assert!(
            entry["bytes"].as_u64().unwrap_or(0) > 0,
            "{file} 应当有字节数"
        );
        assert!(
            Path::new(entry["path"].as_str().expect("path"))
                .parent()
                .is_some_and(|parent| parent == dir),
            "{file} 的路径应当就在 shim 目录里：{entry}"
        );
        if file == "broken.exe" {
            assert_eq!(entry["status"], value!("not-a-shim"));
            assert_eq!(entry["problem"], value!("no-slot-magic"));
            assert_eq!(entry["target"], value!(null));
        } else {
            assert_eq!(entry["status"], value!("ok"), "{entry}");
            let target = entry["target"].as_str().expect("解出来的目标");
            assert!(
                target.ends_with("node.exe"),
                "{file} 应当指向 node.exe：{entry}"
            );
            assert_eq!(
                entry["targetExists"],
                value!(true),
                "载荷文件真的在，所以目标存在：{entry}"
            );
            assert!(
                entry["prefix"].as_str().unwrap_or("").contains("node.exe"),
                "{file} 应当有完整前缀：{entry}"
            );
        }
    }
}

#[test]
fn list_on_a_fresh_machine_is_an_empty_list_not_an_error() {
    // 目录还不存在时 `list` 不该崩，也不该报错 —— 一台新机器就是这样。
    let home = IsolatedHome::new("shim-list-fresh");
    let output = run_shim(&home, &["shim", "list", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let envelope = json(&output);
    assert!(envelope.ok);
    let data = envelope.data.expect("data");
    assert_eq!(data["shims"], value!([]));
    assert!(
        data["shimDir"].as_str().is_some_and(|dir| !dir.is_empty()),
        "空列表也要给出目录：{data}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// remove
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn remove_refuses_a_file_that_is_not_ours() {
    let home = IsolatedHome::new("shim-remove-foreign");
    let dir = shims_dir(&home);
    std::fs::create_dir_all(&dir).expect("造 shim 目录");
    std::fs::write(dir.join("broken.exe"), b"not a tuoen shim").expect("写乱文件");

    let output = run_shim(&home, &["shim", "remove", "broken", "--json"]);
    assert_eq!(output.status.code(), Some(1), "没删成 → 退出码非 0");
    let envelope = json(&output);
    assert!(!envelope.ok);
    let error = envelope.error.expect("失败必须有 error");
    assert_eq!(error.code, "not-a-shim");
    // **部分成功的信封里必须有 data**：消费者要能看见"哪一个没删成、为什么"。
    let data = envelope.data.expect("部分失败也要有 data");
    assert_eq!(data["results"][0]["status"], value!("not-a-shim"));
    assert_eq!(data["results"][0]["code"], value!("not-a-shim"));
    assert!(
        dir.join("broken.exe").is_file(),
        "**不是我们的东西就不能删** —— 这是本命令唯一的破坏性边界"
    );
}

#[test]
fn remove_of_a_missing_name_is_an_idempotent_success_and_says_so() {
    // **本票（#21）的核心用例。**
    //
    // 以前这里断言的是"退出码 1 + 错误码 not-found"：`shim remove` 把"你要删的东西
    // 不在"当成失败，而同一个工具里的 `path remove` 把同一件事当成**成功的空操作**。
    // 两套语义并存，踩坑的是脚本作者 —— 我们自己的验收脚本就踩了
    // （`docs/acceptance/L1-18-l1-real-machine.md` §7.3）。
    //
    // 现在：退出码 0，**人话明说"本来就不在"**，`--json` 里靠 `status: "absent"`
    // 与计数 `absent` 看得出来，而且**一个字节都没改**。
    let home = IsolatedHome::new("shim-remove-missing");
    let dir = shims_dir(&home);
    std::fs::create_dir_all(&dir).expect("造 shim 目录");
    std::fs::write(dir.join("notes.txt"), b"just a note").expect("写笔记");
    let before = list_files(&dir);

    let output = run_shim(&home, &["shim", "remove", "ghost", "--json"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "「本来就不在」是幂等的成功，不是失败：{}",
        describe(&output)
    );
    let envelope = json(&output);
    assert!(envelope.ok, "成功信封：{:?}", envelope.error);
    assert!(envelope.error.is_none(), "成功的空操作没有错误");
    let data = envelope.data.expect("成功必须有 data");
    assert_eq!(
        data["results"][0]["status"],
        value!("absent"),
        "**`--json` 里必须看得出它本来就不在**，而这一条不能靠中文消息：{data}"
    );
    assert_eq!(data["results"][0]["code"], value!(null), "空操作没有错误码");
    assert_eq!(
        data["results"][0]["message"],
        value!(null),
        "成功载荷里不许有本地化文本（决策 35）"
    );
    assert_eq!(
        data["results"][0]["path"],
        value!(dir.join("ghost.exe").display().to_string()),
        "仍然要说清我们找的是哪个文件：{data}"
    );
    assert_eq!(data["removed"], value!(0));
    assert_eq!(data["absent"], value!(1));
    assert_eq!(data["failed"], value!(0));
    assert_eq!(
        data["siblingsLeft"],
        value!([]),
        "什么都没删掉，就不该有「还剩哪几条」的提示：{data}"
    );

    // **一个字节都没改**：目录里连那份笔记都还在。
    assert_eq!(list_files(&dir), before, "空操作不许动任何东西");

    // 人话也必须说，而且不能长得像失败。
    let human = run_shim(&home, &["shim", "remove", "ghost"]);
    assert_eq!(human.status.code(), Some(0));
    let text = stdout(&human);
    assert!(text.contains("ghost"), "要点名是哪一个：{text}");
    assert!(text.contains("本来就不在"), "要明说它本来就不在：{text}");
    assert!(text.contains("什么都没有改"), "要明说没有改动：{text}");
    assert!(text.contains("不是错误"), "要明说这不是错误：{text}");
    assert!(
        !text.contains("没删成"),
        "「本来就不在」不是没删成 —— 一句话说反会把结论说反：{text}"
    );
    assert!(!text.contains("✗"), "不能借用失败那一行的记号：{text}");
}

#[test]
fn removing_a_name_on_a_machine_with_no_shim_dir_writes_nothing_at_all() {
    // 幂等的另一半：**连目录都不该被创建**。
    // shim 目录不存在 = 我们一条命令都没发布，那么"删掉一个名字"的终态已经成立。
    let home = IsolatedHome::new("shim-remove-missing-no-dir");
    assert!(
        !shims_dir(&home).exists(),
        "前提：这台机器上还没有 shim 目录"
    );

    let output = run_shim(&home, &["shim", "remove", "ghost", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    assert_eq!(json(&output).data.expect("data")["absent"], value!(1));
    assert!(
        !shims_dir(&home).exists(),
        "成功的空操作**不许**顺手创建 `{}`",
        shims_dir(&home).display()
    );
}

#[test]
fn remove_refuses_unsafe_names_and_deletes_nothing() {
    // 名字先过安全校验：一个带分隔符的名字根本不该变成路径。
    let home = IsolatedHome::new("shim-remove-unsafe");
    let dir = shims_dir(&home);
    std::fs::create_dir_all(&dir).expect("造 shim 目录");
    for name in ["../evil", r"..\evil", "a/b", "CON", "COM1"] {
        let output = run_shim(&home, &["shim", "remove", name, "--json"]);
        assert_eq!(output.status.code(), Some(1), "`{name}` 应当被拒");
        let error = json(&output).error.expect("失败必须有 error");
        assert_eq!(
            error.code, "bad-name",
            "`{name}` 的错误码必须来自 shim crate 的名字规则"
        );
        assert!(!error.message.is_empty(), "`{name}` 要说清为什么");
    }
    assert_eq!(
        list_files(&dir),
        Vec::<String>::new(),
        "被拒的名字一个文件都不该碰"
    );
    // 尤其是：`../evil` 对应的路径会被拼到 shim 目录**外面**去。
    assert!(
        !home
            .local_app_data()
            .join("tuoen")
            .join("evil.exe")
            .exists(),
        "带分隔符的名字绝不能拼出上级目录里的路径"
    );
}

#[test]
fn remove_deletes_our_shims_and_keeps_going_after_a_failure() {
    let home = IsolatedHome::new("shim-remove-partial");
    activate_node(&home, NODE_VERSION);
    let added = run_shim(&home, &["shim", "add", "node", "--json"]);
    assert_eq!(added.status.code(), Some(0), "{}", stderr(&added));

    // 盘上放一个**不是我们的** `.exe`：第一个名字是我们的、第二个不是 ——
    // 删掉的**不回滚**，而退出码仍然非 0（这一条才是"没删成"的真正形态：
    // 「本来就不在」不再是失败，见 `remove_of_a_missing_name_is_an_idempotent_success_and_says_so`）。
    let dir = shims_dir(&home);
    std::fs::write(dir.join("broken.exe"), b"not a tuoen shim").expect("写乱文件");

    let output = run_shim(&home, &["shim", "remove", "node", "broken", "--json"]);
    assert_eq!(output.status.code(), Some(1), "有一个没删成 → 非 0");
    let envelope = json(&output);
    assert!(!envelope.ok, "有东西没做成就不该报成功");
    let error = envelope.error.expect("失败必须有 error");
    assert_eq!(
        error.code, "not-a-shim",
        "错误码必须来自真的没删成的那一个：{}",
        error.message
    );
    let data = envelope.data.expect("部分失败也要有 data");
    assert_eq!(data["results"][0]["status"], value!("removed"));
    assert_eq!(data["removed"], value!(1));
    assert_eq!(data["failed"], value!(1));
    assert_eq!(data["absent"], value!(0));
    assert!(
        data["results"][0]["bytes"].as_u64().unwrap_or(0) > 0,
        "要说清删掉了多少字节：{data}"
    );
    assert_eq!(data["results"][1]["status"], value!("not-a-shim"));
    assert!(
        !dir.join("node.exe").exists(),
        "删掉的那条**不回滚** —— 它已经不在磁盘上了"
    );
    assert!(dir.join("npm.exe").is_file(), "没点名的那些不该被动");
    assert!(dir.join("broken.exe").is_file(), "不属于我们的文件绝不能删");

    // 全部删成时是干净的成功信封。
    let all = run_shim(
        &home,
        &["shim", "remove", "npm", "npx", "corepack", "--json"],
    );
    assert_eq!(all.status.code(), Some(0), "{}", stderr(&all));
    let envelope = json(&all);
    assert!(envelope.ok);
    assert!(envelope.error.is_none());
    assert_eq!(
        envelope.data.expect("data")["results"][0]["status"],
        value!("removed")
    );
    assert_eq!(
        list_files(&dir),
        vec!["broken.exe"],
        "只剩那个不属于我们的文件"
    );
}

#[test]
fn a_mixed_remove_reports_the_real_failure_not_the_absent_name() {
    // **顺序最容易骗人的一种局面**：`ghost`（本来就不在，现在是成功）排在
    // `broken`（真的没删成）**前面**。凡是"取第一个非 removed 的状态"的实现都会
    // 在这里把错误码报成 `not-found`/`remove-failed` —— 一句与事实不符的结论。
    let home = IsolatedHome::new("shim-remove-mixed");
    let dir = shims_dir(&home);
    std::fs::create_dir_all(&dir).expect("造 shim 目录");
    std::fs::write(dir.join("broken.exe"), b"not a tuoen shim").expect("写乱文件");

    let output = run_shim(&home, &["shim", "remove", "ghost", "broken", "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    let envelope = json(&output);
    assert!(!envelope.ok);
    let error = envelope.error.expect("失败必须有 error");
    assert_eq!(
        error.code, "not-a-shim",
        "错误码必须是**真的**没删成的那一个，而不是它前面那个「本来就不在」的"
    );
    assert!(
        !error.message.contains("ghost"),
        "「本来就不在」不是失败，不该出现在失败汇总里：{}",
        error.message
    );
    assert!(error.message.contains("broken"), "{}", error.message);

    let data = envelope.data.expect("部分失败也要有 data");
    assert_eq!(data["results"][0]["status"], value!("absent"));
    assert_eq!(data["results"][1]["status"], value!("not-a-shim"));
    assert_eq!(data["removed"], value!(0));
    assert_eq!(data["absent"], value!(1));
    assert_eq!(data["failed"], value!(1));

    // 人话那一侧也一样：不能把 `ghost` 说成没删成。
    let human = run_shim(&home, &["shim", "remove", "ghost", "broken"]);
    let text = stdout(&human);
    assert!(text.contains("本来就不在"), "{text}");
    assert!(text.contains("2 个名字里有 1 个没删成"), "{text}");
    assert!(
        text.contains("另外 1 个名字本来就不在"),
        "两种结局都要说，而且要说清谁是哪种：{text}"
    );
}

#[test]
fn remove_accepts_the_exe_suffix_too() {
    // 用户会把 `node.exe` 直接粘进来 —— 我们的文件全是 `.exe`，所以那不是歧义。
    let home = IsolatedHome::new("shim-remove-exe-suffix");
    activate_node(&home, NODE_VERSION);
    let added = run_shim(&home, &["shim", "add", "node", "--json"]);
    assert_eq!(added.status.code(), Some(0), "{}", stderr(&added));

    let output = run_shim(&home, &["shim", "remove", "node.exe", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(!shims_dir(&home).join("node.exe").exists());
}

// ─────────────────────────────────────────────────────────────────────────────
// path
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn path_is_exactly_one_line_and_it_is_the_directory() {
    // 这条命令存在的理由就是 `export PATH=$(tuoen shim path)` —— 所以人类输出
    // **必须是纯路径**：不加引号、不加前缀、不多一行。
    let home = IsolatedHome::new("shim-path");
    let expected = shims_dir(&home).display().to_string();

    let output = run_shim(&home, &["shim", "path"]);
    assert_eq!(output.status.code(), Some(0));
    let text = stdout(&output);
    assert_eq!(text.lines().count(), 1, "必须恰好一行：{text:?}");
    assert_eq!(text.trim_end(), expected, "必须是那个目录本身");
    assert!(!text.contains('"'), "不加引号：{text:?}");
    assert!(
        !Path::new(text.trim_end()).is_relative(),
        "必须是绝对路径：{text:?}"
    );
    assert!(
        !shims_dir(&home).exists(),
        "`shim path` **什么都不改** —— 目录不存在时也不创建它"
    );
}

#[test]
fn path_json_says_the_same_thing_as_the_human_output() {
    let home = IsolatedHome::new("shim-path-json");
    let expected = shims_dir(&home).display().to_string();

    let output = run_shim(&home, &["shim", "path", "--json"]);
    assert_eq!(output.status.code(), Some(0));
    let envelope = json(&output);
    assert!(envelope.ok);
    assert_eq!(envelope.command, "shim.path");
    assert_eq!(envelope.data.expect("data")["path"], value!(expected));
}

// ─────────────────────────────────────────────────────────────────────────────
// `--json` 的两条契约：逐字节稳定，成功载荷不本地化
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_shim_family_is_byte_stable_in_the_same_state() {
    // **必须在同一个状态下比两次**（票据 #6 的教训）：`shim add` 第一次是
    // `created`、第二次是 `replaced`，跨状态比是比错了东西。
    let home = IsolatedHome::new("shim-json-stable");
    activate_node(&home, NODE_VERSION);
    let added = run_shim(&home, &["shim", "add", "node", "--json"]);
    assert_eq!(added.status.code(), Some(0), "{}", stderr(&added));

    for args in [
        vec!["shim", "path", "--json"],
        vec!["shim", "list", "--json"],
        vec!["shim", "add", "node", "--dry-run", "--json"],
        // 一个**什么都不改**的删除（名字本来就不在）：它必须逐字节稳定 ——
        // 而"什么都不改"的命令正是最容易顺手带上一点机器状态的那一类。
        vec!["shim", "remove", "ghost", "--json"],
    ] {
        assert_eq!(
            stdout(&run_shim(&home, &args)),
            stdout(&run_shim(&home, &args)),
            "{args:?} 的 --json 输出必须逐字节稳定"
        );
    }
}

#[test]
fn success_payloads_contain_no_localised_text() {
    // 中文优先针对的是**人类输出**。JSON 里出现中文就意味着界面语言把脚本绑死了（决策 35）。
    let home = IsolatedHome::new("shim-json-no-cjk");
    activate_node(&home, NODE_VERSION);

    for args in [
        vec!["shim", "list", "--json"],
        vec!["shim", "path", "--json"],
        vec!["shim", "add", "node", "--dry-run", "--json"],
        vec!["shim", "add", "node", "--json"],
        vec!["shim", "remove", "node", "--json"],
        // 「本来就不在」现在是**成功**，所以它的载荷也在这一条契约的管辖范围内：
        // 一句中文说明（"它本来就不在"）必须留在人类输出里，不能进 JSON。
        vec!["shim", "remove", "ghost", "--json"],
    ] {
        let output = run_shim(&home, &args);
        let text = stdout(&output);
        let envelope = json(&output);
        assert!(envelope.ok, "{args:?} 应当是成功信封：{text}");
        assert!(
            !contains_cjk(&text),
            "{args:?} 的成功载荷里不该有 CJK：{text}"
        );
    }
}

#[test]
fn error_codes_are_ascii_while_error_messages_may_be_chinese() {
    let home = IsolatedHome::new("shim-json-codes");
    let cases: Vec<(Vec<&str>, &str)> = vec![
        (vec!["shim", "add", "node", "--json"], "no-active-version"),
        (vec!["shim", "add", "nope", "--json"], "unknown-tool"),
        (vec!["shim", "add", "node@", "--json"], "empty-version"),
        (vec!["shim", "remove", "CON", "--json"], "bad-name"),
        // 「本来就不在」（`ghost`）**故意不在这里**：它现在是一个成功信封，
        // 由 `remove_of_a_missing_name_is_an_idempotent_success_and_says_so` 钉住。
        // 一条永远为真的断言比没有断言更坏 —— 而这里留一行说明是为了让下一个
        // 想往里加 `not-found` 的人先看到它为什么不在。
    ];
    for (args, expected) in cases {
        let envelope = json(&run_shim(&home, &args));
        assert!(!envelope.ok, "{args:?} 应当是失败信封");
        let error = envelope.error.expect("失败必须有 error");
        assert_eq!(error.code, expected, "{args:?}");
        assert!(
            !error.code.is_empty()
                && error
                    .code
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '-' || c.is_ascii_digit()),
            "错误码必须是小写 kebab ASCII：{}",
            error.code
        );
        assert!(
            !error.message.is_empty(),
            "{args:?} 的中文消息不该为空 —— 那是给人看的那一半"
        );
    }
}

#[test]
fn a_fresh_machine_gets_chinese_rejections_and_never_panics() {
    // "换一台新机器"时第一个问题就是"这里什么都没有，工具会崩吗"。
    let home = IsolatedHome::new("shim-fresh-human");
    for args in [
        vec!["shim", "list"],
        vec!["shim", "add", "node"],
        vec!["shim", "remove", "node"],
    ] {
        let output = run_shim(&home, &args);
        let text = format!("{}{}", stdout(&output), stderr(&output));
        assert!(!text.is_empty(), "{args:?} 不该没有任何输出");
        assert!(!text.contains("panicked"), "{args:?} 不该 panic：{text}");
        assert!(contains_cjk(&text), "{args:?} 的人类输出应当是中文：{text}");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 真实机器：这一票**绝不**碰它
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_real_tuoen_home_was_not_touched_by_this_files_tests() {
    // **本文件最重要的一条断言。**
    //
    // 这一票比其他票更危险：`shim add` 写出去的是**可执行文件**，而它们一旦
    // 落在真实的 `%LOCALAPPDATA%\tuoen\shims`，就是往用户的 `PATH` 目录里扔东西。
    //
    // 为什么不是"断言真实的 `%LOCALAPPDATA%\tuoen` 不存在"：那是一句**会被误报**
    // 的断言 —— 本机这个目录**已经存在**，因为 L0 的真机验收真的在那里装过 node
    // （`store/node/versions/24.19.0`）。所以这里断言的是更准确的东西：
    // **跑完一整套 shim 生命周期，那个目录的顶层内容一个字节都没变，而且
    // `shims` 这个（本票唯一会创建的）目录从来没出现过。**
    let before = real_home_listing();

    let home = IsolatedHome::new("shim-real-home");
    activate_node(&home, NODE_VERSION);
    for args in [
        vec!["shim", "add", "node", "--json"],
        vec!["shim", "list", "--json"],
        vec!["shim", "remove", "node", "--json"],
        // **#21 的真机复现形状**：一个不存在的名字现在是**成功**，而且**什么都不该碰** ——
        // 包括真实的 `%LOCALAPPDATA%\tuoen`（那个目录里连 `shims` 都不该出现）。
        vec!["shim", "remove", "tuoen-definitely-not-a-shim", "--json"],
    ] {
        let output = run_shim(&home, &args);
        assert!(output.status.success(), "{args:?}：{}", stderr(&output));
    }

    let after = real_home_listing();
    assert_eq!(
        before, after,
        "测试碰到了真实的 `%LOCALAPPDATA%\\tuoen` —— 这是本仓库最容易造成真实伤害的地方"
    );
    // **不断言"真实的 shim 目录不存在"。** 那是机器状态，不是被测代码的性质：
    // 任何真的用 `tuoen shim add` 装过东西的人都有这个目录（L0-07 的真机验收就有），
    // 于是那条断言会在最不该红的时候红 —— 实测就红过一次。
    // 上面那个 before/after 才是真要钉的东西，而 `real_home_listing` 已经往
    // `shims` 里看了一层，所以"往用户 PATH 目录里放可执行文件"照样会红。
}
