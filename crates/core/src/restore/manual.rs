//! 做不到的事，以及"为什么做不到"的**稳定 slug**。
//!
//! # 这个文件为什么存在
//!
//! `restore` 会做不完的事：要提权的、凭据要用户自己重配的、许可不允许再分发的、
//! 别人管的工具、这个版本还不支持的（#24 又加了两类：全局包装在别处、两个来源版本冲突）。
//! 这七类**不是日志**，是票据点名的**公开契约**
//! （决策 152）：GUI（L3）靠 `code` + `detail` 本地化，脚本靠它们做断言。
//!
//! 于是有一条硬要求：**中文散文只进人类输出**。这一层里每个字段都是稳定 ASCII，
//! 而"具体原因"落在 `detail` 的 slug 上 —— 票据点名禁止 `licence-blocked` 写
//! "许可问题"这种**对用户毫无用处**的话：用户要的是"哪一个工具、因为哪一条许可、
//! 结果是什么"。所以 `detail` 是 `oracle-jdk-redistribution-not-permitted`，
//! 而不是 `licence`。
//!
//! # ASCII 不是靠"记得别抄 `evidence`"
//!
//! `capture` 写出来的东西里混着中文散文（`ToolRow.evidence`）与给「人」看的占位符
//! （`ToolRow.path` 可能是 `<无 InstallLocation，卸载键 {GUID}>`）。医生那一票已经
//! 被这个坑抓过一次（占位符原样漏进了成功载荷），仓库里也留了规矩：**翻译成
//! `uninstall-key={GUID}` 这种稳定 slug 才进证据**。
//!
//! 这里的做法更彻底一层：[`ascii_token`] 是**每个进计划的字符串的必经之路**。
//! 它把非 ASCII、控制字符与空白压成 `-`。于是"哪天有人顺手把 `evidence` 传进来"
//! 也不会污染 `--json` —— 契约由**函数**守住，而不是靠调用方记得。

use serde::{Deserialize, Serialize};

/// 手动待办的类别。**七个 `code` 是稳定字符串**（决策 152），`as_str` 是唯一取值处。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ManualActionCode {
    /// 这一步要提权才能做（机器级环境变量、机器级 `PATH`）。
    RequiresElevation,
    /// 凭据要在新机器上自己重配（DPAPI / 凭据管理器是**用户+机器绑定**的，迁移后静默失败）。
    CredentialReconfigure,
    /// 再分发许可不允许 tuoen 下载/安装它。
    LicenceBlocked,
    /// 由 nvm4w 这类第三方版本管理器管的工具 —— 我们不接管（决策 154）。
    ThirdPartyManager,
    /// 这个版本还不支持自动还原的这一条。
    Unsupported,
    /// 快照里的全局包**不在** tuoen 的根里 —— 它们装在机器自己的 prefix 下（票据 #24 §4）。
    ///
    /// 这一条**不是**"缺了什么"，而是"装到哪儿去了"：`npm ls -g` 永远看不到我们装的那些，
    /// 而用户以为"还原完了就该跟原来一样"。这句话必须说出来（决策 190 的不对称）。
    GlobalsPrefixMoved,
    /// 同一个全局包在两个来源里**版本不同** —— 一个都不装，等用户自己定（票据 #24 的安装期规则）。
    GlobalsVersionConflict,
}

impl ManualActionCode {
    /// 全部七类，**顺序即文档里那张表的顺序**（稳定，便于 CLI 生成帮助）。
    pub const ALL: [Self; 7] = [
        Self::RequiresElevation,
        Self::CredentialReconfigure,
        Self::LicenceBlocked,
        Self::ThirdPartyManager,
        Self::Unsupported,
        Self::GlobalsPrefixMoved,
        Self::GlobalsVersionConflict,
    ];

    /// 稳定小写 slug（进 `--json`，**不本地化**）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RequiresElevation => "requires-elevation",
            Self::CredentialReconfigure => "credential-reconfigure",
            Self::LicenceBlocked => "licence-blocked",
            Self::ThirdPartyManager => "third-party-manager",
            Self::Unsupported => "unsupported",
            Self::GlobalsPrefixMoved => "globals-prefix-moved",
            Self::GlobalsVersionConflict => "globals-version-conflict",
        }
    }
}

/// 一条手动待办。
///
/// 三个字符串字段**全是纯 ASCII**（由 [`ascii_token`] 保证）。`remediation` 的取值见下面那七个常量 —— 它是"用户该做什么"的稳定 slug（`run-as-administrator` 这种），
/// 中文说明（例如"不提权时的降级路径是什么"）属于人类输出，不在这个结构里。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualAction {
    /// 哪一类。
    pub code: ManualActionCode,
    /// 要动手的**那个东西的标识**（数据，纯 ASCII）。含义随 `code` 变，见模块文档的表。
    pub subject: String,
    /// **具体原因**的稳定 slug（同样是数据）。
    pub detail: String,
    /// 建议怎么做。
    pub remediation: String,
}

