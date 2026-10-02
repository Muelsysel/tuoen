//! `tuoen restore` —— 把另一台机器的 `tuoen.d/` 快照**还原**到本机。
//!
//! # 这一层在整个项目里的位置
//!
//! `capture` 把整机状态读成文件（只读），`doctor` 对它做体检（只判断），
//! `restore` 是**唯一**会"照着另一台机器改本机"的东西 —— 也是本项目里后果最重的命令
//! （它会装工具、写环境变量）。所以这一票的重点不是"能还原多少"，而是
//! **在动手之前，那份计划说的是不是真的**。
//!
//! 于是有三条结构性的事实，它们都是**类型**而不是纪律：
//!
//! 1. [`plan`] 是**纯函数**：只读两个内存里的 [`RestoreBundle`]，不碰网络、不碰注册表、
//!    不起进程。写是 CLI 与 platform 层的事（决策 150：默认只出计划，`--apply` 才动手）。
//! 2. **两侧同一形状**（决策 126 的沿用）：目标侧是读进来的快照，本机侧是现场 `capture`
//!    出来的**同一套类型**（在内存里）。两侧不同形状的判据必然漂移。
//! 3. **做不到的事是计划里的数据，不是运行时的意外**：[`ManualAction`] 是公开契约，
//!    它的五个 `code` 是稳定字符串（决策 152），中文散文只进人类输出。
//!
//! # 计划里到底有什么
//!
//! ```text
//! RestorePlan { snapshot, sections: [SectionPlan × 4], manual_actions, summary }
//! ```
//!
//! * `snapshot` —— **用户敲的那个路径原样**（决策 162）。它由 [`RestoreBundle::load`]
//!   在读到文件的那一刻定死（`source`），`plan` 只负责搬过来。**没有"占位符等 CLI 覆盖"
//!   这条路**：占位符是一个合法的时间戳字符串，看起来完全像真的，CLI 忘了覆盖永远不会红。
//! * 四个 section **永远都在**，各带一个 `status`：没被 `--only` 选中的是
//!   `skipped` + `note = "not-selected"`；快照里没有它的是 `skipped` +
//!   `note = "section-not-in-snapshot"`（决策 159/161）。两者是不同的两句话。
//! * `counts` 有**两个口径**（决策 158）：`rows` 是分类行数、`effective` 是**真会写的条数**。
//!   真机实测目标侧重复两条 → `add 2` 但只有 1 条落地；只印一个数用户会以为要加两条。
//! * `manual_actions` —— 五类做不到的事，**每一类都要具体到 slug**。
//!
//! # 五类手动待办的字段约定（公开契约，`--json` 里逐字出现）
//!
//! | code | subject | detail | remediation |
//! |---|---|---|---|
//! | `requires-elevation` | 要写的**环境变量名**（`PATH` / `JAVA_HOME`） | `scope=machine var=<名字>` | `run-as-administrator` |
//! | `credential-reconfigure` | 凭据的**标识**（`env:ARK_API_KEY`，**绝不含材料**） | `<kind>`（`credential-named` / `credential`） | `reconfigure-manually` |
//! | `licence-blocked` | 工具名（`oracle-jdk`） | 具体原因 slug（`oracle-jdk-redistribution-not-permitted`） | `install-manually` |
//! | `third-party-manager` | 管理器名（`nvm4w`），**按管理器去重** | 管理器名 | `use-the-manager` |
//! | `unsupported` | 工具名 | `not-reproducible` | `not-supported-in-this-version` |
//!
//! `subject` / `detail` / `remediation` **全是纯 ASCII**（决策 152）—— 而且不是靠"我们记得
//! 别抄 `evidence`"，是靠 [`ascii_token`] 把每个进计划的字符串都压成 ASCII token。
//! `ToolRow.evidence` 是中文散文、`ToolRow.path` 可能是 `<无 InstallLocation，卸载键 {GUID}>`
//! 这种占位符，所以它们**根本没有进计划的入口**：这个模块只读 `name` / `version` /
//! `manager` / `reproducible` 四个字段。
//!
//! # 幂等
//!
//! `plan(target, target, opts)`（两侧是同一台机器）在默认选择下**必须没有 `would-change`**：
//! 本机自身的健康问题（本机 `PATH` 真有 24 条）归 `fix`，默认**不选中、只报告**
//! （决策 151 —— 那是本机自己的病，不是"与快照的差异"）。
//!
//! # 测试 seam
//!
//! [`test_support`] 提供在内存里造两侧 bundle 的夹具。**用例不碰真实的注册表、`PATH`、
//! 用户安装目录或 `%APPDATA%\tuoen`**；需要真文件的只有 [`RestoreBundle::load`]，
//! 它只用 `%TEMP%` 下自建自删的临时目录。

mod bundle;
mod manual;
mod plan;
mod sections;

pub mod test_support;

pub use bundle::{RestoreBundle, RestoreError, SKIPPED_FILE, SNAPSHOT_FILES};
pub use manual::{
    ManualAction, ManualActionCode, REMEDIATION_INSTALL_MANUALLY,
    REMEDIATION_NOT_SUPPORTED_IN_THIS_VERSION, REMEDIATION_RECONFIGURE_MANUALLY,
    REMEDIATION_RUN_AS_ADMINISTRATOR, REMEDIATION_USE_THE_MANAGER, ascii_token,
};
pub use plan::{
    PlannedAction, RestoreOptions, RestorePlan, RestoreSummary, SectionCounts, SectionPlan, plan,
};
pub use sections::{
    NOTE_FIX_NOT_SELECTED, NOTE_MACHINE_SCOPE_REQUIRES_ELEVATION, NOTE_NEEDS_NETWORK,
    NOTE_NOT_SELECTED, NOTE_REPORT_ONLY, NOTE_SECTION_NOT_IN_SNAPSHOT, SectionId, SectionStatus,
};
