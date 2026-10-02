//! `env.toml` —— 持久环境变量（用户级 + 机器级），以及密钥跳过清单。
//!
//! 要求与判据（写给实现者，也写给后来读这份文件的人）：
//!
//! * **两个作用域都要读**，同名变量出现在两个作用域时**两条都在** ——
//!   本机 `NVM_HOME` / `NVM_SYMLINK` 就是真实的双重管理腐坏，把其中一条覆盖掉
//!   等于把这个症状从快照里删掉。
//! * `value_raw` 是注册表原文，**从不展开**；`value_expanded` 按类型算
//!   （`sz` 就是原文，`expand-sz` 才展开）。
//! * 变量名可以含空格（本机 `IntelliJ IDEA`）—— 往返不许丢。
//! * `target` 只在值像**一条**绝对路径时才有；`target_exists` 的四态语义见
//!   [`super::super::files::TargetExistence`]。**值不是路径时必须是 `not-a-path`**：
//!   本机 `UV_PYTHON=3.13`，把它判成"指向的目录不存在"是一个纯粹的假问题。
//!   同理，**`;` 分隔的列表（`Path` / `PSModulePath`）也不是一条路径** ——
//!   它们都以 `C:\` 开头，只看前缀会把整条列表拿去问磁盘（真机验收抓出来的）。
//! * **密钥扫描必须扫三个作用域**（用户级、机器级、进程），命中就**不写进
//!   `env.toml`**，改为返回一条 [`SkipEntry`]。进程级变量本来也不会被写进文件，
//!   但"看见了却不说"比多一条记录糟得多。
//! * 跳过记录里**不许出现值** —— 连前几位、长度、哈希都不行。判据见
//!   [`super::super::secrets`]。
//!
//! # 三个具体的实现选择，各自的理由
//!
//! 1. **进程环境一个字节都不写进 `env.toml`。** 它不可搬运：它是"这一台机器此刻
//!    的结果"（`CreateProcess` 时从父进程复制来的），把 68 个进程变量写进快照只会让
//!    每一次 diff 都淹在噪声里。**但密钥扫描仍然要扫它** —— 见上面的第三条。
//! 2. **`value_expanded` 取决于 `reg_type`，不是"总是展开"。** 理由写在
//!    [`super::super::files::PathRow::expanded`] 上：`REG_SZ` 的值 Windows **不展开**，
//!    替它展开会把它显示成一个根本不存在的路径。
//! 3. **排序键是（变量名忽略大小写，作用域）。** 只有排序稳定，"同一台机器跑两次
//!    逐字节相同"才成立。作用域那一档用 `as_str()` 的 slug（`machine` < `user`）
//!    而不是枚举本身 —— `EnvScope` 没有 `Ord`，而 slug 是更稳的契约。
//!    撞上"仅大小写不同的两个名字"时退化成**稳定排序**保住的原始顺序 —— 这里
//!    不额外定义那条规则，因为 `Expander` 的查表也是忽略大小写的，那两个名字
//!    在展开时本来就指向同一个变量。
//!
//! # 为什么 `target_exists = not-a-path` 是一个必须存在的答案
//!
//! 本机的 `UV_PYTHON=3.13`、`NVM_HOME` 这类值根本不是路径。把它们一律丢给
//! "指向的目录不存在"会报出一整批**纯粹的假问题** —— 而一份充满假问题的报告
//! 等于没有报告。所以"这不是一个路径"必须是**一个明确的结论**，而不是失败。
//!
//! 真机验收把这条规则的两个漏洞都照出来了：`;` 列表（`Path` / `PSModulePath`）
//! 只看前缀会被当成一条路径，于是真机上 4 条 `no` 里有 3 条是假问题；
//! 修完之后那 4 条变成 1 条，而剩下的那一条（`HALCONROOT` 指向已经卸载的
//! HALCON）才是**真的**发现。

use std::path::Path;

use tuoen_platform::{EnvScope, RegType};

use crate::detect::DetectContext;

use super::super::files::{EnvFile, EnvVarRow, SkipEntry, TargetExistence};
use super::super::secrets;
use super::{Expander, has_unresolved};

