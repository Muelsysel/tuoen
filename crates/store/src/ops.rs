//! store 的全部 I/O。**每个公开函数的第一件事都是校验名字。**
//!
//! # 四条贯穿本文件的设计
//!
//! ## ① 目录是事实来源，记录只是附注
//!
//! 一个**目录**就是一个已安装的版本，一份记录只是"从哪来的"。所以
//! [`installed_versions`] 从不因为"记录缺失/坏了"而失败或跳过 —— 它报
//! `record: None`。反过来，一份悬空的记录**不是**一个版本。
//!
//! ## ② 绝不跟随、绝不穿透重解析点
//!
//! 载荷位置上出现 junction / symlink 时一律 [`StoreError::Refused`]：
//! [`adopt_payload`] 不把载荷搬进一个链接位置、也不接受一个链接当载荷；
//! [`uninstall`] 不递归穿过链接删东西。理由是本层最不能出的事故就是"删错了地方"。
//!
//! ## ③ 无 `Result` 的查询函数遇到不安全的名字就返回"没有"
//!
//! [`installed_versions`] / [`find_version`] / [`active_version`] /
//! [`store_is_empty`] 的签名是 `Vec` / `Option` / `bool`，没有地方放
//! [`StoreError::UnsafeName`]。它们的做法是**在第一行拒掉**并返回空答案
//! （`Vec::new()` / `None` / `true`），**绝不把一个不安全的名字拼进路径去碰磁盘**。
//! 带 `Result` 的那几个（[`adopt_payload`] / [`write_record`] / [`activate`] /
//! [`deactivate`] / [`uninstall`]）一律在开头返回 `UnsafeName`。
//!
//! ## ④ 以 `.` 开头的名字是 store 的内部产物位
//!
//! `root/.incoming`（`tuoen install` 的临时解压区）与
//! `root/<tool>/versions/.staging-<label>-<n>`（`tuoen-archive` 的临时解压目录）
//! 都是**正在被写**的东西。它们**不算**工具/版本（见 [`installed_versions`]
//! 与 [`installed_tools`] 的跨 crate 契约），也不能被这些 API **创建**出来 ——
//! 否则会出现"装好了但列表里从来没见过"的版本，而那种失败用户查不出来。

use std::cmp::Ordering;
use std::path::{Component, Path, PathBuf};

use tuoen_platform::{
    FileFacts, FileSystem, RealFileSystem, ReparseKind, Repoint, junction_target, remove_junction,
    repoint_junction,
};

use crate::error::StoreError;
use crate::layout::{Store, check_component};
use crate::record::InstallRecord;

/// 名字类别，进 [`StoreError::UnsafeName::what`]。
const WHAT_TOOL: &str = "工具名";
/// 名字类别，进 [`StoreError::UnsafeName::what`]。
const WHAT_VERSION: &str = "版本号";

// ---------------------------------------------------------------------------
// 返回类型
// ---------------------------------------------------------------------------

/// 一个已安装的版本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledVersion {
    /// 版本号（磁盘上那一段的名字）。
    pub version: String,
    /// 载荷目录（`versions/<version>`）。
    pub path: PathBuf,
    /// 是不是 `current` 指向的那个。
    pub active: bool,
    /// 记录。目录在但记录缺失/坏了 → `None`（**不是错误**，目录才是事实来源）。
    pub record: Option<InstallRecord>,
    /// 载荷里的文件数（递归，根目录不算一个条目）。
    pub files: u64,
    /// 载荷里的字节数。
    pub bytes: u64,
}

/// 一次"把载荷搬进 store"的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptOutcome {
    /// 搬进去之后的位置（`versions/<version>`）—— 原来的载荷路径已经不存在了
    /// （`rename` 是移动，不是复制）。
    pub path: PathBuf,
    /// 落盘后数出来的文件数（与写进记录里的那个数字是**同一个**）。
    pub files: u64,
    /// 落盘后数出来的字节数。
    pub bytes: u64,
}

/// 卸载的选项。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UninstallOptions {
    /// 允许卸载**当前激活**的版本（会先摘掉 `current` 再删载荷）。
    pub force_active: bool,
}

/// 一次卸载的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UninstallOutcome {
    /// 被删的那份是不是当前激活的版本。
    pub was_active: bool,
    /// 被删掉的载荷文件数（**不含**记录文件）。
    pub freed_files: u64,
    /// 被删掉的载荷字节数（**不含**记录文件）。
    pub freed_bytes: u64,
}

// ---------------------------------------------------------------------------
// 版本号的逐段自然比较
// ---------------------------------------------------------------------------

