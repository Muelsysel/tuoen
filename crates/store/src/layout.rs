//! store 的目录布局，以及**名字安全**这道闸。
//!
//! # 布局
//!
//! ```text
//! <root>/                                 ← 默认 `%LOCALAPPDATA%\tuoen\store`
//! <root>/<tool>/                          ← 一个工具
//! <root>/<tool>/versions/                 ← 这个工具的全部版本
//! <root>/<tool>/versions/<version>/       ← 载荷（解压好的产物）
//! <root>/<tool>/versions/<version>.json   ← 记录（附注，不是事实来源）
//! <root>/<tool>/current                   ← junction，指向某个版本目录
//! ```
//!
//! # 为什么路径推导**不做 I/O**、也**不返回 `Result`**
//!
//! 推一个路径是**纯函数**：`root` 加几段名字。它可能失败的原因只有一个 ——
//! 名字不安全 —— 而那件事**不需要碰磁盘就能判断**。所以这里没有一个方法会去
//! `create_dir`、`canonicalize` 或 `exists()`；它们全都只做字符串拼接。
//!
//! 顺带也就没有了"推导失败时返回什么"的问题：一个**推导不出来**的路径
//! （`void`、空串、或者一个**恰好指向用户目录**的巧合拼法）是本层最不该
//! 产生的东西 —— 它看起来像个路径，于是会被传给 `remove_dir_all`。
//! 所以这里的每个方法只做一件事：
//!
//! ```text
//! debug_assert!(check_component(…).is_ok(), "…");
//! ```
//!
//! **真正的拒绝发生在 [`crate::ops`] 的每个公开函数开头**（那里有 `?` 可以返回
//! [`StoreError::UnsafeName`]）。这里的断言只是让**测试**在有人绕过那条纪律时
//! 立刻炸出调用点 —— 在 release 里它被编译掉，因为一个 panic 比一条错误更难处理。
//!
//! # 名字安全为什么是第一道闸
//!
//! `%LOCALAPPDATA%\tuoen\store` 底下的名字有两个来源：清单里的工具 id / 版本号，
//! 以及命令行参数。两者都不能直接拼进路径。每一条规则都对应一个**实测过的**
//! 危险形态（`AGENTS.md`、`research/BSDTAR_SAFETY_MEASURED.md`）：
//!
//! | 拒什么 | 为什么 |
//! |---|---|
//! | `/` `\` | 这不是"名字"，是**路径**：`..\..\Users\me` 拼进去就出了 store |
//! | `.` `..` | 同上，它们连名字都不是 |
//! | 控制字符 | 会破坏列表与日志的行结构（`-tf` 的转义还是**双向**的） |
//! | `:` | NTFS 备用数据流（ADS）的写原语：`file.txt:evil` |
//! | 结尾的点或空格 | Win32 会**吃掉**结尾的点与空格 —— 于是这个目录在多数工具里打不开也删不掉。**实测：用 `\\?\` 前缀真的能造出这种目录**（本 crate 的集成测试就在造它） |
//! | 保留设备名 | `CON` / `PRN` / `AUX` / `NUL` / `COM1-9` / `LPT1-9`（**大小写不敏感，且 `CON.txt` 也算**）。**实测：bsdtar 用 `\\?\` 真的会创建它们，而创建出来的文件普通路径看不见** |
//!
//! ## 一条不在名单里、但同样重要的规则
//!
//! **以 `.` 开头的名字是 store 的内部产物位**（`root/.incoming`、
//! `root/<tool>/versions/.staging-*`）。它不是"不安全"（拼接上完全合法），
//! 所以它**不**在 [`check_component`] 里 —— 它是在 [`crate::ops`] 里作为
//! "这算不算一个工具/版本"的判据出现的。分成两处的理由：一个是路径安全性，
//! 一个是语义归属，混在一起会让"为什么这个合法名字被拒"变得说不清。

use std::path::{Path, PathBuf};

use crate::error::StoreError;

/// Windows 的保留设备名。**比较时只取基名**（第一个点之前的那一段），
/// 因为 `CON.txt` 与 `CON` 在 Windows 上指向同一个设备。
const RESERVED_DEVICE_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// 上标数字：`COM¹` 在 Windows 上**同样**解析成 `COM1`
/// （`crates/archive/src/name.rs` 里也是这么处理的，两层不能一严一宽）。
fn fold_superscript_digits(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '¹' => '1',
            '²' => '2',
            '³' => '3',
            other => other,
        })
        .collect()
}

