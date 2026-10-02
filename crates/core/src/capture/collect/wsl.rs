//! `wsl.toml` —— WSL 发行版与它们的**实际** vhdx 路径。
//!
//! 要求与判据（写给实现者，也写给后来读这份文件的人）：
//!
//! * 数据源是注册表 `HKCU\Software\Microsoft\Windows\CurrentVersion\Lxss` 的**子键**
//!   （每个发行版一个 `{GUID}` 子键），走注入的 [`tuoen_platform::Registry`]，
//!   **不许** `std::process::Command::new("wsl")` —— 那既是进程启动又是真机依赖。
//! * 每个子键里读：`DistributionName`（字符串）、`BasePath`（字符串）、
//!   `Version`（DWORD：1 / 2）、`State`（DWORD：1 = 已安装）。
//! * **必须记实际 `BasePath`**：本机 `Arch-Linux-current` 的 `BasePath` 是
//!   `C:\linux\Arch-Linux-current` —— 手工导入的非标准位置。只 glob 默认目录
//!   （`%LOCALAPPDATA%\wsl` / `%LOCALAPPDATA%\Packages\…`）的实现会完全漏掉它，
//!   而漏掉的恰好是"换电脑时最需要知道它装在哪"的那一类。
//! * vhdx 路径 = `BasePath` + `\ext4.vhdx`；在不在、多大都走注入的文件系统。
//! * `BasePath` 展开后不在 `%LOCALAPPDATA%` 底下 → `non_standard_path = true`。
//!   **展开不出来的**（还留着 `%VAR%`）按非标准处理，因为"说不准"不等于"标准"。
//! * 没有 Lxss 键就是"这台机器上没有 WSL" —— 空文件，不是错误。
//!
//! # 实现里的四个判断，各自的理由
//!
//! 1. **没有 `DistributionName` 的子键直接跳过。** Lxss 下面有 `DefaultDistribution`
//!    之类的兄弟值，历史上也留下过没有名字的半截子键；一条没有名字的记录既不能被
//!    用户认出来，也不能被 `restore` 用上 —— 它只有噪声。跳过是确定性的（子键列表
//!    在假注册表里已排序，在真注册表里我们自己也排了序）。
//! 2. **字符串值 `Sz` 与 `ExpandSz` 一视同仁地展开。** 与 [`super::env`] 里
//!    "类型说了算"不同：那里记的是**一个变量的值**（原文本身就是数据），
//!    这里记的是**一个位置的坐标**（路径）。一个 `REG_SZ` 的 `BasePath` 在真机上
//!    仍然是那条路径，展开它不会伪造出一个不存在的位置。
//! 3. **`%LOCALAPPDATA%` 取不到、或 `BasePath` 展开后还留着 `%` → `true`。**
//!    这个字段回答的是"它在不在默认位置"，而"我们说不准"**不等于**"标准位置"。
//! 4. **比较忽略大小写、忽略尾部分隔符。** Windows 的路径比较就是这样，
//!    而 `C:\Users\x\AppData\Local\wsl` 与 `...\Local\WSL\` 是同一个目录。

use tuoen_platform::RegValue;

use crate::detect::DetectContext;

use super::super::files::{Existence, WslFile, WslRow};
use super::{Expander, has_unresolved};

/// Lxss 子键的位置：`HKCU\Software\Microsoft\Windows\CurrentVersion\Lxss`。
///
/// **这里写的不是 `SOFTWARE\` 开头的那条完整路径，而是一条相对路径。** 理由是
/// [`tuoen_platform::RegHive::resolve`] 的语义：它对**不以绝对前缀开头**的子键
/// 自动拼上 `SOFTWARE\`（`HKCU` / `HKLM` 都拼，`HKLM\WOW6432Node` 多拼一层）。
/// 写成 `Software\Microsoft\...` 会踩到一个**静默**的坑：`RegHive::Hkcu.resolve`
/// 把开头的 `SOFTWARE\` 当成"已经写全了"而**剥掉**它，于是最终路径刚好还是对
/// `SOFTWARE\...` 一次 —— 而**假注册表**那边的键名归一化不剥这个前缀，
/// 于是真机上读得到、测试里读成一个不存在的键。
///
/// 症状正是本仓库最怕的那种："没有报错，只是少了整个 section"。所以常量与固定装置
/// 都写相对路径，让两边落在**同一个**键上。
const LXSS_SUBKEY: &str = r"Microsoft\Windows\CurrentVersion\Lxss";