/// 只写这两个作用域。进程环境**不在其中**，理由见模块文档第 1 条。
///
/// 顺序是"机器级在前、用户级在后" —— 与 `Expander` 的查找顺序、
/// 与 Windows 自己拼环境块时的顺序一致。
const PERSISTENT_SCOPES: [EnvScope; 2] = [EnvScope::Machine, EnvScope::User];

/// 采集持久环境变量，并返回被跳过的那些（**只有结论，没有材料**）。
pub(crate) fn collect_env(ctx: &DetectContext<'_>, captured_at: &str) -> (EnvFile, Vec<SkipEntry>) {
    let expander = Expander::from_context(ctx);
    let mut file = EnvFile::new(captured_at);
    let mut skips = Vec::new();

    for scope in PERSISTENT_SCOPES {
        for var in ctx.env.list(scope) {
            if let Some(entry) = secret_skip(scope, &var.name, &var.value_raw) {
                skips.push(entry);
                continue;
            }
            file.var.push(row(&expander, ctx, scope, &var));
        }
    }

    // 第三个作用域：**只看不写**。命中就记一条 `scope = "process-only"`，
    // 那句话本身就是"它本来也不会被写进文件"的解释。
    for (name, value) in ctx.process_env.vars() {
        if let Some(entry) = secret_skip(EnvScope::ProcessOnly, &name, &value) {
            skips.push(entry);
        }
    }

    // 排序是幂等性的一部分：两次运行的顺序必须一样，否则"逐字节相同"无从谈起。
    //
    // 第二排序键用的是 `scope.as_str()` 而不是 `EnvScope` 本身：那个枚举**没有**
    // `Ord`（`tuoen_platform` 那边没有为它实现），而"按 slug 排序"与"按枚举声明
    // 顺序排序"在这里给出同一个答案 —— `machine` < `user` < `process-only`
    // 与 `Machine` < `User` < `ProcessOnly` 的顺序一致，且 slug 是稳定契约。
    file.var.sort_by(|a, b| {
        (a.name.to_lowercase(), a.scope.as_str()).cmp(&(b.name.to_lowercase(), b.scope.as_str()))
    });
    skips.sort_by(|a, b| {
        (&a.section, &a.scope, a.name.to_lowercase()).cmp(&(
            &b.section,
            &b.scope,
            b.name.to_lowercase(),
        ))
    });

    (file, skips)
}

/// 一个变量写进 `env.toml` 的那一行。
fn row(
    expander: &Expander,
    ctx: &DetectContext<'_>,
    scope: EnvScope,
    var: &tuoen_platform::EnvVar,
) -> EnvVarRow {
    // **类型说了算**：`REG_SZ` 的值 Windows 不展开，替它展开会把它显示成
    // 一个根本不存在的路径（见 `files.rs` 里 `PathRow::expanded` 的文档）。
    let expanded = match var.reg_type {
        RegType::ExpandSz => expander.expand(&var.value_raw),
        RegType::Sz => var.value_raw.clone(),
    };
    let target =
        (looks_like_absolute_path(&expanded) && !is_a_list(&expanded)).then(|| expanded.clone());

    EnvVarRow {
        name: var.name.clone(),
        scope,
        value_raw: var.value_raw.clone(),
        target_exists: existence_of(ctx, &expanded),
        reg_type: var.reg_type,
        target,
        value_expanded: expanded,
    }
}

