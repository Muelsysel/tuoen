//! 存储层的错误。
//!
//! ## 为什么每条错误都带路径
//!
//! 本层的失败几乎全都是"某个具体的目录/文件出事了"，而用户能自己动手的前提是
//! **知道是哪一个**。所以 `Display` 里没有一句"操作失败" —— 失败的那条路径
//! 一定出现在消息里。唯一的例外是 [`StoreError::UnsafeName`]：那个名字
//! **没能**变成路径（这正是它被拒的原因），所以它带的是**被拒的名字**。
//!
//! ## 为什么 `kind()` 永不本地化
//!
//! 决策 35：`--json` 输出必须稳定。中文界面可以改，slug 不可以
//! （与 `tuoen-archive::NameViolation::as_str` 同一条纪律）。
//!
//! ## 为什么区分"用户的问题"与"我们的问题"
//!
//! [`StoreError::is_user_error`] 给调用方决定**退出码**与**提示语气**：
//! "你指定的版本没装"要用户改参数，"磁盘满了"要用户看环境，
//! 而两者混成一句"操作失败"会让用户去改一个根本不是原因的东西。

use std::path::PathBuf;

use thiserror::Error;

/// 存储层的失败。
#[derive(Debug, Error)]
pub enum StoreError {
    /// 一个名字不能当作路径组件用。
    ///
    /// **这是本层的第一道闸**：`%LOCALAPPDATA%\tuoen\store` 底下挂着的名字要么
    /// 来自清单，要么来自命令行，两者都不能直接拼进路径。`what` 说明是
    /// "工具名"还是"版本号"，`value` 是**原样**的那个名字（净化过的名字会掩盖证据）。
    #[error("名字不能用作路径组件：{what} `{value}` —— {why}")]
    UnsafeName {
        /// 是哪种名字（`"工具名"` / `"版本号"`）。
        what: &'static str,
        /// 被拒的名字，原样。
        value: String,
        /// 为什么拒（中文，给用户看）。
        why: String,
    },

    /// 目标位置已经有一份装好的版本。
    ///
    /// **不覆盖**：那一份可能是能用的，而覆盖它等于在用户没同意的情况下换掉他的工具链。
    #[error(
        "`{tool}` 的 {version} 已经装在这里了：{path}\n\
         **不覆盖，也没动你交来的那份载荷** —— 已经装好的那份可能是能用的。\
         要重装请先 `tuoen uninstall {tool} {version}`。"
    )]
    AlreadyInstalled {
        /// 工具 id。
        tool: String,
        /// 版本号。
        version: String,
        /// 已经在的那个版本目录。
        path: PathBuf,
    },

    /// 这个版本没有装在 store 里。
    #[error("`{tool}` 没有已安装的版本 {version}（可以 `tuoen list {tool}` 看有哪些）")]
    NotInstalled {
        /// 工具 id。
        tool: String,
        /// 版本号。
        version: String,
    },

    /// 要删的版本**正被 `current` 指着**。
    ///
    /// 直接删会让 `current` 指向一个不存在的目录 —— 症状是"目录在、打不开"，
    /// 而那时 `current` 上的每一次访问都报错。所以默认拒绝，`--force` 才先摘链接。
    #[error("`{tool}` 的 {version} 是当前激活的版本，删掉它 `current` 就悬空了：{hint}")]
    ActiveVersion {
        /// 工具 id。
        tool: String,
        /// 版本号。
        version: String,
        /// 怎么办（中文，含两条可行路径）。
        hint: String,
    },

    /// **有意的拒绝**：那个位置上的东西我们不动。
    ///
    /// 它不是"我们坏了"，而是"这里的状态需要你先看一眼" —— 典型是载荷位置上
    /// 出现了一个 junction，或者要删的路径**不在 store 里**。
    #[error("拒绝操作 {path}：{why}")]
    Refused {
        /// 拒绝动的那个路径。
        path: PathBuf,
        /// 为什么拒（中文，说明"动了会怎样"）。
        why: String,
    },

    /// 落盘失败。
    #[error("文件系统操作失败：{path}\n底层原因：{source}")]
    Io {
        /// 出错的路径。
        path: PathBuf,
        /// 底层原因。
        source: std::io::Error,
    },

    /// 安装记录**读得出来但不可信**（JSON 坏了，或者 schema 版本我们不认识）。
    ///
    /// 注意它**不是**一个致命错误：记录只是附注，目录才是事实来源 ——
    /// 所以 [`crate::installed_versions`] 把它降级成 `record: None` 而不是失败。
    #[error("安装记录不可信：{path}\n{reason}")]
    RecordBroken {
        /// 记录文件路径（`from_json` 手上没有文件时是占位串 `<文本>`）。
        path: PathBuf,
        /// 为什么不可信。
        reason: String,
    },

    /// 交来的载荷目录不存在。
    #[error("载荷目录不存在：{path}")]
    PayloadMissing {
        /// 那个路径。
        path: PathBuf,
    },

    /// 平台层失败（junction 创建/重指/删除、文件系统事实读取）。
    #[error("平台调用失败：{0}")]
    Platform(#[from] tuoen_platform::PlatformError),
}

