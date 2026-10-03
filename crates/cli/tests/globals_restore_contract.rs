//! `tuoen restore --only globals` 的**进程边界契约**（票据 #24）。
//!
//! # 这一层能证明什么
//!
//! 与 `restore_contract.rs` 的分工：那个文件证明"计划与注册表"，这个文件证明
//! **"照着计划真的把包装进了我们自己的根"** —— 也就是这一票唯一会下载、会写盘的那一节。
//!
//! # 三件事让它能在 `cargo test` 里跑
//!
//! 1. **`PATH` 收窄成"一个临时 `bin` + `SystemRoot` + `System32`"**：本机的 `npm`
//!    / `node` / `pip` 一个都问不到，所以"不联网、不装真包"是**结构保证**的。
//! 2. **`LOCALAPPDATA` 指向临时树**：我们自己的根（`…\tuoen\globals`）落在临时树里，
//!    真实的 `%LOCALAPPDATA%\tuoen\globals` 一个字节都不会被碰。
//! 3. **假 `npm.cmd` 真的往 `--prefix` 指的 staging 里写产物**：这一票要断言的恰恰是
//!    "产物真的出现了"与"翻转真的发生了"（决策 197 的教训："命令成功"与"东西装上了"
//!    是两件事）。它的 `ls`（我们那一问）**读真实的根**，所以幂等不是靠记账装出来的。
//!
//! # `node.exe` 为什么是系统 `cmd.exe` 的副本
//!
//! npm 的根名写在**版本目录**里（`…\npm\<node -v 的原样>`），而 `node -v` 必须是
//! 一个"答得上一句话、退出 0"的真 PE（`.cmd` 不能被 spawn —— 决策 174）。
//! 本仓库的 `globals_contract.rs` 已经用这个手法（把 `cmd.exe` 复制成 `node.exe`），
//! 于是根名是那行 banner 的**原样** —— 断言因此只能是**结构性**的
//! （"`<base>\npm\` 下面恰好一个目录"），不能逐字比 banner：那等于把开发机的
//! 系统语言写进契约里（`AGENTS.md` 规矩五）。
//!
//! # 一条不能失败的断言不是断言
//!
//! "跑前跑后逐字节相同"最容易退化成"两份空串相同"，所以：
//! * [`the_registry_helper_really_reads_both_scopes`] 先证明注册表助手真的读到了东西；
//! * 每一条"没有写"的断言都先证明**那个地方本来是有东西的**（或者明确断言它不存在）。

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::{IsolatedHome, TempDir, json, list_tree, stderr, stdout};
use serde_json::Value;
use tuoen_platform::{EnvBlock, EnvScope, RealEnvBlock, RealFileSystem, RealRegistry};

/// 机器自己那份 npm 的包清单（一个包，`pnpm`）—— 假 npm 的 `ls` 直接 `type` 它。
const MACHINE_ONE: &str = r#"{"name":"lib","dependencies":{"pnpm":{"version":"11.21.0"}}}"#;
/// 两个包（`corepack` 在前 —— `install_globals` 按 `(tool, name)` 排序，顺序是契约）。
const MACHINE_TWO: &str = r#"{"name":"lib","dependencies":{"corepack":{"version":"0.35.0"},"pnpm":{"version":"11.21.0"}}}"#;

// ─────────────────────────────────────────────────────────────────────────────
// 固定装置
// ─────────────────────────────────────────────────────────────────────────────

/// 一台"只有假 npm"的机器 + 一个隔离的家目录。
struct Fixture {
    home: IsolatedHome,
    /// 临时 `bin`：假 `npm.cmd`、冒充 `node.exe` 的 `cmd.exe` 副本、以及几个开关文件。
    bin: TempDir,
    /// 机器自己那份 npm 的前缀（**在临时树里**，所以"不在它那儿"这句话可以逐字比）。
    machine_prefix: PathBuf,
}

