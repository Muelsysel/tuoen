//! `trust.toml` —— **哪个目录被信任过，以及那份 `tuoen.toml` 当时是什么内容**（决策 117/118）。
//!
//! # 信任的单位是**内容**，不是路径
//!
//! `tuoen.toml` 里写着"要跑哪些工具的哪个版本"，而它会变成子进程的 `PATH`。
//! 所以"这个目录被信任过"这句话本身没有意义 —— 有意义的是
//! **"这一份字节被信任过"**：内容一改，之前那次信任就不作数。
//!
//! 判据因此是 [`fingerprint_of_file`]：`sha256:<hex>`，输入是
//! [`normalize_for_fingerprint`] 之后的字节，而它**只做 CRLF → LF**。
//! git 在 Windows 上默认把 LF 换成 CRLF（`core.autocrlf`），
//! 于是"同一份内容、两种行尾"必须算出同一个指纹，否则每次 clone 都要重新信任；
//! 反过来，空白、BOM、末尾换行**都算内容** —— 任何"顺手清理一下"都会扩大
//! "改了内容却不触发重新信任"的面（决策 117 明确推翻票据里"去掉每行行尾空白"的写法）。
//!
//! # 四态（[`TrustState`]）
//!
//! * `Trusted`：指纹对得上；
//! * `Stale`：对不上 —— 带上**当时**与**现在**两个指纹，让人能看出是哪一边变了；
//! * `MissingFile`：拿不到指纹（`tuoen.toml` 没了、目录没了，或者读不了）；
//! * `NotTrusted`：清单里根本没有这个目录。
//!
//! # 不碰真实 `%APPDATA%`
//!
//! [`TrustFile::at_path`] 是**唯一的构造入口**，[`TrustFile::at_default_location`]
//! 只是它在默认位置上的一个薄包装。测试永远走 `at_path(temp)` ——
//! 于是"测试没有碰开发者的信任清单"是**类型与构造路径**保证的，
//! 不是靠谁记得别那么干。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::pin::{PIN_FILE_NAME, PinError, TRUST_FILE_NAME, write_atomically};

/// 信任清单的 schema 版本。
pub const TRUST_SCHEMA_VERSION: u32 = 1;

/// `[policy] source` 的取值：用户级。机器级（决策 122）**没有实现**。
const POLICY_SOURCE_USER: &str = "user";

/// 一个被信任过的目录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustEntry {
    /// 目录的绝对路径（存的是**绝对化之后**的文本，见 [`TrustFile::add`]）。
    pub path: String,
    /// 当时的指纹：`sha256:<64 位小写 hex>`。
    pub fingerprint: String,
    /// 什么时候信任的（RFC3339 UTC，秒精度，来自 `tuoen_store::now_rfc3339`）。
    ///
    /// 由**调用方**给：core 不自己取当前时间，否则"同一份输入产生同一份输出"
    /// 这条性质就没了（而这个文件的一半用途是被人读、被 diff 看）。
    pub trusted_at: String,
}

/// 一个目录相对信任清单的状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustState {
    /// 指纹对得上。
    Trusted,
    /// 对不上：`expected` 是清单里记的，`actual` 是现在算出来的。
    Stale {
        /// 清单里记的指纹。
        expected: String,
        /// 现在算出来的指纹。
        actual: String,
    },
    /// 拿不到指纹：目录里没有 `tuoen.toml`，或者目录本身不存在（**这两件事合成一态**，
    /// 因为用户要做的动作是同一条：去看一眼这个目录还在不在、pin 文件还在不在），
    /// 或者文件在但读不了。
    MissingFile,
    /// 清单里没有这个目录。
    NotTrusted,
}

impl TrustState {
    /// `--json` 里的稳定字符串，小写 kebab，**不本地化**。
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Trusted => "trusted",
            Self::Stale { .. } => "stale",
            Self::MissingFile => "missing-file",
            Self::NotTrusted => "not-trusted",
        }
    }
}

/// 信任清单。
#[derive(Debug, Clone)]
pub struct TrustFile {
    path: PathBuf,
    entries: Vec<TrustEntry>,
    policy_source: String,
}

/// 清单文件的形状。与 [`TrustFile`] 分开：这是**文件**，那是有行为的对象。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TrustDocument {
    schema_version: u32,
    #[serde(default)]
    policy: PolicySection,
    #[serde(default, rename = "trusted")]
    entries: Vec<TrustEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PolicySection {
    source: String,
}

