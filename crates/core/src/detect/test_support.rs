//! 检测引擎的测试支撑：把 `tuoen_platform::fixture` 的假机器接成 [`DetectContext`]。
//!
//! **为什么不在这里另造一套假后端**：`tuoen_platform::fixture` 已经是权威的那一套
//! （`MachineFixture` 可 `serde` 反序列化，`fixtures/detect/*.toml` 就是它）。
//! 在这里再造一套会出现两个后果，两个都不可接受：
//! 1. 两份假注册表会漂移，而漂移出来的形状在真机上根本不存在；
//! 2. "测试绝不读真机"这条硬约束会变成"在两个地方各保证一次"，等于没保证。
//!
//! 所以这里只做一件事：**把假后端与 `DetectContext` 的字段对上**。
//!
//! ## 持久环境变量怎么来的
//!
//! `MachineFixture` 只有**进程**环境（`env` 表）。持久环境（`HKCU\Environment` 与
//! `HKLM\...\Session Manager\Environment`）走注册表，所以固定装置用
//! `registry = [{ hive = "hkcu", path = "Environment", values = { NVM_HOME = { sz = "..." } } }]`
//! 来声明。
//!
//! 这里**复用生产的 `RealEnvBlock`**（它只依赖 `Registry` + `FileSystem` 两个 trait）
//! 而不是另写一个假环境块 —— 这样"读环境变量"这条路径在测试里跑的是**同一份代码**，
//! 包括 `%VAR%` 展开与 `target_exists` 的判定。用一个假实现去测这条路径，
//! 等于把最想测的东西测掉了。

use std::time::Duration;

use tuoen_platform::RealEnvBlock;
use tuoen_platform::fixture::{FakeMachine, MachineFixture};

use crate::detect::{DetectContext, ScanRoot};

/// 检测用的假机器：平台层的假后端 + 环境块。
#[derive(Debug, Clone)]
pub struct DetectFixture {
    /// 平台层造出来的假机器（文件系统 / 注册表 / 进程 / 安装记录 / 进程环境）。
    pub machine: FakeMachine,
    /// 持久环境变量（从假注册表里读，复用生产的读取路径）。
    pub env: RealEnvBlock<tuoen_platform::FakeRegistry, tuoen_platform::FakeFileSystem>,
    /// 扫描根目录。
    pub scan_roots: Vec<ScanRoot>,
    /// 版本探测的超时。
    pub probe_timeout: Duration,
    /// 要不要探测版本。
    pub probe_versions: bool,
}

impl DetectFixture {
    /// 按一份机器描述造固定装置。
    #[must_use]
    pub fn build(description: &MachineFixture) -> Self {
        let machine = description.build();
        Self {
            env: RealEnvBlock::new(machine.registry.clone(), machine.fs.clone()),
            machine,
            scan_roots: Vec::new(),
            // 测试里的假运行器是立刻返回的，所以超时值只影响"它被传下去了没有"。
            probe_timeout: Duration::from_millis(50),
            probe_versions: true,
        }
    }

    /// 加一个扫描根。
    #[must_use]
    pub fn with_scan_root(mut self, path: &str, why: &'static str) -> Self {
        self.scan_roots.push(ScanRoot {
            path: path.into(),
            why,
        });
        self
    }

    /// 关掉版本探测（测"只做结构查询"的分支）。
    #[must_use]
    pub fn without_probing(mut self) -> Self {
        self.probe_versions = false;
        self
    }

