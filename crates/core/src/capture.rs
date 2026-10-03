//! `tuoen capture` —— 把整机开发状态读成机器可读的 `tuoen.d/`。
//!
//! # 这一层在整个项目里的位置
//!
//! `capture` 是 L1 的第一步：它**只读**整机状态并写成文件，一个字节都不改。
//! `doctor` 读它（或读实时状态）做体检，`restore` 把另一台机器的它还原过来。
//! 所以这里最重要的东西不是"能读出多少"，而是**读出来的东西能不能被相信**：
//!
//! * 每条工具记录都带**来源**与**置信度** —— 不允许出现没有来源的条目；
//! * `PATH` 存的是**结构**（所有者、顺序、长度预算、reparse 形状），不是一串字符串；
//! * 所有"我们说不准"的地方都是**三态**而不是布尔（见 [`Existence`]）；
//! * 看见了但没写的东西，必须在 [`SkippedFile`] 里说出来 —— **静默跳过是 bug**。
//!
//! # 分文件
//!
//! ```text
//! tuoen.d/
//!   ├─ schema.toml    这一份快照是谁写的、包含哪些 section
//!   ├─ tools.toml     工具与版本
//!   ├─ path.toml      PATH 结构
//!   ├─ env.toml       持久环境变量（用户级 + 机器级）
//!   ├─ wsl.toml       WSL 发行版与实际 vhdx 路径
//!   ├─ globals.toml   全局包清单（npm / pip）
//!   ├─ configs.toml   配置文件清单（只哈希、不存内容）
//!   └─ skipped.toml   看见了但没写的东西，以及为什么
//! ```
//!
//! 分文件不是为了整齐，而是为了**选择性操作**："`PATH` 变了"与"Java 版本变了"
//! 是两个独立 diff，`restore --only path` 因此在**格式层面**天然成立。
//!
//! # 幂等
//!
//! 同一台机器上连着跑两次，**除 `captured_at` 以外逐字节相同**。
//! 时间戳由调用方传进来（[`CaptureOptions::captured_at`]），所以这条性质在测试里
//! 可以要求"完全逐字节相同"，而不是"忽略某个字段之后相同"。
//! 拿到两份真实的输出比对时用 [`without_timestamp`]。
//!
//! # 测试 seam
//!
//! 全部机器访问都经过 [`tuoen_platform`] 的注入式适配器（文件系统 / 注册表 /
//! 环境块 / 进程），所以采集器可以在**假机器**上跑到字节级确定
//! （[`test_support::CaptureFixture`]）。`--json` 与磁盘副作用用 CLI 进程边界测。

// `doctor` 是同一个 crate 里的兄弟模块，它的事实就是这四个文件形状 ——
// 所以这两个模块对 crate 内可见（对外仍然是私有的：`pub use files::{…}` 才是门面）。
pub(crate) mod collect;
pub(crate) mod files;
pub mod secrets;
pub mod test_support;

pub use files::{
    ConfigRow, ConfigsFile, EntryRefRow, EnvFile, EnvVarRow, Existence, GitFacts, GlobalPackage,
    GlobalRow, GlobalsFile, PathBudgetRow, PathFile, PathRow, SCHEMA_VERSION, SchemaFile,
    SkipEntry, SkippedFile, TargetExistence, ToolRow, ToolsFile, WslFile, WslRow,
};

use std::path::{Path, PathBuf};

use crate::detect::DetectContext;

/// `tuoen.d/` 里的一个 section。
///
/// 与文件名一一对应，且**顺序固定**（`write_bundle` 按这个顺序写），因为文件系统的
/// 写入顺序会影响目录项的 mtime —— 而"两次跑出来一样"是我们承诺的东西。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Section {
    /// `tools.toml`。
    Tools,
    /// `path.toml`。
    Path,
    /// `env.toml`。
    Env,
    /// `wsl.toml`。
    Wsl,
    /// `globals.toml`。
    Globals,
    /// `configs.toml`。
    Configs,
}

impl Section {
    /// 全部 section，**顺序即写入顺序**。
    ///
    /// 新 section **追加在末尾**（决策 165）：顺序决定 `schema.toml` 里 `sections`
    /// 数组的顺序与磁盘写入顺序，而"同一台机器跑两次逐字节相同"依赖顺序稳定 ——
    /// 追加在末尾让**老快照**的顺序一个字都不变。
    pub const ALL: [Self; 6] = [
        Self::Tools,
        Self::Path,
        Self::Env,
        Self::Wsl,
        Self::Globals,
        Self::Configs,
    ];