impl Default for PolicySection {
    fn default() -> Self {
        Self {
            source: POLICY_SOURCE_USER.to_owned(),
        }
    }
}

impl TrustFile {
    /// 指向某个具体路径（测试注入的那条路）。
    #[must_use]
    pub fn at_path(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            entries: Vec::new(),
            policy_source: POLICY_SOURCE_USER.to_owned(),
        }
    }

    /// 默认位置：`%APPDATA%\tuoen\trust.toml`。
    ///
    /// 取不到 `%APPDATA%` → `None`（**不猜**：猜成 `%USERPROFILE%` 之类的另一个位置，
    /// 会写出一个下次谁都不认为该在那里的文件）。
    #[must_use]
    pub fn at_default_location() -> Option<Self> {
        default_trust_path().map(Self::at_path)
    }

    /// 读清单。
    ///
    /// **文件不存在不是错误**：返回一份空清单（"什么都没信任过"是一种正常状态，
    /// 而不是一份坏掉的清单）。
    ///
    /// # Errors
    ///
    /// * TOML 语法/字段类型错 → [`PinError::Parse`]；
    /// * `schema_version` 不认识 → [`PinError::Parse`]（不猜）；
    /// * 条目缺路径或指纹 → [`PinError::Parse`]；
    /// * 读文件失败 → [`PinError::Io`]。
    ///
    /// 返回的是**新的** `TrustFile`（`self` 是"清单在哪"的载体，加载之后才有内容）。
    pub fn load(&self) -> Result<Self, PinError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    path: self.path.clone(),
                    entries: Vec::new(),
                    policy_source: self.policy_source.clone(),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                return Err(PinError::Parse {
                    path: Some(self.path.clone()),
                    message: format!("`{TRUST_FILE_NAME}` 不是 UTF-8 文本（{error}）。"),
                });
            }
            Err(error) => {
                return Err(PinError::Io {
                    path: self.path.clone(),
                    message: error.to_string(),
                });
            }
        };
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        let document: TrustDocument = toml::from_str(text).map_err(|error| PinError::Parse {
            path: Some(self.path.clone()),
            message: format!("`{TRUST_FILE_NAME}` 读不动：{error}。"),
        })?;
        if document.schema_version != TRUST_SCHEMA_VERSION {
            return Err(PinError::Parse {
                path: Some(self.path.clone()),
                message: format!(
                    "`{TRUST_FILE_NAME}` 声明的 schema 是 v{}，本版本只认 v{TRUST_SCHEMA_VERSION} ——\
                     不猜：要么升级 tuoen，要么把这一行改回去。",
                    document.schema_version
                ),
            });
        }
        for entry in &document.entries {
            if entry.path.trim().is_empty() || entry.fingerprint.trim().is_empty() {
                return Err(PinError::Parse {
                    path: Some(self.path.clone()),
                    message: format!(
                        "`{TRUST_FILE_NAME}` 里有一条缺路径或指纹的条目：\
                         这条条目没有任何作用，删掉它（或者重新 `tuoen trust` 一次）。"
                    ),
                });
            }
        }
        Ok(Self {
            path: self.path.clone(),
            entries: document.entries,
            policy_source: document.policy.source,
        })
    }

    /// 被信任过的目录，按路径字典序（不区分大小写）。
    #[must_use]
    pub fn entries(&self) -> &[TrustEntry] {
        &self.entries
    }

    /// `[policy] source`。本版本只会写 `user`；读回来的是什么就带回来什么
    /// （**不据此改变行为**：决策 122 的机器级策略没有实现）。
    #[must_use]
    pub fn policy_source(&self) -> &str {
        &self.policy_source
    }

    /// 清单文件的位置。
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 信任一个目录（同目录再信任一次 = 更新指纹与时间）。
    ///
    /// 存进去的是**绝对化之后**的路径：清单会换机器、换工作目录地读，
    /// 一条相对的 `path = "."` 在下一次读它的时候指向别处 —— 那是"信任了不该信的东西"
    /// 的经典形状。
    pub fn add(&mut self, dir: &Path, fingerprint: &str, trusted_at: &str) {
        let canonical = absolute_dir(dir);
        let entry = TrustEntry {
            path: canonical.to_string_lossy().into_owned(),
            fingerprint: fingerprint.to_owned(),
            trusted_at: trusted_at.to_owned(),
        };
        match self
            .entries
            .iter_mut()
            .find(|existing| same_dir(Path::new(&existing.path), &canonical))
        {
            Some(existing) => *existing = entry,
            None => self.entries.push(entry),
        }
        self.sort_entries();
    }

    /// 撤销一个目录的信任。返回**是不是真的删掉了一条** ——
    /// `false` 意味着"它本来就没被信任过"，与"删掉了"是两条不同的出路。
    pub fn revoke(&mut self, dir: &Path) -> bool {
        let before = self.entries.len();
        self.entries
            .retain(|entry| !same_dir(Path::new(&entry.path), dir));
        before != self.entries.len()
    }

    /// 原子地写回清单。
    ///
    /// # Errors
    ///
    /// 目录建不出来、写不进临时文件、`rename` 失败 → [`PinError::Io`]。
    pub fn write(&self) -> Result<(), PinError> {
        let document = TrustDocument {
            schema_version: TRUST_SCHEMA_VERSION,
            policy: PolicySection {
                source: self.policy_source.clone(),
            },
            entries: self.entries.clone(),
        };
        let text = toml::to_string_pretty(&document)
            .expect("信任清单全是字符串与字符串数组，TOML 序列化不可能失败");
        write_atomically(&self.path, text.as_bytes())
    }

    /// 重算指纹，给出这个目录现在的状态（决策 118）。
    ///
    /// **每次都重算**（没有缓存，也不看 `trusted_at`）：判断信任的唯一依据是磁盘上
    /// 那串字节现在长什么样。
    #[must_use]
    pub fn state(&self, dir: &Path) -> TrustState {
        let Some(entry) = self
            .entries
            .iter()
            .find(|entry| same_dir(Path::new(&entry.path), dir))
        else {
            return TrustState::NotTrusted;
        };
        match fingerprint_of_file(&dir.join(PIN_FILE_NAME)) {
            Ok(actual) if actual == entry.fingerprint => TrustState::Trusted,
            Ok(actual) => TrustState::Stale {
                expected: entry.fingerprint.clone(),
                actual,
            },
            // 拿不到指纹（不存在 / 读不了）：见 `TrustState::MissingFile` 的文档。
            Err(_) => TrustState::MissingFile,
        }
    }

    /// 按路径字典序排（不区分大小写）：写出来的文件稳定，`git diff` 只显示真正的增删。
    fn sort_entries(&mut self) {
        self.entries.sort_by(|left, right| {
            left.path
                .to_ascii_lowercase()
                .cmp(&right.path.to_ascii_lowercase())
                .then_with(|| left.path.cmp(&right.path))
        });
    }
}

