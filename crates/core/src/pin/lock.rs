//! `tuoen.lock` —— 上一次解析出来的**确定**版本（决策 114–116）。
//!
//! # 它是"上次解析的结论"，不是"另一个真值来源"
//!
//! 声明说的是"我要 24 那一档"，锁说的是"上次在这台机器上，24 落到了 24.19.0"。
//! 两者的关系是决策 116 的三条判据：
//!
//! * 规格对不上（`"24"` vs `"20"`）→ 拒绝启动，提示跑 `tuoen lock`；
//! * 只在声明里出现 → 拒绝（新加了工具还没锁）；
//! * 只在锁里出现 → 拒绝（工具被删了，锁还留着）。
//!
//! **没有 `--ignore-lock`。** 一条能被绕过的检查等于没有检查，而这里的逃生门
//! （重跑 `tuoen lock`）本来就一步就到。
//!
//! # 为什么 `to_toml` 是纯函数
//!
//! 决策 115：**两次调用逐字节相同**，且**没有时间戳**。理由是这份文件要进 git ——
//! 一个每次都变的文件会让每次 `tuoen lock` 都产生一个假的 diff，而假的 diff
//! 会训练人忽略真正的 diff（`AGENTS.md` 里"会写报告的代码"那几条的同一条教训）。
//! `"生成于某时刻"` 这种字段在这份文件里没有任何读者：谁想知道什么时候锁的，
//! 看 git 记录。

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::pin::resolve::ResolvedTool;
use crate::pin::{LOCK_FILE_NAME, PinError, PinFile};

/// 锁文件的 schema 版本。读的时候不认识的版本**一律不猜**（同 store 的记录）。
pub const LOCK_SCHEMA_VERSION: u32 = 1;

/// 锁里的一条：一个工具落到哪个版本、从哪来、放哪个目录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockTool {
    /// 工具 id（小写）。
    pub name: String,
    /// 声明时的规格原文（`tuoen.toml` 里写的那一串）。
    pub spec: String,
    /// 解析出来的确定版本号。
    pub version: String,
    /// 来源（`tuoen` 或检测来源的 kebab slug）。
    pub source: String,
    /// 第三方管理器名（`scoop` / `winget` / `nvm4w`…），我们自己装的没有。
    ///
    /// `skip_serializing_if`：TOML 没有 `null`，写一个空的 `manager = ""` 是**撒谎**
    /// （它会看起来像"管理器名叫空字符串"）。缺字段才是"没有"。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manager: Option<String>,
    /// 要前置进子进程 `PATH` 的目录。
    pub path: String,
    /// 制品哈希（`sha256:<hex>`）。第三方来源没有，于是也 `skip`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}

/// 一份 `tuoen.lock`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockFile {
    /// 见 [`LOCK_SCHEMA_VERSION`]。
    pub schema_version: u32,
    /// 逐条工具。序列化成 `[[tool]]`。
    ///
    /// `Vec` 而不是 `BTreeMap`：顺序是**解析顺序**（id 字典序），而且 TOML 的数组表格
    /// 读起来比一堆 `[tool.node]` 更像一份清单。`default` 让它容许一份空的锁。
    #[serde(default)]
    #[serde(rename = "tool")]
    pub tool: Vec<LockTool>,
}

/// 一条差异：声明与锁对不上的地方。
///
/// `declared` / `locked` 都可能缺席 —— 两种"只在一边出现"是决策 116 里真实存在的两态，
/// 合并成一个"对不上"会丢掉**该往哪边修**这个信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockMismatch {
    /// 工具 id。
    pub name: String,
    /// `tuoen.toml` 里的规格原文。
    pub declared: Option<String>,
    /// 锁里的规格原文。
    pub locked: Option<String>,
}

impl LockFile {
    /// 从一组解析结果造一份锁。
    #[must_use]
    pub fn new(tools: Vec<LockTool>) -> Self {
        Self {
            schema_version: LOCK_SCHEMA_VERSION,
            tool: tools,
        }
    }

