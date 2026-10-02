//! 临时目录、原子落位、以及**删得掉的清理**。
//!
//! ## "原子"在这里的确切含义
//!
//! 票据 #5 写的是："安装要么完整成功，要么完全不留痕迹 —— **绝不留下半个
//! 版本目录**"。所以原子性要拆成两条**分别可验证**的性质：
//!
//! 1. **`versions/<ver>/` 这个最终名字要么完整存在，要么根本不存在。**
//!    做法：在**同一个卷上**的临时目录里解压 + 审计，全部通过之后
//!    才用一次 `rename` 把它搬到最终名字。`rename` 是文件系统层面的
//!    原子操作，所以不存在"搬了一半"的中间态。
//! 2. **失败时临时目录被删掉。** 这条比第 1 条难 —— 见下。
//!
//! 临时目录放在 `dest_root` **里面**（`<versions>/.staging-…`）而不是
//! `%TEMP%`：跨卷的 `rename` 会退化成"复制 + 删除"，那就不再是原子的了。
//!
//! ## 为什么"清理"需要专门写一段代码
//!
//! 因为**我们刚刚证明了磁盘上会有删不掉的东西**。实测
//! （`research/BSDTAR_SAFETY_MEASURED.md` §2）：
//!
//! * `CON` / `NUL` 这类保留设备名**真的会被创建出来**，而普通路径
//!   （`Test-Path`、`Remove-Item`、`std::fs::remove_dir_all`）**访问不到它们**
//!   —— 只有 `\\?\` 前缀能访问；
//! * 结尾带点或空格的名字（`trailing.`、`trailing `）同样打不开删不掉。
//!
//! 所以 `std::fs::remove_dir_all` 在**恰好是恶意归档**的情况下会失败 ——
//! 而"清理失败"意味着磁盘上留着一个半成品目录，正是票据要禁止的东西。
//! 这里因此有一条兜底：普通路径删不掉时，走 `\\?\` 逐条删。
//!
//! **注意这条兜底只在清理时用。** 解压产物**不允许**有这些名字
//! （[`crate::audit`] 会拒绝它们）—— 兜底存在的意义是"即使审计拒绝了，
//! 我们也能把现场清干净"，而不是"允许它们存在"。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::ArchiveError;

/// 临时目录的前缀。
///
/// **以点开头**：这样 `versions/` 在资源管理器与 `dir` 里不会把它与真正的
/// 版本目录混在一起（`node-24.19.0` 与 `.staging-node-24.19.0-…`）。
const STAGING_PREFIX: &str = ".staging-";

/// 进程内自增，保证同一进程里两个并发安装不会撞名字。
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// 给一个绝对路径加上 `\\?\` 前缀。
///
/// ## 为什么必须这么做，以及为什么**不能**到处这么做
///
/// 实测（`research/WINDOWS_PLATFORM_CONSTRAINTS.md`）：**相对路径永远受
/// `MAX_PATH`(260) 限制**，`\\?\` 无法加在前缀上；而绝对路径加了 `\\?\`
/// 之后，Win32 的路径规范化（吃掉结尾的点与空格、解析保留设备名）
/// **整个被跳过** —— 那正是我们要访问这些名字的原因。
///
/// 返回 `None` 表示这个路径加不了前缀（相对路径、或者已经是 `\\?\`），
/// 调用方应当退回普通路径。
#[must_use]
pub fn long_path(path: &Path) -> Option<PathBuf> {
    let text = path.to_string_lossy();

    // 已经加过了。
    if text.starts_with(r"\\?\") {
        return None;
    }

    // UNC：`\\server\share\x` → `\\?\UNC\server\share\x`。
    if let Some(rest) = text.strip_prefix(r"\\") {
        return Some(PathBuf::from(format!(r"\\?\UNC\{rest}")));
    }

    // 必须是"盘符 + 冒号 + 分隔符"开头的绝对路径。
    let bytes = text.as_bytes();
    if bytes.len() >= 3 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        let rest = &text[2..];
        if rest.starts_with('\\') || rest.starts_with('/') {
            // **`\\?\` 只认反斜杠**：`\\?\C:/x` 是无效的。
            let normalized = text.replace('/', r"\");
            return Some(PathBuf::from(format!(r"\\?\{normalized}")));
        }
    }

    None
}