/// 默认信任清单的位置：`%APPDATA%\tuoen\trust.toml`。
///
/// `%APPDATA%` 取不到（或为空）→ `None`。**不退回任何别的目录**：见
/// [`TrustFile::at_default_location`]。
#[must_use]
pub fn default_trust_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    if base.is_empty() {
        return None;
    }
    Some(PathBuf::from(base).join("tuoen").join(TRUST_FILE_NAME))
}

/// 一个文件的信任指纹：`sha256:<64 位小写 hex>`，内容是
/// [`normalize_for_fingerprint`] 之后的字节。
///
/// # Errors
///
/// * 文件不存在 → [`PinError::MissingPin`]（"没有这个文件"与"读不动"在调用方是两回事，
///   虽然 [`TrustState`] 把两者合成一态）；
/// * 其他读失败 → [`PinError::Io`]。
pub fn fingerprint_of_file(path: &Path) -> Result<String, PinError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(PinError::MissingPin {
                path: path.to_path_buf(),
            });
        }
        Err(error) => {
            return Err(PinError::Io {
                path: path.to_path_buf(),
                message: error.to_string(),
            });
        }
    };
    Ok(format!(
        "sha256:{}",
        tuoen_download::sha256_hex(&normalize_for_fingerprint(&bytes))
    ))
}

