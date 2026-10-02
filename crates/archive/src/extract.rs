//! 编排：**校验 → 解压 → 审计 → 原子落位**。
//!
//! 这是票据 #5 的主入口。它把三层防线串起来，并保证"失败时完全不留痕迹"。
//!
//! ## 顺序不能换
//!
//! ```text
//! ① -tf 列名字      → 逐条校验（我们的判据，给出具体违规）
//! ② 名字之间查大小写碰撞（必须在解压前 —— 覆盖之后就看不出来了）
//! ③ -tvf 扫条目类型  → 有 symlink / 硬链接 / 设备就整个拒绝
//! ④ 建临时目录（在目标同一个卷上）
//! ⑤ tar -xf         → **要求退出码 0**
//! ⑥ 审计落盘结果     → 权威判据
//! ⑦ rename 到最终名字（原子）
//! ⑧ 清理临时目录
//! ```
//!
//! ③ 必须在 ⑤ 之前：实测 bsdtar 遇到 symlink 会拒绝创建并让退出码变成 1，
// 于是磁盘上留下的是**半个目录**。与其解压一半再失败，不如先拒绝。
//!
//! ⑥ 必须在 ⑦ 之前：审计不通过时，最终名字**从来没有出现过**。
//!
//! ## `strip_components` 的处理方式，以及它的限制
//!
//! `strip_components = 1` 的意思是"归档里那层顶层目录不是内容的一部分"
//! （`node-v24.19.0-win-x64/bin/node.exe` → `bin/node.exe`）。
//!
//! 实现方式：**要求所有条目共享同一个前 N 段前缀**，然后直接把那一层
//! 目录搬到最终位置。这是**唯一一种不需要逐个搬文件**的剥法，
//! 而它覆盖了全部真实制品（node 的 zip、Temurin 的 tar.gz 都是"一个顶层目录"）。
//!
//! 条目不共享前缀时**明确报错**，不猜。逐个搬文件的做法要处理
//! "两个不同前缀合并进同一层"的冲突，而那是一个新的攻击面
//! （两个归档条目映射到同一个目标名）—— 不做。

use std::path::{Path, PathBuf};
use std::time::Duration;

use tuoen_platform::{FileSystem, ProcessRunner};

use crate::audit::{AuditReport, audit_tree};
use crate::error::ArchiveError;
use crate::limits::ExtractLimits;
use crate::name::{SafeName, validate_entry_name};
use crate::staging::StagingDir;
use crate::tar::TarCli;

/// 一次安装要做的事。
#[derive(Debug, Clone)]
pub struct InstallRequest {
    /// 归档文件。
    pub archive: PathBuf,
    /// 版本目录的父目录（`<store>/versions`）。
    pub dest_root: PathBuf,
    /// 版本名（`24.19.0`），最终落在 `dest_root/<version>`。
    pub version: String,
    /// 剥掉几层顶层目录。
    pub strip_components: usize,
    /// 上限。
    pub limits: ExtractLimits,
    /// 解压超时。
    pub timeout: Duration,
}

impl InstallRequest {
    /// 最常用的那一组：不剥层、默认上限、默认超时。
    #[must_use]
    pub fn new(
        archive: impl Into<PathBuf>,
        dest_root: impl Into<PathBuf>,
        version: impl Into<String>,
    ) -> Self {
        Self {
            archive: archive.into(),
            dest_root: dest_root.into(),
            version: version.into(),
            strip_components: 0,
            limits: ExtractLimits::default(),
            timeout: crate::tar::DEFAULT_TIMEOUT,
        }
    }

    /// 设 `strip_components`。
    #[must_use]
    pub fn stripping(mut self, n: usize) -> Self {
        self.strip_components = n;
        self
    }

    /// 最终安装路径。
    #[must_use]
    pub fn destination(&self) -> PathBuf {
        self.dest_root.join(&self.version)
    }
}

/// 一次成功的安装。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallOutcome {
    /// 装到哪了。
    pub installed_to: PathBuf,
    /// 归档里有多少个条目。
    pub listed_entries: usize,
    /// 审计结论。
    pub audit: AuditReport,
    /// 临时目录用过又清掉的那个路径（留在报告里，方便解释"东西从哪来"）。
    pub staging: PathBuf,
}

