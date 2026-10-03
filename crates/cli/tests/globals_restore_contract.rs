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
        copy_fake_node_pe(&bin.path().join("node.exe"));

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

    /// 让装出来的包**带上 `bin`**（票据 #25 的固定装置）：`{"<name>":"bin/<name>.mjs",
    /// "<name>x":"bin/<name>x.mjs"}` 加两个真的载荷文件。
    ///
    /// 两个名字是刻意的：一个与包名相同、一个不同（`pn`/`pnx` 是别名那个形状），
    /// 于是"名字来自 `bin` 的键而不是包名"这件事在断言里是可见的。
    fn with_bin(&self) {
        self.bin.write("with-bin.txt", "1");
    }

    /// 我们自己的 shim 目录（`<LOCALAPPDATA>\tuoen\shims`）。
    fn shim_dir(&self) -> PathBuf {
        self.home.local_app_data().join("tuoen").join("shims")
    }

    /// 产品会拿 `node -v` 的**原样**输出当 npm 那一行的 `tool_version`，
    /// 而 store 里那个目录名是**削过的**版本（决策 201）。
    ///
    /// 这里让测试自己问一次同一个 `node.exe` —— 那是"这台假机器上的 node 版本"
    /// （一个机器事实），不是产品的输出。
    fn node_version(&self) -> String {
        let system_root = std::env::var_os("SystemRoot").expect("SystemRoot 必须存在");
        let path = format!(
            "{};{}",
            self.bin.path().display(),
            Path::new(&system_root).join("System32").display()
        );
        let output = Command::new(self.bin.path().join("node.exe"))
            .arg("-v")
            .env("PATH", path)
            .output()
            .expect("跑 `node -v`");
        let text = String::from_utf8_lossy(&output.stdout);
        let line = text.lines().next().unwrap_or_default().trim().to_owned();
        assert!(!line.is_empty(), "假 node 必须答得上一句话");
        // 削前缀是**没得削**：这一串不以 `v` 开头，所以"原样"与"削过"是同一个字符串。
        // 这正是这个手法能用的前提（真机上那份是 `v24.19.0` / `24.19.0`）。
        assert!(!line.starts_with('v'), "固定装置的版本不该带 `v`：{line:?}");
        line
    }

    /// 在 store 里造出**那个精确版本**的 `node.exe`（决策 200：包 shim 只许指向它）。
    ///
    /// 返回 `node\versions\<版本>` 这个目录。
    fn install_store_node(&self) -> PathBuf {
        let version = self.node_version();
        let dir = self
            .home
            .local_app_data()
            .join("tuoen")
            .join("store")
            .join("node")
            .join("versions")
            .join(&version);
        std::fs::create_dir_all(&dir).expect("建 store 版本目录");
        copy_fake_node_pe(&dir.join("node.exe"));
        dir
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
     if not exist \"%~dp0with-bin.txt\" goto maybe_fail\r\n\
     mkdir \"!PKG!\\bin\" 2>nul\r\n\
     > \"!PKG!\\package.json\" echo {\"name\":\"!NAME!\",\"version\":\"!VER!\",\"bin\":{\"!NAME!\":\"bin/!NAME!.mjs\",\"!NAME!x\":\"bin/!NAME!x.mjs\"}}\r\n\
     > \"!PKG!\\bin\\!NAME!.mjs\" echo export {};\r\n\
     > \"!PKG!\\bin\\!NAME!x.mjs\" echo export {};\r\n\
     :maybe_fail\r\n\
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

/// 一个**答得像 node** 的假 `node.exe`：系统 `cmd.exe` 的副本 + 它的 MUI 资源。
///
/// # 为什么必须连 MUI 一起复制（票据 #25 实测）
///
/// 只复制 `cmd.exe` 本体时，改名后的副本**找不到自己的本地化字符串表**，
/// `node -v` 的第一行会变成
/// `The system cannot find message text for message number 0x2350 in the message file for Application.`
/// —— 一个**以点结尾**的字符串。于是它连"版本号"这一关都过不了：
/// `tuoen_store::layout::check_component` 会以"名字以点或空格结尾"拒掉它
/// （Win32 会吃掉结尾的点，那个目录名在多数工具里打不开也删不掉），
/// 于是 npm 的 globals 根名会变成一个**结尾带点**的目录名 ——
/// 而那正是本模块文档里说的"banner 的原样"。
///
/// 把 `<语言>\cmd.exe.mui` 复制成 `<语言>\<副本名>.mui` 之后，副本找回了自己的
/// 字符串表，`node -v` 报的就是货真价实的
/// `Microsoft Windows [Version 10.0.…]` —— 与真 node 的 `v24.19.0` 同一种形状。
/// **不许硬编码语言目录名**：逐个列 `System32` 下的一层目录，见到
/// `cmd.exe.mui` 就复制 —— 开发机的系统语言不进契约（`AGENTS.md` 规矩五）。
fn copy_fake_node_pe(destination: &Path) {
    copy_system_pe(destination);
    let system32 = Path::new(&std::env::var_os("SystemRoot").expect("SystemRoot")).join("System32");
    let Some(name) = destination.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    let mui = format!("{name}.mui");
    for entry in std::fs::read_dir(&system32).into_iter().flatten().flatten() {
        let source = entry.path().join("cmd.exe.mui");
        if !source.is_file() || !entry.path().is_dir() {
            continue;
        }
        let target_dir = destination
            .parent()
            .unwrap_or(&system32)
            .join(entry.file_name());
        if std::fs::create_dir_all(&target_dir).is_ok() {
            let _ = std::fs::copy(&source, target_dir.join(&mui));
        }
    }
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

// ─────────────────────────────────────────────────────────────────────────────
// 五、shim（#25）：装好的包要**敲得出来**
// ─────────────────────────────────────────────────────────────────────────────

/// 模板与 CLI 在同一个目录里（`target/<profile>`）—— 这是本仓库找模板的规矩，
/// 而下面几条"真的发出了 `.exe`"的用例**依赖它**。所以它必须先被证明，
/// 不能让"模板不在"表现成"产品没发 shim"（`AGENTS.md`：一条不能失败的测量不是测量）。
fn assert_template_is_next_to_the_cli() {
    let exe = std::env::current_exe().expect("测试可执行文件的位置");
    let profile = exe
        .parent()
        .and_then(Path::parent)
        .expect("target/<profile>");
    for name in ["tuoen.exe", "tuoen-shim.exe"] {
        let path = profile.join(name);
        assert!(
            path.is_file(),
            "`{}` 必须存在（`cargo test` 会构建全部 bin）：摸一下 `cargo build --bins`",
            path.display()
        );
    }
}

/// 装好的包 → `PATH` 上的 `.exe` shim：名字来自包自己的 `bin`，目标是 store 里
/// **那个精确版本**的 `node.exe` + 包里的载荷。
#[test]
fn apply_publishes_exe_shims_for_the_installed_bins() {
    assert_template_is_next_to_the_cli();
    let fixture = Fixture::new("shims", MACHINE_ONE);
    let snap = fixture.snapshot("snap-1");
    fixture.with_bin();
    let store_version_dir = fixture.install_store_node();
    let before = registry_snapshot();

    let (data, output) = plan_data(&fixture, &snap, &["--apply", "--offline"]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));

    // ① 逐包结果：装上了，而且带着**它发出来的命令名**（名字来自包自己的 `bin`）。
    let pnpm = packages(&data, "globals")
        .iter()
        .find(|package| package["name"] == "pnpm")
        .expect("pnpm 那一条")
        .clone();
    assert_eq!(pnpm["result"], "installed", "{pnpm}");
    assert_eq!(
        pnpm["shims"],
        serde_json::json!(["pnpm", "pnpmx"]),
        "两个名字都来自 `bin`（`pnpmx` 与包名不同，正是别名那个形状）：{pnpm}"
    );

    // ② 盘上真的有两个 `.exe`，而且**没有** `.cmd` / `.ps1`（铁律）。
    let shim_dir = fixture.shim_dir();
    for name in ["pnpm.exe", "pnpmx.exe"] {
        let path = shim_dir.join(name);
        assert!(path.is_file(), "`{}` 必须存在", path.display());
    }
    let forbidden: Vec<String> = std::fs::read_dir(&shim_dir)
        .expect("shim 目录")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".cmd") || name.ends_with(".ps1"))
        .collect();
    assert!(forbidden.is_empty(), "绝不发 .cmd / .ps1：{forbidden:?}");

    // ③ 烘进去的那条命令行**就是** store 的精确版本 + 包里的载荷。
    //    `shim list` 解的是二进制里的槽位，所以这是"写进去的是什么"的直接证据
    //    （真机验收里那一步是"跑起来与真身逐字比"，测试里只能查到这个层面）。
    let listed = fixture.run(&["shim", "list", "--json"]);
    assert_eq!(listed.status.code(), Some(0), "{}", describe(&listed));
    let listed = json(&listed).data.expect("shim list 的 data");
    let entry = listed["shims"]
        .as_array()
        .expect("shims 是数组")
        .iter()
        .find(|entry| entry["command"] == "pnpm")
        .expect("`pnpm.exe` 那一条")
        .clone();
    assert_eq!(entry["status"], "ok", "{entry}");
    assert_eq!(
        entry["target"].as_str(),
        Some(
            store_version_dir
                .join("node.exe")
                .to_string_lossy()
                .as_ref()
        ),
        "目标必须是 store 里那个**精确版本目录**的 node.exe（决策 200）：{entry}"
    );
    assert_eq!(entry["targetExists"], true, "{entry}");
    let prefix = entry["prefix"].as_str().expect("prefix");
    assert!(prefix.contains("pnpm.mjs"), "前缀参数是那个脚本：{prefix}");
    assert!(
        prefix.contains("bin"),
        "前缀参数是包目录里的相对路径拼出来的：{prefix}"
    );

    // ④ 节里的三个加法键：分母（我们发了哪些）与遮蔽（有没有被抢）**分开**。
    let outcome = section(&data["apply"], "globals");
    assert_eq!(
        outcome["shimDir"].as_str(),
        Some(shim_dir.to_string_lossy().as_ref()),
        "{outcome}"
    );
    assert_eq!(
        outcome["shimCommands"],
        serde_json::json!(["pnpm", "pnpmx"]),
        "分母是 shim 目录里真的有的那些 `.exe`：{outcome}"
    );
    // 这本机 `PATH` 只有临时 `bin` + `System32` ⇒ 不可能命中 `pnpm.cmd`。
    // 分母非空，所以"没被抢"这句话是有内容的（不是"没得比"）。
    assert!(outcome["shadowedShims"].is_null(), "{outcome}");

    assert_registry_unchanged(before, "发 shim");
}