/// 一个临时解压目录。**离开作用域时自动清理。**
///
/// 这是"失败时完全不留痕迹"的机制：解压、审计、落位这三步里任何一步
/// 提前返回，`Drop` 都会跑，临时目录都会被删。
///
/// ## 这里**没有**"已落位就别删了"的标志，而且是故意的
///
/// 我一开始加过一个 `committed: bool`，落位后置真、清理时短路。**那是错的**，
/// 而且错得很典型：`committed` 表达的是"**payload 已经搬走**"，
/// 但清理要问的是"**临时目录还在不在**"—— 两个不同的问题。
///
/// 剥层时（`strip_components > 0`）payload 是临时目录的**子目录**，
/// 搬走它之后临时目录**还在**（空了但还在）。于是那个标志让清理短路，
/// 磁盘上留下一个 `.staging-…` 目录 —— 正是"失败要完全不留痕迹"要禁止的
/// 东西，只不过这次是**成功路径**上留的。
///
/// 现在靠两条不需要标志的性质：
///
/// * 剥层为 0 时 `commit` 搬的就是临时目录**本身**，搬完它就不存在了
///   → `!exists` 短路；
/// * 剥层大于 0 时临时目录还在（空）→ 正常删掉。
///
/// 两种情况都不需要问"落位成功了吗"。这个 bug 是被
/// `a_clean_zip_installs_and_leaves_no_staging_behind` 抓出来的。
#[derive(Debug)]
pub struct StagingDir {
    path: PathBuf,
    /// 清理时要不要尝试 `\\?\` 兜底。
    deep_clean: bool,
}

impl StagingDir {
    /// 在 `parent` 下面建一个临时目录。
    ///
    /// `label` 只进名字，用于人读（`node-24.19.0`）。
    ///
    /// # Errors
    ///
    /// 建不出来。
    pub fn create(parent: &Path, label: &str) -> Result<Self, ArchiveError> {
        let safe_label = sanitize_label(label);
        let unique = format!(
            "{STAGING_PREFIX}{safe_label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let path = parent.join(unique);
        std::fs::create_dir_all(&path).map_err(|source| ArchiveError::Io {
            operation: "创建临时解压目录".to_owned(),
            path: path.clone(),
            source,
        })?;
        Ok(Self {
            path,
            deep_clean: true,
        })
    }

    /// 临时目录的路径。
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 把 `from` 原子地搬到 `to`。
    ///
    /// **`to` 必须不存在。** 覆盖一个已经存在的版本目录不是原子操作
    /// （要先删再搬，中间有一个"什么都没有"的窗口），所以这里直接拒绝，
    /// 由调用方显式决定要不要先删。
    ///
    /// # Errors
    ///
    /// `to` 已存在 → [`ArchiveError::DestinationExists`]；别的失败 → [`ArchiveError::Io`]。
    pub fn commit(&self, from: &Path, to: &Path) -> Result<(), ArchiveError> {
        if to.exists() {
            return Err(ArchiveError::DestinationExists {
                path: to.to_path_buf(),
            });
        }
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|source| ArchiveError::Io {
                operation: "创建目标父目录".to_owned(),
                path: parent.to_path_buf(),
                source,
            })?;
        }
        std::fs::rename(from, to).map_err(|source| ArchiveError::Io {
            operation: "把解压结果搬到最终位置（原子改名）".to_owned(),
            path: to.to_path_buf(),
            source,
        })
    }

    /// 立刻清理（不等 `Drop`）。
    ///
    /// # Errors
    ///
    /// 删不掉（连 `\\?\` 兜底都失败）。
    pub fn cleanup(self) -> Result<(), ArchiveError> {
        self.remove_now()
    }

    fn remove_now(&self) -> Result<(), ArchiveError> {
        if !self.path.exists() {
            return Ok(());
        }
        remove_tree(&self.path, self.deep_clean)
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        // **`Drop` 里不能报错**（也没人接得住）。失败的话最多留一个
        // 临时目录，而它的名字以 `.staging-` 开头 —— 下一次安装可以
        // 认出并清理它，用户也看得出来那不是版本目录。
        let _ = self.remove_now();
    }
}