/// 解压 + 安装一个归档。
///
/// # Errors
///
/// 见 [`ArchiveError`]。**任何一条错误都意味着磁盘上没有留下半个版本目录。**
pub fn install_archive(
    runner: &dyn ProcessRunner,
    fs: &dyn FileSystem,
    tar: &TarCli,
    request: &InstallRequest,
) -> Result<InstallOutcome, ArchiveError> {
    // ---- ① 列名字并逐条校验 ----
    let listed = tar.list_names(runner, &request.archive)?;
    if listed.len() > request.limits.max_entries {
        return Err(ArchiveError::UnsafeEntry {
            entry: format!("（共 {} 个条目）", listed.len()),
            violation: crate::error::NameViolation::TooManyEntries,
        });
    }

    let mut names: Vec<SafeName> = Vec::with_capacity(listed.len());
    for (index, entry) in listed.iter().enumerate() {
        // 非 UTF-8 的名字**单独报**：因为没法把它显示出来，
        // 而"猜出来的名字"正是我们不想要的。
        if entry.raw_name.contains('\u{fffd}') {
            return Err(ArchiveError::NonUtf8Entry {
                index: index + 1,
                bytes: entry.raw_name.as_bytes()[..entry.raw_name.len().min(64)].to_vec(),
            });
        }
        let safe = validate_entry_name(&entry.raw_name, &request.limits).map_err(|violation| {
            ArchiveError::UnsafeEntry {
                entry: entry.raw_name.clone(),
                violation,
            }
        })?;
        names.push(safe);
    }

    // ---- ② 大小写碰撞（必须在解压前） ----
    detect_case_collisions(&names)?;

    // ---- ③ 扫条目类型 ----
    let non_regular = tar.scan_non_regular_kinds(runner, &request.archive)?;
    if let Some(kind) = non_regular.first() {
        return Err(ArchiveError::NonRegularEntry {
            kind: kind.as_str().to_owned(),
        });
    }

    // ---- ④ 临时目录（在目标同一个卷上） ----
    std::fs::create_dir_all(&request.dest_root).map_err(|source| ArchiveError::Io {
        operation: "创建版本目录的父目录".to_owned(),
        path: request.dest_root.clone(),
        source,
    })?;
    let staging = StagingDir::create(&request.dest_root, &request.version)?;
    let staging_path = staging.path().to_path_buf();

    // ---- ⑤ 解压 ----
    tar.extract(runner, &request.archive, staging.path(), request.timeout)?;

    // ---- ⑥ 定出 payload 根并审计 ----
    let payload = payload_root(staging.path(), &names, request.strip_components)?;
    let audit = audit_tree(fs, &payload, &request.limits)?;

    // ---- ⑦ 原子落位 ----
    let destination = request.destination();
    staging.commit(&payload, &destination)?;

    // ---- ⑧ 清理（`Drop` 会做，这里显式做一次好把错误报出来） ----
    staging.cleanup()?;

    Ok(InstallOutcome {
        installed_to: destination,
        listed_entries: names.len(),
        audit,
        staging: staging_path,
    })
}

/// 找出所有"只差大小写"的条目对。
///
/// **必须在解压前查。** 实测（`research/BSDTAR_SAFETY_MEASURED.md` §2）：
/// NTFS 默认大小写不敏感，于是 `Readme.txt` + `README.TXT` 解压后
/// **只剩一个文件，内容是后一个条目的** —— 归档可以借此偷偷替换掉
/// 自己的合法文件。而覆盖发生之后，磁盘上已经看不出曾经有两个了。
fn detect_case_collisions(names: &[SafeName]) -> Result<(), ArchiveError> {
    // 用 `BTreeMap` 而不是 `HashMap`：报告里的顺序要稳定，否则同一个归档
    // 两次运行可能报出不同的"第一对"。
    let mut seen: std::collections::BTreeMap<String, &str> = std::collections::BTreeMap::new();
    for name in names {
        let folded = name.as_str().to_lowercase();
        if let Some(first) = seen.get(&folded) {
            if *first != name.as_str() {
                return Err(ArchiveError::CaseCollision {
                    first: (*first).to_owned(),
                    second: name.as_str().to_owned(),
                });
            }
        } else {
            seen.insert(folded, name.as_str());
        }
    }
    Ok(())
}

