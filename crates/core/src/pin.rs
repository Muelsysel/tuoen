//! 项目级 pin —— `tuoen.toml` / `tuoen.lock` / `trust.toml` 的模型、信任与子进程环境。
//!
//! # 这一票回答的两个问题
//!
//! 1. 「**这个项目**要的是哪个版本」—— 声明的规格（[`VersionSpec`]）与解析出来的确定版本
//!    （[`LockFile`] / [`resolve`]）。
//! 2. 「进入这个项目时，子进程的 `PATH` 应该长什么样」—— [`ShellPlan`]。
//!
//! 决策原文是 `docs/DESIGN.md` §1.16（决策 113–123），票据是 `docs/tickets/L1-04-pin.md`。
//! 这里的每个类型都能对回那两条里的一条 —— 对不回去的设计不要留。
//!
//! # 三条贯穿全部代码的约束
//!
//! 1. **事实与判断分开。** 读机器的动作只有五个，而且都在调用方看得见的地方：
//!    [`PinFile::load`]、[`LockFile::load`]、[`TrustFile::load`]、
//!    [`fingerprint_of_file`]、[`tuoen_store::installed_versions`]。其余全是纯函数
//!    （[`VersionSpec::matches`]、[`LockFile::mismatches`]、[`TrustFile::state`]、
//!    [`resolve`]、[`ShellPlan::build`]）—— 于是"测试必须用固定装置"是**架构**，
//!    不是纪律。
//! 2. **信任是内容，不是路径。** 指纹是 `tuoen.toml` 的**字节**（决策 117），
//!    所以这里没有任何"更聪明"的归一化：[`normalize_for_fingerprint`] 只做
//!    CRLF → LF —— 空白、BOM、末尾换行全都算内容。任何"顺手清理一下"都会扩大
//!    "改了内容却不触发重新信任"的面。
//! 3. **失败要能看出下一步。** [`PinError::code`] 是稳定的小写 kebab（给脚本与 `--json`），
//!    [`PinError::message`] 是中文人话（含该跑哪条命令），两者各写各的、不共用一份字符串。
//!
//! # 不做什么
//!
//! * **不实现机器级策略。** `%PROGRAMDATA%\tuoen\policy.toml` 只写进了文档（决策 122）：
//!   真去读它要回答"谁有权改、改了算不算信任"，而这一票没有那个答案。
//!   真被要求走那条路时返回 [`PinError::UnwiredRoot`] —— 明确说"没接线"，
//!   比悄悄按用户级处理诚实。
//! * **不动父进程环境。** 子 shell 的 `PATH` 是**算出来塞进子进程**的一条串（决策 119）；
//!   父进程那份、`HKCU\Environment`、junction，这里一个都不写。
//! * **不设 `--ignore-lock`。** 声明与锁对不上就拒绝启动（决策 116），
//!   逃生门是跑一次 `tuoen lock`，不是绕过检查。

use std::fmt;
use std::path::PathBuf;

pub mod file;
pub mod lock;
pub mod resolve;
pub mod shell;
pub mod spec;
pub mod test_support;
pub mod trust;

pub use file::PinFile;
pub use lock::{LOCK_SCHEMA_VERSION, LockFile, LockMismatch, LockTool};
pub use resolve::{MissingTool, Resolution, ResolveContext, ResolvedTool, resolve};
pub use shell::{
    MAX_SHELL_DEPTH, SHELL_DEPTH_VAR, ShadowWarning, ShellKind, ShellLauncher, ShellPlan, ShellSpec,
};
pub use spec::VersionSpec;
pub use trust::{
    TRUST_SCHEMA_VERSION, TrustEntry, TrustFile, TrustState, default_trust_path,
    fingerprint_of_file, normalize_for_fingerprint, same_dir,
};

/// 项目 pin 文件的名字。
///
/// **只有这一处定义。** 指纹、`tuoen.toml` 的存在性、`MissingFile` 那一态全都引用它 ——
/// 三处各写一份字面量的后果是某次改名之后，指纹算的是新名字、存在性查的是旧名字。
pub const PIN_FILE_NAME: &str = "tuoen.toml";

/// 锁文件的名字。
pub const LOCK_FILE_NAME: &str = "tuoen.lock";

/// 信任清单的名字（放在 `%APPDATA%\tuoen\` 下，见 [`default_trust_path`]）。
pub const TRUST_FILE_NAME: &str = "trust.toml";

