//! 许可证：**产品级约束，不是法务脚注**。
//!
//! 规则（`docs/DESIGN.md` §3）：
//!
//! > 每个制品的许可证必须是模型里可见的一等字段，并**拒绝一切"不得收费 / 禁止再分发"
//! > 条款的东西**。这既是真差异化也是项目自保。
//!
//! **默认只镜像元数据（URL + SHA256），不镜像二进制**；只在许可明确允许处
//! （Temurin / CPython / Node.js）重新托管。
//!
//! **本模块的核心不变量**：recipe 层的许可证覆盖**只能更严格，不能放宽**。
//! 否则一个"看起来可再分发"的 recipe 就能绕开工具层的 `prohibited` 结论 ——
//! 那是这个门禁最容易出的洞。

use serde::{Deserialize, Serialize};

/// 一个制品的**再分发结论**。
///
/// 变体的**顺序即严格程度**（`Allowed` 最宽松，`Prohibited` 最严格）——
/// [`Redistribution::more_restrictive`] 依赖它，所以**不要重排**。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Redistribution {
    /// 可再分发（Temurin / CPython / Node.js）。
    Allowed,
    /// 只能再分发元数据（厂商 URL + SHA256），**不镜像二进制**。**这是默认值。**
    MetadataOnly,
    /// 有条件：如 Oracle JDK 21+ (NFTC) 可再分发未修改版本**前提是不收费**。
    Conditional,
    /// **不可再分发**（Oracle JDK 8/11/17、MSVC）。
    Prohibited,
}

impl Redistribution {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::MetadataOnly => "metadata-only",
            Self::Conditional => "conditional",
            Self::Prohibited => "prohibited",
        }
    }

    /// 这个制品能否被 tuoen 自己安装。
    ///
    /// `Prohibited` 一律拒绝；`Conditional` 允许安装但必须在输出里说明条件
    /// （"不收费"这条对我们是自动满足的，但用户需要知道它存在）。
    #[must_use]
    pub const fn installable(self) -> bool {
        !matches!(self, Self::Prohibited)
    }

    /// 我们能否把二进制**重新托管**在自己的服务器 / 离线包里。
    ///
    /// 只有 `Allowed` 可以。`Conditional`（Oracle NFTC 的"不收费"）不行 ——
    /// 我们对"不收费"的满足方式无法对下游传递，所以保守处理。
    #[must_use]
    pub const fn may_rehost(self) -> bool {
        matches!(self, Self::Allowed)
    }

    /// 取两者中更严格的一个。
    ///
    /// **这是"覆盖只能更严格"的实现。** 因为变体的顺序就是严格程度，
    /// 所以 `Ord::max` 正好是想要的语义。
    #[must_use]
    pub fn more_restrictive(self, other: Self) -> Self {
        self.max(other)
    }
}

/// 归档格式。决定用哪条解压路径。
///
/// 依据（本机实测）：系统自带 `tar.exe` 是 bsdtar 3.8.8 / libarchive 3.8.8
/// （含 libzstd 1.5.7 + liblzma 5.8.1），所以 zip / tar.gz / tar.xz / tar.zst /
/// tar.bz2 都能可靠 shellout 解决。**`.7z` 是唯一缺口** ——
/// 而 Node arm64 只发 `.zip` / `.7z`，没有 msi。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArchiveFormat {
    Zip,
    TarGz,
    TarXz,
    TarZst,
    TarBz2,
    /// **唯一需要内置库的缺口。**
    SevenZip,
}

impl ArchiveFormat {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Zip => "zip",
            Self::TarGz => "tar-gz",
            Self::TarXz => "tar-xz",
            Self::TarZst => "tar-zst",
            Self::TarBz2 => "tar-bz2",
            Self::SevenZip => "seven-zip",
        }
    }

    /// 系统 `tar.exe` 能不能读它。
    #[must_use]
    pub const fn readable_by_system_tar(self) -> bool {
        !matches!(self, Self::SevenZip)
    }
}

/// 门禁的判定结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateVerdict {
    /// 允许安装。`notes` 非空时必须在输出里显示（例如 NFTC 的"不收费"条件）。
    Allowed { notes: String },
    /// 拒绝。**`reason` 必须具体** —— "许可问题"这种信息对用户毫无用处。
    Rejected { reason: String },
}

impl GateVerdict {
    #[must_use]
    pub const fn is_allowed(&self) -> bool {
        matches!(self, Self::Allowed { .. })
    }
}