/// 版本号的**逐段自然比较**。`24.9.0` < `24.21.0`（数字段按数值比，不是字符串比）。
///
/// [`installed_versions`] 的排序用它；`tuoen install`（不带版本号时取最新可安装版本）
/// 也用它 —— **两处必须同一套顺序**，否则"列表里最上面那个"与"装到的是哪个"
/// 会不一致，而那种不一致用户查不出来。
///
/// # 规则
///
/// * 分隔符只有 `.` 与 `+`（Temurin 的真实版本串是 `21.0.12.1+1`）；
///   其它字符都算**段内的文本**。
/// * 两段都是纯数字 → 按**数值**比。比较用"去前导零后先比长度、再比字典序"，
///   不解析成整数：版本段可以任意长，`u64` 会溢出，而溢出会静默给出错误的顺序。
/// * 否则按文本比（字典序）。
/// * 段数不同 → 段少的更小（`24.19` < `24.19.0`）。
/// * 逐段全等时用**原串**做最后的裁决：`1.01` 与 `1.1` 数值相等但**不是同一个目录名**，
///   没有这一步，排序结果就依赖目录枚举顺序（而 `--json` 必须逐字节稳定）。
#[must_use]
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    let mut left = a.split(['.', '+']);
    let mut right = b.split(['.', '+']);
    loop {
        match (left.next(), right.next()) {
            (None, None) => break,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => match compare_segment(x, y) {
                Ordering::Equal => {}
                different => return different,
            },
        }
    }
    a.cmp(b)
}

/// 一整段都是 ASCII 数字。
fn is_decimal(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// 一个版本段。
fn compare_segment(a: &str, b: &str) -> Ordering {
    match (is_decimal(a), is_decimal(b)) {
        (true, true) => compare_decimal(a, b),
        _ => a.cmp(b),
    }
}

/// 两个纯数字段按**数值**比，且不解析成整数（见 [`compare_versions`] 的规则）。
fn compare_decimal(a: &str, b: &str) -> Ordering {
    let a = a.trim_start_matches('0');
    let b = b.trim_start_matches('0');
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

// ---------------------------------------------------------------------------
// 名字
// ---------------------------------------------------------------------------

/// 一个名字是不是 store 的**内部产物位**（以 `.` 开头）。
///
/// 它**不是** [`check_component`] 的一部分：`.` 开头的名字在路径安全性上完全合法
/// （它就是本层自己的 `.incoming` / `.staging-*`），"它算不算一个工具/版本"
/// 是语义归属，不是安全性。两件事分开才说得清"为什么这个合法名字被跳过"。
fn is_internal_name(name: &str) -> bool {
    name.starts_with('.')
}

/// **创建**一个工具/版本时用的完整校验：安全 + 不是内部产物位。
fn check_creatable(what: &'static str, value: &str) -> Result<(), StoreError> {
    check_component(what, value)?;
    if is_internal_name(value) {
        return Err(StoreError::UnsafeName {
            what,
            value: value.to_owned(),
            why: format!(
                "以 `.` 开头的名字是 store 的内部产物位（`root/.incoming`、\
                 `root/<tool>/versions/.staging-*`），不能拿来当一个{what} —— \
                 否则会出现装好了但列表里从来见不到的版本"
            ),
        });
    }
    Ok(())
}

/// **查询/删除**一个工具/版本时用的校验。
///
/// 不安全的、以及内部产物位的名字都回答"这里没有这样一个工具/版本"。
fn is_addressable(what: &'static str, value: &str) -> bool {
    check_component(what, value).is_ok() && !is_internal_name(value)
}

// ---------------------------------------------------------------------------
// 路径与磁盘事实
// ---------------------------------------------------------------------------

/// 这个位置现在是什么（**不跟随**重解析点 —— `FindFirstFileW` 读的是目录项本身）。
fn probe(path: &Path) -> FileFacts {
    RealFileSystem.inspect(path)
}

/// 严格意义上的"真实目录"：存在、是目录、**没有重解析位**。
///
/// 三个条件缺一不可：junction 指向目录时 `FILE_ATTRIBUTE_DIRECTORY` 也是置位的，
/// 只看 `is_dir` 会把它当成真目录 —— 而本层绝不能那样想。
fn is_real_dir(facts: &FileFacts) -> bool {
    facts.exists && facts.is_dir && facts.reparse == ReparseKind::None
}

/// 能 `canonicalize` 就用它（权威：它会解析 junction、8.3 短名与真实大小写），
/// 失败就退回原路径（被查的东西可能还没落盘）。
fn canonical_or_self(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// 两个路径是不是**同一个位置**。
///
/// 两边都先规范化，再统一转小写 —— NTFS 默认大小写不敏感，
/// 而 `current` 的重解析数据块里存的是**创建时**的拼写（决策 47）。
fn same_location(a: &Path, b: &Path) -> bool {
    comparable(a) == comparable(b)
}

/// 规范化的、可比较的字符串形式。
fn comparable(path: &Path) -> String {
    canonical_or_self(path).to_string_lossy().to_lowercase()
}

/// **词法**规范化：折叠 `.` 与 `..`、丢掉重复与结尾的分隔符，返回
/// `(盘符前缀, 普通组件)`，两边都转小写（NTFS 大小写不敏感）。
///
/// 它**不碰磁盘**，这是它与 `canonicalize` 的分工：守卫必须在"路径还不存在"
/// 时也成立（要删的东西当然可能已经不在），而 `canonicalize` 对不存在的路径直接失败。
///
/// `Path::components` 在 Windows 上把 `\` 与 `/` **都**当分隔符，所以"两种分隔符
/// 混用"在这里自动就是对的 —— 不需要额外归一再切一遍。
fn normalized_parts(path: &Path) -> (String, Vec<String>) {
    let mut prefix = String::new();
    let mut parts: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix_component) => {
                prefix = prefix_component
                    .as_os_str()
                    .to_string_lossy()
                    .to_lowercase();
            }
            // 盘根：`C:\` 里的 `\`。前缀里已经带了盘符，这里不再单独记。
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                // `..` 就地折叠。折叠不到（相对路径开头就是 `..`）时丢掉它 ——
                // 那种路径本来就不可能落在 store 里，结果一样是被拒。
                parts.pop();
            }
            Component::Normal(part) => parts.push(part.to_string_lossy().to_lowercase()),
        }
    }
    (prefix, parts)
}