/// 默认位置所在的根：WSL 默认把发行版放在它底下（`%LOCALAPPDATA%\wsl\{guid}`）。
const LOCAL_APP_DATA: &str = "LOCALAPPDATA";

/// vhdx 的文件名。WSL 的磁盘就叫这个名字 —— 它是**约定**，不是配置。
const VHDX_NAME: &str = r"ext4.vhdx";

/// 采集 WSL 发行版。
pub(crate) fn collect_wsl(ctx: &DetectContext<'_>, captured_at: &str) -> WslFile {
    let expander = Expander::from_context(ctx);

    // 键不存在 = 这台机器上没有 WSL。**空文件，不是错误。**
    let Ok(guids) = ctx
        .registry
        .subkeys(tuoen_platform::RegHive::Hkcu, LXSS_SUBKEY)
    else {
        return WslFile::new(captured_at);
    };

    // 先按子键名排序再读：真注册表的枚举顺序是不保证的，而后面只按 `name` 排序 ——
    // 两个发行版重名时（`wsl --import` 允许），顺序就取决于这里。
    let mut guids = guids;
    guids.sort();

    let mut distribution: Vec<WslRow> = guids
        .iter()
        .filter_map(|guid| read_distribution(ctx, &expander, guid))
        .collect();
    distribution.sort_by(|a, b| a.name.cmp(&b.name));

    WslFile {
        distribution,
        ..WslFile::new(captured_at)
    }
}

/// 读一个 `{GUID}` 子键。**没有名字就返回 `None`**（见模块文档第 1 条）。
fn read_distribution(ctx: &DetectContext<'_>, expander: &Expander, guid: &str) -> Option<WslRow> {
    let subkey = format!("{LXSS_SUBKEY}\\{guid}");
    let values = ctx
        .registry
        .values(tuoen_platform::RegHive::Hkcu, &subkey)
        .ok()?;

    let name = text_of(&values, "DistributionName", expander)?;
    if name.is_empty() {
        return None;
    }
    let base_path = text_of(&values, "BasePath", expander)?;
    let vhdx_path = format!("{}\\{VHDX_NAME}", base_path.trim_end_matches(['\\', '/']));

    // **去看文件时用剥掉 `\\?\` 的那一份**，但记下来的仍是注册表原文 —— 理由见
    // `strip_verbatim_prefix`。本机 `docker-desktop` 的 `BasePath` 就带着那个前缀，
    // 而它的 vhdx 是**真的存在**的（实测 100663296 字节）：不剥的话会报成"找不到"。
    let facts = ctx
        .fs
        .inspect(std::path::Path::new(strip_verbatim_prefix(&vhdx_path)));
    Some(WslRow {
        name,
        guid: guid.to_owned(),
        non_standard_path: !is_under_local_app_data(&base_path, expander),
        wsl_version: dword_of(&values, "Version"),
        state: dword_of(&values, "State"),
        base_path,
        vhdx_exists: if facts.exists {
            Existence::Yes
        } else {
            Existence::No
        },
        vhdx_bytes: facts.exists.then_some(facts.size),
        vhdx_path,
    })
}

/// 取一个字符串值并展开它。
///
/// `Sz` 与 `ExpandSz` 在这里不区分，理由见模块文档第 2 条。
fn text_of(values: &[(String, RegValue)], name: &str, expander: &Expander) -> Option<String> {
    let raw = values
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .and_then(|(_, value)| value.as_str())?;
    Some(expander.expand(raw))
}

/// 取一个 DWORD。**取不到就是 `None`** —— 不猜 0 也不猜 1。
fn dword_of(values: &[(String, RegValue)], name: &str) -> Option<u32> {
    values
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .and_then(|(_, value)| match value {
            RegValue::Dword(number) => Some(*number),
            _ => None,
        })
}

