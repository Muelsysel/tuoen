//! 固定装置：把"一台机器的状态"写成**纯数据**，让检测引擎在不碰真机的前提下可测。
//!
//! **为什么必须有它**（ticket #11 的硬约束）：测试不得读取真实的 `HKCU\Environment` /
//! `HKLM` 注册表 / 真实 `PATH` / 用户真实安装目录，也不得依赖开发机的实时状态。
//! 本机 `PATH` 恰好是坏的（48 条里有重复、有死条目、WindowsApps 别名排在第 8 条而真
//! Python 排在第 36 条）—— 用实时状态会让"测试通过"变得毫无意义。
//!
//! 三件事同时落在这里：
//! 1. [`MachineFixture`] —— `serde` 可反序列化的机器状态描述（`fixtures/detect/*.toml` 就是它）。
//! 2. 三个假后端 —— [`FakeFileSystem`] / [`FakeRegistry`] / [`FakeProcessRunner`]。
//! 3. [`FakeMachine`] —— 把假后端按 `&dyn Trait` 交给业务逻辑的那一层。
//!
//! **假后端只有必要的方法**：`FakeFileSystem` / `FakeProcessRunner` / `FakeManagedStore`
//! 全是只读的，只有 `FakeRegistry` 在票据 #8 之后多了 `set_value` / `delete_value`
//! —— 因为 `PATH` 的 plan/apply 必须在**不碰真注册表**的前提下可测。

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::env_block::InMemoryEnv;
use crate::error::PlatformError;
use crate::fs_facts::{DirEntryFacts, FileFacts, FileSystem, ReadOutcome, ReparseKind};
use crate::managed::{ManagedStore, ManagedTool};
use crate::process::{ProcessOutcome, ProcessRunner};
use crate::registry::{RegHive, RegValue, Registry};

/// 一台机器的状态。字段全部有默认值，所以一个固定装置只需要写它关心的那部分 ——
/// 这也让"这个用例到底在测什么"一眼可见。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineFixture {
    /// 当前进程看到的环境变量（`ProcessEnv` 的来源）。**大小写不敏感**。
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// 独立路径的文件系统事实（不在任何已声明目录里的那些）。
    #[serde(default)]
    pub paths: Vec<FixturePath>,
    /// 目录及其内容。列目录与"路径是否存在"共用同一份声明，所以两者不可能互相矛盾。
    #[serde(default)]
    pub dirs: Vec<FixtureDir>,
    /// 注册表键。**子键由声明自动推导**，不需要单独列一遍。
    #[serde(default)]
    pub registry: Vec<FixtureKey>,
    /// 版本探测的结果。
    #[serde(default)]
    pub processes: Vec<FixtureProcess>,
    /// tuoen 自己的安装记录（`managed` 层的来源）。
    #[serde(default)]
    pub managed: Vec<ManagedTool>,
}

/// 一条路径的事实。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixturePath {
    /// 完整路径（`dirs[].entries[].path` 里则是**相对该目录的名字**，单层，不嵌套）。
    pub path: String,
    /// 存在。默认 `true` —— 写固定装置时少写一半字段。
    #[serde(default = "yes")]
    pub exists: bool,
    /// 是目录。
    #[serde(default)]
    pub is_dir: bool,
    /// 字节数。**App Execution Alias 是 0**。
    #[serde(default)]
    pub size: u64,
    /// reparse 形状。写 `"app-exec-alias"` 就是本机 `WindowsApps\python.exe` 那根东西。
    #[serde(default)]
    pub reparse: ReparseKind,
    /// 链接目标（junction / symlink）。
    #[serde(default)]
    pub link_target: Option<String>,
    /// 文件内容。**只给需要读内容的用例**（决策 177/178：算 `content_hash`、扫凭据形状）。
    ///
    /// 不写就是"有大小、没内容" —— 读它会得到 [`ReadOutcome::Unreadable`]，而这一条
    /// 必须是**故意**的：一份固定装置说"这个文件存在、`size = 494`"时，
    /// 我们**不知道**那 494 字节是什么，而"猜一个内容"会让用例在测一件不存在的事。
    /// 有专门一条用例钉住这个 `Unreadable`（见 `fixture.rs` 的 `read` 用例）。
    ///
    /// 它与 `size` 是**两件事**：`size` 是"声明的事实"（可以是假的、可以是 0 ——
    /// 比如 App Execution Alias），`content` 是"读得到的东西"。要两者一致时用
    /// [`FixturePath::file_with_content`]。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

/// 手写而不是 `derive`：`serde` 那侧的 `exists` 默认是 `true`，Rust 那侧必须是同一个值，
/// 否则"用 TOML 写的固定装置"与"用 Rust 写的固定装置"会给出不同答案 —— 那种不一致
/// 会让人以为是引擎的 bug。
impl Default for FixturePath {
    fn default() -> Self {
        Self {
            path: String::new(),
            exists: true,
            is_dir: false,
            size: 0,
            reparse: ReparseKind::None,
            link_target: None,
            content: None,
        }
    }
}

impl FixturePath {
    /// 一个存在的普通文件。
    #[must_use]
    pub fn file(path: &str, size: u64) -> Self {
        Self {
            path: path.to_owned(),
            size,
            ..Self::default()
        }
    }

    /// 一个**有内容**的普通文件，`size` 由内容长度算出来。
    ///
    /// 手写一个与 `content` 不一致的 `size` 会让 `inspect` 与 `read` 说两套话
    /// （"这个文件 494 字节"而读出来 12 字节）—— 那种固定装置测的是一个真机上
    /// 不存在的形状。所以"要有内容"这条路只留这一个入口。
    #[must_use]
    pub fn file_with_content(path: &str, content: &str) -> Self {
        Self {
            path: path.to_owned(),
            size: content.len() as u64,
            content: Some(content.to_owned()),
            ..Self::default()
        }
    }

    /// 一个存在的目录。
    #[must_use]
    pub fn dir(path: &str) -> Self {
        Self {
            path: path.to_owned(),
            is_dir: true,
            ..Self::default()
        }
    }

    /// 一个 App Execution Alias：**长度 0、tag `0x8000001b`、`exists == true`**。
    ///
    /// 本机 `…\Microsoft\WindowsApps\python.exe` 就长这样，而 `Get-Command python` 成功。
    #[must_use]
    pub fn app_exec_alias(path: &str) -> Self {
        Self {
            path: path.to_owned(),
            size: 0,
            reparse: ReparseKind::AppExecAlias,
            ..Self::default()
        }
    }

    /// 一个目录联接。
    #[must_use]
    pub fn junction(path: &str, target: &str) -> Self {
        Self {
            path: path.to_owned(),
            is_dir: true,
            reparse: ReparseKind::Junction,
            link_target: Some(target.to_owned()),
            ..Self::default()
        }
    }

    /// 一个目录符号链接。
    #[must_use]
    pub fn symlink_dir(path: &str, target: &str) -> Self {
        Self {
            path: path.to_owned(),
            is_dir: true,
            reparse: ReparseKind::SymlinkDir,
            link_target: Some(target.to_owned()),
            ..Self::default()
        }
    }

