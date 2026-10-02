//! `tuoen doctor` 的执行：装配真实依赖 → 跑 `tuoen_core::doctor` → 印出来。
//!
//! # 这一层做什么
//!
//! 装配**六个真实后端**（与 `capture_cmd` / `main.rs` 的 `run_detect` 一模一样 ——
//! "读这台机器"的代码只有那一套，在这里另写一套会让两份事实慢慢漂移），
//! 把 `--json` / `--no-probe` 翻译成 [`DoctorOptions`]，调
//! [`tuoen_core::doctor::run`]，然后把结果交给 [`crate::doctor_view`]。
//!
//! **这一层不读机器、不算判据。** 事实在 `tuoen_core::doctor::facts` 里采，
//! 判断在 `tuoen_core::doctor::checks` 里做 —— 这里只负责"把真的东西装进去"。
//!
//! # 退出码为什么是 0（哪怕发现了 error）
//!
//! `0` = **体检跑完了**。发现了几个问题，是**体检的结论**，不是体检的失败 ——
//! 把"发现了 error"报成退出码非 0，会让每一次"看一眼这台机器怎么了"都看起来像
//! 一次故障，于是 CI 里第一个动作就变成 `|| true`，而判据反而丢了。
//!
//! 脚本要的判据是 `--json` 里的 `counts.error`：一个不用解析人话就能读到的数字。
//! 这也是**不加** `--fail-on` / `--strict` 的理由（`crate::doctor` 的模块文档
//! 里写了完整取舍）：严重度本身就是数据，让消费者自己决定怎么用。
//!
//! `1` 只在**跑不起来**时出现 —— 见 [`report_unwired`]。
//!
//! # 为什么 `probe_versions: false`
//!
//! `doctor` 只关心**结构**：哪个命令解析到了哪个目录、哪条 `PATH` 条目指向不存在
//! 的东西、注册表声称装了什么而盘上没有。这 23 条检查没有一条需要版本号
//! （"`java` 与 `javac` 来自两个不同的安装"看的是**目录**，不是版本 ——
//! `CommandResolution` 的文档里写着为什么：本机两个 Node 的版本号恰好相同，
//! 版本号不是证据）。
//!
//! 而打开 `probe_versions` 意味着**为每一个被检测到的工具启动一个进程**去问版本，
//! 在真机上那是几十次 spawn，每次几百毫秒。花几十秒换一堆没人看的版本号，
//! 还会让"体检"顺带把自己变成了一个会大面积启动第三方程序的命令
//! （`AGENTS.md` 的三条铁律就是两次这类事故换来的）。
//!
//! # 这一层**不写机器**
//!
//! 全文没有一处 `set_value` / `delete_value` / `broadcast_environment_change` /
//! `create_junction` / `fs::write` / `create_dir_all`，`DoctorOptions` 里也没有
//! 任何"修"的入口 —— 票据要求"绝不修改任何东西、不提供 `--fix`"，而这条纪律
//! 的落点不是"小心一点"，是**这段代码里不存在会写的手**。
//! `crates/cli/tests/doctor_contract.rs` 里的用例是它的证据（跑前跑后逐项相同）。

use std::path::Path;

use tuoen_core::doctor::{self, DoctorOptions, DoctorReport};
use tuoen_store::Store;

use crate::doctor::DoctorArgs;
use crate::doctor_view::{DoctorView, UnwiredRootsView, print_human};
use crate::envelope::Envelope;
use crate::{exit, managed};

/// 装配失败的错误码。**稳定的小写 kebab ASCII**，与中文消息分开
/// （消息可以改，判据不能）。
const UNWIRED_ROOTS: &str = "unwired-roots";

// ─────────────────────────────────────────────────────────────────────────────
// 编排
// ─────────────────────────────────────────────────────────────────────────────

