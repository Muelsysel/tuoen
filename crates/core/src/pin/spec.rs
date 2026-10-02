//! 版本规格 —— `[tools]` 里那些字符串（`"24"` / `"24.19"` / `"v24"`）的模型。
//!
//! # 匹配是**数字成分前缀**，不是字符串前缀（决策 113）
//!
//! 规格按 `.` 切成成分，逐个与版本号对应的成分比，比的是**数值**：`"2"` **不**命中
//! `20.1.0`，而 `"24"` 命中 `24.19.0`。字符串前缀在这里是错的
//! （`"2".starts_with` 会命中 `20.1.0`），而"语义化版本区间"（`>=24 <25`）是另一种东西 ——
//! 这一票只做前者。
//!
//! # 成分怎么算"同一个"：`<数字>` + 可选后缀
//!
//! 规格成分是**纯数字**；版本侧的一个成分拆成「**前导数字 + 后缀**」。两者匹配，
//! 当且仅当**前导数字相等**，且后缀**没有**、或者**以 `_` 开头**：
//!
//! | 版本 | 规格 | 结果 | 为什么 |
//! |---|---|---|---|
//! | `1.8.0_492` | `"1.8.0"` | 命中 | Oracle 的 update 分隔符就是 `_`，`1.8.0_492` **是** 1.8.0（本机 `java -version` 报的就是这个形状） |
//! | `24.19.0-rc.1` | `"24.19.0"` | 不命中 | `-` 开头是**预发布标记**，它不是那个正式版 |
//! | `24.19.0` | `"24.19"` | 命中 | 版本比规格长 = 前缀命中 |
//! | `20.1.0` | `"2"` | 不命中 | **数值**比较，不是字符串前缀 |
//! | `nightly` | `"24"` | 不命中 | 成分解析不出前导数字 → 整条不命中 |
//!
//! 方向仍然是"猜错比猜不中贵"：`-` 后缀（rc / beta / nightly）**不是**那个正式版，
//! 而 `_` 后缀在真实世界的版本号里就是同一个版本的补丁号 —— 把后者也否掉，
//! 会让"用户写 `1.8.0`、机器上是 `1.8.0_492`"变成一条永远修不好的失败。
//!
//! 一个**已知且接受**的边角：`"24"` 命中 `24.19.0-rc.1` —— 规格只有一段，于是只比了
//! 第一段，后面那段预发布标记没被看到。这是"前缀匹配"的固有代价，不为它加特例
//! （真要去掉它，需要的是"整串都不许有预发布标记"这条更宽的规则）。
//! 规格一旦写到第三段，`-rc` 就**必须**被拒 —— 两条都有用例钉住。
//!
//! 前导 `v` 两边都剥：`node --version` 给 `v24.19.0`，而人写 `[tools]` 时哪个都写得出。

use std::fmt;

use crate::pin::PinError;

/// 一条版本规格：`"24"` / `"24.19"` / `"v24"`。
///
/// 构造只有 [`VersionSpec::parse`] 一条路 —— 于是"规格里的成分一定是数字"
/// 是这个类型的**不变量**，而不是每次用到时再检查一遍的纪律。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionSpec {
    /// 声明时的原文（去掉首尾空白）。`tuoen.lock` 里回写的是它，不是归一化后的形状 ——
    /// 用户改了什么，锁里就该看见什么。
    raw: String,
    /// 切好的数字成分。
    parts: Vec<u64>,
}

impl VersionSpec {
    /// 解析一条规格。
    ///
    /// # Errors
    ///
    /// * 空规格（`""` / `"v"` / `"   "`）→ [`PinError::Parse`]（决策 113 点名的那条）；
    /// * 有成分不是纯数字（`"24."` / `"24.x"` / `"24 . 19"`）→ [`PinError::Parse`]；
    /// * 数字大到装不进 `u64` → [`PinError::Parse`]。
    ///
    /// `path` 留空：规格是**一段文本**，不知道它从哪个文件来；知道的那个调用方
    /// （`PinFile::parse`）会把它补上。
    pub fn parse(raw: &str) -> Result<Self, PinError> {
        let text = raw.trim();
        let body = strip_leading_v(text);
        if body.is_empty() {
            return Err(PinError::Parse {
                path: None,
                message: format!(
                    "版本规格是空的（原文 `{raw}`）：至少写一个数字，比如 `\"24\"` 或者 `\"24.19\"`。"
                ),
            });
        }
        let mut parts = Vec::new();
        for component in body.split('.') {
            if !is_all_digits(component) {
                return Err(PinError::Parse {
                    path: None,
                    message: format!(
                        "版本规格 `{raw}` 里的这一段（`{component}`）不是纯数字：\
                         只认「数字 + 点」拼起来的规格，比如 `\"24\"` / `\"24.19\"`。"
                    ),
                });
            }
            let value = component.parse::<u64>().map_err(|_| PinError::Parse {
                path: None,
                message: format!(
                    "版本规格 `{raw}` 里的数字 `{component}` 太大了（超过 64 位整数）——\
                     写小一点的段数，或者把它拆成两段。"
                ),
            })?;
            parts.push(value);
        }
        Ok(Self {
            raw: text.to_owned(),
            parts,
        })
    }