/// `from_json` 手上没有文件路径时，`RecordBroken.path` 里放的占位串。
///
/// **刻意不写成空路径**：空路径在消息里表现成"某个地方"，而这里需要一眼看出
/// "问题出在一段文本上，不是某个文件"。读文件的那条路径（`ops` 内部）填的是真实路径。
pub(crate) const TEXT_NOT_A_FILE: &str = "<文本>";

impl StoreError {
    /// 稳定的机器可读 slug（小写 ASCII，进 `--json`）。**永不本地化。**
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::UnsafeName { .. } => "unsafe-name",
            Self::AlreadyInstalled { .. } => "already-installed",
            Self::NotInstalled { .. } => "not-installed",
            Self::ActiveVersion { .. } => "active-version",
            Self::Refused { .. } => "refused",
            Self::Io { .. } => "io",
            Self::RecordBroken { .. } => "record-broken",
            Self::PayloadMissing { .. } => "payload-missing",
            Self::Platform(_) => "platform",
        }
    }

    /// 这条错误是不是"你的操作有问题"而不是"我们坏了"。调用方用它决定退出码提示。
    ///
    /// 判据是"**改一个参数或先处理一下眼前的状态**能不能解决"：
    ///
    /// * `true`：名字不安全、版本已经装了、版本没装、正被激活、有意的拒绝 ——
    ///   这五条都要求用户改点什么（[`Self::Refused`] 尤其：它总是意味着
    ///   "磁盘上有东西需要你先看一眼"，把它报成内部错误会让人去查一个不存在的 bug）。
    /// * `false`：I/O 失败、记录不可信、载荷丢了、平台调用失败 —— 这些是环境或我们自己
    ///   的问题，用户没做错什么，报"请检查你的参数"只会误导。
    #[must_use]
    pub const fn is_user_error(&self) -> bool {
        matches!(
            self,
            Self::UnsafeName { .. }
                | Self::AlreadyInstalled { .. }
                | Self::NotInstalled { .. }
                | Self::ActiveVersion { .. }
                | Self::Refused { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个变体构造一个，用来把 `kind()` 与 `Display` 的纪律钉住。
    fn every_variant() -> Vec<StoreError> {
        vec![
            StoreError::UnsafeName {
                what: "版本号",
                value: "../x".to_owned(),
                why: "名字里有路径分隔符".to_owned(),
            },
            StoreError::AlreadyInstalled {
                tool: "node".to_owned(),
                version: "24.19.0".to_owned(),
                path: PathBuf::from(r"C:\store\node\versions\24.19.0"),
            },
            StoreError::NotInstalled {
                tool: "node".to_owned(),
                version: "24.19.0".to_owned(),
            },
            StoreError::ActiveVersion {
                tool: "node".to_owned(),
                version: "24.19.0".to_owned(),
                hint: "先 `tuoen use` 到别的版本，或加 `--force`".to_owned(),
            },
            StoreError::Refused {
                path: PathBuf::from(r"C:\store\node\versions\24.19.0"),
                why: "是一个 junction".to_owned(),
            },
            StoreError::Io {
                path: PathBuf::from(r"C:\store\node\versions\24.19.0"),
                source: std::io::Error::other("boom"),
            },
            StoreError::RecordBroken {
                path: PathBuf::from(r"C:\store\node\versions\24.19.0.json"),
                reason: "JSON 坏了".to_owned(),
            },
            StoreError::PayloadMissing {
                path: PathBuf::from(r"C:\tmp\payload"),
            },
            StoreError::Platform(tuoen_platform::PlatformError::Unsupported {
                what: "junction".to_owned(),
            }),
        ]
    }

    #[test]
    fn every_kind_is_a_lowercase_ascii_slug_starting_with_a_letter_and_they_are_distinct() {
        // slug 进 `--json`（决策 35）：改它等于改一个脚本 API，
        // 所以它必须稳定、可读、且**互不相同**（否则脚本分不出来是哪一条）。
        let errors = every_variant();
        let mut slugs: Vec<&str> = errors.iter().map(StoreError::kind).collect();
        let total = slugs.len();
        slugs.sort_unstable();
        slugs.dedup();
        assert_eq!(slugs.len(), total, "两个变体共用了同一个 slug");

        for error in &errors {
            let slug = error.kind();
            assert!(slug.is_ascii(), "slug 必须是 ASCII：{slug}");
            assert!(
                !slug.contains(char::is_uppercase),
                "slug 必须是小写：{slug}"
            );
            assert!(
                slug.starts_with(char::is_alphabetic),
                "slug 必须以字母开头：{slug}"
            );
            assert!(
                slug.bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'),
                "slug 只能有小写字母、数字与 `-`：{slug}"
            );
        }
    }

    #[test]
    fn display_is_chinese_prose_not_the_slug() {
        // `kind()` 给机器，`Display` 给人。两者相同就意味着有人把其中一个当另一个用了
        // （与 `tuoen-archive::NameViolation` 的同名测试同一条纪律）。
        for error in every_variant() {
            let text = error.to_string();
            assert_ne!(text, error.kind(), "Display 不能就是 slug");
            assert!(
                !text.is_ascii(),
                "Display 必须是中文（这里全是 ASCII 字符）：{text}"
            );
        }
    }

    #[test]
    fn display_names_the_offending_path_or_name() {
        // "哪个文件"是排查的一半 —— 每条消息都要能回答它。
        let cases = [
            (
                StoreError::Refused {
                    path: PathBuf::from(r"C:\store\node\versions\1.0.0"),
                    why: "是一个 junction".to_owned(),
                },
                r"C:\store\node\versions\1.0.0",
            ),
            (
                StoreError::PayloadMissing {
                    path: PathBuf::from(r"C:\tmp\payload"),
                },
                r"C:\tmp\payload",
            ),
            (
                StoreError::AlreadyInstalled {
                    tool: "node".to_owned(),
                    version: "1.0.0".to_owned(),
                    path: PathBuf::from(r"C:\store\node\versions\1.0.0"),
                },
                r"C:\store\node\versions\1.0.0",
            ),
            (
                StoreError::UnsafeName {
                    what: "版本号",
                    value: "../x".to_owned(),
                    why: "有 `..`".to_owned(),
                },
                "../x",
            ),
        ];
        for (error, needle) in cases {
            assert!(
                error.to_string().contains(needle),
                "{error} 里应当出现 {needle}"
            );
        }
    }

    #[test]
    fn user_errors_are_the_ones_the_user_can_act_on() {
        // 这条断言本身就是文档：它把"退出码该给谁"这件事钉下来。
        let user_errors = [
            StoreError::UnsafeName {
                what: "工具名",
                value: "CON".to_owned(),
                why: "保留设备名".to_owned(),
            },
            StoreError::AlreadyInstalled {
                tool: "node".to_owned(),
                version: "1.0.0".to_owned(),
                path: PathBuf::from(r"C:\store\node\versions\1.0.0"),
            },
            StoreError::NotInstalled {
                tool: "node".to_owned(),
                version: "1.0.0".to_owned(),
            },
            StoreError::ActiveVersion {
                tool: "node".to_owned(),
                version: "1.0.0".to_owned(),
                hint: String::new(),
            },
            StoreError::Refused {
                path: PathBuf::from(r"C:\store\node\versions\1.0.0"),
                why: "是 junction".to_owned(),
            },
        ];
        for error in &user_errors {
            assert!(error.is_user_error(), "应当算用户错误：{error}");
        }

        let environment_errors = [
            StoreError::Io {
                path: PathBuf::from(r"C:\store\node\versions\1.0.0"),
                source: std::io::Error::other("disk full"),
            },
            StoreError::RecordBroken {
                path: PathBuf::from(r"C:\store\node\versions\1.0.0.json"),
                reason: "JSON 坏了".to_owned(),
            },
            StoreError::PayloadMissing {
                path: PathBuf::from(r"C:\tmp\payload"),
            },
            StoreError::Platform(tuoen_platform::PlatformError::Win32 {
                code: 2,
                path: r"C:\store\node\current".to_owned(),
            }),
        ];
        for error in &environment_errors {
            assert!(!error.is_user_error(), "不该算用户错误：{error}");
        }
    }
}