/// 跑 `tuoen doctor`。返回退出码。
pub fn run(args: &DoctorArgs) -> i32 {
    let store = Store::at_default_location();
    let store_root = store.root().to_path_buf();
    // shim 目录用 `shim_cmd::shim_dir`（全仓唯一一份定义），不自己拼 ——
    // 第二份定义会漂移，而它漂移的后果是"遮蔽检测看了一个不存在的目录"：
    // 报告里会静默地少掉一整类发现。
    let shim_dir = crate::shim_cmd::shim_dir(&store);

    // 装配的第一道闸：我们自己的两个根必须是**绝对**路径。
    if !store_root.is_absolute() || !shim_dir.is_absolute() {
        return report_unwired(args, &store_root, &shim_dir);
    }

    // 六个真实依赖，与 `capture_cmd` / `run_detect` 一模一样。
    let fs = tuoen_platform::RealFileSystem;
    let registry = tuoen_platform::RealRegistry;
    let env = tuoen_platform::RealEnvBlock::new(registry, fs);
    let process_env = tuoen_platform::RealProcessEnv::new();
    let runner = tuoen_platform::SystemProcessRunner;
    let managed = managed::StoreManagedStore::at_default_location();
    let scan_roots = tuoen_core::detect::engine::default_scan_roots(&process_env);

    let ctx = tuoen_core::detect::DetectContext {
        fs: &fs,
        registry: &registry,
        env: &env,
        process_env: &process_env,
        runner: &runner,
        managed: &managed,
        probe_timeout: tuoen_platform::DEFAULT_PROBE_TIMEOUT,
        // 体检只关心结构，不关心版本 —— 理由写在模块文档里。
        probe_versions: false,
        scan_roots,
    };

    // 两个根都要给：一个是"我们自己的东西在哪"，一个是 `PATH` 里
    // `owner = "tuoen"` 的判据（与 `CaptureOptions::tuoen_roots` 同一份语义）。
    let mut opts = DoctorOptions::new()
        .with_shim_dir(shim_dir.clone())
        .with_tuoen_root(store_root)
        .with_tuoen_root(shim_dir);
    if !args.no_probe {
        opts = opts.probing_prefixes();
    }

    let report: DoctorReport = doctor::run(&ctx, &opts);

    if args.json {
        crate::print_json(&Envelope::ok("doctor", &DoctorView::new(&report)));
    } else {
        print_human(&report, args.no_probe);
    }
    exit::SUCCESS
}

// ─────────────────────────────────────────────────────────────────────────────
// 失败路径
// ─────────────────────────────────────────────────────────────────────────────

/// 装配失败：算不出我们自己的根位置（两个根里有不是绝对路径的）。
///
/// # 为什么这是"跑不起来"，而不是"照跑不误再少报几条"
///
/// `Store::at_default_location()` 在 `%LOCALAPPDATA%` 与 `%USERPROFILE%` 都读不到时
/// 会退到相对路径 `.tuoen\store`（那是 store 层的既定行为，它**不 panic** ——
/// 一个读不到根位置的进程仍然应该能构造出 store）。而对 `doctor` 来说，相对根
/// 会污染两类判断，**而且是静默的**：
///
/// * `path.toml` 的 `owner` 判据（哪些 `PATH` 条目是我们自己的）比较的是 `PATH` 上的
///   绝对条目与我们的相对根 —— 恒不相等，于是我们自己的条目会被报成"别人的"。
/// * `path.shadowed` 比较的是 shim 目录与 `PATH` 顺序上的目录 —— 同样是恒不相等，
///   于是"我们的 shim 被抢了名字"这一类发现会**一条都不报**。
///
/// 两者都会让报告看起来完全正常。这正是本仓库最不能接受的那类错：
/// "一句看起来完全合理的错话，比一次崩溃更难被发现"（`AGENTS.md`）。
/// 所以这里宁可失败 —— 而且失败里要带出**已经确定的事实**（算出来的那两个根），
/// 让读的人一眼看出是环境缺了变量，而不是工具坏了。
///
/// # 为什么是 `partial` 而不是 `err`
///
/// 与 `capture_cmd::report_failure` 同一条理由：把"算出来的根长什么样"丢掉之后，
/// 消费者只剩一句"失败了"。而 [`UnwiredRootsView`] 里没有 `counts` / `findings`
/// —— 体检根本没跑，报三个 0 就是在说"我看过了，什么都没发现"。
fn report_unwired(args: &DoctorArgs, store_root: &Path, shim_dir: &Path) -> i32 {
    let message = format!(
        "算不出 tuoen 自己的根位置：`%LOCALAPPDATA%` 与 `%USERPROFILE%` 都读不到，\
         于是存储根退成了相对路径 `{}`。\n\
         相对的根本分不出「这条 PATH 条目是不是我们自己的」与「我们的 shim 有没有被\
         别的目录抢在前面」—— 那两个判断会静默地变成「不是」与「没有」，\
         而报告看起来完全正常。所以这里宁可失败：请先设好 `%LOCALAPPDATA%` 再跑一次。",
        store_root.display()
    );
    let data = UnwiredRootsView {
        store_root: store_root.display().to_string(),
        shim_dir: shim_dir.display().to_string(),
    };

    if args.json {
        crate::print_json(&Envelope::partial("doctor", UNWIRED_ROOTS, message, &data));
    } else {
        eprintln!("tuoen: {message}");
    }
    exit::RUNTIME_ERROR
}