/// 名字的基名是不是一个保留设备名。
///
/// **大小写不敏感**：`con` / `Con` / `CON` 在 Windows 上等价。
fn is_reserved_device_name(value: &str) -> bool {
    let folded = fold_superscript_digits(value);
    // 基名 = 第一个点之前的那一段（`CON.txt` 也算 `CON`）。
    let base = folded.split('.').next().unwrap_or(folded.as_str());
    RESERVED_DEVICE_NAMES
        .iter()
        .any(|reserved| base.eq_ignore_ascii_case(reserved))
}

/// 一个名字能不能当作**单层**路径组件用。
///
/// `what` 是给用户看的类别（`"工具名"` / `"版本号"`），进
/// [`StoreError::UnsafeName`]。
///
/// **注意它允许 `+` 与 `.`** —— Temurin 的真实版本串就是 `21.0.12.1+1`，
/// 把 `+` 当非法字符会让真制品装不进去。
///
/// # 为什么它是 `pub`（票据 #25）
///
/// `Store::version_dir` 对版本号那一段是 `debug_assert!` + 这条判据，而调用方
/// （`tuoen_core::globals::shims`）手里那个版本号来自**运行时自己的输出**
/// （`node -v` 削掉 `v`）—— 一个畸形值在 debug 构建里会让进程 panic。
/// 让它**先问这条判据、再拼路径**是那条 `debug_assert!` 存在的意义；
/// 各写一份判据则会漂移（漂移的表现是"产品 panic 了，而检查说没问题"）。
///
/// # Errors
///
/// 名字违反了上面那条表里的任意一条规则。
pub fn check_component(what: &'static str, value: &str) -> Result<(), StoreError> {
    let reject = |why: &str| {
        Err(StoreError::UnsafeName {
            what,
            value: value.to_owned(),
            why: why.to_owned(),
        })
    };

    if value.is_empty() {
        return reject("名字是空的");
    }
    if value == "." || value == ".." {
        return reject("`.` 与 `..` 不是名字，它们是路径语法");
    }
    if value.contains('/') || value.contains('\\') {
        return reject("名字里有路径分隔符 —— 这里要的是**单层**名字，不是路径");
    }
    if value.contains(char::is_control) {
        return reject("名字里有控制字符（会破坏列表与日志的行结构）");
    }
    if value.contains(':') {
        return reject("名字里有 `:` —— 那是 NTFS 备用数据流（ADS）的写原语");
    }
    if value.ends_with('.') || value.ends_with(' ') {
        return reject(
            "名字以点或空格结尾 —— Win32 会吃掉结尾的点与空格，\
             于是这个目录在多数工具里打不开也删不掉",
        );
    }
    if is_reserved_device_name(value) {
        return reject(
            "名字是 Windows 保留设备名（CON/PRN/AUX/NUL/COM1-9/LPT1-9，\
             基名比较、大小写不敏感，`CON.txt` 也算）—— 这类名字会被创建出来，\
             但普通路径看不见也删不掉",
        );
    }
    Ok(())
}

