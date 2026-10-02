//! `tuoen.d/` 的读入侧：**两侧同一形状**里的"目标侧"。
//!
//! # 为什么不另造一套文件格式
//!
//! [`RestoreBundle`] 的五个字段与 [`crate::capture::CaptureBundle`] 一一对应，
//! 类型也是**同一批**（`ToolsFile` / `PathFile` / `EnvFile` / `WslFile` / `SkippedFile`）。
//! 换一个格式就得再写一遍解析、再修一遍版本兼容，而两份形状只要有一处漂移，
//! "还原到本机应当产出接近空的计划"这条判据立刻变成假的（决策 126 的同一条理由）。
//!
//! 于是本机侧是 `from_capture(&captured)`（内存里，不落盘），目标侧是 `load(dir, fs)`
//! （从 `tuoen.d/` 读）—— 两侧流进 [`super::plan`] 时是同一个类型。
//!
//! # `source`：为什么它不是 CLI 的事
//!
//! 计划里那个 `snapshot` 字段是**用户敲的那个路径原样**（决策 162）。它必须在
//! "读到文件"的那一刻就定下来：如果 core 出一个"合法但占位"的值、指望 CLI 记得覆盖，
//! 那么忘记覆盖的后果是**一个看起来完全正常的路径/时间戳**，永远不会红。
//! `load` 拿得到 `dir`，所以它填；`from_capture` 拿不到目录，所以它是 `None`，
//! `plan` 才退到快照自己的 `captured_at`。
//!
//! # `schema.toml` 为什么不参与
//!
//! `capture` 写了一份 `schema.toml`（它声明"这份快照包含哪些 section"），
//! 但 [`RestoreBundle`] 里没有它 —— 计划只信**文件在不在**。
//! 理由是保守：`schema.toml` 说包含 `env`、而 `env.toml` 不在时，能做的事只有
//! "报一个错"或者"当成没有"。报错会把一份**可以部分还原**的快照变成完全不能用，
//! 而当成"没有"是一条**可见**的 `skipped` + `note = "section-not-in-snapshot"`
//! （决策 159）—— 用户看得见少了什么，这比一条读不出来的完整快照更有用。
//!
//! # 只读
//!
//! 这个文件里没有写操作。`load` 只读它知道的那五个文件名，不列目录、不 glob、
//! 不递归 —— 一份快照里多出来的文件不是错误，只是**不是我们的**。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tuoen_platform::FileSystem;

use crate::capture::{CaptureBundle, EnvFile, PathFile, SkippedFile, ToolsFile, WslFile};

use super::SectionId;

/// 四个 section 各自的文件名（**顺序即 `SectionId::ALL` 的顺序**）。
pub const SNAPSHOT_FILES: [(SectionId, &str); 4] = [
    (SectionId::Tools, "tools.toml"),
    (SectionId::Path, "path.toml"),
    (SectionId::Env, "env.toml"),
    (SectionId::Wsl, "wsl.toml"),
];

/// 跳过清单的文件名。**它不是 section**（跨 section 的一份说明），
/// 所以它单独一行，不混进 [`SNAPSHOT_FILES`]。
pub const SKIPPED_FILE: &str = "skipped.toml";

/// 一份 `tuoen.d/` 快照（或它的内存版本）。
///
/// 每个 `Option` 都是"这一侧没有这个 section"，**不是**"这台机器上什么都没有" ——
/// 与 [`CaptureBundle`] 的约定逐字相同。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreBundle {
    /// 这份快照**从哪来**：`load` 填用户敲的那个路径原样（相对就相对），
    /// `from_capture` 填 `None`（本机侧没有"从哪来"这个问题）。
    ///
    /// 它进 [`super::RestorePlan::snapshot`]，语义是"用户敲的那个路径"（决策 162）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// `tools.toml`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsFile>,
    /// `path.toml`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathFile>,
    /// `env.toml`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<EnvFile>,
    /// `wsl.toml`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wsl: Option<WslFile>,
    /// `skipped.toml`。**看见了但没写的东西**（本票只用到凭据那一类）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<SkippedFile>,
}

