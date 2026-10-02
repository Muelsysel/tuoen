//! 测试基础设施：**CLI 进程边界是本仓库的主 seam**。
//!
//! 为什么是进程边界（见 `docs/specs/L0-install-engine.md` 的测试决定）：
//! 跑真实二进制并断言 stdout / stderr / 退出码 / 磁盘副作用，能覆盖用户真正会看到的东西；
//! 而单测内部函数会漏掉"参数解析错了""输出被本地化了""退出码不对"这类整机级 bug。
//!
//! **本文件是给 `tests/*.rs` 用的**：Cargo 把 `tests/common/` 当作共享模块而不是独立测试目标，
//! 所以这里不需要（也不能有）`#[test]`。
//!
//! **硬性约束**：任何测试都不得读取或写入真实的 `HKCU\Environment`、真实 `PATH`、
//! 用户真实安装目录或真实 `%APPDATA%\tuoen`。这是本仓库最容易造成真实伤害的地方。

#![allow(dead_code)] // 不同测试目标用到不同的辅助函数。

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use assert_cmd::cargo::CommandCargoExt;
use serde::Deserialize;

/// 构造一个指向本仓库 `tuoen` 二进制的命令。
///
/// 每次调用都新建 `Command`，因为 `Command` 不是 `Clone`。
#[must_use]
pub fn tuoen() -> Command {
    Command::cargo_bin("tuoen").expect("tuoen 二进制应当已被 cargo 构建")
}

/// 运行 `tuoen` 并捕获输出。
///
/// **不**断言退出码 —— 调用方自己决定期望哪个码，因为"非零退出"本身也是一等输出。
pub fn run<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    tuoen()
        .args(args)
        .output()
        .expect("运行 tuoen 应当成功（失败说明二进制没被构建出来）")
}

/// 一个**隔离的**家目录：`tuoen` 的存储与缓存会被指到这里面的子目录。
///
/// ## 为什么从票据 #6 起每个会写磁盘的测试都必须用它
///
/// `tuoen install` 的默认存储根是 `%LOCALAPPDATA%\tuoen\store`（决策 48）。
/// 不隔离的话，一次 `cargo test` 会往**开发者真实的** `%LOCALAPPDATA%` 里装东西 ——
/// 这正是本仓库最容易造成真实伤害的地方（`tests/common/mod.rs` 开头的硬性约束）。
///
/// ## 为什么能做到
///
/// 因为 `Store::at_default_location()` 读的是**环境变量**而不是 Win32 的
/// `SHGetKnownFolderPath`。这个选择不是随手做的：读环境变量让"存储位置"
/// 成为一个可注入的输入，于是最高层的那条 seam（进程边界）在测试里也可用。
#[derive(Debug)]
pub struct IsolatedHome {
    dir: TempDir,
}

impl IsolatedHome {
    #[must_use]
    pub fn new(label: &str) -> Self {
        Self {
            dir: TempDir::new(&format!("home-{label}")),
        }
    }

    /// `tuoen` 会看到的 `%LOCALAPPDATA%`（存储在这里面）。
    #[must_use]
    pub fn local_app_data(&self) -> PathBuf {
        self.dir.path().join("LocalAppData")
    }

    /// `tuoen` 会看到的 `%APPDATA%`（下载缓存在这里面）。
    #[must_use]
    pub fn roaming_app_data(&self) -> PathBuf {
        self.dir.path().join("AppData")
    }

    /// 存储根：`<LOCALAPPDATA>\tuoen\store`。
    ///
    /// **在这里自己拼出来是刻意的**：测试要知道"东西应该落在哪"才能断言
    /// 磁盘副作用，而向生产代码问路径等于让测试跟着实现走。
    #[must_use]
    pub fn store_root(&self) -> PathBuf {
        self.local_app_data().join("tuoen").join("store")
    }

    /// 跑一次 `tuoen`，环境指向这个隔离的家目录。
    pub fn run<I, S>(&self, args: I) -> Output
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        tuoen()
            .args(args)
            .env("LOCALAPPDATA", self.local_app_data())
            .env("APPDATA", self.roaming_app_data())
            // 不改 HOME/USERPROFILE：它们被 `catalog`/`detect` 用到，
            // 而那两个命令读机器事实是**对的**。只隔离我们自己会写的那两个根。
            .output()
            .expect("运行 tuoen 应当成功")
    }
}

/// 以 UTF-8 解码 stdout。
///
/// 中文优先意味着输出里有 CJK —— 解码失败本身就是 bug，所以要 `expect` 而不是 `from_utf8_lossy`。
#[must_use]
pub fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout 必须是合法 UTF-8")
}

/// 以 UTF-8 解码 stderr。
#[must_use]
pub fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr 必须是合法 UTF-8")
}

/// `--json` 输出的信封，**与 `crates/cli/src/envelope.rs` 独立定义**。
///
/// 故意不共用类型：测试要能发现"生产代码改了形状而测试跟着改"这种共谋。
/// 这里的定义就是**契约本身**。
#[derive(Debug, Deserialize)]
pub struct JsonEnvelope {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub command: String,
    pub ok: bool,
    #[serde(default)]
    pub data: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<JsonError>,
}

#[derive(Debug, Deserialize)]
pub struct JsonError {
    pub code: String,
    pub message: String,
}

/// 解析 `--json` 输出。**先断言它真的是 JSON** —— 中文提示混进 stdout 是最常见的破坏方式。
#[must_use]
pub fn json(output: &Output) -> JsonEnvelope {
    let text = stdout(output);
    serde_json::from_str(text.trim())
        .unwrap_or_else(|err| panic!("stdout 不是合法 JSON：{err}\n实际输出：{text}"))
}

/// 一个测试用的临时目录，`Drop` 时递归删除。
///
/// 自建而不引入 `tempfile`：本仓库的测试数量会很大，而这里只需要一个能清理的目录。
/// 名字里带进程 id 与计数器，避免并行测试互相踩。
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// 在系统临时目录下新建一个唯一目录。
    #[must_use]
    pub fn new(label: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!("tuoen-test-{label}-{}-{n}", std::process::id()));
        // 上一次运行崩溃留下的目录会让 create_dir_all 成功但内容脏，所以先删。
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("创建临时目录");
        Self { path }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 在临时目录下写一个文件（自动建父目录）。
    pub fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let full = self.path.join(relative);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("创建父目录");
        }
        std::fs::write(&full, contents).expect("写文件");
        full
    }

    /// 读回一个文件。
    #[must_use]
    pub fn read(&self, relative: &str) -> String {
        std::fs::read_to_string(self.path.join(relative)).expect("读文件")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

impl std::fmt::Debug for TempDir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TempDir").field("path", &self.path).finish()
    }
}

/// 递归列出目录下所有文件的相对路径（正斜杠分隔），排序后返回。
///
/// 用于断言"产出恰好是这些文件"—— 多一个文件也是 bug。
#[must_use]
pub fn list_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    collect(root, root, &mut out);
    out.sort();
    out
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, out);
        } else if let Ok(rel) = path.strip_prefix(root) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
}