/// 指纹的输入归一化：**只做 CRLF → LF**。
///
/// 不归一化行尾空白、不剥 BOM、不动末尾换行。孤立的 `\r`（老 Mac 行尾）也**保留** ——
/// 只认那一条真实存在的差异（git 在 Windows 上的 `core.autocrlf`），
/// 别的一律当内容（决策 117）。
#[must_use]
pub fn normalize_for_fingerprint(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut previous_was_cr = false;
    for &byte in bytes {
        // `\r\n` 只留下 `\n`；单独的 `\r` 原样留下。
        if byte == b'\n' && previous_was_cr {
            out.pop();
        }
        out.push(byte);
        previous_was_cr = byte == b'\r';
    }
    out
}

/// 两个路径是不是同一个目录：**绝对化 + 忽略大小写 + 忽略尾部分隔符**（决策 118）。
///
/// 只有这一处定义：这套判据要同时用于"信任清单里有没有这个目录"
/// 与"撤销的是不是这条"。散落三份的后果是它们迟早不"同一套"
/// （而那时用户会看到"信任了却说不信任"）。
#[must_use]
pub fn same_dir(left: &Path, right: &Path) -> bool {
    dir_key(left).eq_ignore_ascii_case(&dir_key(right))
}

/// 目录的比较键。绝对化用 `std::path::absolute`（**纯词法**，不碰文件系统：
/// 目录不存在时也要能比，"比"这个动作不该依赖"在不在"）。
fn dir_key(path: &Path) -> String {
    let absolute = absolute_dir(path);
    tuoen_platform::normalize_entry(&absolute.to_string_lossy())
}