    /// 一条**不存在**的路径（幽灵条目、失效的 `PATH` 条目）。
    #[must_use]
    pub fn missing(path: &str) -> Self {
        Self {
            path: path.to_owned(),
            exists: false,
            ..Self::default()
        }
    }
}

/// 一个目录及其内容。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureDir {
    /// 目录路径。
    pub path: String,
    /// 目录本身存在吗。默认 `true`。
    #[serde(default = "yes")]
    pub exists: bool,
    /// 内容。每条的 `path` 是**相对本目录的名字**。
    #[serde(default)]
    pub entries: Vec<FixturePath>,
}

impl Default for FixtureDir {
    fn default() -> Self {
        Self {
            path: String::new(),
            exists: true,
            entries: Vec::new(),
        }
    }
}

impl FixtureDir {
    /// 一个存在的目录及其内容。
    #[must_use]
    pub fn new(path: &str, entries: Vec<FixturePath>) -> Self {
        Self {
            path: path.to_owned(),
            entries,
            ..Self::default()
        }
    }
}

/// 一个注册表键。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureKey {
    /// 哪个 hive。
    pub hive: RegHive,
    /// 相对该 hive `SOFTWARE` 的子键路径。
    pub path: String,
    /// 键不存在（用来测"读不到的键"这条分支）。默认 `true`。
    #[serde(default = "yes")]
    pub exists: bool,
    /// 值名 → 值。**默认值的名字是空字符串**（`values = { "" = { sz = "C:\\x" } }`）。
    #[serde(default)]
    pub values: BTreeMap<String, RegValue>,
}

impl FixtureKey {
    /// 一个存在的键及其值。
    #[must_use]
    pub fn new(hive: RegHive, path: &str, values: BTreeMap<String, RegValue>) -> Self {
        Self {
            hive,
            path: path.to_owned(),
            exists: true,
            values,
        }
    }

    /// 一个**不存在**的键（用来测"注册表里没有这个来源"）。
    #[must_use]
    pub fn absent(hive: RegHive, path: &str) -> Self {
        Self {
            hive,
            path: path.to_owned(),
            exists: false,
            values: BTreeMap::new(),
        }
    }
}

/// 一次版本探测的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureProcess {
    /// 可执行文件的完整路径。
    pub program: String,
    /// 参数前缀。**空 = 通配**（任何参数都能命中），用来兼容既有的"只关心 program"的固定装置。
    ///
    /// 非空时，**调用的参数必须逐元素、按顺序以它开头**才算命中。匹配与取舍规则
    /// （最长赢、同长按声明顺序）写在 [`FakeProcessRunner`] 上 —— 规则本身就是被测对象。
    ///
    /// 为什么需要它：决策 168 把枚举命令定成 `cmd.exe /C <tool>.cmd …`，于是 #17 的采集器
    /// 会有**六次调用共用同一个 `cmd.exe`**（`npm config get prefix` / `npm ls -g --json …` /
    /// `pip --version` / `pip list --format=json …` / `git config --system --list` /
    /// `git config --global --list`）。只按 program 匹配时这六次只能拿到同一份 stdout，
    /// 于是"npm 非零退出"与"git 两层身份不同"这类固定装置**在原理上写不出来**。
    #[serde(default)]
    pub args: Vec<String>,
    /// 标准输出。
    #[serde(default)]
    pub stdout: String,
    /// 标准错误。**版本号经常在这里**（`git --version` / `java -version`）。
    #[serde(default)]
    pub stderr: String,
    /// 退出码。
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// 模拟"这个工具不返回"。
    ///
    /// **假后端立刻返回 `timed_out`**，它证明的是"检测逻辑把超时记为发现但版本未知"；
    /// "真的超时"由 [`crate::process::SystemProcessRunner`] 的独立用例证明
    /// （真的起一个不返回的进程）。
    #[serde(default)]
    pub timed_out: bool,
}

fn yes() -> bool {
    true
}

/// 路径归一化：`/` → `\`、去掉尾部分隔符（保住裸盘符与 UNC 前缀）、小写。
///
/// Windows 的路径比较是大小写不敏感的，固定装置里的大小写不应该影响结果 ——
/// 否则一个 `C:\Tools` 与 `c:\tools` 的笔误会让用例静默变成"什么都没找到"。
#[must_use]
pub fn normalize_path(raw: &str) -> String {
    let text = raw.replace('/', "\\");
    let (prefix, rest) = match text.strip_prefix(r"\\") {
        Some(rest) => (r"\\", rest.to_owned()),
        None => ("", text),
    };
    let mut rest = rest;
    while rest.contains(r"\\") {
        rest = rest.replace(r"\\", "\\");
    }
    let trimmed = rest.trim_end_matches('\\');
    let joined = if trimmed.len() == 2 && trimmed.ends_with(':') {
        format!("{trimmed}\\")
    } else {
        trimmed.to_owned()
    };
    format!("{prefix}{joined}").to_lowercase()
}

/// 注册表键路径归一化：分隔符统一、去掉首尾分隔符、小写。
#[must_use]
fn normalize_key(raw: &str) -> String {
    raw.replace('/', "\\").trim_matches('\\').to_lowercase()
}

/// 读固定装置的假文件系统。
#[derive(Debug, Clone, Default)]
pub struct FakeFileSystem {
    index: HashMap<String, FileFacts>,
    dirs: HashMap<String, Vec<DirEntryFacts>>,
    /// 声明了内容的那些文件（归一化路径 → 内容）。
    ///
    /// 与 `index` 分开存：`FileFacts` 是"这个路径的事实"（大小、reparse、链接目标），
    /// 而内容是**另一件事** —— 绝大多数固定装置只需要前者，写一个 `size` 就够了。
    contents: HashMap<String, String>,
    /// **读过哪些路径**（归一化后的形态，见 [`FakeFileSystem::reads`]）。
    ///
    /// `Rc<RefCell<…>>` 与 `FakeRegistry` 同一个理由、同一个做法：克隆一份假文件系统
    /// 必须克隆出**同一本账**，否则"构造它的人"与"读它的人"各拿一本，断言永远为空。
    ///
    /// # 为什么需要这本账（票据 #25）
    ///
    /// 有一类断言是"**不许读某个文件**"：npm 会在 `node_modules\.bin` 里生成
    /// `.cmd` / `.ps1` 转发器，而它们是**产物不是契约**（本仓库绝不发那种东西，
    /// 决策 10）。固定装置里放一个内容**故意不同**的 `.cmd` 之后，
    /// "产出的命令名与它无关"是一种**间接**证据；"根本没人打开过它"才是直接证据。
    /// 没有这本账，那条断言就只能靠间接推断（而"间接口径"正是决策 186 要炸掉的形态）。
    reads: Rc<RefCell<Vec<String>>>,
}