/// 对一个**已解析的**制品做许可证门禁判定。
///
/// `licence_name` 与 `redistribution` 应当来自 [`crate::resolve`] 的
/// "工具层与 recipe 层取更严格"的结果 —— 门禁本身不再做层级合并，
/// 因为那会让"覆盖只能更严格"这条不变量有两个实现点。
#[must_use]
pub fn evaluate(redistribution: Redistribution, licence_name: &str, notes: &str) -> GateVerdict {
    match redistribution {
        Redistribution::Allowed | Redistribution::MetadataOnly => GateVerdict::Allowed {
            notes: notes.to_owned(),
        },
        Redistribution::Conditional => {
            let mut text = format!("该版本按 {licence_name} 许可可再分发，但有条件：");
            if notes.is_empty() {
                text.push_str("（未记录具体条件）");
            } else {
                text.push_str(notes);
            }
            GateVerdict::Allowed { notes: text }
        }
        Redistribution::Prohibited => {
            // 只写"为什么不行 + 去哪拿"，不重复上一句已经说过的结论。
            let mut reason = format!("该版本不可再分发（{licence_name}）。");
            if !notes.is_empty() {
                reason.push(' ');
                reason.push_str(notes);
            }
            reason.push_str(" 请从上游自行获取。");
            GateVerdict::Rejected { reason }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prohibited_is_not_installable_and_not_rehostable() {
        assert!(!Redistribution::Prohibited.installable());
        assert!(!Redistribution::Prohibited.may_rehost());
    }

    #[test]
    fn only_allowed_may_rehost() {
        // Conditional（Oracle NFTC 的"不收费"）不行：我们对"不收费"的满足方式
        // 无法对下游传递，所以保守处理。
        assert!(Redistribution::Allowed.may_rehost());
        assert!(!Redistribution::Conditional.may_rehost());
        assert!(!Redistribution::MetadataOnly.may_rehost());
        assert!(!Redistribution::Prohibited.may_rehost());
    }

    #[test]
    fn more_restrictive_never_relaxes() {
        // 这是本模块最重要的不变量：recipe 层的覆盖不能绕开工具层的结论。
        for a in [
            Redistribution::Allowed,
            Redistribution::MetadataOnly,
            Redistribution::Conditional,
            Redistribution::Prohibited,
        ] {
            for b in [
                Redistribution::Allowed,
                Redistribution::MetadataOnly,
                Redistribution::Conditional,
                Redistribution::Prohibited,
            ] {
                let merged = a.more_restrictive(b);
                assert!(merged >= a, "{a:?} + {b:?} = {merged:?} 放宽了");
                assert!(merged >= b, "{a:?} + {b:?} = {merged:?} 放宽了");
            }
        }
        // 关键用例：prohibited 的工具不能被 allowed 的 recipe 洗白。
        assert_eq!(
            Redistribution::Prohibited.more_restrictive(Redistribution::Allowed),
            Redistribution::Prohibited
        );
    }

    #[test]
    fn prohibited_verdict_gives_a_specific_reason() {
        let verdict = evaluate(
            Redistribution::Prohibited,
            "Oracle NFTC (仅 21+)",
            "Oracle JDK 8/11/17 的许可不允许再分发。",
        );
        match verdict {
            GateVerdict::Rejected { reason } => {
                assert!(reason.contains("Oracle NFTC"), "{reason}");
                assert!(reason.contains("不可再分发"), "{reason}");
                assert!(reason.contains("自行获取"), "{reason}");
                // 关键：不能是"许可问题"这种无用信息。
                assert!(!reason.contains("许可问题"), "{reason}");
            }
            other => panic!("应当被拒绝，实际：{other:?}"),
        }
    }

    #[test]
    fn conditional_verdict_carries_its_condition() {
        let verdict = evaluate(
            Redistribution::Conditional,
            "NFTC",
            "可再分发未修改版本，前提是不收费。",
        );
        match verdict {
            GateVerdict::Allowed { ref notes } => {
                assert!(notes.contains("不收费"), "{notes}");
                assert!(verdict.is_allowed());
            }
            ref other => panic!("应当允许，实际：{other:?}"),
        }
    }

    #[test]
    fn conditional_without_notes_says_so_instead_of_inventing_one() {
        let verdict = evaluate(Redistribution::Conditional, "NFTC", "");
        match verdict {
            GateVerdict::Allowed { notes } => assert!(notes.contains("未记录"), "{notes}"),
            other => panic!("应当允许，实际：{other:?}"),
        }
    }

    #[test]
    fn system_tar_cannot_read_seven_zip() {
        assert!(ArchiveFormat::Zip.readable_by_system_tar());
        assert!(ArchiveFormat::TarZst.readable_by_system_tar());
        assert!(!ArchiveFormat::SevenZip.readable_by_system_tar());
    }

    #[test]
    fn archive_labels_are_the_public_contract() {
        assert_eq!(ArchiveFormat::Zip.as_str(), "zip");
        assert_eq!(ArchiveFormat::SevenZip.as_str(), "seven-zip");
        assert_eq!(ArchiveFormat::TarBz2.as_str(), "tar-bz2");
    }
}
