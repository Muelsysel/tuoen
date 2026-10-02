//! 平台层的错误。
//!
//! **以码承载，不以文本承载**（`docs/specs/L0-install-engine.md`：错误以码承载，不以文本承载）。
//! 面向用户的文本可以本地化，判断逻辑只能看码 —— 本机是 zh-CN，PowerShell 的报错都是中文的，
//! 任何按错误文本分支的代码换一台机器就静默失效。

use crate::sys::{self, Win32Code};

/// 平台操作失败的原因。
///
/// **`RegistryKeyMissing` 不是"失败"**：注册表里没有这个键是**正常的检测结果**
/// （"这台机器没有装"），调用方应当跳过这条来源而不是中止整个 detect。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlatformError {
    /// 注册表键不存在。
    #[error("注册表键不存在：{hive}\\{subkey}")]
    RegistryKeyMissing {
        /// hive 名（人读，只用于消息）。
        hive: String,
        /// 相对 `SOFTWARE` 的子键路径。
        subkey: String,
    },
    /// 访问被拒绝。**只看 `code`，不看消息**。
    #[error("访问被拒绝（Win32 {code}）：{path}")]
    AccessDenied {
        /// Win32 码（`ERROR_ACCESS_DENIED` = 5）。
        code: Win32Code,
        /// 出错的路径或键。
        path: String,
    },
    /// 其它 Win32 失败。
    #[error("平台调用失败（Win32 {code}）：{path}")]
    Win32 {
        /// Win32 码。
        code: Win32Code,
        /// 出错的路径或键。
        path: String,
    },
    /// 本平台没有这个能力（非 Windows，或功能未实现）。
    #[error("本平台没有这个能力：{what}")]
    Unsupported {
        /// 缺的是什么。
        what: String,
    },
}

impl PlatformError {
    /// 把 Win32 码归一化成一个错误。**唯一的分支依据是码。**
    #[must_use]
    pub fn from_win32(code: Win32Code, path: impl Into<String>) -> Self {
        let path = path.into();
        match code {
            sys::ERROR_ACCESS_DENIED => Self::AccessDenied { code, path },
            sys::ERROR_NOT_SUPPORTED => Self::Unsupported { what: path },
            _ => Self::Win32 { code, path },
        }
    }

    /// 取 Win32 码（如果有）。
    #[must_use]
    pub const fn code(&self) -> Option<Win32Code> {
        match self {
            Self::RegistryKeyMissing { .. } => None,
            Self::AccessDenied { code, .. } | Self::Win32 { code, .. } => Some(*code),
            Self::Unsupported { .. } => Some(sys::ERROR_NOT_SUPPORTED),
        }
    }

    /// 这是"这里没有东西"而不是"读不了"。
    #[must_use]
    pub const fn is_absent(&self) -> bool {
        matches!(self, Self::RegistryKeyMissing { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_denied_is_recognised_by_code_not_by_text() {
        let err = PlatformError::from_win32(5, r"C:\x");
        assert!(matches!(err, PlatformError::AccessDenied { .. }));
        assert_eq!(err.code(), Some(5));
    }

    #[test]
    fn a_missing_registry_key_is_absence_not_failure() {
        let err = PlatformError::RegistryKeyMissing {
            hive: "hklm".to_owned(),
            subkey: r"SOFTWARE\Nope".to_owned(),
        };
        assert!(err.is_absent());
        assert_eq!(err.code(), None);
    }

    #[test]
    fn unsupported_carries_the_documented_code() {
        assert_eq!(
            PlatformError::from_win32(sys::ERROR_NOT_SUPPORTED, "注册表").code(),
            Some(50)
        );
    }
}