impl FakeFileSystem {
    fn from_fixture(fixture: &MachineFixture) -> Self {
        let mut index: HashMap<String, FileFacts> = HashMap::new();
        let mut dirs: HashMap<String, Vec<DirEntryFacts>> = HashMap::new();
        let mut contents: HashMap<String, String> = HashMap::new();

        for dir in &fixture.dirs {
            let mut entries = Vec::new();
            for entry in &dir.entries {
                let full = format!("{}\\{}", dir.path.trim_end_matches('\\'), entry.path);
                entry.check_size_matches_content(&full);
                let facts = entry.to_facts(Path::new(&full));
                index.insert(normalize_path(&full), facts);
                if let Some(content) = &entry.content {
                    contents.insert(normalize_path(&full), content.clone());
                }
                entries.push(DirEntryFacts {
                    name: entry.path.clone(),
                    is_dir: entry.is_dir,
                    reparse: entry.reparse,
                });
            }
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            if dir.exists {
                index.insert(
                    normalize_path(&dir.path),
                    FileFacts {
                        path: Path::new(&dir.path).to_path_buf(),
                        exists: true,
                        is_dir: true,
                        size: 0,
                        reparse: ReparseKind::None,
                        link_target: None,
                        error: None,
                    },
                );
            }
            dirs.insert(normalize_path(&dir.path), entries);
        }

        // **`paths` 覆盖 `dirs`，不是反过来。**
        //
        // 顺序在这里有语义：一条显式的 `paths` 条目说的是"**这个路径本身**是什么"
        // （一个符号链接、一个 0 字节的别名、一个不存在的东西），
        // 而 `dirs` 里的条目说的是"这个目录里**装着**什么"。
        // 两者不冲突，但之前 `paths` 先写、`dirs` 后写，于是
        // `FixturePath::symlink_dir("C:\\nvm4w\\nodejs", …)` 会被同名的
        // `FixtureDir` 覆盖成一个普通目录 —— 符号链接**静默变成目录**，
        // 而依赖 `link_target` 的检测（nvm4w 的当前版本）就再也读不到版本。
        //
        // 现在显式路径赢：`dirs` 仍然提供目录里的文件，`paths` 决定目录节点本身。
        for entry in &fixture.paths {
            entry.check_size_matches_content(&entry.path);
            let facts = entry.to_facts(Path::new(&entry.path));
            let key = normalize_path(&entry.path);
            index.insert(key.clone(), facts);
            // 内容跟着同一条"`paths` 赢"的规矩：一条**没有**声明内容的显式路径会把
            // 同名的 `dirs` 条目刚写进去的内容撤掉。否则 `inspect` 说它是符号链接、
            // 而 `read` 还能读到一份旧内容 —— 两句话对不上，而用例会照着其中一句写断言。
            match &entry.content {
                Some(content) => {
                    contents.insert(key, content.clone());
                }
                None => {
                    contents.remove(&key);
                }
            }
        }

        Self {
            index,
            dirs,
            contents,
            reads: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// 这本账里**被读过**的路径（归一化后的形态：分隔符统一成 `\`、大小写折叠）。
    ///
    /// 给"不许读某个文件"这类断言用，见 `reads` 字段上的说明。**顺序是读取顺序**，
    /// 同一个路径读两次会出现两次 —— 调用方要的是"**有没有**读过它"。
    #[must_use]
    pub fn reads(&self) -> Vec<String> {
        self.reads.borrow().clone()
    }
}

impl FixturePath {
    fn to_facts(&self, path: &Path) -> FileFacts {
        if !self.exists {
            return FileFacts::missing(path);
        }
        FileFacts {
            path: path.to_path_buf(),
            exists: true,
            is_dir: self.is_dir,
            size: self.size,
            reparse: self.reparse,
            link_target: self.link_target.clone(),
            error: None,
        }
    }

    /// 固定装置自证：声明了 `content` 时，`size` 必须等于内容的**字节数**（决策 186）。
    ///
    /// # 为什么在这里炸，而不是在 `read` 里返回点什么
    ///
    /// 真机上这两个数**可以**不同（App Execution Alias 是 0 字节、稀疏文件、并发截断），
    /// 但固定装置里两个数**都是我们自己写的** —— 不一致永远是笔误，而不是"机器长这样"。
    /// 失败模式选"构造时炸"而不是"读的时候返回某个东西"：后者会让一条断言**因为错误的
    /// 理由变绿**（用例想验 494 字节的路径，实际读的是 12 字节的另一份内容）。
    ///
    /// 消息里必须**同时**有路径与两个数：只有"size 不匹配"的话，固定装置一多就得靠猜。
    fn check_size_matches_content(&self, full_path: &str) {
        let Some(content) = &self.content else {
            return;
        };
        let bytes = content.len() as u64;
        assert_eq!(
            self.size, bytes,
            "固定装置自相矛盾（决策 186）：`{full_path}` 声明了 size = {}，\
             而 `content` 是 {bytes} 字节。两个数都是我们自己写的，不一致永远是笔误 —— \
             用 `FixturePath::file_with_content` 让 size 跟着内容走。",
            self.size
        );
    }
}

impl FileSystem for FakeFileSystem {
    fn inspect(&self, path: &Path) -> FileFacts {
        self.index
            .get(&normalize_path(&path.to_string_lossy()))
            .cloned()
            .unwrap_or_else(|| FileFacts::missing(path))
    }

    fn list_dir(&self, path: &Path) -> Vec<DirEntryFacts> {
        self.dirs
            .get(&normalize_path(&path.to_string_lossy()))
            .cloned()
            .unwrap_or_default()
    }

    /// 读固定装置里声明的内容。
    ///
    /// 语义与真实实现**逐条对应**，除了"内容从哪来"：
    ///
    /// | 固定装置里 | 得到 |
    /// |---|---|
    /// | 不存在（或 `exists = false`） | [`ReadOutcome::NotFound`] |
    /// | 是目录 | [`ReadOutcome::Unreadable`] |
    /// | 有 `content`，`content.len() <= limit` | [`ReadOutcome::Bytes`] |
    /// | 有 `content`，但比 `limit` 长 | [`ReadOutcome::TooLarge { size: content.len() }`] |
    /// | 有文件、**没写 `content`** | [`ReadOutcome::Unreadable`]（**故意**，见下） |
    ///
    /// 最后那一条是这一层最重要的语义：固定装置说"这个文件存在、`size = 494`"时，
    /// 我们**不知道**那 494 字节是什么。要么诚实地答"读不了"（于是用例必须显式写出
    /// 内容才能测"读到了什么"），要么编一段内容出来 —— 后者会让一个测凭据扫描的用例
    /// 在**没有任何 token 的输入**上通过。前者是唯一不会说谎的那个。
    fn read(&self, path: &Path, limit: u64) -> ReadOutcome {
        let key = normalize_path(&path.to_string_lossy());
        // 先记账再答：**"读过"这件事与"读没读到"无关** —— 一次落到 NotFound 的读
        // 也是读（`reads()` 要能看见"有人去够过那个文件"）。
        self.reads.borrow_mut().push(key.clone());
        let Some(facts) = self.index.get(&key) else {
            return ReadOutcome::NotFound;
        };
        if !facts.exists {
            return ReadOutcome::NotFound;
        }
        if facts.is_dir {
            return ReadOutcome::Unreadable {
                message: "是一个目录，不是一个文件".to_owned(),
            };
        }
        let Some(content) = self.contents.get(&key) else {
            return ReadOutcome::Unreadable {
                message: "固定装置没有声明这个文件的内容（`content`）".to_owned(),
            };
        };
        if content.len() as u64 > limit {
            return ReadOutcome::TooLarge {
                size: content.len() as u64,
            };
        }
        ReadOutcome::Bytes(content.as_bytes().to_vec())
    }
}

/// 读固定装置的假注册表。
#[derive(Debug, Clone, Default)]
pub struct FakeRegistry {
    /// `RefCell` 是因为票据 #8 起 `Registry` 有了写方法，而这个实现要在
    /// `&self` 下改内容。与 `FakeProcessRunner.calls` 同一个理由、同一个做法。
    ///
    /// **`Rc` 也是必须的**：克隆一份假注册表必须克隆出**同一个**注册表，
    /// 而不是一张独立的副本。真实注册表是全局的 —— `RealEnvBlock` 与 `RealRegistry`
    /// 都是零大小类型、读的是同一个真注册表；假实现如果克隆出副本，
    /// 「写进去之后再读一遍」这种用例就会读到一份永远不变的旧快照。
    keys: Rc<RefCell<HashMap<(RegHive, String), FixtureKey>>>,
}

impl FakeRegistry {
    fn from_fixture(fixture: &MachineFixture) -> Self {
        let keys = fixture
            .registry
            .iter()
            .map(|key| {
                (
                    (key.hive, normalize_key(&key.path)),
                    FixtureKey {
                        path: normalize_key(&key.path),
                        ..key.clone()
                    },
                )
            })
            .collect();
        Self {
            keys: Rc::new(RefCell::new(keys)),
        }
    }

    /// 取一个已声明的键。**返回克隆而不是引用**：写方法会在 `&self` 下改这个表，
    /// 所以不能把内部借用交出去。
    fn declared(&self, hive: RegHive, key: &str) -> Option<FixtureKey> {
        self.keys.borrow().get(&(hive, key.to_owned())).cloned()
    }

    /// 从声明里**推导**直接子键。
    ///
    /// 为什么要推导而不是让固定装置自己列一遍：父键的列表与子键的声明是同一份事实，
    /// 写两遍就会漂移（而漂移出来的假注册表会让用例测出一个真机器上不存在的形状）。
    fn derived_subkeys(&self, hive: RegHive, key: &str) -> Vec<String> {
        let prefix = if key.is_empty() {
            String::new()
        } else {
            format!("{key}\\")
        };
        let borrowed = self.keys.borrow();
        let mut children: Vec<String> = borrowed
            .keys()
            .filter(|(child_hive, path)| {
                *child_hive == hive
                    && path.as_str() != key
                    && path.starts_with(&prefix)
                    && !path[prefix.len()..].contains('\\')
            })
            .map(|(_, path)| path[prefix.len()..].to_owned())
            .collect();
        children.sort();
        children
    }

    fn absent(hive: RegHive, key: &str) -> PlatformError {
        PlatformError::RegistryKeyMissing {
            hive: hive.as_str().to_owned(),
            subkey: key.to_owned(),
        }
    }
}

impl Registry for FakeRegistry {
    fn subkeys(&self, hive: RegHive, subkey: &str) -> Result<Vec<String>, PlatformError> {
        let key = normalize_key(subkey);
        if let Some(declared) = self.declared(hive, &key) {
            if !declared.exists {
                return Err(Self::absent(hive, &key));
            }
            return Ok(self.derived_subkeys(hive, &key));
        }
        let derived = self.derived_subkeys(hive, &key);
        if derived.is_empty() && !key.is_empty() {
            return Err(Self::absent(hive, &key));
        }
        Ok(derived)
    }

    fn values(
        &self,
        hive: RegHive,
        subkey: &str,
    ) -> Result<Vec<(String, RegValue)>, PlatformError> {
        let key = normalize_key(subkey);
        if let Some(declared) = self.declared(hive, &key) {
            if !declared.exists {
                return Err(Self::absent(hive, &key));
            }
            return Ok(declared
                .values
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect());
        }
        if self.derived_subkeys(hive, &key).is_empty() && !key.is_empty() {
            return Err(Self::absent(hive, &key));
        }
        Ok(Vec::new())
    }

    /// 假注册表的写：直接改内存里的那张表。
    ///
    /// **刻意不做任何"真实感"的加工**（不写日志、不延时）。测试要断言的是
    /// 「我们的代码写了什么值、什么类型、写到哪个键」，而不是模拟注册表的行为。
    fn set_value(
        &self,
        hive: RegHive,
        subkey: &str,
        name: &str,
        value: &RegValue,
    ) -> Result<(), PlatformError> {
        let key = normalize_key(subkey);
        let mut keys = self.keys.borrow_mut();
        let entry = keys
            .entry((hive, key.clone()))
            .or_insert_with(|| FixtureKey {
                hive,
                path: key.clone(),
                exists: true,
                values: BTreeMap::new(),
            });
        entry.values.insert(name.to_owned(), value.clone());
        Ok(())
    }

    fn delete_value(&self, hive: RegHive, subkey: &str, name: &str) -> Result<(), PlatformError> {
        let key = normalize_key(subkey);
        if let Some(entry) = self.keys.borrow_mut().get_mut(&(hive, key)) {
            entry.values.remove(name);
        }
        Ok(())
    }
}

/// 一次版本探测调用。测试用它断言**传了哪个参数**
/// （`java` 要 `-version`，`node` 要 `-v`，这件事很容易悄悄错）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessCall {
    /// 程序路径。
    pub program: String,
    /// 参数。
    pub args: Vec<String>,
    /// 这次调用**额外塞进子进程环境**的变量（顺序即调用方给的顺序）。
    ///
    /// # 为什么假运行器要记它（票据 #23）
    ///
    /// 决策 27 只允许用环境变量重定向包管理器，而"我们到底给子进程设了什么"
    /// 是一个**只有调用方知道**的事实：真机上它被 `CreateProcess` 吞进环境块，
    /// 事后没有任何地方能读回来。固定装置是唯一能逐字断言它的地方 ——
    /// 所以 [`ProcessRunner::run_env`] 的 `env` 参数原样进这里。
    ///
    /// **注意它记录的是"我们设置的覆盖项"，不是子进程看到的完整环境块**：
    /// 继承来的那些（`PATH`、`PATHEXT`、`USERPROFILE`…）不在里面，也不该在里面
    /// （那会把开发机的状态带进断言）。
    pub env: Vec<(String, String)>,
}

/// 读固定装置的假进程运行器。
///
/// # 匹配规则（决策 185）—— 规则本身就是被测对象
///
/// 一次 `run(program, args)` 命中哪一条 [`FixtureProcess`]，按顺序：
///
/// 1. `normalize_path(program)` **相等**；
/// 2. 且（`entry.args` **为空** → 通配，任何参数都算命中）**或**（调用的 `args`
///    **以 `entry.args` 开头**：逐元素相等、顺序敏感、长度可以更长）；
/// 3. 多个命中时 **`args` 最长的赢**（精确的压过通配的）；
/// 4. 仍然并列时按**声明顺序**取第一个。
///
/// 第 3、4 条一起保证"同一次调用只有一种解释"：没有它们，一份同时写了
/// `cmd.exe /C npm.cmd config get prefix` 与 `cmd.exe /C npm.cmd ls -g …` 的固定装置
/// 会取决于**迭代顺序**给出两份不同的 stdout —— 而 `HashMap`/`read_dir` 那类顺序
/// 在本仓库已经被明确禁止依赖（决策 35：输出必须逐字节稳定）。
#[derive(Debug, Clone, Default)]
pub struct FakeProcessRunner {
    entries: Vec<FixtureProcess>,
    calls: RefCell<Vec<ProcessCall>>,
}

/// 声明的参数是不是这次调用的**前缀**（决策 185 的第 2 条）。
///
/// 空 = 通配。`["config"]` 命中 `["config", "get", "prefix"]`，但**不**命中
/// `["config-get"]`（逐元素比较，不做字符串前缀）也不命中 `["get", "config"]`（顺序敏感）。
fn args_are_a_prefix(declared: &[String], called: &[String]) -> bool {
    declared.len() <= called.len()
        && declared
            .iter()
            .zip(called)
            .all(|(declared, called)| declared == called)
}

impl FakeProcessRunner {
    fn from_fixture(fixture: &MachineFixture) -> Self {
        Self {
            entries: fixture.processes.clone(),
            calls: RefCell::new(Vec::new()),
        }
    }