    /// 从 [`resolve`](crate::pin::resolve) 的结果造一份锁。
    ///
    /// CLI 用它把"解析出来的那份"落成锁 —— `path` / `hash` 的形状只有这一处定义，
    /// 免得命令行那边再拼一遍字符串（拼错的那一份会静默写进锁里）。
    #[must_use]
    pub fn from_resolved(tools: &[ResolvedTool]) -> Self {
        let entries = tools
            .iter()
            .map(|tool| LockTool {
                name: tool.name.clone(),
                spec: tool.spec.clone(),
                version: tool.version.clone(),
                source: tool.source.clone(),
                manager: tool.manager.clone(),
                path: tool.path.to_string_lossy().into_owned(),
                hash: tool.hash.clone(),
            })
            .collect();
        Self::new(entries)
    }

    /// 从文本解析。
    ///
    /// # Errors
    ///
    /// * TOML 语法/字段类型错 → [`PinError::Parse`]；
    /// * `schema_version` 不是 [`LOCK_SCHEMA_VERSION`] → [`PinError::Parse`]（不猜）；
    /// * 同一个工具名出现两次（忽略大小写）→ [`PinError::Parse`]
    ///   （重复条目会让"这条锁说的是哪个版本"没有答案）；
    /// * 工具名为空 → [`PinError::Parse`]。
    pub fn parse(text: &str) -> Result<Self, PinError> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let lock: Self = toml::from_str(text).map_err(|error| PinError::Parse {
            path: None,
            message: format!("`{LOCK_FILE_NAME}` 读不动：{error}。跑 `tuoen lock` 重新生成一份。"),
        })?;
        if lock.schema_version != LOCK_SCHEMA_VERSION {
            return Err(PinError::Parse {
                path: None,
                message: format!(
                    "`{LOCK_FILE_NAME}` 的 schema_version 是 {}，本版本只认 v{LOCK_SCHEMA_VERSION} ——\
                     不猜。跑 `tuoen lock` 重新生成一份。",
                    lock.schema_version
                ),
            });
        }
        let mut seen: Vec<String> = Vec::new();
        for tool in &lock.tool {
            if tool.name.trim().is_empty() {
                return Err(PinError::Parse {
                    path: None,
                    message: format!("`{LOCK_FILE_NAME}` 里有一条没有名字的工具。"),
                });
            }
            let key = tool.name.to_ascii_lowercase();
            if seen.contains(&key) {
                return Err(PinError::Parse {
                    path: None,
                    message: format!(
                        "`{LOCK_FILE_NAME}` 里 `{key}` 出现了两次：这份锁说不清它到底是哪个版本。"
                    ),
                });
            }
            seen.push(key);
        }
        Ok(lock)
    }

    /// 读文件。**文件不存在不是错误** —— 返回 `Ok(None)`（决策 116 的第一条状态是
    /// "还没锁过"，那是提示跑 `tuoen lock`，不是报错）。
    ///
    /// # Errors
    ///
    /// 文件在那儿但读不动 → [`PinError::Io`] / [`PinError::Parse`]。
    pub fn load(path: &Path) -> Result<Option<Self>, PinError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                return Err(PinError::Parse {
                    path: Some(path.to_path_buf()),
                    message: format!("`{LOCK_FILE_NAME}` 不是 UTF-8 文本（{error}）。"),
                });
            }
            Err(error) => {
                return Err(PinError::Io {
                    path: path.to_path_buf(),
                    message: error.to_string(),
                });
            }
        };
        Self::parse(&text).map(Some).map_err(|error| match error {
            PinError::Parse {
                path: None,
                message,
            } => PinError::Parse {
                path: Some(path.to_path_buf()),
                message,
            },
            other => other,
        })
    }

    /// 序列化成 TOML。**纯函数、无时间戳、两次逐字节相同**（决策 115）。
    ///
    /// 不返回 `Result`：字段全是字符串与字符串数组，serde 在这里没有可失败的分支。
    /// 真有失败就是编程错误 —— 而降级成一个错误返回，意味着**每个**调用方都要处理
    /// 一个不可能发生的情况（`Store` 的记录文件同一套理由）。
    #[must_use]
    pub fn to_toml(&self) -> String {
        toml::to_string(self).expect("LockFile 全是字符串与字符串数组，TOML 序列化不可能失败")
    }

    /// 原子地写一份锁（先写 `<名字>.tmp` 再 `rename`）。
    ///
    /// # Errors
    ///
    /// 目录建不出来、写不进临时文件、`rename` 失败 → [`PinError::Io`]。
    pub fn write(path: &Path, lock: &LockFile) -> Result<(), PinError> {
        crate::pin::write_atomically(path, lock.to_toml().as_bytes())
    }

    /// 按名字取一条（大小写不敏感：工具 id 从声明到锁一路上都是小写，
    /// 但人可能手改过锁文件，那时"差一个大小写就找不到"是没必要的失望）。
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&LockTool> {
        self.tool
            .iter()
            .find(|tool| tool.name.eq_ignore_ascii_case(name))
    }

    /// 声明与锁的差异（决策 116 的三种）。
    ///
    /// **只比 `spec`，不比 `version`。** 版本落点变（24.19.0 → 24.19.1）是正常的事
    /// （上游发了新补丁，或者换机器重锁）；变的是**声明**才意味着"这个项目要换档了"，
    /// 而那正是必须重锁的时刻。比版本会让每次上游发版都拒绝启动。
    ///
    /// 返回顺序是 id 字典序（`BTreeMap` / `BTreeSet`），于是消息与 `--json` 都稳定。
    #[must_use]
    pub fn mismatches(&self, pin: &PinFile) -> Vec<LockMismatch> {
        let mut names: std::collections::BTreeSet<String> = pin.tools().keys().cloned().collect();
        for tool in &self.tool {
            names.insert(tool.name.to_ascii_lowercase());
        }
        let mut out = Vec::new();
        for name in names {
            let declared = pin.tools().get(&name).map(|spec| spec.raw().to_owned());
            let locked = self.get(&name).map(|tool| tool.spec.clone());
            if declared == locked {
                continue;
            }
            out.push(LockMismatch {
                name,
                declared,
                locked,
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pin::test_support::TempDir;
    use crate::pin::{PIN_FILE_NAME, PinFile};

    fn pin_of(tools: &[(&str, &str)]) -> PinFile {
        let mut text = String::from("[tools]\n");
        for (id, spec) in tools {
            text.push_str(&format!("{id} = \"{spec}\"\n"));
        }
        PinFile::parse(&text).expect("测试用的声明应当合法")
    }

    fn lock_tool(name: &str, spec: &str, version: &str) -> LockTool {
        LockTool {
            name: name.to_owned(),
            spec: spec.to_owned(),
            version: version.to_owned(),
            source: "tuoen".to_owned(),
            manager: None,
            path: format!("/tmp/tuoen/{name}/{version}/bin"),
            hash: Some("sha256:abc".to_owned()),
        }
    }

    #[test]
    fn to_toml_is_a_pure_function() {
        let lock = LockFile::new(vec![lock_tool("node", "24", "24.19.0")]);
        let first = lock.to_toml();
        let second = lock.to_toml();
        assert_eq!(first, second, "两次调用必须逐字节相同（决策 115）");
        // 没有时间戳：这份文件要进 git，每次变的文件会造出假的 diff。
        assert!(!first.contains("at ="), "{first}");
        assert!(!first.contains("T00:"), "{first}");
        assert!(!first.contains('\r'), "行尾必须是 LF：{first}");
    }

    #[test]
    fn to_toml_shape_is_pinned() {
        let lock = LockFile::new(vec![lock_tool("node", "24", "24.19.0")]);
        let expected = "\
schema_version = 1

[[tool]]
name = \"node\"
spec = \"24\"
version = \"24.19.0\"
source = \"tuoen\"
path = \"/tmp/tuoen/node/24.19.0/bin\"
hash = \"sha256:abc\"
";
        assert_eq!(lock.to_toml(), expected);
    }

    #[test]
    fn option_fields_are_absent_not_empty() {
        let mut tool = lock_tool("python", "3.12", "3.12.7");
        tool.manager = Some("scoop".to_owned());
        tool.hash = None;
        let text = LockFile::new(vec![tool]).to_toml();
        assert!(text.contains("manager = \"scoop\""), "{text}");
        assert!(!text.contains("hash"), "没有哈希就不该有这一行：{text}");
        assert!(!text.contains("= \"\""), "空串是撒谎：{text}");
    }

    #[test]
    fn round_trips_through_text() {
        let lock = LockFile::new(vec![
            lock_tool("node", "24", "24.19.0"),
            LockTool {
                manager: Some("nvm4w".to_owned()),
                hash: None,
                ..lock_tool("python", "3.12", "3.12.7")
            },
        ]);
        let text = lock.to_toml();
        let back = LockFile::parse(&text).expect("读回");
        assert_eq!(back, lock);
    }

    #[test]
    fn empty_lock_is_allowed_and_stable() {
        let lock = LockFile::new(Vec::new());
        let text = lock.to_toml();
        // 空表也写出来（`tool = []`）：一份"锁过、但什么都没锁"的文件是**说得清**的，
        // 而省略这个键之后，"没有键"与"是空表"在读的人眼里长得一样。
        assert_eq!(text.trim(), "schema_version = 1\ntool = []");
        assert_eq!(LockFile::parse(&text).expect("读回"), lock);
    }

    #[test]
    fn unknown_schema_is_refused() {
        let error = LockFile::parse("schema_version = 2\n").expect_err("不认识的版本要拒");
        assert_eq!(error.code(), "pin-parse");
        assert!(
            error.message().contains("tuoen lock"),
            "{}",
            error.message()
        );
    }

    #[test]
    fn duplicate_tool_names_are_refused() {
        let text = "\
schema_version = 1

[[tool]]
name = \"node\"
spec = \"24\"
version = \"24.19.0\"
source = \"tuoen\"
path = \"/a\"

[[tool]]
name = \"Node\"
spec = \"20\"
version = \"20.11.0\"
source = \"tuoen\"
path = \"/b\"
";
        let error = LockFile::parse(text).expect_err("重名要拒");
        assert_eq!(error.code(), "pin-parse");
        assert!(error.message().contains("两次"), "{}", error.message());
    }

    #[test]
    fn get_is_case_insensitive() {
        let lock = LockFile::new(vec![lock_tool("node", "24", "24.19.0")]);
        assert!(lock.get("node").is_some());
        assert!(lock.get("NODE").is_some());
        assert!(lock.get("python").is_none());
    }

    #[test]
    fn mismatches_covers_the_three_cases_116() {
        // 情形一：规格不同。
        let lock = LockFile::new(vec![lock_tool("node", "24", "24.19.0")]);
        let pin = pin_of(&[("node", "20")]);
        let found = lock.mismatches(&pin);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(
            found[0],
            LockMismatch {
                name: "node".to_owned(),
                declared: Some("20".to_owned()),
                locked: Some("24".to_owned()),
            }
        );

        // 情形二：只在声明里（新加的工具还没锁）。
        let pin = pin_of(&[("node", "24"), ("python", "3.12")]);
        let found = lock.mismatches(&pin);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].declared.as_deref(), Some("3.12"));
        assert_eq!(found[0].locked, None);
        assert!(found[0].name == "python"); // 字典序也顺带钉住

        // 情形三：只在锁里（工具从声明里删掉了，锁还留着）。
        // 声明只剩 node，锁里还有 java：java 是"只在锁里"的那一条。
        let lock = LockFile::new(vec![
            lock_tool("node", "24", "24.19.0"),
            lock_tool("java", "21", "21.0.2"),
        ]);
        let pin = pin_of(&[("node", "24")]);
        let found = lock.mismatches(&pin);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].declared, None);
        assert_eq!(found[0].locked.as_deref(), Some("21"));
        assert_eq!(found[0].name, "java");

        // 两边各自多出一条时，两条都要报（顺序 = id 字典序）。
        let pin = pin_of(&[("node", "24"), ("python", "3.12")]);
        let found = lock.mismatches(&pin);
        let names: Vec<&str> = found.iter().map(|item| item.name.as_str()).collect();
        assert_eq!(names, vec!["java", "python"]);
    }

    #[test]
    fn mismatches_ignores_version_drift_but_not_spec_drift() {
        // 版本落点变了（24.19.0 装的，锁里写 24.19.1）不算差异：只有**声明**变了才算。
        let lock = LockFile::new(vec![lock_tool("node", "24", "24.19.1")]);
        let pin = pin_of(&[("node", "24")]);
        assert!(lock.mismatches(&pin).is_empty());

        // 但声明写成了 `v24`：文本不同 → 算差异（保守：让用户重锁一次，别猜等价）。
        let pin = pin_of(&[("node", "v24")]);
        assert_eq!(lock.mismatches(&pin).len(), 1);

        // 两边都空 → 没有差异。
        assert!(
            LockFile::new(Vec::new())
                .mismatches(&pin_of(&[]))
                .is_empty()
        );
    }

    #[test]
    fn load_returns_none_when_absent() {
        let temp = TempDir::new("lock-absent");
        assert!(
            LockFile::load(&temp.join("tuoen.lock"))
                .expect("不存在不是错误")
                .is_none()
        );
    }

    #[test]
    fn write_replaces_in_place_and_load_reads_it_back() {
        let temp = TempDir::new("lock-write");
        let path = temp.join("tuoen.lock");
        let first = LockFile::new(vec![lock_tool("node", "24", "24.19.0")]);
        LockFile::write(&path, &first).expect("第一次写");
        assert_eq!(LockFile::load(&path).expect("读回").expect("应当有"), first);

        let second = LockFile::new(vec![lock_tool("python", "3.12", "3.12.7")]);
        LockFile::write(&path, &second).expect("覆盖写");
        assert_eq!(
            LockFile::load(&path).expect("读回").expect("应当有"),
            second
        );

        // 目录里只该有锁文件本身（临时文件必须已经被 rename 收走）。
        let names: Vec<String> = std::fs::read_dir(temp.path())
            .expect("列目录")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["tuoen.lock".to_owned()], "{names:?}");
    }

    #[test]
    fn write_creates_the_parent_directory() {
        let temp = TempDir::new("lock-mkdir");
        let path = temp.join("nested").join("tuoen.lock");
        LockFile::write(&path, &LockFile::new(Vec::new())).expect("建目录再写");
        assert!(path.exists());
    }

    #[test]
    fn from_resolved_keeps_the_resolution_shape() {
        let resolved = ResolvedTool {
            name: "node".to_owned(),
            spec: "24".to_owned(),
            version: "24.19.0".to_owned(),
            source: "tuoen".to_owned(),
            manager: None,
            path: std::path::PathBuf::from("C:\\tuoen\\store\\node\\versions\\24.19.0\\bin"),
            hash: Some("sha256:deadbeef".to_owned()),
        };
        let lock = LockFile::from_resolved(std::slice::from_ref(&resolved));
        assert_eq!(lock.tool.len(), 1);
        assert_eq!(lock.tool[0].name, resolved.name);
        assert_eq!(lock.tool[0].version, resolved.version);
        assert_eq!(
            lock.tool[0].path,
            "C:\\tuoen\\store\\node\\versions\\24.19.0\\bin"
        );
        assert_eq!(lock.tool[0].hash.as_deref(), Some("sha256:deadbeef"));
        assert_eq!(lock.tool[0].manager, None);
        assert_eq!(lock.schema_version, LOCK_SCHEMA_VERSION);
        // 常量在两侧都对得上（写错一处会让 CLI 与 core 各说各话）。
        assert_eq!(LOCK_FILE_NAME, "tuoen.lock");
        assert_eq!(PIN_FILE_NAME, "tuoen.toml");
    }
}
