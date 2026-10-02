//! 注册表读取的抽象与真实实现。
//!
//! 检测需要**三套互不重叠的注册表来源**（`docs/specs/L1-dev-state.md` 的问题陈述）：
//! `App Paths`（本机 HKLM 41 + HKCU 19 = 60 条，纯 `PATH` 扫描会**整个漏掉**这套查找机制）
//! 与三个 hive 的卸载键（ARP，本机 HKCU 11 + HKLM 97 + WOW6432Node 44）。
//!
//! **只读**：[`Registry`] 只有读方法，真实实现只调 `RegOpenKeyExW` / `RegEnumKeyExW` /
//! `RegEnumValueW`。没有任何写路径 —— "绝不改写 winget / Scoop 的 `PATH` 与状态"（决策 3）
//! 与"绝不自动清理幽灵条目"（决策 24）在这里是类型事实。
//!
//! **测试实现**：[`crate::fixture::FakeRegistry`]。测试**绝不读真实注册表**
//! （ticket #11 硬约束）。

use serde::{Deserialize, Serialize};

use crate::error::PlatformError;
use crate::sys;

/// `REG_SZ`
pub const REG_SZ: u32 = 1;
/// `REG_EXPAND_SZ`
pub const REG_EXPAND_SZ: u32 = 2;
/// `REG_DWORD`
pub const REG_DWORD: u32 = 4;
/// `REG_MULTI_SZ`
pub const REG_MULTI_SZ: u32 = 7;

/// 注册表里的一个 hive。**三个，不是两个。**
///
/// 所有路径都**相对于该 hive 的 `SOFTWARE`**，这样三个 hive 用同一个子键路径
/// （ARP 的 `Microsoft\Windows\CurrentVersion\Uninstall` 在三个 hive 里都是这个写法），
/// 而不是让调用方记住"WOW6432Node 要写在前面、HKCU 又不用"。
///
/// - [`RegHive::HklmWow6432`] 是 `HKLM\SOFTWARE\WOW6432Node` —— 32 位视图。
///   本机 44 条卸载键只在这里，漏掉它就漏掉一半的已安装软件。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegHive {
    /// `HKEY_CURRENT_USER\SOFTWARE`
    Hkcu,
    /// `HKEY_LOCAL_MACHINE\SOFTWARE`（64 位视图）
    Hklm,
    /// `HKEY_LOCAL_MACHINE\SOFTWARE\WOW6432Node`（32 位视图）
    HklmWow6432,
}

impl RegHive {
    /// `--json` 与固定装置里用的稳定取值，**不本地化**。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hkcu => "hkcu",
            Self::Hklm => "hklm",
            Self::HklmWow6432 => "hklm-wow6432",
        }
    }

    /// 人读的完整前缀。
    #[must_use]
    pub const fn display_prefix(self) -> &'static str {
        match self {
            Self::Hkcu => r"HKCU\SOFTWARE",
            Self::Hklm => r"HKLM\SOFTWARE",
            Self::HklmWow6432 => r"HKLM\SOFTWARE\WOW6432Node",
        }
    }

    /// 真实实现要打开的根键与完整子键路径。
    ///
    /// **以 `SYSTEM\` / `SOFTWARE\` / `Python\` 开头的路径是绝对路径**（相对该 hive 的根，
    /// 而不是相对 `SOFTWARE`），另外 `Environment` 这个名字本身也是绝对的。
    ///
    /// 这条存在的理由是**两个都不在 `SOFTWARE` 下面的键**：
    /// - 机器级环境变量：`HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment`
    /// - 用户级环境变量：`HKCU\Environment`
    ///
    /// 二者都只能用绝对写法表达。《—— **`HKCU\Environment` 这条曾经漏掉，代价是一条真 bug**：
    /// `USER_ENV_SUBKEY` 写 `"Environment"` 时会被拼成 `HKCU\SOFTWARE\Environment`，
    /// 那个键**不存在**，于是用户级环境变量**静默返回空表**。症状是"没有报错、
    /// 只是少了一半数据"：`NVM_HOME` 在 `HKCU\Environment` 与 `HKLM\...\Session Manager\
    /// Environment` 两处都有，而报告里只出现 `（machine）`。
    ///
    /// 判定前缀是**注册表不区分大小写**的，所以这里也大小写不敏感。
    /// ARP 与 App Paths 用的都是 `Microsoft\Windows\...`，不匹配这些前缀，
    /// 所以它们的解析完全不受影响。
    #[must_use]
    pub(crate) fn resolve(self, subkey: &str) -> (sys::RootKey, String) {
        let subkey = subkey.trim_matches('\\');
        let root = match self {
            Self::Hkcu => sys::RootKey::CurrentUser,
            Self::Hklm | Self::HklmWow6432 => sys::RootKey::LocalMachine,
        };
        if let Some(absolute) = strip_absolute_prefix(subkey) {
            return (root, absolute);
        }
        match self {
            Self::Hkcu => (root, format!(r"SOFTWARE\{subkey}")),
            Self::Hklm => (root, format!(r"SOFTWARE\{subkey}")),
            Self::HklmWow6432 => (root, format!(r"SOFTWARE\WOW6432Node\{subkey}")),
        }
    }
}