/// `pin` 这一层的错误。
///
/// **变体字段全是 `pub`**：CLI 侧有两处错误是它自己判出来的（`lock-mismatch`、
/// `version-not-installed`），它必须能造出这个类型的值，否则它只能自己拼一份
/// 中文消息与错误码 —— 那时错误码就有两个来源，而只有一个是真的稳定。
/// 便利构造器见 [`PinError::lock_mismatch`] 等三个。
#[derive(Debug)]
pub enum PinError {
    /// 这个目录里没有 `tuoen.toml`。`path` 是**试着读的那个文件路径**。
    MissingPin {
        /// 试着读的 `tuoen.toml` 路径。
        path: PathBuf,
    },
    /// 文件在那儿，但读不动（TOML 语法、段名、字段类型、版本规格……）。
    Parse {
        /// 出错的文件；对"一段文本"（如 `VersionSpec::parse`）为 `None`。
        path: Option<PathBuf>,
        /// 中文人话：说清**哪一行/哪一段**不对，以及怎么改。
        message: String,
    },
    /// 读写文件失败（权限、占用、不是 UTF-8……）。
    Io {
        /// 出错的文件路径。
        path: PathBuf,
        /// 底层错误原文。**保留原文**：本地化的 `io::Error` 文案会随系统语言变，
        /// 但它是排查时唯一的现场，不翻译、不吞掉。
        message: String,
    },
    /// `[tools]` 里出现了 tuoen 不认识的工具 id（决策 113）。
    UnknownTool {
        /// 声明里的那个 id。
        name: String,
        /// 我们认识的 id（顺序与 `KNOWN_TOOLS` 一致）。
        known: Vec<String>,
    },
    /// 声明的版本规格在机器上没有任何一个候选满足它（决策 114）。
    VersionNotInstalled {
        /// 工具 id。
        tool: String,
        /// 声明的规格原文。
        spec: String,
        /// 已经装了的版本（去重、按版本降序）；一个都没装时为空。
        installed: Vec<String>,
    },
    /// `tuoen.lock` 与 `tuoen.toml` 对不上（决策 116）。
    LockMismatch {
        /// 逐条差异。
        details: Vec<LockMismatch>,
    },
    /// 这条根**没有接线**，而不是"没有配置"。
    UnwiredRoot {
        /// 哪条根。目前只有机器级策略这一条路会走到这里（决策 122）。
        what: &'static str,
    },
    /// 子 shell 嵌得太深（决策 121）。
    Depth {
        /// 请求启动的这一层深度。
        depth: u32,
        /// 上限。
        max: u32,
    },
}