    /// 声明时的原文（去掉了首尾空白）。
    #[must_use]
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// 这个版本号满足这条规格吗。
    ///
    /// 成分个数：规格比版本长 → 不命中（`"24.19.1"` 不命中 `"24.19"`）；版本比规格长 → 命中
    /// （前缀）。数值比较，所以 `"024"` 与 `"24"` 是同一个成分；版本侧的成分可以带
    /// `_` 后缀（`1.8.0_492` 算 1.8.0），但 `-` 后缀是预发布标记（见模块文档那张表）。
    #[must_use]
    pub fn matches(&self, version: &str) -> bool {
        let body = strip_leading_v(version.trim());
        if body.is_empty() {
            return false;
        }
        let components: Vec<&str> = body.split('.').collect();
        if components.len() < self.parts.len() {
            return false;
        }
        // 版本号**自己**得是"数字 + 可选后缀"串起来的：有一个成分解析不出来
        // （`nightly` / `24.x` / 末尾多一个点 / 数字大到溢出）就整条不命中 ——
        // 一个读不懂的版本号不该靠"前缀恰好对上"混进来。
        if components
            .iter()
            .any(|component| split_version_component(component).is_none())
        {
            return false;
        }
        self.parts
            .iter()
            .zip(components)
            .all(|(want, component)| component_matches(*want, component))
    }
}

impl fmt::Display for VersionSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.raw)
    }
}

/// 剥掉可选的前导 `v`（`v24` / `V24`）。只剥一个，`vv24` 不是规格。
fn strip_leading_v(text: &str) -> &str {
    text.strip_prefix(['v', 'V']).unwrap_or(text)
}

/// 这一段是不是"至少一个 ASCII 数字，且全是 ASCII 数字"。
///
/// 用 `is_ascii_digit` 而不是 `char::is_numeric`：后者对 `²` / `٢`（阿拉伯数字）也为真，
/// 而 `parse::<u64>` 对它们为假 —— 两个判据不一致时，`"24²"` 会先被放行再爆一条
/// 看不懂的错误。
fn is_all_digits(component: &str) -> bool {
    !component.is_empty() && component.bytes().all(|byte| byte.is_ascii_digit())
}

/// 版本侧的**一个成分**：前导数字 + 可选后缀（`19` / `0_492` / `0-rc`）。
///
/// 返回 `None` 有两层含义，而调用方对两者的处理一样（不命中）：
/// 这个成分**没有前导数字**（`nightly` / `x` / 空段），或者数字大到装不进 `u64`。
fn split_version_component(component: &str) -> Option<(u64, &str)> {
    let digits_len = component
        .bytes()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if digits_len == 0 {
        return None;
    }
    let value = component[..digits_len].parse::<u64>().ok()?;
    Some((value, &component[digits_len..]))
}