/// 绝对路径前缀（相对 hive 根）。见 [`RegHive::resolve`] 的说明。
///
/// - `SYSTEM\` —— 机器级环境变量在那里
/// - `SOFTWARE\` —— 显式写全的路径
/// - `Python\` —— CPython 官方安装器的键（`HKCU\SOFTWARE\Python\PythonCore\<ver>\InstallPath`）
/// - `Environment\` —— 用户级环境变量的子键（见 [`ABSOLUTE_EXACT`] 关于裸名字的说明）
///
/// `Python\` 这条**不是**为了绕过 `SOFTWARE` 前缀，而是因为它必须**同时**用于
/// `HKCU` 与 `HKLM` 两个 hive，而 `RegHive::HklmWow6432` 会额外插一层
/// `WOW6432Node`。显式走绝对路径能让调用方说清楚"我要的是这个 hive 的根下面的 Python"。
const ABSOLUTE_PREFIXES: &[&str] = &["SYSTEM\\", "SOFTWARE\\", "Python\\", "ENVIRONMENT\\"];

/// **单段但是绝对**的子键名。见 [`strip_absolute_prefix`]。
///
/// `Environment` 在这里是因为**用户级环境变量在 `HKCU\Environment`，不在
/// `HKCU\SOFTWARE\Environment`** —— 与机器级环境变量一样，它不在 `SOFTWARE` 下面。
/// 而 `RegHive::Hkcu` 的默认行为就是往前面拼 `SOFTWARE\`。
///
/// **这条曾经漏掉，代价是一条真 bug**：用户级环境变量静默返回空表，
/// 而症状只是"报告里少了 `（user）` 那半"。
const ABSOLUTE_EXACT: &[&str] = &["ENVIRONMENT"];

/// 把绝对路径的子键原样返回，相对路径返回 `None`。
///
/// 两种写法：**带分隔符的前缀**（`SYSTEM\`…）与**单段但绝对的名字**（`Environment`）。
/// 后者必须精确匹配 —— `starts_with` 会让 `EnvironmentFoo` 也被当成绝对路径。
fn strip_absolute_prefix(subkey: &str) -> Option<String> {
    let upper = subkey.to_uppercase();
    let is_absolute = ABSOLUTE_EXACT.contains(&upper.as_str())
        || ABSOLUTE_PREFIXES
            .iter()
            .any(|prefix| upper.starts_with(*prefix));
    // 用**原样**的子键，不是大写后的 —— 注册表不区分大小写，但把用户给的路径
    // 原样传下去更容易在错误信息里认出来。
    is_absolute.then(|| subkey.to_owned())
}

/// 一个注册表值。
///
/// 序列化形状是**外部标签**（`{ sz = "C:\\x" }`），因为固定装置要手写：
/// 这样一行就能写完一条值，且类型不会被丢掉。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegValue {
    /// `REG_SZ`
    Sz(String),
    /// `REG_EXPAND_SZ` —— 值里可能含 `%VAR%`。
    ExpandSz(String),
    /// `REG_MULTI_SZ`
    MultiSz(Vec<String>),
    /// `REG_DWORD`
    Dword(u32),
    /// 其它类型，原样记录类型码。**记录而不猜测**。
    Unknown(u32),
}

impl RegValue {
    /// 类型名（`--json` 与固定装置里用，不本地化）。
    #[must_use]
    pub const fn kind_name(&self) -> &'static str {
        match self {
            Self::Sz(_) => "sz",
            Self::ExpandSz(_) => "expand-sz",
            Self::MultiSz(_) => "multi-sz",
            Self::Dword(_) => "dword",
            Self::Unknown(_) => "unknown",
        }
    }

    /// 当成字符串取。`REG_DWORD` 等非字符串类型返回 `None` ——
    /// 检测要读的（`DisplayName` / `InstallLocation` / App Paths 默认值）全是字符串。
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Sz(text) | Self::ExpandSz(text) => Some(text),
            Self::MultiSz(_) | Self::Dword(_) | Self::Unknown(_) => None,
        }
    }
}

/// 读注册表的抽象。
pub trait Registry {
    /// 一层子键名。键不存在时返回
    /// [`PlatformError::RegistryKeyMissing`] —— **那是"没有东西"，不是失败**。
    fn subkeys(&self, hive: RegHive, subkey: &str) -> Result<Vec<String>, PlatformError>;

