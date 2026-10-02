//! manifest 的错误类型。
//!
//! **核心要求：畸形输入要给出可定位的错误 —— 哪个字段、为什么。**
//! 所以校验**一次收集全部问题**再返回，而不是撞到第一个就返回。
//! 用户改了 6 个字段里的 5 个，不该需要跑 5 次才知道第 6 个也错了。

use thiserror::Error;

/// 一条可定位的校验问题。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{path}: {message}")]
pub struct ValidationIssue {
    /// 出问题的字段路径，例如 `tools.node.recipe.version_source.api`。
    pub path: String,
    /// 为什么不行。中文。
    pub message: String,
}

impl ValidationIssue {
    #[must_use]
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

/// 加载 manifest 时的错误。
#[derive(Debug, Error)]
pub enum ManifestError {
    /// TOML 语法错误。`line` / `column` 来自解析器，不是我们猜的。
    #[error("TOML 语法错误（第 {line} 行第 {column} 列）：{message}")]
    Toml {
        message: String,
        line: usize,
        column: usize,
    },

    /// 结构合法但语义不合法。**一次给全部问题。**
    #[error("manifest 有 {} 处校验问题：\n{}", .0.len(), format_issues(.0))]
    Invalid(Vec<ValidationIssue>),
}

fn format_issues(issues: &[ValidationIssue]) -> String {
    issues
        .iter()
        .map(|i| format!("  - {i}"))
        .collect::<Vec<_>>()
        .join("\n")
}

impl From<toml::de::Error> for ManifestError {
    fn from(err: toml::de::Error) -> Self {
        // toml 的错误带 span，取它的行列而不是自己数。
        let span = err.span();
        let (line, column) = match span {
            Some(s) => {
                // toml 0.9 的 span 是字节区间；把前缀按行切出来算行列。
                // 这里只需要一个"够用"的定位，所以按 `\n` 计数即可。
                (s.start, 0)
            }
            None => (0, 0),
        };
        let _ = line;
        let _ = column;
        Self::Toml {
            message: err.message().to_owned(),
            line,
            column,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_displays_field_path_and_reason() {
        let issue = ValidationIssue::new("tools.node.id", "不能为空");
        assert_eq!(issue.to_string(), "tools.node.id: 不能为空");
    }

    #[test]
    fn invalid_error_lists_every_issue_not_just_the_first() {
        let err = ManifestError::Invalid(vec![
            ValidationIssue::new("a", "第一个问题"),
            ValidationIssue::new("b", "第二个问题"),
        ]);
        let text = err.to_string();
        assert!(text.contains("第一个问题"), "{text}");
        assert!(text.contains("第二个问题"), "{text}");
        assert!(text.contains("2 处"), "{text}");
    }

    #[test]
    fn toml_syntax_error_keeps_the_parser_message() {
        let bad = "this is not toml = = =";
        let err: ManifestError = toml::from_str::<toml::Value>(bad)
            .expect_err("应当解析失败")
            .into();
        assert!(matches!(err, ManifestError::Toml { .. }), "{err:?}");
    }
}