/// 存储层的根与其下的布局。
///
/// **路径推导故意不返回 `Result`** —— 见本模块头部文档：推导是纯函数，
/// 名字的拒绝在 [`crate::ops`] 里发生。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// 默认位置：`%LOCALAPPDATA%\tuoen\store`；读不到 `LOCALAPPDATA` 时退到
    /// `%USERPROFILE%\.tuoen\store`。
    ///
    /// 两个环境变量都读不到时退到当前目录下的 `.tuoen\store` —— 这在 Windows 上
    /// 几乎不会发生（`USERPROFILE` 总是有），但我们不 panic：一个读不到根位置的进程
    /// 仍然应该能**构造**出 store，把失败推迟到真正做 I/O 的时候。
    ///
    /// **它只拼字符串，绝不创建目录**：这是"看一眼默认位置在哪"与"在那里装东西"
    /// 的分界线（本 crate 的测试正是靠这条断言"默认位置从没被碰过"）。
    #[must_use]
    pub fn at_default_location() -> Store {
        let root = default_root().unwrap_or_else(|| PathBuf::from(".tuoen").join("store"));
        Store::new(root)
    }

    /// 指定根位置。**不做任何检查、不创建任何目录。**
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Store {
        Store { root: root.into() }
    }

    /// 根。
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// tuoen 的**家目录**：存储根的父目录（默认 `%LOCALAPPDATA%\tuoen`）。
    ///
    /// 存在的理由：`store/` 不是家目录下唯一的东西。shim 目录与它**并列**
    /// （`<home>/shims`），因为 `store/<tool>/versions/<version>` 是载荷，
    /// 而 shim 是面向用户的产物 —— 把 shim 塞进 `store/` 会让"清空存储"
    /// 顺手删掉用户 `PATH` 上的命令。
    ///
    /// 存储根没有父目录时退回存储根本身：`Store::new("store")` 这类相对根
    /// 也能算出一个确定的位置，而不是 panic 或给出空路径。
    #[must_use]
    pub fn home(&self) -> &Path {
        self.root.parent().unwrap_or(&self.root)
    }

    /// `root/<tool>`
    #[must_use]
    pub fn tool_dir(&self, tool: &str) -> PathBuf {
        debug_assert!(
            check_component("工具名", tool).is_ok(),
            "工具名没通过 check_component：{tool:?}（ops 里的每个公开函数都应当先拒它）"
        );
        self.root.join(tool)
    }

    /// `root/<tool>/versions`
    #[must_use]
    pub fn versions_dir(&self, tool: &str) -> PathBuf {
        self.tool_dir(tool).join("versions")
    }

    /// `root/<tool>/versions/<version>`
    #[must_use]
    pub fn version_dir(&self, tool: &str, version: &str) -> PathBuf {
        debug_assert!(
            check_component("版本号", version).is_ok(),
            "版本号没通过 check_component：{version:?}（ops 里的每个公开函数都应当先拒它）"
        );
        self.versions_dir(tool).join(version)
    }

    /// `root/<tool>/versions/<version>.json`
    ///
    /// 放在**版本目录旁边**而不是里面：记录不是载荷的一部分，
    /// 而"搬进 store"这个动作（一次 `rename`）只能连目录一起搬 —— 记录要在
    /// 落位之后才知道真实的文件数与字节数（见 [`crate::adopt_payload`]）。
    #[must_use]
    pub fn record_path(&self, tool: &str, version: &str) -> PathBuf {
        self.versions_dir(tool).join(format!("{version}.json"))
    }

    /// `root/<tool>/current` —— **junction**，指向当前激活的版本目录。
    #[must_use]
    pub fn current_link(&self, tool: &str) -> PathBuf {
        self.tool_dir(tool).join("current")
    }
}