/// `base_path` 在 `%LOCALAPPDATA%` 底下吗？
///
/// **"说不准"返回 `false`（= 非标准）**：这个字段的语义是"我知道它在默认位置"，
/// 而我们不知道的时候不能替它说好话。
fn is_under_local_app_data(base_path: &str, expander: &Expander) -> bool {
    if has_unresolved(base_path) {
        return false;
    }
    let Some(local) = expander.get(LOCAL_APP_DATA) else {
        return false;
    };
    if has_unresolved(local) {
        return false;
    }
    let base = normalized(base_path);
    let root = normalized(local);
    // 空的根会匹配一切 —— 那是最糟的一种"假阳性"，直接判非标准。
    if root.is_empty() {
        return false;
    }
    // 比的是**目录前缀**，不是字符串前缀：`root` 后面必须跟一个分隔符。
    // 少了这一条，`C:\Users\x\AppData\LocalFoo\wsl` 会被算成"在默认位置底下" ——
    // 一个把非标准位置报成标准位置的假阴性，而这个字段唯一的用途就是标出它们。
    let prefix = if root.ends_with(['\\', '/']) {
        root
    } else {
        format!("{root}\\")
    };
    base.starts_with(&prefix)
}

/// 路径比较用的归一化：**先剥掉 `\\?\`**、再去掉尾部分隔符、小写。
///
/// **不碰盘符以外的冒号**，也不动中间的分隔符：这里只做"这两个字符串指的是不是
/// 同一个目录前缀"这一个判断。
fn normalized(path: &str) -> String {
    strip_verbatim_prefix(path)
        .trim_end_matches(['\\', '/'])
        .to_lowercase()
}

