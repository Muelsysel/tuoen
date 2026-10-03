//! 六个采集器：把一个注入式的机器读成 `tuoen.d/` 的六个文件。
//!
//! # 这个模块里所有采集器共守的六条规则
//!
//! 1. **只从 [`DetectContext`] 拿机器事实。** 绝不出现 `std::fs`、绝不出现
//!    `RealRegistry` —— 一切经过可注入的适配器。这不是洁癖：测试必须在**假机器**上
//!    跑到字节级确定，而真机上跑的是同一份代码。
//! 2. **读注册表原文时不展开。** [`tuoen_platform::EnvBlock`] 从不展开 `REG_EXPAND_SZ`
//!    （低层走 `RegEnumValueW`，它也不展开）。展开是**不可逆的信息损失**：
//!    只有不展开读，才区分得出"值本来就是这样"与"被 `setx` 展开过"。
//! 3. **展开由我们自己算，而且只在类型允许的时候算。** 见 [`Expander`]。
//!    `REG_SZ` 的值 Windows 不展开 —— 对一个 `REG_SZ` 里字面含 `%VAR%` 的条目做展开，
//!    会把它显示成一个根本不存在的路径。
//! 4. **说不准的地方一律三态。** `Existence::Unknown` / `TargetExistence::NotAPath`
//!    存在的理由就是"答不了的题不许猜"。
//! 5. **确定性。** 除了 `PATH` 条目的顺序（顺序本身是数据），其它一切都要排序 ——
//!    "同一台机器跑两次逐字节相同"是这一票承诺的东西，而排序是它的一部分。
//! 6. **绝不把值写进跳过原因。** 理由见 [`super::secrets`]。
//!
//! 第 7 条只对 `globals` / `configs` 这两个会**起进程**的采集器成立：
//! **外部命令只在一张常量表里定义**（[`Command`]），调用点不许拼字符串 ——
//! 命令字符串是固定装置与产品之间的契约，散落两处就会对不上，而对不上表现为
//! "工具不可用/枚举失败"（能看见，但要花很久才能定位）。

pub(crate) mod configs;
pub(crate) mod env;
pub(crate) mod globals;
pub(crate) mod path;
pub(crate) mod tools;
pub(crate) mod wsl;

pub(crate) use configs::collect_configs;
pub(crate) use env::collect_env;
pub(crate) use globals::collect_globals;
pub(crate) use path::collect_path;
pub(crate) use tools::collect_tools;
pub(crate) use wsl::collect_wsl;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tuoen_platform::{EnvScope, PATH_EXTENSIONS, ProcessOutcome};

use crate::detect::DetectContext;

/// 外部命令的**枚举**超时（决策 170）。
///
/// 给的是"要启动包管理器 / 要读整份配置"的那些命令：`npm ls -g`（冷 12.4 s）、
/// `npm config get prefix`、`git config --list`。`--version` 探测**不用**它 ——
/// 那个只启动一个解释器，用 `ctx.probe_timeout`（3 秒）。
pub(crate) const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

/// 一次外部调用：**程序 + 参数**。
///
/// 这是"命令字符串只有一处定义"的载体：`globals` 与 `configs` 的每一条命令都是
/// 一个常量（见 `globals.rs` / `configs.rs` 顶部的表），调用点只引用常量。
///
/// `program` 一律是**裸名字**（`cmd.exe` / `node.exe` / `pip.exe` / `git.exe`）：
/// `CreateProcess` 自己会按 `System32` → `PATH` 的顺序解析，而我们**不拼绝对路径** ——
/// 固定装置按程序名匹配，拼一个绝对路径只会让固定装置与产品对不上。
#[derive(Debug, Clone, Copy)]
pub(crate) struct Command {
    /// 程序名（裸名字）。
    pub(crate) program: &'static str,
    /// 参数（逐字，顺序即命令行顺序）。
    pub(crate) args: &'static [&'static str],
}

