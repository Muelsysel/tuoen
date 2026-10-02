//! `tuoen path diff` / `tuoen path apply` 的**纯函数内核**：`PATH` 的逐条 diff、
//! 重建（产出完整的新有序列表）与选择性应用。
//!
//! 决策原文是 `docs/DESIGN.md` §1.17（决策 126–140），票据是 `_t15.md`（issue #15）。
//!
//! # 为什么 JSON 合同由 core 拥有
//!
//! `path diff`（只读）、`path apply --dry-run`（只读）与 `path apply`（会写注册表）
//! **三条路径共用这一份类型**：dry-run 不是"另写一条只读分支"，它就是同一份
//! `PathDiff` + `Rebuild` 少调一次 `apply`（决策 20 / 139）。既然三条路径产出的
//! 是同一个对象，那么 `--json` 的键就只能有一处定义 —— 就是这里的字段名。
//! CLI 侧**不要再抄一遍** `beforeEntries` / `duplicateOf` 这种字面量：抄一份的那天起，
//! "预览里的键"与"执行里的键"就有两个来源，而它们漂移的表现是脚本读到 `null`
//! 而不是报错。序列化的规则是：
//!
//! * 结构体一律 `#[serde(rename_all = "camelCase")]`；
//! * 枚举一律 `#[serde(rename_all = "kebab-case")]`，且与 [`DiffClass::as_str`] /
//!   [`DiffReason::as_str`] 逐字相同（有一条用例钉住这件事）；
//! * `DiffCounts::move_` 显式 `rename = "move"`（`move` 是 Rust 关键字，字段名只能带下划线）；
//! * 所有 `Option` 字段 `skip_serializing_if` —— `None` 不进 JSON，脚本读不到键就是 `null`。
//!
//! # 这一层的两条硬性质
//!
//! 1. **纯函数。** [`diff`] 与 [`rebuild`] 不读时钟、不读环境变量、不碰注册表磁盘网络；
//!    唯一的机器访问是**注入进来的** [`FileSystem`]（只读、只用于"拼写疑似"，决策 133）。
//!    真正会做 I/O 的只有三个入口，而且它们是显式的：[`load_snapshot`]（读一份
//!    `path.toml`）、[`current_username_from_env`]（问调用方给的 [`EnvBlock`]）、
//!    [`current_username_from_process`]（问调用方给的 [`ProcessEnv`]）。
//!    这条性质是 `--dry-run` 与真写"逐条相同"能被断言的前提（决策 139）。
//! 2. **两边同一形状。** 本机侧是**现场 `capture` 出来的 [`PathFile`]**（在内存里，
//!    不落盘），目标侧是读进来的 `tuoen.d/path.toml`。两侧不同形状的判据必然漂移。
//!
//! # 只看 `entry`，绝不看 `[[effective]]`（决策 126）
//!
//! 比较的是逐作用域的 `entry` 行，且只取 `scope ∈ {Machine, User}`。`process-only`
//! 的行**不参与比较**：它们不在任何注册表里，重建也写不回任何东西；
//! `[[effective]]` 更是**整个不读** —— 它是"机器级 + 用户级 + 进程注入"的合并视图，
//! 拿它做 diff 会把启动器注入的条目（本机实测有 PowerShell 的 MSIX 别名）当成
//! "本机独有"、把同一个目录数两次，而重建只能作用于**各自 scope 内部**的顺序。
//!
//! # 一条一行：类由理由决定（决策 127）
//!
//! `class == reason.class()` 是**硬不变量**（有一条覆盖全量行的用例）。理由的优先级是
//! 「存在性 → 健康 → 位置 → 大小写 → 相同」，一条条目同时坏又挪了位置 → `fix`，
//! 另用 `also` 记下它同时还挪了（`also` 去重、按 slug 字典序）。本模块的具体名次：
//!
//! | 名次 | 理由 | 类 | 判据（判在哪一侧） |
//! |---|---|---|---|
//! | 0 | `empty-segment` | `fix` | 任一侧是空条目（两侧都判，**唯一**的例外） |
//! | 1 | `only-in-target` | `add` | 本机没有这一条（按"值 + 出现序号"配对） |
//! | 2 | `only-in-local` | `remove` | 目标没有这一条 |
//! | 3 | `duplicate` | `fix` | 本机这条是第 2 次及以后出现（`dup_index > 0`） |
//! | 4 | `username-hardcoded` | `fix` | **真的有旧名可换**（`rewrite_to` 有值，判在目标侧） |
//! | 5 | `dangling` | `fix` | 本机这条指向的目录不在（`!empty && exists == no`） |
//! | 6 | `position-differs` | `move` | 两侧都有，但"共同条目序列"里的序号不同 |
//! | 7 | `case-differs` | `case-only` | 两侧都有、位置相同，`raw`（去首尾空白后）不同但折叠后相同 |
//! | 8 | `identical` | `keep` | 以上都不成立 |
//!
//! 健康（`duplicate` / `dangling` / `username-hardcoded`）**只判本机侧** —— 票据原话是
//! "本机这条是坏的"。目标侧的 `dup_index > 0` / `exists = no` 只作为**行事实**照抄进
//! [`SideRow`] 让用户看得见，**不**产生任何理由：那是**源机器**的事实，而这一票要修的
//! 是本机的 `PATH`。
//!
//! ## `username-hardcoded` 是**动作**，不是事实（Lead 的裁决）
//!
//! 判据是"**这一类的 `fix` 会做什么**"，一个类一个动作、不许复合动作：
//! 名次 4 只在 `rewrite_to` 有值时成立（有旧名可换，选中 `fix` 会**重写**它）。
//! 一条 `has_username == true` 但**没有旧名可换**的行（名字就是当前用户名，或者调用方
//! 没给当前用户名）**不归这一类** —— 否则选中 `fix` 之后它既不会被重写、也不会被丢掉，
//! 用户会以为修好了，那是"看起来在工作"。它按其余性质归入 `dangling` / `move` /
//! `case-only` / `keep`，而 `has_username` 这个**事实**留在 `also` 里（可移植性是证据）。
//! 本机那 12 条硬编码用户名全部是当前用户 `Muelsyse`，所以它们在本机的输出里是
//! `2 条 duplicate + 3 条 dangling + 7 条 keep`，每条的 `also` 里都看得见
//! `username-hardcoded`；验收脚本按 `hasUsername` **单独统计 12 条**做取证对照。
//!
//! 名次 4 在名次 5 之前是刻意的：一条**既失效、又硬编码了旧用户名**的条目
//! （`C:\Users\旧名\…`，换机之后必然同时是这两件事）必须能被**重写**救回来 ——
//! 让 `dangling` 抢先会让重写恰好在其唯一有用的场景里不生效。名次 3 在名次 4 之前：
//! 一条重复条目该被**丢掉**，而不是被重写成一个仍然重复的条目。
//! 名次 2 在名次 3 之前是同一张优先级表的另一面：一条**既重复、目标里又没有**的条目，
//! 主理由归 `only-in-local`（存在性先于健康），但 `duplicate` 会留在 `also` 里、
//! `duplicate_of` 照样指出来 —— 两个事实都不许丢（有用例钉住这一条）。
//!
//! ## 票据 line 29 与决策 129 的冲突：决策 129 赢
//!
//! `_t15.md` 有一句"后续重复标记为 `remove`"。**不采纳**：票据自己把 `remove` 定义成
//! "本机有、目标没有"，而重复条目**在目标里也有** —— 报成 `remove` 会让用户以为
//! "目标里没有它"，于是在只选 `remove` 时把一条目标本来要保留的目录删掉。
//! 所以重复条目归 `fix` + `duplicate`（决策 129），这也是验收脚本的独立重算口径。
//!
//! ## `position-differs` 的"位置"到底指什么（与验收脚本对齐的口径）
//!
//! 指**共同条目序列里的序号**，不是 `local.index != target.index`。两侧都有的条目
//! （`directory_pair`）各按本侧顺序排一次，同一条在两边的名次不同才算挪了。
//! 用下标相等来判断会把"目标在前面插了一条"误报成"后面每一条都挪了"（真机实测
//! 那种假 `move` 多出 11 条）。`add` / `remove` 本来就不在这条序列里，所以它们不影响
//! 别的行的位置判定 —— 这也正是决策 131"没选中的东西连位置都不动"要的效果。
//!
//! # 配对必须按"值 + 出现序号"（真机实测的教训）
//!
//! 本机 `PATH` 有十几组重复。如果按"值 → 第一条本机行"配对，同一个值的第 2 次目标行
//! 会配到第 1 次的本机行，两侧下标必然不同 —— 于是**每一条重复都会被误报成 `move`**
//! （实测多出 11 条假 `move`）。所以配对键是 `(值折叠后, 第几次出现)`：第 i 次配第 i 次。
//! 空条目**不与任何目录配对**（决策 130），只在空条目之间按出现序号两两对齐。
//!
//! 参与比较的"值"是 [`SideRow::value`] = `normalize_entry(row.expanded)`
//! （去首尾空白、去多余的结尾反斜杠、保留 `X:\`），配对时再折叠大小写。
//! **不用 `raw` 配对**：`raw` 含引号，而 `"C:\a"` 与 `C:\a` 是同一个目录。
//!
//! # `id` 与 `--pick`
//!
//! `id = "{scope}:{index}"`（如 `user:12`）：**行在本机存在时用本机下标**。
//! **只有目标侧才有**的行带一个 `+`（`user:+16`）—— 本机侧的行永远是
//! `{scope}:{index}`，两种形状不会撞上。
//!
//! 为什么必须有这个 `+`：没有它，本机 `[C:\a]` 对目标 `[C:\b]` 时，`remove` 行与
//! `add` 行会拿到**同一个 id**（两条都是 `user:0`），于是 `--pick user:0` 一次选中两条 ——
//! 用户想删一条，结果还顺手加了一条。这个不对称是刻意的：加一条新路径是"多做了"，
//! 删掉一条是"少做了"，两种误伤的代价不一样。`--pick` 两种形态都收。
//! `duplicate_of` 指向首次出现那一条的 `id`（本机侧的 id 天然唯一）。
//! `add` 行在 `applied` 里的 `id` 用同一个形状。
//!
//! # 重建：基底是本机列表（决策 131 / 132）
//!
//! `rebuild` 产出的是**一份完整的新列表**（写回时整值替换，不是对原字符串打补丁），
//! 基底是**本机**该 scope 的有序条目：
//!
//! 1. 先应用选中的 `fix` / `remove`：`fix` 里 `empty-segment` / `duplicate` / `dangling`
//!    丢掉，`username-hardcoded` **改写**成 `rewrite_to`；`remove` 丢掉。
//! 2. 再把选中的 `add` 插进来、把选中的 `move` 移到目标相对位置 —— 两者用**同一条**
//!    放置规则：把行 `X` 插到"在目标列表里排在 `X` 之后"的**第一条**行之前；没有这样的行
//!    就追加到末尾。锚点可以是本次已经插进来的行（`add`）—— 少了这一条，连着插两条
//!    会在逆序处理时把后一条排到前一条前面。
//!
//! **没选中的东西连位置都不动**：这是这一票最容易做错的地方。把"重建"实现成"按目标顺序
//! 整表替换"，`--only add` 就会顺手重排整个 `PATH` —— 而重排就是改优先级。
//! （诚实的一句："位置不动"指的是**不丢、不挪**；在它前面插一条，它的**下标**当然会变 ——
//! 那是"在一个有序列表里插入"的必然结果。）
//!
//! 第 2 步按**目标顺序的逆序**处理（不是正序）。只有逆序能保证"全都选中时输出恰等于目标
//! 顺序"：拿 `[A,B,C,D] → [D,A,B,C]` 试，正序会得到错位的结果，逆序得到 `[D,A,B,C]`。
//! 有两条用例把"全选 == 目标顺序"钉住。
//!
//! **重建的输出不变量**：输出里不存在折叠后重复的条目。基底里的重复由选中的
//! `fix/duplicate` 负责；而**插入**的候选行若与输出里已有的行折叠后相同，就跳过它，
//! 并在 `applied` 里记一条 `class = fix`（`value` = 被跳过的值，`to_index` = 已存在那条
//! 的下标）。这只会在"目标快照自己带重复、而本机没有"时发生 —— 那正是票据说的
//! "原样搬运会把旧病一起移植"。`move` 的候选行不会被这条跳过：它是本机已有的一行
//! （基底里的重复是基底自己的事），而且它被移走之后输出里仍然只有它一份。
//! 这条不变量比的是**注册表原文**（去引号、去尾部分隔符、折叠大小写），不是展开后的
//! 路径 —— 展开需要一张变量表，而重建是纯函数。
//!
//! ## 选了但什么都没做：`no_op_classes`
//!
//! [`Rebuild::no_op_classes`] 列出**被选中、但没有任何变换可做**的类（去重、按 slug 稳定
//! 排序），给 CLI 打印"你选了 X，但这一类不做任何改写"用。判据是"这个类有没有产出过任何
//! `applied` 条目"—— 重建里每一次真的改动都会记一条 `applied`（连"插入被输出不变量跳过"
//! 那条也记），所以它与"这一类的 `fix` 到底做了什么"是同一件事。于是：
//!
//! * `case-only` 永远在这里（决策 128：改大小写零收益、纯风险 —— 选中它**什么都不做**，
//!   但用户必须被告知，而不是拿到一份与输入逐字相同的 `PATH` 却没有任何解释）；
//! * 选了 `remove` 而本机一条 `remove` 都没有 → 在这里；
//! * 用 `--pick` 点中一条 `keep` → 它的类也在这里（被选中的行是哪一类，就在场）；
//! * 真的插进去/丢掉/重写了的类 → **不**在这里。
//!
//! # 用户名重写（决策 134）
//!
//! 只换 `\Users\` 后紧跟的**那一段**（其余部分逐字不动），旧名取
//! `tuoen_platform::hardcoded_username`（判据只有一处定义），当前用户名由调用方给
//! （[`PathDiffOptions::current_username`]，**不 spawn `whoami`**）。
//! 两个来源各有一个函数：[`current_username_from_process`]（进程环境，真机上的主来源）
//! 与 [`current_username_from_env`]（用户级注册表块，有值时更权威），调用方的顺序是
//! 前者优先、`or_else` 后者。
//!
//! **类看"有没有旧名可换"，重写看"要写进去的值能不能用"**：
//!
//! * 一条**本机存在、且真的有旧名可换**的行归 `fix`/`username-hardcoded`，重写只在
//!   选中 `fix` 时生效（没有旧名可换的行不归这一类，见上面"动作不是事实"）；
//! * 一条目标侧带旧用户名的 `add` 行会带着 `rewrite_to` 被插进来（跟着 `add` 生效，
//!   不额外要求选中 `fix`）：**不重写就等于插进去一条在本机永远不生效的路径**。
//!
//! 每一条算出来的重写都进 [`PathDiff::rewrites`]，逐条报告 —— 报告与类无关。
//!
//! # 长度预算（决策 140）
//!
//! [`Rebuild::budget`] 按**注册表原文**算：机器级原文 + **重建后**的用户级原文
//! （机器级的选中改动照常算进 `after_raw`，但不落盘 —— 那是 `requires_elevation`）。
//! 它是**下界**：进程注入项看不见（重建是纯函数），真机上的进程 `PATH` 只会更长。
//! 所以 `level` 报 `ok` 不代表真机 `ok`，而 `level` 报 `exceeded` 一定是真的超了。
//! 档位由 `tuoen_platform::PathBudget::of` 决定（`>8191` exceeded / `≥90%` critical /
//! `≥75%` warning），本模块不复制那三个阈值。
//!
//! # 测试夹具
//!
//! [`test_support`] 是这个模块的**测试夹具**（造 `PathFile` / 假文件系统 / 假环境块），
//! 不是产品代码：它只被 `#[cfg(test)]` 与别的 crate 的测试引用。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use tuoen_platform::{
    EnvBlock, EnvScope, FileSystem, PathBudget, ProcessEnv, RegType, ReparseKind,
    hardcoded_username, normalize_entry,
};

use crate::capture::{Existence, PathBudgetRow, PathFile, PathRow};

pub mod test_support;
pub mod typo;

pub use typo::suspected_typo;

// ─────────────────────────────────────────────────────────────────────────────
// 类与理由
// ─────────────────────────────────────────────────────────────────────────────

