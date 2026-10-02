//! 解压**之后**的落盘审计：**第三层，也是权威判据**。
//!
//! ## 为什么必须有这一层
//!
//! 前两层（[`crate::name`] 的名单校验、bsdtar 自己的防线）都建立在
//! **`tar.exe -tf` 的输出**上，而那份输出是**有损的**。实测
//! （`research/BSDTAR_SAFETY_MEASURED.md` §4）：
//!
//! * bsdtar 把名字里的控制字符**转义**成 `\n` / `\r`，所以列表里看不出真相；
//! * 这个转义是**双向的** —— 真目录 `bin` + 文件 `node.exe`，与"名字里含
//!   换行的 `bin<LF>ode.exe`"，在 `-tf` 里长得**一模一样**；
//! * `-tf` **看不出条目类型**（`escape-link` 这个 symlink 在它眼里就是一行字）；
//! * bsdtar 会把 `:` **静默改名**成 `_`，所以磁盘上的名字可能与列表里的不同。
//!
//! 结论：**列表是意图，磁盘是事实。** 只有走一遍真实目录，才能说
//! "这次解压是干净的"。
//!
//! ## 这一层查什么
//!
//! 1. **reparse point**：结果里不该有任何 symlink / junction
//!    （实测 bsdtar 在 Windows 上不创建 symlink，但"它不创建"与
//!    "磁盘上没有"是两件事）；
//! 2. **普通路径看不见的名字**：实测 `CON` / `NUL` 被真的创建出来，
//!    而 `Test-Path` 对它们报 False（只有 `\\?\` 前缀能访问）——
//!    这种残骸删都删不干净；
//! 3. **名字违规**：用与解压前**同一套**判据查磁盘上的真实名字；
//! 4. **大小写碰撞**：实测 `Readme.txt` + `README.TXT` 会**静默覆盖**，
//!    只剩一个 —— 而"只剩一个"在解压后是看不见的，所以要在**同一层
//!    目录的兄弟之间**比；
//! 5. **条目数与总体积**：压缩炸弹的兜底。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tuoen_platform::FileSystem;

use crate::error::{ArchiveError, NameViolation};
use crate::limits::ExtractLimits;
use crate::name::validate_entry_name;

/// 审计通过的结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditReport {
    /// 一共看了多少个条目（文件 + 目录）。
    pub entries: usize,
    /// 文件总字节数。
    pub total_bytes: u64,
    /// 最深一层有几段路径。
    pub max_depth: usize,
}