impl RestoreBundle {
    /// 本机侧：把现场 `capture` 出来的东西原样搬进来（**在内存里，不落盘**）。
    ///
    /// `source` 恒为 `None` —— 本机侧不是一个"用户敲的路径"。
    #[must_use]
    pub fn from_capture(bundle: &CaptureBundle) -> Self {
        Self {
            source: None,
            tools: bundle.tools.clone(),
            path: bundle.path.clone(),
            env: bundle.env.clone(),
            wsl: bundle.wsl.clone(),
            skipped: bundle.skipped.clone(),
        }
    }

    /// 目标侧：从 `tuoen.d/` 读一份快照。
    ///
    /// `source` 是**传进来的 `dir` 原样**（相对就相对，不做 `canonicalize`）——
    /// 用户敲的是哪个串，计划里就印哪个串。
    ///
    /// # Errors
    ///
    /// * `snapshot-io` —— 目录不存在 / 不是目录 / 某个**已存在**的文件读不出来；
    /// * `snapshot-toml` —— 某个文件在那儿但不是合法的 TOML；
    /// * `empty-snapshot` —— 目录在，但一个 section 文件都没有（**绝不静默成功**）。
    pub fn load(dir: &Path, fs: &impl FileSystem) -> Result<Self, RestoreError> {
        let facts = fs.inspect(dir);
        if !facts.exists {
            return Err(RestoreError::Io {
                path: dir.to_path_buf(),
                source: std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("快照目录不存在：{}", dir.display()),
                ),
            });
        }
        if !facts.is_dir {
            return Err(RestoreError::Io {
                path: dir.to_path_buf(),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("快照路径不是目录：{}", dir.display()),
                ),
            });
        }

        let mut bundle = Self {
            source: Some(dir.to_string_lossy().into_owned()),
            ..Self::default()
        };

        for (id, file) in SNAPSHOT_FILES {
            let path = dir.join(file);
            if !fs.inspect(&path).exists {
                continue;
            }
            let text = read(&path)?;
            match id {
                SectionId::Tools => bundle.tools = Some(parse(file, &path, &text)?),
                SectionId::Path => bundle.path = Some(parse(file, &path, &text)?),
                SectionId::Env => bundle.env = Some(parse(file, &path, &text)?),
                SectionId::Wsl => bundle.wsl = Some(parse(file, &path, &text)?),
            }
        }

        let skipped_path = dir.join(SKIPPED_FILE);
        if fs.inspect(&skipped_path).exists {
            let text = read(&skipped_path)?;
            bundle.skipped = Some(parse(SKIPPED_FILE, &skipped_path, &text)?);
        }

        // **空快照不许静默成功**：一个 section 文件都没有时，`plan` 会把四个 section
        // 全记成 `skipped`，而一份"四个 skipped"的计划看起来像"本机已经全都对上了"。
        if bundle.sections_present().is_empty() {
            return Err(RestoreError::EmptySnapshot {
                dir: dir.to_path_buf(),
            });
        }

        Ok(bundle)
    }

    /// 这一侧**真的有**哪些 section，按 [`SectionId::ALL`] 的固定顺序。
    #[must_use]
    pub fn sections_present(&self) -> Vec<SectionId> {
        SectionId::ALL
            .into_iter()
            .filter(|id| self.has_section(*id))
            .collect()
    }

    /// 这一侧有没有这个 section。
    #[must_use]
    pub fn has_section(&self, id: SectionId) -> bool {
        match id {
            SectionId::Tools => self.tools.is_some(),
            SectionId::Path => self.path.is_some(),
            SectionId::Env => self.env.is_some(),
            SectionId::Wsl => self.wsl.is_some(),
        }
    }

    /// 硬要求某一个 section 在。
    ///
    /// 这是唯一会产出 [`RestoreError::SectionMissing`] 的入口。**`plan` 不走它**：
    /// 决策 159 说的是"`--only` 指向快照里没有的 section → 记 `skipped`，
    /// **不是**静默成功"，也不是错误退出。所以 `plan` 用 [`Self::has_section`]，
    /// 而调用方（未来的 `restore --require tools`、或 GUI 的某一步）真的需要
    /// "缺了就停"时才用这个。
    ///
    /// # Errors
    ///
    /// `snapshot-section-missing`。
    pub fn require(&self, id: SectionId) -> Result<(), RestoreError> {
        if self.has_section(id) {
            Ok(())
        } else {
            Err(RestoreError::SectionMissing { section: id })
        }
    }

    /// 这份快照的捕获时间（四个 section 文件里第一个有值的那个）。
    ///
    /// 它只在 `source` 缺失时当兜底（决策 162）—— 本机侧的内存 bundle 没有"从哪来"，
    /// 而快照自己的时间戳至少是一个**真的**能定位到那份快照的值。
    #[must_use]
    pub fn captured_at(&self) -> Option<&str> {
        let first = [
            self.tools.as_ref().map(|file| file.captured_at.as_str()),
            self.path.as_ref().map(|file| file.captured_at.as_str()),
            self.env.as_ref().map(|file| file.captured_at.as_str()),
            self.wsl.as_ref().map(|file| file.captured_at.as_str()),
            self.skipped.as_ref().map(|file| file.captured_at.as_str()),
        ];
        first.into_iter().flatten().find(|text| !text.is_empty())
    }
}

