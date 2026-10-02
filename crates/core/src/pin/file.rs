//! `tuoen.toml` —— **项目声明**要哪些工具、各要哪个版本的规格。
//!
//! # 声明的语义
//!
//! ```toml
//! [project]
//! name = "我的后端"
//!
//! [tools]
//! node = "24"
//! python = "3.12"
//! ```
//!
//! `[project]` 是可选的（只有 `name`，用来在消息里称呼这个项目）；`[tools]` 的键是工具 id
//! （**小写**），值是版本规格（[`VersionSpec`]）。
//!
//! # 解析的三条"拒绝"（每一条都对应一次"看起来正常的错")
//!
//! 1. **不认识的段名直接报错。** 把 `[tools]` 写成 `[tool]` 之后，如果按"没见过的段就忽略"
//!    处理，这份文件会变成"什么都没 pin" —— 用户以为切了版本，实际什么都没切。
//!    同样地，`[tools]` 里出现 tuoen 不认识的 id 会在 [`resolve`](crate::pin::resolve) 里
//!    报 [`PinError::UnknownTool`]（不是在这里：这里只管**形状**，不认识 id 是判断）。
//! 2. **版本规格必须是字符串。** TOML 的数字会把 `24.20` 变成 `24.2`
//!    （浮点的尾零不是信息），而 `node = 24` 与 `node = "24"` 看起来一样、含义不同。
//!    所以数字一律拒掉，消息里给出加引号的写法。
//! 3. **同一个 id 的两种拼法算错。** `Node` 与 `node` 归一化之后撞在一起 ——
//!    静默让后者覆盖前者，等于凭空吃掉一行声明。
//!
//! # BOM 只在**解析**时剥掉，指纹里照算
//!
//! 带 BOM 的 TOML 文本是一份合法的文件（Windows 编辑器默认这么存），
//! 所以解析前剥一个 `\u{feff}`。但**指纹不同**：[`normalize_for_fingerprint`] 不动 BOM，
//! 于是"加了个 BOM"仍然是一次内容变化、仍然要重新信任（决策 117）。
//! 两件事看起来矛盾，其实是一条规则：**解析要宽容，信任要严格**。
//!
//! [`normalize_for_fingerprint`]: crate::pin::trust::normalize_for_fingerprint

use std::collections::BTreeMap;
use std::path::Path;

use crate::pin::spec::VersionSpec;
use crate::pin::{PIN_FILE_NAME, PinError};

/// 一份 `tuoen.toml`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinFile {
    /// 项目名（可选，纯展示）。
    project: Option<String>,
    /// 工具 id（小写）→ 版本规格。`BTreeMap`：迭代顺序是 id 字典序，
    /// 而字典序正是子进程 `PATH` 前置目录的顺序（决策 119）—— 顺序不是巧合，是同一件事。
    tools: BTreeMap<String, VersionSpec>,
}

