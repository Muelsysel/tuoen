//! Junction（目录联接）的创建、读取、重指与删除。
//!
//! # 为什么是 junction 而不是 symlink
//!
//! ADR-0001 定的：**本机（未提权 + Developer Mode 关闭）无法创建 symlink**，
//! 文件与目录都失败（`mklink /D` 报 "You do not have sufficient privilege"，
//! `New-Item -ItemType SymbolicLink` 报 UnauthorizedAccessException）。
//! 而 **junction 可以** —— 因为 junction 是 `IO_REPARSE_TAG_MOUNT_POINT`
//! 的重解析点，创建它只需要对该目录的写权限，**不需要
//! `SeCreateSymbolicLinkPrivilege`**。
//!
//! `SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE`(0x2) **不是绕路方案**：
//! 文档明确说它只在 Developer Mode 已开启时有效（本机实测也确认了）。
//!
//! # 为什么不用 `std::os::windows::fs::symlink_dir`
//!
//! 因为那**就是** symlink，在未提权时会失败。Rust 标准库没有创建 junction 的 API。
//!
//! # 与 symlink 的语义差别（写在代码里，因为它是真的）
//!
//! | | junction | symlink |
//! |---|---|---|
//! | 目标 | **只能是本机绝对目录路径** | 文件或目录，可相对 |
//! | 需要权限 | 否 | `SeCreateSymbolicLinkPrivilege` 或 Developer Mode |
//! | 远程目标 | 不支持 | 支持 |
//! | 解析位置 | **内核**（对用户态完全透明） | 内核 |
//! | 删除 | `RemoveDirectory` / `std::fs::remove_dir` | 同上 |
//!
//! 这几条**不可互换** —— `GLOSSARY.md` 里专门写了一条。
//!
//! # 目标路径的两种形式（这一条不知道就会踩坑）
//!
//! 重解析点的数据块里要写**两个**名字：
//!
//! * **替代名**（substitute name）：内核用来解析的那个，格式是
//!   `\??\C:\目标路径` —— 注意 `\??\` 这个前缀，它指向**当前进程的
//!   DOS 设备命名空间**。少了它，junction 会指向一个字面量相对路径。
//! * **打印名**（print name）：给人看的那份，就是普通的 `C:\目标路径`。
//!   `dir` 与资源管理器显示的是它。
//!
//! 只写替代名的话资源管理器会显示一片空白；只写打印名则**根本不能用**。

use std::path::{Path, PathBuf};

use crate::error::PlatformError;

/// 一次重指的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Repoint {
    /// 原位置**没有**重解析点，已就地创建。
    Created,
    /// 原位置**有**同类型的重解析点，数据已就地替换。
    ///
    /// **这是原子翻转的关键**：`FSCTL_SET_REPARSE_POINT` 在标签相同时
    /// 是"替换数据"而不是"失败"，所以整个翻转是**一次 IOCTL**，
    /// 不存在"切到一半"的中间状态（没有先删再建的窗口）。
    Replaced,
    /// 用了降级手段（见 [`crate::junction::repoint_junction`]）。
    Degraded {
        /// 为什么降级（中文，给用户看）。
        reason: String,
    },
}

/// 把一个 Win32 码变成一个带路径的错误。
fn on(path: &Path, code: crate::sys::Win32Code) -> PlatformError {
    PlatformError::from_win32(code, path.display().to_string())
}