    /// 稳定 slug（`--json` 与 `schema.toml` 里的取值，不本地化）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tools => "tools",
            Self::Path => "path",
            Self::Env => "env",
            Self::Wsl => "wsl",
            Self::Globals => "globals",
            Self::Configs => "configs",
        }
    }

    /// 它落在哪个文件里。
    #[must_use]
    pub const fn file_name(self) -> &'static str {
        match self {
            Self::Tools => "tools.toml",
            Self::Path => "path.toml",
            Self::Env => "env.toml",
            Self::Wsl => "wsl.toml",
            Self::Globals => "globals.toml",
            Self::Configs => "configs.toml",
        }
    }

    /// 从 slug 解析（`--only` 的取值走 clap 的枚举，这里给库调用方用）。
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == name)
    }
}

/// 一次捕获要做什么。
#[derive(Debug, Clone)]
pub struct CaptureOptions {
    /// 写到哪个目录（就是 `tuoen.d/` 本身）。
    pub out_dir: PathBuf,
    /// 只捕获这些 section。**空 = 全部**（而不是"什么都不捕获"——
    /// 那个语义会让一个忘了传参的调用安静地什么都不做）。
    pub sections: Vec<Section>,
    /// 捕获时间（RFC 3339）。由调用方给，这样测试能钉住它。
    pub captured_at: String,
    /// 我们自己的根目录（存储根、shim 目录）。
    ///
    /// **它只用来判 `owner = "tuoen"`**：哪些 `PATH` 条目是我们自己放上去的，
    /// 决定了还原时哪些可以动。传空就是"这台机器上没有我们"，不是错误。
    pub tuoen_roots: Vec<PathBuf>,
}

impl CaptureOptions {
    /// 全部 section 写进一个目录。
    #[must_use]
    pub fn all(out_dir: impl Into<PathBuf>, captured_at: &str) -> Self {
        Self {
            out_dir: out_dir.into(),
            sections: Vec::new(),
            captured_at: captured_at.to_owned(),
            tuoen_roots: Vec::new(),
        }
    }

    /// 只捕获这些 section。
    #[must_use]
    pub fn only(out_dir: impl Into<PathBuf>, captured_at: &str, sections: Vec<Section>) -> Self {
        Self {
            sections,
            ..Self::all(out_dir, captured_at)
        }
    }

    /// 加一个"这是我们自己的目录"的根。
    #[must_use]
    pub fn with_tuoen_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.tuoen_roots.push(root.into());
        self
    }

    /// 实际要捕获的 section，**去重并排序**（顺序稳定才能比逐字节）。
    #[must_use]
    pub fn effective_sections(&self) -> Vec<Section> {
        let mut sections = if self.sections.is_empty() {
            Section::ALL.to_vec()
        } else {
            self.sections.clone()
        };
        sections.sort();
        sections.dedup();
        sections
    }
}

/// 一次捕获的全部产物。
///
/// 每个 `Option` 都是"这次没捕获这个 section"，**不是**"这台机器上什么都没有"——
/// 两者的区别靠 [`SchemaFile::sections`] 保留。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureBundle {
    /// `schema.toml`，永远有。
    pub schema: SchemaFile,
    /// `tools.toml`。
    pub tools: Option<ToolsFile>,
    /// `path.toml`。
    pub path: Option<PathFile>,
    /// `env.toml`。
    pub env: Option<EnvFile>,
    /// `wsl.toml`。
    pub wsl: Option<WslFile>,
    /// `globals.toml`。
    pub globals: Option<GlobalsFile>,
    /// `configs.toml`。
    pub configs: Option<ConfigsFile>,
    /// `skipped.toml`。**只有真的扫过环境变量或配置文件时才有** —— 没扫过就没有
    /// "跳过了什么"可报，凭空写一个空清单会让"这份快照不完整"看起来像"没有东西被跳过"。
    pub skipped: Option<SkippedFile>,
}

impl CaptureBundle {
    /// 这次会写出哪些文件（相对 `tuoen.d/`），顺序即写入顺序。
    #[must_use]
    pub fn file_names(&self) -> Vec<&'static str> {
        let mut names = vec!["schema.toml"];
        if self.tools.is_some() {
            names.push("tools.toml");
        }
        if self.path.is_some() {
            names.push("path.toml");
        }
        if self.env.is_some() {
            names.push("env.toml");
        }
        if self.wsl.is_some() {
            names.push("wsl.toml");
        }
        if self.globals.is_some() {
            names.push("globals.toml");
        }
        if self.configs.is_some() {
            names.push("configs.toml");
        }
        if self.skipped.is_some() {
            names.push("skipped.toml");
        }
        names
    }
}