impl PinFile {
    /// 从文本解析。
    ///
    /// # Errors
    ///
    /// * 空的/只有注释的文件 → [`PinError::Parse`]（一份"存在但什么都没说"的
    ///   声明文件几乎总是误操作：多半是编辑器刚建的空文件）；
    /// * TOML 语法错误、不认识的段、工具值不是字符串、规格非法 → [`PinError::Parse`]。
    pub fn parse(text: &str) -> Result<Self, PinError> {
        // 解析时剥 BOM（见模块文档）。指纹不剥 —— 那是另一条路。
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        if text.trim().is_empty() {
            return Err(PinError::Parse {
                path: None,
                message: format!(
                    "`{PIN_FILE_NAME}` 是空的：至少要写一个 `[tools]` 段，\
                     比如 `[tools]` 下面一行 `node = \"24\"`。"
                ),
            });
        }
        let value: toml::Value = toml::from_str(text).map_err(|error| PinError::Parse {
            path: None,
            message: format!("TOML 语法错误：{error}"),
        })?;
        let table = match value {
            toml::Value::Table(table) => table,
            other => {
                return Err(PinError::Parse {
                    path: None,
                    message: format!(
                        "`{PIN_FILE_NAME}` 的顶层必须是一张表（一行行 `键 = 值`），\
                         现在读到的是 {}。",
                        kind_of(&other)
                    ),
                });
            }
        };

        let mut project = None;
        let mut tools = BTreeMap::new();
        let mut saw_tools = false;
        for (key, value) in table {
            match key.as_str() {
                "project" => {
                    // **形状以票据为准**：票据 §1 写的是 `[project]` 段 + `name = "my-project"`。
                    // 第一版把它实现成顶层 `project = "我的项目"`（一个字符串），代码与自己的
                    // 文档自洽、用例也全绿 —— 而真机验收第一次跑就红了，因为**用户会照票据写**。
                    // 这是决策 92 那条教训的第四次：字段的语义以票据为准。
                    let entries = match value {
                        toml::Value::Table(entries) => entries,
                        other => {
                            return Err(PinError::Parse {
                                path: None,
                                message: format!(
                                    "`project` 必须是一张表：写成 `[project]` 段 + \
                                     `name = \"我的项目\"`，现在读到的是 {}。",
                                    kind_of(&other)
                                ),
                            });
                        }
                    };
                    for (field, value) in entries {
                        match field.as_str() {
                            "name" => {
                                let name = match value {
                                    toml::Value::String(name) => name,
                                    other => {
                                        return Err(PinError::Parse {
                                            path: None,
                                            message: format!(
                                                "`[project] name` 必须是字符串，\
                                                 写成 `name = \"我的项目\"`；现在读到的是 {}。",
                                                kind_of(&other)
                                            ),
                                        });
                                    }
                                };
                                if name.trim().is_empty() {
                                    return Err(PinError::Parse {
                                        path: None,
                                        message: "`[project] name` 是空字符串：填个名字，\
                                                  或者把整个 `[project]` 段删掉。"
                                            .to_owned(),
                                    });
                                }
                                project = Some(name);
                            }
                            other => {
                                return Err(PinError::Parse {
                                    path: None,
                                    message: format!(
                                        "`[project]` 段里不认识 `{other}`：目前只认 `name`。\
                                         段里的键名写错时不能当成「没有声明」——\
                                         那会让你以为写了项目名，其实没写。"
                                    ),
                                });
                            }
                        }
                    }
                }
                "tools" => {
                    saw_tools = true;
                    let entries = match value {
                        toml::Value::Table(entries) => entries,
                        other => {
                            return Err(PinError::Parse {
                                path: None,
                                message: format!(
                                    "`[tools]` 必须是一张表（一个工具一行 `node = \"24\"`），\
                                     现在读到的是 {}。",
                                    kind_of(&other)
                                ),
                            });
                        }
                    };
                    for (id, value) in entries {
                        let raw = match value {
                            toml::Value::String(raw) => raw,
                            other => {
                                return Err(PinError::Parse {
                                    path: None,
                                    message: format!(
                                        "工具 `{id}` 的版本规格必须是字符串，写成 `{id} = \"24\"`；\
                                         现在读到的是 {} —— TOML 里的数字会把 `24.20` 变成 `24.2`，\
                                         所以这里不认数字。",
                                        kind_of(&other)
                                    ),
                                });
                            }
                        };
                        let key = id.trim().to_ascii_lowercase();
                        if key.is_empty() {
                            return Err(PinError::Parse {
                                path: None,
                                message: "`[tools]` 里有一个空的工具名：补上 id，或者删掉这一行。"
                                    .to_owned(),
                            });
                        }
                        if let Some(existing) = tools.get(&key) {
                            return Err(PinError::Parse {
                                path: None,
                                message: format!(
                                    "工具 `{key}` 写了两遍（`{id}` 与另一个大小写不同的拼法，\
                                     规格分别是 `{existing}` 与 `{raw}`）：只留一行。"
                                ),
                            });
                        }
                        let spec = VersionSpec::parse(&raw).map_err(|error| PinError::Parse {
                            path: None,
                            message: format!("工具 `{id}`：{}", error.message()),
                        })?;
                        tools.insert(key, spec);
                    }
                }
                other => {
                    return Err(PinError::Parse {
                        path: None,
                        message: format!(
                            "`{PIN_FILE_NAME}` 里不认识 `{other}` 这个段：目前只认 `project` 与 `[tools]`。\
                             段名写错时不能当成「没有声明」—— 那会让你以为切了版本，其实什么都没切。"
                        ),
                    });
                }
            }
        }
        // 一份"存在但什么都没说"的文件（空的、或者只有注释）几乎总是误操作：
        // 它会让 `tuoen shell` 一个目录都不前置，而用户以为自己 pin 了东西。
        // 真不需要 pin 的项目，正确做法是**没有**这份文件。
        if !saw_tools && project.is_none() {
            return Err(PinError::Parse {
                path: None,
                message: format!(
                    "`{PIN_FILE_NAME}` 里既没有 `project` 也没有 `[tools]` —— 它什么都没声明。\
                     加上 `[tools]` 段（每个工具一行 `node = \"24\"`），或者把这份文件删掉。"
                ),
            });
        }
        Ok(Self { project, tools })
    }

