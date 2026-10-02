//! 六个真实后端的**唯一**装配处。
//!
//! # 为什么需要这个文件（而不是各命令各装一套）
//!
//! `detect` / `capture` / `doctor` / `path diff` 都在回答同一个问题："这台机器现在是什么样"。
//! 检测引擎（`tuoen_core::detect`）是**唯一**一处"读这台机器"的代码，而它的六个后端
//! ——文件系统、注册表、持久环境块、进程环境、进程运行器、我们自己的存储——
//! 必须由调用方装配。装配的第二份副本会让两份事实**慢慢漂移**，而漂移的表现是
//! "同一个 `PATH`，`capture` 与 `path diff` 读到两样东西"：没有任何断言会红，
//! 报告看起来完全正常。这正是本仓库最不能接受的那类错
//! （`AGENTS.md`：「一句看起来完全合理的错话，比一次崩溃更难被发现」）。
//!
//! 所以这里做两件事：把六个后端装成**一个**值，再把它投影成
//! [`DetectContext`]（引擎要的形状）。命令之间的差别只剩一个显式的开关
//! （`probe_versions`），而不是"谁装了什么"。
//!
//! # 为什么顺手把"我们自己的两个根"也放在这里
//!
//! 存储根与 shim 目录是同一批事实的另一半：`owner = "tuoen"` 的判据
//! （哪些 `PATH` 条目是我们放上去的）与遮蔽检测（我们的 shim 有没有被抢名字）
//! 都要它们，而它们必须来自**同一个** `Store` 实例 —— 两处各算一次
//! `Store::at_default_location()` 今天恰好相等，但它相等是巧合而不是契约。
//!
//! 相对根会让上面那两个判断**静默**变成"不是"与"没有"（详见
//! [`Backends::roots_are_absolute`]），所以这里也给出那个判据本身。

use std::path::{Path, PathBuf};

use tuoen_core::detect::{DetectContext, ScanRoot, engine::default_scan_roots};
use tuoen_platform::{
    DEFAULT_PROBE_TIMEOUT, RealEnvBlock, RealFileSystem, RealProcessEnv, RealRegistry,
    SystemProcessRunner,
};
use tuoen_store::Store;

use crate::managed::StoreManagedStore;

/// 六个真实后端 + 我们自己的两个根，装在一起。
///
/// 它是**值**而不是一组自由函数：同一个 `Store` 要同时喂给
/// `CaptureOptions::tuoen_roots` 与 `StoreManagedStore`，而"两个根来自同一个
/// `Store`"这件事只能靠把它们放进同一个结构体来保证。
pub(crate) struct Backends {
    store_root: PathBuf,
    shim_dir: PathBuf,
    fs: RealFileSystem,
    registry: RealRegistry,
    env: RealEnvBlock<RealRegistry, RealFileSystem>,
    process_env: RealProcessEnv,
    runner: SystemProcessRunner,
    managed: StoreManagedStore,
    scan_roots: Vec<ScanRoot>,
}

impl Backends {
    /// 装出六个真实实现。
    ///
    /// **只拼路径、不创建任何目录**（`Store::at_default_location` 的既定行为）——
    /// "看一眼根在哪"与"在那里装东西"是两件事。
    pub(crate) fn assemble() -> Self {
        let store = Store::at_default_location();
        let store_root = store.root().to_path_buf();
        // shim 目录用 `shim_cmd::shim_dir`（全仓唯一一份定义），不自己拼 ——
        // 第二份定义会漂移，而它漂移的后果是"遮蔽检测看了一个不存在的目录"。
        let shim_dir = crate::shim_cmd::shim_dir(&store);

        let fs = RealFileSystem;
        let registry = RealRegistry;
        let env = RealEnvBlock::new(registry, fs);
        let process_env = RealProcessEnv::new();
        let runner = SystemProcessRunner;
        let managed = StoreManagedStore::new(store.clone());
        // 扫描根由进程环境推出来，所以必须在 `process_env` 之后算。
        let scan_roots = default_scan_roots(&process_env);

        Self {
            store_root,
            shim_dir,
            fs,
            registry,
            env,
            process_env,
            runner,
            managed,
            scan_roots,
        }
    }