impl PinError {
    /// 稳定的错误码，小写 kebab，**不本地化**（`--json` 与脚本用它）。
    ///
    /// 与 [`PinError::message`] 分开写是刻意的：消息会随用户反馈改词，
    /// 而错误码一旦发出去就是契约 —— 两个需求共用一份字符串时，改消息就成了改契约。
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::MissingPin { .. } => "missing-pin",
            Self::Parse { .. } => "pin-parse",
            Self::Io { .. } => "pin-io",
            Self::UnknownTool { .. } => "unknown-tool",
            Self::VersionNotInstalled { .. } => "version-not-installed",
            Self::LockMismatch { .. } => "lock-mismatch",
            Self::UnwiredRoot { .. } => "unwired-root",
            Self::Depth { .. } => "shell-depth",
        }
    }

    /// 中文人话，**含下一步该跑什么命令**。
    ///
    /// 一条只说"出错了"的消息等于把排查成本转给用户；本仓库的每条错误都要求
    /// 能回答"那我该干什么"。
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::MissingPin { path } => format!(
                "这个目录里没有 `{PIN_FILE_NAME}`（找的是 `{}`）。项目级的 pin 声明是**手写**的\
                 ——`[tools]` 段里一行一个工具（`node = \"24\"`），或者确认你是不是进错了目录。",
                path.display()
            ),
            Self::Parse { path, message } => match path {
                Some(path) => format!(
                    "`{}` 读不动：{message}。改完这一处再重跑刚才那条命令。",
                    path.display()
                ),
                None => message.clone(),
            },
            Self::Io { path, message } => format!(
                "`{}` 读写失败：{message}。检查这个文件是不是被别的程序占着、或者当前用户有没有权限，\
                 然后重跑刚才那条命令。",
                path.display()
            ),
            Self::UnknownTool { name, known } => format!(
                "`[tools]` 里的 `{name}` 不是 tuoen 认识的工具。认识的是：{}。\
                 把它删掉，或者跑 `tuoen catalog list` 看看认识哪些工具。",
                known.join(" / ")
            ),
            Self::VersionNotInstalled {
                tool,
                spec,
                installed,
            } => {
                let installed_text = if installed.is_empty() {
                    "一个都没装".to_owned()
                } else {
                    installed.join(", ")
                };
                format!(
                    "`{tool}` 没有满足 `{spec}` 的版本（已经装了的：{installed_text}）。\
                     跑 `tuoen install {tool}@{spec}` 装上，或者把 `{PIN_FILE_NAME}` 里的规格改成已装的版本。"
                )
            }
            Self::LockMismatch { details } => {
                let lines: Vec<String> = details.iter().map(describe_mismatch).collect();
                format!(
                    "`{LOCK_FILE_NAME}` 与 `{PIN_FILE_NAME}` 对不上（{} 处）：{}。\
                     跑 `tuoen lock` 重新生成锁文件，再重跑刚才那条命令。",
                    details.len(),
                    lines.join("；")
                )
            }
            Self::UnwiredRoot { what } => format!(
                "`{what}` 这条根还没有接线：本版本只支持用户级信任清单（`%APPDATA%\\tuoen\\{TRUST_FILE_NAME}`），\
                 机器级策略 `%PROGRAMDATA%\\tuoen\\policy.toml` 只写进了文档、没有实现（决策 122）。"
            ),
            Self::Depth { depth, max } => format!(
                "嵌套子 shell 已经有 {depth} 层了（上限 {max}）——`{SHELL_DEPTH_VAR}` 是这么说的。\
                 先 `exit` 掉几层，或者检查是不是有脚本在递归地调 `tuoen shell`。"
            ),
        }
    }

    /// 造一条 `lock-mismatch`。CLI 自己判出差异时用这个，别自己拼错误码。
    #[must_use]
    pub fn lock_mismatch(details: Vec<LockMismatch>) -> Self {
        Self::LockMismatch { details }
    }

    /// 从一条缺口造 `version-not-installed`。
    #[must_use]
    pub fn version_not_installed(missing: &MissingTool) -> Self {
        Self::VersionNotInstalled {
            tool: missing.name.clone(),
            spec: missing.spec.clone(),
            installed: missing.installed.clone(),
        }
    }

    /// 从一个不认识的 id 造 `unknown-tool`。`known` 直接给工具规格表的切片。
    #[must_use]
    pub fn unknown_tool(name: &str, known: &[crate::detect::ToolSpec]) -> Self {
        Self::UnknownTool {
            name: name.to_owned(),
            known: known.iter().map(|spec| spec.id.to_owned()).collect(),
        }
    }

    /// 这条错误是不是"用户把项目配置写错了"。
    ///
    /// CLI 用它决定要不要在消息后面追加"看看是哪一条 pin 写错了"之类的提示 ——
    /// 分类放进调用方会让每个调用方各判一遍（然后各处判得不一样）。
    #[must_use]
    pub const fn is_project_config(&self) -> bool {
        matches!(
            self,
            Self::MissingPin { .. }
                | Self::Parse { .. }
                | Self::UnknownTool { .. }
                | Self::VersionNotInstalled { .. }
                | Self::LockMismatch { .. }
        )
    }
}

impl fmt::Display for PinError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl std::error::Error for PinError {}

/// 一条差异的人话。声明与锁各可能缺席 —— 四种组合里 `(None, None)` 造不出来，
/// 但枚举类型不禁止它，所以这里也不 panic。
fn describe_mismatch(detail: &LockMismatch) -> String {
    match (&detail.declared, &detail.locked) {
        (Some(declared), Some(locked)) => {
            format!(
                "`{}`：声明的是 `{declared}`，锁里是 `{locked}`",
                detail.name
            )
        }
        (Some(declared), None) => format!("`{}`：声明了 `{declared}`，锁里没有", detail.name),
        (None, Some(locked)) => format!("`{}`：锁里有 `{locked}`，声明里已经没有", detail.name),
        (None, None) => format!("`{}`：声明和锁里都没有（这一条不该出现）", detail.name),
    }
}

