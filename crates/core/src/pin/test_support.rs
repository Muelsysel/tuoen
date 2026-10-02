//! `pin` 这一层的**假装置**。
//!
//! 票据的硬约束是"所有用例走假装置，不许碰真机"：
//!
//! * 绝不读真实 `HKCU\Environment` / 真实 `PATH` / 真实 `%APPDATA%\tuoen`；
//! * 绝不翻转真实 junction；
//! * 真实临时目录 + `std::fs` 是允许的（`%TEMP%` 下自建、自删）。
//!
//! # 两条"假"的边界
//!
//! * **存储那一侧是"真的目录，假的 store"**：[`FakeInstall`] 在临时目录里搭出
//!   `root/<tool>/versions/<version>/…` 与旁边的 `<version>.json`
//!   （`store` 的记录格式由 `write_record` 自己写，不手拼 JSON ——
//!   手拼的那份会随 store 的 schema 悄悄过期，而测试会继续通过）。
//!   用真目录的理由很直接：[`tuoen_store::installed_versions`] 是**被测代码的输入**，
//!   而它的判据就是"磁盘上有没有这个目录"。
//! * **文件系统那一侧是可注入的假装置**：[`FakeDirs`] 让"某个目录里有哪些文件"
//!   变成一张表，于是遮蔽判定（决策 120）可以在不看真机的情况下被断言。
//!   它同时证明 [`ShellPlan::build_with`] 那条注入路径是真的通的。
//!
//! 这里**没有**"出厂二进制的后门开关"：这些装置只在 `cfg(test)` 与 dev-dependencies
//! 里被引用，产品代码拿不到 `%TEMP%` 之外的任何"假装"。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::detect::{Confidence, DetectedTool, DetectionSource};
use crate::pin::resolve::ResolvedTool;
use tuoen_platform::{DirEntryFacts, FileFacts, FileSystem, ReparseKind};

/// 自建、自删的临时目录。
///
/// **只碰 `%TEMP%` 下面自己建的那一层**：名字带上进程 id 与一个自增序号，
/// 于是并发的用例（同一个进程里的多个测试线程）不会互相踩；`Drop` 里整个删掉。
#[derive(Debug)]
pub struct TempDir {
    path: PathBuf,
}

/// 同一进程里多个 `TempDir` 的区分号。
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

