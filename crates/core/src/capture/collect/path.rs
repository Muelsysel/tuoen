//! `path.toml` —— `PATH` 的结构。
//!
//! **本票最重的部分。** 要求与判据（写给实现者，也写给后来读这份文件的人）：
//!
//! * `[[entry]]` 逐条覆盖**三个作用域**：机器级、用户级、以及**只在进程里**的那些。
//!   最后一个不能少：本机实测有 77 字符的启动器注入条目不在任何注册表里，
//!   而它真的在 `PATH` 上、真的占长度。输出顺序也就是这三个作用域的次序，
//!   每个作用域内按**该作用域的坐标系**升序。
//! * `raw` / `expanded` / `quoted` / `reg_type` / `has_vars` / `has_username` /
//!   `exists` / `reparse` / `link_target` / `dup_index` / `owner` 的语义见
//!   [`super::super::files::PathRow`] 的字段文档。
//! * `expanded` **只在类型是 `expand-sz` 时展开**；`sz` 的值原文就是答案。
//!   `setx` 事故的残留形状正是"`REG_SZ` 里字面含 `%VAR%`"，对它做展开会凭空
//!   造出一个不存在的路径 —— 而那条路径看起来还挺对。
//! * `exists` 在展开后还留着 `%` 时必须是 `Existence::Unknown`，**并且不去问磁盘**：
//!   那个值我们解不开，看磁盘只会得到一个答非所问的答案。
//! * **空条目**（`;;`，本机 HKLM 的 `Path` 第 21 段就是）也不是目录：`exists` 恒为
//!   `unknown`、**不问磁盘**，形状记在 `empty` 上。真机验收抓出来的：第一版让空串
//!   走正常分支，磁盘对它答"不存在"，于是它变成一条 `exists = "no"` 的失效条目 ——
//!   而"这里缺一个目录"与"这里本来就什么都没有"是两件事。判"失效条目"的判据是
//!   `!empty && exists == "no"`（真机上正好 7 条，不是 8 条）。
//! * **两套坐标系**（见 [`tuoen_platform::path::EntryRef`]）：注册表作用域的 `index`
//!   是作用域内的段号，进程注入项的 `index` 是**进程 `PATH` 内**的下标。
//!   同一份文件里两个坐标系并存，所以每一处索引都必须先看 `scope` 再解释
//!   —— 判据是 `[[effective]]` 里的引用必须能在 `entry` 里**按 `(scope, index)` 精确查到**，
//!   本文件末尾有不变量用例钉住这件事。
//! * `owner` 的判据只有一条阶梯：我们自己的根 → `tuoen`；展开后的 `%SystemRoot%` 里 →
//!   `system`；本身是 reparse point（别人的版本切换器：junction / symlink /
//!   App Execution Alias）→ `third-party`；其余 `unknown`。
//!   **故意没有"能归属到某个已检测到的工具"这一档** —— 它不在本票的判据里，
//!   而且会让同一棵树里的两条记录互相依赖（`tools.toml` 的结果影响 `path.toml`，
//!   而两条记录各自都要求确定性）。
//! * `dup_index`：按**本文件里 `[[entry]]` 的输出顺序**，对规范化后的值
//!   （去首尾空白与首尾引号、去结尾反斜杠、小写）计数 —— 它是
//!   **同值条目里的序号**（票据原话），也就是"这个值第几次出现"：
//!
//!   ```text
//!   machine C:\a   -> dup_index 0     第一次出现
//!   machine C:\a\  -> dup_index 1     去反斜杠 + 大小写之后是同一个值
//!   user    c:\A   -> dup_index 2     第三次
//!   user    C:\z   -> dup_index 0     从没出现过 → 又是第一次
//!   ```
//!
//!   两条推论：
//!   - 一个**从没重复过**的值恒为 `0`，所以"有没有重复"的判据就是 `dup_index > 0`，
//!     而**重复条目的条数**就是 `dup_index > 0` 的行数（本机真机上是 18 —— 与
//!     独立用 PowerShell 数出来的"48 条非空、30 个不同值、18 条富余"逐数对上）；
//!   - 计数**跨三个作用域**（进程注入项也参与），因为这一份文件里的重复才是
//!     用户看到的那种"同一个目录写了两遍"。
//!
//!   **它与 [`tuoen_platform::PathAnalysis::duplicates`] 不是同一个视图，两者都保留**：
//!   那个是平台层的分析结论（按 `;` 切出来的条目在**作用域内**是不是重复），
//!   这个是这一份文件里的**重复度**（跨三个作用域、含进程注入项）。
//!
//!   **第一版把序号做成了"不同的值依次编号"**（`A B C C D → 0 1 2 2 3`）：
//!   那样一来"没重复过的值"也会拿到非零号，`dup_index > 0` 的行数变成
//!   "不同值的个数 − 1"，与票据要的"重复条目计数"完全不是一回事
//!   （真机上会印 32 而不是 18）。语义只有一个，就是上面那一个。
//! * `[[effective]]` 是**真实解析顺序**的一串引用，含注入项，**顺序原样保留**：
//!   它就是 `PathAnalysis::effective`（已经写明"以进程里那一条 `PATH` 为准"），
//!   这里只做形状转换，不重新推导 —— 重新推导一遍等于把那条不变量抄成两份。
//!
//! # 一处实测出来的坑：不能用 `PathEntry::value` 当展开的输入
//!
//! 票据说的是"用 `PathEntry.value`（已去引号）去算路径"，而实测下来它**不能**用在
//! 这里：[`tuoen_platform::parse_entries`] 把 `value` 定义成**比较形态**，会顺手去掉
//! 结尾反斜杠（`C:\shared\` → `C:\shared`）。那个归一化对"这两条是不是同一个目录"
//! 是对的，对"Windows 会把这个值解析成什么"是错的 —— 拿它当输入，捕获出来的
//! `expanded` 会与注册表里的原文不一致，而这份文件是 `restore` 与 `doctor` 的输入。
//! 所以展开用的是 [`entry_value`]：从 `raw` 剥掉引号与空白，其余一个字不动。
//! `PathEntry::raw` / `quoted` 照样照抄，一个字节都没丢。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tuoen_platform::path::EntryRef;
use tuoen_platform::{EnvScope, RegType, ReparseKind};

use crate::detect::DetectContext;

use super::super::files::{EntryRefRow, Existence, PathBudgetRow, PathFile, PathRow};
use super::{Expander, has_unresolved, has_vars};