/// store 里没有那个精确版本的 `node.exe` ⇒ **报告**，一条 shim 都不发，
/// 而且**绝不**退回机器自己的 node（决策 200：那会是一个不报错的假话）。
#[test]
fn a_missing_store_node_is_reported_and_no_shim_is_written() {
    assert_template_is_next_to_the_cli();
    let fixture = Fixture::new("no-store-node", MACHINE_ONE);
    let snap = fixture.snapshot("snap-1");
    fixture.with_bin();
    // 刻意**不**建 store 里那个版本目录。

    let (data, output) = plan_data(&fixture, &snap, &["--apply", "--offline"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "包装上了就是成功 —— 发不出 shim 是一件事，不是安装失败：{}",
        describe(&output)
    );
    let pnpm = packages(&data, "globals")[0].clone();
    assert_eq!(pnpm["result"], "installed", "{pnpm}");
    assert!(pnpm["shims"].is_null(), "一条都不许发：{pnpm}");
    let issues = pnpm["shimIssues"].as_array().expect("shimIssues");
    assert_eq!(issues.len(), 2, "{pnpm}");
    assert!(
        issues.iter().all(|issue| issue["reason"] == "node-missing"),
        "原因要说出来（而不是静默退回机器侧的 node）：{pnpm}"
    );
    assert!(
        !fixture.shim_dir().join("pnpm.exe").exists(),
        "store 里没有那一版 node ⇒ 一条 shim 都不该出现"
    );
}

/// 两个名字都被**别人的文件**占着 ⇒ 逐包结果是 `skipped-shadowed`（票据 #25 §4：
/// 这个词表成员**只**由这一票产出），而且别人的文件一个字节都不许被改。
#[test]
fn a_package_whose_names_are_taken_reports_skipped_shadowed() {
    assert_template_is_next_to_the_cli();
    let fixture = Fixture::new("shadowed", MACHINE_ONE);
    let snap = fixture.snapshot("snap-1");
    fixture.with_bin();
    fixture.install_store_node();

    // 第一趟：真的发出来了（先证明"没被抢"时的样子）。
    let (data, output) = plan_data(&fixture, &snap, &["--apply", "--offline"]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    assert_eq!(results(&data, "globals"), ["installed"]);
    assert_eq!(
        packages(&data, "globals")[0]["shims"],
        serde_json::json!(["pnpm", "pnpmx"]),
        "{data}"
    );

    let shim_dir = fixture.shim_dir();
    let occupied = "not-a-shim";
    for name in ["pnpm.exe", "pnpmx.exe"] {
        assert!(
            shim_dir.join(name).is_file(),
            "{}",
            shim_dir.join(name).display()
        );
        std::fs::write(shim_dir.join(name), occupied).expect("写一个占位文件");
    }
    // 让这一趟**真的要装**（不删的话计划是 `no-change`，走不到发 shim 那一步）。
    std::fs::remove_dir_all(fixture.npm_root().join("node_modules").join("pnpm")).expect("删掉包");

    // 人类输出先看一眼（它要能说出"被谁占了"）。
    let human = fixture.run(&[
        "restore",
        &snap.display().to_string(),
        "--only",
        "globals",
        "--apply",
        "--offline",
    ]);
    assert_eq!(human.status.code(), Some(0), "{}", describe(&human));
    let text = stdout(&human);
    assert!(text.contains("没发出来"), "{text}");
    assert!(text.contains("已经被别人占了"), "{text}");
    assert!(text.contains("by=file:"), "要点名被谁占了：{text}");

    // 机器可读那一份：同一个局面（把包再删一次，让它重新成为"要装的那一个"）。
    std::fs::remove_dir_all(fixture.npm_root().join("node_modules").join("pnpm")).expect("删掉包");
    let (data, output) = plan_data(&fixture, &snap, &["--apply", "--offline"]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    assert_eq!(
        results(&data, "globals"),
        ["skipped-shadowed"],
        "一个名字都没发出来、且都是被占 ⇒ 那是 skipped-shadowed：{data}"
    );
    let pnpm = packages(&data, "globals")[0].clone();
    let issues = pnpm["shimIssues"].as_array().expect("shimIssues");
    assert_eq!(issues.len(), 2, "{pnpm}");
    assert!(
        issues.iter().all(|issue| issue["reason"] == "shadowed"
            && issue["detail"]
                .as_str()
                .is_some_and(|detail| detail.starts_with("by=file:"))),
        "{pnpm}"
    );
    // **不覆盖**：那两个文件逐字未变。
    for name in ["pnpm.exe", "pnpmx.exe"] {
        assert_eq!(
            std::fs::read_to_string(shim_dir.join(name)).expect("读占位文件"),
            occupied,
            "别人的文件一个字节都不许动"
        );
    }
}