impl TempDir {
    /// 建一个临时目录。`prefix` 只影响目录名（方便出事时认出来是谁留下的）。
    #[must_use]
    pub fn new(prefix: &str) -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "tuoen-pin-{prefix}-{}-{nanos}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("建临时目录");
        Self { path }
    }

    /// 目录本身。
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 拼一条路径（**不创建**）。
    #[must_use]
    pub fn join(&self, relative: &str) -> PathBuf {
        self.path
            .join(relative.replace('/', std::path::MAIN_SEPARATOR_STR))
    }

    /// 建目录（连父目录一起）。
    pub fn mkdir(&self, relative: &str) -> PathBuf {
        let path = self.join(relative);
        std::fs::create_dir_all(&path).expect("建目录");
        path
    }

    /// 写一个文件（连父目录一起），返回它的路径。
    pub fn write(&self, relative: &str, bytes: &[u8]) -> PathBuf {
        let path = self.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("建父目录");
        }
        std::fs::write(&path, bytes).expect("写文件");
        path
    }

    /// 写一段文本（连父目录一起）。
    pub fn write_text(&self, relative: &str, text: &str) -> PathBuf {
        self.write(relative, text.as_bytes())
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // 删不掉就算了：测试的清理失败不该盖住测试真正的结论。
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// 一个假的"我们装过的版本"：在临时目录里搭出 store 的目录形状与记录。
#[derive(Debug, Clone)]
pub struct FakeInstall {
    tool: String,
    version: String,
    files: Vec<String>,
    bin: BTreeMap<String, String>,
    home: Option<String>,
    sha256: String,
}

impl FakeInstall {
    /// `tool@version`。
    #[must_use]
    pub fn new(tool: &str, version: &str) -> Self {
        Self {
            tool: tool.to_owned(),
            version: version.to_owned(),
            files: Vec::new(),
            bin: BTreeMap::new(),
            home: None,
            // 64 位小写 hex：与真实记录同形状（`3f…` 那种真哈希在测试里没有意义，
            // 但"长度与字符集对得上"能让格式化那几条断言有意义）。
            sha256: "0000000000000000000000000000000000000000000000000000000000000001".to_owned(),
        }
    }

    /// 只落一个文件。
    #[must_use]
    pub fn file(mut self, relative: &str) -> Self {
        self.files.push(relative.to_owned());
        self
    }

    /// 落一个文件**并且**把它记进 `layout.bin`（命令名 → 归档内相对路径）。
    #[must_use]
    pub fn command(mut self, command: &str, relative: &str) -> Self {
        self.files.push(relative.to_owned());
        self.bin.insert(command.to_owned(), relative.to_owned());
        self
    }

    /// 设 `layout.home`。
    #[must_use]
    pub fn home(mut self, relative: &str) -> Self {
        self.home = Some(relative.to_owned());
        self
    }

    /// 换一个记录里的哈希。
    #[must_use]
    pub fn sha256(mut self, hex: &str) -> Self {
        self.sha256 = hex.to_owned();
        self
    }

    /// 在 `store_root` 下搭出这个版本，返回**载荷目录**
    /// （`<root>/<tool>/versions/<version>`）。
    pub fn install(&self, store_root: &Path) -> PathBuf {
        let store = tuoen_store::Store::new(store_root);
        let version_dir = store.version_dir(&self.tool, &self.version);
        std::fs::create_dir_all(&version_dir).expect("建版本目录");
        for file in &self.files {
            let path = version_dir.join(file.replace('/', std::path::MAIN_SEPARATOR_STR));
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("建载荷子目录");
            }
            std::fs::write(&path, b"").expect("写载荷文件");
        }
        let record = tuoen_store::InstallRecord {
            schema_version: tuoen_store::RECORD_SCHEMA_VERSION,
            tool: self.tool.clone(),
            version: self.version.clone(),
            display_name: self.tool.clone(),
            source_id: "official".to_owned(),
            url: "https://example.invalid/fixture.zip".to_owned(),
            sha256: self.sha256.clone(),
            archive: "fixture.zip".to_owned(),
            installed_at: "2025-10-02T13:45:01Z".to_owned(),
            payload_files: self.files.len() as u64,
            payload_bytes: 0,
            layout: tuoen_manifest::Layout {
                strip_components: 1,
                bin: self.bin.clone(),
                home: self.home.clone(),
            },
        };
        tuoen_store::write_record(&store, &record).expect("写安装记录");
        version_dir
    }
}

/// 一条检测结果（固定装置）。
#[must_use]
pub fn detected_tool(
    name: &str,
    version: Option<&str>,
    path: &str,
    source: DetectionSource,
) -> DetectedTool {
    DetectedTool {
        name: name.to_owned(),
        version: version.map(str::to_owned),
        path: path.to_owned(),
        source,
        confidence: match source {
            DetectionSource::Tuoen => Confidence::Managed,
            DetectionSource::RegistryArp => Confidence::Registered,
            _ => Confidence::Executable,
        },
        manager: None,
        evidence: "fixture".to_owned(),
    }
}

/// 一条解析结果（固定装置）：来源固定是 `tuoen`，哈希是固定值的 `sha256:` 形状。
#[must_use]
pub fn resolved_tool(name: &str, spec: &str, version: &str, path: &Path) -> ResolvedTool {
    ResolvedTool {
        name: name.to_owned(),
        spec: spec.to_owned(),
        version: version.to_owned(),
        source: "tuoen".to_owned(),
        manager: None,
        path: path.to_path_buf(),
        hash: Some(format!("sha256:{}", "0".repeat(64))),
    }
}