/// 断言 `candidate` 在 `store_root` **底下**，否则拒绝。
///
/// 这是防"路径拼错了把用户目录删了"的那道守卫：删除之前先问一句
/// "这个路径真的在 store 里吗"。
///
/// # 判据：折叠 + **逐组件**
///
/// ①两边先做词法折叠（`.` / `..` / 重复分隔符 / `\` 与 `/` 混用），
/// ②再按**路径组件**逐个比较（大小写不敏感），
/// ③`candidate` 必须比 `store_root` **深至少一层**：store 根自己**不算通过**
/// —— `uninstall` 永远删的是 `versions/<version>`，没有一条正当路径要去删 store 根。
///
/// **为什么必须是逐组件，而不是字符串前缀**：这道守卫防的正是"前缀相同的邻居目录"。
/// `C:\a\b` 与 `C:\a\bc` 是真实存在的邻居，字符串前缀会把后者放行 ——
/// 那不是宽容，那恰恰是它本该拦下的那一类事故（拼路径时少写一层目录，
/// 或者两个目录名前缀相同）。同理，纯组件式 `starts_with` 也不行：它会**放行**
/// `C:\a\b\..\..\x`（它的前几段正好等于根），所以折叠必须在比较**之前**做。
///
/// # Errors
///
/// `candidate` 不在 `store_root` 底下 → [`StoreError::Refused`]。
pub(crate) fn ensure_inside_store(store_root: &Path, candidate: &Path) -> Result<(), StoreError> {
    let (root_prefix, root_parts) = normalized_parts(store_root);
    let (target_prefix, target_parts) = normalized_parts(candidate);
    let inside = root_prefix == target_prefix
        && target_parts.len() > root_parts.len()
        && root_parts
            .iter()
            .zip(&target_parts)
            .all(|(root, target)| root == target);
    if inside {
        return Ok(());
    }
    Err(StoreError::Refused {
        path: candidate.to_path_buf(),
        why: format!(
            "它不在 store 里（store 根：{}）。**拒绝删除** —— \
             路径一旦拼错，删的就是用户自己的目录。",
            store_root.display()
        ),
    })
}

/// 递归统计一个目录里的**普通文件**数与字节数。
///
/// 三条约定，每条都有理由：
///
/// 1. **根目录自己不算一个条目**（否则一个空载荷会报"1 个文件"）；
/// 2. **不跟随链接**：junction / symlink 既不计数也不递归进去 —— 跟随会引入环与
///    重复计数，而且载荷里本来就不该有重解析点（`tuoen-archive` 的解压后审计
///    正是为了这件事）；
/// 3. 读不出来的条目**跳过而不是报错**：这个数字只进记录，目录才是事实来源
///    （与 `tuoen_platform::FileSystem::list_dir` 的既有约定一致）。
fn measure_dir(root: &Path) -> (u64, u64) {
    let mut files = 0_u64;
    let mut bytes = 0_u64;
    let mut todo = vec![root.to_path_buf()];
    while let Some(dir) = todo.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                todo.push(entry.path());
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            files += 1;
            bytes += entry.metadata().map(|meta| meta.len()).unwrap_or(0);
        }
    }
    (files, bytes)
}

/// 读一份记录。**缺失与损坏都回答 `None`** —— 见模块文档 ①。
fn read_record(path: &Path) -> Option<InstallRecord> {
    let text = std::fs::read_to_string(path).ok()?;
    InstallRecord::from_json(&text).ok()
}

/// 一个 `versions/` 条目算不算"一个已安装的版本"。
///
/// 三道判据缺一不可：
///
/// 1. **真实目录** —— 普通文件（`foo.json`）与任何链接都不算；
/// 2. 名字是[安全组件](check_component)；
/// 3. 名字**不是内部产物位**（不以 `.` 开头）。
fn is_version_entry(name: &str, kind: &std::fs::FileType) -> bool {
    kind.is_dir() && !is_internal_name(name) && check_component(WHAT_VERSION, name).is_ok()
}

/// 这个工具的 `versions/` 里至少有一个版本目录。
fn has_a_version(store: &Store, tool: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(store.versions_dir(tool)) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let Ok(kind) = entry.file_type() else {
            return false;
        };
        let Ok(name) = entry.file_name().into_string() else {
            return false;
        };
        is_version_entry(&name, &kind)
    })
}