impl Command {
    /// 跑一次，**追加参数 + 覆盖子进程环境**。
    ///
    /// 这两件事只在一处发生：tuoen 自己的全局根（ticket #23）。我们既不写
    /// `.npmrc` 也不写 `pip.ini`（决策 27），于是"把包装到我们的根里 / 从我们的
    /// 根里读"**只能**靠进程环境变量（`NPM_CONFIG_PREFIX` / `PYTHONUSERBASE`）
    /// 与命令行开关（`npm ls --prefix`）。
    ///
    /// 不需要这两样时传空表：`run(&[], &[], timeout)` 与 #17 的 `run(timeout)`
    /// 逐字节等价（调用点只传空表，所以"原来的行为"没有第二条代码路径）。
    ///
    /// # 为什么参数是 `&[String]` 而不是拼进 [`Command::args`]
    ///
    /// [`Command`] 的 `args` 是**编译期常量**（模块文档第 7 条：外部命令只在一张
    /// 常量表里定义，调用点不许拼字符串）。根里含用户的 `%LOCALAPPDATA%`
    /// 与运行时版本号，拼不进常量 —— 所以它们只能是"追加在常量载荷后面的
    /// 两段值"，而这段值从哪来由 [`globals`] 一处决定。
    pub(crate) fn run_with(
        &self,
        ctx: &DetectContext<'_>,
        extra_args: &[String],
        env: &[(String, String)],
        timeout: Duration,
    ) -> ProcessOutcome {
        let mut args: Vec<&str> = self.args.to_vec();
        args.extend(extra_args.iter().map(String::as_str));
        ctx.runner
            .run_env(Path::new(self.program), &args, env, timeout)
    }

    /// 跑一次并只要 stdout —— 而且**只在真的成功时**才算数。
    ///
    /// `spawned == false` / `timed_out` / 非零退出都返回 `None`：这三个都是
    /// "这个工具没回答"，而把它们当成"它回答了空字符串"会让我们**编**出一个值。
    pub(crate) fn run_text(&self, ctx: &DetectContext<'_>, timeout: Duration) -> Option<String> {
        self.run_text_with(ctx, &[], &[], timeout)
    }

    /// [`Command::run_text`] 的带参数/带环境版本（见 [`Command::run_with`]）。
    pub(crate) fn run_text_with(
        &self,
        ctx: &DetectContext<'_>,
        extra_args: &[String],
        env: &[(String, String)],
        timeout: Duration,
    ) -> Option<String> {
        let outcome = self.run_with(ctx, extra_args, env, timeout);
        if !outcome.spawned || outcome.timed_out || outcome.exit_code != Some(0) {
            return None;
        }
        Some(outcome.stdout)
    }
}

/// stdout 的第一行（去掉行尾的 `\r`）。
///
/// `node -v` / `npm config get prefix` 都是一行一条，而 Windows 上那行以 `\r\n` 结束 ——
/// 不去掉 `\r` 会让 `prefix` 变成一个**末尾带回车**的路径（它在文件里看不出来，
/// 但在任何按路径使用它的地方都会失败）。
pub(crate) fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .to_owned()
}

/// 在**进程** `%Path%` 上找一条命令，返回它的完整路径。
///
/// 判据与 `pin::shell::dir_has_command` / `tuoen_platform::path::detect_shadowing`
/// 同一套：逐目录 `list_dir`，名字大小写不敏感，裸名字再逐个拼
/// [`PATH_EXTENSIONS`]。多出来的那一条是"名字本身就是 `node`（没有扩展名）"——
/// `cmd.exe` 真的会尝试执行这种文件。
///
/// **绝不返回 App Execution Alias**（`WindowsApps\python.exe` 那种 0 字节别名）：
/// 执行它会启动应用商店，而"找不到"在这里是一个正确且无害的答案。
///
/// 用**进程**环境那一份 `Path`（而不是持久环境）：CreateProcess 用的就是它，
/// 所以"这个命令敲得出来吗"的答案来自它。
pub(crate) fn find_on_path(ctx: &DetectContext<'_>, name: &str) -> Option<PathBuf> {
    let path = ctx.process_var("Path")?;
    let wanted = name.to_ascii_lowercase();

    for dir in path.split(';').map(str::trim).filter(|d| !d.is_empty()) {
        let dir_path = Path::new(dir);
        for entry in ctx.fs.list_dir(dir_path) {
            if entry.is_dir || entry.reparse.is_app_exec_alias() {
                continue;
            }
            let found = entry.name.to_ascii_lowercase();
            let matches = found == wanted
                || (!wanted.contains('.')
                    && PATH_EXTENSIONS
                        .iter()
                        .any(|extension| found == format!("{wanted}.{extension}")));
            if matches {
                return Some(dir_path.join(&entry.name));
            }
        }
    }
    None
}