/// 一条条目归入的**类**。六类，一条只归一类。
///
/// 序列化是 `kebab-case`，与本模块的 [`DiffClass::as_str`] 逐字相同 ——
/// `--json` 里的 `"case-only"` 就是 CLI 的 `--only case-only`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiffClass {
    /// 两边都有，位置相同，本机这条健康。
    Keep,
    /// 目标有、本机没有。
    Add,
    /// 本机有、目标没有。
    Remove,
    /// 两边都有，但位置不同。
    Move,
    /// 两边都有，但本机这条是坏的（空 / 重复 / 失效 / 用户名依赖）。
    Fix,
    /// 仅大小写不同 —— Windows 上它们是同一个目录。
    CaseOnly,
}

impl DiffClass {
    /// 全部六类，**顺序就是本模块文档里表格的顺序**（`--only` 的稳定枚举顺序）。
    pub const ALL: [Self; 6] = [
        Self::Keep,
        Self::Add,
        Self::Remove,
        Self::Move,
        Self::Fix,
        Self::CaseOnly,
    ];

    /// 稳定小写 slug（进 `--json`，不本地化）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::Add => "add",
            Self::Remove => "remove",
            Self::Move => "move",
            Self::Fix => "fix",
            Self::CaseOnly => "case-only",
        }
    }

    /// 从 CLI 的 `--only <class>` 解析。**精确匹配**（大小写敏感）—— 大小写在这里
    /// 是数据，不是风格：`case-only` 与 `caseonly` 不该都能过。
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|class| class.as_str() == s)
    }
}

/// 为什么归入这一类。**类由理由决定**（`class == reason.class()` 是硬不变量）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiffReason {
    /// 一样。
    Identical,
    /// 只有目标侧有。
    OnlyInTarget,
    /// 只有本机侧有。
    OnlyInLocal,
    /// 两侧都有，但"共同条目序列"里的序号不同。
    PositionDiffers,
    /// 两侧都有、位置相同，`raw`（去首尾空白后）不同但折叠后相同。
    CaseDiffers,
    /// 这是一段空条目（`;;`、开头、结尾）。
    EmptySegment,
    /// 本机这条是同一个值的第 2 次及以后出现。
    Duplicate,
    /// 本机这条指向的目录不在。
    Dangling,
    /// 这条**真的有旧用户名可换**（`rewrite_to` 有值）—— 选中 `fix` 会重写它。
    ///
    /// 注意它说的是**动作**而不是事实：一条硬编码了用户名、但名字就是当前用户名的行
    /// 不归这里（它按其余性质归类，`has_username` 这个事实留在 `also` 里）。
    UsernameHardcoded,
}

impl DiffReason {
    /// 稳定小写 slug（进 `--json`，不本地化）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Identical => "identical",
            Self::OnlyInTarget => "only-in-target",
            Self::OnlyInLocal => "only-in-local",
            Self::PositionDiffers => "position-differs",
            Self::CaseDiffers => "case-differs",
            Self::EmptySegment => "empty-segment",
            Self::Duplicate => "duplicate",
            Self::Dangling => "dangling",
            Self::UsernameHardcoded => "username-hardcoded",
        }
    }

    /// 这个理由决定哪一类。**全函数**：每个理由都对应一类。
    #[must_use]
    pub const fn class(self) -> DiffClass {
        match self {
            Self::Identical => DiffClass::Keep,
            Self::OnlyInTarget => DiffClass::Add,
            Self::OnlyInLocal => DiffClass::Remove,
            Self::PositionDiffers => DiffClass::Move,
            Self::CaseDiffers => DiffClass::CaseOnly,
            Self::EmptySegment | Self::Duplicate | Self::Dangling | Self::UsernameHardcoded => {
                DiffClass::Fix
            }
        }
    }
}

/// 主理由的名次（**只有这一处定义**，文档里的表格就是它）。
///
/// 越小越优先。`also` 用不到它 —— `also` 是"除主理由之外还成立的理由"，
/// 按 slug 字典序排（稳定，且与 `--json` 里的顺序无关）。
///
/// `username-hardcoded` 排在 `dangling` 前面，但它的条件比别的理由严：
/// **只有真的有旧名可换（`rewrite_to` 有值）才成立**。一条硬编码了用户名、
/// 但名字就是当前用户名的行没有可换的旧名，按 `dangling` / `move` / `case-only` /
/// `keep` 归类，`has_username` 这个事实留在 `also` 里（决策 134 的重写规则写的是
/// "`C:\Users\<旧名>\…` → 当前用户名"：没有旧名就没有重写这件事）。
const fn priority(reason: DiffReason) -> u8 {
    match reason {
        DiffReason::EmptySegment => 0,
        DiffReason::OnlyInTarget => 1,
        DiffReason::OnlyInLocal => 2,
        DiffReason::Duplicate => 3,
        DiffReason::UsernameHardcoded => 4,
        DiffReason::Dangling => 5,
        DiffReason::PositionDiffers => 6,
        DiffReason::CaseDiffers => 7,
        DiffReason::Identical => 8,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 一行在某一侧的事实
// ─────────────────────────────────────────────────────────────────────────────

/// 一行在**某一侧**（本机 / 目标）的事实快照。
///
/// 它是"每条输出必须带来源快照里的 `owner` / `reparse` / `has_username`"这条要求的落点：
/// diff 只做判断，事实原样带给用户。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SideRow {
    /// `user` / `machine`。
    pub scope: EnvScope,
    /// 这个作用域里的位置（0 起，**空条目也占位**）。
    pub index: usize,
    /// 注册表 / 环境块里的原文（含引号）。
    pub raw: String,
    /// 比较形态：`normalize_entry(row.expanded)` —— 去首尾空白、去多余的结尾反斜杠。
    /// **配对用的就是它**（再折叠大小写），不是 `raw`。
    pub value: String,
    /// `tuoen` / `third-party` / `system` / `unknown`。
    pub owner: String,
    /// 在不在（三态）。空条目恒为 `unknown`。
    pub exists: Existence,
    /// reparse 形状。**只记录，不跟随**。
    pub reparse: ReparseKind,
    /// 值里硬编码了 `C:\Users\<某个名字>\`（含 `%` 的不算）。**判据在 capture 里**。
    pub has_username: bool,
    /// 同一个值在整份快照里第几次出现（0 = 首次）。**捕获期的跨作用域计数**，
    /// 原样带来；本模块**不**重算它（重算会让"真机 18 条重复"这个数字对不上）。
    pub dup_index: u32,
    /// 这是一段空条目（`;;`）。
    pub empty: bool,
}

impl SideRow {
    /// 从一份 `path.toml` 的行造出侧事实。
    #[must_use]
    pub fn of(row: &PathRow) -> Self {
        Self {
            scope: row.scope,
            index: row.index,
            raw: row.raw.clone(),
            value: normalize_entry(&row.expanded),
            owner: row.owner.clone(),
            exists: row.exists,
            reparse: row.reparse,
            has_username: row.has_username,
            dup_index: u32::try_from(row.dup_index).unwrap_or(u32::MAX),
            empty: row.empty,
        }
    }

    /// `"{scope}:{index}"`，如 `user:12`。**`--pick` 用的就是它。**
    #[must_use]
    pub fn id(&self) -> String {
        row_id(self.scope, self.index)
    }
}

/// `"{scope}:{index}"` —— id 的**唯一**构造处。
fn row_id(scope: EnvScope, index: usize) -> String {
    format!("{}:{}", scope.as_str(), index)
}

// ─────────────────────────────────────────────────────────────────────────────
// 一次 diff 的输出
// ─────────────────────────────────────────────────────────────────────────────

/// 逐条 diff 的一行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathDiffRow {
    /// `"{scope}:{index}"`。取值规则见模块文档（本机存在时用本机下标）。
    pub id: String,
    /// 归入的类。**恒等于 `reason.class()`。**
    pub class: DiffClass,
    /// 主理由（决定类的那一条）。
    pub reason: DiffReason,
    /// 除主理由之外还成立的理由，去重且按 slug 字典序。
    pub also: Vec<DiffReason>,
    /// 目标侧的事实。只有本机有这一条时为 `None`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<SideRow>,
    /// 本机侧的事实。只有目标有这一条时为 `None`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local: Option<SideRow>,
    /// 首次出现那一条的 `id`。`duplicate` 成立时必定给出。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicate_of: Option<String>,
    /// 重写后的**完整值**（只换 `\Users\` 后那一段）。用户名已经对、或这一行不是
    /// 用户名依赖、或调用方没给当前用户名时为 `None`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rewrite_to: Option<String>,
    /// 拼写疑似（只报告，**永不纠错**）。只对本机侧、且 `!empty && exists == no` 的行算。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

/// 一次完整 diff 的结果。**`--json` 的顶层就是它**（CLI 侧可以再包一层信封）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathDiff {
    /// 逐条，按（作用域：机器级在前，然后用户级）→（位置）排序。
    pub rows: Vec<PathDiffRow>,
    /// 各类的条数。`sum(counts) == rows.len()`。
    pub counts: DiffCounts,
    /// 用户名重写清单，**逐条报告**。
    pub rewrites: Vec<Rewrite>,
    /// 拼写疑似清单，**只报告**。
    pub typos: Vec<TypoSuspect>,
}

/// 各类的条数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffCounts {
    /// `keep` 的条数。
    pub keep: usize,
    /// `add` 的条数。
    pub add: usize,
    /// `remove` 的条数。
    pub remove: usize,
    /// `move` 的条数。**字段名带下划线是因为 `move` 是 Rust 关键字**，
    /// 序列化出来的键仍然是 `move`。
    #[serde(rename = "move")]
    pub move_: usize,
    /// `fix` 的条数。
    pub fix: usize,
    /// `case-only` 的条数。
    pub case_only: usize,
}

/// 一条用户名的重写。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rewrite {
    /// 哪一行（`PathDiffRow::id`）。
    pub id: String,
    /// 重写前的完整值。
    pub from: String,
    /// 重写后的完整值。
    pub to: String,
}

/// 一条拼写疑似。**只报告**：本模块不会替用户改任何一个字节。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypoSuspect {
    /// 哪一行（`PathDiffRow::id`）。
    pub id: String,
    /// 那一条的值（写错的那个）。
    pub value: String,
    /// 父目录里真实存在的那个目录（完整路径）。
    pub suggestion: String,
}

/// `diff` 的选项。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathDiffOptions<'a> {
    /// 当前用户名。**`None` 就没有重写**（宁可少做一步，也不猜）。
    ///
    /// 调用方给的值通常来自 `current_username_from_process(process_env)`
    /// `.or_else(|| current_username_from_env(env))` —— 进程环境是主来源，用户级注册表块
    /// 有值时更权威。本机实测 `HKCU\Environment` 里**没有** `USERPROFILE` 与 `USERNAME`
    /// （它们是进程环境里的变量），所以只读注册表块会拿到 `None`。
    /// `None` 的后果是**一条都不重写**：那些行的类不再是 `username-hardcoded`
    /// （没有旧名可换），但 `has_username` 这个事实照样在 `also` 里看得见。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_username: Option<&'a str>,
}

// ─────────────────────────────────────────────────────────────────────────────
// diff
// ─────────────────────────────────────────────────────────────────────────────

/// 一次配对：本机侧的某一行 ↔ 目标侧的某一行（任一侧可以为空）。
#[derive(Debug, Clone, Copy)]
struct Pair {
    local: Option<usize>,
    target: Option<usize>,
}

/// 输出顺序的键：作用域名次 → 位置 → 本机侧优先。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SortKey {
    scope: u8,
    index: usize,
    side: u8,
}

const fn scope_rank(scope: EnvScope) -> u8 {
    match scope {
        EnvScope::Machine => 0,
        EnvScope::User => 1,
        EnvScope::ProcessOnly => 2,
    }
}

/// 这一行参与比较吗。**只看 `Machine` / `User`**（决策 126）。
const fn compared_scope(scope: EnvScope) -> bool {
    matches!(scope, EnvScope::Machine | EnvScope::User)
}

/// 配对键：值的比较形态再折叠大小写。
pub(crate) fn compared_key(text: &str) -> String {
    normalize_entry(text).to_lowercase()
}

/// **注册表原文**的比较形态（去引号、去尾部分隔符、折叠大小写）。
///
/// 重建的"输出里不许有折叠后重复的条目"这条不变量用的就是它 ——
/// 它比的是原文，不是展开后的路径：展开需要一张变量表，而重建是纯函数。
fn raw_key(raw: &str) -> String {
    tuoen_platform::parse_entries(raw)
        .first()
        .map(|entry| entry.value.to_lowercase())
        .unwrap_or_default()
}

/// 按"值 + 出现序号"配对；空条目只在空条目之间对齐（决策 129 / 130）。
fn pair_scope(l_scope: &[&PathRow], t_scope: &[&PathRow]) -> Vec<Pair> {
    let mut pairs = Vec::new();

    // 空条目：第 k 个配第 k 个。它们**不与任何目录配对**，两边多出来的各成一列。
    let l_empty: Vec<usize> = l_scope
        .iter()
        .enumerate()
        .filter(|(_, row)| row.empty)
        .map(|(at, _)| at)
        .collect();
    let t_empty: Vec<usize> = t_scope
        .iter()
        .enumerate()
        .filter(|(_, row)| row.empty)
        .map(|(at, _)| at)
        .collect();
    for k in 0..l_empty.len().max(t_empty.len()) {
        pairs.push(Pair {
            local: l_empty.get(k).copied(),
            target: t_empty.get(k).copied(),
        });
    }

    // 非空：第 i 次出现配第 i 次出现。
    let mut buckets: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (at, row) in t_scope.iter().enumerate() {
        if !row.empty {
            buckets
                .entry(compared_key(&row.expanded))
                .or_default()
                .push(at);
        }
    }
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut claimed = vec![false; t_scope.len()];
    for (at, row) in l_scope.iter().enumerate() {
        if row.empty {
            continue;
        }
        let key = compared_key(&row.expanded);
        let nth = {
            let slot = seen.entry(key.clone()).or_insert(0);
            let nth = *slot;
            *slot += 1;
            nth
        };
        let target = buckets
            .get(&key)
            .and_then(|positions| positions.get(nth))
            .copied();
        if let Some(at) = target {
            claimed[at] = true;
        }
        pairs.push(Pair {
            local: Some(at),
            target,
        });
    }
    for (at, row) in t_scope.iter().enumerate() {
        if row.empty || claimed[at] {
            continue;
        }
        pairs.push(Pair {
            local: None,
            target: Some(at),
        });
    }
    pairs
}

/// 这一对是"目录对目录"吗 —— 位置比较只在这样的对上成立。
fn directory_pair(pair: Pair, l_scope: &[&PathRow], t_scope: &[&PathRow]) -> bool {
    match (pair.local, pair.target) {
        (Some(i), Some(j)) => !l_scope[i].empty && !t_scope[j].empty,
        _ => false,
    }
}