impl ManualAction {
    /// 造一条。`subject` 过一遍 [`ascii_token`]（它是一个**标识**），
    /// `detail` 过一遍 [`ascii_layout`]（它可能是 `scope=machine var=PATH` 这种 `key=value` 串，
    /// 那个 `=` 是结构，不能被打成 `-`）。
    #[must_use]
    pub fn new(
        code: ManualActionCode,
        subject: &str,
        detail: &str,
        remediation: &'static str,
    ) -> Self {
        Self {
            code,
            subject: ascii_token(subject),
            detail: ascii_layout(detail),
            remediation: remediation.to_owned(),
        }
    }

    /// 去重用的键：`code` + `subject` + `detail`（`remediation` 由 `code` 决定，不参与）。
    #[must_use]
    pub(crate) fn key(&self) -> (ManualActionCode, String, String) {
        (self.code, self.subject.clone(), self.detail.clone())
    }
}

/// 建议"以管理员身份跑一次"。**tuoen 绝不自己弹 UAC**（决策 136：静默提权会让"我只是想看看 diff"变成一次系统级改动）。
pub const REMEDIATION_RUN_AS_ADMINISTRATOR: &str = "run-as-administrator";
/// 建议"自己把凭据重新配一遍"（材料**绝不进计划**，决策 153）。
pub const REMEDIATION_RECONFIGURE_MANUALLY: &str = "reconfigure-manually";
/// 建议"从上游自己装"（许可不允许我们再分发它）。
pub const REMEDIATION_INSTALL_MANUALLY: &str = "install-manually";
/// 建议"用它自己的管理器"（nvm4w / uv 的符号链接、环境变量、`settings.txt` 我们都不碰）。
pub const REMEDIATION_USE_THE_MANAGER: &str = "use-the-manager";
/// 建议"等这个版本支持"（不是失败，是我们明确不做）。
pub const REMEDIATION_NOT_SUPPORTED_IN_THIS_VERSION: &str = "not-supported-in-this-version";
/// 建议"用 `tuoen globals list` 看我们管着哪些全局包"（票据 #24 §4）。
///
/// 为什么不是 `install-manually`：那些包**已经装好了**，只是装在别的地方 ——
/// 用户要做的不是再装一遍，而是知道去哪儿找它们。
pub const REMEDIATION_USE_TUOEN_GLOBALS_LIST: &str = "use-tuoen-globals-list";
/// 建议"自己决定留哪个版本"（票据 #24 的安装期规则：两个来源版本不同 ⇒ 一个都不装）。
///
/// **绝不静默挑一个**：`machine` 那一份是用户此刻真正在用的，`tuoen` 那一份是我们管的根 ——
/// 挑错了的表现是"工具版本悄悄变了"，而那种事没有任何人会发现。
pub const REMEDIATION_RESOLVE_MANUALLY: &str = "resolve-manually";

/// 把任意文本压成**纯 ASCII token**：非 ASCII / 控制字符 / 空白 → `-`（连续折叠）。
///
/// 保留的字符是"标识里真的会出现"的那些：字母数字与 `- _ . : \ / @ + { }`。
/// **`=` 被刻意排除** —— `detail` 是 `scope=machine var=PATH` 这种 `key=value` 串，
/// 值里再出现一个 `=` 会让它没法被解析。
///
/// 一个字符都不剩时返回 `unnamed`（而不是空串）：空 `subject` 在 `--json` 里看起来像
/// "这个字段忘了填"，而它实际上是一条真实存在的、名字全是非 ASCII 的记录。
#[must_use]
pub fn ascii_token(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_dash = false;
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric()
            || matches!(
                ch,
                '-' | '_' | '.' | ':' | '\\' | '/' | '@' | '+' | '{' | '}'
            )
        {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(ch);
        } else {
            pending_dash = true;
        }
    }
    if out.is_empty() {
        "unnamed".to_owned()
    } else {
        out
    }
}