/// 把一个已经确认"真实目录"的版本目录读成一条 [`InstalledVersion`]。
fn snapshot(
    name: &str,
    path: &Path,
    store: &Store,
    tool: &str,
    current: Option<&Path>,
) -> InstalledVersion {
    let (files, bytes) = measure_dir(path);
    InstalledVersion {
        version: name.to_owned(),
        path: path.to_path_buf(),
        active: current.is_some_and(|target| same_location(target, path)),
        record: read_record(&store.record_path(tool, name)),
        files,
        bytes,
    }
}

// ---------------------------------------------------------------------------
// 查询
// ---------------------------------------------------------------------------

/// 列出一个工具已安装的版本。**目录是事实来源**，记录只是附加信息。
///
/// 顺序：**版本号降序**（人类习惯"最新在最上"），用的是 [`compare_versions`]
/// —— 不是字符串排序，否则 `24.9.0` 会排在 `24.21.0` 上面。
///
/// # 哪些条目会被跳过
///
/// * **非目录**（`foo.json`、误放进来的文件）；
/// * **任何链接**（junction / symlink）—— 它们不是我们装的载荷；
/// * **名字不安全的目录**（结尾带点、保留设备名……）—— 见
///   [`check_component`]；跳过而不是 panic，因为磁盘上什么都可能有；
/// * **名字不是合法 UTF-8 的目录** —— 用 `to_string_lossy` 会得到一个
///   "看起来合法但指向别处"的名字（替换字符 U+FFFD），那比跳过危险得多；
///
/// # 跨 crate 契约：`versions/` 里的 `.staging-*`
///
/// `tuoen-archive` 的 `install_archive` 把临时解压目录建在
/// **`<dest_root>/.staging-<label>-<n>`**，而票 #6 的安装路径正是
/// `dest_root = <root>/<tool>/versions`（它靠一次 `rename` 落位，那是原子性的来源）。
/// 所以正常情况下 `versions/` 里**就会出现 `.staging-*` 目录** —— 它们是**内部产物**，
/// 绝不能当成一个版本号列出来。判据是"以 `.` 开头"，
/// 与 [`installed_tools`] 对 `root/` 下 `.incoming` 的判据是同一条。
#[must_use]
pub fn installed_versions(store: &Store, tool: &str) -> Vec<InstalledVersion> {
    if !is_addressable(WHAT_TOOL, tool) {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(store.versions_dir(tool)) else {
        return Vec::new();
    };
    // `current` 只读一次：每个版本目录都要与它比一次（见 [`snapshot`]）。
    let current = junction_target(&store.current_link(tool)).ok().flatten();
    let mut installed: Vec<InstalledVersion> = entries
        .flatten()
        .filter_map(|entry| {
            let kind = entry.file_type().ok()?;
            let name = entry.file_name().into_string().ok()?;
            if !is_version_entry(&name, &kind) {
                return None;
            }
            Some(snapshot(
                &name,
                &entry.path(),
                store,
                tool,
                current.as_deref(),
            ))
        })
        .collect();
    installed.sort_by(|a, b| compare_versions(&b.version, &a.version));
    installed
}

/// 单个版本。没装返回 `None`。
///
/// 报告里的 `version` 是**你给的那个拼写** —— NTFS 大小写不敏感，
/// 所以 `24.19.0` 与 `24.19.0` 指向同一个目录，而磁盘上的那一段写法可能不同。
#[must_use]
pub fn find_version(store: &Store, tool: &str, version: &str) -> Option<InstalledVersion> {
    if !is_addressable(WHAT_TOOL, tool) || !is_addressable(WHAT_VERSION, version) {
        return None;
    }
    let path = store.version_dir(tool, version);
    if !is_real_dir(&probe(&path)) {
        return None;
    }
    let current = junction_target(&store.current_link(tool)).ok().flatten();
    Some(snapshot(version, &path, store, tool, current.as_deref()))
}

/// `current` 现在指向哪个版本。没有 `current` 或它坏了 → `None`。
///
/// 三件事都算"它坏了"，而且都返回 `None` 而不是硬报一个名字：
///
/// 1. 目标**不落在** `root/<tool>/versions/` 里（`current` 指到了 store 之外 ——
///    那不是我们认的激活）；
/// 2. 目标那一段不是一个[可寻址的](is_addressable)版本名；
/// 3. 那个版本目录**已经不在**了（悬空的链接不算"激活了一个版本"）。
#[must_use]
pub fn active_version(store: &Store, tool: &str) -> Option<String> {
    if !is_addressable(WHAT_TOOL, tool) {
        return None;
    }
    let target = junction_target(&store.current_link(tool)).ok().flatten()?;
    if !same_location(target.parent()?, &store.versions_dir(tool)) {
        return None;
    }
    let name = target.file_name()?.to_str()?;
    if !is_addressable(WHAT_VERSION, name) {
        return None;
    }
    if !is_real_dir(&probe(&store.version_dir(tool, name))) {
        return None;
    }
    Some(name.to_owned())
}

/// 存储里**有版本目录**的全部工具 id，**升序**（稳定输出）。
///
/// * 只认**真实目录**：根下的散文件不算工具；
/// * `root/` 下以 `.` 开头的（比如 `.incoming`，`tuoen install` 的临时解压区）
///   **不算工具**；
/// * 一个工具目录存在但 `versions/` 空/不存在，**也不算**。
///
/// 它和 [`store_is_empty`] 是**同一个实现**（后者就是前者的 `is_empty()`）：
/// 两处判据不同的后果是"`list` 说有一个工具、而 `store_is_empty` 说存储是空的"，
/// 那种输出没人能解释。
#[must_use]
pub fn installed_tools(store: &Store) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(store.root()) else {
        return Vec::new();
    };
    let mut tools: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !kind.is_dir() || !is_addressable(WHAT_TOOL, &name) {
            continue;
        }
        if has_a_version(store, &name) {
            tools.push(name);
        }
    }
    tools.sort();
    tools
}