/// 这个值指向的目标在不在。
///
/// # 四条判据的顺序是有意为之（照票据 #12 的第 2 条）
///
/// 0. **列表 → `not-a-path`，而且这一条必须排在"像不像绝对路径"前面。**
///    真机验收抓出来的：`looks_like_absolute_path` 只看前缀，而 `Path` 与
///    `PSModulePath` 的第一个字符就是 `C` —— 于是**整条 `;` 串**被当成一条路径
///    拿去问磁盘，答案恒为"不存在"。真机上 4 条 `target_exists = "no"` 里有 3 条
///    是这么来的（`Path` 两份 + `PSModulePath`），而"`Path` 指向的目录不存在"
///    是一句会直接误导 `doctor` 的假话（剩下那条 `HALCONROOT` 才是真的）。
/// 1. **不是路径 → `not-a-path`。** 一个版本号（`UV_PYTHON=3.13`）拿去 `inspect`
///    只会得到一个恒为假的答案，而那个答案会被读成"这个目录不存在" ——
///    那是一个纯粹的假问题，而一份充满假问题的报告等于没有报告。
/// 2. **展开后还留着 `%` → `unknown`。** 这一条**排在"在不在"前面**，而且它管的是
///    一个比"绝对路径"宽一点的形状（[`is_unresolved_path`]）：`%NOPE%\bin` 展开前后
///    都不匹配 `X:\…`，但把它归成"不是路径"是在说另一件事。答不了的题不许猜 ——
///    所以"我们认得出它是想写一个路径、只是没认出那个变量"这一态必须存在。
/// 3. **看过了 → `yes` / `no`。** 只有这一条是真的去问了文件系统。
fn existence_of(ctx: &DetectContext<'_>, expanded: &str) -> TargetExistence {
    if is_a_list(expanded) {
        return TargetExistence::NotAPath;
    }
    if looks_like_absolute_path(expanded) {
        if has_unresolved(expanded) {
            return TargetExistence::Unknown;
        }
        return if ctx.fs.inspect(Path::new(expanded)).exists {
            TargetExistence::Yes
        } else {
            TargetExistence::No
        };
    }
    if is_unresolved_path(expanded) {
        return TargetExistence::Unknown;
    }
    TargetExistence::NotAPath
}

/// 值是不是一个**列表**（`;` 分隔）：`Path`、`PSModulePath`、`PATHEXT` 都是。
///
/// # 为什么它必须排在 [`looks_like_absolute_path`] 前面
///
/// 真机验收抓出来的：那个函数只看前缀，而 `Path` 与 `PSModulePath` 的第一个字符
/// 就是 `C` —— 于是**整条 `;` 串**被当成一条路径拿去问磁盘，答案恒为"不存在"。
/// 真机上 4 条 `target_exists = "no"` 里有 3 条是这么来的（`Path` 两份 +
/// `PSModulePath`），而"`Path` 指向的目录不存在"是一句会直接误导 `doctor`
/// 的假话。剩下那条 `HALCONROOT` 才是真的：HALCON 卸了，变量还留着。
///
/// `;` 在 Windows 的环境变量里就是列表分隔符 —— 一个值的**中间**有分号，
/// 它就不是"一条路径"了（`target` 也就没有意义，见 [`row`]）。
fn is_a_list(value: &str) -> bool {
    value.contains(';')
}

/// 像不像一个**绝对**路径目标：`X:\…` / `\\…` / `//…`。
///
/// # 为什么要求"绝对"
///
/// 环境变量的值里有大量**不是路径**的东西（版本号 `3.13`、枚举 `true`、列表
/// `a;b;c`），把它们拿去 `inspect` 只会得到一个恒为假的答案，而那个答案会被
/// 读成"这个目录不存在"。这正是 [`TargetExistence::NotAPath`] 存在的理由。
///
/// `%SystemRoot%\System32` 在这里是 `false`（首字节是 `%`），它由
/// [`is_unresolved_path`] 接手判成 `unknown`。
fn looks_like_absolute_path(value: &str) -> bool {
    if value.starts_with(r"\\") || value.starts_with("//") {
        return true;
    }
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
}

/// "我们认得出它是想写一个路径，但那个变量名没认出来"：以 `%` 开头、值里有分隔符，
/// 而且把开头那个没认出来的 `%VAR%` 剥掉之后确实剩下一个路径形状。
///
/// # 为什么它必须与 [`looks_like_absolute_path`] 分开
///
/// `%NOPE%\bin` 展开前后都不匹配 `X:\…`，但它与 `UV_PYTHON=3.13` 是**两件不同的事**：
/// 前者是"我们不知道那个变量是什么"（`unknown`），后者是"它根本不是路径"
/// （`not-a-path`）。合并成一种回答会让"这台机器上有一条指向未知位置的 PATH 条目"
/// 这个发现消失 —— 而那正是本机 `%NOPE%\bin` 那一类条目该被报出来的原因。
fn is_unresolved_path(value: &str) -> bool {
    let Some(rest) = value.strip_prefix('%') else {
        return false;
    };
    let Some(close) = rest.find('%') else {
        return false;
    };
    let after = &rest[close + 1..];
    !after.is_empty() && (after.starts_with(['\\', '/']) || looks_like_absolute_path(after))
}

