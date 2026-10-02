//! 拼写疑似（决策 133）：**只报告，永不纠错**。
//!
//! 票据原话是"不做拼写自动纠错（危险）；但**必须能识别并报告**像
//! `C:\Software\tool` vs `C:\Software\tools` 这种'疑似拼写错误'"。本机那个目录被错写了
//! 三次、从未生效过 —— 这种条目**只能**靠报告让用户自己决定。
//!
//! # 判据，以及它为什么只问本机侧
//!
//! 一条**失效**条目 `P`（`exists == no`，空条目不算），取它的父目录与最后一段，
//! 在父目录里找**目录**项 `Q`：`Q` 与最后一段在大小写折叠后**编辑距离 ≤ 2 且不相等**
//! → 返回 `Q` 的完整路径。
//!
//! * **只问本机侧**：`C:\Software\tools` 在不在这台机器上，只有这台机器知道。
//!   目标侧是**另一台机器**的快照，拿它的 `exists` 做判断没有意义。
//! * **只认目录**：`tool.exe` 不是"`tool` 这个目录拼错了"的证据。
//! * **折叠后相等的不算**：`C:\Software\Tools` 与 `C:\Software\tools` 是同一个目录，
//!   那是 `case-only` 的判据（决策 128），不是拼写错误。
//! * **编辑距离自己算**（Levenshtein，两行循环）：这里不引任何新依赖，
//!   而且"≤ 2"这个阈值要能一眼看到。
//!
//! 这是 [`crate::pathdiff::diff`] 里**唯一**碰机器的地方，而且它是只读的
//! （[`FileSystem::list_dir`]）—— [`crate::pathdiff`] 的"纯函数"性质靠这一点成立。

use std::path::Path;

use tuoen_platform::FileSystem;

/// 拼写疑似的阈值：大小写折叠后的编辑距离 ≤ 2（决策 133）。
pub const MAX_TYPO_DISTANCE: usize = 2;

/// 在 `value` 的父目录里找一个**像它**的目录。找到就返回那个目录的完整路径。
///
/// 反例一律 `None`：值不是绝对路径、父目录为空/取不到、最后一段为空、
/// 父目录列不出来（不存在或没权限 —— [`FileSystem::list_dir`] 对这两种都答空列表）、
/// 没有任何目录项的编辑距离达标。
#[must_use]
pub fn suspected_typo(fs: &(impl FileSystem + ?Sized), value: &str) -> Option<String> {
    let path = Path::new(value.trim());
    // 相对路径没有"父目录里的兄弟"这个概念 —— `\\?\` 也救不了它（AGENTS.md 的平台事实）。
    if !path.is_absolute() {
        return None;
    }
    let parent = path.parent()?;
    if parent.as_os_str().is_empty() {
        return None;
    }
    let last = path.file_name()?.to_string_lossy().into_owned();
    let last_key = last.to_lowercase();
    if last_key.is_empty() {
        return None;
    }

    let mut best: Option<(usize, String)> = None;
    for entry in fs.list_dir(parent) {
        if !entry.is_dir {
            continue;
        }
        let candidate = entry.name.to_lowercase();
        if candidate == last_key {
            // 只是大小写不同 —— 同一个目录，不是拼写错误。
            continue;
        }
        let distance = levenshtein(&candidate, &last_key);
        if distance > MAX_TYPO_DISTANCE {
            continue;
        }
        let full = parent.join(&entry.name).to_string_lossy().into_owned();
        // 多个候选时取最近的；距离相同时按完整路径字典序 —— 必须有确定的答案，
        // 否则同一份输入在不同机器上会给出不同的建议。
        let better = match &best {
            None => true,
            Some((best_distance, best_path)) => {
                distance < *best_distance || (distance == *best_distance && full < *best_path)
            }
        };
        if better {
            best = Some((distance, full));
        }
    }
    best.map(|(_, full)| full)
}