/// 默认根位置。
fn default_root() -> Option<PathBuf> {
    // `LOCALAPPDATA` 优先：它是"本机、本用户的机器状态"，不是漫游数据 ——
    // 而一个装着几百 MB 工具链的目录**绝不该**跟着漫游配置文件走。
    for (variable, subdir) in [("LOCALAPPDATA", "tuoen"), ("USERPROFILE", ".tuoen")] {
        if let Some(value) = std::env::var_os(variable).filter(|value| !value.is_empty()) {
            return Some(PathBuf::from(value).join(subdir).join("store"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_names_are_accepted_including_real_world_ones() {
        // 每一条都对应一个**真的会出现**的名字。
        for (what, name) in [
            ("工具名", "node"),
            ("工具名", "temurin"),
            ("工具名", "oracle-jdk"),
            ("版本号", "24.19.0"),
            // Temurin 的真实版本串 —— `+` 必须是合法的，否则真制品装不进去。
            ("版本号", "21.0.12.1+1"),
            ("版本号", "21.0.11+10"),
            ("版本号", "1"),
            // 中间的空格没问题（`Program Files` 是同类现实）。
            ("版本号", "1.0 beta"),
            // 不在保留名单里：`COM10` 与 `COM` 都不是设备名。
            ("版本号", "COM10"),
            ("版本号", "CONS"),
            ("版本号", "console"),
            // 非 ASCII 合法：Windows 文件名是 UTF-16。
            ("工具名", "版本"),
        ] {
            assert!(
                check_component(what, name).is_ok(),
                "`{name}` 应当被接受：{:?}",
                check_component(what, name)
            );
        }
    }

    #[test]
    fn unsafe_names_are_rejected_each_with_its_own_reason() {
        let cases: [(&str, &str); 17] = [
            ("工具名", ""),         // 空
            ("工具名", "."),        // 路径语法
            ("工具名", ".."),       // 路径语法（`..` 造不出来的那个）
            ("版本号", "../x"),     // 分隔符 + `..`
            ("版本号", "a/b"),      // 正斜杠
            ("版本号", r"a\b"),     // 反斜杠
            ("版本号", "1.0\n"),    // 控制字符
            ("版本号", "1.0\t0"),   // 控制字符（中间）
            ("版本号", "1.0:evil"), // ADS 写原语
            ("版本号", "1.0."),     // 结尾的点
            ("版本号", "1.0 "),     // 结尾的空格
            ("版本号", "CON"),      // 保留设备名
            ("版本号", "con"),      // 大小写不敏感
            ("版本号", "Con.TXT"),  // 基名
            ("版本号", "NUL"),      // 保留设备名
            ("版本号", "LPT9"),     // 保留设备名（编号段）
            ("版本号", "COM¹"),     // 上标形式同样解析成设备名
        ];
        for (what, name) in cases {
            let error = check_component(what, name)
                .expect_err(&format!("`{name}` 必须被拒绝（what={what}）"));
            assert_eq!(error.kind(), "unsafe-name");
            let StoreError::UnsafeName {
                what: got,
                value,
                why,
            } = error
            else {
                panic!("必须是 UnsafeName 变体");
            };
            assert_eq!(got, what);
            assert_eq!(value, name, "被拒的名字要**原样**带回去");
            assert!(!why.is_empty(), "每一条拒绝都要说明为什么");
        }
    }

    #[test]
    fn a_line_break_is_its_own_reason_not_a_separator_complaint() {
        // `1.0\n` 同时命中"有控制字符"与"结尾不是点/空格"... 反过来 `1.0. \n` 之类
        // 的会同时命中多条。**检查顺序决定了报哪一条**，所以把它钉住：
        // 顺序是 空 → 点段 → 分隔符 → 控制字符 → `:` → 结尾 → 设备名，
        // 也就是"先说它根本不是名字，再说名字本身的形态"。
        let error = check_component("版本号", "1.0\n").expect_err("控制字符");
        let StoreError::UnsafeName { why, .. } = error else {
            panic!("必须是 UnsafeName");
        };
        assert!(why.contains("控制字符"), "{why}");
    }

    #[test]
    fn a_leading_dot_is_a_safe_path_component_but_a_reserved_position() {
        // 这一条**刻意**是"接受"：`.` 开头的名字在**路径安全**上完全合法
        // （它就是本层自己的 `.incoming` / `.staging-*`），
        // "它不算一个工具/版本"是 `ops` 里的语义判据，不是这里的安全性判据。
        // 两件事分开，才说得清"为什么这个合法名字被跳过"。
        assert!(check_component("版本号", ".staging-node-1").is_ok());
        assert!(check_component("工具名", ".incoming").is_ok());
    }

    #[test]
    fn the_reserved_list_is_the_documented_one() {
        // 名单本身要被钉住：漏一个 `NUL` 就是一个"删不掉的残骸"。
        assert_eq!(RESERVED_DEVICE_NAMES.len(), 22);
        for name in ["CON", "PRN", "AUX", "NUL"] {
            assert!(RESERVED_DEVICE_NAMES.contains(&name), "{name} 不在名单里");
        }
        for n in 1..=9 {
            assert!(RESERVED_DEVICE_NAMES.contains(&format!("COM{n}").as_str()));
            assert!(RESERVED_DEVICE_NAMES.contains(&format!("LPT{n}").as_str()));
        }
        // `COM0` / `COM10` 不是设备名 —— 别把名单写宽。
        assert!(!is_reserved_device_name("COM0"));
        assert!(!is_reserved_device_name("COM10"));
    }

    #[test]
    fn the_layout_is_root_tool_versions_version() {
        let store = Store::new(r"C:\root");
        let tool = PathBuf::from(r"C:\root").join("node");
        assert_eq!(store.root(), Path::new(r"C:\root"));
        assert_eq!(store.tool_dir("node"), tool);
        assert_eq!(store.versions_dir("node"), tool.join("versions"));
        assert_eq!(
            store.version_dir("node", "24.19.0"),
            tool.join("versions").join("24.19.0")
        );
        assert_eq!(
            store.record_path("node", "24.19.0"),
            tool.join("versions").join("24.19.0.json")
        );
        assert_eq!(store.current_link("node"), tool.join("current"));
    }

    #[test]
    fn the_default_location_is_under_tuoen_and_never_touched() {
        // 默认位置只拼字符串：这里断言它的形状，并且**绝不**在那里创建东西。
        // 测试跑在别人的真机上，往 `%LOCALAPPDATA%\tuoen\store` 写一个字节
        // 都是对用户机器的污染（本 crate 的其余测试全长在临时目录里）。
        let store = Store::at_default_location();
        let text = store.root().to_string_lossy().to_lowercase();
        assert!(text.contains("tuoen"), "默认位置里应当有 tuoen：{text}");
        assert!(text.ends_with("store"), "{text}");
        assert!(!store.root().as_os_str().is_empty());
    }
}
