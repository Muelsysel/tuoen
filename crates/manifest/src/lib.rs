//! tuoen 的 manifest 层：**两层 schema**。
//!
//! - **catalog** —— 工具元数据（名字、别名、描述、**许可证**、上游、版本来源）
//! - **recipe** —— 某个具体版本的安装步骤（URL 模板、校验、归档格式、解压布局、暴露方式）
//!
//! **为什么必须两层**：许可证字段必须挂在"工具"层，但 Oracle 的规则**按版本分界**
//! （JDK 21+ 为 NFTC 可镜像；JDK 8/11/17 不可再分发），两层的切分点正好落在"版本"上。
//!
//! **本 crate 目前只有形状**（ticket #2）。schema 的完整实现、许可证门禁、种子 catalog
//! 在 ticket #3 落地。见 `docs/DESIGN.md` 决策 18/19/32。

use serde::{Deserialize, Serialize};

/// 一个工具的**许可证结论**。这是模型里的一等字段，不是法务脚注。
///
/// **门禁规则**：拒绝一切"不得收费 / 禁止再分发"条款的上游，并给出**具体原因**
/// —— "许可问题"这种信息对用户毫无用处。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Redistribution {
    /// 可再分发（Temurin / CPython / Node.js）。
    Allowed,
    /// 只能再分发元数据（厂商 URL + SHA256），**不镜像二进制**。这是默认值。
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
        matches!(self, Self::Allowed | Self::MetadataOnly | Self::Conditional)
    }
}

/// 归档格式。决定用哪条解压路径。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArchiveFormat {
    Zip,
    TarGz,
    TarXz,
    TarZst,
    TarBz2,
    /// **唯一需要内置库的缺口** —— 系统自带 `tar.exe` 是 bsdtar，不含 7z。
    SevenZip,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prohibited_artifacts_are_not_installable() {
        assert!(!Redistribution::Prohibited.installable());
        assert!(Redistribution::Allowed.installable());
        assert!(Redistribution::MetadataOnly.installable());
        assert!(Redistribution::Conditional.installable());
    }

    #[test]
    fn licence_labels_are_the_public_contract() {
        assert_eq!(Redistribution::Allowed.as_str(), "allowed");
        assert_eq!(Redistribution::MetadataOnly.as_str(), "metadata-only");
        assert_eq!(Redistribution::Conditional.as_str(), "conditional");
        assert_eq!(Redistribution::Prohibited.as_str(), "prohibited");
    }

    #[test]
    fn seven_zip_is_its_own_variant_because_tar_cannot_read_it() {
        // 系统 tar.exe 是 bsdtar 3.8.8（含 zstd + lzma，但不含 7z），
        // 而 Node arm64 只发 .zip/.7z —— 所以 7z 必须能独立表达。
        let json = serde_json::to_string(&ArchiveFormat::SevenZip).expect("serialise");
        assert_eq!(json, r#""seven-zip""#);
    }
}