/// 采集 `PATH` 结构。
pub(crate) fn collect_path(
    ctx: &DetectContext<'_>,
    captured_at: &str,
    tuoen_roots: &[PathBuf],
) -> PathFile {
    let analysis = tuoen_platform::analyze(ctx.env, ctx.process_env, ctx.fs, None);
    // 展开只用**一份**表：同一个值在不同作用域里必须展开成同样的结果，
    // 否则"这条到底存不存在"会取决于它在哪个作用域里。
    let expander = Expander::from_context(ctx);
    let system_root = expander.get("SystemRoot").map(str::to_owned);
    // 我们自己的根比较的是**文字形式**，所以在这里就取好字符串。
    let tuoen_roots: Vec<String> = tuoen_roots
        .iter()
        .map(|root| root.to_string_lossy().into_owned())
        .collect();

    let mut raw_user_chars = 0;
    let mut raw_machine_chars = 0;
    for scope in &analysis.scopes {
        match scope.scope {
            tuoen_platform::EnvScope::User => raw_user_chars = scope.chars,
            tuoen_platform::EnvScope::Machine => raw_machine_chars = scope.chars,
            tuoen_platform::EnvScope::ProcessOnly => {}
        }
    }
    let mut file = PathFile::new(
        captured_at,
        PathBudgetRow {
            raw_user_chars,
            raw_machine_chars,
            effective_chars: analysis.budget.effective_chars,
            cliff: analysis.budget.cliff,
            remaining: analysis.budget.remaining,
            level: analysis.budget.level.as_str().to_owned(),
        },
    );

    // ── [[entry]] ─────────────────────────────────────────────────────
    //
    // 顺序就是作用域顺序，作用域内按各自的坐标系升序。
    // `dup_index` 的所有前提都在这里成立：它是**这一个循环算出来的**，
    // 所以它跟文件的输出顺序天然一致，不需要事后再扫一遍。
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();

    for scope in &analysis.scopes {
        for (index, entry) in scope.entries.iter().enumerate() {
            // 展开的输入是**条目的值**（见 [`entry_value`]），不是 `PathEntry::value`。
            let value = entry_value(&entry.raw);
            // **类型说了算**：只有 `REG_EXPAND_SZ` 才展开。
            let expanded = match scope.reg_type {
                Some(RegType::ExpandSz) => expander.expand(&value),
                Some(RegType::Sz) | None => value.clone(),
            };
            let row = build_row(
                ctx,
                system_root.as_deref(),
                &tuoen_roots,
                &mut seen,
                RowInput {
                    scope: scope.scope,
                    index,
                    raw: entry.raw.clone(),
                    value,
                    expanded,
                    quoted: entry.quoted,
                    reg_type: scope.reg_type,
                },
            );
            file.entry.push(row);
        }
    }

    // 进程注入项。**它们的下标是进程 `PATH` 内的下标**，不是这份 `process_only`
    // 表里的序号 —— 见 [`process_only_index`]，那两个数实测会错位。
    for (at, value) in analysis.process_only.iter().enumerate() {
        let index = process_only_index(&analysis.effective, ctx, value).unwrap_or(at);
        let row = build_row(
            ctx,
            system_root.as_deref(),
            &tuoen_roots,
            &mut seen,
            RowInput {
                scope: EnvScope::ProcessOnly,
                index,
                // 进程条目没有"注册表原文"，原文就是进程里那一段。
                raw: value.clone(),
                value: value.clone(),
                expanded: value.clone(),
                // 进程 `PATH` 里的值不带引号：Windows 拼环境块时不会加。
                quoted: false,
                reg_type: None,
            },
        );
        file.entry.push(row);
    }

    // ── [[effective]] ────────────────────────────────────────────────
    //
    // 顺序原样保留 —— 它就是真实解析顺序（含启动器注入项），
    // 而"机器级在前、用户级在后"只是这个顺序的一部分。
    file.effective = analysis
        .effective
        .iter()
        .map(|reference| EntryRefRow {
            scope: reference.scope,
            index: reference.index,
        })
        .collect();

    file
}

// ─────────────────────────────────────────────────────────────────────────────
// 一条 `[[entry]]` 是怎么算出来的
// ─────────────────────────────────────────────────────────────────────────────

/// [`build_row`] 的输入。**抽成结构体是因为参数太多**：一个八参数的函数
/// 在调用处读不出"哪个是哪个"，而这里的三个 `String` 恰好是意义完全不同的东西
/// （原文、值、展开值）。
struct RowInput {
    scope: EnvScope,
    index: usize,
    raw: String,
    /// 条目的**值**：去首尾空白与一对首尾引号，其余一个字不动（见 [`entry_value`]）。
    value: String,
    /// 已经按类型算好的展开值（`expand-sz` 展开过，`sz` 就是值的原文）。
    expanded: String,
    quoted: bool,
    reg_type: Option<RegType>,
}

/// 算出一条 [`PathRow`]。
///
/// 四个"不许猜"的地方都在这里：
/// 1. 展开后还留着 `%` → `Existence::Unknown`，**且不问磁盘**；
/// 2. **空条目** → `Existence::Unknown` + `empty = true`，**且不问磁盘**
///    （空串不是一个目录，见模块文档里那一条）；
/// 3. `exists == No` 时 reparse 与链接目标一律置空（"不存在的东西不是链接"）；
/// 4. `owner` 只走那一条阶梯，判不出来就 `unknown`。
///
/// 它**不做展开**：那是调用方按注册表类型决定的事（见 [`collect_path`] 的
/// `[[entry]]` 那一段），展开表不越过这一层。
fn build_row(
    ctx: &DetectContext<'_>,
    system_root: Option<&str>,
    tuoen_roots: &[String],
    seen: &mut BTreeMap<String, usize>,
    input: RowInput,
) -> PathRow {
    let RowInput {
        scope,
        index,
        raw,
        value,
        expanded,
        quoted,
        reg_type,
    } = input;

    // **答不了的题不许猜**：值里还留着 `%VAR%` 时我们连"它是不是路径"都不知道，
    // 更不该去问磁盘 —— 假文件系统会对任何未声明的路径答"不存在"，
    // 而真文件系统只会告诉我们一个与问题无关的答案。
    //
    // **空条目同理，而且这条是真机验收抓出来的**：`PATH` 里的 `;;` 是一段空字符串，
    // 它不是一个目录（Windows 的历史行为是把它当"当前目录"），所以"它存不存在"
    // 这个问题本身就不成立。第一版让它走正常分支，磁盘对 `""` 当然答"不存在"，
    // 于是它被记成 `exists = "no"` —— 真机上 8 条 `no` 里有 1 条是它，
    // 而"这里缺一个目录"与"这里本来就什么都没有"是两件事。`doctor`（票据 #13）
    // 要是照 `exists == no` 数失效条目，就会多报一条永远修不好的"坏条目"。
    let empty = value.trim().is_empty();
    let (exists, reparse, link_target) = if empty || has_unresolved(&expanded) {
        (Existence::Unknown, ReparseKind::None, None)
    } else {
        let facts = ctx.fs.inspect(Path::new(&expanded));
        if facts.exists {
            (Existence::Yes, facts.reparse, facts.link_target.clone())
        } else {
            (Existence::No, ReparseKind::None, None)
        }
    };

    let owner = owner_of(tuoen_roots, system_root, &expanded, reparse).to_owned();
    let dup_index = next_dup_index(seen, &expanded);

    PathRow {
        scope,
        index,
        owner,
        // `raw` 是**原文**：含引号与首尾空白，一个字都不动（写回时按它写）。
        raw,
        expanded,
        quoted,
        empty,
        reg_type,
        exists,
        reparse,
        link_target,
        // `has_vars` 问的是"值里有没有 `%`"，与"展开得开"、与类型都无关 ——
        // `%NOPE%\bin` 也有变量，只是我们解不开它；`REG_SZ` 里字面含 `%`
        // 也**照样**是 `true`（那正是 `setx` 事故的指纹）。
        has_vars: has_vars(&value),
        has_username: tuoen_platform::path::hardcoded_username(&value).is_some(),
        dup_index,
    }
}