    /// 投影成引擎要的上下文。
    ///
    /// `probe_versions` 是**唯一**允许逐命令不同的开关：`capture --no-version`、
    /// `path diff`（只关心结构）都关掉它，`detect` 打开它。关掉它只影响
    /// "要不要 spawn 工具问版本"，不影响任何后端本身。
    pub(crate) fn context(&self, probe_versions: bool) -> DetectContext<'_> {
        DetectContext {
            fs: &self.fs,
            registry: &self.registry,
            env: &self.env,
            process_env: &self.process_env,
            runner: &self.runner,
            managed: &self.managed,
            probe_timeout: DEFAULT_PROBE_TIMEOUT,
            probe_versions,
            scan_roots: self.scan_roots.clone(),
        }
    }

    /// 持久环境块（用户级 + 机器级）。
    ///
    /// 返回**具体类型**而不是 `&dyn EnvBlock`：`tuoen_core::pathdiff` 的
    /// `current_username_from_env` 与 `tuoen_platform::plan_rewrite` 都收
    /// `&impl EnvBlock`（隐含 `Sized`），而 `DetectContext::env` 是 `&dyn EnvBlock`。
    /// 同一条理由适用于 [`Self::process_env`]。
    pub(crate) fn env(&self) -> &RealEnvBlock<RealRegistry, RealFileSystem> {
        &self.env
    }

    /// 当前进程的环境块。
    pub(crate) fn process_env(&self) -> &RealProcessEnv {
        &self.process_env
    }

    /// 注册表。零大小类型，但**仍然只从这里拿**：装配只有一处。
    pub(crate) fn registry(&self) -> &RealRegistry {
        &self.registry
    }

    /// 存储根的绝对路径。
    pub(crate) fn store_root(&self) -> &Path {
        &self.store_root
    }

    /// shim 目录的绝对路径。
    pub(crate) fn shim_dir(&self) -> &Path {
        &self.shim_dir
    }

    /// 我们自己的两个根**都是绝对路径**吗。
    ///
    /// # 为什么这是一个"宁可失败"的判据（`doctor` / `path diff` 的 `unwired-roots`）
    ///
    /// `Store::at_default_location()` 在 `%LOCALAPPDATA%` 与 `%USERPROFILE%` 都读不到时
    /// 会退到相对路径 `.tuoen\store`（那是 store 层的既定行为：它**不 panic**，
    /// 一个读不到根位置的进程仍然应该能构造出 store）。而相对根会**静默**污染两类判断：
    ///
    /// * `owner`（哪些 `PATH` 条目是我们自己的）比较的是 `PATH` 上的绝对条目与我们的
    ///   相对根 —— 恒不相等，于是我们自己的条目被报成"别人的"；
    /// * 遮蔽检测比较 shim 目录与 `PATH` 顺序上的目录 —— 同样是恒不相等，于是
    ///   "我们的 shim 被抢了名字"这一类发现**一条都不报**。
    ///
    /// 两者都会让报告看起来完全正常，所以调用方该**失败**并带出这两个根。
    /// 判据只有这一处，免得两个命令各写一遍（写两遍就会有一天只改了一处）。
    pub(crate) fn roots_are_absolute(&self) -> bool {
        self.store_root.is_absolute() && self.shim_dir.is_absolute()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assembling_twice_gives_the_same_roots() {
        // `assemble` **不创建任何目录**，也不读时钟 —— 所以两次装配的根必须逐字相同。
        // 这一条钉住"两个根来自同一个 Store 实例"这件事（真机上它是同一串字符）。
        let first = Backends::assemble();
        let second = Backends::assemble();
        assert_eq!(first.store_root(), second.store_root());
        assert_eq!(first.shim_dir(), second.shim_dir());
        // shim 目录与存储根**并列**（`<home>/shims` 与 `<home>/store`），不在 store 里面：
        // `store/` 可以被清空重来，而 `PATH` 上那批文件是用户环境的一部分。
        assert_ne!(
            first.shim_dir(),
            first.store_root(),
            "shim 目录不该等于存储根"
        );
    }

    #[test]
    fn the_two_roots_are_absolute_on_any_sane_windows() {
        // 这一条**不断言机器状态**，只断言"环境里有 LOCALAPPDATA 时它们是绝对的"——
        // 没有变量时相对根是 store 层的既定行为，调用方该走 `unwired-roots`。
        let backends = Backends::assemble();
        let has_local_app_data = std::env::var_os("LOCALAPPDATA").is_some();
        assert_eq!(
            backends.roots_are_absolute(),
            has_local_app_data,
            "根的绝对性完全由 %LOCALAPPDATA% 决定（store 层的既定行为）"
        );
    }
}