impl Fixture {
    fn new(label: &str, machine_packages: &str) -> Self {
        let home = IsolatedHome::new(label);
        let bin = TempDir::new(&format!("globals-restore-bin-{label}"));
        let machine_prefix = home.local_app_data().join("machine-npm-prefix");
        // 机器前缀里真的放一个包：`capture` 会去读它的 `package.json`（binNames 的来源）。
        let package = machine_prefix.join("node_modules").join("pnpm");
        std::fs::create_dir_all(&package).expect("建机器前缀里的包目录");
        std::fs::write(
            package.join("package.json"),
            r#"{"name":"pnpm","version":"11.21.0"}"#,
        )
        .expect("写机器前缀的 package.json");

        bin.write("machine-prefix.txt", &machine_prefix.display().to_string());
        bin.write("machine-packages.json", machine_packages);
        bin.write("npm.cmd", &npm_script());
        copy_system_pe(&bin.path().join("node.exe"));

        Self {
            home,
            bin,
            machine_prefix,
        }
    }

    /// 我们自己的根（`<LOCALAPPDATA>\tuoen\globals`）—— 测试里就是临时树里那个。
    fn globals_base(&self) -> PathBuf {
        self.home.local_app_data().join("tuoen").join("globals")
    }

    /// 那个**唯一**的版本目录（`<base>\npm\<node -v 的原样>`）。
    ///
    /// 找不到或不止一个就 panic —— "根名是哪一段"是产品的事，测试只认
    /// "**恰好一个**"（决策 203：一个根装 N 个包）。
    fn npm_root(&self) -> PathBuf {
        let npm = self.globals_base().join("npm");
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(&npm)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|entry| entry.path().is_dir())
                    .map(|entry| entry.path())
                    .collect()
            })
            .unwrap_or_default();
        dirs.sort();
        assert_eq!(
            dirs.len(),
            1,
            "`{}` 下面应当恰好一个版本目录",
            npm.display()
        );
        dirs.remove(0)
    }

    fn command(&self) -> Command {
        let system_root = std::env::var_os("SystemRoot").expect("SystemRoot 必须存在");
        let mut path = self.bin.path().as_os_str().to_os_string();
        path.push(";");
        path.push(&system_root);
        path.push("\\System32");

        let mut command = self.home.command();
        command
            .env("USERPROFILE", self.home.local_app_data().join("home"))
            .env("HOME", self.home.local_app_data().join("home"))
            // **清掉**开发机可能有的重定向变量：它们会把"这台假机器"的答案换成
            // 开发机自己的，而且我们的重定向会被继承进来的值悄悄盖掉。
            .env_remove("NPM_CONFIG_PREFIX")
            .env_remove("PYTHONUSERBASE")
            .env_remove("PIP_USER")
            .env("PATH", path);
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command()
            .args(args)
            .output()
            .expect("运行 tuoen 应当成功")
    }

    /// 现场 `capture --only globals` 出来的快照目录（`globals.toml` + `schema.toml`）。
    ///
    /// **不手写 TOML**：手写的形状迟早与 `capture` 漂移，而这一票要测的恰恰是
    /// "capture 写出来的东西 restore 认不认"。
    fn snapshot(&self, name: &str) -> PathBuf {
        let dir = self.home.tuoen_home().join("scratch").join(name);
        let out = dir.display().to_string();
        let captured = self.run(&["capture", "--only", "globals", "--out", &out]);
        assert_eq!(captured.status.code(), Some(0), "{}", describe(&captured));
        let files: Vec<String> = std::fs::read_dir(&dir)
            .expect("快照目录")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            files.iter().any(|file| file == "globals.toml"),
            "快照里必须有 globals.toml：{files:?}"
        );
        dir
    }

    /// 打开"缓存里没有"这个开关（假 npm 的 `pack` 会 `ENOTCACHED` 退出 1）。
    fn not_cached(&self) {
        self.bin.write("not-cached.txt", "1");
    }

    /// 让某个包的安装命令失败（**先写半个产物**再非零退出）。
    fn fail_install(&self, name: &str) {
        self.bin.write(&format!("fail-install-{name}.txt"), "1");
    }
}