    /// 构造注入上下文。
    #[must_use]
    pub fn context(&self) -> DetectContext<'_> {
        DetectContext {
            fs: &self.machine.fs,
            registry: &self.machine.registry,
            env: &self.env,
            process_env: &self.machine.env,
            runner: &self.machine.runner,
            managed: &self.machine.managed,
            probe_timeout: self.probe_timeout,
            probe_versions: self.probe_versions,
            scan_roots: self.scan_roots.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuoen_platform::EnvScope;
    use tuoen_platform::fixture::{
        FixtureDir, FixtureKey, FixturePath, FixtureProcess, MachineFixture,
    };
    use tuoen_platform::{EnvBlock, RegHive, RegValue};

    use crate::detect::Confidence;

    /// 本机取证的一个缩影：`python` 被 0 字节别名抢走、真 Python 在后、
    /// nvm4w 管着 node、`C:\Dev\Tool` 下有一个任何注册表都没提到的 Maven。
    fn realistic_machine() -> MachineFixture {
        MachineFixture {
            env: std::collections::BTreeMap::from([(
                "Path".to_owned(),
                r"C:\WindowsApps;C:\nvm4w\nodejs;C:\Python312;".to_owned(),
            )]),
            paths: Vec::new(),
            dirs: vec![
                FixtureDir::new(
                    r"C:\WindowsApps",
                    vec![FixturePath::app_exec_alias("python.exe")],
                ),
                FixtureDir::new(
                    r"C:\Python312",
                    vec![FixturePath::file("python.exe", 102_400)],
                ),
                FixtureDir::new(
                    r"C:\nvm4w\nodejs",
                    vec![FixturePath::file("node.exe", 80_000)],
                ),
                FixtureDir::new(r"C:\Dev\Tool", vec![FixturePath::dir("apache-maven-3.9.5")]),
            ],
            registry: vec![
                FixtureKey::new(
                    RegHive::Hkcu,
                    "Environment",
                    std::collections::BTreeMap::from([
                        (
                            "NVM_HOME".to_owned(),
                            RegValue::Sz(r"C:\Users\x\AppData\Local\nvm".to_owned()),
                        ),
                        (
                            "NVM_SYMLINK".to_owned(),
                            RegValue::Sz(r"C:\nvm4w\nodejs".to_owned()),
                        ),
                    ]),
                ),
                FixtureKey::new(
                    RegHive::Hklm,
                    r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
                    std::collections::BTreeMap::from([(
                        "NVM_HOME".to_owned(),
                        RegValue::Sz(r"C:\nvm4w".to_owned()),
                    )]),
                ),
            ],
            processes: vec![
                FixtureProcess {
                    program: r"C:\Python312\python.exe".to_owned(),
                    stdout: "Python 3.12.10\n".to_owned(),
                    stderr: String::new(),
                    exit_code: Some(0),
                    timed_out: false,
                },
                FixtureProcess {
                    program: r"C:\nvm4w\nodejs\node.exe".to_owned(),
                    stdout: "v24.19.0\n".to_owned(),
                    stderr: String::new(),
                    exit_code: Some(0),
                    timed_out: false,
                },
            ],
            managed: Vec::new(),
        }
    }

    #[test]
    fn persistent_env_comes_through_the_real_reader() {
        let fixture = DetectFixture::build(&realistic_machine());
        // 用户级与机器级同名变量都要读得到 —— 这是"双重管理腐坏"能被发现的前提。
        assert_eq!(
            fixture
                .env
                .get(EnvScope::User, "NVM_HOME")
                .expect("user NVM_HOME")
                .value_expanded,
            r"C:\Users\x\AppData\Local\nvm"
        );
        assert_eq!(
            fixture
                .env
                .get(EnvScope::Machine, "NVM_HOME")
                .expect("machine NVM_HOME")
                .value_expanded,
            r"C:\nvm4w"
        );
    }

    #[test]
    fn the_alias_is_found_but_reported_as_a_ghost() {
        let fixture = DetectFixture::build(&realistic_machine());
        let summary = crate::detect::detect_all(&fixture.context());
        let ghosts: Vec<_> = summary
            .tools
            .iter()
            .filter(|tool| tool.confidence == Confidence::AliasGhost)
            .collect();
        assert_eq!(
            ghosts.len(),
            1,
            "应当恰好有一条别名幽灵：{:?}",
            summary.tools
        );
        assert!(ghosts[0].path.ends_with(r"WindowsApps\python.exe"));
        // **别名不能被当成可用的 python**：它没有版本。
        assert_eq!(ghosts[0].version, None);
    }

    #[test]
    fn the_real_python_is_still_found_behind_the_alias() {
        let fixture = DetectFixture::build(&realistic_machine());
        let summary = crate::detect::detect_all(&fixture.context());
        let real: Vec<_> = summary
            .tools
            .iter()
            .filter(|tool| {
                tool.name == "python"
                    && tool.confidence == Confidence::Executable
                    && tool.path.ends_with(r"Python312\python.exe")
            })
            .collect();
        assert_eq!(real.len(), 1, "{:?}", summary.tools);
        assert_eq!(real[0].version.as_deref(), Some("3.12.10"));
    }

    #[test]
    fn the_manager_is_detected_and_the_duplicate_scope_is_reported() {
        let fixture = DetectFixture::build(&realistic_machine());
        let summary = crate::detect::detect_all(&fixture.context());
        let managed: Vec<_> = summary
            .tools
            .iter()
            .filter(|tool| tool.confidence == Confidence::ManagerOwned)
            .collect();
        assert_eq!(managed.len(), 1, "{:?}", summary.tools);
        assert_eq!(managed[0].manager.as_deref(), Some("nvm4w"));
        assert!(
            managed[0].evidence.contains("双重管理腐坏"),
            "同名变量在两个 scope 里都必须被点名：{}",
            managed[0].evidence
        );
    }

    #[test]
    fn a_directory_nobody_registered_is_still_found() {
        // 本机 `C:\Dev\Tool\apache-maven-3.9.5` 既不在 winget 也不在 PATH，
        // 但 `~/.m2/repository` 证明它在用 —— 这类东西最容易在换电脑时丢掉。
        let fixture =
            DetectFixture::build(&realistic_machine()).with_scan_root(r"C:\Dev\Tool", "测试用");
        let summary = crate::detect::detect_all(&fixture.context());
        let found: Vec<_> = summary
            .tools
            .iter()
            .filter(|tool| tool.name == "maven")
            .collect();
        assert_eq!(found.len(), 1, "{:?}", summary.tools);
        assert_eq!(found[0].version.as_deref(), Some("3.9.5"));
        assert_eq!(found[0].confidence, Confidence::DirectoryOnly);
    }

    #[test]
    fn the_alias_is_never_executed_but_the_real_file_is() {
        // 对 0 字节别名跑 `--version` 只会启动应用商店。这条断言钉住"我们没跑它"。
        let fixture = DetectFixture::build(&realistic_machine());
        let _ = crate::detect::detect_all(&fixture.context());
        let calls = fixture.machine.runner.calls();
        let programs: Vec<&str> = calls.iter().map(|call| call.program.as_str()).collect();
        assert!(
            !programs.iter().any(|p| p.contains("WindowsApps")),
            "不得对别名跑探测：{programs:?}"
        );
        assert!(
            programs.iter().any(|p| p.contains("Python312")),
            "真实文件必须被探测：{programs:?}"
        );
    }

    #[test]
    fn without_probing_no_process_is_started_at_all() {
        let fixture = DetectFixture::build(&realistic_machine()).without_probing();
        let summary = crate::detect::detect_all(&fixture.context());
        assert!(fixture.machine.runner.calls().is_empty());
        // 但发现本身还在，只是版本未知 —— "发现但版本未知"与"没发现"是两件事。
        assert!(
            summary
                .tools
                .iter()
                .any(|tool| tool.name == "python" && tool.confidence == Confidence::Executable)
        );
        assert!(
            summary
                .tools
                .iter()
                .filter(|tool| tool.confidence == Confidence::Executable)
                .all(|tool| tool.version.is_none())
        );
    }

    #[test]
    fn a_hanging_tool_yields_discovery_without_a_version() {
        let mut description = realistic_machine();
        // 把真 Python 换成"永远不返回"。
        description.processes[0].timed_out = true;
        description.processes[0].stdout = String::new();
        let fixture = DetectFixture::build(&description);
        let summary = crate::detect::detect_all(&fixture.context());
        let real = summary
            .tools
            .iter()
            .find(|tool| tool.path.ends_with(r"Python312\python.exe"))
            .expect("发现了");
        assert_eq!(real.version, None, "超时不得编出版本");
        assert_eq!(real.confidence, Confidence::Executable);
    }

    #[test]
    fn a_shadowed_command_says_which_entry_won() {
        // **遮蔽是必须处理的产品问题**（ADR-0002）：进程 PATH 是"机器级在前、用户级在后"，
        // 所以用户级条目永远输掉名字冲突 —— 我们（用户级 shim）永远轮不到执行。
        // 检测必须把这件事说出来，否则用户会以为"我装的版本生效了"。
        let mut description = realistic_machine();
        description.env.insert(
            "Path".to_owned(),
            r"C:\machine-tools;C:\nvm4w\nodejs;C:\Python312;".to_owned(),
        );
        description.dirs.push(FixtureDir::new(
            r"C:\machine-tools",
            vec![FixturePath::file("node.exe", 90_000)],
        ));
        let fixture = DetectFixture::build(&description);
        let summary = crate::detect::detect_all(&fixture.context());

        let nodes: Vec<_> = summary
            .tools
            .iter()
            .filter(|tool| {
                tool.name == "node" && tool.source == crate::detect::DetectionSource::PathResolution
            })
            .collect();
        assert_eq!(nodes.len(), 2, "两个 node.exe 都要报：{:?}", summary.tools);

        let winner = nodes
            .iter()
            .find(|tool| tool.path.contains("machine-tools"))
            .expect("机器级那条");
        assert!(
            !winner.evidence.contains("遮蔽"),
            "赢家不该说自己被遮蔽：{}",
            winner.evidence
        );

        let shadowed = nodes
            .iter()
            .find(|tool| tool.path.contains("nvm4w"))
            .expect("用户级那条");
        assert!(
            shadowed.evidence.contains("被 PATH 第 1 条遮蔽"),
            "被遮蔽的那条必须点名赢家在第几条：{}",
            shadowed.evidence
        );
    }

    #[test]
    fn a_managed_tool_is_the_only_level_we_can_fully_rebuild() {
        let mut description = realistic_machine();
        description.managed = vec![tuoen_platform::ManagedTool {
            name: "node".to_owned(),
            version: Some("24.19.0".to_owned()),
            path: r"C:\Users\x\AppData\Local\tuoen\tools\node\24.19.0".to_owned(),
        }];
        let fixture = DetectFixture::build(&description);
        let summary = crate::detect::detect_all(&fixture.context());
        let managed: Vec<_> = summary
            .tools
            .iter()
            .filter(|tool| tool.confidence == Confidence::Managed)
            .collect();
        assert_eq!(managed.len(), 1, "{:?}", summary.tools);
        assert_eq!(managed[0].source, crate::detect::DetectionSource::Tuoen);
        assert!(managed[0].confidence.is_reproducible());
    }

    #[test]
    fn the_manager_table_is_self_consistent() {
        // 这张表被三个人读：识别（`env_vars`）、路径（`path_env_var`）、
        // 版本（`version_link_var`）。三者指错一个都不会让编译失败，
        // 只会让报告悄悄变差 —— 所以这里逐条钉住。
        for manager in crate::detect::engine::KNOWN_MANAGERS {
            assert!(
                crate::detect::spec::spec_for_id(manager.tool).is_some(),
                "{} 声称管着 `{}`，但那个工具不在 KNOWN_TOOLS 里",
                manager.id,
                manager.tool
            );
            assert!(
                manager.env_vars.contains(&manager.path_env_var),
                "{} 的 path_env_var `{}` 不在 env_vars 里 —— 那个变量永远不会被读到",
                manager.id,
                manager.path_env_var
            );
            if let Some(link) = manager.version_link_var {
                assert!(
                    manager.env_vars.contains(&link),
                    "{} 的 version_link_var `{link}` 不在 env_vars 里",
                    manager.id
                );
                assert_ne!(
                    link, manager.path_env_var,
                    "{} 把同一个变量既当版本库根目录又当版本链接 —— \
                     版本库根目录不携带版本信息，读出来只会在 `path` 里放一个版本号",
                    manager.id
                );
            }
            assert!(
                manager.env_vars.iter().all(|name| !name.is_empty()),
                "{} 有一个空的环境变量名",
                manager.id
            );
        }
    }

    #[test]
    fn a_manager_whose_path_variable_is_a_version_number_does_not_put_it_in_path() {
        // **真机踩到的 bug，钉在这里。** 本机 `UV_PYTHON=3.13` —— 一个**版本号**。
        // 早先的实现按名字猜路径变量（"以 `_HOME` / `_ROOT` 结尾，否则用第一个命中"），
        // 于是 `uv` 那条记录的 `path` 变成了 `3.13`。
        //
        // 一个叫 `path` 的字段里放版本号，任何按路径解析它的消费者都会失败。
        // 是契约测试 `every_record_carries_a_source_a_confidence_and_evidence` 抓到的
        // —— 它断言 `path` 必须像路径或 `<…>` 占位符。
        let mut description = realistic_machine();
        // 持久环境变量走**注册表**，不走 `env` 表（`env` 是**进程**环境，
        // 而管理器识别读的是持久的那一份）。
        description.registry.push(FixtureKey::new(
            RegHive::Hkcu,
            "Environment",
            std::collections::BTreeMap::from([(
                "UV_PYTHON".to_owned(),
                RegValue::Sz("3.13".to_owned()),
            )]),
        ));
        let fixture = DetectFixture::build(&description);
        let summary = crate::detect::detect_all(&fixture.context());

        let uv = summary
            .tools
            .iter()
            .find(|tool| tool.manager.as_deref() == Some("uv"))
            .expect("UV_PYTHON 存在时 uv 必须被报出来");
        assert_eq!(uv.name, "python");
        assert_ne!(uv.path, "3.13", "版本号绝不能出现在 path 字段里");
        assert!(
            uv.path.starts_with('<') && uv.path.ends_with('>'),
            "没有路径型变量时必须给明确的占位符，实际是 `{}`",
            uv.path
        );
        // 但识别信号必须保留 —— 用户需要知道"uv 在这台机器上管着 python"。
        assert!(uv.evidence.contains("UV_PYTHON=3.13"), "{}", uv.evidence);
    }

    #[test]
    fn nvm4w_reads_the_version_from_the_symlink_not_from_the_repo_root() {
        // `NVM_HOME` 指向装着**所有**版本的目录（`…\nvm`），它本身不携带版本信息；
        // 版本在 `NVM_SYMLINK`（`C:\nvm4w\nodejs`）的链接目标 `…\nvm\v24.19.0` 里。
        //
        // 这条用例同时钉住假文件系统的一个语义：**显式的 `paths` 条目覆盖同名的
        // `dirs` 条目**。不覆盖的话，下面这个符号链接会被固定装置里的
        // `FixtureDir::new(r"C:\nvm4w\nodejs", …)` 静默变成一个普通目录，
        // `link_target` 永远是 `None`，而我们**测的就不再是符号链接那条路径**。
        let mut description = realistic_machine();
        description.paths.push(FixturePath::symlink_dir(
            r"C:\nvm4w\nodejs",
            r"C:\Users\x\AppData\Local\nvm\v24.19.0",
        ));
        description.dirs.push(FixtureDir::new(
            r"C:\Users\x\AppData\Local\nvm\v24.19.0",
            vec![FixturePath::file("node.exe", 80_000)],
        ));

        let fixture = DetectFixture::build(&description);
        // 先确认固定装置**真的**把那个目录做成了符号链接 —— 否则后面的断言
        // 会因为错误的原因通过（例如读到 `FixtureDir` 造出来的普通目录）。
        use tuoen_platform::FileSystem as _;
        let facts = fixture
            .machine
            .fs
            .inspect(std::path::Path::new(r"C:\nvm4w\nodejs"));
        assert!(
            matches!(facts.reparse, tuoen_platform::ReparseKind::SymlinkDir),
            "固定装置必须把它做成符号链接，实际是 {:?}",
            facts.reparse
        );

        let summary = crate::detect::detect_all(&fixture.context());
        let node = summary
            .tools
            .iter()
            .find(|tool| tool.manager.as_deref() == Some("nvm4w"))
            .expect("nvm4w 必须被报出来");

        assert_eq!(
            node.path, r"C:\Users\x\AppData\Local\nvm",
            "path 必须是版本库根目录"
        );
        assert_eq!(
            node.version.as_deref(),
            Some("24.19.0"),
            "version 必须从符号链接的目标里读出来"
        );
        assert_eq!(
            node.confidence,
            Confidence::ManagerOwned,
            "第三方管理器管的工具是只读采纳"
        );
    }

    #[test]
    fn an_app_paths_entry_that_is_not_on_path_is_still_found() {
        // **纯 PATH 扫描会整个漏掉这套查找机制**：Windows 的解析顺序是
        // 先 PATH、后 App Paths，所以一条只在 App Paths 里的安装是"真的能敲，
        // 但不在 PATH 上"的。
        let mut description = realistic_machine();
        description.registry.push(FixtureKey::new(
            RegHive::Hklm,
            r"Microsoft\Windows\CurrentVersion\App Paths\mvn.cmd",
            std::collections::BTreeMap::from([(
                String::new(),
                RegValue::Sz(r"C:\Dev\Tool\apache-maven-3.9.5\bin\mvn.cmd".to_owned()),
            )]),
        ));
        // 那个文件必须真的存在，否则会被报成 registered-missing。
        description.dirs.push(FixtureDir::new(
            r"C:\Dev\Tool\apache-maven-3.9.5\bin",
            vec![FixturePath::file("mvn.cmd", 2_048)],
        ));
        let fixture = DetectFixture::build(&description);
        let summary = crate::detect::detect_all(&fixture.context());

        let from_app_paths: Vec<_> = summary
            .tools
            .iter()
            .filter(|tool| tool.source == crate::detect::DetectionSource::AppPaths)
            .collect();
        assert_eq!(from_app_paths.len(), 1, "{:?}", summary.tools);
        assert_eq!(from_app_paths[0].name, "maven");
        assert_eq!(from_app_paths[0].confidence, Confidence::Executable);
        assert!(
            from_app_paths[0].evidence.contains("App Paths"),
            "{}",
            from_app_paths[0].evidence
        );
    }

    #[test]
    fn a_registry_entry_whose_directory_is_gone_is_a_ghost_not_an_install() {
        // 本机 `Python 3.11.9` 就是这个形状：注册表里一堆活卸载键，
        // 而注册的安装目录 `...\Programs\Python\Python311\` 不存在。
        let mut description = realistic_machine();
        description.registry.push(FixtureKey::new(
            RegHive::Hklm,
            r"Microsoft\Windows\CurrentVersion\Uninstall\{GHOST-1}",
            std::collections::BTreeMap::from([
                (
                    "DisplayName".to_owned(),
                    RegValue::Sz("Python 3.11.9 (64-bit)".to_owned()),
                ),
                (
                    "InstallLocation".to_owned(),
                    RegValue::Sz(r"C:\Users\x\AppData\Local\Programs\Python\Python311\".to_owned()),
                ),
                (
                    "DisplayVersion".to_owned(),
                    RegValue::Sz("3.11.9150.0".to_owned()),
                ),
            ]),
        ));
        // **故意不声明那个目录** —— 假文件系统对未声明的路径一律答"不存在"，
        // 所以这条用例测的正是"我们真的去看了文件在不在"。
        let fixture = DetectFixture::build(&description);
        let summary = crate::detect::detect_all(&fixture.context());

        let ghost = summary
            .tools
            .iter()
            .find(|tool| tool.confidence == Confidence::RegisteredMissing)
            .expect("必须报成幽灵条目");
        assert_eq!(ghost.name, "python");
        // **幽灵条目不得有版本**：注册表里的 `DisplayVersion` 是安装器的内部版本
        // （`3.11.9150.0`），它看起来像真版本号，所以比"未知"更糟。
        assert_eq!(ghost.version, None, "幽灵条目不得报版本：{ghost:?}");
        assert!(!ghost.confidence.is_reproducible());
        assert!(ghost.evidence.contains("幽灵条目"), "{}", ghost.evidence);
    }
}