    /// 一个键下的全部值（名 → 值）。默认值的名字是空字符串。
    fn values(&self, hive: RegHive, subkey: &str)
    -> Result<Vec<(String, RegValue)>, PlatformError>;

    /// 取单个值。找不到返回 `None`（值缺失与键缺失在这一层不区分，
    /// 因为检测对两者的处理相同：这条记录没有这个字段）。
    fn value(&self, hive: RegHive, subkey: &str, name: &str) -> Option<RegValue> {
        self.values(hive, subkey)
            .ok()?
            .into_iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
    }

    /// 键存在吗（哪怕它是空的）。
    fn key_exists(&self, hive: RegHive, subkey: &str) -> bool {
        self.subkeys(hive, subkey).is_ok() || self.values(hive, subkey).is_ok()
    }
}

/// 真实实现：`RegOpenKeyExW` + `RegEnumKeyExW` + `RegEnumValueW`。
#[derive(Debug, Default, Clone, Copy)]
pub struct RealRegistry;

impl RealRegistry {
    fn decode(raw: sys::RawRegValue) -> RegValue {
        match raw.kind {
            REG_SZ => RegValue::Sz(decode_utf16z(&raw.bytes)),
            REG_EXPAND_SZ => RegValue::ExpandSz(decode_utf16z(&raw.bytes)),
            REG_MULTI_SZ => RegValue::MultiSz(
                decode_utf16(&raw.bytes)
                    .split('\0')
                    .filter(|part| !part.is_empty())
                    .map(ToOwned::to_owned)
                    .collect(),
            ),
            REG_DWORD => RegValue::Dword(
                raw.bytes
                    .get(..4)
                    .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            ),
            other => RegValue::Unknown(other),
        }
    }
}

/// UTF-16LE 字节 → 字符串，**读到第一个 NUL 为止**（注册表字符串带结尾 NUL）。
fn decode_utf16z(bytes: &[u8]) -> String {
    decode_utf16(bytes)
        .split('\0')
        .next()
        .unwrap_or_default()
        .to_owned()
}

fn decode_utf16(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    String::from_utf16_lossy(&units)
}

impl Registry for RealRegistry {
    fn subkeys(&self, hive: RegHive, subkey: &str) -> Result<Vec<String>, PlatformError> {
        let (root, full) = hive.resolve(subkey);
        sys::reg_subkeys(root, &full).map_err(|code| registry_error(hive, subkey, code))
    }

    fn values(
        &self,
        hive: RegHive,
        subkey: &str,
    ) -> Result<Vec<(String, RegValue)>, PlatformError> {
        let (root, full) = hive.resolve(subkey);
        sys::reg_values(root, &full)
            .map(|raw| {
                raw.into_iter()
                    .map(|value| (value.name.clone(), Self::decode(value)))
                    .collect()
            })
            .map_err(|code| registry_error(hive, subkey, code))
    }
}

fn registry_error(hive: RegHive, subkey: &str, code: sys::Win32Code) -> PlatformError {
    if code == sys::ERROR_FILE_NOT_FOUND || code == sys::ERROR_PATH_NOT_FOUND {
        PlatformError::RegistryKeyMissing {
            hive: hive.as_str().to_owned(),
            subkey: subkey.to_owned(),
        }
    } else {
        PlatformError::from_win32(code, format!("{}\\{subkey}", hive.display_prefix()))
    }
}