/// 一个假 `npm.cmd`：`config` / `ls` / `pack` / `install` 四件事，按参数分岔。
///
/// * `--prefix` 在参数里 ⇒ 这是**我们那一问**（决策 172/190）：`ls` 读**真实的根**
///   （`%NPM_CONFIG_PREFIX%\node_modules\*`），于是"装上了吗"这件事由磁盘说了算；
/// * 没有 `--prefix` ⇒ 机器那一问：`config` 答前缀、`ls` 答固定装置里的清单、
///   `pack` 是**缓存探针**（`not-cached.txt` 在就是 `ENOTCACHED` 退出 1）；
/// * `install` 真的往 `%NPM_CONFIG_PREFIX%`（= staging）里写 `node_modules\<name>\package.json`
///   —— 这正是产品用来判"东西装上了吗"的那一份产物（决策 197）。
///
/// # 为什么全用 `goto` 而不是嵌套的 `if (...)` 块
///
/// **实测**：`exit /b 1` 写在**嵌套的括号块**里时，`cmd.exe` 会把退出码吃掉
/// （块里 `exit /b 1` → 进程退出码 0；同一句放在标签下面就是 1）。而这一票的
/// 失败判据全在退出码上 —— 一个"看起来完全合理"的 0 会让每一条失败路径静默变绿。
/// 所以每一处 `exit /b` 都在**标签的顶层**，嵌套只留在 `for` 里。
fn npm_script() -> String {
    "@echo off\r\n\
     setlocal enabledelayedexpansion\r\n\
     echo %* | findstr /c:\"--prefix\" >nul\r\n\
     if not errorlevel 1 goto with_prefix\r\n\
     \r\n\
     if \"%~1\"==\"config\" goto answer_prefix\r\n\
     if \"%~1\"==\"ls\" goto answer_machine\r\n\
     if \"%~1\"==\"pack\" goto probe\r\n\
     goto unexpected\r\n\
     \r\n\
     :answer_prefix\r\n\
     type \"%~dp0machine-prefix.txt\"\r\n\
     exit /b 0\r\n\
     \r\n\
     :answer_machine\r\n\
     type \"%~dp0machine-packages.json\"\r\n\
     exit /b 0\r\n\
     \r\n\
     :probe\r\n\
     if exist \"%~dp0not-cached.txt\" goto not_cached\r\n\
     echo npm notice Tarball Contents\r\n\
     exit /b 0\r\n\
     \r\n\
     :not_cached\r\n\
     echo npm error code ENOTCACHED 1>&2\r\n\
     echo npm error cache mode is 'only-if-cached' but no cached response is available. 1>&2\r\n\
     exit /b 1\r\n\
     \r\n\
     :with_prefix\r\n\
     if \"%~1\"==\"ls\" goto answer_ours\r\n\
     if \"%~1\"==\"install\" goto install\r\n\
     goto unexpected\r\n\
     \r\n\
     :answer_ours\r\n\
     set \"OUT=\"\r\n\
     for /d %%D in (\"%NPM_CONFIG_PREFIX%\\node_modules\\*\") do (\r\n\
     \x20 set \"VER=\"\r\n\
     \x20 set /p VER=<\"%%D\\.fake-version\"\r\n\
     \x20 set \"FRAG=\"%%~nxD\":{\"version\":\"!VER!\"}\"\r\n\
     \x20 if defined OUT set \"OUT=!OUT!,\"\r\n\
     \x20 set \"OUT=!OUT!!FRAG!\"\r\n\
     )\r\n\
     echo {\"name\":\"lib\",\"dependencies\":{!OUT!}}\r\n\
     exit /b 0\r\n\
     \r\n\
     :install\r\n\
     for /f \"tokens=1,2 delims=@\" %%A in (\"%~5\") do (\r\n\
     \x20 set \"NAME=%%A\"\r\n\
     \x20 set \"VER=%%B\"\r\n\
     )\r\n\
     set \"PKG=%NPM_CONFIG_PREFIX%\\node_modules\\!NAME!\"\r\n\
     mkdir \"!PKG!\" 2>nul\r\n\
     > \"!PKG!\\package.json\" echo {\"name\":\"!NAME!\",\"version\":\"!VER!\"}\r\n\
     > \"!PKG!\\.fake-version\" echo !VER!\r\n\
     > \"%NPM_CONFIG_PREFIX%\\!NAME!.cmd\" echo @echo off\r\n\
     if exist \"%~dp0fail-install-!NAME!.txt\" goto install_failed\r\n\
     echo added 1 package\r\n\
     exit /b 0\r\n\
     \r\n\
     :install_failed\r\n\
     echo npm error code EACCES 1>&2\r\n\
     exit /b 1\r\n\
     \r\n\
     :unexpected\r\n\
     echo npm.cmd: unexpected arguments 1>&2\r\n\
     exit /b 1\r\n"
        .to_owned()
}