/// 走一遍 `root`，确认它是一次干净的解压。
///
/// `root` 必须**已经存在**。它里面的所有东西都会被认为是解压产物 ——
/// 所以调用方要给一个专用的临时目录，而不是一个混着别的东西的目录。
///
/// # Errors
///
/// 见 [`ArchiveError`]：`ReparsePointInResult` / `InvisibleFile` /
/// `UnsafeResultPath` / `Io`。
pub fn audit_tree(
    fs: &dyn FileSystem,
    root: &Path,
    limits: &ExtractLimits,
) -> Result<AuditReport, ArchiveError> {
    if !fs.inspect(root).is_dir {
        return Err(ArchiveError::Io {
            operation: "审计一个不是目录的路径".to_owned(),
            path: root.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "审计的根必须是一个已经存在的目录",
            ),
        });
    }

    let mut report = AuditReport {
        entries: 0,
        total_bytes: 0,
        max_depth: 0,
    };

    // **显式栈，不用递归**：一个恶意归档可以用极深的目录把递归爆栈，
    // 而"爆栈"是一种崩溃 —— 崩溃在安全代码里就是失败。
    let mut pending: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];

    while let Some((dir, prefix)) = pending.pop() {
        let children = fs.list_dir(&dir);

        // 大小写碰撞要在**兄弟之间**查：实测 NTFS 会把 `Readme.txt` 与
        // `README.TXT` 当成同一个名字，第二个静默覆盖第一个。
        // 覆盖之后磁盘上只剩一个，所以事后是查不出来的 —— 但**解压出来的
        // 目录里如果只剩一个，我们也无从知道曾经有两个**。所以这里查的是
        // "同一层里有没有只差大小写的两个名字"，能查到的场景是解压器
        // 用了 `\\?\` 之类的方式把两个都建了出来。
        let mut seen: BTreeMap<String, String> = BTreeMap::new();

        for child in children {
            report.entries += 1;
            if report.entries > limits.max_entries {
                return Err(ArchiveError::UnsafeResultPath {
                    path: dir.join(&child.name),
                    violation: NameViolation::TooManyEntries,
                });
            }

            let relative = if prefix.is_empty() {
                child.name.clone()
            } else {
                format!("{prefix}/{}", child.name)
            };

            // ① 名字违规 —— 用与解压前**同一套**判据。
            //
            // 这里查的是磁盘上的真实名字。它比解压前那一次更权威：
            // 实测 bsdtar 会把 `:` 改成 `_`，所以磁盘上的名字可能
            // 与列表里的不同。
            let safe = validate_entry_name(&relative, limits).map_err(|violation| {
                ArchiveError::UnsafeResultPath {
                    path: dir.join(&child.name),
                    violation,
                }
            })?;
            report.max_depth = report.max_depth.max(safe.depth());

            // ② 大小写碰撞。
            let folded = child.name.to_lowercase();
            if let Some(first) = seen.get(&folded) {
                if first != &child.name {
                    return Err(ArchiveError::CaseCollision {
                        first: first.clone(),
                        second: child.name.clone(),
                    });
                }
            } else {
                seen.insert(folded, child.name.clone());
            }

            // ③ reparse point。**任何**一种都不行 —— 结果里不该有链接。
            let facts = fs.inspect(&dir.join(&child.name));
            if facts.reparse.is_link() {
                return Err(ArchiveError::ReparsePointInResult {
                    path: dir.join(&child.name),
                    kind: facts.reparse.to_string(),
                });
            }
            // 别的 reparse（App Execution Alias 之类）也不该出现在解压结果里。
            if !matches!(facts.reparse, tuoen_platform::ReparseKind::None) {
                return Err(ArchiveError::ReparsePointInResult {
                    path: dir.join(&child.name),
                    kind: facts.reparse.to_string(),
                });
            }

            if child.is_dir {
                pending.push((dir.join(&child.name), relative));
                continue;
            }

            // ④ **普通路径看不见的文件。**
            //
            // `list_dir` 用 `FindFirstFileW` 看到了它（`dir /b /a` 也能看到），
            // 而 `inspect` 走的是普通路径 —— 对 `CON` / `NUL` 这种保留设备名，
            // Win32 的路径解析会把它当成设备而不是文件，于是 `exists == false`。
            //
            // 这正是实测里"清不掉的残骸"的判据。
            if !facts.exists {
                return Err(ArchiveError::InvisibleFile {
                    path: dir.join(&child.name),
                });
            }

            // ⑤ 体积。
            report.total_bytes = report.total_bytes.saturating_add(facts.size);
            if facts.size > limits.max_entry_bytes {
                return Err(ArchiveError::UnsafeResultPath {
                    path: dir.join(&child.name),
                    violation: NameViolation::EntryTooLarge,
                });
            }
            if report.total_bytes > limits.max_total_bytes {
                return Err(ArchiveError::UnsafeResultPath {
                    path: dir.join(&child.name),
                    violation: NameViolation::TooLarge,
                });
            }
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuoen_platform::fixture::{FixtureDir, FixturePath, MachineFixture};

    /// 固定装置里的 `FixtureDir::entries` 用的是**相对该目录的单层名字**
    /// （见 `FixturePath` 的文档），所以目录与它的内容要分开写两条。
    fn stage_fixture(
        extra_dirs: Vec<FixtureDir>,
        stage_entries: Vec<FixturePath>,
    ) -> MachineFixture {
        let mut dirs = vec![FixtureDir::new(r"C:\stage", stage_entries)];
        dirs.extend(extra_dirs);
        MachineFixture {
            dirs,
            ..MachineFixture::default()
        }
    }

    #[test]
    fn a_clean_tree_passes_and_is_counted() {
        let fixture = stage_fixture(
            vec![FixtureDir::new(
                r"C:\stage\bin",
                vec![FixturePath::file("node.exe", 1234)],
            )],
            vec![FixturePath::dir("bin"), FixturePath::file("README.md", 100)],
        );
        let machine = fixture.build();
        let report = audit_tree(
            &machine.fs,
            Path::new(r"C:\stage"),
            &ExtractLimits::default(),
        )
        .expect("干净的解压结果应当通过");

        // **根目录本身不算一个条目** —— 它是容器，不是内容。
        // 所以是 bin + README.md + node.exe = 3。
        assert_eq!(report.entries, 3, "bin + README.md + node.exe");
        assert_eq!(report.total_bytes, 1334);
        assert_eq!(report.max_depth, 2, "bin/node.exe 是两段");
    }

    #[test]
    fn a_symlink_in_the_result_is_caught_even_though_tar_says_it_did_not_create_one() {
        // 这条测的是"列表说没有，磁盘上有"这种情况 —— 而它正是这一层
        // 存在的理由。固定装置直接造一个 symlink 出来。
        let fixture = stage_fixture(
            vec![],
            vec![FixturePath::symlink_dir("escape-link", r"C:\outside")],
        );
        let machine = fixture.build();
        let error = audit_tree(
            &machine.fs,
            Path::new(r"C:\stage"),
            &ExtractLimits::default(),
        )
        .expect_err("结果里有 symlink 就该失败");
        assert_eq!(error.kind(), "reparse-point-in-result");
        assert!(error.is_archive_at_fault());
    }

    #[test]
    fn a_case_collision_in_the_result_is_caught() {
        // 实测：NTFS 上 `Readme.txt` + `README.TXT` 会静默覆盖。
        // 固定装置能把两个都建出来，于是这条判据有机会生效。
        let fixture = stage_fixture(
            vec![],
            vec![
                FixturePath::file("Readme.txt", 10),
                FixturePath::file("README.TXT", 20),
            ],
        );
        let machine = fixture.build();
        let error = audit_tree(
            &machine.fs,
            Path::new(r"C:\stage"),
            &ExtractLimits::default(),
        )
        .expect_err("只差大小写的两个条目应当失败");
        assert_eq!(error.kind(), "case-collision");
        match error {
            ArchiveError::CaseCollision { first, second } => {
                // 顺序不确定（BTreeMap 按折叠后的键排），但两个名字都要在。
                let pair = [first, second];
                assert!(pair.contains(&"Readme.txt".to_owned()));
                assert!(pair.contains(&"README.TXT".to_owned()));
            }
            other => panic!("期望 CaseCollision，得到 {other}"),
        }
    }

    #[test]
    fn a_reserved_device_name_on_disk_is_caught() {
        // 实测：bsdtar 真的会创建 `CON`，而它普通路径看不见。
        let fixture = stage_fixture(vec![], vec![FixturePath::file("CON", 12)]);
        let machine = fixture.build();
        let error = audit_tree(
            &machine.fs,
            Path::new(r"C:\stage"),
            &ExtractLimits::default(),
        )
        .expect_err("磁盘上有保留设备名就该失败");
        assert_eq!(
            error.name_violation(),
            Some(NameViolation::ReservedDeviceName),
            "应当点名是哪一条规则"
        );
    }

    #[test]
    fn a_trailing_dot_on_disk_is_caught() {
        let fixture = stage_fixture(vec![], vec![FixturePath::file("trailing.", 13)]);
        let machine = fixture.build();
        let error = audit_tree(
            &machine.fs,
            Path::new(r"C:\stage"),
            &ExtractLimits::default(),
        )
        .expect_err("磁盘上有结尾的点就该失败");
        assert_eq!(
            error.name_violation(),
            Some(NameViolation::TrailingDotOrSpace)
        );
    }

    #[test]
    fn the_entry_limit_is_enforced_during_the_walk() {
        let fixture = stage_fixture(
            vec![],
            vec![
                FixturePath::file("a", 1),
                FixturePath::file("b", 1),
                FixturePath::file("c", 1),
                FixturePath::file("d", 1),
            ],
        );
        let machine = fixture.build();
        // tiny 的上限是 3 个条目。
        let error = audit_tree(&machine.fs, Path::new(r"C:\stage"), &ExtractLimits::tiny())
            .expect_err("超过条目上限就该失败");
        assert_eq!(error.name_violation(), Some(NameViolation::TooManyEntries));
    }

    #[test]
    fn the_per_entry_size_limit_is_enforced() {
        // `tiny()` 的单文件上限是 256。这条查的是**单文件**上限 ——
        // 它存在的理由是"一个条目声明 4 PB 时要在读它之前就拒掉"。
        let fixture = stage_fixture(vec![], vec![FixturePath::file("big", 5000)]);
        let machine = fixture.build();
        let error = audit_tree(&machine.fs, Path::new(r"C:\stage"), &ExtractLimits::tiny())
            .expect_err("超过单文件上限就该失败");
        assert_eq!(error.name_violation(), Some(NameViolation::EntryTooLarge));
    }

    #[test]
    fn the_total_size_limit_is_enforced_across_entries() {
        // `tiny()` 的总量上限是 1024、单文件上限是 256。
        // 所以五个 250 字节的文件：每个都合法，合起来超了。
        // **这一条与上一条不是重复的** —— 它们挡的是两种不同的炸弹。
        let fixture = stage_fixture(
            vec![],
            (0..5)
                .map(|i| FixturePath::file(&format!("f{i}"), 250))
                .collect(),
        );
        let machine = fixture.build();
        // **不能用 `tiny()`**：它的条目上限是 3，会先触发 TooManyEntries。
        // 这条测的是总量上限，所以只把总量调小、把条目数放宽。
        let limits = ExtractLimits {
            max_total_bytes: 1000,
            ..ExtractLimits::default()
        };
        let error = audit_tree(&machine.fs, Path::new(r"C:\stage"), &limits)
            .expect_err("总量超过上限就该失败");
        assert_eq!(error.name_violation(), Some(NameViolation::TooLarge));
    }

    #[test]
    fn auditing_a_missing_root_is_an_io_error_not_a_pass() {
        // **空目录也要能过，不存在的目录不能过。** 这两者混起来的话，
        // "解压什么都没产出"会被当成成功。
        let fixture = MachineFixture::default();
        let machine = fixture.build();
        let error = audit_tree(
            &machine.fs,
            Path::new(r"C:\nope"),
            &ExtractLimits::default(),
        )
        .expect_err("审计一个不存在的根必须失败");
        assert_eq!(error.kind(), "io");
        assert!(!error.is_archive_at_fault());
    }

    #[test]
    fn an_empty_directory_passes_with_zero_entries() {
        let fixture = stage_fixture(vec![], vec![]);
        let machine = fixture.build();
        let report = audit_tree(
            &machine.fs,
            Path::new(r"C:\stage"),
            &ExtractLimits::default(),
        )
        .expect("空目录是合法的（有些归档只有一个空目录）");
        assert_eq!(report.entries, 0);
        assert_eq!(report.total_bytes, 0);
    }
}