/// 相对路径按当前工作目录绝对化；已经是绝对路径就原样（只做词法清理）。
///
/// 不调 `canonicalize`：它会解析 junction 与符号链接、并且**要求路径存在** ——
/// 而这套比较要在"目录已经被删掉"的时候也能用（[`TrustState::MissingFile`] 那条路）。
pub(crate) fn absolute_dir(dir: &Path) -> PathBuf {
    if dir.as_os_str().is_empty() {
        return std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    }
    std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pin::test_support::TempDir;

    /// 造一个"已信任的目录"：写一份 pin 文件、算指纹、记进清单。
    fn trusted_dir(name: &str, content: &[u8]) -> (TempDir, TrustFile) {
        let temp = TempDir::new(name);
        let pin = temp.write(PIN_FILE_NAME, content);
        let fingerprint = fingerprint_of_file(&pin).expect("算指纹");
        let mut trust = TrustFile::at_path(temp.join("trust.toml"));
        trust.add(temp.path(), &fingerprint, "2025-10-02T13:45:01Z");
        (temp, trust)
    }

    #[test]
    fn same_content_is_trusted() {
        let (temp, mut trust) = trusted_dir("trust-same", b"[tools]\nnode = \"24\"\n");
        // 文件内容一样、但这是**另一次读**：状态仍然是 trusted。
        assert_eq!(trust.state(temp.path()), TrustState::Trusted);

        // 重新信任一次（同目录 upsert）：条目还是一条，不是两条。
        let fingerprint = fingerprint_of_file(&temp.join(PIN_FILE_NAME)).expect("算指纹");
        trust.add(temp.path(), &fingerprint, "2025-10-03T00:00:00Z");
        assert_eq!(trust.entries().len(), 1);
        assert_eq!(trust.entries()[0].trusted_at, "2025-10-03T00:00:00Z");
        assert_eq!(trust.state(temp.path()), TrustState::Trusted);
    }

    #[test]
    fn one_changed_character_is_stale() {
        let (temp, trust) = trusted_dir("trust-stale", b"[tools]\nnode = \"24\"\n");
        let before = fingerprint_of_file(&temp.join(PIN_FILE_NAME)).expect("算指纹");
        std::fs::write(temp.join(PIN_FILE_NAME), "[tools]\nnode = \"25\"\n").expect("改一个字符");
        match trust.state(temp.path()) {
            TrustState::Stale { expected, actual } => {
                assert_eq!(expected, before);
                assert_ne!(actual, before);
                assert!(actual.starts_with("sha256:"), "{actual}");
                assert_eq!(actual.len(), "sha256:".len() + 64);
                assert!(
                    actual["sha256:".len()..]
                        .chars()
                        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
                );
            }
            other => panic!("应当是 stale，实际是 {other:?}"),
        }
    }

    #[test]
    fn crlf_and_lf_are_the_same_content() {
        // 同一份内容、两种行尾 → 同一个指纹（决策 117：git 的 autocrlf 不该逼人重新信任）。
        let windows = normalize_for_fingerprint(b"a\r\nb\r\n");
        let unix = normalize_for_fingerprint(b"a\nb\n");
        assert_eq!(windows, unix);
        assert_eq!(windows, b"a\nb\n".to_vec());

        let (temp, trust) = trusted_dir("trust-crlf", b"[tools]\r\nnode = \"24\"\r\n");
        std::fs::write(temp.join(PIN_FILE_NAME), "[tools]\nnode = \"24\"\n").expect("换成 LF");
        assert_eq!(trust.state(temp.path()), TrustState::Trusted);
    }

    #[test]
    fn bom_and_trailing_whitespace_are_content() {
        // BOM 是内容：多一个 BOM 就是另一次内容。
        assert_ne!(
            normalize_for_fingerprint(b"\xef\xbb\xbf[tools]\n"),
            normalize_for_fingerprint(b"[tools]\n")
        );
        // 行尾空白也是内容（票据里"去掉每行行尾空白"的写法被决策 117 推翻）。
        assert_ne!(
            normalize_for_fingerprint(b"[tools] \n"),
            normalize_for_fingerprint(b"[tools]\n")
        );
        // 孤立的 CR 不做归一化（只有 CRLF 那一条）。
        assert_eq!(normalize_for_fingerprint(b"a\rb"), b"a\rb".to_vec());
        // 末尾换行也是内容。
        assert_ne!(
            normalize_for_fingerprint(b"[tools]\n"),
            normalize_for_fingerprint(b"[tools]")
        );

        let (temp, trust) = trusted_dir("trust-bom", b"[tools]\nnode = \"24\"\n");
        std::fs::write(
            temp.join(PIN_FILE_NAME),
            "\u{feff}[tools]\nnode = \"24\"\n".as_bytes(),
        )
        .expect("加 BOM");
        assert!(matches!(trust.state(temp.path()), TrustState::Stale { .. }));
    }

    #[test]
    fn untrusted_then_revoked_is_untrusted_again() {
        let temp = TempDir::new("trust-revoke");
        let pin = temp.write(PIN_FILE_NAME, b"[tools]\nnode = \"24\"\n");
        let fingerprint = fingerprint_of_file(&pin).expect("算指纹");
        let mut trust = TrustFile::at_path(temp.join("trust.toml"));

        assert_eq!(trust.state(temp.path()), TrustState::NotTrusted);
        assert_eq!(trust.state(temp.path()).as_str(), "not-trusted");

        trust.add(temp.path(), &fingerprint, "2025-10-02T13:45:01Z");
        assert_eq!(trust.state(temp.path()), TrustState::Trusted);

        assert!(trust.revoke(temp.path()), "撤销一条存在的条目");
        assert_eq!(trust.state(temp.path()), TrustState::NotTrusted);
        assert!(!trust.revoke(temp.path()), "再撤销一次没有东西可撤");
    }

    #[test]
    fn missing_file_covers_no_pin_and_no_dir() {
        let temp = TempDir::new("trust-missing");
        let pin = temp.write(PIN_FILE_NAME, b"[tools]\nnode = \"24\"\n");
        let fingerprint = fingerprint_of_file(&pin).expect("算指纹");
        let mut trust = TrustFile::at_path(temp.join("trust.toml"));
        trust.add(temp.path(), &fingerprint, "2025-10-02T13:45:01Z");

        // 目录里没有 tuoen.toml。
        std::fs::remove_file(temp.join(PIN_FILE_NAME)).expect("删 pin");
        assert_eq!(trust.state(temp.path()), TrustState::MissingFile);
        assert_eq!(trust.state(temp.path()).as_str(), "missing-file");

        // 目录本身不存在（同一个值，见枚举文档）。
        let gone = TempDir::new("trust-gone");
        trust.add(gone.path(), "sha256:00", "2025-10-02T13:45:01Z");
        let gone_path = gone.path().to_path_buf();
        drop(gone);
        assert_eq!(trust.state(&gone_path), TrustState::MissingFile);
    }

    #[test]
    fn state_is_not_trusted_for_a_different_directory() {
        let (temp, trust) = trusted_dir("trust-other", b"[tools]\nnode = \"24\"\n");
        let other = TempDir::new("trust-other-2");
        assert_eq!(trust.state(other.path()), TrustState::NotTrusted);
        // 大小写与尾部分隔符不同**还是同一个目录**（决策 118）。
        let shouty = PathBuf::from(temp.path().to_string_lossy().to_uppercase());
        assert_eq!(trust.state(&shouty), TrustState::Trusted);
        let trailing = PathBuf::from(format!("{}\\", temp.path().display()));
        assert_eq!(trust.state(&trailing), TrustState::Trusted);
    }

    #[test]
    fn same_dir_judgement_lives_in_one_place() {
        let temp = TempDir::new("trust-same-dir");
        let base = temp.path();
        assert!(same_dir(
            base,
            &PathBuf::from(base.to_string_lossy().to_uppercase())
        ));
        assert!(same_dir(
            base,
            &PathBuf::from(format!("{}\\", base.display()))
        ));
        assert!(same_dir(
            base,
            &PathBuf::from(format!(
                "{}/",
                base.display().to_string().replace('\\', "/")
            ))
        ));
        assert!(!same_dir(base, &temp.join("nope")));
        // 相对路径按当前工作目录绝对化：`.` 与 cwd 是同一个目录。
        let cwd = std::env::current_dir().expect("取 cwd");
        assert!(same_dir(Path::new("."), &cwd));
    }

    #[test]
    fn write_and_load_round_trip_on_an_injected_path() {
        let temp = TempDir::new("trust-write");
        let path = temp.join("trust.toml");
        let pin_dir = TempDir::new("trust-write-target");
        let pin = pin_dir.write(PIN_FILE_NAME, b"[tools]\nnode = \"24\"\n");
        let fingerprint = fingerprint_of_file(&pin).expect("算指纹");

        let mut trust = TrustFile::at_path(&path);
        trust.add(pin_dir.path(), &fingerprint, "2025-10-02T13:45:01Z");
        trust.write().expect("写清单");

        let text = std::fs::read_to_string(&path).expect("读回文本");
        assert!(text.contains("schema_version = 1"), "{text}");
        assert!(text.contains("source = \"user\""), "{text}");
        assert!(text.contains("[[trusted]]"), "{text}");
        assert!(text.contains("sha256:"), "{text}");

        let loaded = TrustFile::at_path(&path).load().expect("读回清单");
        assert_eq!(loaded.entries().len(), 1);
        assert_eq!(loaded.entries()[0].fingerprint, fingerprint);
        assert_eq!(loaded.entries()[0].trusted_at, "2025-10-02T13:45:01Z");
        assert_eq!(loaded.policy_source(), "user");
        assert_eq!(loaded.state(pin_dir.path()), TrustState::Trusted);
        assert_eq!(loaded.path(), path.as_path());

        // 目录里只该有清单本身（临时文件已经被 rename 收走）。
        let names: Vec<String> = std::fs::read_dir(temp.path())
            .expect("列目录")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["trust.toml".to_owned()], "{names:?}");
    }

    #[test]
    fn load_without_a_file_is_an_empty_list_not_an_error() {
        let temp = TempDir::new("trust-empty");
        let trust = TrustFile::at_path(temp.join("trust.toml"))
            .load()
            .expect("文件不存在不是错误");
        assert!(trust.entries().is_empty());
        assert_eq!(trust.policy_source(), "user");
        assert_eq!(trust.state(temp.path()), TrustState::NotTrusted);
    }

    #[test]
    fn load_refuses_broken_or_unknown_documents() {
        let temp = TempDir::new("trust-broken");

        let path = temp.write("broken.toml", b"schema_version = \"one\"\n");
        let error = TrustFile::at_path(&path)
            .load()
            .expect_err("类型不对要报错");
        assert_eq!(error.code(), "pin-parse");
        assert!(
            error.message().contains("trust.toml"),
            "{}",
            error.message()
        );

        let path = temp.write("future.toml", b"schema_version = 9\n");
        let error = TrustFile::at_path(&path)
            .load()
            .expect_err("不认识的版本要报错");
        assert_eq!(error.code(), "pin-parse");
        assert!(error.message().contains("v1"), "{}", error.message());

        let path = temp.write(
            "blank.toml",
            b"schema_version = 1\n\n[[trusted]]\npath = \"\"\nfingerprint = \"\"\ntrusted_at = \"\"\n",
        );
        let error = TrustFile::at_path(&path).load().expect_err("空条目要报错");
        assert_eq!(error.code(), "pin-parse");
    }

    #[test]
    fn entries_are_sorted_by_path() {
        let temp = TempDir::new("trust-sorted");
        let mut trust = TrustFile::at_path(temp.join("trust.toml"));
        trust.add(&temp.join("b"), "sha256:b", "2025-10-02T13:45:01Z");
        trust.add(&temp.join("a"), "sha256:a", "2025-10-02T13:45:01Z");
        trust.add(&temp.join("c"), "sha256:c", "2025-10-02T13:45:01Z");
        let paths: Vec<String> = trust
            .entries()
            .iter()
            .map(|entry| entry.path.to_ascii_lowercase())
            .collect();
        assert_eq!(paths.len(), 3);
        assert!(
            paths[0].ends_with("\\a") || paths[0].ends_with("/a"),
            "{paths:?}"
        );
        assert!(
            paths[1].ends_with("\\b") || paths[1].ends_with("/b"),
            "{paths:?}"
        );
        assert!(
            paths[2].ends_with("\\c") || paths[2].ends_with("/c"),
            "{paths:?}"
        );

        // 大小写不同但是**同一个目录** → upsert 成一条（决策 118 的路径比较）。
        trust.add(&temp.join("A"), "sha256:uppercase", "2025-10-02T13:45:02Z");
        assert_eq!(trust.entries().len(), 3, "不该多出一条");
        assert_eq!(
            trust
                .entries()
                .iter()
                .filter(|entry| entry.fingerprint == "sha256:uppercase")
                .count(),
            1,
            "指纹应当被更新，而不是新增"
        );
    }

    #[test]
    fn add_stores_an_absolute_path() {
        let temp = TempDir::new("trust-absolute");
        let mut trust = TrustFile::at_path(temp.join("trust.toml"));
        trust.add(Path::new("."), "sha256:x", "2025-10-02T13:45:01Z");
        let stored = Path::new(&trust.entries()[0].path);
        assert!(stored.is_absolute(), "{}", trust.entries()[0].path);
        assert!(same_dir(stored, &std::env::current_dir().expect("cwd")));
    }

    #[test]
    fn default_trust_path_is_built_from_appdata_only() {
        // 只断言**形状**：不读真实文件、不碰真实 %APPDATA%\tuoen（票据硬约束）。
        // 唯一被读的是本进程的环境变量，不是注册表。
        let appdata = std::env::var_os("APPDATA");
        match (default_trust_path(), appdata) {
            (Some(path), Some(base)) => {
                assert!(path.ends_with(TRUST_FILE_NAME), "{}", path.display());
                assert!(
                    path.parent().expect("父目录").ends_with("tuoen"),
                    "{}",
                    path.display()
                );
                assert_eq!(
                    path.parent().expect("父目录").parent().expect("祖父"),
                    Path::new(&base)
                );
            }
            (None, None) => {}
            (None, Some(base)) if base.is_empty() => {}
            other => panic!("%APPDATA% 存在时应当给出一个位置：{other:?}"),
        }
    }

    #[test]
    fn trust_state_strings_are_stable() {
        assert_eq!(TrustState::Trusted.as_str(), "trusted");
        assert_eq!(
            TrustState::Stale {
                expected: "sha256:a".to_owned(),
                actual: "sha256:b".to_owned()
            }
            .as_str(),
            "stale"
        );
        assert_eq!(TrustState::MissingFile.as_str(), "missing-file");
        assert_eq!(TrustState::NotTrusted.as_str(), "not-trusted");
    }

    #[test]
    fn fingerprint_matches_the_download_crate_for_the_same_bytes() {
        // 指纹的算法与 download 那份是同一个（只有 `sha256:` 前缀是这一层加的）。
        let temp = TempDir::new("trust-sha");
        let path = temp.write("tuoen.toml", b"[tools]\nnode = \"24\"\n");
        let expected = format!(
            "sha256:{}",
            tuoen_download::sha256_hex(b"[tools]\nnode = \"24\"\n")
        );
        assert_eq!(fingerprint_of_file(&path).expect("算指纹"), expected);
        assert_eq!(expected.len(), "sha256:".len() + 64);

        match fingerprint_of_file(&temp.join("nope.toml")).expect_err("不存在") {
            PinError::MissingPin { path: reported } => assert!(reported.ends_with("nope.toml")),
            other => panic!("变体错了：{other:?}"),
        }
    }
}