/// 就地创建或重指一个 junction。
///
/// 这是"`tuoen use` 翻转 `current`"的底层原语。
///
/// ## 顺序是刻意的
///
/// 1. 先试 **`FSCTL_SET_REPARSE_POINT` 就地替换**（如果原位置已经是
///    junction，这就是**一次原子操作**）；
/// 2. 原位置不存在 → 直接创建（`Created`）；
/// 3. 原位置存在但不是 junction（一个真目录、一个文件、或者一个 symlink）
///    → **拒绝**，不猜。那多半是用户的真实数据，删掉它是我们最不该做的事；
/// 4. 原位置是 symlink（Developer Mode 开着时可能出现的形态）→ 删掉它再建
///    junction。这一条**不是原子的**，所以我们明确报 `Degraded` 并把原因
///    写进结果，而不是假装它是原子的。
///
/// # Errors
///
/// 见 [`PlatformError`]。**任何失败都不会留下半个 junction** —— 要么原样，
/// 要么完整。
pub fn repoint_junction(link: &Path, target: &Path) -> Result<Repoint, PlatformError> {
    if !target.is_dir() {
        return Err(PlatformError::Win32 {
            code: crate::sys::ERROR_FILE_NOT_FOUND,
            path: format!(
                "junction 的目标必须是一个已存在的目录：{}",
                target.display()
            ),
        });
    }

    match crate::sys::junction_state(link) {
        crate::sys::JunctionState::Missing => {
            crate::sys::create_junction(link, target).map_err(|code| on(link, code))?;
            Ok(Repoint::Created)
        }
        crate::sys::JunctionState::Junction => {
            // **这一条是原子路径。** 标签相同，`FSCTL_SET_REPARSE_POINT`
            // 替换数据而不是失败。
            match crate::sys::set_junction_data(link, target) {
                Ok(()) => Ok(Repoint::Replaced),
                Err(code) => {
                    // 就地替换失败（少见：可能是别的重解析标签）。
                    // **不擅自降级到"删掉重建"** —— 那会引入一个窗口。
                    Err(on(link, code))
                }
            }
        }
        crate::sys::JunctionState::Symlink => {
            // symlink 是**另一种**重解析点，标签不同 → 就地替换一定失败
            // （`ERROR_REPARSE_TAG_MISMATCH`）。所以我们只能删掉再建，
            // 而**那有窗口**。明确报出来，不假装原子。
            crate::sys::remove_reparse_point(link).map_err(|code| on(link, code))?;
            crate::sys::create_junction(link, target).map_err(|code| on(link, code))?;
            Ok(Repoint::Degraded {
                reason: "原位置是一个 symlink（不是 junction），重解析标签不同，\
                         无法就地替换 —— 只能删掉再建，中间有一个极短的空窗。"
                    .to_owned(),
            })
        }
        crate::sys::JunctionState::File => Err(PlatformError::Win32 {
            code: crate::sys::ERROR_ALREADY_EXISTS,
            path: format!(
                "{} 是一个普通文件。**不覆盖** —— 那可能是你的数据。",
                link.display()
            ),
        }),
        crate::sys::JunctionState::RealDirectory => Err(PlatformError::Win32 {
            code: crate::sys::ERROR_ALREADY_EXISTS,
            path: format!(
                "{} 是一个**真实的**目录（不是 junction）。**不删除** —— \
                 删掉它等于删掉里面可能有的东西。请你自己确认后处理。",
                link.display()
            ),
        }),
        crate::sys::JunctionState::OtherReparse { tag } => Err(PlatformError::Win32 {
            code: crate::sys::ERROR_REPARSE_TAG_MISMATCH,
            path: format!(
                "{} 是另一种重解析点（tag 0x{tag:08x}），不是 junction。**不碰它**。",
                link.display()
            ),
        }),
    }
}

/// 读出一个 junction 现在指向哪。
///
/// 用 `std::fs::read_link`（Rust 在 Windows 上能正确读出 junction 的替代名）
/// 而不是自己调 `FSCTL_GET_REPARSE_POINT`：前者已经处理好了 `\??\` 前缀
/// 与 UNC 形式，而重复实现一遍只会引入差异。
///
/// 返回 `Ok(None)` 表示那个路径不是一个链接（不存在、或是真目录/文件）。
///
/// # Errors
///
/// 路径存在但是链接、却读不出来。
pub fn junction_target(link: &Path) -> Result<Option<PathBuf>, PlatformError> {
    if !matches!(
        crate::sys::junction_state(link),
        crate::sys::JunctionState::Junction | crate::sys::JunctionState::Symlink
    ) {
        return Ok(None);
    }
    match std::fs::read_link(link) {
        Ok(target) => Ok(Some(target)),
        Err(source) => Err(PlatformError::from_win32(
            u32::try_from(source.raw_os_error().unwrap_or(0)).unwrap_or(0),
            format!("读不出 {} 的链接目标：{source}", link.display()),
        )),
    }
}