/// 假的文件系统：一张"目录 → 里面的文件名"表。
///
/// 只回答"有哪些名字"，够遮蔽判定用（决策 120 问的就是这个）。
#[derive(Debug, Default, Clone)]
pub struct FakeDirs {
    dirs: BTreeMap<String, Vec<String>>,
}

impl FakeDirs {
    /// 空表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 加一个目录与它的内容。
    #[must_use]
    pub fn with_dir(mut self, dir: &Path, names: &[&str]) -> Self {
        self.dirs.insert(
            key(dir),
            names.iter().map(|name| (*name).to_owned()).collect(),
        );
        self
    }
}

fn key(path: &Path) -> String {
    tuoen_platform::normalize_entry(&path.to_string_lossy()).to_ascii_lowercase()
}

impl FileSystem for FakeDirs {
    fn inspect(&self, path: &Path) -> FileFacts {
        if self.dirs.contains_key(&key(path)) {
            FileFacts {
                exists: true,
                is_dir: true,
                ..FileFacts::missing(path)
            }
        } else {
            FileFacts::missing(path)
        }
    }

    fn list_dir(&self, path: &Path) -> Vec<DirEntryFacts> {
        self.dirs
            .get(&key(path))
            .map(|names| {
                names
                    .iter()
                    .map(|name| DirEntryFacts {
                        name: name.clone(),
                        is_dir: false,
                        reparse: ReparseKind::None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_dir_is_created_and_removed() {
        let path = {
            let temp = TempDir::new("self-check");
            assert!(temp.path().is_dir());
            temp.path().to_path_buf()
        };
        assert!(!path.exists(), "Drop 之后应当删掉");
    }

    #[test]
    fn fake_install_looks_like_a_real_install() {
        let temp = TempDir::new("fake-install");
        let version_dir = FakeInstall::new("node", "24.19.0")
            .command("node", "bin/node.exe")
            .install(temp.path());
        let store = tuoen_store::Store::new(temp.path());
        let installed = tuoen_store::installed_versions(&store, "node");
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].version, "24.19.0");
        assert_eq!(installed[0].path, version_dir);
        let record = installed[0].record.as_ref().expect("记录应当读得回来");
        assert_eq!(record.sha256, FakeInstall::new("node", "24.19.0").sha256);
        assert_eq!(
            record.layout.bin.get("node").map(String::as_str),
            Some("bin/node.exe")
        );
    }

    #[test]
    fn fake_dirs_answers_like_a_file_system() {
        let dirs = FakeDirs::new().with_dir(Path::new("C:\\a"), &["node.exe", "NPM.CMD"]);
        assert!(dirs.inspect(Path::new("c:\\a\\")).exists);
        assert!(!dirs.inspect(Path::new("C:\\b")).exists);
        let names: Vec<String> = dirs
            .list_dir(Path::new("C:\\A"))
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        assert_eq!(names, vec!["node.exe".to_owned(), "NPM.CMD".to_owned()]);
        assert!(dirs.list_dir(Path::new("C:\\nope")).is_empty());
    }

    #[test]
    fn detected_and_resolved_fixtures_are_shaped_like_the_real_ones() {
        let detected = detected_tool(
            "node",
            Some("24.19.0"),
            "C:\\Program Files\\nodejs\\node.exe",
            DetectionSource::PathResolution,
        );
        assert_eq!(detected.name, "node");
        assert_eq!(detected.version.as_deref(), Some("24.19.0"));
        assert_eq!(detected.source.as_str(), "path-resolution");
        assert_eq!(detected.confidence, Confidence::Executable);
        assert!(detected.manager.is_none());

        let resolved = resolved_tool("node", "24", "24.19.0", Path::new("C:\\node\\bin"));
        assert_eq!(resolved.source, "tuoen");
        assert_eq!(
            resolved.hash.as_deref().map(str::len),
            Some("sha256:".len() + 64)
        );
    }
}