/// 逐条 diff 本机与目标。
///
/// **纯函数**：不读时钟、不读环境、不写任何东西。唯一的机器访问是 `fs`（只读），
/// 而且只用于"拼写疑似"（决策 133）。`fs` 建议传 [`tuoen_platform::RealFileSystem`]；
/// 测试传 [`tuoen_platform::fixture::FakeFileSystem`]。
#[must_use]
pub fn diff(
    local: &PathFile,
    target: &PathFile,
    fs: &(impl FileSystem + ?Sized),
    opts: &PathDiffOptions<'_>,
) -> PathDiff {
    let local_rows: Vec<&PathRow> = local
        .entry
        .iter()
        .filter(|row| compared_scope(row.scope))
        .collect();
    let target_rows: Vec<&PathRow> = target
        .entry
        .iter()
        .filter(|row| compared_scope(row.scope))
        .collect();

    // "首次出现"的 id：按文件顺序（捕获的顺序就是先机器级、再用户级）。
    // `dup_index` 是**跨作用域**的计数，所以这个表也必须跨作用域建。
    let mut first_id_by_key: BTreeMap<String, String> = BTreeMap::new();
    for row in &local_rows {
        if row.empty {
            continue;
        }
        first_id_by_key
            .entry(compared_key(&row.expanded))
            .or_insert_with(|| row_id(row.scope, row.index));
    }

    let mut staged: Vec<(SortKey, PathDiffRow)> = Vec::new();
    let mut staged_rewrites: Vec<(SortKey, Rewrite)> = Vec::new();
    let mut staged_typos: Vec<(SortKey, TypoSuspect)> = Vec::new();

    for scope in [EnvScope::Machine, EnvScope::User] {
        let l_scope: Vec<&PathRow> = local_rows
            .iter()
            .copied()
            .filter(|row| row.scope == scope)
            .collect();
        let t_scope: Vec<&PathRow> = target_rows
            .iter()
            .copied()
            .filter(|row| row.scope == scope)
            .collect();
        if l_scope.is_empty() && t_scope.is_empty() {
            continue;
        }

        let pairs = pair_scope(&l_scope, &t_scope);

        // 位置：**共同条目序列**里的序号。两侧各排一次，序号不同才算挪了。
        let mut local_seq: Vec<usize> = pairs
            .iter()
            .enumerate()
            .filter(|(_, pair)| directory_pair(**pair, &l_scope, &t_scope))
            .map(|(at, _)| at)
            .collect();
        local_seq.sort_by_key(|&at| pairs[at].local.unwrap_or(usize::MAX));
        let mut target_seq: Vec<usize> = pairs
            .iter()
            .enumerate()
            .filter(|(_, pair)| directory_pair(**pair, &l_scope, &t_scope))
            .map(|(at, _)| at)
            .collect();
        target_seq.sort_by_key(|&at| pairs[at].target.unwrap_or(usize::MAX));
        let mut pos_local: BTreeMap<usize, usize> = BTreeMap::new();
        for (rank, &at) in local_seq.iter().enumerate() {
            pos_local.insert(at, rank);
        }
        let mut pos_target: BTreeMap<usize, usize> = BTreeMap::new();
        for (rank, &at) in target_seq.iter().enumerate() {
            pos_target.insert(at, rank);
        }

        for (at, pair) in pairs.iter().copied().enumerate() {
            let lrow = pair.local.map(|i| l_scope[i]);
            let trow = pair.target.map(|j| t_scope[j]);
            let lside = lrow.map(SideRow::of);
            let tside = trow.map(SideRow::of);

            let is_dup = lrow.is_some_and(|row| !row.empty && row.dup_index > 0);
            let key = lrow
                .filter(|row| !row.empty)
                .map(|row| compared_key(&row.expanded));
            let duplicate_of = if is_dup {
                key.as_ref()
                    .and_then(|key| first_id_by_key.get(key))
                    .cloned()
            } else {
                None
            };

            let pos_differs = directory_pair(pair, &l_scope, &t_scope)
                && pos_local.get(&at) != pos_target.get(&at);
            // 与验收脚本的独立重算逐字对齐：比较的是**去首尾空白后**的原文
            // （`raw` 里可能带首尾空白，而 `raw.trim()` 才是用户在 PATH 里看到的写法）。
            let case_differs = match (lrow, trow) {
                (Some(l), Some(t)) => {
                    let (left, right) = (l.raw.trim(), t.raw.trim());
                    left != right && left.to_lowercase() == right.to_lowercase()
                }
                _ => false,
            };

            // `id`：本机侧的行永远是 `{scope}:{index}`；**只有目标侧才有**的行带一个 `+`
            // （`user:+16`）。这样 `user:0`（remove）与 `user:+0`（add）不再撞在一起，
            // `--pick` 不会再"想删一条、结果还顺手加了一条"（见模块文档）。
            let id = match (&lside, &tside) {
                (Some(local_side), _) => local_side.id(),
                (None, Some(target_side)) => {
                    format!("{}:+{}", target_side.scope.as_str(), target_side.index)
                }
                (None, None) => unreachable!("一列必须至少有一侧"),
            };
            let sort_key = SortKey {
                scope: scope_rank(scope),
                index: lside.as_ref().map_or_else(
                    || tside.as_ref().map_or(0, |side| side.index),
                    |side| side.index,
                ),
                side: u8::from(lside.is_none()),
            };

            // ── 用户名重写：闸门在**目标侧** ────────────────────────────
            //
            // 先算重写、再定类：**"有没有旧名可换"本身就是决定类的东西**（决策 134）。
            // 一条硬编码了用户名、但名字就是当前用户名（或者调用方没给当前用户名）的行
            // **没有可换的旧名** —— 那它就不该归 `username-hardcoded`（选中 `fix` 之后
            // 既不会重写、也不会被丢掉，用户会以为修好了）。它按其余性质归类，
            // `has_username` 这个事实留在 `also` 里。
            let mut rewrite_to = None;
            if let Some(target_side) = &tside
                && target_side.has_username
                && let Some(current) = opts.current_username
            {
                let from_value = lside
                    .as_ref()
                    .map_or_else(|| target_side.value.clone(), |side| side.value.clone());
                if let Some(old) = hardcoded_username(&from_value)
                    && !old.eq_ignore_ascii_case(current)
                    && let Some(to) = replace_username_segment(&from_value, current)
                {
                    staged_rewrites.push((
                        sort_key,
                        Rewrite {
                            id: id.clone(),
                            from: from_value,
                            to: to.clone(),
                        },
                    ));
                    rewrite_to = Some(to);
                }
            }
            let rewritable = rewrite_to.is_some();

            // ── 条件 → 主理由 ──────────────────────────────────────────
            let mut conditions: Vec<DiffReason> = Vec::new();
            if lrow.is_some_and(|row| row.empty) || trow.is_some_and(|row| row.empty) {
                conditions.push(DiffReason::EmptySegment);
            }
            if trow.is_some() && lrow.is_none() {
                conditions.push(DiffReason::OnlyInTarget);
            }
            if lrow.is_some() && trow.is_none() {
                conditions.push(DiffReason::OnlyInLocal);
            }
            if is_dup {
                conditions.push(DiffReason::Duplicate);
            }
            // `username-hardcoded` 是**动作**，不是事实：只有真的有旧名可换
            // （`rewrite_to` 有值）才配得上主理由。`has_username` 这个事实照样进 `also`
            // （除非它就是主理由）—— 见下面 `also` 的收尾。
            if rewritable {
                conditions.push(DiffReason::UsernameHardcoded);
            }
            if lrow.is_some_and(|row| !row.empty && row.exists == Existence::No) {
                conditions.push(DiffReason::Dangling);
            }
            if pos_differs {
                conditions.push(DiffReason::PositionDiffers);
            }
            if case_differs {
                conditions.push(DiffReason::CaseDiffers);
            }
            if conditions.is_empty() {
                conditions.push(DiffReason::Identical);
            }
            let reason = conditions
                .iter()
                .copied()
                .min_by_key(|reason| priority(*reason))
                .expect("条件集合非空");
            let class = reason.class();
            let mut also: Vec<DiffReason> = conditions
                .iter()
                .copied()
                .filter(|other| *other != reason)
                .collect();
            // 事实不是动作：本机这条硬编码了用户名，但名字就是当前用户名（没有旧名可换）
            // 时，它不该决定类 —— 但它必须在证据里看得见。
            if lrow.is_some_and(|row| row.has_username) && reason != DiffReason::UsernameHardcoded {
                also.push(DiffReason::UsernameHardcoded);
            }
            also.sort_by_key(|reason| reason.as_str());
            also.dedup();

            // ── 拼写疑似：只问本机侧、只问磁盘（决策 133） ──────────────
            let mut suggestion = None;
            if let Some(local_side) = &lside
                && !local_side.empty
                && local_side.exists == Existence::No
                && let Some(found) = suspected_typo(fs, &local_side.value)
            {
                staged_typos.push((
                    sort_key,
                    TypoSuspect {
                        id: id.clone(),
                        value: local_side.value.clone(),
                        suggestion: found.clone(),
                    },
                ));
                suggestion = Some(found);
            }

            staged.push((
                sort_key,
                PathDiffRow {
                    id,
                    class,
                    reason,
                    also,
                    target: tside,
                    local: lside,
                    duplicate_of,
                    rewrite_to,
                    suggestion,
                },
            ));
        }
    }

    staged.sort_by_key(|(sort_key, _)| *sort_key);
    staged_rewrites.sort_by_key(|(sort_key, _)| *sort_key);
    staged_typos.sort_by_key(|(sort_key, _)| *sort_key);

    let rows: Vec<PathDiffRow> = staged.into_iter().map(|(_, row)| row).collect();
    let mut counts = DiffCounts::default();
    for row in &rows {
        match row.class {
            DiffClass::Keep => counts.keep += 1,
            DiffClass::Add => counts.add += 1,
            DiffClass::Remove => counts.remove += 1,
            DiffClass::Move => counts.move_ += 1,
            DiffClass::Fix => counts.fix += 1,
            DiffClass::CaseOnly => counts.case_only += 1,
        }
    }

    PathDiff {
        rows,
        counts,
        rewrites: staged_rewrites.into_iter().map(|(_, item)| item).collect(),
        typos: staged_typos.into_iter().map(|(_, item)| item).collect(),
    }
}

/// 只换 `\Users\` 后紧跟的那一段，其余部分逐字不动（决策 134）。
///
/// 旧名由 [`hardcoded_username`] 给（判据只有一处定义），`\Users\` 标记用 ASCII
/// 字节比较找（**不能**先 `to_lowercase()` 再找：Unicode 的小写化会改变字节长度，
/// 那样算出来的切片位置会串位）。
fn replace_username_segment(text: &str, new_name: &str) -> Option<String> {
    let old = hardcoded_username(text)?;
    let marker = br"\users\";
    let at = text
        .as_bytes()
        .windows(marker.len())
        .position(|window| window.eq_ignore_ascii_case(marker))?;
    let start = at + marker.len();
    let rest = &text[start..];
    let end = rest.find(['\\', '/']).unwrap_or(rest.len());
    // 防串位：`\Users\` 后面那一段必须就是 `hardcoded_username` 认出来的那个名字。
    if !rest[..end].eq_ignore_ascii_case(&old) {
        return None;
    }
    Some(format!("{}{}{}", &text[..start], new_name, &rest[end..]))
}

// ─────────────────────────────────────────────────────────────────────────────
// 选择
// ─────────────────────────────────────────────────────────────────────────────

/// 用户选了什么。**两者都空 = 什么都没选**（`path apply` 必须拒绝执行，决策 135）。
///
/// 一行被选中 ⟺ 它的类在 `classes` 里 **或** 它的 `id` 在 `picks` 里。
/// `Selection` 不做归一化（去重、排序、校验都留给 CLI），只回答"选中了没有"。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    classes: Vec<DiffClass>,
    picks: Vec<String>,
}

impl Selection {
    /// 按类 + 按 id 选。
    #[must_use]
    pub fn new(classes: Vec<DiffClass>, picks: Vec<String>) -> Self {
        Self { classes, picks }
    }

    /// `classes` 与 `picks` 都空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.classes.is_empty() && self.picks.is_empty()
    }

    /// 这个类被选中了吗（`--only`）。
    #[must_use]
    pub fn selects_class(&self, class: DiffClass) -> bool {
        self.classes.contains(&class)
    }

    /// 这一行被选中了吗：类在 `classes` 里 **或** id 在 `picks` 里。
    #[must_use]
    pub fn selects(&self, row: &PathDiffRow) -> bool {
        self.selects_class(row.class) || self.picks.contains(&row.id)
    }

    /// `--only` 的原样列表。
    #[must_use]
    pub fn classes(&self) -> &[DiffClass] {
        &self.classes
    }

    /// `--pick` 的原样列表。
    #[must_use]
    pub fn picks(&self) -> &[String] {
        &self.picks
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 重建
// ─────────────────────────────────────────────────────────────────────────────

/// 一次重建的结果：一份完整的新有序列表 + 逐条"动了什么"。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rebuild {
    /// 逐作用域。**只出现"有东西"的作用域**：本机侧有行、或者有改动。
    /// （一个没有本机行、也没有改动的作用域，`after_raw` 会是空串 ——
    /// 把它写回注册表等于删掉那个 `Path`。所以本模块干脆不产出它。）
    pub scopes: Vec<RebuiltScope>,
    /// 重建前的长度预算（机器级原文 + 用户级原文）。
    pub budget_before: PathBudgetRow,
    /// 重建后的长度预算。**下界**：不含进程注入项，机器级改动也未计入。
    pub budget: PathBudgetRow,
    /// **被选中、但没有任何变换可做**的类（去重、按 slug 稳定排序）。
    ///
    /// 这是给 CLI 打印"你选了 X，但这一类不做任何改写"用的：`case-only` 永远在这里
    /// （决策 128：改大小写零收益、纯风险），别的类在"选了但没有行、或者有行却一条
    /// 都没被改动"时也在这里。判据是"这个类有没有产出过任何 `applied` 条目"——
    /// 于是"选了 `keep`"、"选了 `remove` 但本机一条 remove 都没有"、
    /// "选了 `add` 但那些 add 因为输出不变量全被跳过"都会如实落进来。
    pub no_op_classes: Vec<DiffClass>,
}

/// 一个作用域的重建结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RebuiltScope {
    /// 哪个作用域。
    pub scope: EnvScope,
    /// 重建前该作用域的注册表原文（本机行 `raw` 用 `;` 拼回来，**逐字**）。
    pub before_raw: String,
    /// 重建后的完整原文（写回就是整值替换成它）。
    pub after_raw: String,
    /// 重建前的条目（`raw`），下标就是 `applied.fromIndex` 的坐标系。
    pub before_entries: Vec<String>,
    /// 重建后的条目（`raw`），下标就是 `applied.toIndex` 的坐标系。
    pub after_entries: Vec<String>,
    /// 重建前的注册表类型。写回时 `write_type_for` 要它（决策 137）。
    pub before_type: Option<RegType>,
    /// 逐条"动了什么"，按落点排序。
    pub applied: Vec<AppliedRow>,
    /// 本机侧**没有真的被改动**的条目数（"选中了但结果没变"也算没动）。
    pub untouched: usize,
    /// 机器级 + 有改动 → 需要提权。**本模块不写任何东西**（决策 136）。
    pub requires_elevation: bool,
}

/// 一行因为哪一类被动了、从第几位到第几位。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedRow {
    /// 哪一行（`PathDiffRow::id`）。
    pub id: String,
    /// 因为哪一类被动。
    pub class: DiffClass,
    /// 结果值：丢掉的记它原来的值，重写的记新值，移动的记原值，插入的记插进去的那个值。
    pub value: String,
    /// `before_entries` 里的位置。插入（含被跳过的插入）没有来源，为 `None`。
    /// **丢掉的行 `toIndex == fromIndex`** —— 它没有落点。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_index: Option<usize>,
    /// `after_entries` 里的落点。
    pub to_index: usize,
}

/// 槽位的来源：基底里的第 `p` 条，还是 `applied` 里的第 `a` 条（这一次插进来的）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Src {
    Base(usize),
    New(usize),
}

/// 重建中的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Slot {
    /// 它的 `id`。**只用于报告**：槽位的定位一律用 [`Src::Base`] 的下标，
    /// 因为 `add` 行的 id 可能与本机某一行的 id 撞上（见模块文档）。
    id: String,
    raw: String,
    key: String,
    src: Src,
    /// 它在**目标列表**里的位置。本机独有的行为 `None`（它不是锚点）。
    target_pos: Option<usize>,
}

/// 一个作用域的重建中间结果。
#[derive(Debug, Clone)]
struct ScopeBuild {
    scope: EnvScope,
    before_raw: String,
    after_raw: String,
    before_entries: Vec<String>,
    after_entries: Vec<String>,
    before_type: Option<RegType>,
    applied: Vec<AppliedRow>,
    untouched: usize,
    requires_elevation: bool,
}

impl ScopeBuild {
    /// 有东西可写吗（本机侧有行，或者有改动）。
    fn has_content(&self) -> bool {
        !self.before_entries.is_empty() || !self.applied.is_empty()
    }

    fn into_scope(self) -> RebuiltScope {
        RebuiltScope {
            scope: self.scope,
            before_raw: self.before_raw,
            after_raw: self.after_raw,
            before_entries: self.before_entries,
            after_entries: self.after_entries,
            before_type: self.before_type,
            applied: self.applied,
            untouched: self.untouched,
            requires_elevation: self.requires_elevation,
        }
    }
}

/// 一个待放置的候选行（选中的 `add` 或 `move`）。
#[derive(Debug, Clone)]
struct Candidate<'a> {
    row: &'a PathDiffRow,
    /// 它在目标列表里的位置 —— 放置规则唯一的坐标。
    target_pos: usize,
    kind: DiffClass,
}