/// 把系统 `cmd.exe` 复制成 `destination` —— 一个"答得上一句话、退出 0"的真 PE。
fn copy_system_pe(destination: &Path) {
    let cmd = Path::new(&std::env::var_os("SystemRoot").expect("SystemRoot"))
        .join("System32")
        .join("cmd.exe");
    std::fs::copy(&cmd, destination).unwrap_or_else(|err| {
        panic!(
            "复制 {} → {} 失败：{err}",
            cmd.display(),
            destination.display()
        )
    });
}

fn describe(output: &Output) -> String {
    format!(
        "退出码 {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status.code(),
        stdout(output),
        stderr(output)
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// 注册表护栏
// ─────────────────────────────────────────────────────────────────────────────

/// 两个作用域的 `Path`（类型 + 原文）。判据是**平台事实**，不是开发机事实。
fn registry_snapshot() -> (String, String) {
    let block = RealEnvBlock::new(RealRegistry, RealFileSystem);
    let read = |scope| match block.get(scope, "Path") {
        Some(var) => format!("{:?}|{}", var.reg_type, var.value_raw),
        None => "<没有这个值>".to_owned(),
    };
    (read(EnvScope::User), read(EnvScope::Machine))
}

fn assert_registry_unchanged(before: (String, String), what: &str) {
    let after = registry_snapshot();
    assert_eq!(after.0, before.0, "用户级 Path 被改了（{what}）");
    assert_eq!(after.1, before.1, "机器级 Path 被改了（{what}）");
}

#[test]
fn the_registry_helper_really_reads_both_scopes() {
    // 这一条是全部"没变"断言的护栏：读不到东西的助手会让每一条 `before == after`
    // 都退化成"两份空串相同"（`AGENTS.md` 规矩五）。
    let (user, machine) = registry_snapshot();
    assert!(!machine.starts_with("<没有这个值>"), "机器级 Path 必然存在");
    assert!(user.contains('|'), "用户级的判据必须带类型：{user}");
}

// ─────────────────────────────────────────────────────────────────────────────
// `--json` 取用
// ─────────────────────────────────────────────────────────────────────────────

/// `restore <snap> --only globals --json` 的 `data`。
fn plan_data(fixture: &Fixture, snap: &Path, extra: &[&str]) -> (Value, Output) {
    let path = snap.display().to_string();
    let mut args = vec!["restore", path.as_str(), "--only", "globals"];
    args.extend_from_slice(extra);
    args.push("--json");
    let output = fixture.run(&args);
    let envelope = json(&output);
    (envelope.data.unwrap_or(Value::Null), output)
}

fn section<'a>(data: &'a Value, id: &str) -> &'a Value {
    data["sections"]
        .as_array()
        .expect("sections 是数组")
        .iter()
        .find(|section| section["id"] == id)
        .unwrap_or_else(|| panic!("计划里必须有 {id} 这一节：{data}"))
}

