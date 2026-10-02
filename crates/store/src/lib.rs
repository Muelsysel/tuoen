//! `tuoen` 的安装存储层：磁盘上那棵树。
//!
//! # 布局
//!
//! ```text
//! <root>/<tool>/versions/<version>/      ← 载荷（解压好的产物，**事实来源**）
//! <root>/<tool>/versions/<version>.json  ← 记录（谁装的、从哪来的，**附注**）
//! <root>/<tool>/current                  ← junction，指向某个版本目录
//! ```
//!
//! `<root>` 默认是 `%LOCALAPPDATA%\tuoen\store`（见 [`Store::at_default_location`]）。
//!
//! # 三条贯穿全 crate 的契约
//!
//! 这三条不是风格，它们各自决定了一批函数的签名与错误变体。
//!
//! ## ① 目录是事实来源，记录只是附注
//!
//! "装了什么"由**磁盘上的目录**回答，不由 `<version>.json` 回答。所以
//! [`installed_versions`] 看见一个目录就是一个版本，**哪怕它的记录丢了、写坏了、
//! 或者根本没写过**（那时 `record: None`）。反过来说：一个没有目录的记录**不是**
//! 一个版本。这条的理由很实际 —— 记录与目录是两次写入，中间必然有窗口，
//! 而"以记录为准"意味着那个窗口里 `list` 会撒谎。
//!
//! ## ② `current` 是 junction，**永远不是** symlink
//!
//! 见 `docs/DESIGN.md` 决策 46/47 与 ADR-0001：未提权 + Developer Mode 关闭时
//! symlink **创建不出来**，而 junction 可以；两者的重解析标签不同，所以
//! "就地替换"只对 junction 成立。翻转 `current` 因此是**一次 IOCTL**
//! （[`activate`] 返回的 [`tuoen_platform::Repoint`] 把这件事变得可断言：
//! 第一次 `Created`、第二次 `Replaced`）。
//!
//! ## ③ 绝不跟随、绝不穿透重解析点
//!
//! 载荷目录位置上出现 junction 或 symlink 时，本层**一律拒绝**
//! （[`StoreError::Refused`]），而不是"小心地递归"。理由是本层最不能出的
//! 事故就是"删错了地方"：`remove_dir_all` 顺着链接走会删掉链接**目标**里的
//! 东西，那可能是任何地方的任何文件。拒绝的代价是用户要自己看一眼，
//! 而猜错的代价是不可逆的。
//!
//! # 与其它 crate 的边界
//!
//! * `tuoen-archive` 负责"把归档安全地展开成**一个目录**"，本层负责"把这个目录
//!   搬进 store 并让它可选、可激活、可卸载"。搬运用 `rename`（同卷原子）；
//!   **跨卷会失败**，这是刻意的 —— 见 [`adopt_payload`]。
//! * `tuoen-platform` 提供 junction 原语与文件系统事实（`RealFileSystem`），
//!   本层不直接调 Win32。

pub mod error;
pub mod layout;
pub mod ops;
pub mod record;

pub use error::StoreError;
pub use layout::Store;
pub use ops::{
    AdoptOutcome, InstalledVersion, UninstallOptions, UninstallOutcome, activate, active_version,
    adopt_payload, compare_versions, deactivate, find_version, installed_tools, installed_versions,
    store_is_empty, uninstall, write_record,
};
pub use record::{InstallRecord, RECORD_SCHEMA_VERSION, now_rfc3339};