/// 去掉 Win32 的"别解释我"前缀（`\\?\` 或 `\??\`）。
///
/// 它不是路径的一部分，而是**给内核的标记**：告诉 Win32 不要做规范化、不要做
/// `MAX_PATH` 检查。本机实测 `docker-desktop` 的 `BasePath` 就是
/// `\\?\C:\Users\Muelsyse\AppData\Local\Docker\wsl\main`。不剥掉会有两个后果，
/// 两个都是**假的问题**：
///
/// 1. 与展开后的 `%LOCALAPPDATA%` 做前缀比较必然失败 → 一个明明在默认位置底下的
///    发行版被报成"非标准位置"；
/// 2. 拼出来的 vhdx 路径与仓库其余部分（`FileFacts`、`ensure_inside_store`）用的
///    普通形式不一致 → 一个**真的存在**的 vhdx 被报成"找不到"。
///
/// 只剥开头的一次。`\\?\UNC\server\share` 剥完是 `UNC\server\share`，那不是合法路径，
/// 所以这一支**原样返回**：UNC 共享本来就不在 `%LOCALAPPDATA%` 底下（判成"非标准"是
/// 对的），而 `\\?\UNC\…` 这个写法 Win32 自己认识（本机不存在这一支）。
///
/// 按**字节**比较而不是按 `&str` 切片：前缀是 ASCII，用 `str` 下标切会在路径以
/// 多字节字符开头时 panic —— 一个只在别人的机器上才会炸的 bug。
fn strip_verbatim_prefix(path: &str) -> &str {
    let bytes = path.as_bytes();
    for prefix in [r"\\?\", r"\??\"] {
        let head = prefix.as_bytes();
        if bytes.len() >= head.len() && bytes[..head.len()].eq_ignore_ascii_case(head) {
            let rest = &path[head.len()..];
            let tail = rest.as_bytes();
            if tail.len() >= 4 && tail[..4].eq_ignore_ascii_case(b"UNC\\") {
                return path;
            }
            return rest;
        }
    }
    path
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tuoen_platform::fixture::{FixtureDir, FixtureKey, FixturePath, MachineFixture};
    use tuoen_platform::{RegHive, RegValue};

    use crate::capture::test_support::CaptureFixture;
    use crate::capture::{Existence, Section, WslFile};

    use super::strip_verbatim_prefix;

    const AT: &str = "2026-10-02T12:00:00Z";

    /// Lxss 的**声明**写法：相对 `HKCU`，所以会被拼成
    /// `SOFTWARE\Microsoft\Windows\CurrentVersion\Lxss`（与生产代码里的常量一致）。
    const LXSS: &str = r"Microsoft\Windows\CurrentVersion\Lxss";

    /// 本机的真实形状：Ubuntu 在默认位置，`Arch-Linux-current` 是手工导入的
    /// 非标准位置（`C:\linux\Arch-Linux-current`）。
    const LOCAL_APP_DATA_VAR: &str = "LOCALAPPDATA";
    const LOCAL_APP_DATA: &str = r"C:\Users\Muelsyse\AppData\Local";
    const UBUNTU_GUID: &str = "{11111111-1111-1111-1111-111111111111}";
    const ARCH_GUID: &str = "{22222222-2222-2222-2222-222222222222}";

    fn lxss_key() -> FixtureKey {
        FixtureKey::new(RegHive::Hkcu, LXSS, BTreeMap::new())
    }

    fn distro(guid: &str, values: Vec<(&str, RegValue)>) -> FixtureKey {
        FixtureKey::new(
            RegHive::Hkcu,
            &format!("{LXSS}\\{guid}"),
            values
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value))
                .collect(),
        )
    }

    fn wsl_file(fixture: &CaptureFixture) -> WslFile {
        let files = fixture.files(&[Section::Wsl], AT);
        toml::from_str(files.get("wsl.toml").expect("wsl.toml")).expect("反序列化")
    }

    /// 两个发行版、两种位置：一个在 `%LOCALAPPDATA%\wsl\{guid}`，
    /// 一个是本机真实形状 `C:\linux\Arch-Linux-current`。
    fn two_distributions() -> MachineFixture {
        let ubuntu_base = format!("%LOCALAPPDATA%\\wsl\\{UBUNTU_GUID}");
        MachineFixture {
            registry: vec![
                lxss_key(),
                distro(
                    UBUNTU_GUID,
                    vec![
                        ("DistributionName", RegValue::Sz("Ubuntu-22.04".into())),
                        ("BasePath", RegValue::ExpandSz(ubuntu_base)),
                        ("Version", RegValue::Dword(2)),
                        ("State", RegValue::Dword(1)),
                    ],
                ),
                distro(
                    ARCH_GUID,
                    vec![
                        (
                            "DistributionName",
                            RegValue::Sz("Arch-Linux-current".into()),
                        ),
                        (
                            "BasePath",
                            RegValue::Sz(r"C:\linux\Arch-Linux-current".into()),
                        ),
                        // 注意没有 `State`：取不到就是 `None`，不许猜。
                        ("Version", RegValue::Dword(1)),
                    ],
                ),
            ],
            // 展开 `%LOCALAPPDATA%` 要用的那一个变量。**键是变量名、值是路径** ——
            // 反过来的话 `Expander` 会去查一个叫 `C:\Users\...` 的变量名。
            env: BTreeMap::from([(LOCAL_APP_DATA_VAR.to_owned(), LOCAL_APP_DATA.to_owned())]),
            dirs: vec![
                FixtureDir::new(
                    &format!(r"{LOCAL_APP_DATA}\wsl\{UBUNTU_GUID}"),
                    vec![FixturePath::file("ext4.vhdx", 12_884_901_888)],
                ),
                // **故意不声明 Arch 目录里的 vhdx** —— 假文件系统对未声明的路径
                // 一律答"不存在"，所以这条用例测的正是"我们真的去看了它在不在"。
                FixtureDir::new(r"C:\linux\Arch-Linux-current", Vec::new()),
            ],
            ..MachineFixture::default()
        }
    }

    #[test]
    fn both_distributions_are_captured_in_name_order() {
        let fixture = CaptureFixture::build(&two_distributions());
        let file = wsl_file(&fixture);

        assert_eq!(file.distribution.len(), 2, "{:?}", file.distribution);
        assert_eq!(
            file.distribution[0].name, "Arch-Linux-current",
            "按名字排序"
        );
        assert_eq!(file.distribution[0].guid, ARCH_GUID, "GUID 就是子键名");
        assert_eq!(
            file.distribution[0].base_path, r"C:\linux\Arch-Linux-current",
            "**必须记实际路径**，只 glob 默认目录的实现会漏掉它"
        );
        assert_eq!(file.distribution[1].name, "Ubuntu-22.04");
        assert_eq!(file.distribution[1].guid, UBUNTU_GUID);
    }

    /// 默认位置 vs 非标准位置 —— 这个字段的全部意义就在这里。
    #[test]
    fn non_standard_path_distinguishes_the_default_location_from_a_hand_imported_one() {
        let fixture = CaptureFixture::build(&two_distributions());
        let file = wsl_file(&fixture);

        let arch = file
            .distribution
            .iter()
            .find(|row| row.name == "Arch-Linux-current")
            .expect("Arch");
        assert!(
            arch.non_standard_path,
            "手工导入到 C:\\linux 的是非标准位置"
        );

        let ubuntu = file
            .distribution
            .iter()
            .find(|row| row.name == "Ubuntu-22.04")
            .expect("Ubuntu");
        assert_eq!(
            ubuntu.base_path,
            format!(r"{LOCAL_APP_DATA}\wsl\{UBUNTU_GUID}"),
            "expand-sz 的 BasePath 必须展开"
        );
        assert!(!ubuntu.non_standard_path, "默认位置在 %LOCALAPPDATA% 底下");
    }

    /// **真机取证抓出来的形状**：`docker-desktop` 的 `BasePath` 带 `\\?\` 前缀。
    ///
    /// 本机实测：`\\?\C:\Users\Muelsyse\AppData\Local\Docker\wsl\main`，而它的
    /// `ext4.vhdx` **真的存在**（100663296 字节）。第一版实现没剥前缀，于是同一台机器上
    /// 同时报出两个**假问题**：明明在默认位置底下的发行版被算成"非标准位置"，
    /// 而一个真实存在的 vhdx 被算成"找不到"。
    #[test]
    fn a_verbatim_prefix_is_not_part_of_the_path() {
        const DOCKER_GUID: &str = "{00000000-0000-0000-0000-0000000000dd}";
        // **从 `LOCAL_APP_DATA` 拼出来**，不手写第二份：手写的那一份一旦与固定装置里的
        // 环境变量不一致，这条用例就会因为错误的原因失败（第一版就是这么红的 ——
        // 我用的是另一份写死的 `C:\Users\x\…`）。
        let docker_base = format!(r"\\?\{LOCAL_APP_DATA}\Docker\wsl\main");
        let docker_dir = format!(r"{LOCAL_APP_DATA}\Docker\wsl\main");
        let fixture = CaptureFixture::build(&MachineFixture {
            registry: vec![
                lxss_key(),
                distro(
                    DOCKER_GUID,
                    vec![
                        ("DistributionName", RegValue::Sz("docker-desktop".into())),
                        ("BasePath", RegValue::Sz(docker_base.clone())),
                        ("Version", RegValue::Dword(2)),
                        ("State", RegValue::Dword(1)),
                    ],
                ),
            ],
            env: BTreeMap::from([(LOCAL_APP_DATA_VAR.to_owned(), LOCAL_APP_DATA.to_owned())]),
            // 文件声明的是**普通形式** —— 真机上它也是这样存在的。
            dirs: vec![FixtureDir::new(
                &docker_dir,
                vec![FixturePath::file("ext4.vhdx", 100_663_296)],
            )],
            ..MachineFixture::default()
        });
        let file = wsl_file(&fixture);
        let docker = &file.distribution[0];

        assert_eq!(docker.base_path, docker_base, "记录的是注册表原文，不加工");
        assert_eq!(
            docker.vhdx_path,
            format!(r"{docker_base}\ext4.vhdx"),
            "拼出来的路径也要与注册表一致（不能出现双反斜杠）"
        );
        assert!(
            !docker.non_standard_path,
            "它就在 %LOCALAPPDATA%\\Docker\\wsl 底下 —— 带前缀不等于换个位置"
        );
        assert_eq!(
            docker.vhdx_exists,
            Existence::Yes,
            "那个 vhdx 真的在：不剥前缀就会报成找不到"
        );
        assert_eq!(docker.vhdx_bytes, Some(100_663_296));
    }

    /// 前缀剥离本身的边界：只剥开头一次、UNC 那一支原样返回、多字节路径不 panic。
    #[test]
    fn the_verbatim_prefix_stripper_only_touches_the_head() {
        assert_eq!(strip_verbatim_prefix(r"\\?\C:\a"), r"C:\a");
        assert_eq!(strip_verbatim_prefix(r"\??\C:\a"), r"C:\a");
        assert_eq!(strip_verbatim_prefix(r"C:\a"), r"C:\a");
        // 中间的 `\\?\` 不是前缀。
        assert_eq!(strip_verbatim_prefix(r"C:\a\\?\b"), r"C:\a\\?\b");
        // `\\?\UNC\…` 剥完不是合法路径 —— 原样返回（UNC 共享本来也不在 LOCALAPPDATA 底下）。
        assert_eq!(
            strip_verbatim_prefix(r"\\?\UNC\server\share"),
            r"\\?\UNC\server\share"
        );
        // 多字节开头不许 panic（按字节比较，不按 str 下标切）。
        assert_eq!(strip_verbatim_prefix("C:\\用户\\x"), "C:\\用户\\x");
        assert_eq!(strip_verbatim_prefix(""), "");
        assert_eq!(strip_verbatim_prefix(r"\\?\"), "");
    }

    /// 展开不出来的 `BasePath` 按**非标准**处理：说不准不等于标准。
    #[test]
    fn an_unresolvable_base_path_counts_as_non_standard() {
        let fixture = CaptureFixture::build(&MachineFixture {
            registry: vec![
                lxss_key(),
                distro(
                    ARCH_GUID,
                    vec![
                        ("DistributionName", RegValue::Sz("Mystery".into())),
                        ("BasePath", RegValue::Sz(r"%SOMEWHERE%\wsl\Mystery".into())),
                    ],
                ),
            ],
            // 故意不给 `%SOMEWHERE%`，也**不给** `%LOCALAPPDATA%`：
            // 两个"说不准"的来源都必须落到 `true`。
            ..MachineFixture::default()
        });
        let file = wsl_file(&fixture);

        assert_eq!(file.distribution.len(), 1, "{:?}", file.distribution);
        assert!(file.distribution[0].non_standard_path);
        assert_eq!(
            file.distribution[0].base_path, r"%SOMEWHERE%\wsl\Mystery",
            "展开不出来就原样记录 —— 那是可被肉眼发现的证据"
        );
    }

    /// "在 `%LOCALAPPDATA%` 底下"比的是**目录**，不是字符串。
    ///
    /// `…\AppData\LocalFoo` 与 `…\AppData\Local` **不是**同一棵树：朴素字符串前缀比较
    /// 会把前者也算成标准位置 —— 而这个字段唯一的用途就是标出非标准位置，
    /// 所以那是一个"把该报的藏起来"的假阴性。
    #[test]
    fn a_sibling_directory_is_not_under_local_app_data() {
        let build = |base: &str| {
            CaptureFixture::build(&MachineFixture {
                registry: vec![
                    lxss_key(),
                    distro(
                        ARCH_GUID,
                        vec![
                            ("DistributionName", RegValue::Sz("Sibling".into())),
                            ("BasePath", RegValue::Sz(base.to_owned())),
                        ],
                    ),
                ],
                env: BTreeMap::from([(LOCAL_APP_DATA_VAR.to_owned(), LOCAL_APP_DATA.to_owned())]),
                ..MachineFixture::default()
            })
        };

        // 逐字符前缀相同、但不在同一棵子树下。
        let sibling = build(r"C:\Users\Muelsyse\AppData\LocalFoo\wsl");
        assert!(
            wsl_file(&sibling).distribution[0].non_standard_path,
            "`…\\AppData\\LocalFoo` 不在 `…\\AppData\\Local` 底下"
        );

        // 对照组：真的在底下（本机默认位置那种）。
        let inside = build(r"C:\Users\Muelsyse\AppData\Local\wsl\{guid}");
        assert!(!wsl_file(&inside).distribution[0].non_standard_path);

        // 尾巴带分隔符的写法也一样（比较前会去掉尾部反斜杠）。
        let trailing = build(r"C:\Users\Muelsyse\AppData\Local\WSL\");
        assert!(!wsl_file(&trailing).distribution[0].non_standard_path);
    }

    /// vhdx 在 → `yes` 且带字节数；不在 → `no` 且**没有**字节数。
    #[test]
    fn vhdx_existence_and_size_are_both_recorded_and_absent_size_is_omitted() {
        let fixture = CaptureFixture::build(&two_distributions());
        let file = wsl_file(&fixture);

        let ubuntu = file
            .distribution
            .iter()
            .find(|row| row.name == "Ubuntu-22.04")
            .expect("Ubuntu");
        assert_eq!(ubuntu.vhdx_exists, Existence::Yes);
        assert_eq!(ubuntu.vhdx_bytes, Some(12_884_901_888));
        assert_eq!(
            ubuntu.vhdx_path,
            format!(r"{LOCAL_APP_DATA}\wsl\{UBUNTU_GUID}\ext4.vhdx")
        );

        let arch = file
            .distribution
            .iter()
            .find(|row| row.name == "Arch-Linux-current")
            .expect("Arch");
        assert_eq!(arch.vhdx_exists, Existence::No);
        assert_eq!(arch.vhdx_bytes, None, "不在的时候不许编一个大小");
        assert_eq!(arch.vhdx_path, r"C:\linux\Arch-Linux-current\ext4.vhdx");

        // `None` 的字节数不许在文件里变成一个键（TOML 没有 null）。
        let text = fixture.files(&[Section::Wsl], AT);
        let toml_text = text.get("wsl.toml").expect("wsl.toml");
        assert_eq!(
            toml_text.matches("vhdx_bytes").count(),
            1,
            "只有存在的那个发行版才写字节数：\n{toml_text}"
        );
    }

    /// `Version` / `State` 是 DWORD；**取不到就是 `None`**，不许猜。
    #[test]
    fn version_and_state_come_from_dwords_and_missing_ones_stay_none() {
        let fixture = CaptureFixture::build(&two_distributions());
        let file = wsl_file(&fixture);

        let ubuntu = file
            .distribution
            .iter()
            .find(|row| row.name == "Ubuntu-22.04")
            .expect("Ubuntu");
        assert_eq!(ubuntu.wsl_version, Some(2));
        assert_eq!(ubuntu.state, Some(1));

        let arch = file
            .distribution
            .iter()
            .find(|row| row.name == "Arch-Linux-current")
            .expect("Arch");
        assert_eq!(arch.wsl_version, Some(1));
        assert_eq!(arch.state, None, "没有 State 值就必须是 None，不许猜 1");
    }

    /// 没有 Lxss 键 = 这台机器上没有 WSL：空文件，**不是错误、不 panic**。
    #[test]
    fn a_machine_without_wsl_yields_an_empty_file() {
        let fixture = CaptureFixture::build(&MachineFixture::default());
        let file = wsl_file(&fixture);
        assert!(file.distribution.is_empty());
        assert_eq!(file.schema_version, crate::capture::SCHEMA_VERSION);
        assert_eq!(file.captured_at, AT);
    }

    /// Lxss 键存在但一个发行版都没有（装过又卸干净了）—— 同样是空文件。
    #[test]
    fn an_empty_lxss_key_is_not_an_error_either() {
        let fixture = CaptureFixture::build(&MachineFixture {
            registry: vec![lxss_key()],
            ..MachineFixture::default()
        });
        assert!(wsl_file(&fixture).distribution.is_empty());
    }

    /// 没有 `DistributionName` 的子键被跳过 —— 一条没有名字的记录既认不出来也用不上。
    #[test]
    fn a_distribution_without_a_name_is_skipped() {
        let fixture = CaptureFixture::build(&MachineFixture {
            registry: vec![
                lxss_key(),
                distro(
                    ARCH_GUID,
                    vec![("BasePath", RegValue::Sz(r"C:\linux\unnamed".into()))],
                ),
            ],
            ..MachineFixture::default()
        });
        assert!(
            wsl_file(&fixture).distribution.is_empty(),
            "没有名字就没有记录"
        );
    }

    /// 同一台假机器跑两次，**逐字节相同**。
    #[test]
    fn capturing_twice_is_byte_identical() {
        let fixture = CaptureFixture::build(&two_distributions());
        assert_eq!(
            fixture.files(&[Section::Wsl], AT),
            fixture.files(&[Section::Wsl], AT)
        );
    }
}