/// 这个 store 里一个工具都没有。
#[must_use]
pub fn store_is_empty(store: &Store) -> bool {
    installed_tools(store).is_empty()
}

// ---------------------------------------------------------------------------
// 写入
// ---------------------------------------------------------------------------

/// 把一个**已经解压好**的载荷目录搬进 store。
///
/// # 它做什么
///
/// 1. 校验工具名/版本名（不安全的名字在这里就被拒，绝不碰磁盘）；
/// 2. 载荷必须是一个**真实目录**（普通文件、junction、symlink 都不行）；
/// 3. 在 store 里建出父目录（`versions/`），然后**一次 `rename`** 把载荷搬到
///    `versions/<version>` —— 同卷上是原子的：要么完整落位，要么根本没出现过；
/// 4. 数一遍落盘结果，**覆盖** `record.payload_files` / `payload_bytes`，
///    顺手把 `record.tool` / `record.version` 拧成参数值（记录是随载荷走的，
///    路径与内容必须一致），再写 `versions/<version>.json`；
/// 5. 返回落位后的路径与统计。
///
/// # 两条契约
///
/// * **目标已存在 → [`StoreError::AlreadyInstalled`]**，而且**两边都不动**：
///   已经在那儿的那份可能是能用的（覆盖它等于在用户没同意时换掉他的工具链），
///   你交来的载荷也**不删**（它是你的，不是我们的 —— 由调用方清理）。
///   目标位置上是一个**重解析点**时优先报 [`StoreError::Refused`]：
///   往一个链接上搬载荷比"已经装过"严重得多。
/// * **记录写失败不回滚载荷**：那时载荷已经就位、可激活、可卸载，
///   [`installed_versions`] 仍然看得见它（`record: None`）。记录只是附注 ——
///   为了它把一份已经装好的工具链删掉，是拿事实去凑附注。
///
/// # 跨卷会失败，这是刻意的
///
/// `rename` 不能跨卷（`ERROR_NOT_SAME_DEVICE`）。替代方案是"复制 + 删除"，
/// 而它既不原子、又会在失败时留下**半个版本目录**。所以调用方应当把载荷
/// 解压到 store 同卷的位置（`tuoen-archive` 的 `install_archive` 正是把临时解压
/// 目录建在 `versions/` 里，就是为了这一点）。
///
/// # Errors
///
/// 名字不安全、载荷不存在或不是真实目录、目标已存在或是个链接、I/O 失败、
/// 记录写不下去。
pub fn adopt_payload(
    store: &Store,
    tool: &str,
    version: &str,
    payload: &Path,
    record: &mut InstallRecord,
) -> Result<AdoptOutcome, StoreError> {
    check_creatable(WHAT_TOOL, tool)?;
    check_creatable(WHAT_VERSION, version)?;

    // ① 载荷：必须存在，而且必须是一个**真实目录**。
    let facts = probe(payload);
    if !facts.exists {
        return Err(StoreError::PayloadMissing {
            path: payload.to_path_buf(),
        });
    }
    if !is_real_dir(&facts) {
        return Err(StoreError::Refused {
            path: payload.to_path_buf(),
            why: "载荷必须是一个**真实目录**（解压好的产物）。普通文件、junction、\
                  symlink 都不行 —— 链接会让 store 里出现一个指向别处的版本目录，\
                  而卸载时就分不清"
                .to_owned(),
        });
    }

    // ② 目标位置：什么都不许有。链接优先报"拒绝"（见函数文档）。
    let target = store.version_dir(tool, version);
    let existing = probe(&target);
    if existing.exists {
        if existing.reparse != ReparseKind::None {
            return Err(StoreError::Refused {
                path: target,
                why: "目标位置已经是一个重解析点（junction / symlink）。**绝不往链接上搬载荷**\
                      —— 那等于把别人目录里的东西当成我们的版本，而卸载时会删到别处去。"
                    .to_owned(),
            });
        }
        return Err(StoreError::AlreadyInstalled {
            tool: tool.to_owned(),
            version: version.to_owned(),
            path: target,
        });
    }

    // ③ 落位：一次 `rename`。父目录先建出来（`rename` 不会造父目录）。
    let versions_dir = store.versions_dir(tool);
    std::fs::create_dir_all(&versions_dir).map_err(|source| StoreError::Io {
        path: versions_dir,
        source,
    })?;
    std::fs::rename(payload, &target).map_err(|source| StoreError::Io {
        path: target.clone(),
        source,
    })?;

    // ④ 记录：数字按**落盘后**的真实统计写（调用方填的值不算数）。
    let (files, bytes) = measure_dir(&target);
    record.tool = tool.to_owned();
    record.version = version.to_owned();
    record.payload_files = files;
    record.payload_bytes = bytes;
    write_record(store, record)?;

    Ok(AdoptOutcome {
        path: target,
        files,
        bytes,
    })
}