/// 读一个**已经确认存在**的文件。
fn read(path: &Path) -> Result<String, RestoreError> {
    std::fs::read_to_string(path).map_err(|source| RestoreError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// 反序列化一个 section 文件。
fn parse<T: serde::de::DeserializeOwned>(
    file: &'static str,
    path: &Path,
    text: &str,
) -> Result<T, RestoreError> {
    toml::from_str(text).map_err(|source| RestoreError::Toml {
        file,
        path: path.to_path_buf(),
        // **装箱**：`toml::de::Error` 有 88 字节，直接放进来会让 `RestoreError` 变成
        // 136 字节 —— 而它是**每一个** `Result<_, RestoreError>` 的宽度（clippy 的
        // `result_large_err` 说的就是这件事）。装成一层指针之后错误链一点没少。
        source: Box::new(source),
    })
}

/// 读一份快照失败的原因。
///
/// 四个码是稳定契约（`--json` 里逐字出现，**不本地化**）。它们必须分得开，因为
/// 用户要做的下一步完全不同：目录敲错了 / 文件坏了 / 快照是空的 / 这一份里没有那一段。
#[derive(Debug, thiserror::Error)]
pub enum RestoreError {
    /// 目录在，但一个 section 文件都没有。**这不是"没有差异"，是一份没用的快照。**
    #[error("快照目录 {dir} 里一个 section 文件都没有（tools/path/env/wsl）")]
    EmptySnapshot {
        /// 用户敲的那个目录。
        dir: PathBuf,
    },
    /// 读不到：目录不存在 / 不是目录 / 文件读不出来。
    #[error("读不到 {path}：{source}")]
    Io {
        /// 出问题的路径。
        path: PathBuf,
        /// 底层 I/O 错误。
        #[source]
        source: std::io::Error,
    },
    /// 读到了，但内容不是一份合法的 section 文件。
    #[error("{path} 不是合法的 TOML（{file}）：{source}")]
    Toml {
        /// 文件名（相对 `tuoen.d/`）—— 报错时用户要看的是"哪个文件"。
        file: &'static str,
        /// 被读的完整路径。
        path: PathBuf,
        /// 底层 TOML 错误。**装箱**：它的原身有 88 字节，会让 `RestoreError` 撑到
        /// 136 字节，而那个宽度会摊到**每一个** `Result<_, RestoreError>` 上。
        #[source]
        source: Box<toml::de::Error>,
    },
    /// 硬要求某一个 section 却不在（[`RestoreBundle::require`]）。
    #[error("这份快照里没有 `{section}` 这一段")]
    SectionMissing {
        /// 缺的是哪一段。
        section: SectionId,
    },
}

impl RestoreError {
    /// 稳定小写 slug（`--json` 的错误码，**不本地化**）。
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::EmptySnapshot { .. } => "empty-snapshot",
            Self::Io { .. } => "snapshot-io",
            Self::Toml { .. } => "snapshot-toml",
            Self::SectionMissing { .. } => "snapshot-section-missing",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{EnvFile, PathBudgetRow, PathFile, SCHEMA_VERSION, SkippedFile};
    use crate::restore::test_support::TempDir;

    /// 一份最小的、合法的 `tuoen.d/`。**只写这一个目录**（`%TEMP%` 下面自建的那层）。
    fn full_snapshot(temp: &TempDir) {
        temp.write(
            "schema.toml",
            "schema_version = 1\ncaptured_at = \"2026-10-02T12:00:00Z\"\ntuoen_version = \"0.1.0\"\nsections = [\"tools\"]\n",
        );
        temp.write(
            "tools.toml",
            "schema_version = 1\ncaptured_at = \"2026-10-02T12:00:00Z\"\n\
             [[tool]]\nname = \"node\"\nversion = \"24.19.0\"\npath = \"C:\\\\nvm4w\\\\nodejs\"\n\
             source = \"manager\"\nconfidence = \"manager-owned\"\nevidence = \"夹具\"\nreproducible = false\n",
        );
        temp.write(
            "path.toml",
            "schema_version = 1\ncaptured_at = \"2026-10-02T12:00:00Z\"\n\
             [budget]\nraw_user_chars = 0\nraw_machine_chars = 0\neffective_chars = 0\n\
             cliff = 8191\nremaining = 8191\nlevel = \"ok\"\n",
        );
        temp.write(
            "env.toml",
            "schema_version = 1\ncaptured_at = \"2026-10-02T12:00:00Z\"\n",
        );
        temp.write(
            "wsl.toml",
            "schema_version = 1\ncaptured_at = \"2026-10-02T12:00:00Z\"\n",
        );
        temp.write(
            "skipped.toml",
            "schema_version = 1\ncaptured_at = \"2026-10-02T12:00:00Z\"\n\
             [[skipped]]\nsection = \"env\"\nscope = \"user\"\nname = \"ARK_API_KEY\"\n\
             kind = \"credential-named\"\nreason = \"夹具\"\n",
        );
    }

    #[test]
    fn load_reads_every_section_and_remembers_the_source_verbatim() {
        let temp = TempDir::new("load-full");
        full_snapshot(&temp);
        // `dir` 带上一个多余的样本（`.`），证明我们**不做 canonicalize**。
        let dir = temp.path().join(".");
        let bundle = RestoreBundle::load(&dir, &tuoen_platform::RealFileSystem).expect("读得回来");

        assert_eq!(
            bundle.source.as_deref(),
            Some(dir.to_string_lossy().as_ref()),
            "决策 162：source 就是传进来那个字符串"
        );
        assert_eq!(
            bundle.sections_present(),
            vec![
                SectionId::Tools,
                SectionId::Path,
                SectionId::Env,
                SectionId::Wsl
            ]
        );
        assert_eq!(bundle.tools.expect("tools").tool[0].name, "node");
        assert!(bundle.skipped.expect("skipped").skipped[0].name == "ARK_API_KEY");
    }

    #[test]
    fn load_of_a_directory_without_any_section_file_is_an_empty_snapshot() {
        let temp = TempDir::new("load-empty");
        let error = RestoreBundle::load(temp.path(), &tuoen_platform::RealFileSystem)
            .expect_err("空目录不许静默成功");
        assert_eq!(error.code(), "empty-snapshot");
    }

    #[test]
    fn a_lone_skipped_file_is_still_an_empty_snapshot() {
        // `skipped.toml` **不是 section**：只有它，等于"这份快照什么都没说"。
        let temp = TempDir::new("load-only-skipped");
        temp.write(
            "skipped.toml",
            "schema_version = 1\ncaptured_at = \"2026-10-02T12:00:00Z\"\n",
        );
        let error = RestoreBundle::load(temp.path(), &tuoen_platform::RealFileSystem)
            .expect_err("没有 section 文件");
        assert_eq!(error.code(), "empty-snapshot");
    }

    #[test]
    fn a_missing_directory_is_snapshot_io_not_an_empty_snapshot() {
        // 两种情况的下一步完全不同：一个是"路径敲错了"，一个是"这份快照是空的"。
        let temp = TempDir::new("load-missing");
        let missing = temp.path().join("nope");
        let error =
            RestoreBundle::load(&missing, &tuoen_platform::RealFileSystem).expect_err("目录不在");
        assert_eq!(error.code(), "snapshot-io");
        assert!(error.to_string().contains("nope"), "{error}");
    }

    #[test]
    fn a_file_that_is_not_toml_is_snapshot_toml_and_names_the_file() {
        let temp = TempDir::new("load-bad-toml");
        temp.write("tools.toml", "this is = = not toml");
        let error =
            RestoreBundle::load(temp.path(), &tuoen_platform::RealFileSystem).expect_err("坏 TOML");
        assert_eq!(error.code(), "snapshot-toml");
        assert!(error.to_string().contains("tools.toml"), "{error}");
    }

    #[test]
    fn a_file_that_exists_but_cannot_be_read_is_snapshot_io() {
        // 存在性是 `fs` 说了算的（假文件系统说"在"，磁盘上却没有）——
        // 这正是"读不到"与"没这一段"必须分开的地方。
        let temp = TempDir::new("load-ghost");
        let fs = tuoen_platform::fixture::MachineFixture {
            dirs: vec![tuoen_platform::fixture::FixtureDir::new(
                &temp.path().to_string_lossy(),
                vec![tuoen_platform::fixture::FixturePath::file("tools.toml", 10)],
            )],
            ..tuoen_platform::fixture::MachineFixture::default()
        }
        .build()
        .fs;
        let error = RestoreBundle::load(temp.path(), &fs).expect_err("假 fs 说在、磁盘上没有");
        assert_eq!(error.code(), "snapshot-io");
    }

    #[test]
    fn require_is_the_only_source_of_snapshot_section_missing() {
        let bundle = RestoreBundle::default();
        let error = bundle.require(SectionId::Env).expect_err("没有 env");
        assert_eq!(error.code(), "snapshot-section-missing");
        assert!(bundle.require(SectionId::Tools).is_err());

        let temp = TempDir::new("load-require");
        temp.write("env.toml", "schema_version = 1\ncaptured_at = \"x\"\n");
        let bundle =
            RestoreBundle::load(temp.path(), &tuoen_platform::RealFileSystem).expect("读得回来");
        assert!(bundle.require(SectionId::Env).is_ok());
        assert!(bundle.require(SectionId::Path).is_err());
    }

    #[test]
    fn from_capture_copies_every_section_and_has_no_source() {
        let captured = CaptureBundle {
            schema: crate::capture::SchemaFile::new("2026-10-02T12:00:00Z", "0.1.0", vec![]),
            tools: None,
            path: Some(PathFile::new(
                "2026-10-02T12:00:00Z",
                PathBudgetRow {
                    raw_user_chars: 0,
                    raw_machine_chars: 0,
                    effective_chars: 0,
                    cliff: 8191,
                    remaining: 8191,
                    level: "ok".to_owned(),
                },
            )),
            env: Some(EnvFile::new("2026-10-02T12:00:00Z")),
            wsl: None,
            skipped: Some(SkippedFile::new("2026-10-02T12:00:00Z")),
        };
        let bundle = RestoreBundle::from_capture(&captured);
        assert_eq!(bundle.source, None, "本机侧没有'从哪来'这个问题");
        assert!(bundle.path.is_some() && bundle.env.is_some() && bundle.skipped.is_some());
        assert!(bundle.tools.is_none() && bundle.wsl.is_none());
        assert_eq!(
            bundle.sections_present(),
            vec![SectionId::Path, SectionId::Env]
        );
        assert_eq!(bundle.captured_at(), Some("2026-10-02T12:00:00Z"));
        assert_eq!(SCHEMA_VERSION, 1);
    }
}