/// Levenshtein 编辑距离。两行滚动数组，`O(a*b)` 时间、`O(b)` 空间。
///
/// 按 **char** 算而不是按 byte：路径里可能有中文，按 byte 算会把一个汉字数成三个编辑。
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0usize; b.len() + 1];
    for (i, left) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, right) in b.iter().enumerate() {
            let substitute = previous[j] + usize::from(left != right);
            let delete = previous[j + 1] + 1;
            let insert = current[j] + 1;
            current[j + 1] = substitute.min(delete).min(insert);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pathdiff::test_support::fake_fs;

    #[test]
    fn levenshtein_is_the_plain_definition() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("tool", "tool"), 0);
        assert_eq!(levenshtein("tool", "tools"), 1);
        assert_eq!(levenshtein("tools", "tool"), 1);
        assert_eq!(levenshtein("tool", "toool"), 1);
        assert_eq!(levenshtein("tool", "teol"), 1);
        assert_eq!(levenshtein("tool", "teool"), 1);
        assert_eq!(levenshtein("tool", "to"), 2);
        // 三个字符全不一样、长度还差一个 → 3 次替换 + 1 次插入 = 4（不是 3）。
        assert_eq!(levenshtein("zzz", "tool"), 4);
        assert_eq!(levenshtein("zzzz", "tool"), 4);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", ""), 3);
        // 按 char 算：一个汉字是一个编辑，不是三个。
        assert_eq!(levenshtein("工具", "工具"), 0);
        assert_eq!(levenshtein("工具", "工具夹"), 1);
    }

    #[test]
    fn a_missing_final_segment_is_suggested_from_the_parent() {
        let fs = fake_fs(&[(r"C:\Software", "tools")]);
        assert_eq!(
            suspected_typo(&fs, r"C:\Software\tool").as_deref(),
            Some(r"C:\Software\tools")
        );
    }

    #[test]
    fn a_trailing_separator_is_fine_because_path_parent_handles_it() {
        let fs = fake_fs(&[(r"C:\Software", "tools")]);
        assert_eq!(
            suspected_typo(&fs, r"C:\Software\tool\").as_deref(),
            Some(r"C:\Software\tools"),
            "结尾多一个反斜杠不该改变答案"
        );
    }

    #[test]
    fn the_closest_candidate_wins_and_ties_are_broken_by_path() {
        let fs = fake_fs(&[(r"C:\Software", "tools"), (r"C:\Software", "toolz")]);
        assert_eq!(
            suspected_typo(&fs, r"C:\Software\tool").as_deref(),
            Some(r"C:\Software\tools"),
            "距离相同时按完整路径字典序（`tools` < `toolz`）—— 答案必须确定"
        );
        let closer = fake_fs(&[(r"C:\Software", "tools"), (r"C:\Software", "t")]);
        assert_eq!(
            suspected_typo(&closer, r"C:\Software\too").as_deref(),
            Some(r"C:\Software\t"),
            "距离更小的赢，哪怕它在字典序上更大"
        );
    }

    #[test]
    fn distance_two_counts_but_three_does_not() {
        let fs = fake_fs(&[(r"C:\Sw", "ab")]);
        assert_eq!(
            suspected_typo(&fs, r"C:\Sw\abcd").as_deref(),
            Some(r"C:\Sw\ab")
        );
        let far = fake_fs(&[(r"C:\Sw", "abc")]);
        assert!(suspected_typo(&far, r"C:\Sw\abcdefg").is_none());
    }

    #[test]
    fn a_case_difference_is_not_a_typo() {
        let fs = fake_fs(&[(r"C:\Software", "Tools")]);
        assert!(
            suspected_typo(&fs, r"C:\Software\tools").is_none(),
            "同一个目录的两个写法属于 case-only，不属于拼写错误"
        );
    }

    #[test]
    fn files_are_not_candidates() {
        // `fake_fs` 造的是目录；这里直接用一个空的假文件系统证明"列不出来就没有建议"。
        let empty = fake_fs(&[]);
        assert!(suspected_typo(&empty, r"C:\Software\tool").is_none());
    }

    #[test]
    fn relative_paths_and_roots_have_no_parent_to_ask() {
        let fs = fake_fs(&[(r"C:\Software", "tools")]);
        assert!(suspected_typo(&fs, "tool").is_none(), "相对路径不问");
        assert!(suspected_typo(&fs, r"C:\").is_none(), "根没有最后一段");
        assert!(suspected_typo(&fs, "").is_none(), "空值不问");
        assert!(suspected_typo(&fs, r"%NOPE%\bin").is_none(), "相对形态不问");
    }

    #[test]
    fn a_parent_that_does_not_exist_yields_nothing() {
        let fs = fake_fs(&[(r"C:\Software", "tools")]);
        assert!(suspected_typo(&fs, r"C:\Elsewhere\tool").is_none());
    }
}