fn manual_codes(data: &Value) -> Vec<String> {
    data["manualActions"]
        .as_array()
        .expect("manualActions 是数组")
        .iter()
        .map(|action| action["code"].as_str().expect("code").to_owned())
        .collect()
}

/// `apply.sections[id=<id>].packages[]`。
fn packages<'a>(data: &'a Value, id: &str) -> &'a Vec<Value> {
    let section = section(&data["apply"], id);
    section["packages"]
        .as_array()
        .unwrap_or_else(|| panic!("这一节必须有 packages：{section}"))
}

fn results(data: &Value, id: &str) -> Vec<String> {
    packages(data, id)
        .iter()
        .map(|package| package["result"].as_str().expect("result").to_owned())
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// 一、计划：装进我们自己的根，**不在**机器那个前缀
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_plan_says_the_packages_go_into_our_own_root() {
    let fixture = Fixture::new("plan", MACHINE_ONE);
    let snap = fixture.snapshot("snap-1");

    let (data, output) = plan_data(&fixture, &snap, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));

    let globals = section(&data, "globals");
    assert_eq!(globals["status"], "would-change", "{globals}");
    assert_eq!(
        globals["needsNetwork"], false,
        "缓存里能解决 ⇒ 计划说 false（票据 #24 §3）：{globals}"
    );
    assert_eq!(globals["counts"]["rows"]["install"], 1);
    assert_eq!(globals["counts"]["effective"]["install"], 1);
    let action = &globals["actions"][0];
    assert_eq!(action["id"], "globals:npm:pnpm");
    assert_eq!(action["kind"], "install-global");
    assert_eq!(action["subject"], "npm:pnpm@11.21.0");
    let detail = action["detail"].as_str().expect("detail");
    assert!(detail.contains("cached=yes"), "{detail}");
    assert!(detail.contains("sources=machine"), "{detail}");
    assert!(
        detail.contains(r"npm\v") || detail.contains("npm\\"),
        "目标路径必须在我们的根下面：{detail}"
    );

    // **票据 #24 §4 的必需部分**：这句话必须点名机器那个前缀。
    assert!(
        manual_codes(&data).contains(&"globals-prefix-moved".to_owned()),
        "{data}"
    );

    // 五节都在（`globals` 从"不认识的 section"变成了认识的那一个，决策 188 的收口）。
    assert_eq!(data["sections"].as_array().expect("数组").len(), 5);

    // 人类输出：**逐字**说出"不在 <机器前缀>"。
    let human = fixture.run(&["restore", &snap.display().to_string(), "--only", "globals"]);
    assert_eq!(human.status.code(), Some(0), "{}", describe(&human));
    let text = stdout(&human);
    assert!(text.contains("不在"), "{text}");
    assert!(
        text.contains(&fixture.machine_prefix.display().to_string()),
        "人类输出必须点名机器那个前缀 `{}`：{text}",
        fixture.machine_prefix.display()
    );
    assert!(
        text.contains("tuoen globals list"),
        "要说清去哪儿看它们：{text}"
    );
    // 决策 203：装包会执行来自包本身的代码，这句话必须说出来。
    assert!(text.contains("安装脚本"), "{text}");
}