/// 单独写一份记录（给"记录坏了想修"用）。
///
/// 记录写在 `root/<tool>/versions/<version>.json` —— **版本目录旁边**。
/// 只要记录里的 `tool` / `version` 与落盘的目录一致，[`installed_versions`]
/// 就会把它认成那个版本的来源信息。
///
/// **schema 版本必须等于 [`crate::RECORD_SCHEMA_VERSION`]**：我们拒绝写一份自己
/// 读不懂的记录。只在校验一处（只读、或只写）的后果是"写出去、下次自己读不回来"，
/// 而那条失败链在用户眼里是"我装好了，但 `list` 说这份记录不认"。
///
/// # Errors
///
/// 名字不安全、schema 版本不认识、I/O 失败。
pub fn write_record(store: &Store, record: &InstallRecord) -> Result<(), StoreError> {
    check_creatable(WHAT_TOOL, &record.tool)?;
    check_creatable(WHAT_VERSION, &record.version)?;

    let path = store.record_path(&record.tool, &record.version);
    if record.schema_version != crate::RECORD_SCHEMA_VERSION {
        return Err(StoreError::RecordBroken {
            path,
            reason: format!(
                "要写的记录声明 schema v{}，本版本只认 v{} —— 拒绝写一份自己读不懂的记录",
                record.schema_version,
                crate::RECORD_SCHEMA_VERSION
            ),
        });
    }

    let versions_dir = store.versions_dir(&record.tool);
    std::fs::create_dir_all(&versions_dir).map_err(|source| StoreError::Io {
        path: versions_dir,
        source,
    })?;
    std::fs::write(&path, record.to_json()).map_err(|source| StoreError::Io { path, source })
}

/// 翻转 `current`。版本没装 → [`StoreError::NotInstalled`]。
///
/// 返回 [`Repoint`]，于是"这是一次**原子**翻转"变成可断言的事：
/// 第一次是 `Repoint::Created`，之后每一次都必须是 `Repoint::Replaced`
/// （同标签的重解析点被**就地替换**，见决策 46）—— 如果是 `Degraded`，
/// 那条路径就有窗口，调用方应当知道。
///
/// # Errors
///
/// 名字不安全、版本没装、那个位置上不是真实目录、平台调用失败。
pub fn activate(store: &Store, tool: &str, version: &str) -> Result<Repoint, StoreError> {
    check_component(WHAT_TOOL, tool)?;
    check_component(WHAT_VERSION, version)?;
    let not_installed = || StoreError::NotInstalled {
        tool: tool.to_owned(),
        version: version.to_owned(),
    };
    if is_internal_name(version) {
        return Err(not_installed());
    }

    let dir = store.version_dir(tool, version);
    let facts = probe(&dir);
    // 不存在、或者那个位置上是一个**文件**：都不构成一个已安装的版本。
    if !facts.exists || !facts.is_dir {
        return Err(not_installed());
    }
    // 而"是目录形状的链接"是另一回事：那是别人放在那里的东西，拒绝（与 `uninstall` 同一条规则）。
    if facts.reparse != ReparseKind::None {
        return Err(StoreError::Refused {
            path: dir,
            why: "那个位置上是一个重解析点（junction / symlink）。\
                  激活只指向我们自己的载荷 —— 指向别处会让 `current` 背着用户\
                  跑到一个我们不知道的目录上。"
                .to_owned(),
        });
    }

    Ok(repoint_junction(&store.current_link(tool), &dir)?)
}

/// 摘掉 `current`（如果有）。返回是否真的摘了。
///
/// **只摘链接本身，永远不碰目标** —— 这是关键的一半：版本目录里的东西一件都不会少。
/// `current` 是一个**真实目录**（用户自己建的、或者上一次安装留下的）时
/// **拒绝**并报 [`StoreError::Refused`]：把它当链接删掉就等于递归删掉里面的东西。
///
/// # Errors
///
/// 名字不安全、`current` 不是链接、平台调用失败。
pub fn deactivate(store: &Store, tool: &str) -> Result<bool, StoreError> {
    check_component(WHAT_TOOL, tool)?;
    let link = store.current_link(tool);
    let facts = probe(&link);
    if !facts.exists {
        return Ok(false);
    }
    if !facts.reparse.is_link() {
        return Err(StoreError::Refused {
            path: link,
            why: "`current` 存在但不是 junction/symlink（是真实目录、普通文件或别的重解析点）。\
                  摘链接的操作**不会**递归删一个真实目录 —— 那里面可能是你的数据，\
                  请你自己确认后处理。"
                .to_owned(),
        });
    }
    remove_junction(&link)?;
    Ok(true)
}