/// 版本侧的一个成分与规格里的一个数字算不算"同一个版本"。
///
/// 后缀决定它是**同一个版本**（`_`：补丁号，如 Oracle 的 `1.8.0_492`）
/// 还是**另一个版本**（`-`：预发布标记，如 `24.19.0-rc.1`）。
fn component_matches(want: u64, component: &str) -> bool {
    match split_version_component(component) {
        Some((value, suffix)) => value == want && (suffix.is_empty() || suffix.starts_with('_')),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(raw: &str) -> VersionSpec {
        VersionSpec::parse(raw).expect("这条规格应当能解析")
    }

    #[test]
    fn numeric_prefix_matching_113() {
        // 决策 113 逐条：这三条是本票最容易被"字符串前缀"实现骗过去的判据。
        assert!(spec("24").matches("24.19.0"));
        assert!(!spec("2").matches("20.1.0"));
        assert!(spec("24.19").matches("24.19.0"));
        assert!(!spec("24.19").matches("24.20.0"));
    }

    #[test]
    fn major_only_matches_any_minor() {
        for version in ["24.0.0", "24.19.0", "24.20.1", "24.99.99"] {
            assert!(spec("24").matches(version), "{version}");
        }
        assert!(!spec("24").matches("23.19.0"));
        assert!(!spec("24").matches("240.0.0"));
        assert!(!spec("24").matches("2.4.0"));
    }

    #[test]
    fn leading_v_is_stripped_on_both_sides() {
        assert!(spec("v24").matches("24.19.0"));
        assert!(spec("24").matches("v24.19.0"));
        assert!(spec("V24").matches("v24.19.0"));
        // 只剥一个：`vv24` 剥完是 `v24`，不是数字 → 报错，而不是被当成 24。
        assert_eq!(
            VersionSpec::parse("vv24").expect_err("只剥一个 v").code(),
            "pin-parse"
        );
    }

    #[test]
    fn spec_longer_than_version_does_not_match() {
        assert!(!spec("24.19.1").matches("24.19"));
        assert!(!spec("24.19.1").matches("24"));
        assert!(spec("24.19").matches("24.19"));
    }

    #[test]
    fn numeric_comparison_ignores_leading_zeros() {
        assert!(spec("24").matches("024.19.0"));
        assert!(spec("024").matches("24.19.0"));
    }

    #[test]
    fn empty_spec_is_a_parse_error() {
        for raw in ["", "   ", "v", "V", "\t"] {
            let error = VersionSpec::parse(raw).expect_err("空规格必须报错");
            assert_eq!(error.code(), "pin-parse", "{raw:?}");
            assert!(error.message().contains("空的"), "{}", error.message());
        }
    }

    #[test]
    fn non_numeric_components_are_a_parse_error() {
        for raw in [
            "24.", ".24", "24.x", "24 . 19", "24-rc1", "2.4+1", "-1", "1,2",
        ] {
            let error = VersionSpec::parse(raw).expect_err("非数字成分必须报错");
            assert_eq!(error.code(), "pin-parse", "{raw:?}");
        }
    }

    #[test]
    fn overflowing_component_is_a_parse_error() {
        let error = VersionSpec::parse("99999999999999999999").expect_err("超过 u64 必须报错");
        assert_eq!(error.code(), "pin-parse");
        assert!(error.message().contains("太大"), "{}", error.message());
    }

    #[test]
    fn java_8_update_style_is_matched_by_its_release_prefix() {
        // 本机 `java -version` 报 `1.8.0_492`：`_` 后缀是**同一个版本**的补丁号。
        assert!(spec("1.8.0").matches("1.8.0_492"));
        assert!(spec("1.8").matches("1.8.0_492"));
        assert!(spec("1").matches("1.8.0_492"));
        assert!(spec("1.8.0").matches("1.8.0"));
        assert!(spec("24.19.0").matches("24.19.0_492"));
        // 前导数字仍然要相等：`1.8.1` 不命中 `1.8.0_492`。
        assert!(!spec("1.8.1").matches("1.8.0_492"));
        assert!(!spec("1.8.0").matches("1.8.1_492"));
    }

    #[test]
    fn a_prerelease_suffix_is_not_a_release() {
        // `-` 后缀是预发布标记：`24.19.0-rc.1` **不是** 24.19.0。
        assert!(!spec("24.19.0").matches("24.19.0-rc.1"));
        assert!(!spec("1.8.0").matches("1.8.0-rc.1"));
        assert!(!spec("24.19.0").matches("24.19.0-beta"));
        // 别的分隔符也一样不是正式版（只有 `_` 算补丁号）。
        assert!(!spec("1.8.0").matches("1.8.0+492"));
        // 没有后缀的正式版当然命中。
        assert!(spec("24.19.0").matches("24.19.0"));
    }

    #[test]
    fn known_corner_a_broad_spec_does_not_see_the_later_components() {
        // 已知且**接受**的边角（见模块文档）：规格只有一段时，后面的预发布标记看不到。
        // 这条用例的作用是把它钉住 —— 哪天有人"顺手修好"它，红在这里，
        // 而不是悄悄改掉 pin 的语义。
        assert!(spec("24").matches("24.19.0-rc.1"));
        assert!(spec("24").matches("24.19.0_492"));
        // 规格一旦写到第三段，预发布标记就**必须**被拒。
        assert!(!spec("24.19.0").matches("24.19.0-rc.1"));
        assert!(spec("24.19.0").matches("24.19.0_492"));
    }

    #[test]
    fn unreadable_versions_never_match() {
        // 读不懂的版本号一律不命中：成分必须先能解析出前导数字
        // （宽松只发生在**后缀**上，不发生在"这段到底是什么"上）。
        for version in [
            "",
            "v",
            "nightly",
            "24.",
            "24.x",
            "x.1",
            ".24",
            "99999999999999999999.0",
        ] {
            assert!(!spec("24").matches(version), "{version:?} 不该命中 24");
            assert!(!spec("1").matches(version), "{version:?} 不该命中 1");
        }
    }

    #[test]
    fn raw_keeps_the_declared_text_and_display_follows_it() {
        assert_eq!(spec("  v24.19  ").raw(), "v24.19");
        assert_eq!(spec("  v24.19  ").to_string(), "v24.19");
    }
}