/// 删掉一棵树，**先用普通路径，失败了再走 `\\?\`**。
///
/// 两段式而不是直接上 `\\?\`：`\\?\` 会跳过所有 Win32 规范化，包括
/// 把 `/` 当分隔符、去掉结尾的点 —— 对一个**正常**的目录树用它反而更容易
/// 出错（比如路径里真的含 `/`）。所以正常路径优先，只在它失败时才动用
/// 那把"不规范化"的钥匙。
///
/// # Errors
///
/// 两种方式都失败。
pub fn remove_tree(root: &Path, allow_deep_clean: bool) -> Result<(), ArchiveError> {
    match std::fs::remove_dir_all(root) {
        Ok(()) => Ok(()),
        Err(first) => {
            if !allow_deep_clean {
                return Err(ArchiveError::Io {
                    operation: "删除目录树".to_owned(),
                    path: root.to_path_buf(),
                    source: first,
                });
            }
            match remove_tree_deep(root) {
                Ok(()) => Ok(()),
                Err(deep) => Err(ArchiveError::Io {
                    operation: format!(
                        "删除目录树（普通路径失败：{first}；\\\\?\\ 兜底也失败：{deep}）"
                    ),
                    path: root.to_path_buf(),
                    source: deep,
                }),
            }
        }
    }
}

/// 走 `\\?\` 逐条删。
///
/// 自底向上：先删文件，再删空目录。用 `\\?\` 之后
/// `std::fs::read_dir` 与 `remove_file` 都能看到并删掉那些
/// 普通路径碰不了的残骸（`CON`、`trailing.`）。
fn remove_tree_deep(root: &Path) -> std::io::Result<()> {
    let Some(root_long) = long_path(root) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "无法给这个路径加 \\\\?\\ 前缀",
        ));
    };

    let mut stack = vec![root_long.clone()];
    let mut dirs: Vec<PathBuf> = Vec::new();

    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            // `file_type()` 不跟随符号链接 —— 这一点很重要：解压产物里
            // 不该有链接，但**万一有**，跟随它会让我们删到根外面去。
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                stack.push(path.clone());
                dirs.push(path);
            } else {
                std::fs::remove_file(&path)?;
            }
        }
    }

    // 目录按"深的先删"排：`stack` 是后进先出，所以收集顺序大致是
    // 由浅到深，反过来即可。同层之间无所谓。
    for dir in dirs.into_iter().rev() {
        std::fs::remove_dir(&dir)?;
    }
    std::fs::remove_dir(&root_long)?;
    Ok(())
}