    /// 读文件再解析。
    ///
    /// # Errors
    ///
    /// * 文件不存在 → [`PinError::MissingPin`]（**不是** `Ok(None)`：
    ///   "这个目录没有 pin"与"pin 是空的"在 CLI 里是两条不同的出路，见决策 116 的邻居）；
    /// * 不是 UTF-8 → [`PinError::Parse`]（它是一份文本文件，编码不对就是内容不对）；
    /// * 其他 I/O 失败 → [`PinError::Io`]。
    pub fn load(path: &Path) -> Result<Self, PinError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(PinError::MissingPin {
                    path: path.to_path_buf(),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                return Err(PinError::Parse {
                    path: Some(path.to_path_buf()),
                    message: format!(
                        "`{PIN_FILE_NAME}` 不是 UTF-8 文本（{error}）——\
                         多半是用了别的编码存的，用编辑器另存成 UTF-8。"
                    ),
                });
            }
            Err(error) => {
                return Err(PinError::Io {
                    path: path.to_path_buf(),
                    message: error.to_string(),
                });
            }
        };
        // 解析失败时补上文件路径：文本级的错误知道自己不对，但不知道自己在哪个文件里。
        Self::parse(&text).map_err(|error| match error {
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

    /// 项目名。
    #[must_use]
    pub fn project(&self) -> Option<&str> {
        self.project.as_deref()
    }

    /// 工具 id（小写）→ 版本规格。
    #[must_use]
    pub fn tools(&self) -> &BTreeMap<String, VersionSpec> {
        &self.tools
    }
}

/// TOML 值的种类（只用于错误消息：用户写错类型时最想知道的是"你读成了什么"）。
fn kind_of(value: &toml::Value) -> &'static str {
    match value {
        toml::Value::String(_) => "字符串",
        toml::Value::Integer(_) => "整数",
        toml::Value::Float(_) => "浮点数",
        toml::Value::Boolean(_) => "布尔值",
        toml::Value::Datetime(_) => "日期时间",
        toml::Value::Array(_) => "数组",
        toml::Value::Table(_) => "一张表",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pin::test_support::TempDir;

    #[test]
    fn parses_the_documented_shape() {
        let pin = PinFile::parse(
            r#"
[project]
name = "我的后端"

[tools]
node = "24"
python = "3.12"
"#,
        )
        .expect("应当能解析");
        assert_eq!(pin.project(), Some("我的后端"));
        let ids: Vec<&str> = pin.tools().keys().map(String::as_str).collect();
        assert_eq!(ids, vec!["node", "python"]); // 字典序
        assert!(pin.tools()["node"].matches("24.19.0"));
    }

    #[test]
    fn project_is_optional_and_tools_may_be_absent() {
        let pin = PinFile::parse("[tools]\n").expect("空的 [tools] 是合法的");
        assert_eq!(pin.project(), None);
        assert!(pin.tools().is_empty());

        let only_project =
            PinFile::parse("[project]\nname = \"x\"\n").expect("只有 project 也合法");
        assert_eq!(only_project.project(), Some("x"));
    }

    /// **形状以票据为准**：票据 §1 的样例是 `[project]` 段 + `name`。
    ///
    /// 第一版把项目名实现成顶层 `project = "我的项目"`（一个字符串），代码、文档与用例三处
    /// 自洽 —— 而真机验收第一次跑就红了：**用户会照票据写**。所以这里同时钉住两件事：
    /// 表形状能用，字符串形状被明确拒绝（而不是被静默当成"没有项目名"）。
    #[test]
    fn the_project_name_lives_in_a_table_not_in_a_bare_string() {
        let error = PinFile::parse("project = \"我的后端\"\n[tools]\nnode = \"24\"\n")
            .expect_err("字符串形状必须被拒绝");
        assert_eq!(error.code(), "pin-parse");
        assert!(
            error.message().contains("[project]") && error.message().contains("name"),
            "错误消息要告诉用户正确形状：{}",
            error.message()
        );

        let unknown = PinFile::parse("[project]\nnmae = \"x\"\n[tools]\nnode = \"24\"\n")
            .expect_err("段内键名写错也要报错");
        assert!(unknown.message().contains("nmae"), "{}", unknown.message());
    }

    #[test]
    fn tool_ids_are_lowercased() {
        let pin = PinFile::parse("[tools]\nNode = \"24\"\n").expect("大写 id 归一化");
        assert!(pin.tools().contains_key("node"));
        assert_eq!(pin.tools()["node"].raw(), "24");
    }

    #[test]
    fn two_spellings_of_one_id_is_an_error() {
        let error =
            PinFile::parse("[tools]\nNode = \"24\"\nnode = \"20\"\n").expect_err("撞车要报错");
        assert_eq!(error.code(), "pin-parse");
        assert!(error.message().contains("写了两遍"), "{}", error.message());
    }

    #[test]
    fn unknown_section_is_refused_not_ignored() {
        // 段名写错（`[tool]` 而不是 `[tools]`）不能被当成"什么都没 pin"。
        let error = PinFile::parse("[tool]\nnode = \"24\"\n").expect_err("段名写错要报错");
        assert_eq!(error.code(), "pin-parse");
        assert!(error.message().contains("tool"), "{}", error.message());

        assert!(PinFile::parse("project = \"x\"\ncolour = \"blue\"\n").is_err());
    }

    #[test]
    fn numeric_spec_is_refused_with_a_hint() {
        let error = PinFile::parse("[tools]\nnode = 24\n").expect_err("数字规格要报错");
        assert_eq!(error.code(), "pin-parse");
        let message = error.message();
        assert!(message.contains("整数"), "{message}");
        assert!(message.contains("node = \"24\""), "{message}");

        // `24.20` 在 TOML 里是浮点 24.2 —— 正是"看起来一样、含义不同"的那种错。
        let float = PinFile::parse("[tools]\nnode = 24.20\n").expect_err("浮点规格要报错");
        assert!(float.message().contains("浮点数"), "{}", float.message());
    }

    #[test]
    fn bad_spec_reports_the_tool_id() {
        let error = PinFile::parse("[tools]\nnode = \"24.x\"\n").expect_err("坏规格要报错");
        assert_eq!(error.code(), "pin-parse");
        assert!(error.message().contains("node"), "{}", error.message());
    }

    #[test]
    fn empty_or_blank_file_is_an_error() {
        for text in ["", "   \n\t\n", "# 只有注释\n"] {
            let error = PinFile::parse(text).expect_err("空文件要报错");
            assert_eq!(error.code(), "pin-parse", "{text:?}");
        }
    }

    #[test]
    fn syntax_error_mentions_toml() {
        let error = PinFile::parse("[tools]\nnode = \n").expect_err("语法错误要报错");
        assert_eq!(error.code(), "pin-parse");
        assert!(error.message().contains("TOML"), "{}", error.message());
    }

    #[test]
    fn bom_is_stripped_for_parsing_but_not_for_the_fingerprint() {
        let with_bom = "\u{feff}[tools]\nnode = \"24\"\n";
        let pin = PinFile::parse(with_bom).expect("带 BOM 的 TOML 要能解析");
        assert!(pin.tools().contains_key("node"));
        // 指纹两条路：BOM 是内容（决策 117）→ 与不带 BOM 的文本不同。
        let without = "[tools]\nnode = \"24\"\n";
        assert_ne!(
            crate::pin::trust::normalize_for_fingerprint(with_bom.as_bytes()),
            crate::pin::trust::normalize_for_fingerprint(without.as_bytes())
        );
    }

    #[test]
    fn load_reports_missing_pin_and_attaches_the_path() {
        let temp = TempDir::new("pin-load");
        let path = temp.join("tuoen.toml");
        match PinFile::load(&path).expect_err("文件不存在") {
            PinError::MissingPin { path: reported } => assert_eq!(reported, path),
            other => panic!("变体错了：{other:?}"),
        }

        std::fs::write(&path, "[tools]\nnode = \"24.x\"\n").expect("写");
        match PinFile::load(&path).expect_err("坏规格") {
            PinError::Parse {
                path: Some(reported),
                ..
            } => assert_eq!(reported, path),
            other => panic!("变体错了：{other:?}"),
        }
    }

    #[test]
    fn load_reports_non_utf8_as_parse_error() {
        let temp = TempDir::new("pin-load-utf8");
        let path = temp.write("tuoen.toml", &[0xff, 0xfe, 0x00, 0x01]);
        let error = PinFile::load(&path).expect_err("不是 UTF-8");
        assert_eq!(error.code(), "pin-parse");
        assert!(error.message().contains("UTF-8"), "{}", error.message());
    }
}