/// 一条条目的**值**：剥离一对首尾引号与首尾空白，**其余一个字都不动**。
///
/// # 为什么不直接用 `PathEntry::value`
///
/// 它是**比较形态**：`parse_entries` 顺手去掉了结尾反斜杠（`C:\shared\` →`C:\shared`）。
/// 拿它当"Windows 会解析成什么"的输入，会静默改写用户的 `PATH` —— 而这份文件是
/// `restore` 与 `doctor` 的输入，一处改写会在还原时放大成一条丢掉了尾部分隔符的路径。
///
/// 所以这里只做"为了拿到值"必须做的那一步（去引号、去空白），
/// 展开的输入与 `PathEntry.value` 的差别就只剩这一个归一化。
///
/// 引号**不配对也剥**（与 `parse_entries` 一致）：`"C:\a` 这种坏数据不该让整条路径
/// 带上一个引号，那样它一定查不到。
fn entry_value(raw: &str) -> String {
    let trimmed = raw.trim();
    match trimmed
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        Some(inner) => inner.to_owned(),
        None => trimmed.to_owned(),
    }
}

/// 这个目录归谁。**只有一条阶梯、四个取值**，照抄
/// [`super::super::files::PathRow`] 的字段文档：
///
/// 1. 规范化后落在 `tuoen_roots` 里（或正好等于其中一条）→ `tuoen`；
/// 2. 落在展开后的 `%SystemRoot%` 里 → `system`；
/// 3. 本身是个 reparse point → `third-party`（别人的版本切换器：junction /
///    symlink / App Execution Alias —— 它们都是"这个目录被别的工具接管了"的形状）；
/// 4. 其余 → `unknown`。
///
/// 第 3 条**只看 reparse 位**，不看目录是不是真的存在（`exists == No` 时它是 `none`），
/// 因为"接管"这件事由 reparse point 本身表达，与能不能读到目标无关。
/// 顺序有意义：`C:\Windows` 底下当然也可能有 junction，而它仍然是 `system`。
fn owner_of<'a>(
    tuoen_roots: &[String],
    system_root: Option<&str>,
    expanded: &str,
    reparse: ReparseKind,
) -> &'a str {
    if tuoen_roots
        .iter()
        .any(|root| same_path_or_under(root, expanded))
    {
        return "tuoen";
    }
    if system_root.is_some_and(|root| same_path_or_under(root, expanded)) {
        return "system";
    }
    if reparse != ReparseKind::None {
        return "third-party";
    }
    "unknown"
}

/// 两个路径说的是不是同一个目录 —— 大小写不敏感、首尾空白与结尾反斜杠不算差别。
///
/// 与 `crates/cli/src/path_view.rs` 的 `same_path` 同一个判据（那一份是私有的，
/// 而本 crate 不许改它）。`C:\` 这种盘根削完会变成 `C:`，补回来 ——
/// 否则 `C:\` 与 `C:` 会被当成同一个东西。
fn same_path(left: &str, right: &str) -> bool {
    comparison_key(left) == comparison_key(right)
}

/// `path` 就是 `root` 本身，或者落在 `root` 底下。
fn same_path_or_under(root: &str, path: &str) -> bool {
    if root.trim().is_empty() {
        // 空的根不该匹配任何东西 —— 否则一个没配好的 `tuoen_roots` 会把整条
        // `PATH` 都认成我们自己的。
        return false;
    }
    if same_path(root, path) {
        return true;
    }
    // 按**段**比较，不是按字符串前缀：`C:\WindowsApps` 不在 `C:\Windows` 里。
    // `comparison_key` 留下的是盘根上那一个反斜杠（`c:\`），所以把根接上分隔符。
    let root = comparison_key(root);
    let path = comparison_key(path);
    match root.strip_suffix('\\') {
        Some(stem) => path
            .strip_prefix(stem)
            .is_some_and(|rest| rest.starts_with('\\')),
        None => path
            .strip_prefix(&root)
            .is_some_and(|rest| rest.starts_with('\\')),
    }
}

/// 值本身的形式：去首尾空白与一对首尾引号、小写。**结尾反斜杠保留**。
///
/// 它有两个用途，两个都要求"`C:\a\` 与 `C:\a` 是同一个目录"：
/// [`same_path`] 与 [`same_path_or_under`]（归属判定），以及跨行关联
/// （[`process_only_index`] 要在 `effective` 里认出同一个值）。
///
/// 它**不是** `dup_index` 用的那个键 —— 那个还要求去掉结尾反斜杠，见
/// [`next_dup_index`]。两张键分开是必要的：`C:\shared\` 与 `C:\shared`
/// 是同一个目录（比较时要相等），却是**两条不同的注册表条目**（计数时是两次出现）。
fn path_key(value: &str) -> String {
    comparison_key(value)
        .trim_end_matches(['\\', '/'])
        .to_owned()
}

/// 比较形态：去首尾空白与一对首尾引号、小写，**结尾反斜杠保留**。
fn comparison_key(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_end_matches(' ')
        .to_lowercase()
}

/// 同一个值在这一份文件里的第几次出现（0 = 首次）。
///
/// 键是**规范化后的值**：去尾部反斜杠 + 小写（见 [`path_key`]），
/// 所以 `C:\shared` 与 `C:\shared\` 在这里算同一个值。
///
/// # 语义只有一条：**同值条目里的序号**
///
/// 票据与 `files.rs` 的字段文档都是这句话，所以：
///
/// * 一个从没出现过的值拿到 `0`；
/// * 第二次出现拿到 `1`，第三次 `2`；
/// * 于是 `dup_index > 0` 的行数**就是**重复条目的条数（真机 18），
///   而"有没有重复"的判据就是 `dup_index > 0`。
///
/// 中间有过一版把它做成"不同的值依次编号"（`A B C C D → 0 1 2 2 3`），
/// 那一版让"没重复过的值"也拿到非零号，`dup_index > 0` 的行数变成了
/// "不同值的个数 − 1"（真机上 32），与票据要的计数不是一回事。
fn next_dup_index(seen: &mut BTreeMap<String, usize>, expanded: &str) -> usize {
    let key = path_key(expanded);
    let slot = seen.entry(key).or_insert(0);
    let index = *slot;
    *slot += 1;
    index
}