// ─────────────────────────────────────────────────────────────────────────────
// 二、真装 + 幂等
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn apply_installs_into_our_root_and_a_second_run_is_no_change() {
    let fixture = Fixture::new("apply", MACHINE_ONE);
    let snap = fixture.snapshot("snap-1");
    let before = registry_snapshot();

    let (data, output) = plan_data(&fixture, &snap, &["--apply", "--offline"]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    assert_eq!(data["apply"]["wrote"], true, "{data}");
    let outcome = section(&data["apply"], "globals");
    assert_eq!(outcome["outcome"], "applied", "{outcome}");
    assert_eq!(outcome["wrote"], true);
    assert_eq!(results(&data, "globals"), ["installed"]);

    // **产物真的出现了**（决策 197 的教训）：装进的是我们的根、版本是快照里那个。
    let root = fixture.npm_root();
    let manifest = root.join("node_modules").join("pnpm").join("package.json");
    let text = std::fs::read_to_string(&manifest).expect("装好的 package.json");
    assert!(text.contains(r#""version":"11.21.0""#), "{text}");
    // staging 一个都不剩（逐包 staging + 翻转的收尾）。
    assert!(
        list_tree(&fixture.globals_base())
            .iter()
            .all(|line| !line.contains(".staging-")),
        "{:?}",
        list_tree(&fixture.globals_base())
    );

    // **幂等**：第二次是 `no-change`（判据是枚举出来的 `(包, 版本)` 集合，决策 203）。
    let (again, output) = plan_data(&fixture, &snap, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    let globals = section(&again, "globals");
    assert_eq!(globals["status"], "no-change", "{globals}");
    assert_eq!(globals["counts"]["rows"]["already-present"], 1);
    assert_eq!(globals["counts"]["rows"]["install"], 0);
    assert!(
        globals["actions"].as_array().expect("数组").is_empty(),
        "已经在了就不是一条要做的动作：{globals}"
    );

    assert_registry_unchanged(before, "装全局包");
}

#[test]
fn deleting_one_package_installs_only_that_one() {
    let fixture = Fixture::new("partial", MACHINE_TWO);
    let snap = fixture.snapshot("snap-1");

    let (data, output) = plan_data(&fixture, &snap, &["--apply", "--offline"]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    assert_eq!(results(&data, "globals"), ["installed", "installed"]);

    // 从我们的根里删掉**一个**包（另一个照旧）。
    let root = fixture.npm_root();
    let victim = root.join("node_modules").join("corepack");
    std::fs::remove_dir_all(&victim).expect("删掉一个包");

    let (data, output) = plan_data(&fixture, &snap, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    let globals = section(&data, "globals");
    assert_eq!(globals["status"], "would-change", "{globals}");
    assert_eq!(globals["counts"]["rows"]["already-present"], 1);
    assert_eq!(globals["counts"]["rows"]["install"], 1);
    assert_eq!(globals["actions"].as_array().expect("数组").len(), 1);

    let (data, output) = plan_data(&fixture, &snap, &["--apply", "--offline"]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    assert_eq!(
        results(&data, "globals"),
        ["already-present", "installed"],
        "逐包结果按 (tool, name) 排：已经在的在前、要装的在后（顺序是契约）"
    );
    assert!(root.join("node_modules").join("corepack").is_dir());
}

// ─────────────────────────────────────────────────────────────────────────────
// 三、`--offline` 是承诺，不是偏好
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn offline_refuses_a_package_that_is_not_cached_and_writes_nothing() {
    let fixture = Fixture::new("offline", MACHINE_ONE);
    let snap = fixture.snapshot("snap-1");
    fixture.not_cached();
    let before = registry_snapshot();

    // 计划如实说"要网络"（量出来的，不是猜的）。
    let (data, output) = plan_data(&fixture, &snap, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    let globals = section(&data, "globals");
    assert_eq!(globals["status"], "needs-network", "{globals}");
    assert_eq!(globals["needsNetwork"], true);

    // `--apply --offline` 必须**拒绝整节**并点名那个包，而且一个字节都不写。
    let (data, output) = plan_data(&fixture, &snap, &["--apply", "--offline"]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "拒绝要能被脚本看见：{}",
        describe(&output)
    );
    assert_eq!(data["apply"]["wrote"], false, "{data}");
    let outcome = section(&data["apply"], "globals");
    assert_eq!(outcome["outcome"], "failed", "{outcome}");
    assert_eq!(outcome["error"]["code"], "needs-network");
    assert!(
        outcome["error"]["detail"]
            .as_str()
            .expect("detail")
            .contains("pnpm@11.21.0"),
        "要点名是哪个包：{outcome}"
    );
    assert!(
        !fixture.globals_base().join("npm").exists(),
        "承诺是'绝不碰网络'：连一个目录都不该建出来"
    );

    assert_registry_unchanged(before, "--offline 拒绝");
}

// ─────────────────────────────────────────────────────────────────────────────
// 四、一个失败不拖垮整节
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_failed_package_does_not_stop_the_others() {
    let fixture = Fixture::new("half", MACHINE_TWO);
    let snap = fixture.snapshot("snap-1");

    // 先全装上，再让 `corepack` 失败一次 —— 于是"正式目录逐字节未变"有内容可比。
    let (data, output) = plan_data(&fixture, &snap, &["--apply", "--offline"]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    assert_eq!(results(&data, "globals"), ["installed", "installed"]);
    let root = fixture.npm_root();
    let victim = root.join("node_modules").join("corepack");
    std::fs::remove_dir_all(&victim).expect("删掉一个包");
    let before = list_tree(&fixture.globals_base());

    fixture.fail_install("corepack");
    let path = snap.display().to_string();
    let output = fixture.run(&[
        "restore",
        path.as_str(),
        "--only",
        "globals",
        "--apply",
        "--offline",
        "--json",
    ]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "有失败就要非零退出：{}",
        describe(&output)
    );
    // **信封不许与退出码自相矛盾**：有失败 ⇒ `ok: false` + 一个稳定错误码，而 `data`
    // 照旧交出去（已经做了什么必须看得见）。这两条在修复前是红的 —— 那时退出码是 1
    // 而信封写着 `ok: true`，正是 `crates/cli/src/envelope.rs` 的 `partial` 文档
    // 点名禁止的形态（"脚本会以为一切正常"）。
    let envelope = json(&output);
    assert!(!envelope.ok, "有失败就不该报成功：{}", describe(&output));
    assert_eq!(
        envelope.error.as_ref().map(|error| error.code.as_str()),
        Some("apply-failed"),
        "错误码是稳定字符串：{}",
        describe(&output)
    );
    assert!(
        envelope.data.is_some(),
        "失败信封里仍然要带着已经发生的事实：{}",
        describe(&output)
    );
    let data = envelope.data.unwrap_or(Value::Null);
    let outcome = section(&data["apply"], "globals");
    assert_eq!(outcome["outcome"], "failed", "{outcome}");
    assert_eq!(
        results(&data, "globals"),
        ["already-present", "install-failed"],
        "一个失败不拖垮整节（这一趟只有那一个要装）：{data}"
    );
    assert_eq!(
        data["apply"]["wrote"], false,
        "失败的包一个字节都没进正式目录 ⇒ 这一趟什么都没写：{data}"
    );
    let failed = packages(&data, "globals")
        .iter()
        .find(|package| package["name"] == "corepack")
        .expect("失败的那一行必须在报告里");
    assert_eq!(failed["result"], "install-failed");
    assert!(
        failed["detail"]
            .as_str()
            .expect("detail")
            .contains("code=EACCES"),
        "判据来自机器 token，不是本地化的散文：{failed}"
    );

    // 正式目录：成功的那个在，失败的那个不在，而且**没有 `.staging-` 残余**。
    assert!(root.join("node_modules").join("pnpm").is_dir());
    assert!(!victim.exists(), "失败的那个连一个字节都不许进正式目录");
    let after = list_tree(&fixture.globals_base());
    assert!(
        after.iter().all(|line| !line.contains(".staging-")),
        "{after:?}"
    );
    let staging_before: Vec<&String> = before
        .iter()
        .filter(|line| !line.contains("corepack"))
        .collect();
    let staging_after: Vec<&String> = after
        .iter()
        .filter(|line| !line.contains("corepack"))
        .collect();
    assert_eq!(
        staging_after, staging_before,
        "失败那一趟不许动别的任何东西"
    );
}