/// 把一段**已经是 slug 形状**的文本压成 ASCII，**保留结构字符**（`=` 与空格）。
///
/// 与 [`ascii_token`] 的分工：那个是给**值**用的（值里出现 `=` 会让 `key=value` 没法解析），
/// 这个是给**整条 `detail`** 用的 —— `detail` 里的 `=` 是我们自己拼的结构，
/// 打掉它就把 `scope=machine var=PATH` 变成了 `scope-machine-var-PATH`（一个看起来
/// 还挺像 slug 的东西，所以这个 bug 不会自己跳出来喊）。
///
/// 非 ASCII 与控制字符仍然一律换成 `-`：中文散文混进 `detail` 时会被压平，
/// 而不是漏进 `--json`。
#[must_use]
pub(crate) fn ascii_layout(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if ch.is_ascii() && !ch.is_ascii_control() {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

/// 去掉重复的手动待办（`code` + `subject` + `detail` 相同即同一条），**保留首次出现**。
///
/// 去重的理由不是整齐：同一个管理器管着 `node` 与 `python` 是**一件事**
/// （"别碰 nvm4w"），每个工具行报一遍会让用户以为自己要做两件事。
pub(crate) fn dedupe_manual(actions: &mut Vec<ManualAction>) {
    let mut seen: Vec<(ManualActionCode, String, String)> = Vec::new();
    actions.retain(|action| {
        let key = action.key();
        if seen.contains(&key) {
            false
        } else {
            seen.push(key);
            true
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_seven_codes_have_stable_kebab_slugs() {
        let slugs: Vec<&str> = ManualActionCode::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(
            slugs,
            vec![
                "requires-elevation",
                "credential-reconfigure",
                "licence-blocked",
                "third-party-manager",
                "unsupported",
                // #24 加的两类：前五类的位置**一个字都没动**（已冻结的载荷顺序）。
                "globals-prefix-moved",
                "globals-version-conflict",
            ],
            "七个 code 是公开契约，顺序与取值都不许漂"
        );
        // 每个 code 都有自己的 slug（复制粘贴漏改会在这里红）。
        let mut unique = slugs.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), slugs.len());
    }

    #[test]
    fn remediation_slugs_are_the_ones_the_decision_names() {
        assert_eq!(REMEDIATION_RUN_AS_ADMINISTRATOR, "run-as-administrator");
        assert_eq!(REMEDIATION_RECONFIGURE_MANUALLY, "reconfigure-manually");
        assert_eq!(REMEDIATION_INSTALL_MANUALLY, "install-manually");
        assert_eq!(REMEDIATION_USE_THE_MANAGER, "use-the-manager");
        assert_eq!(
            REMEDIATION_NOT_SUPPORTED_IN_THIS_VERSION,
            "not-supported-in-this-version"
        );
        assert_eq!(REMEDIATION_USE_TUOEN_GLOBALS_LIST, "use-tuoen-globals-list");
        assert_eq!(REMEDIATION_RESOLVE_MANUALLY, "resolve-manually");
    }

    #[test]
    fn ascii_token_flattens_chinese_prose_instead_of_leaking_it() {
        // 真机上的 `ToolRow.evidence` 就长这样。它绝不能进 `--json`。
        let prose = "PATH 第 1 条（process-only）里有 cargo.exe";
        let token = ascii_token(prose);
        assert!(token.is_ascii(), "{token}");
        assert!(!token.contains('（'), "{token}");
        // `cargo.exe` 那一段是 ASCII，可读的部分应当留下。
        assert!(token.contains("cargo.exe"), "{token}");
    }

    #[test]
    fn ascii_token_keeps_the_characters_an_identity_needs() {
        assert_eq!(ascii_token("env:ARK_API_KEY"), "env:ARK_API_KEY");
        assert_eq!(ascii_token(r"C:\Users\a\bin"), r"C:\Users\a\bin");
        assert_eq!(ascii_token("nvm4w"), "nvm4w");
        assert_eq!(ascii_token("IntelliJ IDEA"), "IntelliJ-IDEA");
        assert_eq!(ascii_token("24.19.0"), "24.19.0");
    }

    #[test]
    fn ascii_token_drops_the_equals_sign_so_key_value_details_stay_parseable() {
        // 值里混进一个 `=` 会让 `scope=machine var=a=b` 没法被解析。
        assert_eq!(ascii_token("a=b"), "a-b");
        assert_eq!(ascii_token("x==y"), "x-y");
    }

    #[test]
    fn ascii_token_never_returns_an_empty_string() {
        // 空 `subject` 在 `--json` 里看起来像"字段忘了填"，而它其实是一条真实的记录。
        assert_eq!(ascii_token(""), "unnamed");
        assert_eq!(ascii_token("中文"), "unnamed");
        assert_eq!(ascii_token("  "), "unnamed");
    }

    #[test]
    fn manual_action_fields_are_all_ascii_and_never_empty() {
        let action = ManualAction::new(
            ManualActionCode::LicenceBlocked,
            "oracle-jdk",
            "oracle-jdk-redistribution-not-permitted",
            REMEDIATION_INSTALL_MANUALLY,
        );
        for field in [&action.subject, &action.detail, &action.remediation] {
            assert!(field.is_ascii(), "{field}");
            assert!(!field.is_empty(), "{field}");
        }
    }

    #[test]
    fn dedupe_keeps_the_first_and_drops_exact_repeats() {
        let mut actions = vec![
            ManualAction::new(
                ManualActionCode::ThirdPartyManager,
                "nvm4w",
                "nvm4w",
                REMEDIATION_USE_THE_MANAGER,
            ),
            ManualAction::new(
                ManualActionCode::ThirdPartyManager,
                "uv",
                "uv",
                REMEDIATION_USE_THE_MANAGER,
            ),
            ManualAction::new(
                ManualActionCode::ThirdPartyManager,
                "nvm4w",
                "nvm4w",
                REMEDIATION_USE_THE_MANAGER,
            ),
        ];
        dedupe_manual(&mut actions);
        let subjects: Vec<&str> = actions.iter().map(|a| a.subject.as_str()).collect();
        assert_eq!(subjects, ["nvm4w", "uv"], "保留首次出现");
    }
}
