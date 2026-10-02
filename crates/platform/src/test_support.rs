//! 测试用的最小工具。
//!
//! **为什么需要它**：ticket #11 的硬约束禁止测试读真实 `PATH` / 注册表 / 用户安装目录，
//! 所以"要验证真实 Win32 原语"的那些断言必须落在**测试自己造的临时目录**上。
//! 本模块只在 `cfg(test)` 下编译，不会进入产物。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

/// 系统临时目录下的一个唯一目录，`Drop` 时递归删除。
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// 新建。名字里带进程 id 与计数器，避免并行测试互相踩。
    #[must_use]
    pub fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!("tuoen-platform-{label}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("创建临时目录");
        Self { path }
    }

    /// 根路径。
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 拼一个子路径。
    #[must_use]
    pub fn join(&self, relative: &str) -> PathBuf {
        self.path.join(relative)
    }

    /// 写一个文件（自动建父目录），返回完整路径。
    pub fn write(&self, relative: &str, bytes: &[u8]) -> PathBuf {
        let full = self.join(relative);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("创建父目录");
        }
        std::fs::write(&full, bytes).expect("写文件");
        full
    }

    /// 建一个目录，返回完整路径。
    pub fn mkdir(&self, relative: &str) -> PathBuf {
        let full = self.join(relative);
        std::fs::create_dir_all(&full).expect("创建目录");
        full
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

impl std::fmt::Debug for TempDir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TempDir").field("path", &self.path).finish()
    }
}

/// 在 `link` 处建一个指向 `target` 的**目录联接**（junction）。
///
/// 用 `mklink /J` 而不是 `std::os::windows::fs::symlink_dir`：ADR-0001 的本机实测是
/// "未提权 + Developer Mode 关闭时 symlink 文件与目录**都失败**，而 junction 成功"，
/// 而我们要的恰恰是一个**确定是 junction** 的 fixture。
///
/// 三个标准流都接 NUL 设备（不开管道），这样它不依赖任何父进程的管道能力。
/// 返回是否真的建出来了。
#[must_use]
pub fn make_junction(link: &Path, target: &Path) -> bool {
    let status = Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    matches!(status, Ok(status) if status.success()) && link.is_dir()
}