/// 这个变量是不是凭据？是的话给一条**不含材料**的跳过记录。
///
/// 扫描器本身在 [`secrets`] 里，且它有一条硬不变量：结论里不含值本身的任何字节。
/// 这里要守的是那条不变量的**推论**：`SkipEntry` 的每个字段都只能来自**名字**
/// 或**结论**，绝不能来自值。
fn secret_skip(scope: EnvScope, name: &str, value: &str) -> Option<SkipEntry> {
    let detection = secrets::scan(name, value)?;
    Some(SkipEntry {
        section: "env".to_owned(),
        scope: Some(scope.as_str().to_owned()),
        // 变量名（`scanned_name` 是它的一部分）—— 它不是材料。
        name: name.to_owned(),
        kind: detection.kind().to_owned(),
        reason: detection.reason(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tuoen_platform::fixture::{FixtureDir, FixtureKey, FixturePath, MachineFixture};
    use tuoen_platform::{EnvScope, RegHive, RegValue};

    use crate::capture::Section;
    use crate::capture::test_support::CaptureFixture;

    /// 用户级 / 机器级环境变量分别写在**两个不同的注册表键**里。
    ///
    /// 用户级是 `HKCU\Environment`（**不在 `SOFTWARE` 下面**），机器级是
    /// `HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment`。
    /// 这条路径本仓库踩过一次真 bug（见 `tuoen_platform::env_block::USER_ENV_SUBKEY`），
    /// 所以用例里把它写全，而不是藏进某个辅助函数。
    const USER_ENV: &str = "Environment";
    const MACHINE_ENV: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";

    const AT: &str = "2026-10-02T12:00:00Z";

    fn user(values: Vec<(&str, RegValue)>) -> FixtureKey {
        FixtureKey::new(
            RegHive::Hkcu,
            USER_ENV,
            values
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value))
                .collect(),
        )
    }

    fn machine(values: Vec<(&str, RegValue)>) -> FixtureKey {
        FixtureKey::new(
            RegHive::Hklm,
            MACHINE_ENV,
            values
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value))
                .collect(),
        )
    }

    /// 从渲染出来的 `env.toml` 读回结构 —— 断言的是**文件里到底写了什么**，
    /// 而不是内存里的中间结果。
    fn env_file(fixture: &CaptureFixture) -> crate::capture::EnvFile {
        let files = fixture.files(&[Section::Env], AT);
        toml::from_str(files.get("env.toml").expect("env.toml")).expect("反序列化")
    }

    /// 从渲染出来的 `env.toml` 取原文。
    fn env_text(fixture: &CaptureFixture) -> String {
        fixture
            .files(&[Section::Env], AT)
            .get("env.toml")
            .expect("env.toml")
            .clone()
    }

    /// 从渲染出来的 `skipped.toml` 取原文。
    fn skipped_text(fixture: &CaptureFixture) -> String {
        fixture
            .files(&[Section::Env], AT)
            .get("skipped.toml")
            .expect("扫过 env 就该有跳过清单")
            .clone()
    }

    /// 本机 `NVM_HOME` / `NVM_SYMLINK` 的真实形状：同一个变量同时被用户级与机器级
    /// 设过（**双重管理腐坏**）。两条都必须在，否则这个症状被快照悄悄抹掉了。
    #[test]
    fn the_same_variable_in_two_scopes_is_two_rows_not_one() {
        let fixture = CaptureFixture::build(&MachineFixture {
            registry: vec![
                user(vec![(
                    "NVM_HOME",
                    RegValue::Sz(r"C:\Users\x\AppData\Local\nvm".into()),
                )]),
                machine(vec![("NVM_HOME", RegValue::Sz(r"C:\nvm4w".into()))]),
            ],
            ..MachineFixture::default()
        });
        let file = env_file(&fixture);

        let homes: Vec<_> = file
            .var
            .iter()
            .filter(|row| row.name == "NVM_HOME")
            .collect();
        assert_eq!(homes.len(), 2, "{:?}", file.var);
        assert_eq!(
            homes[0].scope,
            EnvScope::Machine,
            "排序：同名时 machine 在前"
        );
        assert_eq!(homes[0].value_raw, r"C:\nvm4w");
        assert_eq!(homes[1].scope, EnvScope::User, "排序：同名时 user 在后");
        assert_eq!(homes[1].value_raw, r"C:\Users\x\AppData\Local\nvm");
    }

    /// 变量名可以含空格（本机 `IntelliJ IDEA`）—— 它是合法名字，往返不许丢。
    #[test]
    fn a_variable_name_with_a_space_round_trips() {
        let fixture = CaptureFixture::build(&MachineFixture {
            registry: vec![user(vec![(
                "IntelliJ IDEA",
                RegValue::Sz(r"C:\Program Files\JetBrains".into()),
            )])],
            ..MachineFixture::default()
        });
        let file = env_file(&fixture);

        assert_eq!(file.var.len(), 1, "{:?}", file.var);
        assert_eq!(file.var[0].name, "IntelliJ IDEA");
        // 序列化出去再读回来的**文本**里也必须带空格 —— 否则它就不是"往返"。
        assert!(
            env_text(&fixture).contains(r#"name = "IntelliJ IDEA""#),
            "名字里的空格必须原样写进文件"
        );
    }

    /// `target_exists` 的四态**必须各自出现过一次**。
    ///
    /// 四态各自的理由不同：`yes` / `no` 是"我们看过了"，`unknown` 是"我们认得出它是
    /// 路径，但展开不出来"，`not-a-path` 是"它根本不是路径" ——
    /// 最后那一态是本机 `UV_PYTHON=3.13` 那一类，缺了它整个报告会被假问题淹掉。
    #[test]
    fn all_four_target_existences_are_reachable() {
        let fixture = CaptureFixture::build(&MachineFixture {
            registry: vec![user(vec![
                ("THERE", RegValue::Sz(r"C:\Tools\here".into())),
                ("GONE", RegValue::Sz(r"C:\Tools\gone".into())),
                ("UNRESOLVED", RegValue::ExpandSz(r"%NOPE%\bin".into())),
                ("A_VERSION", RegValue::Sz("3.13".into())),
            ])],
            dirs: vec![FixtureDir::new(
                r"C:\Tools\here",
                vec![FixturePath::file("keep.exe", 10)],
            )],
            ..MachineFixture::default()
        });
        let file = env_file(&fixture);

        let row = |name: &str| {
            file.var
                .iter()
                .find(|row| row.name == name)
                .unwrap_or_else(|| panic!("`{name}` 必须被捕获：{:?}", file.var))
        };
        assert_eq!(
            row("THERE").target_exists,
            crate::capture::TargetExistence::Yes
        );
        assert_eq!(row("THERE").target.as_deref(), Some(r"C:\Tools\here"));
        assert_eq!(
            row("GONE").target_exists,
            crate::capture::TargetExistence::No
        );
        assert_eq!(
            row("UNRESOLVED").target_exists,
            crate::capture::TargetExistence::Unknown
        );
        // **本条用例的重点**：`3.13` 不是"一个不存在的目录"，它是"不是目录"。
        assert_eq!(
            row("A_VERSION").target_exists,
            crate::capture::TargetExistence::NotAPath,
            "版本号不是路径 —— 把它判成「不存在」就是一个纯粹的假问题"
        );
        assert_eq!(row("A_VERSION").target, None);
    }

    /// **列表不是一条路径** —— 而且这一条必须排在"像不像绝对路径"前面。
    ///
    /// 真机验收抓出来的：`Path` 与 `PSModulePath` 的第一个字符就是 `C`，
    /// 只看前缀会把整条 `;` 串拿去问磁盘，答案恒为"不存在"。真机上 4 条
    /// `target_exists = "no"` 里有 3 条是这么来的，而"`Path` 指向的目录不存在"
    /// 是一句会直接误导 `doctor` 的假话。
    #[test]
    fn a_list_is_not_a_single_path_target() {
        let fixture = CaptureFixture::build(&MachineFixture {
            registry: vec![user(vec![
                (
                    "PSModulePath",
                    RegValue::Sz(r"C:\Tools\here;C:\Tools\gone".into()),
                ),
                // 对照组：**单条**路径仍然要照常判"在不在"。
                ("SINGLE", RegValue::Sz(r"C:\Tools\here".into())),
            ])],
            dirs: vec![FixtureDir::new(
                r"C:\Tools\here",
                vec![FixturePath::file("keep.exe", 10)],
            )],
            ..MachineFixture::default()
        });
        let file = env_file(&fixture);

        let list = file
            .var
            .iter()
            .find(|row| row.name == "PSModulePath")
            .expect("列表型变量也要被捕获");
        assert_eq!(
            list.target_exists,
            crate::capture::TargetExistence::NotAPath,
            "整条 `;` 串不是一个路径目标"
        );
        assert_eq!(
            list.target, None,
            "`target` 也不该记下整条列表 —— 它是 path.toml 的事"
        );
        assert_eq!(
            list.value_raw, r"C:\Tools\here;C:\Tools\gone",
            "值本身一个字都不动"
        );

        let single = file
            .var
            .iter()
            .find(|row| row.name == "SINGLE")
            .expect("单条路径");
        assert_eq!(single.target_exists, crate::capture::TargetExistence::Yes);
        assert_eq!(single.target.as_deref(), Some(r"C:\Tools\here"));
    }

    /// `expand-sz` 展开、`sz` **不展开** —— 类型说了算。
    #[test]
    fn value_expanded_follows_the_registry_type_not_the_presence_of_a_percent() {
        let fixture = CaptureFixture::build(&MachineFixture {
            registry: vec![user(vec![
                (
                    "EXPANDED",
                    RegValue::ExpandSz(r"%SystemRoot%\System32".into()),
                ),
                (
                    "NOT_EXPANDED",
                    RegValue::Sz(r"%SystemRoot%\System32".into()),
                ),
            ])],
            env: BTreeMap::from([("SystemRoot".to_owned(), r"C:\Windows".to_owned())]),
            ..MachineFixture::default()
        });
        let file = env_file(&fixture);

        let row = |name: &str| {
            file.var
                .iter()
                .find(|row| row.name == name)
                .unwrap_or_else(|| panic!("`{name}` 必须被捕获：{:?}", file.var))
        };

        assert_eq!(
            row("EXPANDED").value_raw,
            r"%SystemRoot%\System32",
            "原文从不展开"
        );
        assert_eq!(
            row("EXPANDED").value_expanded,
            r"C:\Windows\System32",
            "expand-sz 必须展开"
        );

        assert_eq!(
            row("NOT_EXPANDED").value_raw,
            r"%SystemRoot%\System32",
            "sz 的原文与展开值必须是同一个字节"
        );
        assert_eq!(
            row("NOT_EXPANDED").value_expanded,
            r"%SystemRoot%\System32",
            "**sz 不许展开**：Windows 不会展开一个 REG_SZ，替它展开就是报一个假的路径"
        );
    }

    /// **本票的安全红线**：形似凭据的值不写进 `tuoen.d/` 的**任何**文件，
    /// 但必须在 `skipped.toml` 里说出"看见了、跳过了、为什么" —— 而且**只说不含材料的结论**。
    #[test]
    fn a_credential_is_skipped_and_never_reaches_any_file() {
        // **前缀与正文分开写**：`concat!` 是编译期的，拼出来的值与一个字面量完全一样，
        // 而提交进仓库的文本里没有一个是完整令牌 —— GitHub 的 push protection 会拦下
        // `glpat-` 开头的字面量（票据 #12 的提交就被它拦过一次）。
        let gitlab = concat!("glpat-", "AbCdEfGhIjKlMnOpQrSt");
        let opaque = "0123456789abcdef0123456789abcdef";
        let fixture = CaptureFixture::build(&MachineFixture {
            registry: vec![user(vec![("GITLAB_TOKEN", RegValue::Sz(gitlab.into()))])],
            env: BTreeMap::from([("APP_TOKEN".to_owned(), opaque.to_owned())]),
            ..MachineFixture::default()
        });

        let files = fixture.all_files(AT);

        // ① 没有任何一个产出文件里出现那些值（连一段都不行）。
        for (name, text) in &files {
            for secret in [gitlab, opaque] {
                assert!(!text.contains(secret), "`{name}` 里出现了凭据：\n{text}");
            }
        }

        // ② 跳过清单里有记录。
        let skipped = skipped_text(&fixture);
        assert_eq!(
            skipped.matches("[[skipped]]").count(),
            2,
            "用户级与进程级各一条：\n{skipped}"
        );

        // ③ 记的是**变量名**与**结论**，其中不含那个值。
        assert!(skipped.contains(r#"name = "GITLAB_TOKEN""#), "{skipped}");
        assert!(skipped.contains(r#"name = "APP_TOKEN""#), "{skipped}");
        assert!(skipped.contains(r#"scope = "process-only""#), "{skipped}");
        assert!(skipped.contains(r#"kind = "credential""#), "{skipped}");
        assert!(skipped.contains("GitLab"), "原因要说清像什么：{skipped}");

        // 变量名是必须的，而"这个值特有的字节"一个都不许在 —— 厂商前缀除外
        // （`glpat-` 是 GitLab 公开的格式，不是这个值特有的内容）。
        for fragment in ["AbCd", "MnOpQrSt", "0123456789abcdef", "3456789abcdef"] {
            assert!(
                !skipped.contains(fragment),
                "`{fragment}` 泄露了：\n{skipped}"
            );
        }
    }

    /// 跳过的是**值**，不是**名字**：`env.toml` 里不能有 `GITLAB_TOKEN` 这一行
    /// （它的值就是凭据），但它在 `skipped.toml` 里必须被点名。
    #[test]
    fn the_skipped_variable_is_absent_from_env_toml() {
        let fixture = CaptureFixture::build(&MachineFixture {
            registry: vec![user(vec![
                (
                    "GL_TOKEN",
                    RegValue::Sz(concat!("glpat-", "AbCdEfGhIjKlMnOpQrSt").into()),
                ),
                ("SAFE", RegValue::Sz(r"C:\Tools".into())),
            ])],
            ..MachineFixture::default()
        });
        let file = env_file(&fixture);
        assert_eq!(file.var.len(), 1, "{:?}", file.var);
        assert_eq!(file.var[0].name, "SAFE");
    }

    /// 同一台假机器跑两次，**逐字节相同**。
    ///
    /// 捕获的幂等性是这一票承诺的东西，而它最容易悄悄坏掉的地方就是"某个
    /// 容器按迭代顺序输出"（`HashMap` 的迭代顺序、注册表枚举顺序）。
    #[test]
    fn capturing_twice_is_byte_identical() {
        let fixture = CaptureFixture::build(&MachineFixture {
            registry: vec![
                user(vec![
                    ("NVM_SYMLINK", RegValue::Sz(r"C:\nvm4w\nodejs".into())),
                    (
                        "IntelliJ IDEA",
                        RegValue::Sz(r"C:\Program Files\JetBrains".into()),
                    ),
                    (
                        "NVM_HOME",
                        RegValue::Sz(r"C:\Users\x\AppData\Local\nvm".into()),
                    ),
                    ("GONE", RegValue::Sz(r"C:\Tools\gone".into())),
                ]),
                machine(vec![("NVM_HOME", RegValue::Sz(r"C:\nvm4w".into()))]),
            ],
            env: BTreeMap::from([
                ("Path".to_owned(), r"C:\Windows;C:\Tools".to_owned()),
                ("NVM_HOME".to_owned(), r"C:\nvm4w".to_owned()),
            ]),
            ..MachineFixture::default()
        });

        assert_eq!(
            fixture.files(&[Section::Env], AT),
            fixture.files(&[Section::Env], AT)
        );
    }

    /// 没扫过 env 就没有跳过清单 —— "扫了没跳过东西"与"没扫"是两件事。
    #[test]
    fn skipping_the_section_produces_no_skipped_file() {
        let fixture = CaptureFixture::build(&MachineFixture::default());
        let files = fixture.files(&[Section::Wsl], AT);
        assert!(!files.contains_key("skipped.toml"), "{:?}", files.keys());
    }
}