/// 一条进程注入项在**进程 `PATH`** 里的下标。
///
/// # 为什么不能拿 `process_only` 的序号当它
///
/// 那是**两套坐标系**：[`tuoen_platform::path::EntryRef`] 写着 `ProcessOnly` 的
/// `index` 是进程 `PATH` 内的位置，而 `process_only` 是**去重之后**的表 ——
/// 两个表在"注入项之前有空条目"或者"同一个目录出现两次"时就不再对齐了，
/// 而对齐不上的后果是 `[[effective]]` 里出现一个**查不到**的引用。
///
/// # 为什么也不能按"第几个注入项"去数 `effective`
///
/// **这条是实测踩出来的。** 生效顺序里 `ProcessOnly` 的那些引用**不是**按进程 `PATH`
/// 的下标排的，甚至不都是注入项：进程 `PATH` 里的一段只要在两个作用域的注册表原文里
/// 找不到（哪怕它**展开之后**与某条注册表条目指向同一个目录 —— 进程里是展开过的形式、
/// 注册表里不是，就是这么常见的形状），它就会被归到 `ProcessOnly` 名下。
/// 所以"`effective` 里第 `k` 个 `ProcessOnly`"与"`process_only` 里第 `k` 个"会错位。
///
/// 站得住的判据是**值**：拿 `process_only` 里那个值回 `effective` 里查第一条同值的
/// `ProcessOnly` 引用，它带着平台层自己记下的下标。兜底是把那个值拿去进程 `PATH` 里
/// 数一遍 —— 两级都查不到（真机上不该发生）时返回 `None`，调用方退回 `process_only`
/// 的序号：**宁可给一个可解释的位置，也不要 panic**，捕获是只读的，
/// 一个下标偏了不该让整份快照拿不到。
fn process_only_index(
    effective: &[EntryRef],
    ctx: &DetectContext<'_>,
    value: &str,
) -> Option<usize> {
    let wanted = path_key(value);
    if let Some(found) = effective.iter().find(|reference| {
        reference.scope == EnvScope::ProcessOnly && path_key(&reference.value) == wanted
    }) {
        return Some(found.index);
    }
    ctx.process_env
        .path_entries()
        .iter()
        .enumerate()
        .find(|(_, raw)| path_key(raw) == wanted)
        .map(|(index, _)| index)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tuoen_platform::RegHive;
    use tuoen_platform::fixture::{
        FixtureDir, FixtureKey, FixturePath, FixtureProcess, MachineFixture,
    };
    use tuoen_platform::{EnvScope, RegType, RegValue, ReparseKind};

    use super::super::super::files::{Existence, PathFile, PathRow};
    use super::super::super::test_support::CaptureFixture;

    /// 一份取证缩影的固定装置，一次覆盖本票的全部判据。
    ///
    /// 刻意做成"一张表多个人读"：各条用例只钉自己那一条判据，
    /// 而"采集器会不会漏掉某个作用域"这种整体形状只需要一份输入。
    ///
    /// # 这台假机器上的形状
    ///
    /// | 值 | 在哪里 | 说的是哪一件事 |
    /// |---|---|---|
    /// | `%SystemRoot%\System32` | 机器级 `expand-sz` | `expand-sz` 会被展开 |
    /// | `C:\missing-bin` | 机器级 | 失效条目（磁盘上不存在） |
    /// | `C:\shared`（两次，其中一次带 `\`） | 机器级 | `dup_index` + 原文一个字不动 |
    /// | `C:\Users\Muelsyse\AppData\Local\tuoen\shims` | 机器级 | 硬编码用户名 + `owner = tuoen` |
    /// | `C:\nvm4w\nodejs` | 机器级 | junction → `reparse = junction` |
    /// | `"C:\Program Files\Some Tool"` | 用户级（带引号） | 引号保住空格 |
    /// | `%SystemRoot%` | 用户级 `reg-sz` | **`REG_SZ` 不展开**（`setx` 事故的形状） |
    /// | `C:\Users\Muelsyse\bin` | 用户级 | 用户级的硬编码用户名 |
    /// | `%USERPROFILE%\bin` | 用户级 | 含 `%` **不算**硬编码用户名 |
    /// | `C:\shared` | 用户级 | 两套坐标系：同名值在两个作用域里各有下标 |
    /// | `%NOPE%\bin` | 用户级 | 解不开 → `Existence::Unknown`，且不问磁盘 |
    /// | `C:\Tools\dev-junction` | 用户级 | junction |
    /// | `C:\Tools\dev-link` | 用户级 | symlink |
    /// | `C:\Aliases` | 用户级 | App Execution Alias |
    /// | `C:\Launcher\bin` | **只在进程里** | 启动器注入项（不在任何注册表里） |
    ///
    /// 另外，进程 `PATH` 里的 `C:\Windows\System32` 是**展开过的**形式，而注册表里
    /// 那一条是 `%SystemRoot%\System32` —— 于是平台层会把它也算成一条 `ProcessOnly`
    /// 引用。这是真机上常见的形状，也是"注入项不能只按 `ProcessOnly` 的序号去数"
    /// 那条实测结论的来源。
    fn machine() -> MachineFixture {
        const MACHINE_ENV: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";

        MachineFixture {
            // 进程环境块 = 注册表那份的合并结果 + 启动器注入项。
            // 真机就是这个形状（本机 PowerShell 的 MSIX 别名排在最前面）。
            env: BTreeMap::from([
                ("SystemRoot".to_owned(), r"C:\Windows".to_owned()),
                (
                    "Path".to_owned(),
                    concat!(
                        r"C:\Windows\System32;C:\missing-bin;C:\shared;C:\shared\",
                        r";C:\Users\Muelsyse\AppData\Local\tuoen\shims;C:\nvm4w\nodejs",
                        r";C:\Program Files\Some Tool;%SystemRoot%;C:\Users\Muelsyse\bin",
                        r";%USERPROFILE%\bin;C:\shared;%NOPE%\bin;C:\Tools\dev-junction",
                        r";C:\Tools\dev-link;C:\Aliases;C:\Launcher\bin",
                    )
                    .to_owned(),
                ),
            ]),
            // `paths` 覆盖 `dirs`：这一批决定"节点本身是什么"（junction / symlink /
            // 0 字节别名 / 不存在），而下面的 `dirs` 提供目录里的内容。
            paths: vec![
                FixturePath::junction(
                    r"C:\nvm4w\nodejs",
                    r"C:\Users\x\AppData\Local\nvm\v24.19.0",
                ),
                FixturePath::junction(r"C:\Tools\dev-junction", r"C:\Tools\real-1"),
                FixturePath::symlink_dir(r"C:\Tools\dev-link", r"C:\Tools\real-2"),
                // App Execution Alias：0 字节、`exists == true`、`Test-Path` 会通过。
                //
                // 固定装置把**这一条条目本身**标成别名：真机上 `WindowsApps` 那根
                // `python.exe` 是文件，而我们要测的是采集器对 reparse 位的处理，
                // 不是 Windows 的别名机制。
                FixturePath::app_exec_alias(r"C:\Aliases"),
            ],
            dirs: vec![
                FixtureDir::new(r"C:\Windows\System32", Vec::new()),
                FixtureDir::new(r"C:\Users\Muelsyse\AppData\Local\tuoen\shims", Vec::new()),
                FixtureDir::new(r"C:\Users\Muelsyse\bin", Vec::new()),
                FixtureDir::new(r"C:\Tools\real-1", Vec::new()),
                FixtureDir::new(r"C:\Tools\real-2", Vec::new()),
                FixtureDir::new(r"C:\Launcher\bin", Vec::new()),
                FixtureDir::new(
                    r"C:\nvm4w\nodejs",
                    vec![FixturePath::file("node.exe", 80_000)],
                ),
                FixtureDir::new(
                    r"C:\Program Files\Some Tool",
                    vec![FixturePath::file("tool.exe", 4_096)],
                ),
            ],
            registry: vec![
                FixtureKey::new(
                    RegHive::Hklm,
                    MACHINE_ENV,
                    BTreeMap::from([(
                        "Path".to_owned(),
                        RegValue::ExpandSz(
                            concat!(
                                r"%SystemRoot%\System32;C:\missing-bin;C:\shared;C:\shared\",
                                r";C:\Users\Muelsyse\AppData\Local\tuoen\shims;C:\nvm4w\nodejs",
                            )
                            .to_owned(),
                        ),
                    )]),
                ),
                FixtureKey::new(
                    RegHive::Hkcu,
                    "Environment",
                    BTreeMap::from([(
                        "Path".to_owned(),
                        // **用户级是 `REG_SZ`**（本机实测就是这个形状）：
                        // 里面的 `%SystemRoot%` 与 `%NOPE%\bin` 都不会被展开。
                        RegValue::Sz(
                            concat!(
                                r#""C:\Program Files\Some Tool";%SystemRoot%;C:\Users\Muelsyse\bin"#,
                                r";%USERPROFILE%\bin;C:\shared;%NOPE%\bin",
                                r";C:\Tools\dev-junction;C:\Tools\dev-link;C:\Aliases",
                            )
                            .to_owned(),
                        ),
                    )]),
                ),
            ],
            // 采集器自己不启动任何进程；这里留着是为了说明它读的是注入的假机器。
            processes: vec![FixtureProcess {
                program: r"C:\nvm4w\nodejs\node.exe".to_owned(),
                // 空 = 通配（决策 185 的匹配规则）。
                args: Vec::new(),
                stdout: "v24.19.0\n".to_owned(),
                stderr: String::new(),
                exit_code: Some(0),
                timed_out: false,
            }],
            managed: Vec::new(),
        }
    }

    /// 自己就是 tuoen 的根，而且**刻意带一个结尾反斜杠**：
    /// 规范化那一步如果不做，`owner` 就会在这里答 `unknown`。
    const TUOEN_ROOT: &str = r"C:\Users\Muelsyse\AppData\Local\tuoen\";

    fn fixture_of(description: &MachineFixture) -> CaptureFixture {
        CaptureFixture::build(description).with_tuoen_root(TUOEN_ROOT)
    }

    fn fixture() -> CaptureFixture {
        fixture_of(&machine())
    }

    fn path_of(fixture: &CaptureFixture) -> PathFile {
        fixture
            .capture_all("2026-10-02T12:00:00Z")
            .path
            .expect("全部 section 的捕获必然包含 path")
    }

    /// 按作用域 + 值找一条。**值用规范化之后的形式比**：用例里写 `C:\shared`，
    /// 而文件里那一行可能是 `C:\shared\` 或者展开过的形式。
    fn row<'a>(file: &'a PathFile, scope: EnvScope, value: &str) -> &'a PathRow {
        let wanted = super::path_key(value);
        file.entry
            .iter()
            .find(|row| row.scope == scope && super::path_key(&row.expanded) == wanted)
            .unwrap_or_else(|| {
                panic!(
                    "没找到 {scope:?} 里的 `{value}`；这份文件里的条目是：{:#?}",
                    file.entry
                        .iter()
                        .map(|row| (row.scope, row.index, row.raw.as_str()))
                        .collect::<Vec<_>>()
                )
            })
    }

    fn count_rows(file: &PathFile, scope: EnvScope, value: &str) -> usize {
        let wanted = super::path_key(value);
        file.entry
            .iter()
            .filter(|row| row.scope == scope && super::path_key(&row.expanded) == wanted)
            .count()
    }

    // ── 三个作用域 ───────────────────────────────────────────────────

    /// **只在进程 `PATH` 里的那些目录必须被捕获。**
    ///
    /// 只读注册表的实现会整个漏掉它，而它真的在 `PATH` 上、真的占长度
    /// （本机实测：PowerShell 的 MSIX 别名目录，77 字符，不在任何注册表里）。
    #[test]
    fn a_process_only_directory_is_captured_with_no_registry_type() {
        let file = path_of(&fixture());
        let injected = row(&file, EnvScope::ProcessOnly, r"C:\Launcher\bin");
        assert_eq!(injected.reg_type, None, "进程注入项没有注册表类型");
        assert_eq!(injected.exists, Existence::Yes);
        assert_eq!(injected.raw, r"C:\Launcher\bin", "进程条目的原文就是那一段");
        assert!(!injected.quoted);
        assert_eq!(
            count_rows(&file, EnvScope::Machine, r"C:\Launcher\bin")
                + count_rows(&file, EnvScope::User, r"C:\Launcher\bin"),
            0,
            "它不该出现在任何注册表作用域里"
        );
    }

    /// 一个值同时出现在机器级与用户级 → **两条都在**，各自的 `index` 是**作用域内**的下标。
    #[test]
    fn both_scopes_keep_their_own_index_for_the_same_value() {
        let file = path_of(&fixture());

        let machine_shared = &file.entry[2];
        assert_eq!(machine_shared.scope, EnvScope::Machine);
        assert_eq!(machine_shared.index, 2, "机器级 `Path` 里的第 3 段");

        let user_shared = row(&file, EnvScope::User, r"C:\shared");
        assert_eq!(user_shared.index, 4, "用户级 `Path` 里的第 5 段");
        assert_eq!(count_rows(&file, EnvScope::Machine, r"C:\shared"), 2);
        assert_eq!(count_rows(&file, EnvScope::User, r"C:\shared"), 1);

        // 机器级里面那句 `C:\shared\` 带着结尾反斜杠，原文一个字都不许动。
        assert_eq!(file.entry[3].raw, r"C:\shared\");
        assert_eq!(file.entry[3].index, 3);
        assert_eq!(
            file.entry[3].expanded, r"C:\shared\",
            "`REG_EXPAND_SZ` 的原文里没有变量，展开之后就是它自己"
        );
    }

    // ── exists 三态 ──────────────────────────────────────────────────

    /// 三态各一条，而且第三条**证明我们没去问磁盘**：
    /// 假文件系统对未声明的路径一律答"不存在"，所以 `Unknown` 与 `No` 的区别
    /// 本身就是"有没有去看"的证据 —— 只看磁盘就会得到 `No`。
    #[test]
    fn existence_is_three_valued_and_unknown_never_touches_the_disk() {
        let file = path_of(&fixture());

        let present = row(&file, EnvScope::Machine, r"C:\Windows\System32");
        assert_eq!(present.exists, Existence::Yes, "{present:?}");
        assert_eq!(present.expanded, r"C:\Windows\System32");

        let missing = row(&file, EnvScope::Machine, r"C:\missing-bin");
        assert_eq!(missing.exists, Existence::No);

        let unresolved = row(&file, EnvScope::User, r"%NOPE%\bin");
        assert_eq!(unresolved.expanded, r"%NOPE%\bin", "解不开就原样留着");
        assert_eq!(
            unresolved.exists,
            Existence::Unknown,
            "解不开的值得不到 `No` —— 那是「我们看过了、它不在」，而我们没看"
        );
        assert_eq!(unresolved.reparse, ReparseKind::None);
        assert_eq!(unresolved.link_target, None);
    }

    /// **空条目不是一个目录。**
    ///
    /// 真机验收抓出来的（本机 HKLM 的 `Path` 第 21 段就是空的）：第一版让空串走
    /// 正常分支，磁盘对它答"不存在"，于是它变成一条 `exists = "no"` 的失效条目。
    /// 真机上 8 条 `no` 里有 1 条是它，而"这里缺一个目录"的判据必须是
    /// `!empty && exists == "no"`（真机上 7 条 —— 与独立用 PowerShell 数出来的
    /// 7 条失效条目对上）。
    #[test]
    fn an_empty_entry_is_not_a_missing_directory() {
        // 第一段用 `C:\Windows\System32`：它在固定装置里**真的存在**（`C:\Windows`
        // 自己没被声明成目录 —— 假文件系统只认声明过的东西）。
        const WITH_EMPTY: &str = r"C:\Windows\System32;;C:\missing-bin";
        let mut description = machine();
        description
            .env
            .insert("Path".to_owned(), WITH_EMPTY.to_owned());
        if let Some(key) = description
            .registry
            .iter_mut()
            .find(|key| key.hive == RegHive::Hklm)
        {
            key.values
                .insert("Path".to_owned(), RegValue::Sz(WITH_EMPTY.to_owned()));
        }
        let file = path_of(&fixture_of(&description));

        let empty = file
            .entry
            .iter()
            .find(|row| row.empty)
            .expect("空条目必须被记下来，位置也要占");
        assert_eq!(empty.raw, "", "原文就是空的");
        assert_eq!(empty.index, 1, "它是第 2 段");
        assert_eq!(
            empty.exists,
            Existence::Unknown,
            "空串不是一个目录 ——「在不在」对它不成立，更不该去问磁盘"
        );
        assert_eq!(empty.reparse, ReparseKind::None);
        assert_eq!(empty.link_target, None);

        let broken: Vec<&str> = file
            .entry
            .iter()
            .filter(|row| {
                row.scope == EnvScope::Machine && !row.empty && row.exists == Existence::No
            })
            .map(|row| row.raw.as_str())
            .collect();
        assert_eq!(
            broken,
            vec![r"C:\missing-bin"],
            "失效条目的判据是 `!empty && exists == no` —— 空条目不许被数进去"
        );
        assert!(
            !file
                .effective
                .iter()
                .any(|reference| reference.scope == EnvScope::Machine && reference.index == 1),
            "空条目不占 `[[effective]]`：它对解析没有任何贡献"
        );
    }

    /// **`expanded` 由类型说了算**：`REG_EXPAND_SZ` 展开，`REG_SZ` 字面保留。
    ///
    /// 这条钉的正是本机实测的形状：`Path` 标着 `REG_EXPAND_SZ`，而**同一个值**
    /// 换一个类型就完全不该被展开 —— 展开它会把一条在真机上必然失效的条目
    /// 显示成一个正常路径。
    #[test]
    fn expanded_follows_the_registry_type_not_the_value() {
        let mut description = machine();
        if let Some(key) = description
            .registry
            .iter_mut()
            .find(|key| key.hive == RegHive::Hkcu)
        {
            key.values.insert(
                "Path".to_owned(),
                RegValue::Sz(r"%SystemRoot%\System32;%SystemRoot%".to_owned()),
            );
        }
        let file = path_of(&fixture_of(&description));

        let expanded_row = row(&file, EnvScope::Machine, r"C:\Windows\System32");
        assert_eq!(expanded_row.reg_type, Some(RegType::ExpandSz));
        assert_eq!(expanded_row.raw, r"%SystemRoot%\System32");
        assert_eq!(
            expanded_row.expanded, r"C:\Windows\System32",
            "expand-sz 要被展开"
        );
        assert!(expanded_row.has_vars, "原文里有 `%`");
        assert_eq!(
            expanded_row.exists,
            Existence::Yes,
            "展开之后真的去看了磁盘"
        );

        // **同一个值，换一个类型。** 它是 `REG_SZ`，所以 Windows 就是不会展开它。
        let literal_row = row(&file, EnvScope::User, r"%SystemRoot%");
        assert_eq!(literal_row.reg_type, Some(RegType::Sz));
        assert_eq!(
            literal_row.expanded, r"%SystemRoot%",
            "REG_SZ 的值不展开 —— 展开它会凭空造出一个不存在的路径"
        );
        assert_eq!(
            literal_row.exists,
            Existence::Unknown,
            "不展开就解不开，因此是「说不准」而不是「没有」"
        );
        assert!(literal_row.has_vars);
    }

    /// 两条硬编码用户名，而那两条 `%` 写法**不算**。
    #[test]
    fn a_hardcoded_username_is_reported_in_both_scopes_and_variables_are_not() {
        let file = path_of(&fixture());

        let machine_user = row(
            &file,
            EnvScope::Machine,
            r"C:\Users\Muelsyse\AppData\Local\tuoen\shims",
        );
        assert!(machine_user.has_username, "机器级的硬编码用户名最危险");
        assert_eq!(machine_user.owner, "tuoen", "它同时还落在我们自己的根里");

        let user_user = row(&file, EnvScope::User, r"C:\Users\Muelsyse\bin");
        assert!(user_user.has_username);

        let profile_var = row(&file, EnvScope::User, r"%USERPROFILE%\bin");
        assert!(
            !profile_var.has_username,
            "`%USERPROFILE%` 是可搬运的写法，不是硬编码"
        );
        assert!(profile_var.has_vars);

        for entry in &file.entry {
            assert_eq!(
                entry.has_username,
                tuoen_platform::path::hardcoded_username(&entry.expanded).is_some(),
                "`has_username` 的判据只有一处定义：{entry:?}"
            );
        }
    }

    // ── reparse ──────────────────────────────────────────────────────

    /// junction / symlink / App Execution Alias 三种形状都要能出现，且**只记录不跟随**。
    #[test]
    fn reparse_shapes_are_recorded_with_their_targets() {
        let file = path_of(&fixture());

        let junction = row(&file, EnvScope::Machine, r"C:\nvm4w\nodejs");
        assert_eq!(junction.reparse, ReparseKind::Junction);
        assert_eq!(
            junction.link_target.as_deref(),
            Some(r"C:\Users\x\AppData\Local\nvm\v24.19.0"),
            "链接目标只记录，不跟着走"
        );

        let symlink = row(&file, EnvScope::User, r"C:\Tools\dev-link");
        assert_eq!(symlink.reparse, ReparseKind::SymlinkDir);
        assert_eq!(symlink.link_target.as_deref(), Some(r"C:\Tools\real-2"));

        // App Execution Alias：0 字节、`exists == true`，而**它不是链接**。
        let alias = row(&file, EnvScope::User, r"C:\Aliases");
        assert_eq!(alias.reparse, ReparseKind::AppExecAlias);
        assert_eq!(alias.exists, Existence::Yes);
        assert_eq!(alias.link_target, None, "别名没有目标路径可读");
    }

    /// 不存在的条目**不是链接**：磁盘上什么都没有，就没有 reparse 可言。
    #[test]
    fn a_missing_entry_carries_no_reparse_and_no_target() {
        let file = path_of(&fixture());
        let missing = row(&file, EnvScope::Machine, r"C:\missing-bin");
        assert_eq!(missing.exists, Existence::No);
        assert_eq!(missing.reparse, ReparseKind::None);
        assert_eq!(missing.link_target, None);
    }

    // ── owner ────────────────────────────────────────────────────────

    /// 四个取值各一条，而且不许有第五个。
    #[test]
    fn the_owner_ladder_has_four_values_and_no_fifth() {
        let file = path_of(&fixture());

        let system = row(&file, EnvScope::Machine, r"C:\Windows\System32");
        assert_eq!(system.owner, "system");
        assert_eq!(
            system.raw, r"%SystemRoot%\System32",
            "判据用的是**展开后**的值 —— 拿原文比会得到 unknown"
        );
        assert_eq!(
            row(
                &file,
                EnvScope::Machine,
                r"C:\Users\Muelsyse\AppData\Local\tuoen\shims"
            )
            .owner,
            "tuoen"
        );
        assert_eq!(
            row(&file, EnvScope::Machine, r"C:\nvm4w\nodejs").owner,
            "third-party",
            "别人的版本切换器（junction / symlink / 别名）都算被第三方接管"
        );
        assert_eq!(
            row(&file, EnvScope::User, r"C:\Tools\dev-link").owner,
            "third-party"
        );
        assert_eq!(
            row(&file, EnvScope::User, r"C:\Aliases").owner,
            "third-party"
        );
        assert_eq!(
            row(&file, EnvScope::User, r"C:\shared").owner,
            "unknown",
            "`unknown` 是默认值，不是失败"
        );

        for entry in &file.entry {
            assert!(
                ["tuoen", "system", "third-party", "unknown"].contains(&entry.owner.as_str()),
                "不许发明第五个取值：{entry:?}"
            );
        }
    }

    /// `C:\WindowsApps` **不在** `C:\Windows` 里 —— 归属判定是按路径段比的，
    /// 不是按字符串前缀。
    #[test]
    fn a_sibling_directory_is_not_inside_the_system_root() {
        let mut description = machine();
        description
            .dirs
            .push(FixtureDir::new(r"C:\WindowsApps", Vec::new()));
        if let Some(key) = description
            .registry
            .iter_mut()
            .find(|key| key.hive == RegHive::Hklm)
        {
            key.values.insert(
                "Path".to_owned(),
                RegValue::ExpandSz(r"%SystemRoot%\System32;C:\WindowsApps".to_owned()),
            );
        }
        let file = path_of(&fixture_of(&description));
        assert_eq!(
            row(&file, EnvScope::Machine, r"C:\WindowsApps").owner,
            "unknown",
            "字符串前缀会把 `C:\\WindowsApps` 错误地算进 `C:\\Windows`"
        );
        assert_eq!(
            row(&file, EnvScope::Machine, r"C:\Windows\System32").owner,
            "system"
        );
    }

    // ── dup_index 与 [[effective]] ───────────────────────────────────

    /// **`dup_index` 是"同值条目里的序号"，所以从没重复过的值恒为 `0`。**
    ///
    /// 这条用例钉住两件事：
    ///
    /// 1. 中间那一版把它做成"不同的值依次编号"（`0 1 2 2 3`）时，`D` 会拿到 `3` ——
    ///    而票据要的是 `dup_index > 0` 的行数**等于**重复条目的条数，那一版会把它变成
    ///    "不同值的个数 − 1"（真机上 32 而不是 18）；
    /// 2. 第一版更早的 bug 是 `seen.len()` 当"下一个号"，撞上重复值表不长，于是
    ///    两条**不同的**值会共用一个号。
    #[test]
    fn the_dup_counter_is_the_occurrence_ordinal() {
        let mut seen = std::collections::BTreeMap::new();
        let got: Vec<usize> = [r"C:\a", r"C:\b", r"C:\c", r"C:\c\", r"C:\d"]
            .iter()
            .map(|value| super::next_dup_index(&mut seen, value))
            .collect();
        assert_eq!(
            got,
            vec![0, 0, 0, 1, 0],
            "每个从没出现过的值都是 0，第二次出现才是 1 —— 大小写与尾部反斜杠不改变「同一个值」"
        );
        // 重复条目的条数 = `dup_index > 0` 的行数 —— 上面这五个值里恰好一条。
        assert_eq!(got.iter().filter(|n| **n > 0).count(), 1);
    }

    /// 同一个值在机器级出现两次 → `0` 与 `1`；第三次出现在用户级 → `2`。
    /// 计数**跨作用域**（进程注入项也参与）。
    ///
    /// 而 `[[effective]]` 里只有一条：它记的是**解析顺序**，
    /// 重复的第二条对解析没有任何意义。
    #[test]
    fn the_same_value_twice_gets_zero_then_one() {
        let file = path_of(&fixture());
        assert_eq!(file.entry[2].raw, r"C:\shared");
        assert_eq!(file.entry[2].dup_index, 0, "这个值的第一次出现");
        assert_eq!(file.entry[3].raw, r"C:\shared\");
        assert_eq!(
            file.entry[3].dup_index, 1,
            "去尾部反斜杠 + 小写之后是同一个值，所以它是第二次"
        );
        assert_eq!(
            row(&file, EnvScope::User, r"C:\shared").dup_index,
            2,
            "第三次出现，跨作用域计数"
        );
        assert_eq!(
            file.entry[4].dup_index, 0,
            "`C:\\nvm4w\\nodejs` 从没出现过 —— 它又是第一次，不许沿用别人的号"
        );

        let effective_shared = file
            .effective
            .iter()
            .filter(|row| row.scope == EnvScope::Machine && row.index == 2)
            .count();
        assert_eq!(effective_shared, 1, "生效顺序里没有第二条");
    }

    /// **不变量：`[[effective]]` 里每一条都必须能在 `[[entry]]` 里查到。**
    ///
    /// 两套坐标系最容易在这里出错 —— 一个进程注入项的下标要是当成
    /// `process_only` 表里的序号写进去，这份快照就出现了悬空引用。
    #[test]
    fn every_effective_reference_resolves_to_an_entry_row() {
        let file = path_of(&fixture());
        assert!(!file.effective.is_empty());
        assert!(
            file.effective
                .iter()
                .any(|reference| reference.scope == EnvScope::ProcessOnly),
            "这份固定装置里有注入项，生效顺序里必须看得见它"
        );
        for reference in &file.effective {
            let found = file
                .entry
                .iter()
                .find(|row| row.scope == reference.scope && row.index == reference.index)
                .unwrap_or_else(|| {
                    panic!(
                        "悬空引用 {reference:?}；这份文件里的坐标是：{:#?}",
                        file.entry
                            .iter()
                            .map(|row| (row.scope, row.index))
                            .collect::<Vec<_>>()
                    )
                });
            assert!(!found.raw.is_empty(), "被引用的行不该是空条目：{found:?}");
        }
    }

    /// 进程注入项的坐标是**进程 `PATH` 内的下标**。
    ///
    /// 这台假机器里 `C:\Windows\System32`（进程里展开过）与 `%SystemRoot%\System32`
    /// （注册表里没有）是两个不同的键，于是平台层把前者也算成了一条 `ProcessOnly`
    /// 引用 —— 那条引用的下标是 `0`。所以"`effective` 里第一个 `ProcessOnly`"
    /// 并不是我们要找的注入项，**必须按值去找**。
    #[test]
    fn a_process_only_index_is_its_position_in_the_process_path() {
        let file = path_of(&fixture());
        let injected = row(&file, EnvScope::ProcessOnly, r"C:\Launcher\bin");
        assert_eq!(
            injected.index, 15,
            "`C:\\Launcher\\bin` 是进程 PATH 的第 16 段"
        );
        assert!(
            file.effective.iter().any(|reference| {
                reference.scope == EnvScope::ProcessOnly && reference.index == 15
            }),
            "生效顺序里必须有它，而且坐标要对得上：{:#?}",
            file.effective
        );

        // 两套坐标系在同一个作用域里就已经打架了：进程 `PATH` 的第 0 段是
        // `C:\Windows\System32`，它展开之后与机器级第 0 段指向同一个目录。
        let system32 = row(&file, EnvScope::ProcessOnly, r"C:\Windows\System32");
        assert_eq!(system32.index, 0);
        assert_eq!(
            row(&file, EnvScope::Machine, r"C:\Windows\System32").index,
            0
        );
    }

    /// 注入项**前面有空条目**时下标也不能偏：空条目占一个位置，
    /// 所以"进程 `PATH` 里的第几段"跟"第几个非空条目"是两个数。
    #[test]
    fn empty_process_entries_do_not_shift_a_process_only_index() {
        let mut description = machine();
        description.env.insert(
            "Path".to_owned(),
            r";;C:\Launcher\bin;;C:\Windows\System32".to_owned(),
        );
        let file = path_of(&fixture_of(&description));
        let injected = row(&file, EnvScope::ProcessOnly, r"C:\Launcher\bin");
        assert_eq!(injected.index, 2, "两段空条目占掉 0 与 1，注入项是第 3 段");
        for reference in &file.effective {
            assert!(
                file.entry
                    .iter()
                    .any(|row| row.scope == reference.scope && row.index == reference.index),
                "悬空引用 {reference:?}"
            );
        }
    }

    /// 三个作用域**都要有**自己的条目段，且顺序是机器级 → 用户级 → 进程注入项。
    #[test]
    fn the_rows_cover_all_three_scopes_in_that_order() {
        let file = path_of(&fixture());
        let scopes: Vec<EnvScope> = file.entry.iter().map(|row| row.scope).collect();
        let first_user = scopes
            .iter()
            .position(|scope| *scope == EnvScope::User)
            .expect("要有用户级");
        let first_process = scopes
            .iter()
            .position(|scope| *scope == EnvScope::ProcessOnly)
            .expect("要有进程注入项");
        assert!(
            scopes[..first_user].iter().all(|s| *s == EnvScope::Machine),
            "机器级在最前：{scopes:?}"
        );
        assert!(
            scopes[first_user..first_process]
                .iter()
                .all(|s| *s == EnvScope::User),
            "用户级在中间：{scopes:?}"
        );
        assert!(
            scopes[first_process..]
                .iter()
                .all(|s| *s == EnvScope::ProcessOnly),
            "进程注入项在最后：{scopes:?}"
        );

        // 作用域内的下标必须升序 —— 顺序本身是数据。
        for pairs in file.entry.windows(2) {
            if pairs[0].scope == pairs[1].scope {
                assert!(
                    pairs[0].index < pairs[1].index,
                    "同一个作用域里的下标必须升序：{:?} vs {:?}",
                    (pairs[0].scope, pairs[0].index),
                    (pairs[1].scope, pairs[1].index)
                );
            }
        }
    }

    /// 空条目（`;;`、结尾的 `;`）**占一个位置**，与平台的 `ScopedPath::entries` 对齐。
    ///
    /// 吃掉它会让写回时静默改写用户的 `PATH`，所以它必须有自己的行。
    #[test]
    fn an_empty_registry_entry_still_gets_a_row() {
        let mut description = machine();
        description.env.insert("Path".to_owned(), String::new());
        if let Some(key) = description
            .registry
            .iter_mut()
            .find(|key| key.hive == RegHive::Hklm)
        {
            key.values.insert(
                "Path".to_owned(),
                RegValue::ExpandSz(r"C:\Windows\System32;;".to_owned()),
            );
        }
        let file = path_of(&fixture_of(&description));
        let machine_rows: Vec<&PathRow> = file
            .entry
            .iter()
            .filter(|row| row.scope == EnvScope::Machine)
            .collect();
        assert_eq!(
            machine_rows.len(),
            3,
            "`;;` 是两段空条目：{machine_rows:#?}"
        );
        assert_eq!(machine_rows[1].index, 1);
        assert_eq!(machine_rows[1].raw, "");
        assert_eq!(machine_rows[2].index, 2);
        assert!(!machine_rows[1].has_vars);
        assert!(!machine_rows[1].has_username);
    }

    // ── 确定性 ───────────────────────────────────────────────────────

    /// 同一台假机器跑两次，`toml::to_string_pretty` 的输出**逐字节相同**。
    ///
    /// 采集器的输出顺序必须完全由数据决定：任何一处 `HashMap` 迭代或者
    /// 未排序的集合都会在这里露出来。
    #[test]
    fn the_same_machine_captures_byte_identically_twice() {
        let fixture = fixture();
        let first = path_of(&fixture);
        let second = path_of(&fixture);
        assert_eq!(first, second);
        let first_text = toml::to_string_pretty(&first).expect("序列化");
        let second_text = toml::to_string_pretty(&second).expect("序列化");
        assert_eq!(first_text, second_text, "同一台机器两次捕获必须逐字节相同");
        assert!(first_text.contains("[[entry]]"), "{first_text}");
        assert!(first_text.contains("[[effective]]"), "{first_text}");
        assert!(first_text.contains("level = \"ok\""), "{first_text}");
    }
}