/// 把一份内容原子地写到 `path`：先写同目录的临时文件，再 `rename` 顶上去。
///
/// **为什么必须是 `rename`**：这两份文件会被"读一次、判断、写回"的流程碰到
/// （`tuoen trust` / `tuoen lock`），半截文件被读到会变成一条"清单坏了"的假警报。
/// `rename` 在同一卷上是原子的，读到的一半只能是旧内容或新内容。
///
/// 临时文件名是**确定的**（不带 pid / 时间戳）：一个确定的名字才谈得上"跑完
/// 目录里不该多出别的东西"这种可断言的性质。代价是两个 `tuoen` 同时写同一份清单会
/// 互相覆盖 —— 那是"最后写的赢"，不是"文件坏掉"。
pub(crate) fn write_atomically(path: &std::path::Path, bytes: &[u8]) -> Result<(), PinError> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    std::fs::create_dir_all(&parent).map_err(|error| PinError::Io {
        path: parent.clone(),
        message: error.to_string(),
    })?;
    let name = path.file_name().map_or_else(
        || "tuoen".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    let temporary = parent.join(format!("{name}.tmp"));
    std::fs::write(&temporary, bytes).map_err(|error| PinError::Io {
        path: temporary.clone(),
        message: error.to_string(),
    })?;
    std::fs::rename(&temporary, path).map_err(|error| {
        // 顶不上去就把临时文件收掉：留着它，下一次 `load` 不会读到（名字不同），
        // 但它会在用户目录里越攒越多。
        let _ = std::fs::remove_file(&temporary);
        PinError::Io {
            path: path.to_path_buf(),
            message: error.to_string(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::KNOWN_TOOLS;

    #[test]
    fn constants_match_frozen_api() {
        assert_eq!(PIN_FILE_NAME, "tuoen.toml");
        assert_eq!(LOCK_FILE_NAME, "tuoen.lock");
        assert_eq!(TRUST_FILE_NAME, "trust.toml");
        assert_eq!(SHELL_DEPTH_VAR, "TUOEN_SHELL_DEPTH");
        assert_eq!(MAX_SHELL_DEPTH, 5);
    }

    #[test]
    fn every_variant_has_a_stable_code() {
        let cases: Vec<(PinError, &str)> = vec![
            (
                PinError::MissingPin {
                    path: PathBuf::from("C:\\proj\\tuoen.toml"),
                },
                "missing-pin",
            ),
            (
                PinError::Parse {
                    path: None,
                    message: "x".to_owned(),
                },
                "pin-parse",
            ),
            (
                PinError::Io {
                    path: PathBuf::from("x"),
                    message: "y".to_owned(),
                },
                "pin-io",
            ),
            (
                PinError::UnknownTool {
                    name: "nod".to_owned(),
                    known: vec!["node".to_owned()],
                },
                "unknown-tool",
            ),
            (
                PinError::VersionNotInstalled {
                    tool: "node".to_owned(),
                    spec: "24".to_owned(),
                    installed: Vec::new(),
                },
                "version-not-installed",
            ),
            (
                PinError::LockMismatch {
                    details: Vec::new(),
                },
                "lock-mismatch",
            ),
            (PinError::UnwiredRoot { what: "policy" }, "unwired-root"),
            (PinError::Depth { depth: 6, max: 5 }, "shell-depth"),
        ];
        for (error, expected) in cases {
            assert_eq!(error.code(), expected, "{error:?}");
            // 码是小写 kebab：没有下划线、没有大写、没有空格。
            assert!(
                !error.code().contains('_') && !error.code().contains(' '),
                "{} 不是小写 kebab",
                error.code()
            );
            assert_eq!(error.code(), error.code().to_lowercase());
        }
    }

    #[test]
    fn message_is_chinese_and_names_the_next_command() {
        let missing = PinError::MissingPin {
            path: PathBuf::from("C:\\proj\\tuoen.toml"),
        };
        // **不许点名一个不存在的命令。** 第一版这里写的是"跑 `tuoen pin` 建一份"，
        // 而 `tuoen pin` 根本不存在（`unrecognized subcommand 'pin'`）——
        // 一句看起来完全合理的错话，正好出现在用户第一次跑 `tuoen shell` 时看到的那一行上。
        // 判据因此是**否定的**：消息里不许出现任何"跑 `tuoen <x>`"形式的建议，除非 x 真的存在。
        assert!(missing.message().contains("手写"), "{}", missing.message());
        // 两条都**不存在或答非所问**：`tuoen pin` 没有这个子命令；`tuoen list` 只列
        // 我们自己管的工具，答不了"我们认识哪些工具"（那是 `tuoen catalog list`）。
        for word in ["tuoen pin", "tuoen list"] {
            assert!(
                !missing.message().contains(word),
                "消息里点名了不该点名的命令 `{word}`：{}",
                missing.message()
            );
        }

        let mismatch = PinError::LockMismatch {
            details: vec![LockMismatch {
                name: "node".to_owned(),
                declared: Some("24".to_owned()),
                locked: Some("20".to_owned()),
            }],
        };
        let message = mismatch.message();
        assert!(message.contains("node"), "{message}");
        assert!(
            message.contains("24") && message.contains("20"),
            "{message}"
        );
        assert!(message.contains("tuoen lock"), "{message}");

        let not_installed = PinError::VersionNotInstalled {
            tool: "node".to_owned(),
            spec: "24".to_owned(),
            installed: vec!["20.11.0".to_owned()],
        };
        let message = not_installed.message();
        assert!(message.contains("20.11.0"), "{message}");
        assert!(message.contains("tuoen install node@24"), "{message}");

        // `Display` 就是 `message()`：两处各写一份的话，日志与 UI 会各说各话。
        assert_eq!(missing.to_string(), missing.message());
        // 人话里有中文（不是把英文原文丢给用户）。
        assert!(
            missing
                .message()
                .chars()
                .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
        );
    }

    #[test]
    fn constructors_match_variant_fields() {
        let known = KNOWN_TOOLS;
        let error = PinError::unknown_tool("nod", known);
        match &error {
            PinError::UnknownTool { name, known: ids } => {
                assert_eq!(name, "nod");
                assert!(ids.iter().any(|id| id == "node"));
                assert_eq!(ids.len(), known.len());
            }
            other => panic!("变体错了：{other:?}"),
        }
        // `tuoen catalog list` 才是"我们认识哪些工具"（`tuoen list` 只列**我们自己管的**，
        // 答不了这个问题 —— 第二句错话，同样是复核时抓到的）。
        assert!(
            error.message().contains("tuoen catalog list"),
            "{}",
            error.message()
        );
        assert!(
            !error.message().contains("跑 `tuoen list`"),
            "{}",
            error.message()
        );

        let missing = MissingTool {
            name: "node".to_owned(),
            spec: "24".to_owned(),
            installed: vec!["20.11.0".to_owned()],
            next_command: "tuoen install node@24".to_owned(),
        };
        let error = PinError::version_not_installed(&missing);
        match &error {
            PinError::VersionNotInstalled {
                tool,
                spec,
                installed,
            } => {
                assert_eq!(tool, "node");
                assert_eq!(spec, "24");
                assert_eq!(installed, &vec!["20.11.0".to_owned()]);
            }
            other => panic!("变体错了：{other:?}"),
        }

        let details = vec![LockMismatch {
            name: "node".to_owned(),
            declared: Some("24".to_owned()),
            locked: None,
        }];
        match PinError::lock_mismatch(details) {
            PinError::LockMismatch { details } => {
                assert_eq!(details.len(), 1);
                assert!(details[0].declared.is_some() && details[0].locked.is_none());
            }
            other => panic!("变体错了：{other:?}"),
        }
    }

    #[test]
    fn describe_mismatch_never_panics() {
        let tool = |declared: Option<&str>, locked: Option<&str>| LockMismatch {
            name: "node".to_owned(),
            declared: declared.map(str::to_owned),
            locked: locked.map(str::to_owned),
        };
        for detail in [
            tool(Some("24"), Some("20")),
            tool(Some("24"), None),
            tool(None, Some("20")),
            tool(None, None),
        ] {
            let text = describe_mismatch(&detail);
            assert!(text.contains("node"), "{text}");
        }
    }

    #[test]
    fn project_config_classification() {
        assert!(
            PinError::MissingPin {
                path: PathBuf::from("x")
            }
            .is_project_config()
        );
        assert!(
            !PinError::Io {
                path: PathBuf::from("x"),
                message: "y".to_owned()
            }
            .is_project_config()
        );
        assert_eq!(
            PinError::MissingPin {
                path: PathBuf::from("x")
            }
            .to_string(),
            PinError::MissingPin {
                path: PathBuf::from("x")
            }
            .message()
        );
    }

    #[test]
    fn atomic_write_overwrites_in_place() {
        let temp = test_support::TempDir::new("atomic");
        let path = temp.join("tuoen.lock");
        write_atomically(&path, b"first\n").expect("第一次写");
        assert_eq!(std::fs::read_to_string(&path).expect("读回"), "first\n");
        write_atomically(&path, b"second\n").expect("覆盖写");
        assert_eq!(std::fs::read_to_string(&path).expect("读回"), "second\n");
        // 临时文件不许留下：名字确定，所以"留下了"是看得见的。
        let leftovers: Vec<String> = std::fs::read_dir(temp.path())
            .expect("列目录")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "留下了临时文件：{leftovers:?}");
    }
}