/// 按选中的类别/条目重建某个作用域的列表。**纯函数**。
fn rebuild_scope(
    scope: EnvScope,
    local_rows: &[&PathRow],
    diff: &PathDiff,
    selection: &Selection,
) -> ScopeBuild {
    let before_entries: Vec<String> = local_rows.iter().map(|row| row.raw.clone()).collect();
    let before_raw = before_entries.join(";");
    // 一个良构的捕获里同一 scope 的所有行共享同一个注册表类型；取第一条有类型的行。
    let before_type = local_rows.iter().find_map(|row| row.reg_type);
    // `PathRow::index` → 基底里的位置。**槽位的定位用它，不用 id 字符串。**
    let base_of: BTreeMap<usize, usize> = local_rows
        .iter()
        .enumerate()
        .map(|(at, row)| (row.index, at))
        .collect();

    // 本机侧 id → diff 行，以及"本机侧这一条在目标列表里的位置"。
    //
    // ⚠ 只认**本机侧存在**的行：目标侧独有的行用的是目标下标，那个数字可能与本机某
    // 一行的下标相同（见模块文档里的 id 冲突），所以绝不能用 id 字符串反过来索引本机行。
    let mut row_by_local: BTreeMap<String, &PathDiffRow> = BTreeMap::new();
    let mut target_pos_of: BTreeMap<usize, Option<usize>> = BTreeMap::new();
    for row in &diff.rows {
        if let Some(local_side) = &row.local
            && local_side.scope == scope
        {
            target_pos_of.insert(local_side.index, row.target.as_ref().map(|side| side.index));
            row_by_local.insert(local_side.id(), row);
        }
    }

    let mut slots: Vec<Option<Slot>> = local_rows
        .iter()
        .enumerate()
        .map(|(at, row)| {
            Some(Slot {
                key: raw_key(&row.raw),
                target_pos: target_pos_of.get(&row.index).copied().flatten(),
                id: row_id(scope, row.index),
                raw: row.raw.clone(),
                src: Src::Base(at),
            })
        })
        .collect();

    let mut applied: Vec<AppliedRow> = Vec::new();

    // ── ① 选中的 fix / remove ────────────────────────────────────────
    for (at, row) in local_rows.iter().enumerate() {
        let id = row_id(scope, row.index);
        let Some(diff_row) = row_by_local.get(&id).copied() else {
            continue;
        };
        if !selection.selects(diff_row) {
            continue;
        }
        let local_value = diff_row
            .local
            .as_ref()
            .map_or_else(String::new, |side| side.value.clone());
        match diff_row.class {
            DiffClass::Remove => {
                slots[at] = None;
                applied.push(AppliedRow {
                    id,
                    class: DiffClass::Remove,
                    value: local_value,
                    from_index: Some(at),
                    to_index: at,
                });
            }
            DiffClass::Fix => match diff_row.reason {
                DiffReason::UsernameHardcoded => {
                    // `rewrite_to == None` = 用户名已经对，这一行**什么都没变**
                    // （它不进 `applied`，也算 `untouched`）。
                    let Some(to) = diff_row.rewrite_to.clone() else {
                        continue;
                    };
                    let Some(new_name) = hardcoded_username(&to) else {
                        continue;
                    };
                    let Some(rewritten) = replace_username_segment(&row.raw, &new_name) else {
                        continue;
                    };
                    if let Some(slot) = slots[at].as_mut() {
                        slot.key = raw_key(&rewritten);
                        slot.raw = rewritten;
                    }
                    applied.push(AppliedRow {
                        id,
                        class: DiffClass::Fix,
                        value: to,
                        from_index: Some(at),
                        to_index: at,
                    });
                }
                DiffReason::EmptySegment | DiffReason::Duplicate | DiffReason::Dangling => {
                    slots[at] = None;
                    applied.push(AppliedRow {
                        id,
                        class: DiffClass::Fix,
                        value: local_value,
                        from_index: Some(at),
                        to_index: at,
                    });
                }
                // `class == reason.class()` 是硬不变量，所以这里到不了。
                _ => continue,
            },
            DiffClass::Keep | DiffClass::Add | DiffClass::Move | DiffClass::CaseOnly => {}
        }
    }

    // ── ② 选中的 add / move：同一条放置规则，按目标顺序的**逆序** ──────
    let mut candidates: Vec<Candidate<'_>> = Vec::new();
    for row in &diff.rows {
        if !selection.selects(row) {
            continue;
        }
        match row.class {
            DiffClass::Add => {
                if let Some(target_side) = &row.target
                    && target_side.scope == scope
                {
                    candidates.push(Candidate {
                        row,
                        target_pos: target_side.index,
                        kind: DiffClass::Add,
                    });
                }
            }
            DiffClass::Move => {
                if let Some(local_side) = &row.local
                    && local_side.scope == scope
                    && let Some(target_side) = &row.target
                {
                    candidates.push(Candidate {
                        row,
                        target_pos: target_side.index,
                        kind: DiffClass::Move,
                    });
                }
            }
            _ => {}
        }
    }
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.target_pos));

    // "被跳过的插入"最后要回填成已存在那条的**最终**下标。
    let mut skipped: Vec<(usize, String)> = Vec::new();

    for candidate in candidates {
        match candidate.kind {
            DiffClass::Add => {
                let target_side = candidate.row.target.as_ref().expect("add 行必有目标侧");
                // 目标侧的旧用户名在这里生效：插进去的是重写后的值（Lead 的澄清）。
                let value = candidate
                    .row
                    .rewrite_to
                    .clone()
                    .unwrap_or_else(|| target_side.value.clone());
                let key = raw_key(&value);
                if let Some(at) = slots.iter().flatten().position(|slot| slot.key == key) {
                    let applied_at = applied.len();
                    applied.push(AppliedRow {
                        id: candidate.row.id.clone(),
                        class: DiffClass::Fix,
                        value,
                        from_index: None,
                        to_index: at,
                    });
                    skipped.push((applied_at, key));
                    continue;
                }
                let at = anchor(&slots, candidate.target_pos);
                let applied_at = applied.len();
                applied.push(AppliedRow {
                    id: candidate.row.id.clone(),
                    class: DiffClass::Add,
                    value: value.clone(),
                    from_index: None,
                    to_index: at,
                });
                slots.insert(
                    at,
                    Some(Slot {
                        id: candidate.row.id.clone(),
                        raw: value.clone(),
                        key,
                        src: Src::New(applied_at),
                        target_pos: Some(candidate.target_pos),
                    }),
                );
            }
            DiffClass::Move => {
                let local_side = candidate.row.local.as_ref().expect("move 行必有本机侧");
                let Some(&base) = base_of.get(&local_side.index) else {
                    continue;
                };
                let Some(from) = slots.iter().position(|slot| {
                    slot.as_ref()
                        .is_some_and(|slot| slot.src == Src::Base(base))
                }) else {
                    continue;
                };
                let Some(slot) = slots.remove(from) else {
                    continue;
                };
                let at = anchor(&slots, candidate.target_pos);
                let applied_at = applied.len();
                applied.push(AppliedRow {
                    id: local_side.id(),
                    class: DiffClass::Move,
                    value: local_side.value.clone(),
                    from_index: Some(base),
                    to_index: at,
                });
                slots.insert(
                    at,
                    Some(Slot {
                        src: Src::New(applied_at),
                        ..slot
                    }),
                );
            }
            _ => {}
        }
    }

    // ── ③ 收尾：回填落点、数 `untouched` ──────────────────────────────
    let mut after_entries: Vec<String> = Vec::with_capacity(slots.len());
    for slot in slots.iter().flatten() {
        let at = after_entries.len();
        after_entries.push(slot.raw.clone());
        if let Src::New(applied_at) = slot.src {
            applied[applied_at].to_index = at;
        }
    }
    for (applied_at, key) in skipped {
        if let Some(at) = slots.iter().flatten().position(|slot| slot.key == key) {
            applied[applied_at].to_index = at;
        }
    }

    let touched: BTreeSet<usize> = applied.iter().filter_map(|row| row.from_index).collect();
    let untouched = local_rows.len().saturating_sub(touched.len());
    applied.sort_by(|a, b| {
        (a.to_index, a.from_index.unwrap_or(usize::MAX), &a.id).cmp(&(
            b.to_index,
            b.from_index.unwrap_or(usize::MAX),
            &b.id,
        ))
    });

    let requires_elevation = scope == EnvScope::Machine && !applied.is_empty();
    ScopeBuild {
        scope,
        before_raw,
        after_raw: after_entries.join(";"),
        before_entries,
        after_entries,
        before_type,
        applied,
        untouched,
        requires_elevation,
    }
}

/// 放置规则（决策 132）：插到"在目标列表里排在它之后"的**第一条**行之前；没有就追加。
///
/// 锚点可以是**已经插进来的行**（它们也有目标位置）—— 少了这一条，连续插两条
/// `add` 会在逆序处理时把后一条排到前一条前面。
fn anchor(slots: &[Option<Slot>], target_pos: usize) -> usize {
    for (at, slot) in slots.iter().enumerate() {
        let Some(slot) = slot else {
            continue;
        };
        if slot.target_pos.is_some_and(|pos| pos > target_pos) {
            return at;
        }
    }
    slots.len()
}

/// 按选择重建两个作用域，产出一份完整的新有序列表。
///
/// **纯函数**：不写注册表、不读环境、不碰磁盘（决策 139 的"预览与执行共用同一份计划"
/// 就建立在这条性质上）。写回是 CLI + platform 层的事，而且**机器级的改动不落盘**
/// （决策 136）：这里照常把 `after_raw` 算出来，只在 `requires_elevation` 上说清
/// "这一步要提权"。
///
/// `target` 与 `diff` 必须来自同一次 [`diff`]。放置坐标全部取自 `diff.rows`，
/// 所以 `target` 只用于一条 `debug_assert`：`diff` 里的目标下标必须真的落在 `target`
/// 的条目里 —— 三份输入自相矛盾时，宁可在这里就吵，也不要在写回之后才发现。
///
/// # Panics
///
/// 只在 `debug_assert` 下（`diff` 与 `target` 对不上时）。
#[must_use]
pub fn rebuild(
    local: &PathFile,
    target: &PathFile,
    diff: &PathDiff,
    selection: &Selection,
) -> Rebuild {
    debug_assert!(
        diff.rows
            .iter()
            .all(|row| row.target.as_ref().is_none_or(|side| {
                target
                    .entry
                    .iter()
                    .any(|entry| entry.scope == side.scope && entry.index == side.index)
            })),
        "Rebuild 的 target 与 diff 不是同一次采集出来的"
    );

    let machine_local: Vec<&PathRow> = local
        .entry
        .iter()
        .filter(|row| row.scope == EnvScope::Machine)
        .collect();
    let user_local: Vec<&PathRow> = local
        .entry
        .iter()
        .filter(|row| row.scope == EnvScope::User)
        .collect();

    let machine = rebuild_scope(EnvScope::Machine, &machine_local, diff, selection);
    let user = rebuild_scope(EnvScope::User, &user_local, diff, selection);

    let machine_before_chars = machine.before_raw.chars().count();
    let machine_after_chars = machine.after_raw.chars().count();
    let user_before_chars = user.before_raw.chars().count();
    let user_after_chars = user.after_raw.chars().count();

    // 档位由 platform 的 `PathBudget::of` 决定（阈值只有那一处定义）。
    // 两个参数相同：重建看得到的只有注册表原文，进程注入项不在输入里。
    let before = PathBudget::of(
        machine_before_chars + user_before_chars,
        machine_before_chars + user_before_chars,
    );
    let after = PathBudget::of(
        machine_after_chars + user_after_chars,
        machine_after_chars + user_after_chars,
    );

    let mut scopes = Vec::new();
    if machine.has_content() {
        scopes.push(machine.into_scope());
    }
    if user.has_content() {
        scopes.push(user.into_scope());
    }

    // ── 选了但什么都没做的类 ──────────────────────────────────────────
    //
    // "被选中"包括两件事：类在 `--only` 里，或者某条行被 `--pick` 选中（那样它的类
    // 也在场）。"什么都没做"的判据是**这个类没有产出任何 `applied` 条目** ——
    // 重建里每一次真的改动都会记一条 `applied`（含"插入被输出不变量跳过"那条），
    // 所以这个判据与"这一类的 fix 到底做了什么"是同一件事。
    let mut in_play: Vec<DiffClass> = selection.classes().to_vec();
    for row in &diff.rows {
        if selection.selects(row) {
            in_play.push(row.class);
        }
    }
    in_play.sort_by_key(|class| class.as_str());
    in_play.dedup();
    let no_op_classes: Vec<DiffClass> = in_play
        .into_iter()
        .filter(|class| {
            !scopes
                .iter()
                .any(|scope| scope.applied.iter().any(|row| row.class == *class))
        })
        .collect();

    Rebuild {
        scopes,
        budget_before: budget_row(before, user_before_chars, machine_before_chars),
        budget: budget_row(after, user_after_chars, machine_after_chars),
        no_op_classes,
    }
}

/// 照 `capture/collect/path.rs` 的写法把 [`PathBudget`] 填成落盘形状。
fn budget_row(
    budget: PathBudget,
    raw_user_chars: usize,
    raw_machine_chars: usize,
) -> PathBudgetRow {
    PathBudgetRow {
        raw_user_chars,
        raw_machine_chars,
        effective_chars: budget.effective_chars,
        cliff: budget.cliff,
        remaining: budget.remaining,
        level: budget.level.as_str().to_owned(),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 两个 I/O 入口
// ─────────────────────────────────────────────────────────────────────────────

/// 读一份 `PATH` 快照（`tuoen.d/path.toml`）。
///
/// 两个错误码是稳定契约：`path-snapshot-io`（读不到）/ `path-snapshot-toml`（不是
/// 合法的 `PathFile`）。**两者必须分得开**：第一种是"文件不在/没权限"，第二种是
/// "文件在那儿但内容变了" —— 用户要做的下一步完全不同。
///
/// # Errors
///
/// 读文件失败，或 TOML 反序列化失败。
pub fn load_snapshot(path: &Path) -> Result<PathFile, PathDiffError> {
    let text = std::fs::read_to_string(path).map_err(|source| PathDiffError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    toml::from_str(&text).map_err(|source| PathDiffError::Toml {
        path: path.to_path_buf(),
        source,
    })
}

/// 读快照失败的原因。
#[derive(Debug, thiserror::Error)]
pub enum PathDiffError {
    /// 读不到（不存在 / 没权限 / 不是文件）。
    #[error("读不到 PATH 快照 {path}：{source}")]
    Io {
        /// 被读的路径。
        path: PathBuf,
        /// 底层的 I/O 错误。
        #[source]
        source: std::io::Error,
    },
    /// 读到了，但内容不是一份合法的 `PathFile`。
    #[error("PATH 快照 {path} 不是合法的 TOML：{source}")]
    Toml {
        /// 被读的路径。
        path: PathBuf,
        /// 底层的 TOML 错误。
        #[source]
        source: toml::de::Error,
    },
}

impl PathDiffError {
    /// 稳定小写 slug（给脚本与 `--json`，**不本地化**）。
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Io { .. } => "path-snapshot-io",
            Self::Toml { .. } => "path-snapshot-toml",
        }
    }
}

/// 当前用户名：读**用户级环境块**（`EnvScope::User`）的 `USERPROFILE` 取最后一段，
/// 再退 `USERNAME`，都空 → `None`。
///
/// **不 spawn `whoami`**：读一个环境变量不该起进程（决策 134）。
/// 也不展开 `%VAR%`（值里有变量时"最后一段"照样是那个名字）。
/// **只问用户级注册表块**：进程环境里那份可能被调用方改过，不能当成"这一台机器的
/// 用户名" —— 那是 [`current_username_from_process`] 的口径，两个函数的分工见下。
///
/// # 与 [`current_username_from_process`] 的分工
///
/// 本机实测 `HKCU\Environment` 里**既没有 `USERPROFILE` 也没有 `USERNAME`**
/// （它们是进程环境里的变量，不是注册表值）—— 所以**这一票真正的用户名来源是进程环境**。
/// 注册表块里有值时它更权威（那是用户显式设过的），所以调用方的顺序是
/// `current_username_from_process(process_env).or_else(|| current_username_from_env(env))`。
/// 两个函数都不猜、都不兜底：都给不出名字时 `rewrites` 就是空的，而
/// `rewrites: []` 与"用户名恰好就是当前用户名"在输出上长得一模一样，别把两者混为一谈。
#[must_use]
pub fn current_username_from_env(block: &impl EnvBlock) -> Option<String> {
    username_from_values(
        block
            .get(EnvScope::User, "USERPROFILE")
            .map(|var| var.value_raw),
        block
            .get(EnvScope::User, "USERNAME")
            .map(|var| var.value_raw),
    )
}

/// 当前用户名：读**进程环境**（`ProcessEnv::vars()`）的 `USERPROFILE` 取最后一段，
/// 再退 `USERNAME`，都空 → `None`。
///
/// 这是真实路径上的主来源（见 [`current_username_from_env`] 里的实测）。与它一样：
/// 不 spawn 进程、不展开 `%VAR%`、**不读注册表块**（名字大小写不敏感）。
#[must_use]
pub fn current_username_from_process(process: &impl ProcessEnv) -> Option<String> {
    let vars = process.vars();
    let get = |name: &str| {
        vars.iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    };
    username_from_values(get("USERPROFILE"), get("USERNAME"))
}

/// 两个来源共用的判据：`USERPROFILE` 的最后一段优先，退 `USERNAME`（去空白、非空）。
fn username_from_values(profile: Option<String>, username: Option<String>) -> Option<String> {
    if let Some(value) = profile
        && let Some(name) = last_segment(&value)
    {
        return Some(name);
    }
    let name = username?;
    let name = name.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_owned())
    }
}