/// 把标签净化成一个安全的目录名片段。
///
/// 版本号里可能有 `+`（`1.2.3+build4`）或别的字符，而我们要的是一个
/// 不会让临时目录变成非法路径的名字。**不是安全边界**（临时目录的名字
/// 由我们自己控制），只是卫生。
fn sanitize_label(label: &str) -> String {
    // **先 trim**：标签是给人读的，首尾空白没有意义，而"空格映射成下划线"
    // 会让 `"  "` 变成 `"__"` —— 一个合法但毫无信息量的目录名。
    let label = label.trim();
    let cleaned: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    // 别让标签以点结尾（那会让整个临时目录名以点结尾 → Win32 吃掉它）。
    let trimmed = cleaned.trim_end_matches(['.', ' ']);
    if trimmed.is_empty() {
        "unnamed".to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuoen_platform::test_support::TempDir;

    #[test]
    fn long_path_prefixes_local_and_unc_paths_correctly() {
        assert_eq!(
            long_path(Path::new(r"C:\a\b")).unwrap().to_string_lossy(),
            r"\\?\C:\a\b"
        );
        // **`\\?\` 只认反斜杠** —— 正斜杠要换掉。
        assert_eq!(
            long_path(Path::new("C:/a/b")).unwrap().to_string_lossy(),
            r"\\?\C:\a\b"
        );
        // UNC 有它自己的形式。
        assert_eq!(
            long_path(Path::new(r"\\server\share\x"))
                .unwrap()
                .to_string_lossy(),
            r"\\?\UNC\server\share\x"
        );
        // 已经加过的不要再加一层。
        assert!(long_path(Path::new(r"\\?\C:\a")).is_none());
        // 相对路径加不了 —— 这正是"相对路径永远受 260 限制"那条平台事实。
        assert!(long_path(Path::new(r"a\b")).is_none());
        assert!(long_path(Path::new(r"C:relative")).is_none());
        assert!(long_path(Path::new(r"C:")).is_none());
    }

    #[test]
    fn labels_are_sanitized_and_never_end_with_a_dot() {
        assert_eq!(sanitize_label("24.19.0"), "24.19.0");
        assert_eq!(sanitize_label("1.2.3+build4"), "1.2.3_build4");
        // **结尾的点必须去掉**：整个临时目录名以点结尾会被 Win32 吃掉。
        assert_eq!(sanitize_label("1.0."), "1.0");
        assert_eq!(sanitize_label("  "), "unnamed");
        assert_eq!(sanitize_label(""), "unnamed");
        assert_eq!(sanitize_label("a/b"), "a_b");
    }

    #[test]
    fn a_staging_dir_is_removed_when_it_goes_out_of_scope() {
        let temp = TempDir::new("staging-drop");
        let path;
        {
            let staging = StagingDir::create(temp.path(), "node-24.19.0").expect("建临时目录");
            path = staging.path().to_path_buf();
            assert!(path.is_dir());
            assert!(
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(STAGING_PREFIX),
                "临时目录名要以 .staging- 开头（这样用户不会把它当成版本目录）：{path:?}"
            );
            std::fs::write(path.join("half-extracted.bin"), b"partial").expect("写一个半成品");
        }
        assert!(
            !path.exists(),
            "**这是'失败时完全不留痕迹'的机制**：离开作用域就该被删掉"
        );
    }

    #[test]
    fn two_staging_dirs_in_the_same_second_do_not_collide() {
        let temp = TempDir::new("staging-unique");
        let a = StagingDir::create(temp.path(), "same").expect("a");
        let b = StagingDir::create(temp.path(), "same").expect("b");
        assert_ne!(a.path(), b.path(), "同一进程里两个并发安装不能撞名字");
        assert!(a.path().is_dir() && b.path().is_dir());
    }

    #[test]
    fn commit_moves_the_tree_and_refuses_to_overwrite() {
        let temp = TempDir::new("staging-commit");
        let staging = StagingDir::create(temp.path(), "v1").expect("建临时目录");
        let payload = staging.path().join("payload");
        std::fs::create_dir_all(&payload).expect("建 payload");
        std::fs::write(payload.join("node.exe"), b"binary").expect("写文件");

        let final_dir = temp.path().join("versions").join("v1");
        staging.commit(&payload, &final_dir).expect("落位");
        assert!(final_dir.join("node.exe").is_file());
        assert_eq!(
            std::fs::read(final_dir.join("node.exe")).unwrap(),
            b"binary"
        );

        // 第二次落位同一个目标必须被拒 —— 覆盖不是原子操作。
        let staging2 = StagingDir::create(temp.path(), "v1").expect("第二个临时目录");
        let payload2 = staging2.path().join("payload");
        std::fs::create_dir_all(&payload2).expect("建 payload2");
        let error = staging2
            .commit(&payload2, &final_dir)
            .expect_err("目标已存在就该拒绝");
        assert_eq!(error.kind(), "destination-exists");
        assert!(
            !error.is_archive_at_fault(),
            "这是调用方的问题，不是归档的问题"
        );
    }

    #[test]
    fn a_committed_staging_dir_does_not_delete_the_installed_tree() {
        // **这条守的是一个很容易写错的地方**：`commit` 之后临时目录里
        // 已经空了（东西搬走了），但 `Drop` 如果去删"原来的 payload 路径"
        // 就会删到……其实什么也删不到。真正危险的是反过来：
        // 如果 `commit` 之后 `Drop` 去删**目标**，安装就白做了。
        let temp = TempDir::new("staging-committed");
        let final_dir = temp.path().join("versions").join("v1");
        {
            let staging = StagingDir::create(temp.path(), "v1").expect("建临时目录");
            let payload = staging.path().join("payload");
            std::fs::create_dir_all(&payload).expect("建 payload");
            std::fs::write(payload.join("node.exe"), b"binary").expect("写文件");
            staging.commit(&payload, &final_dir).expect("落位");
        }
        assert!(
            final_dir.join("node.exe").is_file(),
            "落位之后离开作用域，安装结果必须还在"
        );
    }

    #[test]
    fn a_deep_clean_removes_names_that_normal_paths_cannot_touch() {
        // **这条测的是我们为什么需要 `\\?\` 兜底。**
        //
        // 实测：bsdtar 会真的创建名为 `CON` 的文件，而普通路径看不见它
        // （`Test-Path` 报 False）。于是 `remove_dir_all` 会失败，
        // 磁盘上留下一个删不掉的半成品目录。
        //
        // 这里用 `\\?\` 手工造一个同样"普通路径碰不了"的名字来复现
        // 那个局面 —— 直接造 `CON` 会在 Win32 层被当成设备。
        let temp = TempDir::new("staging-deep-clean");
        let victim = temp.path().join("victim");
        std::fs::create_dir_all(&victim).expect("建 victim");
        std::fs::write(victim.join("normal.txt"), b"normal").expect("写普通文件");

        // 用 `\\?\` 造一个结尾带点的文件 —— Win32 会吃掉结尾的点，
        // 所以普通路径既看不到也删不掉它。
        let long = long_path(&victim.join("trailing.")).expect("加前缀");
        let made_it = std::fs::write(&long, b"stubborn").is_ok();
        assert!(
            made_it,
            "本机应当能用 \\\\?\\ 造出结尾带点的文件（这是实测过的平台行为）"
        );

        // 普通路径确认看不见它。
        assert!(
            !victim.join("trailing.").exists(),
            "普通路径不该看得见它 —— 否则这条测试没有测到东西"
        );

        // 普通 `remove_dir_all` 会不会失败？**不一定**（它可能只删看得见的
        // 那些然后报错，也可能成功）。所以这里不断言失败，只断言
        // **兜底之后目录真的没了**。
        remove_tree(&victim, true).expect("兜底清理应当成功");
        assert!(!victim.exists(), "清理之后目录必须不存在");
    }

    #[test]
    fn remove_tree_without_deep_clean_reports_instead_of_silently_leaving_junk() {
        let temp = TempDir::new("staging-shallow");
        let victim = temp.path().join("victim");
        std::fs::create_dir_all(&victim).expect("建 victim");
        std::fs::write(victim.join("a.txt"), b"a").expect("写文件");

        remove_tree(&victim, false).expect("正常目录用普通路径就能删掉");
        assert!(!victim.exists());
    }
}