/// 删一个版本。
///
/// # 顺序是刻意的
///
/// * 被 `current` 指着且 `force_active == false` → [`StoreError::ActiveVersion`]，
///   **载荷一个字节都不动**（直接删会让 `current` 悬空，症状是"目录在、打不开"）；
/// * `force_active == true` → **先摘 `current`，再删载荷**。这个顺序不是细节：
///   反过来会留下一个指向"马上要被删掉的目录"的链接，而那个窗口里每一次访问
///   都失败。代价是"摘了链接但删载荷失败"会留下一个不再激活的版本 ——
///   那比一个悬空的 `current` 好得多，而且看得出来。
///
/// # 绝不穿透链接
///
/// 版本位置上是一个重解析点时**拒绝**（[`StoreError::Refused`]）：
/// `remove_dir_all` 顺着链接走会删掉链接**目标**里的东西，那可能是任何地方的
/// 任何文件。删之前还会再问一句"这个路径真的在 store 里吗"
/// （[`ensure_inside_store`]）。
///
/// # Errors
///
/// 名字不安全、版本没装、正被激活且没给 `force_active`、版本位置是重解析点、
/// 路径不在 store 里、I/O 失败。
pub fn uninstall(
    store: &Store,
    tool: &str,
    version: &str,
    options: UninstallOptions,
) -> Result<UninstallOutcome, StoreError> {
    check_component(WHAT_TOOL, tool)?;
    check_component(WHAT_VERSION, version)?;
    let not_installed = || StoreError::NotInstalled {
        tool: tool.to_owned(),
        version: version.to_owned(),
    };
    if is_internal_name(version) {
        // 内部产物位（`.staging-*`）不是一个版本，而且它可能正被另一个进程在写。
        return Err(not_installed());
    }

    let dir = store.version_dir(tool, version);
    ensure_inside_store(store.root(), &dir)?;

    let facts = probe(&dir);
    if !facts.exists || !facts.is_dir {
        return Err(not_installed());
    }
    if facts.reparse != ReparseKind::None {
        return Err(StoreError::Refused {
            path: dir,
            why: "版本目录是一个重解析点（junction / symlink）。**绝不递归穿过链接删除**\
                  —— 顺着链接走删掉的是链接**目标**里的东西，那可能是任何地方的任何文件。\
                  请你自己确认后手动处理。"
                .to_owned(),
        });
    }

    // 是不是当前激活的那个：比的是**路径**，不是名字（大小写、8.3 短名、verbatim 前缀
    // 都可能不同，而它们指的是同一个位置）。
    let was_active = match junction_target(&store.current_link(tool)) {
        Ok(Some(target)) => same_location(&target, &dir),
        _ => false,
    };
    if was_active && !options.force_active {
        return Err(StoreError::ActiveVersion {
            tool: tool.to_owned(),
            version: version.to_owned(),
            hint: "先 `tuoen use` 到别的版本，或加 `--force`（它会先摘掉 `current` 再删这份载荷）"
                .to_owned(),
        });
    }
    if was_active {
        deactivate(store, tool)?;
    }

    // 统计要在删之前做 —— 删完就数不出来了。
    let (freed_files, freed_bytes) = measure_dir(&dir);
    std::fs::remove_dir_all(&dir).map_err(|source| StoreError::Io {
        path: dir.clone(),
        source,
    })?;

    // 记录跟着载荷走：载荷没了，附注也不该留着（否则它会变成一份悬空记录）。
    // `NotFound` 当成功：没有记录是**正常**状态（见模块文档 ①）。
    let record_path = store.record_path(tool, version);
    match std::fs::remove_file(&record_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(StoreError::Io {
                path: record_path,
                source,
            });
        }
    }

    Ok(UninstallOutcome {
        was_active,
        freed_files,
        freed_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_compared_segment_by_segment_not_as_strings() {
        // 这张表就是 [`compare_versions`] 的规格。**没有一处用字符串排序**
        // —— 那样 `24.9.0` 会大于 `24.21.0`（`'9' > '2'`），而这个陷阱
        // 恰恰是版本排序里最常见的那种。
        let cases = [
            // 数字段按**数值**比：9 < 21，尽管 `"9" > "2"`。
            ("24.9.0", "24.21.0", Ordering::Less),
            // 多段 + `+` 都是分隔符：11 < 12。
            ("21.0.11+10", "21.0.12.1+1", Ordering::Less),
            // `+build` 比没有 build 的**更具体** → 更大。
            ("21.0.12.1", "21.0.12.1+1", Ordering::Less),
            ("21.0.12.1+1", "21.0.12.1+2", Ordering::Less),
            // 段数不同：段少的更小。
            ("24.19", "24.19.0", Ordering::Less),
            // 完全相等（同一个串）—— 包括带 `+` 的那种。
            ("24.19.0", "24.19.0", Ordering::Equal),
            ("21.0.12.1+1", "21.0.12.1+1", Ordering::Equal),
            // 数值相等但拼写不同的段：先用原串裁决（否则顺序会依赖枚举顺序）。
            ("1.01", "1.1", Ordering::Less),
            // 非数字段按文本。
            ("24.19.0-rc1", "24.19.0", Ordering::Greater),
        ];
        for (a, b, expected) in cases {
            assert_eq!(compare_versions(a, b), expected, "{a} vs {b}");
            // 反向必须对称 —— 排序要的是一个全序。
            assert_eq!(
                compare_versions(b, a),
                expected.reverse(),
                "{b} vs {a}（反向）"
            );
        }
        // 数值段长到 `u64` 装不下也不能出错（这就是不解析成整数的理由）。
        assert_eq!(
            compare_versions(
                "202601011200000000000000000001",
                "202601011200000000000000000002"
            ),
            Ordering::Less
        );
    }

    #[test]
    fn the_store_guard_rejects_paths_that_leave_the_store() {
        // 守卫的四个判据（票 #6 的验收里点名的）。
        let root = Path::new(r"C:\a\b");
        assert!(
            ensure_inside_store(root, Path::new(r"C:\a\b\c")).is_ok(),
            "store 底下的路径要放行"
        );
        assert!(
            ensure_inside_store(root, Path::new(r"C:\a\b\c\d")).is_ok(),
            "更深一层也是 store 底下"
        );
        for outside in [
            // **前缀相同的邻居**：字符串前缀会把它放行，而它正是这道守卫要拦的那一类。
            r"C:\a\bc",
            r"C:\a",
            // store 根自己：`uninstall` 永远删的是 `versions/<version>`，
            // 没有一条正当路径要去删 store 根。
            r"C:\a\b",
            // 折成 `C:\x` —— 纯组件式 `starts_with` 会放行它，所以折叠必须在比较之前。
            r"C:\a\b\..\..\x",
            r"C:\Users\me",
            r"..\x",
        ] {
            let error = ensure_inside_store(root, Path::new(outside))
                .expect_err("store 之外的路径必须被拒");
            assert_eq!(error.kind(), "refused");
            assert!(
                error.to_string().contains(outside),
                "消息里要有那个路径：{error}"
            );
        }
    }

    #[test]
    fn the_guard_folds_dot_segments_like_windows_does() {
        // 折叠是这条守卫的一半技术含量：纯组件式比较会**放行** `C:\a\b\..\..\x`
        // （它的前四段正好是根），而那是一个逃逸。
        let (prefix, parts) = normalized_parts(Path::new(r"C:\a\b\..\..\x"));
        assert_eq!(prefix, "c:");
        assert_eq!(parts, ["x"]);
        let (_, parts) = normalized_parts(Path::new(r"C:\a\b\.\c\"));
        assert_eq!(parts, ["a", "b", "c"], "`.` 与重复分隔符都要丢掉");
        let (_, parts) = normalized_parts(Path::new(r"C:\A\B"));
        assert_eq!(parts, ["a", "b"], "NTFS 大小写不敏感");
        let (_, parts) = normalized_parts(Path::new(r"C:/a\b"));
        assert_eq!(parts, ["a", "b"], "`\\` 与 `/` 混用要当成同一件事");
        assert!(
            ensure_inside_store(Path::new(r"C:\a\b"), Path::new(r"C:\a\b\c\..\d")).is_ok(),
            "在 store 里绕一圈再回来仍然在 store 里"
        );
        assert!(
            ensure_inside_store(Path::new(r"C:\a\b"), Path::new(r"C:\A\B\c")).is_ok(),
            "盘符与目录名的大小写差异不该让它出去"
        );
    }

    #[test]
    fn internal_names_are_skipped_not_treated_as_versions() {
        // `.` 开头的名字是内部产物位：**查询**它们等于"没有"。
        assert!(is_internal_name(".staging-node-1"));
        assert!(is_internal_name(".incoming"));
        assert!(!is_internal_name("24.19.0"));
        assert!(!is_addressable(WHAT_VERSION, ".staging-node-1"));
        // 不安全的名字同样不可寻址（但理由不同：一个是语义，一个是安全性）。
        assert!(!is_addressable(WHAT_VERSION, ".."));
        assert!(!is_addressable(WHAT_TOOL, "a/b"));
        assert!(is_addressable(WHAT_VERSION, "21.0.12.1+1"));
    }

    #[test]
    fn creating_an_internal_name_is_refused_with_its_own_reason() {
        // **创建**内部产物位是另一回事：那会造出一个列表里永远看不见的版本。
        let error = check_creatable(WHAT_VERSION, ".staging-node-1")
            .expect_err("内部产物位不能被创建成一个版本");
        assert_eq!(error.kind(), "unsafe-name");
        let text = error.to_string();
        assert!(text.contains("内部产物位"), "{text}");
        assert!(text.contains(".staging-node-1"), "{text}");

        // 不安全的名字仍然先走安全性那条理由。
        let error = check_creatable(WHAT_VERSION, "..").expect_err("`..` 必须被拒");
        assert!(error.to_string().contains("路径语法"), "{error}");

        // 正常名字照过。
        assert!(check_creatable(WHAT_TOOL, "node").is_ok());
        assert!(check_creatable(WHAT_VERSION, "21.0.12.1+1").is_ok());
    }
}