    /// 到目前为止被调用过哪些程序、带什么参数。
    #[must_use]
    pub fn calls(&self) -> Vec<ProcessCall> {
        self.calls.borrow().clone()
    }
}

impl ProcessRunner for FakeProcessRunner {
    /// 记录 `program` / `args` / `env`，然后按决策 185 的规则取一条声明。
    ///
    /// **`env` 不参与匹配**：匹配规则是"program 相等 + args 前缀"（决策 185 冻结的
    /// 三条），把环境也变成匹配维度会让"这条为什么没命中"多出一个说不清的理由。
    /// 环境是**断言的对象**（[`ProcessCall::env`]），不是选择条目的判据。
    fn run_env(
        &self,
        program: &Path,
        args: &[&str],
        env: &[(String, String)],
        _timeout: Duration,
    ) -> ProcessOutcome {
        let wanted = normalize_path(&program.to_string_lossy());
        let called: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        self.calls.borrow_mut().push(ProcessCall {
            program: program.to_string_lossy().into_owned(),
            args: called.clone(),
            env: env.to_vec(),
        });

        // 决策 185 的四条规则（见 `FakeProcessRunner` 的文档）。"严格更长才替换"这一步
        // 同时实现了"最长赢"与"同长按声明顺序"—— 后者靠 `>` 而不是 `>=`。
        let mut matched: Option<&FixtureProcess> = None;
        for entry in &self.entries {
            if normalize_path(&entry.program) != wanted || !args_are_a_prefix(&entry.args, &called)
            {
                continue;
            }
            if matched.is_none_or(|best| entry.args.len() > best.args.len()) {
                matched = Some(entry);
            }
        }

        match matched {
            Some(entry) => ProcessOutcome {
                spawned: true,
                timed_out: entry.timed_out,
                exit_code: entry.exit_code,
                // 固定装置里的输出是文本，所以两种形式相同。
                // 真实进程的 `stdout_bytes` 可能不是合法 UTF-8，但固定装置
                // 表达的是"版本探测的输出"，那种输出本来就是文本。
                stdout_bytes: entry.stdout.clone().into_bytes(),
                stdout: entry.stdout.clone(),
                stderr: entry.stderr.clone(),
                spawn_error: None,
            },
            // 固定装置里没写 = 这个程序起不来。**不编一个空版本**，
            // 因为"发现但版本未知"与"根本没跑起来"在输出里必须能区分。
            //
            // 两条消息分开写：写了 `args` 之后，"程序在、但参数没对上"会变成最常见的
            // 固定装置笔误（比如把 `["config", "get", "prefix"]` 写成了
            // `["config", "get", "prefix"]` 之外的顺序），而一句
            // "固定装置里没有这个程序"会让人去查 program —— 查错地方。
            None => ProcessOutcome::not_spawned(
                if self
                    .entries
                    .iter()
                    .any(|entry| normalize_path(&entry.program) == wanted)
                {
                    "固定装置里有这个程序，但没有一条的 `args` 是这次调用的前缀"
                } else {
                    "固定装置里没有这个程序"
                },
            ),
        }
    }
}

/// 读固定装置的假安装记录。
#[derive(Debug, Clone, Default)]
pub struct FakeManagedStore {
    tools: Vec<ManagedTool>,
}

impl ManagedStore for FakeManagedStore {
    fn installed(&self) -> Vec<ManagedTool> {
        self.tools.clone()
    }
}

/// 一台假的机器：四个后端 + 环境块。
#[derive(Debug, Clone)]
pub struct FakeMachine {
    /// 进程环境块。
    pub env: InMemoryEnv,
    /// 文件系统。
    pub fs: FakeFileSystem,
    /// 注册表。
    pub registry: FakeRegistry,
    /// 进程运行器。
    pub runner: FakeProcessRunner,
    /// tuoen 自己的安装记录。
    pub managed: FakeManagedStore,
}

impl MachineFixture {
    /// 按这份描述造一台假机器。
    #[must_use]
    pub fn build(&self) -> FakeMachine {
        FakeMachine {
            env: InMemoryEnv::new(
                self.env
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect(),
            ),
            fs: FakeFileSystem::from_fixture(self),
            registry: FakeRegistry::from_fixture(self),
            runner: FakeProcessRunner::from_fixture(self),
            managed: FakeManagedStore {
                tools: self.managed.clone(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> MachineFixture {
        MachineFixture {
            env: BTreeMap::from([("Path".to_owned(), r"C:\machine;C:\user".to_owned())]),
            paths: vec![FixturePath::file(r"C:\machine\node.exe", 1024)],
            dirs: vec![FixtureDir::new(
                r"C:\machine",
                vec![
                    FixturePath::file("node.exe", 1024),
                    FixturePath::app_exec_alias("python.exe"),
                ],
            )],
            registry: vec![
                FixtureKey::new(
                    RegHive::Hklm,
                    r"Microsoft\Windows\CurrentVersion\App Paths",
                    BTreeMap::new(),
                ),
                FixtureKey::new(
                    RegHive::Hklm,
                    r"Microsoft\Windows\CurrentVersion\App Paths\foo.exe",
                    BTreeMap::from([(
                        String::new(),
                        RegValue::Sz(r"C:\machine\foo.exe".to_owned()),
                    )]),
                ),
            ],
            processes: vec![FixtureProcess {
                program: r"C:\machine\node.exe".to_owned(),
                args: Vec::new(),
                stdout: "v1.2.3\n".to_owned(),
                stderr: String::new(),
                exit_code: Some(0),
                timed_out: false,
            }],
            managed: Vec::new(),
        }
    }

    #[test]
    fn the_fixture_is_never_confused_with_the_real_machine() {
        // 这是本文件存在的理由：假文件系统对**任何**未声明的路径都答"不存在"，
        // 所以一个忘了写固定装置的用例会失败，而不是静默读到开发机的真实磁盘。
        let machine = fixture().build();
        assert!(
            !machine
                .fs
                .inspect(Path::new(r"C:\Windows\System32\cmd.exe"))
                .exists
        );
        assert!(!machine.fs.inspect(Path::new(r"C:\Dev")).exists);
        assert!(machine.fs.list_dir(Path::new(r"C:\Windows")).is_empty());
        // 注册表也一样：只有声明过的 hive + 键存在。
        assert!(
            machine
                .registry
                .subkeys(RegHive::Hkcu, r"Microsoft\Windows\CurrentVersion\Uninstall")
                .unwrap_err()
                .is_absent()
        );
    }

    #[test]
    fn listing_and_existence_agree_because_they_share_one_declaration() {
        let machine = fixture().build();
        let entries = machine.fs.list_dir(Path::new(r"C:\machine"));
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["node.exe", "python.exe"]);
        assert!(machine.fs.inspect(Path::new(r"C:\machine\node.exe")).exists);
        assert!(
            machine
                .fs
                .inspect(Path::new(r"C:\machine\python.exe"))
                .is_file()
        );
        assert!(
            machine
                .fs
                .inspect(Path::new(r"C:\machine\python.exe"))
                .reparse
                .is_app_exec_alias()
        );
    }

    #[test]
    fn path_lookup_is_case_insensitive_and_separator_insensitive() {
        let machine = fixture().build();
        assert!(machine.fs.inspect(Path::new("c:/MACHINE/NODE.EXE")).exists);
        assert!(
            machine
                .fs
                .inspect(Path::new(r"C:\machine\node.exe\"))
                .exists
        );
    }

    #[test]
    fn registry_subkeys_are_derived_from_the_declared_keys() {
        let machine = fixture().build();
        assert_eq!(
            machine
                .registry
                .subkeys(RegHive::Hklm, r"Microsoft\Windows\CurrentVersion\App Paths")
                .expect("父键存在"),
            vec!["foo.exe".to_owned()]
        );
        // 没声明的键 = 不存在，而且是"没有东西"而不是失败。
        let err = machine
            .registry
            .subkeys(RegHive::Hklm, r"Microsoft\Nope")
            .unwrap_err();
        assert!(err.is_absent(), "{err}");
        // 值也要读得到，且默认值的名字是空字符串。
        assert_eq!(
            machine.registry.value(
                RegHive::Hklm,
                r"Microsoft\Windows\CurrentVersion\App Paths\foo.exe",
                ""
            ),
            Some(RegValue::Sz(r"C:\machine\foo.exe".to_owned()))
        );
    }

    #[test]
    fn registry_hives_do_not_leak_into_each_other() {
        let machine = fixture().build();
        assert!(
            machine
                .registry
                .subkeys(RegHive::Hkcu, r"Microsoft\Windows\CurrentVersion\App Paths")
                .is_err(),
            "同一路径在另一个 hive 里没有声明，就必须不存在"
        );
    }

    #[test]
    fn an_undeclared_program_is_not_spawned_rather_than_invented() {
        let machine = fixture().build();
        let outcome = machine.runner.run(
            Path::new(r"C:\machine\python.exe"),
            &["--version"],
            Duration::from_millis(100),
        );
        assert!(!outcome.spawned);
        assert!(outcome.spawn_error.is_some());
        let outcome = machine.runner.run(
            Path::new(r"C:\MACHINE\node.exe"),
            &["-v"],
            Duration::from_millis(100),
        );
        assert_eq!(outcome.stdout, "v1.2.3\n");
        assert_eq!(machine.runner.calls().len(), 2);
        assert_eq!(machine.runner.calls()[1].args, vec!["-v".to_owned()]);
    }

    #[test]
    fn normalization_keeps_bare_drives_and_unc_prefixes() {
        assert_eq!(normalize_path(r"C:\Tools\"), r"c:\tools");
        assert_eq!(normalize_path(r"C:\"), r"c:\");
        assert_eq!(normalize_path("C:/"), r"c:\");
        assert_eq!(normalize_path(r"C:\\Tools\\bin"), r"c:\tools\bin");
        assert_eq!(normalize_path(r"\\srv\share\bin\"), r"\\srv\share\bin");
    }

    // ------------------------------------------------------------------
    // `FixtureProcess.args` 的匹配规则（决策 185）—— 规则本身就是被测对象。
    // ------------------------------------------------------------------

    /// 六次调用共用同一个 `cmd.exe` 的那份固定装置（决策 168 的形状）。
    fn cmd_fixture() -> MachineFixture {
        fn cmd(args: &[&str], stdout: &str) -> FixtureProcess {
            FixtureProcess {
                program: r"C:\Windows\System32\cmd.exe".to_owned(),
                args: args.iter().map(|arg| (*arg).to_owned()).collect(),
                stdout: stdout.to_owned(),
                stderr: String::new(),
                exit_code: Some(0),
                timed_out: false,
            }
        }
        MachineFixture {
            processes: vec![
                cmd(&[], "通配：任何参数都到这里"),
                cmd(
                    &["/C", "npm.cmd", "config", "get", "prefix"],
                    "C:\\nvm4w\\nodejs\n",
                ),
                cmd(
                    &[
                        "/C",
                        "npm.cmd",
                        "ls",
                        "-g",
                        "--json",
                        "--depth=0",
                        "--offline",
                    ],
                    "[]",
                ),
                cmd(&["/C", "npm.cmd"], "短前缀"),
                cmd(
                    &["/C", "git.exe", "config", "--system", "--list"],
                    "system 层",
                ),
                cmd(
                    &["/C", "git.exe", "config", "--global", "--list"],
                    "global 层",
                ),
            ],
            ..MachineFixture::default()
        }
    }

    fn ask(machine: &FakeMachine, args: &[&str]) -> ProcessOutcome {
        machine.runner.run(
            Path::new(r"C:\Windows\System32\cmd.exe"),
            args,
            Duration::from_millis(100),
        )
    }

    #[test]
    fn an_exact_args_entry_beats_the_wildcard_for_the_same_program() {
        let machine = cmd_fixture().build();
        let outcome = ask(&machine, &["/C", "npm.cmd", "config", "get", "prefix"]);
        assert_eq!(outcome.stdout, "C:\\nvm4w\\nodejs\n");
        // 通配那条仍然给"完全没写 args 的调用"兜底（旧固定装置的行为）。
        let outcome = ask(&machine, &["/C", "whoami"]);
        assert_eq!(outcome.stdout, "通配：任何参数都到这里");
    }

    #[test]
    fn the_longest_args_prefix_wins() {
        let machine = cmd_fixture().build();
        // `["/C", "npm.cmd"]`（2 个）与 `["/C","npm.cmd","config","get","prefix"]`（5 个）
        // 都是这次调用的前缀 —— 长的赢，否则"npm 的前缀"会把每一个 npm 子命令都吃掉。
        assert_eq!(
            ask(&machine, &["/C", "npm.cmd", "config", "get", "prefix"]).stdout,
            "C:\\nvm4w\\nodejs\n"
        );
        assert_eq!(
            ask(
                &machine,
                &[
                    "/C",
                    "npm.cmd",
                    "ls",
                    "-g",
                    "--json",
                    "--depth=0",
                    "--offline"
                ]
            )
            .stdout,
            "[]"
        );
        // 只写了短前缀的调用命中短的那条 —— 它没被更长的条目抢走。
        assert_eq!(
            ask(&machine, &["/C", "npm.cmd", "--version"]).stdout,
            "短前缀"
        );
        // 两次 git 调用拿到两份**不同**的输出：这是"只按 program 匹配"做不到的事。
        assert_eq!(
            ask(&machine, &["/C", "git.exe", "config", "--system", "--list"]).stdout,
            "system 层"
        );
        assert_eq!(
            ask(&machine, &["/C", "git.exe", "config", "--global", "--list"]).stdout,
            "global 层"
        );
    }

    #[test]
    fn equal_length_prefixes_are_decided_by_declaration_order() {
        let machine = MachineFixture {
            processes: vec![
                FixtureProcess {
                    program: r"C:\bin\tool.exe".to_owned(),
                    args: vec!["--json".to_owned()],
                    stdout: "先声明的".to_owned(),
                    stderr: String::new(),
                    exit_code: Some(0),
                    timed_out: false,
                },
                FixtureProcess {
                    program: r"C:\bin\tool.exe".to_owned(),
                    args: vec!["--json".to_owned()],
                    stdout: "后声明的".to_owned(),
                    stderr: String::new(),
                    exit_code: Some(0),
                    timed_out: false,
                },
            ],
            ..MachineFixture::default()
        }
        .build();
        // 同长（都是 1）时取**声明顺序的第一个** —— 没有这条，答案取决于迭代顺序，
        // 而"同一台机器跑两次逐字节相同"（决策 35）就没了。
        let outcome = machine.runner.run(
            Path::new(r"C:\bin\tool.exe"),
            &["--json", "--depth=0"],
            Duration::from_millis(100),
        );
        assert_eq!(outcome.stdout, "先声明的");
    }

    #[test]
    fn args_match_element_wise_and_in_order_and_not_as_a_string_prefix() {
        let machine = cmd_fixture().build();
        // 顺序反了 → 不命中（会落到通配那条）。
        assert_eq!(
            ask(&machine, &["/C", "config", "npm.cmd", "get", "prefix"]).stdout,
            "通配：任何参数都到这里"
        );
        // 逐元素比较，不是字符串前缀：`npm.cmd-extra` 不是 `npm.cmd`。
        assert_eq!(
            ask(&machine, &["/C", "npm.cmd-extra", "config"]).stdout,
            "通配：任何参数都到这里"
        );
        // 调用的参数比声明的**少** → 不命中（前缀只允许更长）。
        assert_eq!(ask(&machine, &["/C"]).stdout, "通配：任何参数都到这里");
    }

    #[test]
    fn a_program_that_exists_but_whose_args_miss_says_that_and_not_something_else() {
        let machine = MachineFixture {
            processes: vec![FixtureProcess {
                program: r"C:\bin\tool.exe".to_owned(),
                args: vec!["--json".to_owned()],
                stdout: String::new(),
                stderr: String::new(),
                exit_code: Some(0),
                timed_out: false,
            }],
            ..MachineFixture::default()
        }
        .build();
        // 程序在、参数没对上：消息必须指向**参数**，否则固定装置作者会去查 program。
        let missed = machine.runner.run(
            Path::new(r"C:\bin\tool.exe"),
            &["--plain"],
            Duration::from_millis(100),
        );
        assert!(!missed.spawned);
        assert!(
            missed
                .spawn_error
                .as_deref()
                .is_some_and(|m| m.contains("args")),
            "消息要说清是参数没对上：{:?}",
            missed.spawn_error
        );
        // 程序根本不在：另一条消息（"发现但版本未知"与"根本没跑起来"必须能区分）。
        let absent = machine.runner.run(
            Path::new(r"C:\bin\other.exe"),
            &["--json"],
            Duration::from_millis(100),
        );
        assert!(!absent.spawned);
        assert!(
            absent
                .spawn_error
                .as_deref()
                .is_some_and(|m| m.contains("没有这个程序")),
            "{:?}",
            absent.spawn_error
        );
    }

    // ------------------------------------------------------------------
    // `FixturePath.content` 与 `FakeFileSystem::read`
    // ------------------------------------------------------------------

    /// 一份"配置文件在目录里"的固定装置（`fixtures/**` 里的形状）。
    fn config_fixture() -> MachineFixture {
        MachineFixture {
            dirs: vec![FixtureDir::new(
                r"C:\Users\dev\.m2",
                vec![
                    FixturePath::file_with_content(
                        "settings.xml",
                        "<settings><servers/></settings>",
                    ),
                    // 声明了文件、**故意**不写内容。
                    FixturePath::file("settings-security.xml", 494),
                ],
            )],
            paths: vec![
                FixturePath::file_with_content(r"C:\Users\dev\.gitconfig", "[user]\n"),
                FixturePath::file(r"C:\Users\dev\.npmrc", 88),
                FixturePath::missing(r"C:\Users\dev\.docker\config.json"),
            ],
            ..MachineFixture::default()
        }
    }

    #[test]
    fn content_inside_a_directory_entry_is_reachable_by_its_full_path() {
        let machine = config_fixture().build();
        assert_eq!(
            machine
                .fs
                .read(Path::new(r"C:\Users\dev\.m2\settings.xml"), 4096),
            ReadOutcome::Bytes(b"<settings><servers/></settings>".to_vec()),
            "目录条目里的内容必须按**完整路径**读得到（`dirs[].entries[]` 的相对名）"
        );
        // 大小写与分隔符不敏感（与 `inspect` 同一条归一化）。
        assert_eq!(
            machine
                .fs
                .read(Path::new("c:/USERS/DEV/.m2/SETTINGS.XML"), 4096),
            ReadOutcome::Bytes(b"<settings><servers/></settings>".to_vec())
        );
    }

    #[test]
    fn a_declared_file_without_content_is_unreadable_on_purpose() {
        let machine = config_fixture().build();
        // 固定装置说"这个文件存在、494 字节"，但我们**不知道**那 494 字节是什么。
        // 编一段内容出来会让"扫凭据形状"的用例在没有任何 token 的输入上通过 ——
        // 所以这里必须是 Unreadable，而且理由要说得出。
        assert!(
            machine
                .fs
                .inspect(Path::new(r"C:\Users\dev\.m2\settings-security.xml"))
                .exists
        );
        match machine
            .fs
            .read(Path::new(r"C:\Users\dev\.m2\settings-security.xml"), 4096)
        {
            ReadOutcome::Unreadable { message } => {
                assert!(message.contains("content"), "要说清缺什么：{message}");
            }
            other => panic!("没声明内容必须是 Unreadable，实际 {other:?}"),
        }
        // 顶层 `paths` 里的同一种形状。
        assert!(
            matches!(
                machine.fs.read(Path::new(r"C:\Users\dev\.npmrc"), 4096),
                ReadOutcome::Unreadable { .. }
            ),
            "只有 size 的条目读不出内容"
        );
    }

    #[test]
    fn a_directory_is_unreadable_and_an_undeclared_path_is_not_found() {
        let machine = config_fixture().build();
        // 目录：存在，但"读它"这件事不成立 —— 不是 NotFound。
        assert!(
            matches!(
                machine.fs.read(Path::new(r"C:\Users\dev\.m2"), 4096),
                ReadOutcome::Unreadable { .. }
            ),
            "目录不是 NotFound"
        );
        // 不存在：`paths` 里显式写了 `exists = false` 的，与完全没声明的，都是 NotFound。
        assert_eq!(
            machine
                .fs
                .read(Path::new(r"C:\Users\dev\.docker\config.json"), 4096),
            ReadOutcome::NotFound
        );
        assert_eq!(
            machine
                .fs
                .read(Path::new(r"C:\Windows\System32\cmd.exe"), 4096),
            ReadOutcome::NotFound,
            "没声明的路径永远是 NotFound —— 这是假文件系统存在的理由"
        );
    }

    #[test]
    fn too_large_uses_the_real_content_length_and_not_the_limit() {
        let machine = config_fixture().build();
        // 内容 31 字节（`"<settings><servers/></settings>"`），limit 30 → TooLarge，
        // 且 `size` 是 **31**（不是 30）。
        assert_eq!(
            machine
                .fs
                .read(Path::new(r"C:\Users\dev\.m2\settings.xml"), 30),
            ReadOutcome::TooLarge { size: 31 }
        );
        // 恰好 limit 的那一边读得到。
        assert_eq!(
            machine
                .fs
                .read(Path::new(r"C:\Users\dev\.m2\settings.xml"), 31),
            ReadOutcome::Bytes(b"<settings><servers/></settings>".to_vec())
        );
    }

    #[test]
    fn an_explicit_path_entry_overrides_the_directory_entrys_content() {
        // 与 `index` 同一条"`paths` 赢"的规矩：显式路径把同名目录条目的内容撤掉，
        // 否则 `inspect` 说它是符号链接、而 `read` 还能读到一份旧内容。
        let machine = MachineFixture {
            dirs: vec![FixtureDir::new(
                r"C:\nvm4w",
                vec![FixturePath::file_with_content("nodejs", "目录条目的内容")],
            )],
            paths: vec![FixturePath::symlink_dir(
                r"C:\nvm4w\nodejs",
                r"C:\Users\dev\AppData\Local\nvm\v24.19.0",
            )],
            ..MachineFixture::default()
        }
        .build();
        let facts = machine.fs.inspect(Path::new(r"C:\nvm4w\nodejs"));
        assert!(facts.reparse.is_link(), "显式路径赢：它是符号链接");
        assert!(
            matches!(
                machine.fs.read(Path::new(r"C:\nvm4w\nodejs"), 4096),
                ReadOutcome::Unreadable { .. }
            ),
            "内容跟着同一条规矩被撤掉，不许留下一个自相矛盾的答案"
        );
    }

    #[test]
    fn file_with_content_keeps_size_and_content_in_step() {
        let file = FixturePath::file_with_content(r"C:\a\.gitconfig", "[user]\nname=x\n");
        assert_eq!(file.size, file.content.as_deref().unwrap().len() as u64);
        assert_eq!(file.size, 14);
        assert_eq!(FixturePath::file(r"C:\a\x", 10).content, None);
    }

    #[test]
    fn content_is_omitted_from_toml_when_it_is_not_declared() {
        // 加字段是**加法**：没写 `content` 的固定装置序列化回来必须一个字节都不变
        // （`fixtures/**` 里那几十份 TOML 不能因为这一票而集体变样）。
        let fixture = MachineFixture {
            paths: vec![
                FixturePath::file(r"C:\a\.npmrc", 88),
                FixturePath::file_with_content(r"C:\a\.gitconfig", "[user]\n"),
            ],
            ..MachineFixture::default()
        };
        let text = toml::to_string(&fixture).expect("序列化");
        assert_eq!(
            text.matches("content = ").count(),
            1,
            "只有声明了内容的那一条才出键：\n{text}"
        );
        // 往返：字段名写错会被 `deny_unknown_fields` 当场拒绝。
        let back: MachineFixture = toml::from_str(&text).expect("反序列化");
        assert_eq!(back, fixture);
    }

    /// 决策 186：固定装置里 `size` 与 `content` 不一致 → **构造时就炸**。
    ///
    /// 期望的消息里**同时**有路径与两个数：只报"size 不匹配"的话，固定装置一多就得靠猜
    /// 是哪一条。`should_panic(expected = …)` 是子串匹配，所以这一条同时钉住了三者。
    #[test]
    #[should_panic(
        expected = "`C:\\Users\\dev\\.npmrc` 声明了 size = 494，而 `content` 是 19 字节"
    )]
    fn a_fixture_whose_size_disagrees_with_its_content_is_refused_when_it_is_built() {
        // 19 字节的内容配 494 的 `size`：两个数都是我们自己写的，不一致永远是笔误。
        // 失败模式必须是"构造时炸"—— 在 `read` 里返回点什么会让断言**因为错误的理由变绿**。
        let _ = MachineFixture {
            paths: vec![FixturePath {
                path: r"C:\Users\dev\.npmrc".to_owned(),
                size: 494,
                content: Some("allow-scripts=false".to_owned()),
                ..FixturePath::default()
            }],
            ..MachineFixture::default()
        }
        .build();
    }

    /// 同一条校验必须也覆盖 `dirs[].entries[]` —— 只写在一处的话，另一处就是"看不住"。
    #[test]
    #[should_panic(expected = "`C:\\Users\\dev\\.m2\\settings.xml` 声明了 size = 31")]
    fn the_size_check_also_covers_entries_inside_a_directory() {
        let _ = MachineFixture {
            dirs: vec![FixtureDir::new(
                r"C:\Users\dev\.m2",
                vec![FixturePath {
                    path: "settings.xml".to_owned(),
                    size: 31,
                    content: Some("很短".to_owned()),
                    ..FixturePath::default()
                }],
            )],
            ..MachineFixture::default()
        }
        .build();
    }
}