/// 一个路径里有没有"像版本号"的段（决策 173 的第三支用的）。
///
/// 判据 = 段里**去掉开头的 `v` 之后**只剩数字与点，而且以数字开头：
/// `v24` / `v24.19.0` / `3.12` / `24.19.0` / `312` 都算，
/// 而 `Python312` / `nvm4w` / `current` / `C:` 都不算（带字母的段是**名字**，不是版本）。
///
/// **注意**：`crates/core/tests/capture_globals_configs.rs` 里那份独立实现把
/// "点分"那一路写成了 `lower.split('.')`（带着 `v`），于是 `v24.19.0` 在它那里是
/// **假** —— 那与决策 173 的原文（"`v` + 数字，或纯数字点分"）不符，已报给 lead 与
/// `pathdiff-core`。产品这一侧按决策原文实现：`v24.19.0` 是版本段。
pub(crate) fn has_version_segment(path: &str) -> bool {
    path.split(['\\', '/']).any(version_like)
}

fn version_like(segment: &str) -> bool {
    let lower = segment.to_ascii_lowercase();
    let body = lower.strip_prefix('v').unwrap_or(&lower);
    !body.is_empty()
        && body.starts_with(|c: char| c.is_ascii_digit())
        && body.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// 最多展开几轮。
///
/// 存在的理由是**自引用**：`A=%A%` 或者 `A=%B%` + `B=%A%`。没有上限的话，
/// 一个手滑写坏的变量会让 `capture` 卡死；有了上限，它只是留下一个没展开的
/// `%A%`，于是那条被报成"不知道"—— 一个正确且无害的结果。
const MAX_EXPANSION_PASSES: usize = 8;

/// 按当前的进程 / 持久环境展开 `%VAR%`。
///
/// **为什么不直接用 `ExpandEnvironmentStringsW`**：那个函数看的是**当前进程**的环境块，
/// 而我们要回答的问题是"**这台机器上**这个值会被解析成什么"。在真机上两者几乎一样，
/// 在固定装置里差了十万八千里 —— 而固定装置才是这条逻辑被测试的地方。
///
/// 查找顺序（后写的覆盖先写的）：**机器级 → 用户级 → 进程**。
/// 进程在最后，因为真机上它已经是前两者合并后的结果（外加启动器注入）。
#[derive(Debug, Clone, Default)]
pub(crate) struct Expander {
    /// 键是**大写**的变量名 —— Windows 的环境变量名大小写不敏感。
    vars: BTreeMap<String, String>,
}

impl Expander {
    /// 从注入上下文里把三个来源的环境变量都读进来。
    ///
    /// 读不到就**静默跳过那一层**：捕获的其余部分仍然有价值，
    /// 而一个读不到的作用域会让展开结果里留下 `%VAR%`，那正好被报成"不知道"。
    pub(crate) fn from_context(ctx: &DetectContext<'_>) -> Self {
        let mut vars = BTreeMap::new();

        for scope in [EnvScope::Machine, EnvScope::User] {
            for var in ctx.env.list(scope) {
                // 用**原文**而不是展开值：展开值是我们自己算出来的，
                // 拿它当后续展开的输入会把一个错误放大成一片。
                vars.insert(var.name.to_uppercase(), var.value_raw);
            }
        }

        for (name, value) in ctx.process_env.vars() {
            vars.insert(name.to_uppercase(), value);
        }

        Self { vars }
    }

    /// 一个变量名（大小写不敏感）对应的值。
    #[must_use]
    pub(crate) fn get(&self, name: &str) -> Option<&str> {
        self.vars.get(&name.to_uppercase()).map(String::as_str)
    }

    /// 展开 `%VAR%`。
    ///
    /// 认不出来的 `%NAME%` **原样保留** —— 那正是"我们不知道"的证据，
    /// 调用方靠 [`has_unresolved`] 把它变成 `Existence::Unknown`。
    #[must_use]
    pub(crate) fn expand(&self, value: &str) -> String {
        let mut current = value.to_owned();
        for _ in 0..MAX_EXPANSION_PASSES {
            let (next, substituted) = self.pass(&current);
            if !substituted {
                return next;
            }
            current = next;
            if !current.contains('%') {
                break;
            }
        }
        current
    }

    /// 走一遍：替换所有能认出来的 `%NAME%`。
    ///
    /// # 认不出来的时候**不吞掉后面那个 `%`**
    ///
    /// 第一版把"第一个 `%` 与下一个 `%` 之间"整段当成变量名，认不出就整段写回。
    /// 那在 `100% done %A%` 上给出错误答案：`%`(3) 与 `%A%` 的**开引号**(10) 被配成一对，
    /// 于是 `A` 永远等不到它的引号，一个完全正常的变量被一个字面百分号吃掉了。
    /// 正确做法是：认不出就只把**这一个** `%` 当字面量，从它的下一个字节继续找。
    fn pass(&self, value: &str) -> (String, bool) {
        let mut out = String::with_capacity(value.len());
        let mut substituted = false;
        let bytes = value.as_bytes();
        let mut at = 0;

        while at < value.len() {
            if bytes[at] != b'%' {
                // 逐个字符推进：`%` 是 ASCII，所以按字节走不会切坏一个多字节字符
                // （我们只在**等于** `%` 时才会停，其余位置整段复制）。
                let ch = value[at..].chars().next().expect("at 在界内");
                out.push(ch);
                at += ch.len_utf8();
                continue;
            }
            match value[at + 1..].find('%') {
                Some(close) => {
                    let name = &value[at + 1..at + 1 + close];
                    if !name.is_empty()
                        && let Some(replacement) = self.get(name)
                    {
                        out.push_str(replacement);
                        substituted = true;
                        at += 1 + close + 1;
                        continue;
                    }
                    // 认不出来：这个 `%` 是字面量。
                    out.push('%');
                    at += 1;
                }
                None => {
                    // 落单的 `%`：原样保留（它可能就是一个字面百分号）。
                    out.push('%');
                    at += 1;
                }
            }
        }

        (out, substituted)
    }
}

/// 展开之后还留着 `%` 吗？—— 留着就说明我们对这个值**说不准**。
#[must_use]
pub(crate) fn has_unresolved(expanded: &str) -> bool {
    expanded.contains('%')
}

/// 值里有没有 `%`（不区分它展开得开）。**这就是 `has_vars`。**
#[must_use]
pub(crate) fn has_vars(raw: &str) -> bool {
    raw.contains('%')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expander(pairs: &[(&str, &str)]) -> Expander {
        let mut vars = BTreeMap::new();
        for (name, value) in pairs {
            vars.insert(name.to_uppercase(), (*value).to_owned());
        }
        Expander { vars }
    }

    #[test]
    fn a_single_variable_is_expanded() {
        let e = expander(&[("SystemRoot", r"C:\Windows")]);
        assert_eq!(e.expand(r"%SystemRoot%\System32"), r"C:\Windows\System32");
    }

    #[test]
    fn expansion_is_case_insensitive_and_nested() {
        let e = expander(&[
            ("LOCALAPPDATA", r"C:\Users\x\AppData\Local"),
            ("TUOEN_HOME", r"%LOCALAPPDATA%\tuoen"),
        ]);
        assert_eq!(
            e.expand(r"%tuoen_home%\shims"),
            r"C:\Users\x\AppData\Local\tuoen\shims"
        );
    }

    #[test]
    fn an_unknown_variable_is_kept_verbatim_so_the_caller_can_say_unknown() {
        let e = expander(&[]);
        let expanded = e.expand(r"%NOPE%\bin");
        assert_eq!(expanded, r"%NOPE%\bin");
        assert!(has_unresolved(&expanded));
    }

    /// 自引用不许把 `capture` 挂死 —— 它只该留下一个没展开的 `%A%`。
    #[test]
    fn self_reference_terminates_and_leaves_the_marker() {
        let e = expander(&[("A", "%A%"), ("B", "%A%;ok")]);
        assert_eq!(e.expand("%A%"), "%A%");
        assert_eq!(e.expand("%B%"), "%A%;ok");
    }

    #[test]
    fn a_mutual_cycle_terminates_too() {
        let e = expander(&[("A", "%B%"), ("B", "%A%")]);
        let out = e.expand("%A%");
        assert!(out.contains('%'), "环上必然会留下没展开的标记：{out}");
    }

    #[test]
    fn a_lone_percent_is_not_a_variable() {
        let e = expander(&[("A", "1")]);
        // **这条用例抓出过一个真 bug**：第一版把 `%`(3) 与 `%A%` 的开引号配成一对，
        // 于是 `A` 永远等不到它的引号 —— 一个字面百分号吃掉了一个正常的变量。
        assert_eq!(e.expand("100% done %A%"), "100% done 1");
        assert_eq!(e.expand("50%"), "50%");
        assert_eq!(e.expand("%A% %A%"), "1 1");
        assert_eq!(e.expand("%NOPE% %A%"), "%NOPE% 1");
    }

    #[test]
    fn an_empty_name_is_never_substituted() {
        let e = expander(&[("", "boom")]);
        assert_eq!(e.expand("%%"), "%%");
    }

    #[test]
    fn has_vars_and_unresolved_answer_different_questions() {
        assert!(has_vars(r"%SystemRoot%\System32"));
        assert!(!has_unresolved(
            &expander(&[("SystemRoot", r"C:\Windows")]).expand(r"%SystemRoot%\System32")
        ));
        assert!(!has_vars(r"C:\Windows"));
    }
}