/// 捕获失败的原因。
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    /// 写文件失败。
    #[error("写 `{path}` 失败：{source}")]
    Io {
        /// 出问题的路径。
        path: PathBuf,
        /// 底层错误。
        #[source]
        source: std::io::Error,
    },
    /// 序列化失败。**这不该发生**：它意味着某个字段的形状 TOML 表达不了
    /// （最常见的是漏了 `skip_serializing_if` 的 `Option`）。
    #[error("把 `{file}` 序列化成 TOML 失败：{source}")]
    Toml {
        /// 文件名（相对 `tuoen.d/`）。
        file: &'static str,
        /// 底层错误。
        #[source]
        source: toml::ser::Error,
    },
}

impl CaptureError {
    /// `--json` 里的稳定错误码，**不本地化**。
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Io { .. } => "capture-io",
            Self::Toml { .. } => "capture-serialize",
        }
    }
}

/// 捕获整机状态。**只读**：一个字节都不写（写盘是 [`write_bundle`] 的事）。
///
/// 分开的理由很实际：`--dry-run` 与"先看一眼再决定"都要能拿到 bundle 而不落盘，
/// 而"计划与执行走同一套代码"是本项目的既定规矩。
///
/// # Errors
///
/// 目前这个函数不返回错误（它只读）。返回 `Result` 是为了**保留这个签名**：
/// 采集器开始读某些会失败的东西时（WSL 注册表权限、损坏的键），失败的形状
/// 不该在那一天才被发明出来。
pub fn capture(
    ctx: &DetectContext<'_>,
    opts: &CaptureOptions,
) -> Result<CaptureBundle, CaptureError> {
    let sections = opts.effective_sections();
    let schema = SchemaFile::new(
        &opts.captured_at,
        env!("CARGO_PKG_VERSION"),
        sections.iter().map(|s| s.as_str().to_owned()).collect(),
    );

    let mut bundle = CaptureBundle {
        schema,
        tools: None,
        path: None,
        env: None,
        wsl: None,
        globals: None,
        configs: None,
        skipped: None,
    };

    // 跳过项**攒起来最后写一次**：`skipped.toml` 是一句跨 section 的话（"这份快照
    // 完整吗"），而它的触发条件是"`env` 或 `configs` 被扫过"（决策 179）。
    // 先攒后写也让写入顺序与 section 的处理顺序无关 —— 顺序稳定才能比逐字节。
    let mut skips: Vec<SkipEntry> = Vec::new();
    let mut scanned_skips = false;

    for section in &sections {
        match section {
            Section::Tools => {
                bundle.tools = Some(collect::collect_tools(ctx, &opts.captured_at));
            }
            Section::Path => {
                bundle.path = Some(collect::collect_path(
                    ctx,
                    &opts.captured_at,
                    &opts.tuoen_roots,
                ));
            }
            Section::Env => {
                let (env, env_skips) = collect::collect_env(ctx, &opts.captured_at);
                bundle.env = Some(env);
                scanned_skips = true;
                skips.extend(env_skips);
            }
            Section::Wsl => {
                bundle.wsl = Some(collect::collect_wsl(ctx, &opts.captured_at));
            }
            Section::Globals => {
                bundle.globals = Some(collect::collect_globals(ctx, &opts.captured_at));
            }
            Section::Configs => {
                let (configs, config_skips) = collect::collect_configs(ctx, &opts.captured_at);
                bundle.configs = Some(configs);
                scanned_skips = true;
                skips.extend(config_skips);
            }
        }
    }

    if scanned_skips {
        // 扫过就写这个文件，**空清单也是信息**：它说明"扫了，没跳过东西"。
        // 没扫过（`--only path`）就没有这个文件，因为那时"跳过了什么"无从谈起。
        let mut file = SkippedFile::new(&opts.captured_at);
        for entry in skips {
            file.push(entry);
        }
        bundle.skipped = Some(file);
    }

    Ok(bundle)
}

/// 把 bundle 写成文件，返回真的落盘的那些路径（顺序即写入顺序）。
///
/// # Errors
///
/// 建目录失败、写文件失败、或者某个文件序列化不出来。
pub fn write_bundle(bundle: &CaptureBundle, out_dir: &Path) -> Result<Vec<PathBuf>, CaptureError> {
    std::fs::create_dir_all(out_dir).map_err(|source| CaptureError::Io {
        path: out_dir.to_path_buf(),
        source,
    })?;

    let mut written = Vec::new();
    for (name, text) in render(bundle)? {
        let path = out_dir.join(name);
        std::fs::write(&path, text).map_err(|source| CaptureError::Io {
            path: path.clone(),
            source,
        })?;
        written.push(path);
    }
    Ok(written)
}