/// 一条路径的最后一段。根（`C:\`）与空值没有最后一段 → `None`。
fn last_segment(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_end_matches(['\\', '/']);
    if trimmed.is_empty() {
        return None;
    }
    let name = trimmed.rsplit(['\\', '/']).next().unwrap_or(trimmed).trim();
    if name.is_empty() || name.ends_with(':') {
        None
    } else {
        Some(name.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::EntryRefRow;
    use crate::pathdiff::test_support::{
        FakeEnvBlock, diff_of, fake_fs, path_file, row, selection,
    };
    use tuoen_platform::InMemoryEnv;

    fn nothing() -> Selection {
        Selection::new(Vec::new(), Vec::new())
    }

    fn options(username: Option<&str>) -> PathDiffOptions<'_> {
        PathDiffOptions {
            current_username: username,
        }
    }

    // ── 1. 六类各一个 ────────────────────────────────────────────────

    #[test]
    fn class_keep_is_identical_rows_at_the_same_position() {
        let local = path_file(&[
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, r"C:\b", Existence::Yes),
        ]);
        let target = local.clone();
        let diff = diff_of(&local, &target, &fake_fs(&[]));
        assert_eq!(diff.rows.len(), 2);
        assert_eq!(diff.rows[0].class, DiffClass::Keep);
        assert_eq!(diff.rows[0].reason, DiffReason::Identical);
        assert_eq!(diff.rows[0].id, "user:0");
        assert!(diff.rows[0].also.is_empty());
        assert_eq!(diff.rows[1].id, "user:1");
        assert_eq!(diff.counts.keep, 2);
    }

    #[test]
    fn class_add_is_a_target_row_the_local_machine_does_not_have() {
        let local = path_file(&[(EnvScope::User, r"C:\a", Existence::Yes)]);
        let target = path_file(&[
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, r"C:\new", Existence::Yes),
        ]);
        let diff = diff_of(&local, &target, &fake_fs(&[]));
        assert_eq!(diff.counts.add, 1);
        let added = diff
            .rows
            .iter()
            .find(|row| row.class == DiffClass::Add)
            .expect("应有一条 add");
        assert_eq!(added.reason, DiffReason::OnlyInTarget);
        assert_eq!(
            added.id, "user:+1",
            "只有目标侧才有的行带一个 `+`：本机侧的行永远是 `{{scope}}:{{index}}`"
        );
        assert!(added.local.is_none());
        assert_eq!(added.target.as_ref().expect("目标侧").index, 1);
    }

    #[test]
    fn class_remove_is_a_local_row_the_target_does_not_have() {
        let local = path_file(&[
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, r"C:\stale", Existence::Yes),
        ]);
        let target = path_file(&[(EnvScope::User, r"C:\a", Existence::Yes)]);
        let diff = diff_of(&local, &target, &fake_fs(&[]));
        assert_eq!(diff.counts.remove, 1);
        let removed = diff
            .rows
            .iter()
            .find(|row| row.class == DiffClass::Remove)
            .expect("应有一条 remove");
        assert_eq!(removed.reason, DiffReason::OnlyInLocal);
        assert_eq!(removed.id, "user:1", "本机存在时用本机下标");
        assert_eq!(removed.local.as_ref().expect("本机侧").index, 1);
        assert!(removed.target.is_none());
    }

    #[test]
    fn class_move_is_a_pair_whose_relative_position_changed() {
        let local = path_file(&[
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, r"C:\b", Existence::Yes),
        ]);
        let target = path_file(&[
            (EnvScope::User, r"C:\b", Existence::Yes),
            (EnvScope::User, r"C:\a", Existence::Yes),
        ]);
        let diff = diff_of(&local, &target, &fake_fs(&[]));
        assert_eq!(diff.counts.move_, 2);
        assert_eq!(diff.rows.len(), 2);
        for row in &diff.rows {
            assert_eq!(row.class, DiffClass::Move);
            assert_eq!(row.reason, DiffReason::PositionDiffers);
        }
        // 输出按本机下标排：a 在前（user:0），b 在后（user:1）。
        assert_eq!(diff.rows[0].id, "user:0");
        assert_eq!(diff.rows[0].local.as_ref().expect("本机侧").value, r"C:\a");
        assert_eq!(diff.rows[1].id, "user:1");
    }

    #[test]
    fn class_fix_is_a_healthy_target_row_but_a_broken_local_one() {
        let local = path_file(&[(EnvScope::User, r"C:\gone", Existence::No)]);
        let target = path_file(&[(EnvScope::User, r"C:\gone", Existence::Yes)]);
        let diff = diff_of(&local, &target, &fake_fs(&[]));
        assert_eq!(diff.counts.fix, 1);
        assert_eq!(diff.rows[0].class, DiffClass::Fix);
        assert_eq!(diff.rows[0].reason, DiffReason::Dangling);
        assert_eq!(diff.rows[0].id, "user:0");
        assert!(diff.rows[0].also.is_empty());
        // 目标侧的 `exists` 是**源机器**的事实，不参与判断，但照抄进 SideRow。
        assert_eq!(
            diff.rows[0].target.as_ref().expect("目标侧").exists,
            Existence::Yes
        );
    }

    #[test]
    fn class_case_only_is_the_same_directory_spelled_differently() {
        let local = path_file(&[(EnvScope::User, r"C:\Tools", Existence::Yes)]);
        let target = path_file(&[(EnvScope::User, r"c:\tools", Existence::Yes)]);
        let diff = diff_of(&local, &target, &fake_fs(&[]));
        assert_eq!(diff.rows.len(), 1);
        assert_eq!(diff.rows[0].class, DiffClass::CaseOnly);
        assert_eq!(diff.rows[0].reason, DiffReason::CaseDiffers);
        assert_eq!(diff.rows[0].id, "user:0");
    }

    // ── 2. 仅大小写差异绝不是 add + remove ───────────────────────────

    #[test]
    fn a_case_only_row_never_becomes_an_add_plus_a_remove() {
        let local = path_file(&[
            (EnvScope::Machine, r"C:\Shared\bin", Existence::Yes),
            (EnvScope::User, r"C:\Tools", Existence::Yes),
            (EnvScope::User, r"C:\keep", Existence::Yes),
        ]);
        let target = path_file(&[
            (EnvScope::Machine, r"c:\shared\BIN", Existence::Yes),
            (EnvScope::User, r"c:\tools", Existence::Yes),
            (EnvScope::User, r"C:\keep", Existence::Yes),
        ]);
        let diff = diff_of(&local, &target, &fake_fs(&[]));
        assert_eq!(diff.counts.case_only, 2);
        assert_eq!(diff.counts.add, 0, "同一个目录不该被报成 add");
        assert_eq!(diff.counts.remove, 0, "同一个目录不该被报成 remove");
        assert_eq!(diff.counts.keep, 1);
        assert_eq!(diff.rows.len(), 3);
    }

    // ── 3. 去重保留首次出现 ──────────────────────────────────────────

    #[test]
    fn dedup_keeps_the_first_occurrence_and_never_reports_a_move() {
        let local = path_file(&[
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, r"C:\b", Existence::Yes),
            (EnvScope::User, r"C:\a", Existence::Yes),
        ]);
        let target = local.clone();
        let diff = diff_of(&local, &target, &fake_fs(&[]));
        assert_eq!(diff.rows.len(), 3);
        assert_eq!(diff.counts.keep, 2);
        assert_eq!(diff.counts.fix, 1);
        assert_eq!(
            diff.counts.move_, 0,
            "按值 + 出现序号配对，重复不该报成 move"
        );
        assert_eq!(diff.counts.add, 0);
        assert_eq!(diff.counts.remove, 0);
        assert_eq!(diff.rows[0].class, DiffClass::Keep);
        assert_eq!(diff.rows[2].class, DiffClass::Fix);
        assert_eq!(diff.rows[2].reason, DiffReason::Duplicate);
        assert_eq!(diff.rows[2].id, "user:2");
        assert_eq!(diff.rows[2].duplicate_of.as_deref(), Some("user:0"));
    }

    #[test]
    fn a_duplicate_that_the_target_lacks_keeps_both_facts_visible() {
        // 决策 129 说"后续出现归 `duplicate`"，决策 127 说"存在性先于健康"。
        // 一条既重复、目标里又没有的条目同时命中两条：主理由必须是存在性
        // （`remove`/`only-in-local`），而 `duplicate` 与 `duplicate_of` 一条都不许丢 ——
        // 两类给出的**动作**其实一样（都是把这一条丢掉），但用户读到的是两件不同的事。
        let local = path_file(&[
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, r"C:\a", Existence::Yes),
        ]);
        let target = path_file(&[(EnvScope::User, r"C:\a", Existence::Yes)]);
        let diff = diff_of(&local, &target, &fake_fs(&[]));
        assert_eq!(diff.counts.remove, 1);
        assert_eq!(diff.counts.fix, 0);
        assert_eq!(diff.rows[1].class, DiffClass::Remove);
        assert_eq!(diff.rows[1].reason, DiffReason::OnlyInLocal);
        assert_eq!(diff.rows[1].also, vec![DiffReason::Duplicate]);
        assert_eq!(diff.rows[1].duplicate_of.as_deref(), Some("user:0"));
    }

    #[test]
    fn duplicate_of_can_point_across_scopes_because_the_counter_is_global() {
        // 捕获期的 `dup_index` 跨三个作用域计数，所以用户级的那条会指向机器级那条；
        // 而 `id` 里的下标是**作用域内**的（两条各自是 `:0`）。
        let local = path_file(&[
            (EnvScope::Machine, r"C:\shared", Existence::Yes),
            (EnvScope::User, r"C:\shared", Existence::Yes),
        ]);
        let target = local.clone();
        let diff = diff_of(&local, &target, &fake_fs(&[]));
        let duplicate = diff
            .rows
            .iter()
            .find(|row| row.reason == DiffReason::Duplicate)
            .expect("用户级那条是重复");
        assert_eq!(duplicate.id, "user:0");
        assert_eq!(duplicate.duplicate_of.as_deref(), Some("machine:0"));
    }

    // ── 4. 空条目 ─────────────────────────────────────────────────────

    #[test]
    fn empty_segments_are_three_fix_rows_and_never_pair_with_a_directory() {
        // 本机 `;C:\a;;C:\b;` → 开头、中间、结尾各一个空条目。
        let local = path_file(&[
            (EnvScope::User, "", Existence::Unknown),
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, "", Existence::Unknown),
            (EnvScope::User, r"C:\b", Existence::Yes),
            (EnvScope::User, "", Existence::Unknown),
        ]);
        let target = path_file(&[
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, r"C:\b", Existence::Yes),
        ]);
        let diff = diff_of(&local, &target, &fake_fs(&[]));
        assert_eq!(diff.rows.len(), 5);
        assert_eq!(diff.counts.fix, 3, "三个空条目各成一行");
        assert_eq!(diff.counts.keep, 2);
        assert_eq!(diff.counts.add, 0, "空条目不算 add");
        assert_eq!(diff.counts.remove, 0, "空条目不算 remove");
        for row in &diff.rows {
            if row.reason == DiffReason::EmptySegment {
                assert_eq!(row.class, DiffClass::Fix);
                assert!(row.local.as_ref().expect("本机侧").empty);
                assert!(
                    row.target.as_ref().is_none_or(|side| side.empty),
                    "空条目不许与目录配对"
                );
            }
        }
        let empties: Vec<&str> = diff
            .rows
            .iter()
            .filter(|row| row.reason == DiffReason::EmptySegment)
            .map(|row| row.id.as_str())
            .collect();
        assert_eq!(empties, vec!["user:0", "user:2", "user:4"]);
    }

    #[test]
    fn a_target_only_empty_segment_is_a_fix_row_that_changes_nothing() {
        let local = path_file(&[(EnvScope::User, r"C:\a", Existence::Yes)]);
        let target = path_file(&[
            (EnvScope::User, "", Existence::Unknown),
            (EnvScope::User, r"C:\a", Existence::Yes),
        ]);
        let diff = diff_of(&local, &target, &fake_fs(&[]));
        let empty = diff
            .rows
            .iter()
            .find(|row| row.reason == DiffReason::EmptySegment)
            .expect("目标侧的空条目也要报出来");
        assert_eq!(empty.class, DiffClass::Fix);
        assert!(empty.local.is_none());
        assert_eq!(empty.target.as_ref().expect("目标侧").index, 0);
        assert_eq!(diff.counts.keep, 1);

        let rebuilt = rebuild(&local, &target, &diff, &selection(&[DiffClass::Fix], &[]));
        let scope = &rebuilt.scopes[0];
        assert_eq!(
            scope.after_entries, scope.before_entries,
            "空条目不会被插进来"
        );
        assert!(scope.applied.is_empty());
    }

    // ── 5. 8191 悬崖 ─────────────────────────────────────────────────

    fn sized(fill: char, total: usize) -> String {
        // `C:\` + 重复填充 = 恰好 total 个字符。
        let mut text = String::from(r"C:\");
        text.extend(std::iter::repeat_n(fill, total - 3));
        assert_eq!(text.chars().count(), total);
        text
    }

    #[test]
    fn the_rebuilt_budget_follows_the_real_thresholds_at_the_8191_cliff() {
        // `PathBudget::of` 的真实档位（阈值只有那一处定义）：
        //   > 8191 → exceeded；≥ 8191*90% = 7371.9 → critical；≥ 8191*75% = 6143.25 → warning。
        // 所以 1719 → ok、8190 → critical、8191 → critical、8192 → exceeded。
        // （简报里"8190/8191 落在 warning 档"的猜测是错的：8190 已经超过 90%。）
        for (total, expected) in [
            (1719usize, "ok"),
            (8190, "critical"),
            (8191, "critical"),
            (8192, "exceeded"),
        ] {
            let machine = sized('m', total.div_ceil(2));
            let user = sized('u', total / 2);
            let local = path_file(&[
                (EnvScope::Machine, &machine, Existence::Yes),
                (EnvScope::User, &user, Existence::Yes),
            ]);
            let target = local.clone();
            let diff = diff_of(&local, &target, &fake_fs(&[]));
            let rebuilt = rebuild(&local, &target, &diff, &nothing());
            assert_eq!(
                rebuilt.budget.raw_machine_chars + rebuilt.budget.raw_user_chars,
                total,
                "预算算的是**注册表原文**"
            );
            assert_eq!(rebuilt.budget.level, expected, "总长 {total} 的档位");
            assert_eq!(rebuilt.budget_before.level, expected);
            assert_eq!(rebuilt.budget.cliff, 8191);
            assert_eq!(
                rebuilt.budget.remaining,
                8191usize.saturating_sub(total),
                "超出崖顶时 remaining 归零、不回绕"
            );
        }
    }

    // ── 6. 用户名重写 ────────────────────────────────────────────────

    #[test]
    fn username_rewrites_are_reported_per_row_and_apply_only_with_fix() {
        let local = path_file(&[
            (EnvScope::Machine, r"C:\Users\old\bin", Existence::Yes),
            (EnvScope::User, r"C:\Users\old\tools", Existence::Yes),
        ]);
        let target = local.clone();
        let diff = diff(&local, &target, &fake_fs(&[]), &options(Some("new")));
        assert_eq!(diff.counts.fix, 2);
        assert_eq!(diff.rewrites.len(), 2, "逐条报告");
        let pairs: Vec<(&str, &str)> = diff
            .rewrites
            .iter()
            .map(|rewrite| (rewrite.from.as_str(), rewrite.to.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                (r"C:\Users\old\bin", r"C:\Users\new\bin"),
                (r"C:\Users\old\tools", r"C:\Users\new\tools"),
            ]
        );
        for row in &diff.rows {
            assert_eq!(row.reason, DiffReason::UsernameHardcoded);
            assert!(row.rewrite_to.is_some());
        }
        assert_eq!(diff.rows[0].id, "machine:0");
        assert_eq!(diff.rows[1].id, "user:0");

        // 选中 fix → 重写；没选中 → 一个字节都不动。
        let fixed = rebuild(&local, &target, &diff, &selection(&[DiffClass::Fix], &[]));
        let machine = fixed
            .scopes
            .iter()
            .find(|scope| scope.scope == EnvScope::Machine)
            .expect("机器级也该出现");
        assert_eq!(machine.after_entries, vec![r"C:\Users\new\bin".to_owned()]);
        assert!(machine.requires_elevation, "机器级有改动 → 要提权");
        let user = fixed
            .scopes
            .iter()
            .find(|scope| scope.scope == EnvScope::User)
            .expect("用户级");
        assert_eq!(user.after_entries, vec![r"C:\Users\new\tools".to_owned()]);
        assert!(!user.requires_elevation);

        let untouched = rebuild(&local, &target, &diff, &nothing());
        assert_eq!(
            untouched.scopes[1].before_entries, untouched.scopes[1].after_entries,
            "没选中 fix → 连值都不动"
        );
    }

    #[test]
    fn the_username_segment_is_replaced_and_quotes_are_kept() {
        let local = path_file(&[(EnvScope::User, r#""C:\Users\old\My Tools""#, Existence::Yes)]);
        let target = local.clone();
        let diff = diff(&local, &target, &fake_fs(&[]), &options(Some("new")));
        assert_eq!(diff.rewrites.len(), 1);
        let rebuilt = rebuild(&local, &target, &diff, &selection(&[DiffClass::Fix], &[]));
        assert_eq!(
            rebuilt.scopes[0].after_entries,
            vec![r#""C:\Users\new\My Tools""#.to_owned()],
            "只换 `\\Users\\` 后那一段，引号原样保留"
        );
    }

    #[test]
    fn a_current_username_that_matches_is_not_a_rewrite() {
        let local = path_file(&[(EnvScope::User, r"C:\Users\me\bin", Existence::Yes)]);
        let target = local.clone();
        let diff = diff(&local, &target, &fake_fs(&[]), &options(Some("me")));
        assert_eq!(diff.counts.keep, 1, "没有旧名可换 → 它是一条健康的行");
        assert_eq!(diff.counts.fix, 0);
        assert_eq!(diff.rows[0].reason, DiffReason::Identical);
        assert_eq!(diff.rows[0].also, vec![DiffReason::UsernameHardcoded]);
        assert!(diff.rows[0].rewrite_to.is_none(), "用户名已经对 → 不重写");
        assert!(diff.rewrites.is_empty());
        let rebuilt = rebuild(&local, &target, &diff, &selection(&[DiffClass::Fix], &[]));
        assert_eq!(
            rebuilt.scopes[0].after_entries, rebuilt.scopes[0].before_entries,
            "选中 fix 但没什么可改 → 原样"
        );
        assert!(rebuilt.scopes[0].applied.is_empty());
        assert_eq!(rebuilt.scopes[0].untouched, 1);
    }

    #[test]
    fn without_a_current_username_nothing_is_rewritten() {
        let local = path_file(&[(EnvScope::User, r"C:\Users\old\bin", Existence::Yes)]);
        let target = local.clone();
        let diff = diff(&local, &target, &fake_fs(&[]), &options(None));
        assert_eq!(diff.counts.keep, 1, "不知道当前用户名 → 没有旧名可换");
        assert_eq!(diff.rows[0].also, vec![DiffReason::UsernameHardcoded]);
        assert!(diff.rows[0].rewrite_to.is_none());
        assert!(diff.rewrites.is_empty());
    }

    #[test]
    fn a_target_row_with_an_old_username_is_inserted_rewritten() {
        // Lead 的澄清：`add` 的目标值带的是**旧机器**的用户名，插进去就是一条在本机
        // 永远不生效的路径 —— 所以重写跟着 `add` 生效，不额外要求选中 `fix`。
        let local = path_file(&[(EnvScope::User, r"C:\keep", Existence::Yes)]);
        let target = path_file(&[
            (EnvScope::User, r"C:\keep", Existence::Yes),
            (EnvScope::User, r"C:\Users\old\bin", Existence::Yes),
        ]);
        let diff = diff(&local, &target, &fake_fs(&[]), &options(Some("new")));
        assert_eq!(diff.counts.add, 1);
        let added = diff
            .rows
            .iter()
            .find(|row| row.class == DiffClass::Add)
            .expect("应有一条 add");
        assert_eq!(added.rewrite_to.as_deref(), Some(r"C:\Users\new\bin"));
        assert_eq!(diff.rewrites.len(), 1, "重写要逐条报告，哪怕类不是 fix");
        assert_eq!(diff.rewrites[0].id, added.id);

        let rebuilt = rebuild(&local, &target, &diff, &selection(&[DiffClass::Add], &[]));
        assert_eq!(
            rebuilt.scopes[0].after_entries,
            vec![r"C:\keep".to_owned(), r"C:\Users\new\bin".to_owned()]
        );
        assert_eq!(rebuilt.scopes[0].applied[0].value, r"C:\Users\new\bin");
    }

    #[test]
    fn a_portable_row_with_variables_is_never_treated_as_hardcoded() {
        // `%USERPROFILE%\bin` 是可搬运的写法：`has_username` 是 capture 算出来的事实，
        // 本模块照抄，不重算、也不重写。
        let local = path_file(&[(EnvScope::User, r"%USERPROFILE%\bin", Existence::Yes)]);
        let target = local.clone();
        let diff = diff(&local, &target, &fake_fs(&[]), &options(Some("new")));
        assert_eq!(diff.counts.keep, 1);
        assert!(diff.rewrites.is_empty());
    }

    #[test]
    fn a_rewritable_username_row_is_a_fix_that_really_rewrites() {
        // (a) 有旧名可换 → 类就是 `fix`/`username-hardcoded`，而且**选中 `fix` 之后
        // `after_entries` 里出现的是重写后的值**（不是被丢掉）。换机之后
        // `C:\Users\旧名\…` 必然同时"不存在"，所以这一条必须压过 `dangling` ——
        // 否则重写功能恰好在其唯一有用的场景里不生效。
        let local = path_file(&[(EnvScope::User, r"C:\Users\old\bin", Existence::No)]);
        let target = path_file(&[(EnvScope::User, r"C:\Users\old\bin", Existence::Yes)]);
        let diff = diff(&local, &target, &fake_fs(&[]), &options(Some("new")));
        assert_eq!(diff.rows[0].class, DiffClass::Fix);
        assert_eq!(diff.rows[0].reason, DiffReason::UsernameHardcoded);
        assert_eq!(
            diff.rows[0].also,
            vec![DiffReason::Dangling],
            "失效的事实也看得见"
        );
        assert_eq!(
            diff.rows[0].rewrite_to.as_deref(),
            Some(r"C:\Users\new\bin")
        );
        assert_eq!(diff.rewrites.len(), 1);

        let rebuilt = rebuild(&local, &target, &diff, &selection(&[DiffClass::Fix], &[]));
        assert_eq!(
            rebuilt.scopes[0].after_entries,
            vec![r"C:\Users\new\bin".to_owned()],
            "选中 fix 是**重写**，不是丢掉"
        );
        assert_eq!(rebuilt.scopes[0].applied[0].class, DiffClass::Fix);
        assert_eq!(rebuilt.scopes[0].applied[0].value, r"C:\Users\new\bin");
    }

    #[test]
    fn a_username_row_with_nothing_to_rewrite_falls_through_to_dangling() {
        // (b) 硬编码了用户名、但名字**就是**当前用户名 → 没有旧名可换，所以这一行
        // 不能归 `username-hardcoded`（那样选中 `fix` 之后它既不会被重写、也不会被丢掉，
        // 用户会以为修好了）。它按其余性质归 `dangling`，事实留在 `also` 里，
        // 选中 `fix` 时**真的被丢掉**。
        let local = path_file(&[(EnvScope::User, r"C:\Users\Muelsyse\bin", Existence::No)]);
        let target = path_file(&[(EnvScope::User, r"C:\Users\Muelsyse\bin", Existence::Yes)]);
        let diff = diff(&local, &target, &fake_fs(&[]), &options(Some("Muelsyse")));
        assert_eq!(diff.rows[0].class, DiffClass::Fix);
        assert_eq!(diff.rows[0].reason, DiffReason::Dangling);
        assert_eq!(diff.rows[0].also, vec![DiffReason::UsernameHardcoded]);
        assert!(diff.rows[0].rewrite_to.is_none(), "没有旧名 → 不重写");
        assert!(diff.rewrites.is_empty());

        let rebuilt = rebuild(&local, &target, &diff, &selection(&[DiffClass::Fix], &[]));
        assert!(rebuilt.scopes[0].after_entries.is_empty(), "被丢掉");
        assert_eq!(rebuilt.scopes[0].untouched, 0);
    }

    #[test]
    fn a_healthy_username_row_is_keep_with_the_fact_in_also() {
        // (c) 存在且健康、名字就是当前用户名 → `keep`，`also` 里照样看得见这个事实
        // （可移植性是证据，不是动作）。
        let local = path_file(&[(EnvScope::User, r"C:\Users\Muelsyse\bin", Existence::Yes)]);
        let target = local.clone();
        let healthy = diff(&local, &target, &fake_fs(&[]), &options(Some("Muelsyse")));
        assert_eq!(healthy.rows[0].class, DiffClass::Keep);
        assert_eq!(healthy.rows[0].reason, DiffReason::Identical);
        assert_eq!(healthy.rows[0].also, vec![DiffReason::UsernameHardcoded]);
        assert_eq!(healthy.counts.keep, 1);
        assert_eq!(healthy.counts.fix, 0);

        // 没有当前用户名时同理：没有旧名可换 ≠ 没有这个事实。
        let blind = diff(&local, &target, &fake_fs(&[]), &options(None));
        assert_eq!(blind.rows[0].class, DiffClass::Keep);
        assert_eq!(blind.rows[0].also, vec![DiffReason::UsernameHardcoded]);
    }

    #[test]
    fn duplicate_beats_username_because_a_redundant_copy_should_be_dropped() {
        let local = path_file(&[
            (EnvScope::User, r"C:\Users\old\bin", Existence::Yes),
            (EnvScope::User, r"C:\Users\old\bin", Existence::Yes),
        ]);
        let target = local.clone();
        let diff = diff(&local, &target, &fake_fs(&[]), &options(Some("new")));
        assert_eq!(diff.rows[1].reason, DiffReason::Duplicate);
        assert!(
            diff.rows[1].also.contains(&DiffReason::UsernameHardcoded),
            "另一条理由要看得见：{:?}",
            diff.rows[1].also
        );
        assert!(diff.rows[1].rewrite_to.is_some(), "重写依然逐条报出来");
    }

    #[test]
    fn a_whitespace_only_difference_is_not_case_only() {
        // 验收脚本比较的是 `raw.Trim()`：` C:\a ` 与 `C:\a` 的差别只是首尾空白，
        // 而用户在 PATH 里看到的写法一样 —— 归 `keep`。归 `case-only` 会让
        // `--only case-only` 去"修"一个看不见的东西。
        let local = path_file(&[(EnvScope::User, " C:\\a ", Existence::Yes)]);
        let target = path_file(&[(EnvScope::User, r"C:\a", Existence::Yes)]);
        let diff = diff_of(&local, &target, &fake_fs(&[]));
        assert_eq!(diff.rows[0].class, DiffClass::Keep);
        assert_eq!(diff.rows[0].reason, DiffReason::Identical);
    }

    #[test]
    fn the_real_machines_three_way_overlap_keeps_every_reason_visible() {
        // 真机上的重叠：`C:\Software\tool` 出现三次（第三次带尾斜杠），三条都失效。
        // 主理由只报一条（与验收脚本同序：duplicate 压过 dangling），其余全进 `also`。
        let local = path_file(&[
            (EnvScope::Machine, r"C:\Software\tool", Existence::No),
            (EnvScope::Machine, r"C:\Software\tool", Existence::No),
            (EnvScope::User, r"C:\Software\tool\", Existence::No),
        ]);
        let target = local.clone();
        let diff = diff_of(&local, &target, &fake_fs(&[]));
        assert_eq!(diff.rows[0].reason, DiffReason::Dangling);
        assert_eq!(diff.rows[1].reason, DiffReason::Duplicate);
        assert_eq!(diff.rows[2].reason, DiffReason::Duplicate);
        assert_eq!(diff.rows[1].duplicate_of.as_deref(), Some("machine:0"));
        assert_eq!(diff.rows[2].duplicate_of.as_deref(), Some("machine:0"));
        assert!(diff.rows[1].also.contains(&DiffReason::Dangling));
        assert!(diff.rows[2].also.contains(&DiffReason::Dangling));
    }

    // ── 7. 拼写疑似 ──────────────────────────────────────────────────

    #[test]
    fn a_dangling_row_gets_a_suggestion_from_the_parent_directory() {
        let fs = fake_fs(&[(r"C:\Software", "tools")]);
        let local = path_file(&[(EnvScope::User, r"C:\Software\tool", Existence::No)]);
        let target = local.clone();
        let diff = diff_of(&local, &target, &fs);
        assert_eq!(diff.typos.len(), 1);
        assert_eq!(diff.typos[0].value, r"C:\Software\tool");
        assert_eq!(diff.typos[0].suggestion, r"C:\Software\tools");
        assert_eq!(diff.typos[0].id, "user:0");
        assert_eq!(
            diff.rows[0].suggestion.as_deref(),
            Some(r"C:\Software\tools")
        );
        assert_eq!(diff.rows[0].reason, DiffReason::Dangling, "拼写疑似不改类");
    }

    #[test]
    fn an_unrelated_sibling_directory_is_not_a_suggestion() {
        let fs = fake_fs(&[(r"C:\Software", "zzz")]);
        let local = path_file(&[(EnvScope::User, r"C:\Software\tool", Existence::No)]);
        let diff = diff_of(&local, &local, &fs);
        assert!(diff.typos.is_empty());
        assert!(diff.rows[0].suggestion.is_none());
    }

    #[test]
    fn a_healthy_row_is_never_checked_against_the_disk() {
        // `exists == yes` 的行不问磁盘 —— 假文件系统对未声明的路径答"不存在"，
        // 真要是问了，这条用例会拿到一个 suggestion。
        let fs = fake_fs(&[(r"C:\Software", "tools")]);
        let local = path_file(&[(EnvScope::User, r"C:\Software\tool", Existence::Yes)]);
        let diff = diff_of(&local, &local, &fs);
        assert!(diff.typos.is_empty());
    }

    // ── 8. 选择性应用 ────────────────────────────────────────────────

    #[test]
    fn selecting_only_add_leaves_remove_rows_exactly_where_they_were() {
        let local = path_file(&[
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, r"C:\stale", Existence::Yes),
        ]);
        let target = path_file(&[
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, r"C:\new", Existence::Yes),
        ]);
        let diff = diff_of(&local, &target, &fake_fs(&[]));
        assert_eq!(diff.counts.keep, 1);
        assert_eq!(diff.counts.remove, 1);
        assert_eq!(diff.counts.add, 1);

        let rebuilt = rebuild(&local, &target, &diff, &selection(&[DiffClass::Add], &[]));
        let scope = &rebuilt.scopes[0];
        assert_eq!(scope.before_entries, vec![r"C:\a", r"C:\stale"]);
        assert_eq!(scope.after_entries, vec![r"C:\a", r"C:\stale", r"C:\new"]);
        assert_eq!(
            scope.before_entries.iter().position(|e| e == r"C:\stale"),
            scope.after_entries.iter().position(|e| e == r"C:\stale"),
            "remove 类条目连位置都不动"
        );
        assert_eq!(scope.applied.len(), 1);
        assert_eq!(scope.applied[0].class, DiffClass::Add);
        assert!(
            scope
                .applied
                .iter()
                .all(|row| row.class != DiffClass::Remove),
            "没选中的类不许出现在 applied 里"
        );
        assert_eq!(scope.untouched, 2);
        assert!(!scope.requires_elevation);
    }

    #[test]
    fn selecting_only_remove_drops_remove_rows_and_nothing_else() {
        let local = path_file(&[
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, r"C:\stale", Existence::Yes),
        ]);
        let target = path_file(&[
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, r"C:\new", Existence::Yes),
        ]);
        let diff = diff_of(&local, &target, &fake_fs(&[]));
        let rebuilt = rebuild(
            &local,
            &target,
            &diff,
            &selection(&[DiffClass::Remove], &[]),
        );
        let scope = &rebuilt.scopes[0];
        assert_eq!(
            scope.after_entries,
            vec![r"C:\a"],
            "add 没被选中 → 不插进来"
        );
        assert_eq!(scope.applied.len(), 1);
        assert_eq!(scope.applied[0].class, DiffClass::Remove);
        assert_eq!(scope.applied[0].from_index, Some(1));
        assert_eq!(scope.applied[0].to_index, 1, "丢掉的行没有落点，用原位表示");
        assert_eq!(scope.untouched, 1);
    }

    #[test]
    fn picking_one_row_by_id_selects_exactly_that_row() {
        let local = path_file(&[
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, r"C:\b", Existence::Yes),
        ]);
        let target = path_file(&[(EnvScope::User, r"C:\a", Existence::Yes)]);
        let diff = diff_of(&local, &target, &fake_fs(&[]));
        let picked = selection(&[], &["user:1"]);
        assert!(!picked.is_empty());
        assert!(picked.selects(&diff.rows[1]));
        assert!(!picked.selects(&diff.rows[0]));
        let rebuilt = rebuild(&local, &target, &diff, &picked);
        assert_eq!(rebuilt.scopes[0].after_entries, vec![r"C:\a"]);
        assert_eq!(rebuilt.scopes[0].applied.len(), 1);
        assert_eq!(rebuilt.scopes[0].applied[0].id, "user:1");
    }

    // ── 9. 空选择 ────────────────────────────────────────────────────

    #[test]
    fn an_empty_selection_selects_nothing_at_all() {
        let local = path_file(&[
            (EnvScope::Machine, r"C:\gone", Existence::No),
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, "", Existence::Unknown),
        ]);
        let target = path_file(&[(EnvScope::User, r"C:\b", Existence::Yes)]);
        let diff = diff(&local, &target, &fake_fs(&[]), &options(Some("new")));
        assert!(!diff.rows.is_empty());
        let empty = nothing();
        assert!(empty.is_empty());
        assert!(empty.classes().is_empty());
        assert!(empty.picks().is_empty());
        for row in &diff.rows {
            assert!(!empty.selects(row), "{} 不该被选中", row.id);
        }
        for class in DiffClass::ALL {
            assert!(!empty.selects_class(class));
        }

        let rebuilt = rebuild(&local, &target, &diff, &empty);
        for scope in &rebuilt.scopes {
            assert_eq!(
                scope.before_entries, scope.after_entries,
                "什么都没选 → 一个字节都不动"
            );
            assert!(scope.applied.is_empty());
            assert!(!scope.requires_elevation);
        }
    }

    // ── 10. class == reason.class() ──────────────────────────────────

    #[test]
    fn every_row_obeys_class_equals_reason_class() {
        let fixtures: Vec<(PathFile, PathFile)> = vec![
            (
                path_file(&[
                    (EnvScope::Machine, r"C:\gone", Existence::No),
                    (EnvScope::Machine, r"C:\broken\dupe", Existence::Yes),
                    (EnvScope::Machine, r"C:\broken\dupe", Existence::Yes),
                    (EnvScope::User, "", Existence::Unknown),
                    (EnvScope::User, r"C:\Users\old\bin", Existence::No),
                    (EnvScope::User, r"C:\Tools", Existence::Yes),
                    (EnvScope::User, r"C:\x", Existence::Yes),
                    (EnvScope::User, r"C:\y", Existence::Yes),
                ]),
                path_file(&[
                    (EnvScope::Machine, r"C:\gone", Existence::Yes),
                    (EnvScope::Machine, r"C:\broken\dupe", Existence::Yes),
                    (EnvScope::Machine, "", Existence::Unknown),
                    (EnvScope::User, r"C:\Users\old\bin", Existence::Yes),
                    (EnvScope::User, r"c:\tools", Existence::Yes),
                    (EnvScope::User, r"C:\y", Existence::Yes),
                    (EnvScope::User, r"C:\z", Existence::Yes),
                ]),
            ),
            (
                path_file(&[(EnvScope::User, r"C:\a", Existence::Yes)]),
                path_file(&[
                    (EnvScope::User, r"C:\a", Existence::Yes),
                    (EnvScope::User, r"C:\a", Existence::Yes),
                ]),
            ),
            (
                path_file(&[]),
                path_file(&[(EnvScope::Machine, r"C:\only", Existence::No)]),
            ),
        ];
        for (index, (local, target)) in fixtures.iter().enumerate() {
            let diff = diff(local, target, &fake_fs(&[]), &options(Some("new")));
            assert!(!diff.rows.is_empty(), "夹具 {index} 不该是空的");
            for row in &diff.rows {
                assert_eq!(
                    row.class,
                    row.reason.class(),
                    "夹具 {index} 的 {} 破坏了硬不变量",
                    row.id
                );
                assert_eq!(
                    DiffClass::parse(row.class.as_str()),
                    Some(row.class),
                    "as_str/parse 必须往返"
                );
                assert!(
                    !row.also.contains(&row.reason),
                    "主理由不许出现在 also 里：{:?}",
                    row.also
                );
                let mut sorted = row.also.clone();
                sorted.sort_by_key(|reason| reason.as_str());
                sorted.dedup();
                assert_eq!(row.also, sorted, "also 必须去重且按 slug 字典序");
            }
            let total = diff.counts.keep
                + diff.counts.add
                + diff.counts.remove
                + diff.counts.move_
                + diff.counts.fix
                + diff.counts.case_only;
            assert_eq!(total, diff.rows.len());
        }
    }

    // ── 11. 放置规则边界 ─────────────────────────────────────────────

    #[test]
    fn placement_inserts_at_the_head() {
        let local = path_file(&[(EnvScope::User, r"C:\b", Existence::Yes)]);
        let target = path_file(&[
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, r"C:\b", Existence::Yes),
        ]);
        let diff = diff_of(&local, &target, &fake_fs(&[]));
        let rebuilt = rebuild(&local, &target, &diff, &selection(&[DiffClass::Add], &[]));
        assert_eq!(rebuilt.scopes[0].after_entries, vec![r"C:\a", r"C:\b"]);
        assert_eq!(rebuilt.scopes[0].applied[0].to_index, 0, "插到首位");
    }

    #[test]
    fn placement_appends_when_nothing_in_the_base_comes_after_it() {
        let local = path_file(&[(EnvScope::User, r"C:\a", Existence::Yes)]);
        let target = path_file(&[
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, r"C:\z", Existence::Yes),
        ]);
        let diff = diff_of(&local, &target, &fake_fs(&[]));
        let rebuilt = rebuild(&local, &target, &diff, &selection(&[DiffClass::Add], &[]));
        assert_eq!(rebuilt.scopes[0].after_entries, vec![r"C:\a", r"C:\z"]);
        assert_eq!(rebuilt.scopes[0].applied[0].to_index, 1, "追加到末尾");
    }

    #[test]
    fn placement_falls_back_to_the_end_when_the_anchor_is_not_local() {
        // 目标里 `x` 与 `y` 本机都没有：`x` 的锚点是 `y`（目标里排在它之后），
        // 而 `y` 本机不存在 → 追加末尾。这条同时钉住"逆序处理"：正序会把 y 排到 x 前面。
        let local = path_file(&[(EnvScope::User, r"C:\a", Existence::Yes)]);
        let target = path_file(&[
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, r"C:\x", Existence::Yes),
            (EnvScope::User, r"C:\y", Existence::Yes),
        ]);
        let diff = diff_of(&local, &target, &fake_fs(&[]));
        assert_eq!(diff.counts.add, 2);
        let rebuilt = rebuild(&local, &target, &diff, &selection(&[DiffClass::Add], &[]));
        assert_eq!(
            rebuilt.scopes[0].after_entries,
            vec![r"C:\a", r"C:\x", r"C:\y"],
            "两条 add 必须保持目标里的相对顺序"
        );
    }

    #[test]
    fn move_and_add_share_the_same_placement_rule() {
        // 目标 `[x, b, a]`：`a`、`b` 是 move，`x` 是 add。
        let local = path_file(&[
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, r"C:\b", Existence::Yes),
        ]);
        let target = path_file(&[
            (EnvScope::User, r"C:\x", Existence::Yes),
            (EnvScope::User, r"C:\b", Existence::Yes),
            (EnvScope::User, r"C:\a", Existence::Yes),
        ]);
        let diff = diff_of(&local, &target, &fake_fs(&[]));
        assert_eq!(diff.counts.move_, 2);
        assert_eq!(diff.counts.add, 1);
        let all = Selection::new(DiffClass::ALL.to_vec(), Vec::new());
        let rebuilt = rebuild(&local, &target, &diff, &all);
        assert_eq!(
            rebuilt.scopes[0].after_entries,
            vec![r"C:\x", r"C:\b", r"C:\a"],
            "全都选中时输出恰等于目标顺序"
        );
    }

    #[test]
    fn selecting_everything_reproduces_the_target_order() {
        let local = path_file(&[
            (EnvScope::Machine, r"C:\m1", Existence::Yes),
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, r"C:\b", Existence::Yes),
            (EnvScope::User, r"C:\c", Existence::Yes),
            (EnvScope::User, r"C:\d", Existence::Yes),
        ]);
        let target = path_file(&[
            (EnvScope::Machine, r"C:\m1", Existence::Yes),
            (EnvScope::User, r"C:\d", Existence::Yes),
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, r"C:\b", Existence::Yes),
            (EnvScope::User, r"C:\c", Existence::Yes),
        ]);
        let diff = diff_of(&local, &target, &fake_fs(&[]));
        let all = Selection::new(DiffClass::ALL.to_vec(), Vec::new());
        let rebuilt = rebuild(&local, &target, &diff, &all);
        let user = rebuilt
            .scopes
            .iter()
            .find(|scope| scope.scope == EnvScope::User)
            .expect("用户级");
        assert_eq!(user.after_entries, vec![r"C:\d", r"C:\a", r"C:\b", r"C:\c"]);
    }

    #[test]
    fn a_move_that_lands_elsewhere_is_recorded_with_both_indices() {
        let local = path_file(&[
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, r"C:\b", Existence::Yes),
        ]);
        let target = path_file(&[
            (EnvScope::User, r"C:\b", Existence::Yes),
            (EnvScope::User, r"C:\a", Existence::Yes),
        ]);
        let diff = diff_of(&local, &target, &fake_fs(&[]));
        let all = Selection::new(DiffClass::ALL.to_vec(), Vec::new());
        let rebuilt = rebuild(&local, &target, &diff, &all);
        assert_eq!(rebuilt.scopes[0].after_entries, vec![r"C:\b", r"C:\a"]);
        assert_eq!(rebuilt.scopes[0].applied.len(), 2);
        assert_eq!(rebuilt.scopes[0].applied[0].id, "user:1");
        assert_eq!(rebuilt.scopes[0].applied[0].from_index, Some(1));
        assert_eq!(rebuilt.scopes[0].applied[0].to_index, 0);
        assert_eq!(rebuilt.scopes[0].applied[1].id, "user:0");
        assert_eq!(rebuilt.scopes[0].applied[1].from_index, Some(0));
        assert_eq!(rebuilt.scopes[0].applied[1].to_index, 1);
        assert_eq!(rebuilt.scopes[0].untouched, 0);
    }

    // ── 12. 纯函数：before_raw 逐字相同、两次调用相同 ────────────────

    #[test]
    fn rebuild_is_pure_and_before_raw_is_the_snapshot_verbatim() {
        let raw = r";C:\a;;C:\Users\b\c;";
        let local = path_file(&[
            (EnvScope::User, "", Existence::Unknown),
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, "", Existence::Unknown),
            (EnvScope::User, r"C:\Users\b\c", Existence::Yes),
            (EnvScope::User, "", Existence::Unknown),
        ]);
        let target = path_file(&[
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, r"C:\Users\b\c", Existence::Yes),
        ]);
        let diff = diff_of(&local, &target, &fake_fs(&[]));
        let selection = selection(&[DiffClass::Fix], &[]);

        let first = rebuild(&local, &target, &diff, &selection);
        let second = rebuild(&local, &target, &diff, &selection);
        assert_eq!(first, second, "重建必须是纯函数");
        assert_eq!(
            first.scopes[0].before_raw, raw,
            "before_raw 必须与传入的快照逐字相同"
        );
        assert_eq!(
            first.scopes[0].before_entries,
            vec!["", r"C:\a", "", r"C:\Users\b\c", ""]
        );
        assert_eq!(
            first.scopes[0].after_raw, r"C:\a;C:\Users\b\c",
            "三个空条目被丢掉了；用户名没给当前值 → 不重写"
        );
        assert_eq!(first.scopes[0].applied.len(), 3);
        assert!(
            first.scopes[0]
                .applied
                .iter()
                .all(|row| row.class == DiffClass::Fix)
        );
        assert_eq!(first.scopes[0].untouched, 2);

        // `local` / `target` 一个字都没被改。
        assert_eq!(local.entry[1].raw, r"C:\a");
        assert_eq!(local.entry.len(), 5);
        assert_eq!(local.entry[0].raw, "");
    }

    #[test]
    fn diff_is_pure_and_deterministic() {
        let local = path_file(&[
            (EnvScope::Machine, r"C:\Users\old\bin", Existence::No),
            (EnvScope::User, r"C:\Tools", Existence::Yes),
        ]);
        let target = path_file(&[
            (EnvScope::Machine, r"C:\Users\old\bin", Existence::Yes),
            (EnvScope::User, r"c:\tools", Existence::Yes),
        ]);
        let first = diff(&local, &target, &fake_fs(&[]), &options(Some("new")));
        let second = diff(&local, &target, &fake_fs(&[]), &options(Some("new")));
        assert_eq!(first, second);
        let json_first = serde_json::to_string(&first).expect("序列化");
        let json_second = serde_json::to_string(&second).expect("序列化");
        assert_eq!(json_first, json_second, "--json 必须逐字节稳定");
    }

    // ── 13. load_snapshot ────────────────────────────────────────────

    #[test]
    fn load_snapshot_reports_io_and_toml_errors_separately() {
        let temp = tuoen_platform::test_support::TempDir::new("pathdiff-snapshot");
        let missing = temp.join("nope.toml");
        let error = load_snapshot(&missing).expect_err("读不到");
        assert_eq!(error.code(), "path-snapshot-io");

        let bad = temp.write("bad.toml", b"schema_version = \"not a number\"\n");
        let error = load_snapshot(&bad).expect_err("不是合法的 PathFile");
        assert_eq!(error.code(), "path-snapshot-toml");

        let broken = temp.write("broken.toml", b"this is not = = toml\n");
        assert_eq!(
            load_snapshot(&broken).expect_err("坏 TOML").code(),
            "path-snapshot-toml"
        );

        let original = path_file(&[
            (EnvScope::Machine, r"C:\m", Existence::Yes),
            (EnvScope::User, r"C:\u", Existence::No),
        ]);
        let text = toml::to_string(&original).expect("PathFile 必须能写成 TOML");
        let good = temp.write("path.toml", text.as_bytes());
        assert_eq!(
            load_snapshot(&good).expect("读得回来"),
            original,
            "PathFile 必须能读回来"
        );
    }

    // ── 14. 目标侧的重复不许被插进来（Lead 的输出不变量） ─────────────

    #[test]
    fn a_target_duplicate_is_reported_as_add_but_never_inserted() {
        let local = path_file(&[(EnvScope::User, r"C:\a", Existence::Yes)]);
        let target = path_file(&[
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, r"C:\a", Existence::Yes),
        ]);
        let diff = diff_of(&local, &target, &fake_fs(&[]));
        let added = diff
            .rows
            .iter()
            .find(|row| row.class == DiffClass::Add)
            .expect("目标侧多出来的那条是 add");
        assert_eq!(added.id, "user:+1");
        assert!(
            !added.also.contains(&DiffReason::Duplicate),
            "目标侧的 dup_index 只是行事实，不产生理由：{:?}",
            added.also
        );
        assert_eq!(
            added.target.as_ref().expect("目标侧").dup_index,
            1,
            "但事实要照抄进 SideRow，让用户看得见"
        );

        let rebuilt = rebuild(&local, &target, &diff, &selection(&[DiffClass::Add], &[]));
        assert_eq!(
            rebuilt.scopes[0].after_entries,
            vec![r"C:\a"],
            "输出里不许出现折叠后重复的条目"
        );
        let skipped = rebuilt.scopes[0]
            .applied
            .iter()
            .find(|row| row.class == DiffClass::Fix)
            .expect("被跳过的插入要记一条");
        assert_eq!(skipped.value, r"C:\a");
        assert_eq!(skipped.to_index, 0, "落点 = 已存在那条的下标");
        assert_eq!(skipped.from_index, None);
    }

    #[test]
    fn a_second_batch_of_adds_cannot_smuggle_a_duplicate_in() {
        let local = path_file(&[(EnvScope::User, r"C:\a", Existence::Yes)]);
        let target = path_file(&[
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, r"C:\b", Existence::Yes),
        ]);
        let diff = diff_of(&local, &target, &fake_fs(&[]));
        let rebuilt = rebuild(&local, &target, &diff, &selection(&[DiffClass::Add], &[]));
        assert_eq!(rebuilt.scopes[0].after_entries, vec![r"C:\a", r"C:\b"]);
        assert_eq!(rebuilt.scopes[0].applied.len(), 2);
    }

    // ── 决策 126：只看 entry、只看两个注册表作用域 ───────────────────

    #[test]
    fn process_only_rows_and_the_effective_view_are_never_compared() {
        let mut local = path_file(&[(EnvScope::User, r"C:\a", Existence::Yes)]);
        local.entry.push(row(
            EnvScope::ProcessOnly,
            0,
            r"C:\injected",
            Existence::Yes,
        ));
        local.effective = vec![EntryRefRow {
            scope: EnvScope::ProcessOnly,
            index: 0,
        }];
        let target = path_file(&[(EnvScope::User, r"C:\a", Existence::Yes)]);
        let diff = diff_of(&local, &target, &fake_fs(&[]));
        assert_eq!(diff.rows.len(), 1, "进程注入项不参与比较");
        assert_eq!(diff.counts.keep, 1);
        assert_eq!(
            diff.counts.add, 0,
            "拿 effective 做 diff 会把注入项当成 add/remove"
        );
        assert_eq!(diff.counts.remove, 0);
    }

    // ── SideRow 的事实照抄 ───────────────────────────────────────────

    #[test]
    fn side_rows_carry_the_snapshot_facts_verbatim() {
        let local = path_file(&[
            (EnvScope::User, r"C:\Tools\bin\", Existence::Yes),
            (EnvScope::User, r"C:\Users\old\x", Existence::Yes),
        ]);
        let target = path_file(&[
            (EnvScope::User, r"C:\Tools\bin", Existence::Yes),
            (EnvScope::User, r"C:\Users\old\x", Existence::Yes),
        ]);
        let diff = diff(&local, &target, &fake_fs(&[]), &options(Some("new")));
        let side = diff.rows[0].local.as_ref().expect("本机侧");
        assert_eq!(side.scope, EnvScope::User);
        assert_eq!(side.index, 0);
        assert_eq!(side.raw, r"C:\Tools\bin\", "raw 保持原文");
        assert_eq!(
            side.value, r"C:\Tools\bin",
            "value 去掉多余的结尾反斜杠（normalize_entry）"
        );
        assert_eq!(side.owner, "unknown");
        assert_eq!(side.exists, Existence::Yes);
        assert_eq!(side.reparse, ReparseKind::None);
        assert!(!side.has_username);
        assert_eq!(side.dup_index, 0);
        assert!(!side.empty);
        assert_eq!(side.id(), "user:0");
        assert_eq!(
            diff.rows[0].reason,
            DiffReason::Identical,
            "`C:\\Tools\\bin\\` 与 `C:\\Tools\\bin` 是同一个目录"
        );

        let target_side = diff.rows[1].target.as_ref().expect("目标侧");
        assert!(target_side.has_username, "事实来自捕获，不是我算的");
        assert_eq!(diff.rows[1].rewrite_to.as_deref(), Some(r"C:\Users\new\x"));
        assert_eq!(diff.rewrites[0].from, r"C:\Users\old\x");
    }

    // ── --json 的键就是冻结契约 ─────────────────────────────────────

    #[test]
    fn json_keys_are_the_frozen_contract() {
        for class in DiffClass::ALL {
            assert_eq!(
                serde_json::to_value(class).expect("序列化"),
                serde_json::Value::String(class.as_str().to_owned()),
                "枚举 slug 必须与 as_str 一致"
            );
        }
        for reason in [
            DiffReason::Identical,
            DiffReason::OnlyInTarget,
            DiffReason::OnlyInLocal,
            DiffReason::PositionDiffers,
            DiffReason::CaseDiffers,
            DiffReason::EmptySegment,
            DiffReason::Duplicate,
            DiffReason::Dangling,
            DiffReason::UsernameHardcoded,
        ] {
            assert_eq!(
                serde_json::to_value(reason).expect("序列化"),
                serde_json::Value::String(reason.as_str().to_owned())
            );
        }

        let local = path_file(&[
            (EnvScope::Machine, r"C:\Users\old\bin", Existence::No),
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, r"C:\a", Existence::Yes),
        ]);
        let target = path_file(&[
            (EnvScope::Machine, r"C:\Users\old\bin", Existence::Yes),
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, r"C:\b", Existence::Yes),
        ]);
        let diff = diff(&local, &target, &fake_fs(&[]), &options(Some("new")));
        let json = serde_json::to_value(&diff).expect("序列化");

        let counts = json["counts"].as_object().expect("counts 是对象");
        assert!(counts.contains_key("move"), "`move_` 序列化成 `move`");
        assert!(counts.contains_key("caseOnly"), "camelCase");
        assert!(!counts.contains_key("move_"));
        assert!(!counts.contains_key("case_only"));

        let rows = json["rows"].as_array().expect("rows 是数组");
        assert!(
            rows.iter().any(|row| row.get("duplicateOf").is_some()),
            "重复行要给出 duplicateOf"
        );
        assert!(
            rows.iter().any(|row| row.get("rewriteTo").is_some()),
            "用户名行要给出 rewriteTo"
        );
        let plain = rows
            .iter()
            .find(|row| row["class"] == "keep")
            .expect("有一条 keep");
        assert!(
            plain.get("rewriteTo").is_none(),
            "`None` 不进 JSON：{plain:?}"
        );
        assert!(
            plain.get("duplicateOf").is_none(),
            "`None` 不进 JSON：{plain:?}"
        );
        assert!(plain.get("also").is_some(), "also 永远在（可能是空数组）");

        let rewrite = &json["rewrites"][0];
        assert!(rewrite.get("id").is_some());
        assert!(rewrite.get("from").is_some());
        assert!(rewrite.get("to").is_some());

        let rebuilt = rebuild(&local, &target, &diff, &selection(&[DiffClass::Fix], &[]));
        let json = serde_json::to_value(&rebuilt).expect("序列化");
        assert!(json.get("budgetBefore").is_some());
        assert!(json.get("budget").is_some());
        assert!(
            json.get("noOpClasses").is_some(),
            "选了但没做的类要能被 CLI 读到：{json:?}"
        );
        let scope = &json["scopes"][0];
        for key in [
            "scope",
            "beforeRaw",
            "afterRaw",
            "beforeEntries",
            "afterEntries",
            "beforeType",
            "applied",
            "untouched",
            "requiresElevation",
        ] {
            assert!(scope.get(key).is_some(), "缺少键 {key}：{scope:?}");
        }
        let applied = &scope["applied"][0];
        assert!(applied.get("id").is_some());
        assert!(applied.get("class").is_some());
        assert!(applied.get("value").is_some());
        assert!(applied.get("toIndex").is_some());
    }

    // ── Selection 的行为 ─────────────────────────────────────────────

    #[test]
    fn selection_is_class_or_pick() {
        let local = path_file(&[
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, r"C:\b", Existence::Yes),
        ]);
        let target = path_file(&[(EnvScope::User, r"C:\a", Existence::Yes)]);
        let diff = diff_of(&local, &target, &fake_fs(&[]));
        let by_class = selection(&[DiffClass::Remove], &[]);
        assert!(by_class.selects_class(DiffClass::Remove));
        assert!(
            !by_class.selects(&diff.rows[0]),
            "第一条是 keep，没被任何 `--only` 选中"
        );
        assert!(by_class.selects(&diff.rows[1]), "第二条是 remove");
        let by_id = selection(&[], &["user:0"]);
        assert!(by_id.selects(&diff.rows[0]));
        assert!(!by_id.selects(&diff.rows[1]));
        assert!(by_id.classes().is_empty());
        assert_eq!(by_id.picks().to_vec(), vec!["user:0".to_owned()]);
    }

    // ── 选了但什么都没做的类（给 CLI 打印用） ────────────────────────

    #[test]
    fn only_case_only_is_reported_as_a_no_op() {
        // 决策 128：改大小写零收益、纯风险 —— 所以 `case-only` 永远不做任何改写。
        // 但"用户敲了 `--only case-only` 却什么都没发生"必须**说出来**，不能静默。
        let local = path_file(&[(EnvScope::User, r"C:\Tools", Existence::Yes)]);
        let target = path_file(&[(EnvScope::User, r"C:\tools", Existence::Yes)]);
        let diff = diff_of(&local, &target, &fake_fs(&[]));
        assert_eq!(diff.rows[0].class, DiffClass::CaseOnly);

        let rebuilt = rebuild(
            &local,
            &target,
            &diff,
            &selection(&[DiffClass::CaseOnly], &[]),
        );
        assert_eq!(rebuilt.no_op_classes, vec![DiffClass::CaseOnly]);
        assert!(rebuilt.scopes[0].applied.is_empty());
        assert_eq!(
            rebuilt.scopes[0].after_entries, rebuilt.scopes[0].before_entries,
            "一个字节都不动"
        );
        assert_eq!(rebuilt.scopes[0].untouched, 1);
    }

    #[test]
    fn a_selected_class_with_nothing_to_do_is_a_no_op() {
        let local = path_file(&[(EnvScope::User, r"C:\a", Existence::Yes)]);
        let target = local.clone();
        let diff = diff_of(&local, &target, &fake_fs(&[]));

        // 选了 `remove`，可本机一条 remove 都没有 → 如实报出来。
        let nothing_selected_here = rebuild(
            &local,
            &target,
            &diff,
            &selection(&[DiffClass::Remove], &[]),
        );
        assert_eq!(nothing_selected_here.no_op_classes, vec![DiffClass::Remove]);

        // 用 `--pick` 点中一条 `keep` → 那一条也不做任何改写，它的类同样报出来。
        let picked = rebuild(&local, &target, &diff, &selection(&[], &["user:0"]));
        assert_eq!(picked.no_op_classes, vec![DiffClass::Keep]);
    }

    #[test]
    fn a_class_that_really_did_something_is_not_a_no_op() {
        let local = path_file(&[(EnvScope::User, r"C:\a", Existence::Yes)]);
        let target = path_file(&[
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, r"C:\b", Existence::Yes),
        ]);
        let diff = diff_of(&local, &target, &fake_fs(&[]));
        let rebuilt = rebuild(&local, &target, &diff, &selection(&[DiffClass::Add], &[]));
        assert_eq!(rebuilt.scopes[0].after_entries, vec![r"C:\a", r"C:\b"]);
        assert!(
            rebuilt.no_op_classes.is_empty(),
            "`add` 真的插进去了一条：{:?}",
            rebuilt.no_op_classes
        );
    }

    #[test]
    fn a_remove_and_a_same_index_add_never_share_an_id() {
        // 本机 `[C:\a]`、目标 `[C:\b]`：`remove` 是 `user:0`，`add` 是 `user:+0`。
        // 没有这个 `+`，`--pick user:0` 会**同时**选中两条 —— 用户想删一条，
        // 结果还顺手加了一条。
        let local = path_file(&[(EnvScope::User, r"C:\a", Existence::Yes)]);
        let target = path_file(&[(EnvScope::User, r"C:\b", Existence::Yes)]);
        let diff = diff_of(&local, &target, &fake_fs(&[]));
        let ids: Vec<&str> = diff.rows.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, vec!["user:0", "user:+0"]);
        let unique: BTreeSet<&str> = ids.iter().copied().collect();
        assert_eq!(unique.len(), ids.len(), "id 必须互不相同：{ids:?}");

        // `--pick` 两种形态都收，而且各自只选中一条。
        let by_plain = selection(&[], &["user:0"]);
        assert!(by_plain.selects(&diff.rows[0]) && !by_plain.selects(&diff.rows[1]));
        let by_plus = selection(&[], &["user:+0"]);
        assert!(by_plus.selects(&diff.rows[1]) && !by_plus.selects(&diff.rows[0]));

        // 重建里那条 `add` 的 `applied.id` 用同一个形状。
        // （`remove` 没被选中 → `C:\a` 留在原位，而它在目标里没有位置、当不了锚点，
        // 于是 `add` 按放置规则追加到末尾 → 落点是 1。）
        let rebuilt = rebuild(&local, &target, &diff, &selection(&[DiffClass::Add], &[]));
        assert_eq!(rebuilt.scopes[0].applied[0].id, "user:+0");
        assert_eq!(rebuilt.scopes[0].after_entries, vec![r"C:\a", r"C:\b"]);
        assert_eq!(rebuilt.scopes[0].applied[0].to_index, 1);
    }

    // ── 两个用户名入口：注册表块 / 进程环境 ──────────────────────────

    #[test]
    fn current_username_prefers_userprofile_then_falls_back_to_username() {
        let block = FakeEnvBlock::user(&[
            ("USERPROFILE", r"C:\Users\Muelsyse"),
            ("USERNAME", "ignored"),
        ]);
        assert_eq!(
            current_username_from_env(&block).as_deref(),
            Some("Muelsyse")
        );

        let trailing = FakeEnvBlock::user(&[("USERPROFILE", r"C:\Users\Muelsyse\")]);
        assert_eq!(
            current_username_from_env(&trailing).as_deref(),
            Some("Muelsyse"),
            "真机上 USERPROFILE 常以反斜杠结尾"
        );

        let only_username = FakeEnvBlock::user(&[("USERNAME", "Muelsyse")]);
        assert_eq!(
            current_username_from_env(&only_username).as_deref(),
            Some("Muelsyse")
        );

        let root_only = FakeEnvBlock::user(&[("USERPROFILE", r"C:\"), ("USERNAME", "")]);
        assert_eq!(current_username_from_env(&root_only), None);

        let empty = FakeEnvBlock::default();
        assert_eq!(current_username_from_env(&empty), None);
    }

    #[test]
    fn the_username_lookup_does_not_read_the_machine_scope() {
        // 口径是**用户级**：进程里/机器级那份可能被调用方改过，不当成"这台机器的用户名"。
        let block = FakeEnvBlock::new(&[(EnvScope::Machine, "USERPROFILE", r"C:\Users\m")]);
        assert_eq!(current_username_from_env(&block), None);
    }

    #[test]
    fn current_username_from_process_reads_only_the_process_environment() {
        // 真机上 `HKCU\Environment` 里没有 `USERPROFILE`/`USERNAME`，
        // 所以**进程环境才是这一票真正的用户名来源**。
        let with_profile = InMemoryEnv::new(vec![
            ("USERPROFILE".to_owned(), r"C:\Users\Muelsyse\".to_owned()),
            ("USERNAME".to_owned(), "ignored".to_owned()),
        ]);
        assert_eq!(
            current_username_from_process(&with_profile).as_deref(),
            Some("Muelsyse"),
            "USERPROFILE 以反斜杠结尾也要取到最后一段"
        );

        let only_username = InMemoryEnv::new(vec![("username".to_owned(), "Muelsyse".to_owned())]);
        assert_eq!(
            current_username_from_process(&only_username).as_deref(),
            Some("Muelsyse"),
            "名字大小写不敏感"
        );

        let neither = InMemoryEnv::new(vec![("Path".to_owned(), r"C:\bin".to_owned())]);
        assert_eq!(current_username_from_process(&neither), None);

        // 分工：`..._from_process` **不读注册表块**，`..._from_env` **不读进程环境**。
        let block = FakeEnvBlock::user(&[("USERPROFILE", r"C:\Users\from-registry")]);
        assert_eq!(
            current_username_from_process(&neither),
            None,
            "注册表块里有值也不算"
        );
        assert_eq!(
            current_username_from_env(&block).as_deref(),
            Some("from-registry")
        );
        assert_eq!(
            current_username_from_env(&FakeEnvBlock::default()),
            None,
            "进程里有值也不算"
        );
    }
}