/// 从归档条目名推出"解压出来之后，真正的内容在哪一层"。
///
/// `strip_components == 0` 时就是临时目录本身。
/// 大于 0 时要求所有条目共享同一个前 N 段前缀（见模块文档）。
fn payload_root(
    staging: &Path,
    names: &[SafeName],
    strip_components: usize,
) -> Result<PathBuf, ArchiveError> {
    if strip_components == 0 {
        return Ok(staging.to_path_buf());
    }

    let mut prefixes: Vec<String> = Vec::new();
    for name in names {
        if name.strip_components(strip_components).is_none() {
            // 这个条目比要剥的层数还短 —— 剥完它就不存在了。
            //
            // **目录条目可以忽略**：每个 tar/zip 都有 `topdir/` 这一条
            // （实测 `tar.exe -tf` 把它列成 `node-v24.19.0-win-x64/`），
            // 剥掉 1 层之后它本来就该消失。把它当成错误会让
            // **每一个真实归档**都装不上。
            //
            // **文件条目不能忽略**：那意味着归档里有个顶层文件会被
            // 静默丢掉 —— "内容凭空消失"是必须报出来的。
            if name.is_dir_entry() {
                continue;
            }
            return Err(ArchiveError::UnsafeEntry {
                entry: name.as_str().to_owned(),
                violation: crate::error::NameViolation::TooDeep,
            });
        }
    }

    // 取"被剥掉的那部分"的**去重集合**。
    for name in names {
        if name.depth() < strip_components {
            continue; // 上面已经确认过它是个目录条目
        }
        let prefix = name
            .as_str()
            .split('/')
            .take(strip_components)
            .collect::<Vec<_>>()
            .join("/");
        if !prefixes.contains(&prefix) {
            prefixes.push(prefix);
        }
    }

    match prefixes.len() {
        0 => Ok(staging.to_path_buf()),
        1 => Ok(staging.join(prefixes[0].replace('/', std::path::MAIN_SEPARATOR_STR))),
        _ => Err(ArchiveError::UnsafeEntry {
            entry: prefixes.join("、"),
            violation: crate::error::NameViolation::EmptyOrDotSegment,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(raw: &[&str]) -> Vec<SafeName> {
        raw.iter()
            .map(|r| validate_entry_name(r, &ExtractLimits::default()).expect(r))
            .collect()
    }

    #[test]
    fn case_collisions_are_detected_before_anything_is_written() {
        let error = detect_case_collisions(&names(&["Readme.txt", "README.TXT"]))
            .expect_err("只差大小写的两个条目必须被拒");
        assert_eq!(error.kind(), "case-collision");
        match error {
            ArchiveError::CaseCollision { first, second } => {
                assert_eq!(first, "Readme.txt");
                assert_eq!(second, "README.TXT");
            }
            other => panic!("期望 CaseCollision，得到 {other}"),
        }
    }

    #[test]
    fn a_collision_across_directories_is_not_a_collision() {
        // `a/Readme.txt` 与 `b/README.TXT` 是两个不同的文件 ——
        // 它们不在同一层，NTFS 不会把它们当成同一个。
        detect_case_collisions(&names(&["a/Readme.txt", "b/README.TXT"]))
            .expect("不同目录下的同名文件不是碰撞");
        // 同一层的目录与文件也不能只差大小写。
        let error = detect_case_collisions(&names(&["Bin/node.exe", "bin/node.exe"]))
            .expect_err("同一层只差大小写就是碰撞");
        assert_eq!(error.kind(), "case-collision");
    }

    #[test]
    fn the_same_name_twice_is_not_reported_as_a_case_collision() {
        // 完全相同的名字出现两次是"归档里有重复条目"，不是大小写碰撞。
        // **两者是不同的上游问题**，所以不能混报 —— 但这里只断言
        // 不会误报成 case-collision（重复条目由解压器自己处理）。
        detect_case_collisions(&names(&["node.exe", "node.exe"]))
            .expect("同名重复不该报成大小写碰撞");
    }

    #[test]
    fn payload_root_is_the_staging_dir_when_nothing_is_stripped() {
        let root = payload_root(
            Path::new(r"C:\stage"),
            &names(&["bin/node.exe", "README.md"]),
            0,
        )
        .expect("不剥层时 payload 就是临时目录");
        assert_eq!(root, Path::new(r"C:\stage"));
    }

    #[test]
    fn payload_root_walks_into_the_shared_prefix() {
        // 这是 node 的真实形状：归档里全在一个顶层目录下。
        let root = payload_root(
            Path::new(r"C:\stage"),
            &names(&[
                "node-v24.19.0-win-x64/node.exe",
                "node-v24.19.0-win-x64/npm.cmd",
                "node-v24.19.0-win-x64/node_modules/npm/package.json",
            ]),
            1,
        )
        .expect("共享前缀时应当走进去");
        assert_eq!(root, Path::new(r"C:\stage\node-v24.19.0-win-x64"));
    }

    #[test]
    fn payload_root_refuses_to_guess_when_prefixes_differ() {
        // **这一条是刻意的限制。** 剥层之后 `a/x` 与 `b/y` 会落在同一层，
        // 而"把两个不同目录合并进同一层"要处理名字冲突 —— 那是一个
        // 新的攻击面。所以这里明确报错，不猜。
        let error = payload_root(Path::new(r"C:\stage"), &names(&["a/x.txt", "b/y.txt"]), 1)
            .expect_err("前缀不一致时必须报错，不能猜");
        assert!(error.to_string().contains("a"), "{error}");
        assert!(error.to_string().contains("b"), "{error}");
    }

    #[test]
    fn payload_root_refuses_when_an_entry_is_shorter_than_the_strip_count() {
        // 归档里有个顶层文件 `README.md`，而我们要剥 1 层 ——
        // 剥完它就不存在了。**静默忽略它是不行的**：那意味着
        // "归档里的一个条目凭空消失"。
        let error = payload_root(
            Path::new(r"C:\stage"),
            &names(&["node-v24.19.0-win-x64/node.exe", "README.md"]),
            1,
        )
        .expect_err("有条目比剥层数还短时必须报错");
        assert!(error.to_string().contains("README.md"), "{error}");
    }

    #[test]
    fn destination_is_dest_root_joined_with_the_version() {
        let request = InstallRequest::new(r"C:\store\x.zip", r"C:\store\versions", "24.19.0");
        assert_eq!(
            request.destination(),
            PathBuf::from(r"C:\store\versions").join("24.19.0")
        );
        assert_eq!(request.strip_components, 0);
        assert_eq!(request.limits, ExtractLimits::default());
    }
}