/// 把 bundle 渲染成 `(文件名, 文本)`，顺序即写入顺序。
///
/// **单独抽出来是为了能被测到**：`--dry-run`（以及"先看看会写出什么"）需要文本
/// 而不落盘，而"计划与执行走同一套代码"是本项目的既定规矩。
///
/// # Errors
///
/// 序列化失败。
pub fn render(bundle: &CaptureBundle) -> Result<Vec<(&'static str, String)>, CaptureError> {
    fn toml_of<T: serde::Serialize>(file: &'static str, value: &T) -> Result<String, CaptureError> {
        toml::to_string_pretty(value).map_err(|source| CaptureError::Toml { file, source })
    }

    let mut out = Vec::with_capacity(8);
    out.push(("schema.toml", toml_of("schema.toml", &bundle.schema)?));
    if let Some(tools) = &bundle.tools {
        out.push(("tools.toml", toml_of("tools.toml", tools)?));
    }
    if let Some(path) = &bundle.path {
        out.push(("path.toml", toml_of("path.toml", path)?));
    }
    if let Some(env) = &bundle.env {
        out.push(("env.toml", toml_of("env.toml", env)?));
    }
    if let Some(wsl) = &bundle.wsl {
        out.push(("wsl.toml", toml_of("wsl.toml", wsl)?));
    }
    if let Some(globals) = &bundle.globals {
        out.push(("globals.toml", toml_of("globals.toml", globals)?));
    }
    if let Some(configs) = &bundle.configs {
        out.push(("configs.toml", toml_of("configs.toml", configs)?));
    }
    if let Some(skipped) = &bundle.skipped {
        out.push(("skipped.toml", toml_of("skipped.toml", skipped)?));
    }
    Ok(out)
}

/// 去掉时间戳行，用来比较两次捕获。
///
/// 作用在**文本**上而不是反序列化后的结构上：反序列化会顺手抹掉"我们写出去的
/// 到底是什么字节"这个信息，而幂等要求的正是字节。
///
/// 同时认 TOML 的 `captured_at` 与 `--json` 的 `capturedAt`。
#[must_use]
pub fn without_timestamp(text: &str) -> String {
    text.lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !trimmed.starts_with("captured_at") && !trimmed.starts_with("\"capturedAt\"")
        })
        .map(|line| {
            // 保留行尾：比较的是"去掉时间戳之后还一样不一样"，不是"排版一样不一样"。
            let mut owned = line.to_owned();
            owned.push('\n');
            owned
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_section_list_means_everything() {
        let opts = CaptureOptions::all("out", "2026-10-02T12:00:00Z");
        assert_eq!(opts.effective_sections(), Section::ALL.to_vec());
    }

    #[test]
    fn sections_are_deduped_and_sorted() {
        let opts = CaptureOptions::only(
            "out",
            "2026-10-02T12:00:00Z",
            vec![Section::Wsl, Section::Path, Section::Wsl],
        );
        assert_eq!(
            opts.effective_sections(),
            vec![Section::Path, Section::Wsl],
            "顺序稳定才能比逐字节"
        );
    }

    #[test]
    fn section_slugs_and_file_names_agree() {
        for section in Section::ALL {
            assert_eq!(Section::parse(section.as_str()), Some(section));
            assert_eq!(
                section.file_name(),
                format!("{}.toml", section.as_str()),
                "slug 与文件名必须是同一个词 —— 两套名字迟早会漂移"
            );
        }
        assert_eq!(Section::parse("tool"), None, "`tool` 不是 `tools`");
    }

    #[test]
    fn the_timestamp_stripper_only_touches_the_timestamp() {
        let before = "schema_version = 1\ncaptured_at = \"A\"\n[sections]\n";
        let after = "schema_version = 1\ncaptured_at = \"B\"\n[sections]\n";
        assert_eq!(without_timestamp(before), without_timestamp(after));
        assert!(
            without_timestamp(before).contains("schema_version = 1"),
            "别的东西不许被删掉"
        );
        assert!(without_timestamp(before).contains("[sections]"));
        // `captured_at` 只能作为键名被删，不能把值里含这几个字的东西一起删掉。
        let quoted = "note = \"captured_at 是键名\"\ncaptured_at = \"X\"\n";
        assert!(without_timestamp(quoted).contains("note ="));
    }

    #[test]
    fn json_timestamps_are_stripped_too() {
        let a = "{\n  \"capturedAt\": \"A\",\n  \"tools\": []\n}\n";
        let b = "{\n  \"capturedAt\": \"B\",\n  \"tools\": []\n}\n";
        assert_eq!(without_timestamp(a), without_timestamp(b));
        assert!(without_timestamp(a).contains("\"tools\""));
    }
}