/// 把注册表值里的 `%VAR%` 展开。
///
/// **为什么需要它**：`App Paths` 的值真的会出现 `%ProgramFiles%\...`，而 Windows 自己
/// 会展开它。不展开的话，一条真实存在的安装会被我们报成"幽灵条目" —— 这正是本票要
/// 避免的那类**静默错误**。纯函数，所以不碰任何机器状态，可以直接单测。
///
/// 未定义的变量原样保留（与 `ExpandEnvironmentStrings` 的行为一致，也**可被肉眼发现**）。
#[must_use]
pub fn expand_vars(value: &str, vars: &[(String, String)]) -> String {
    if !value.contains('%') {
        return value.to_owned();
    }
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                match vars
                    .iter()
                    .find(|(key, _)| key.eq_ignore_ascii_case(name))
                    .filter(|_| !name.is_empty())
                {
                    Some((_, replacement)) => out.push_str(replacement),
                    None => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn hive_labels_are_the_public_contract() {
        assert_eq!(RegHive::Hkcu.as_str(), "hkcu");
        assert_eq!(RegHive::Hklm.as_str(), "hklm");
        assert_eq!(RegHive::HklmWow6432.as_str(), "hklm-wow6432");
    }

    #[test]
    fn wow6432_is_the_only_hive_with_an_extra_prefix() {
        // 三个 hive 用同一个相对子键，前缀差异只在这一处 —— 这是它作为
        // "路径拼接策略"而不是"调用方约定"存在的理由。
        let subkey = r"Microsoft\Windows\CurrentVersion\Uninstall";
        assert_eq!(
            RegHive::Hkcu.resolve(subkey).1,
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"
        );
        assert_eq!(
            RegHive::Hklm.resolve(subkey).1,
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"
        );
        assert_eq!(
            RegHive::HklmWow6432.resolve(subkey).1,
            r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall"
        );
    }

    #[test]
    fn leading_and_trailing_separators_are_tolerated() {
        // 固定装置里手写的子键路径经常带多余的分隔符，归一化比要求调用方写对更稳。
        assert_eq!(
            RegHive::Hklm.resolve(r"\Microsoft\Windows\Uninstall\").1,
            r"SOFTWARE\Microsoft\Windows\Uninstall"
        );
    }

    #[test]
    fn value_kinds_are_pinned() {
        assert_eq!(REG_SZ, 1);
        assert_eq!(REG_EXPAND_SZ, 2);
        assert_eq!(REG_DWORD, 4);
        assert_eq!(REG_MULTI_SZ, 7);
        assert_eq!(RegValue::Sz(String::new()).kind_name(), "sz");
        assert_eq!(RegValue::ExpandSz(String::new()).kind_name(), "expand-sz");
        assert_eq!(RegValue::MultiSz(Vec::new()).kind_name(), "multi-sz");
        assert_eq!(RegValue::Dword(0).kind_name(), "dword");
        assert_eq!(RegValue::Unknown(3).kind_name(), "unknown");
        assert_eq!(RegValue::Dword(7).as_str(), None);
    }

    #[cfg(windows)]
    #[test]
    fn registry_type_codes_agree_with_windows_sys() {
        use windows_sys::Win32::System::Registry as r;
        assert_eq!(REG_SZ, r::REG_SZ);
        assert_eq!(REG_EXPAND_SZ, r::REG_EXPAND_SZ);
        assert_eq!(REG_DWORD, r::REG_DWORD);
        assert_eq!(REG_MULTI_SZ, r::REG_MULTI_SZ);
    }

    #[test]
    fn registry_strings_are_utf16le_and_stop_at_the_nul() {
        let bytes: Vec<u8> = "C:\\Tools"
            .encode_utf16()
            .chain(std::iter::once(0))
            .chain("垃圾".encode_utf16())
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(decode_utf16z(&bytes), "C:\\Tools");
    }

    #[test]
    fn multi_sz_splits_on_nul_and_drops_the_empty_tail() {
        let bytes: Vec<u8> = "a\0b\0\0"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(
            RealRegistry::decode(sys::RawRegValue {
                name: String::new(),
                kind: REG_MULTI_SZ,
                bytes,
            }),
            RegValue::MultiSz(vec!["a".to_owned(), "b".to_owned()])
        );
    }

    #[test]
    fn a_dword_is_little_endian() {
        assert_eq!(
            RealRegistry::decode(sys::RawRegValue {
                name: String::new(),
                kind: REG_DWORD,
                bytes: vec![0x01, 0x00, 0x00, 0x00],
            }),
            RegValue::Dword(1)
        );
    }

    #[test]
    fn unknown_types_are_recorded_with_their_code() {
        assert_eq!(
            RealRegistry::decode(sys::RawRegValue {
                name: String::new(),
                kind: 3,
                bytes: vec![1, 2, 3],
            }),
            RegValue::Unknown(3)
        );
    }

    #[test]
    fn expansion_handles_the_app_paths_case() {
        let vars = env(&[("ProgramFiles", r"C:\Program Files")]);
        assert_eq!(
            expand_vars(r"%ProgramFiles%\Git\cmd\git.exe", &vars),
            r"C:\Program Files\Git\cmd\git.exe"
        );
        // 变量名大小写不敏感（Windows 环境变量就是这样）。
        assert_eq!(
            expand_vars(r"%PROGRAMFILES%\x", &vars),
            r"C:\Program Files\x"
        );
        // 两个变量连写，以及值里带百分号的情况。
        assert_eq!(expand_vars(r"a%b%c", &env(&[("b", "B")])), "aBc");
        assert_eq!(expand_vars("100%", &vars), "100%");
        assert_eq!(expand_vars("a%", &vars), "a%");
        assert_eq!(expand_vars("%", &vars), "%");
        // 未定义的变量原样保留，并保持它本来的样子（可被肉眼发现）。
        assert_eq!(expand_vars(r"%NOPE%\x", &vars), r"%NOPE%\x");
        // 无 `%` 时快速返回。
        assert_eq!(expand_vars(r"C:\plain", &vars), r"C:\plain");
    }
}