/// 删掉一个 junction（或 symlink 目录）。
///
/// **只删链接本身，永远不删目标。**
///
/// # Errors
///
/// 路径不是链接，或删除失败。
pub fn remove_junction(link: &Path) -> Result<(), PlatformError> {
    match crate::sys::junction_state(link) {
        crate::sys::JunctionState::Missing => Ok(()),
        crate::sys::JunctionState::Junction | crate::sys::JunctionState::Symlink => {
            crate::sys::remove_reparse_point(link).map_err(|code| on(link, code))
        }
        crate::sys::JunctionState::RealDirectory => Err(PlatformError::Win32 {
            code: crate::sys::ERROR_ALREADY_EXISTS,
            path: format!(
                "{} 是一个**真实的**目录，不是链接。`remove_junction` 不会递归删它。",
                link.display()
            ),
        }),
        crate::sys::JunctionState::File | crate::sys::JunctionState::OtherReparse { .. } => {
            Err(PlatformError::Win32 {
                code: crate::sys::ERROR_ALREADY_EXISTS,
                path: format!("{} 不是 junction，不删。", link.display()),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn a_junction_can_be_created_read_and_repointed_without_admin() {
        // **这条测试问的是一个平台问题**：未提权时能不能创建 junction，
        // 以及能不能**就地**重指它（而不是删掉再建）。
        //
        // 本机实测（`research/WINDOWS_PLATFORM_CONSTRAINTS.md`）：symlink
        // 文件与目录都失败，而 junction 与硬链接成功。这条测试把那个结论
        // 变成可执行的门禁 —— 换一台机器、换一个卷，它会告诉我们。
        let temp = TempDir::new("junction-basic");
        let v1 = temp.mkdir("versions/v1");
        let v2 = temp.mkdir("versions/v2");
        std::fs::write(v1.join("marker.txt"), b"one").expect("写 v1");
        std::fs::write(v2.join("marker.txt"), b"two").expect("写 v2");
        let link = temp.join("current");

        // ① 创建。
        let created = repoint_junction(&link, &v1).expect("未提权也应当能创建 junction");
        assert_eq!(created, Repoint::Created);
        assert!(link.is_dir(), "junction 应当能被当成目录用");
        assert_eq!(
            std::fs::read_to_string(link.join("marker.txt")).expect("透过 junction 读"),
            "one"
        );

        // ② 读回来。
        let target = junction_target(&link).expect("读目标").expect("有目标");
        assert_eq!(
            target.canonicalize().ok(),
            v1.canonicalize().ok(),
            "读到的目标应当是 v1：{target:?}"
        );

        // ③ **就地重指** —— 这一步的结果决定了 `tuoen use` 能不能是原子的。
        let repointed = repoint_junction(&link, &v2).expect("应当能就地重指");
        assert_eq!(
            repointed,
            Repoint::Replaced,
            "**同标签的重解析点应当能被就地替换** —— 这正是原子翻转的依据。\
             如果这里是 Degraded 或报错，说明 `FSCTL_SET_REPARSE_POINT` \
             在本机上不是替换语义，`tuoen use` 就有一个空窗。"
        );
        assert_eq!(
            std::fs::read_to_string(link.join("marker.txt")).expect("重指后读"),
            "two"
        );

        // ④ 版本目录**都还在** —— 重指不该动到目标。
        assert!(v1.join("marker.txt").is_file());
        assert!(v2.join("marker.txt").is_file());

        // ⑤ 删链接不动目标。
        remove_junction(&link).expect("删 junction");
        assert!(!link.exists(), "链接本身应当没了");
        assert!(v2.join("marker.txt").is_file(), "**删链接绝不能删目标**");
    }

    #[test]
    fn a_real_directory_is_never_deleted_or_overwritten() {
        // **这条守的是最危险的一种误操作。** 如果 `current` 是一个真实的
        // 目录（用户自己建的、或者上一次安装留下的），把它当成 junction
        // 删掉就等于**递归删掉里面的东西**。
        let temp = TempDir::new("junction-guard");
        let real = temp.mkdir("current");
        std::fs::write(real.join("我的数据.txt"), b"precious").expect("写用户数据");
        let target = temp.mkdir("versions/v1");

        let error = repoint_junction(&real, &target).expect_err("真目录必须被拒绝");
        assert!(
            error.to_string().contains("真实的"),
            "错误消息要说明它是真目录：{error}"
        );
        assert!(
            real.join("我的数据.txt").is_file(),
            "**用户的数据必须还在**"
        );

        // `remove_junction` 同样不碰它。
        let error = remove_junction(&real).expect_err("真目录必须被拒绝");
        assert!(error.to_string().contains("真实的"), "{error}");
        assert!(real.join("我的数据.txt").is_file());
    }

    #[test]
    fn a_plain_file_is_refused_not_overwritten() {
        let temp = TempDir::new("junction-file");
        let file = temp.write("current", b"this is a file, not a link");
        let target = temp.mkdir("versions/v1");

        let error = repoint_junction(&file, &target).expect_err("普通文件必须被拒绝");
        assert!(error.to_string().contains("普通文件"), "{error}");
        assert_eq!(
            std::fs::read_to_string(&file).expect("文件还在"),
            "this is a file, not a link",
            "**不能被覆盖**"
        );
    }

    #[test]
    fn the_target_must_already_exist() {
        // 指向一个不存在的目录会造出一个"悬空 junction"，
        // 而它的症状是"目录存在但打不开" —— 那种诊断体验很糟。
        // 所以我们在**建之前**就拒绝。
        let temp = TempDir::new("junction-dangling");
        let link = temp.join("current");
        let missing = temp.join("versions/nope");

        let error = repoint_junction(&link, &missing).expect_err("目标不存在必须被拒绝");
        assert!(error.to_string().contains("已存在的目录"), "{error}");
        assert!(!link.exists(), "不该留下任何东西");
    }

    #[test]
    fn matching_the_target_byte_for_byte_is_not_required_but_repointing_is_idempotent() {
        // 重复指到同一个目标必须是幂等的 —— `tuoen use` 在已经是那个版本时
        // 不该报错，也不该把 junction 弄坏。
        let temp = TempDir::new("junction-idempotent");
        let v1 = temp.mkdir("versions/v1");
        std::fs::write(v1.join("marker.txt"), b"one").expect("写 v1");
        let link = temp.join("current");

        repoint_junction(&link, &v1).expect("第一次");
        repoint_junction(&link, &v1).expect("第二次（同一个目标）");
        repoint_junction(&link, &v1).expect("第三次");
        assert_eq!(
            std::fs::read_to_string(link.join("marker.txt")).expect("透过 junction 读"),
            "one"
        );
    }
}
