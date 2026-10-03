//! `configs.toml` —— 配置文件的**指纹**与 `[git]` 身份（决策 176–184）。
//!
//! # 这一节回答两个问题
//!
//! 1. **这台机器上有哪些配置文件、它们的内容指纹是什么**（决策 177：只哈希、
//!    **绝不存内容**）；
//! 2. **git 的身份在哪一层**（决策 183：system → global，global 覆盖 system）。
//!
//! # 存不进快照的东西必须**说出来**（决策 179）
//!
//! 候选清单里**存在**、却没写进 `configs.toml` 的每一样，都要在 `skipped.toml` 里
//! 有一条**具体**理由 —— 静默跳过是 bug，它让用户以为"都备份好了"。
//! 反过来，**"没看"不是"跳过"**：`authorized_keys`、`id_rsa.pub`、JetBrains 目录里
//! 的其它文件都不在候选集里，所以它们一个字都不该出现。
//!
//! # 四个 env 变量，而且读的是**进程**那一份
//!
//! 候选路径只由 `USERPROFILE` / `APPDATA` / `LOCALAPPDATA` 拼出来，**注册表一个键都不读**
//! （那是 `env` 那一节的事）。
//!
//! 而且必须读**进程环境**（[`DetectContext::process_var`]）：这三个名字是系统按登录会话
//! 派生的量，**注册表里通常根本没有它们**（本机实测：`HKCU\Environment` 14 个值、
//! 机器级 19 个值，两处都没有这三个名字）。用持久环境去读，真机上会一行候选都拼不出来 ——
//! 整节变成空的，而固定装置那一侧（`[env]` 表）又都写得好好的。
//!
//! # 内容级的凭据判据只有一份实现（决策 178）
//!
//! 复用 [`secrets::scan_content`]，**不写第二个扫描器**：两份实现迟早会对同一份内容
//! 给出不同答案，而这里的答案决定"要不要把这份文件写进快照"。

use std::path::Path;

use tuoen_platform::ReadOutcome;

use crate::detect::DetectContext;

use super::super::files::{ConfigRow, ConfigsFile, GitFacts, SkipEntry};
use super::super::{Section, secrets};
use super::{COMMAND_TIMEOUT, Command};

/// 读一个配置文件的上限（决策 176/179）：超过它就是 `too-large`。
///
/// **先看大小、再读**（真实实现读元数据）：256 KiB 的填充读进内存只为了算一个哈希，
/// 而 `~/.m2/repository` 那种东西是以 GB 计的。
pub(crate) const MAX_CONFIG_BYTES: u64 = 256 * 1024;

/// 普通配置文件候选：`(env 变量, 相对后缀, kind, layer)`（决策 176 的 kind 词表）。
///
/// `layer` 只在**有层级概念**的文件上出键：git 的两层（system / global）是唯一的例子。
/// `~/.gitconfig` 是 git 的全局层，所以它有 `layer = "global"`；系统层那一份
/// （`C:\Program Files\Git\etc\gitconfig`）**只能从 git 自己的回答里拿到路径**
/// （决策 183 的 `--show-origin`），所以它在 [`collect_git`] 里补。
const FILE_CANDIDATES: [(&str, &str, &str, Option<&str>); 7] = [
    ("USERPROFILE", r"\.gitconfig", "git", Some("global")),
    ("USERPROFILE", r"\.npmrc", "npm", None),
    ("USERPROFILE", r"\.m2\settings.xml", "maven", None),
    ("USERPROFILE", r"\.docker\config.json", "docker", None),
    ("USERPROFILE", r"\.ssh\config", "ssh", None),
    ("USERPROFILE", r"\.wslconfig", "wsl", None),
    ("APPDATA", r"\Code\User\settings.json", "vscode", None),
];

/// 决策 180 的缓存目录表：**只按路径前缀命中**，命中**且存在**才是一条 `cache-directory`。
///
/// 它们**只在跳过清单里**：不进 `configs.toml`，也**绝不走进去、不报体积**
/// （本机六处约 8 GB，数一遍就是几十秒，而那个数字没有任何用处）。
const CACHE_DIRECTORIES: [(&str, &str); 6] = [
    ("LOCALAPPDATA", r"\pnpm\store"),
    ("LOCALAPPDATA", r"\npm-cache"),
    ("LOCALAPPDATA", r"\pip\Cache"),
    ("LOCALAPPDATA", r"\uv\cache"),
    ("USERPROFILE", r"\.cache\codex-runtimes"),
    ("USERPROFILE", r"\.m2\repository"),
];

/// JetBrains 的根：`%APPDATA%\JetBrains`（决策 182：**只 list 这一层**）。
const JETBRAINS_ROOT: (&str, &str) = ("APPDATA", r"\JetBrains");

/// 产品目录里的密钥材料 → 跳过项 kind（决策 182）。
const JETBRAINS_SECRETS: [(&str, &str); 3] = [
    ("c.kdbx", CREDENTIAL_DATABASE),
    ("c.pwd", CREDENTIAL_DATABASE),
    ("idea.key", PRIVATE_KEY_FILE),
];

/// `~/.ssh` 的目录名（决策 181：私钥靠 `list_dir` 发现，不是靠猜文件名表）。
const SSH_DIR: (&str, &str) = ("USERPROFILE", r"\.ssh");

/// git 的两层（决策 183）。`--show-origin` 是**唯一**能同时给出"这条键来自哪个文件"的开关。
const GIT_SYSTEM: Command = Command {
    program: "git.exe",
    args: &["config", "--system", "--list", "--show-origin"],
};
const GIT_GLOBAL: Command = Command {
    program: "git.exe",
    args: &["config", "--global", "--list", "--show-origin"],
};

/// 跳过项的 kind（决策 179 的稳定 slug 表）。
const UNREADABLE: &str = "unreadable";
const TOO_LARGE: &str = "too-large";
const BINARY: &str = "binary";
const CONTAINS_CREDENTIAL_SHAPE: &str = "contains-credential-shape";
const PRIVATE_KEY_FILE: &str = "private-key-file";
const HOST_KEYS_NOT_CAPTURED: &str = "host-keys-not-captured";
const CREDENTIAL_DATABASE: &str = "credential-database";
const CACHE_DIRECTORY: &str = "cache-directory";

/// 跳过项的 `scope`：这些跳过项都是**文件**（`env` 那一节用的是 `user` / `machine`）。
const SCOPE_FILE: &str = "file";

/// 一条跳过项的中文理由。**全部具体**（决策 179：写"读不到"等于没写），
/// 而且**不含任何材料**（决策 152 的注记：中文散文只进人类输出，且不抄值）。
const UNREADABLE_REASON: &str = "文件存在但读不出来（权限、被占用，或者它其实是个目录）";
const BINARY_REASON: &str = "内容不是 UTF-8 文本（含 NUL 字节），没法判断里面有没有凭据";
const PRIVATE_KEY_REASON: &str = "私钥文件不进快照（快照要提交进仓库）";
const HOST_KEYS_REASON: &str = "known_hosts 记的是内网主机名，快照要提交进仓库";
const CREDENTIAL_DATABASE_REASON: &str = "凭据数据库（加密的密码库）不进快照";
const CACHE_REASON: &str = "缓存不是状态：重建它不需要它";

/// 采集配置文件与 git 身份。
///
/// 返回 `(configs.toml, 跳过项)`。跳过项由调用方汇总后统一写进 `skipped.toml`
/// （它是**跨 section** 的一句话，所以不在这一层落盘）。
pub(crate) fn collect_configs(
    ctx: &DetectContext<'_>,
    captured_at: &str,
) -> (ConfigsFile, Vec<SkipEntry>) {
    let mut file = ConfigsFile::new(captured_at);
    let mut collected = Collected::default();

    // ① 普通配置文件候选。
    for (env_name, suffix, kind, layer) in FILE_CANDIDATES {
        let Some(path) = rooted(ctx, env_name, suffix) else {
            continue;
        };
        read_file(ctx, &path, kind, layer, &mut collected);
    }

    // ② git：两个配置文件 + 身份。
    file.git = Some(collect_git(ctx, &mut collected));

    // ③ `~/.ssh`、④ JetBrains、⑤ 缓存目录。
    collect_ssh(ctx, &mut collected);
    collect_jetbrains(ctx, &mut collected);
    collect_caches(ctx, &mut collected);

    // 行的顺序按路径 —— 确定性是幂等性的一部分（决策 174 的同一条规矩）。
    collected.rows.sort_by(|a, b| a.path.cmp(&b.path));
    file.config = collected.rows;
    (file, collected.skips)
}

/// 采集的中间结果：`configs.toml` 的行与 `skipped.toml` 的条目。
///
/// 两个集合是**互斥子集**（决策 184）：一行要么 `captured = true`，要么带着
/// `skip_reason`，不可能两头都不占。唯一的例外是 JetBrains 的目录标记行
/// （`captured = true` 而没有 `bytes` / `content_hash` —— 目录不是文件）。
#[derive(Debug, Default)]
struct Collected {
    rows: Vec<ConfigRow>,
    skips: Vec<SkipEntry>,
}

impl Collected {
    /// 一行"读到了、算出了哈希"。
    fn captured(&mut self, path: &str, kind: &str, layer: Option<&str>, bytes: &[u8]) {
        self.rows.push(ConfigRow {
            path: path.to_owned(),
            kind: kind.to_owned(),
            layer: layer.map(str::to_owned),
            captured: true,
            bytes: Some(bytes.len() as u64),
            // 信任指纹用的就是同一个 sha256 实现（`tuoen_download`），
            // 不在这里再写一遍 —— 两份实现迟早会对同一份内容给出两个哈希。
            content_hash: Some(format!("sha256:{}", tuoen_download::sha256_hex(bytes))),
            skip_reason: None,
        });
    }

    /// 一行"目录标记"（决策 182 的唯一例外）。
    fn marker(&mut self, path: &str, kind: &str) {
        self.rows.push(ConfigRow {
            path: path.to_owned(),
            kind: kind.to_owned(),
            layer: None,
            captured: true,
            bytes: None,
            content_hash: None,
            skip_reason: None,
        });
    }

    /// 一行"没写进快照" + 它在 `skipped.toml` 里的那一条。
    ///
    /// **跳过的行不出 `bytes` 也不出 `content_hash`**：我们没打算把它的内容带出去，
    /// 而一个哈希就是"我读过它"的证据 —— 凭据那一条尤其不能有。
    fn skipped(&mut self, path: &str, kind: &str, layer: Option<&str>, slug: &str, reason: &str) {
        self.rows.push(ConfigRow {
            path: path.to_owned(),
            kind: kind.to_owned(),
            layer: layer.map(str::to_owned),
            captured: false,
            bytes: None,
            content_hash: None,
            skip_reason: Some(slug.to_owned()),
        });
        self.skips.push(SkipEntry {
            section: Section::Configs.as_str().to_owned(),
            scope: Some(SCOPE_FILE.to_owned()),
            name: path.to_owned(),
            kind: slug.to_owned(),
            reason: reason.to_owned(),
        });
    }

    /// 这个文件已经有一条行了吗（按 Windows 的路径语义比较）。
    ///
    /// 用来避免**同一份文件出两行**：git 的全局配置几乎总是 `~/.gitconfig`
    /// （它已经是 env 候选了），而 git 报出来的路径可能写成正斜杠。
    fn has_path(&self, path: &str) -> bool {
        let wanted = normalized(path);
        self.rows.iter().any(|row| normalized(&row.path) == wanted)
    }
}

/// 读一个文件候选并把结论写进 `collected`。
///
/// 四态映射（决策 179 + 决策 186）：
///
/// | `read` 说 | 结果 |
/// |---|---|
/// | `Bytes` | 扫内容 → 干净就**捕获**（`bytes` + `sha256:` 哈希），形似凭据就跳过 |
/// | `TooLarge { size }` | 跳过，理由里**写出真实字节数**（"超过 limit"不是理由） |
/// | `Unreadable` | 跳过 |
/// | `NotFound` | **什么都不出** —— 不存在的东西不是"被跳过的" |
fn read_file(
    ctx: &DetectContext<'_>,
    path: &str,
    kind: &str,
    layer: Option<&str>,
    collected: &mut Collected,
) {
    match ctx.fs.read(Path::new(path), MAX_CONFIG_BYTES) {
        ReadOutcome::NotFound => {}
        ReadOutcome::Unreadable { .. } => {
            collected.skipped(path, kind, layer, UNREADABLE, UNREADABLE_REASON);
        }
        ReadOutcome::TooLarge { size } => {
            let reason = format!("这个文件 {size} 字节，超过 {MAX_CONFIG_BYTES} 字节的上限");
            collected.skipped(path, kind, layer, TOO_LARGE, &reason);
        }
        ReadOutcome::Bytes(bytes) => {
            if is_binary(&bytes) {
                collected.skipped(path, kind, layer, BINARY, BINARY_REASON);
                return;
            }
            // 走到这里一定是合法 UTF-8（`is_binary` 已经排除了非法的那一半）。
            let text = String::from_utf8_lossy(&bytes);
            if let Some(detection) = secrets::scan_content(&text) {
                // **理由来自扫描器**（决策 178：一个扫描器两个调用方），
                // 它只说形状的名字与键名提示，不含任何材料。
                collected.skipped(
                    path,
                    kind,
                    layer,
                    CONTAINS_CREDENTIAL_SHAPE,
                    &detection.reason(),
                );
                return;
            }
            collected.captured(path, kind, layer, &bytes);
        }
    }
}

/// 内容像二进制吗。
///
/// 判据是 **NUL 字节或非法 UTF-8**，不是扩展名（决策 179 的注记：`content` 是 `String`，
/// 非法 UTF-8 表达不出来，所以固定装置用 NUL 表达"二进制"）。
/// 二进制文件读不出文本 → 扫不了凭据形状 → 只能跳过；而**猜**它没有凭据是拿安全换安静。
fn is_binary(bytes: &[u8]) -> bool {
    bytes.contains(&0) || std::str::from_utf8(bytes).is_err()
}

/// 拼一个候选路径：`<env 值><后缀>`。根读不到就返回 `None`（拼不出来就不做）。
fn rooted(ctx: &DetectContext<'_>, env_name: &str, suffix: &str) -> Option<String> {
    let root = ctx.process_var(env_name)?;
    let root = root.trim_end_matches(['\\', '/']);
    if root.is_empty() {
        return None;
    }
    Some(format!("{root}{suffix}"))
}

/// 子路径拼接（候选表的根与名字都不会带尾部分隔符）。
fn join(dir: &str, name: &str) -> String {
    format!("{}\\{name}", dir.trim_end_matches(['\\', '/']))
}

/// 比较用的路径归一化：`/` → `\`、去尾部分隔符、小写。
///
/// 与 `tuoen_platform::fixture::normalize_path` 同一套语义（Windows 的路径比较本来就是
/// 大小写与分隔符不敏感的）。**只用于比较**，绝不写进快照 —— 快照里存的是
/// "工具说了什么"（决策 183 的原样口径）。
fn normalized(path: &str) -> String {
    path.replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}

// ─────────────────────────────────────────────────────────────────────────────
// ~/.ssh（决策 181）
// ─────────────────────────────────────────────────────────────────────────────

/// `~/.ssh`：`config` 捕获；私钥形状与 `known_hosts` 各一条跳过项；其余**不是候选**。
fn collect_ssh(ctx: &DetectContext<'_>, collected: &mut Collected) {
    let Some(dir) = rooted(ctx, SSH_DIR.0, SSH_DIR.1) else {
        return;
    };

    for entry in ctx.fs.list_dir(Path::new(&dir)) {
        if entry.is_dir {
            continue;
        }
        let full = join(&dir, &entry.name);
        let lower = entry.name.to_ascii_lowercase();
        if lower == "config" {
            // 票据点名要捕获它：`Host *` / `ProxyJump` 这类东西是**状态**，不是秘密。
            read_file(ctx, &full, "ssh", None, collected);
        } else if lower == "known_hosts" {
            collected.skipped(&full, "ssh", None, HOST_KEYS_NOT_CAPTURED, HOST_KEYS_REASON);
        } else if is_private_key_shape(&lower) {
            collected.skipped(&full, "ssh", None, PRIVATE_KEY_FILE, PRIVATE_KEY_REASON);
        }
        // 其余（`authorized_keys`、`id_rsa.pub`、`config.old`…）不在候选集里 ——
        // "没看"不是"跳过"，把它们写进清单等于虚报。
    }
}

/// 私钥形状（决策 181）：`id_*`（**不是** `.pub`）/ `*.pem` / `*.ppk` / `*.key`。
///
/// 判据是**名字的形状**，不是内容：`note.key` 里写着"not really a key"也照样跳过 ——
/// 为了放行一个假文件而去读每一个候选的内容，代价与风险都比多跳过一个文件大。
fn is_private_key_shape(lower_name: &str) -> bool {
    if lower_name.ends_with(".pub") {
        return false;
    }
    lower_name.starts_with("id_")
        || lower_name.ends_with(".pem")
        || lower_name.ends_with(".ppk")
        || lower_name.ends_with(".key")
}

// ─────────────────────────────────────────────────────────────────────────────
// JetBrains（决策 182）
// ─────────────────────────────────────────────────────────────────────────────

/// `%APPDATA%\JetBrains\<Product><Version>`：目录本身一条**标记行**，三个密钥材料各一条跳过项。
///
/// **只 list 这一层**：JetBrains 的配置目录里有几百个文件，逐个读会把它变成一个
/// "顺便备份 IDE 设置"的功能 —— 那不是这一票的事。
fn collect_jetbrains(ctx: &DetectContext<'_>, collected: &mut Collected) {
    let Some(root) = rooted(ctx, JETBRAINS_ROOT.0, JETBRAINS_ROOT.1) else {
        return;
    };

    for entry in ctx.fs.list_dir(Path::new(&root)) {
        if !entry.is_dir || !is_product_directory(&entry.name) {
            continue;
        }
        let product = join(&root, &entry.name);
        collected.marker(&product, "jetbrains");

        for name in secret_names_in(ctx, &product) {
            let Some((_, kind)) = JETBRAINS_SECRETS
                .iter()
                .find(|(candidate, _)| candidate.eq_ignore_ascii_case(&name))
            else {
                continue;
            };
            let reason = if *kind == CREDENTIAL_DATABASE {
                CREDENTIAL_DATABASE_REASON
            } else {
                PRIVATE_KEY_REASON
            };
            collected.skipped(&join(&product, &name), "jetbrains", None, kind, reason);
        }
    }
}

/// 产品目录里**存在**的密钥材料文件名（大小写不敏感）。
fn secret_names_in(ctx: &DetectContext<'_>, product: &str) -> Vec<String> {
    ctx.fs
        .list_dir(Path::new(product))
        .into_iter()
        .filter(|entry| !entry.is_dir)
        .map(|entry| entry.name)
        .filter(|name| {
            JETBRAINS_SECRETS
                .iter()
                .any(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        })
        .collect()
}

/// 目录名是不是 `<Product><Version>`（决策 182 只认这一种）。
///
/// 判据 = 名字以"数字与点"结尾，而且**前面还有产品名**：
/// `IntelliJIdea2026.1` 是，`acp-agents` / `consentOptions` 不是。
/// 不用"以大写字母开头"之类的形状猜：JetBrains 的产品名会变，而版本尾巴不会。
fn is_product_directory(name: &str) -> bool {
    let Some((at, ch)) = name
        .char_indices()
        .rev()
        .find(|(_, c)| !(c.is_ascii_digit() || *c == '.'))
    else {
        return false;
    };
    let tail = &name[at + ch.len_utf8()..];
    !tail.is_empty()
        && tail.starts_with(|c: char| c.is_ascii_digit())
        && tail.chars().all(|c| c.is_ascii_digit() || c == '.')
}

// ─────────────────────────────────────────────────────────────────────────────
// 缓存目录（决策 180）
// ─────────────────────────────────────────────────────────────────────────────

/// 命中前缀**且存在**的缓存目录 → 一条 `cache-directory` 跳过项。
///
/// 判据只看 `inspect`（存不存在）：**绝不 `read`、绝不 `list_dir`、绝不报体积**。
fn collect_caches(ctx: &DetectContext<'_>, collected: &mut Collected) {
    for (env_name, suffix) in CACHE_DIRECTORIES {
        let Some(path) = rooted(ctx, env_name, suffix) else {
            continue;
        };
        if !ctx.fs.inspect(Path::new(&path)).exists {
            continue;
        }
        collected.skips.push(SkipEntry {
            section: Section::Configs.as_str().to_owned(),
            scope: Some(SCOPE_FILE.to_owned()),
            name: path,
            kind: CACHE_DIRECTORY.to_owned(),
            reason: CACHE_REASON.to_owned(),
        });
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// git（决策 183）
// ─────────────────────────────────────────────────────────────────────────────

/// git 的一层。
#[derive(Debug, Default)]
struct GitLayer {
    /// `--show-origin` 第一行的 `file:` 后面那一段，**原样**（不归一化分隔符）。
    config: Option<String>,
    /// `user.name` / `user.email`（这一层里没有就是 `None`）。
    name: Option<String>,
    email: Option<String>,
    /// 这条命令跑成功了吗 —— `missing` 与 `unknown` 的唯一区别就在这里。
    answered: bool,
}

/// 采集 `[git]` 表，并把两个 git 配置文件也作为候选读一遍。
///
/// **`[git]` 表永远存在**（决策 183）：省略整张表会让"没问"与"没装 git"长得一样。
fn collect_git(ctx: &DetectContext<'_>, collected: &mut Collected) -> GitFacts {
    let system = git_layer(ctx, &GIT_SYSTEM);
    let global = git_layer(ctx, &GIT_GLOBAL);

    // 两个配置文件本身：**路径来自 git 自己的回答**（`file:` 后面那一段，原样）。
    // 已经是 env 候选的那一份（Windows 上就是 `~/.gitconfig`）不重复出行。
    for (layer, name) in [(&system, "system"), (&global, "global")] {
        let Some(path) = layer.config.as_deref() else {
            continue;
        };
        if collected.has_path(path) {
            continue;
        }
        read_file(ctx, path, "git", Some(name), collected);
    }

    let (identity_source, user_name, user_email) = identity(&system, &global);
    GitFacts {
        identity_source: identity_source.to_owned(),
        system_config: system.config.clone(),
        global_config: global.config.clone(),
        user_name,
        user_email,
    }
}

/// 解析一层 `git config --list --show-origin` 的输出。
///
/// 每行是 `<来源>\t<键>=<值>`：来源可能是 `file:C:/Program Files/Git/etc/gitconfig`
/// （**含空格**，所以只能按第一个 TAB 切，不能用 `^\S*\t`），
/// 而值里可能有 `=`（所以键值只能按**第一个** `=` 切）。
fn git_layer(ctx: &DetectContext<'_>, command: &Command) -> GitLayer {
    let mut layer = GitLayer::default();
    let Some(text) = command.run_text(ctx, COMMAND_TIMEOUT) else {
        // 没启动 / 超时 / 非零退出：这一层**没回答**。
        return layer;
    };
    layer.answered = true;

    for line in text.lines() {
        let Some((origin, key_value)) = line.split_once('\t') else {
            continue;
        };
        if layer.config.is_none()
            && let Some(path) = origin.strip_prefix("file:")
        {
            layer.config = Some(path.to_owned());
        }
        let Some((key, value)) = key_value.split_once('=') else {
            continue;
        };
        match key {
            "user.name" => layer.name = Some(value.to_owned()),
            "user.email" => layer.email = Some(value.to_owned()),
            _ => {}
        }
    }
    layer
}

/// 身份来自哪一层（决策 183）。
///
/// * **global 覆盖 system**：只要全局级说了话（名字或邮箱任一个），身份就来自全局级；
/// * 两层都没有身份 **且两层都回答了** → `missing`（"缺失就明确说缺失"）；
/// * 有一层没回答 → `unknown` —— 我们**问不了**，而"问不了"与"没有"是两句话。
fn identity(
    system: &GitLayer,
    global: &GitLayer,
) -> (&'static str, Option<String>, Option<String>) {
    let name = global.name.clone().or_else(|| system.name.clone());
    let email = global.email.clone().or_else(|| system.email.clone());

    if global.name.is_some() || global.email.is_some() {
        return ("global", name, email);
    }
    if system.name.is_some() || system.email.is_some() {
        return ("system", name, email);
    }
    if system.answered && global.answered {
        return ("missing", None, None);
    }
    ("unknown", None, None)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tuoen_platform::fixture::{FixtureDir, FixturePath, FixtureProcess, MachineFixture};

    use super::*;
    use crate::detect::test_support::DetectFixture;

    const AT: &str = "2026-10-02T12:00:00Z";
    const HOME: &str = r"C:\Users\dev";
    const APPDATA: &str = r"C:\Users\dev\AppData\Roaming";
    const LOCALAPPDATA: &str = r"C:\Users\dev\AppData\Local";

    /// 内容一律**单行 + `\n` 转义**：工作树的行尾（`core.autocrlf`）不该影响任何断言。
    const GITCONFIG: &str =
        "[credential]\n\thelper = manager\n[http]\n\tproxy = http://proxy.example:8080\n";
    const NPMRC: &str = "allow-scripts=true\nregistry=https://registry.example/\n";
    const SETTINGS: &str = "<?xml version=\"1.0\"?>\n<settings>\n  <localRepository>C:/Users/dev/.m2/repository</localRepository>\n</settings>\n";
    const DOCKER: &str = "{\n  \"auths\": {},\n  \"credsStore\": \"desktop\"\n}\n";
    const SSH_CONFIG: &str = "Host *\n  ServerAliveInterval 60\n";
    const KNOWN_HOSTS: &str = "intranet-build.example ssh-ed25519 AAAAC3NzaC1lZDI1\n";
    const VSCODE: &str = "{\n  \"editor.fontSize\": 14\n}\n";
    const WSLCONFIG: &str = "[wsl2]\nmemory=8GB\n";
    /// git 的系统级配置（路径由 git 自己报，见 [`GIT_SYSTEM_OUT`]）。
    const SYSTEM_GITCONFIG: &str = "[core]\n\tautocrlf = true\n";

    const GIT_SYSTEM_OUT: &str = "file:C:/Program Files/Git/etc/gitconfig\tcore.autocrlf=true\n";
    const GIT_GLOBAL_OUT: &str = "file:C:/Users/dev/.gitconfig\tcredential.helper=manager\n";

    /// 假 token：**拼出来**，源码里不留字面量（本仓库被 GH013 拦过一次）。
    fn token() -> String {
        format!("{}0000000000000000", concat!("glpat", "-"))
    }

    fn process(program: &str, args: &[&str], stdout: &str) -> FixtureProcess {
        FixtureProcess {
            program: program.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            stdout: stdout.to_owned(),
            stderr: String::new(),
            exit_code: Some(0),
            timed_out: false,
        }
    }

    fn git_processes(system: &str, global: &str) -> Vec<FixtureProcess> {
        vec![
            process(
                "git.exe",
                &["config", "--system", "--list", "--show-origin"],
                system,
            ),
            process(
                "git.exe",
                &["config", "--global", "--list", "--show-origin"],
                global,
            ),
        ]
    }

    /// 一台"候选文件都在、git 两层都回答了"的机器。
    fn machine() -> MachineFixture {
        MachineFixture {
            env: BTreeMap::from([
                ("USERPROFILE".to_owned(), HOME.to_owned()),
                ("APPDATA".to_owned(), APPDATA.to_owned()),
                ("LOCALAPPDATA".to_owned(), LOCALAPPDATA.to_owned()),
                ("Path".to_owned(), r"C:\Program Files\Git\cmd".to_owned()),
            ]),
            paths: Vec::new(),
            dirs: vec![
                FixtureDir::new(
                    HOME,
                    vec![
                        FixturePath::file_with_content(".gitconfig", GITCONFIG),
                        FixturePath::file_with_content(".npmrc", NPMRC),
                        FixturePath::file_with_content(".wslconfig", WSLCONFIG),
                    ],
                ),
                FixtureDir::new(
                    &format!("{HOME}\\.m2"),
                    vec![FixturePath::file_with_content("settings.xml", SETTINGS)],
                ),
                FixtureDir::new(
                    &format!("{HOME}\\.docker"),
                    vec![FixturePath::file_with_content("config.json", DOCKER)],
                ),
                FixtureDir::new(
                    &format!("{HOME}\\.ssh"),
                    vec![
                        FixturePath::file_with_content("config", SSH_CONFIG),
                        FixturePath::file("id_ed25519", 464),
                        FixturePath::file("id_ed25519.pub", 43),
                        FixturePath::file_with_content("known_hosts", KNOWN_HOSTS),
                        FixturePath::file("work.pem", 1_704),
                    ],
                ),
                FixtureDir::new(
                    &format!("{APPDATA}\\Code\\User"),
                    vec![FixturePath::file_with_content("settings.json", VSCODE)],
                ),
                FixtureDir::new(
                    &format!("{APPDATA}\\JetBrains"),
                    vec![
                        FixturePath::dir("IntelliJIdea2026.1"),
                        FixturePath::dir("acp-agents"),
                    ],
                ),
                FixtureDir::new(
                    &format!("{APPDATA}\\JetBrains\\IntelliJIdea2026.1"),
                    vec![
                        FixturePath::file("c.kdbx", 2_682),
                        FixturePath::file("idea.key", 28_232),
                        FixturePath::dir("consentOptions"),
                    ],
                ),
                FixtureDir::new(&format!("{LOCALAPPDATA}\\npm-cache"), Vec::new()),
                FixtureDir::new(
                    r"C:\Program Files\Git\etc",
                    vec![FixturePath::file_with_content(
                        "gitconfig",
                        SYSTEM_GITCONFIG,
                    )],
                ),
            ],
            registry: Vec::new(),
            processes: git_processes(GIT_SYSTEM_OUT, GIT_GLOBAL_OUT),
            managed: Vec::new(),
        }
    }

    fn collect(description: &MachineFixture) -> (ConfigsFile, Vec<SkipEntry>) {
        collect_configs(&DetectFixture::build(description).context(), AT)
    }

    fn row<'a>(configs: &'a ConfigsFile, path: &str) -> Option<&'a ConfigRow> {
        let wanted = normalized(path);
        configs
            .config
            .iter()
            .find(|row| normalized(&row.path) == wanted)
    }

    fn skip<'a>(skips: &'a [SkipEntry], path: &str) -> Option<&'a SkipEntry> {
        let wanted = normalized(path);
        skips.iter().find(|entry| normalized(&entry.name) == wanted)
    }

    fn hash_of(content: &str) -> String {
        format!("sha256:{}", tuoen_download::sha256_hex(content.as_bytes()))
    }

    #[test]
    fn every_present_candidate_gets_a_row_with_its_kind_layer_bytes_and_hash() {
        let (configs, skips) = collect(&machine());

        let cases: [(&str, &str, Option<&str>, &str); 7] = [
            (r"C:\Users\dev\.gitconfig", "git", Some("global"), GITCONFIG),
            (r"C:\Users\dev\.npmrc", "npm", None, NPMRC),
            (r"C:\Users\dev\.m2\settings.xml", "maven", None, SETTINGS),
            (r"C:\Users\dev\.docker\config.json", "docker", None, DOCKER),
            (r"C:\Users\dev\.ssh\config", "ssh", None, SSH_CONFIG),
            (r"C:\Users\dev\.wslconfig", "wsl", None, WSLCONFIG),
            (
                r"C:\Users\dev\AppData\Roaming\Code\User\settings.json",
                "vscode",
                None,
                VSCODE,
            ),
        ];

        for (path, kind, layer, content) in cases {
            let row = row(&configs, path).unwrap_or_else(|| panic!("{path} 必须有一行"));
            assert_eq!(row.kind, kind, "{path}");
            assert_eq!(row.layer.as_deref(), layer, "{path}");
            assert!(row.captured, "{path}");
            assert_eq!(row.bytes, Some(content.len() as u64), "{path}");
            assert_eq!(
                row.content_hash.as_deref(),
                Some(hash_of(content).as_str()),
                "{path}"
            );
            assert_eq!(row.skip_reason, None, "{path}");
            assert!(skip(&skips, path).is_none(), "{path} 不该出现在跳过清单里");
        }

        // 行按路径排序（确定性是幂等性的一部分）。
        assert!(
            configs
                .config
                .windows(2)
                .all(|pair| pair[0].path <= pair[1].path),
            "行必须按 path 排序"
        );
    }

    /// 不存在的东西**什么都不出** —— 不是"跳过项"，也不是一行 `captured = false`。
    #[test]
    fn a_file_that_does_not_exist_produces_neither_a_row_nor_a_skip() {
        let mut description = machine();
        description
            .dirs
            .retain(|dir| dir.path != format!("{HOME}\\.docker"));
        let (configs, skips) = collect(&description);

        assert!(row(&configs, r"C:\Users\dev\.docker\config.json").is_none());
        assert!(skip(&skips, r"C:\Users\dev\.docker\config.json").is_none());
        // 其余照旧。
        assert!(row(&configs, r"C:\Users\dev\.npmrc").is_some());
    }

    /// 决策 178 的安全红线：形似凭据的文件被跳过，而且**材料一个片段都不进快照**。
    #[test]
    fn a_credential_shaped_file_is_skipped_and_no_fragment_of_it_leaves_the_machine() {
        let secret = token();
        let mut description = machine();
        let content = format!(
            "<settings>\n  <servers>\n    <server>\n      <value>{secret}</value>\n    </server>\n  </servers>\n</settings>\n"
        );
        description
            .dirs
            .retain(|dir| dir.path != format!("{HOME}\\.m2"));
        description.dirs.push(FixtureDir::new(
            &format!("{HOME}\\.m2"),
            vec![FixturePath::file_with_content("settings.xml", &content)],
        ));

        let (configs, skips) = collect(&description);
        let path = r"C:\Users\dev\.m2\settings.xml";
        let row = row(&configs, path).expect("被跳过的文件**也必须在清单里**");
        assert!(!row.captured);
        assert_eq!(row.skip_reason.as_deref(), Some(CONTAINS_CREDENTIAL_SHAPE));
        assert!(row.bytes.is_none(), "跳过的行不许假装读过");
        assert!(row.content_hash.is_none());

        let entry = skip(&skips, path).expect("必须在跳过清单里（决策 179）");
        assert_eq!(entry.kind, CONTAINS_CREDENTIAL_SHAPE);
        assert_eq!(entry.section, "configs");
        assert!(!entry.reason.trim().is_empty());

        // **材料一个 8 字符窗口都不许出现**（渲染出来的每一份文件都查）。
        // 走真实的捕获路径渲染，而不是手搭一个 bundle —— 那样才测得到"落盘的东西"。
        let bundle = crate::capture::test_support::CaptureFixture::build(&description)
            .capture(&[Section::Configs], AT);
        let rendered = crate::capture::render(&bundle).expect("渲染");
        assert!(
            rendered.len() >= 3,
            "{:?}",
            rendered.iter().map(|(n, _)| *n).collect::<Vec<_>>()
        );
        for (name, text) in &rendered {
            for window in secret.as_bytes().windows(8) {
                let window = std::str::from_utf8(window).expect("ASCII");
                assert!(!text.contains(window), "{name} 里出现了凭据片段：{window}");
            }
        }
    }

    /// 决策 179：`unreadable` / `too-large` / `binary` 三种，三种**不同的**理由。
    #[test]
    fn unreadable_too_large_and_binary_get_three_distinct_reasons() {
        let mut description = machine();
        description.dirs.retain(|dir| {
            dir.path != HOME
                && dir.path != format!("{HOME}\\.m2")
                && dir.path != format!("{HOME}\\.docker")
        });
        // 读不出来：声明了存在与大小，但没有 `content`（假文件系统不肯替我们编内容）。
        description.dirs.push(FixtureDir::new(
            HOME,
            vec![
                FixturePath::file(".gitconfig", 142),
                FixturePath::file_with_content(".npmrc", NPMRC),
            ],
        ));
        // 太大：**内容**超过上限（判据是读到的长度，不是声明的 size）。
        let big = "x".repeat((MAX_CONFIG_BYTES + 1) as usize);
        description.dirs.push(FixtureDir::new(
            &format!("{HOME}\\.m2"),
            vec![FixturePath::file_with_content("settings.xml", &big)],
        ));
        // 二进制：内容里有 NUL。
        description.dirs.push(FixtureDir::new(
            &format!("{HOME}\\.docker"),
            vec![FixturePath::file_with_content(
                "config.json",
                "{\"auths\":{}}\u{0}\u{0}MZ",
            )],
        ));

        let (configs, skips) = collect(&description);
        let cases = [
            (r"C:\Users\dev\.gitconfig", UNREADABLE),
            (r"C:\Users\dev\.m2\settings.xml", TOO_LARGE),
            (r"C:\Users\dev\.docker\config.json", BINARY),
        ];
        for (path, slug) in cases {
            let row = row(&configs, path).unwrap_or_else(|| panic!("{path} 必须是候选"));
            assert!(!row.captured, "{path}");
            assert_eq!(row.skip_reason.as_deref(), Some(slug), "{path}");
            assert!(row.content_hash.is_none(), "{path}");
            let entry = skip(&skips, path).unwrap_or_else(|| panic!("{path} 要在跳过清单里"));
            assert_eq!(entry.kind, slug, "{path}");
            assert!(!entry.reason.trim().is_empty(), "{path}");
        }
        // `too-large` 的理由要**具体**（写出真实字节数），不是"超过 limit"。
        let entry = skip(&skips, r"C:\Users\dev\.m2\settings.xml").expect("too-large");
        assert!(
            entry.reason.contains(&(MAX_CONFIG_BYTES + 1).to_string()),
            "理由里要写出真实字节数：{}",
            entry.reason
        );
        // 对照组：`.npmrc` 必须被捕获（否则"全部跳过"的实现也能过）。
        assert!(
            row(&configs, r"C:\Users\dev\.npmrc")
                .expect("候选")
                .captured
        );
    }

    /// 决策 181：`~/.ssh` 的三分类，以及"没看不是跳过"。
    #[test]
    fn ssh_private_keys_and_known_hosts_are_classified_and_the_rest_is_not_a_candidate() {
        let (configs, skips) = collect(&machine());

        let ssh = r"C:\Users\dev\.ssh";
        assert!(
            row(&configs, &format!("{ssh}\\config"))
                .expect("config")
                .captured
        );
        assert_eq!(
            skip(&skips, &format!("{ssh}\\id_ed25519"))
                .expect("私钥")
                .kind,
            PRIVATE_KEY_FILE
        );
        assert_eq!(
            skip(&skips, &format!("{ssh}\\work.pem"))
                .expect("私钥")
                .kind,
            PRIVATE_KEY_FILE
        );
        assert_eq!(
            skip(&skips, &format!("{ssh}\\known_hosts"))
                .expect("known_hosts")
                .kind,
            HOST_KEYS_NOT_CAPTURED
        );
        // `id_ed25519.pub` **不是候选** —— "没看"不是"跳过"。
        assert!(skip(&skips, &format!("{ssh}\\id_ed25519.pub")).is_none());
        assert!(row(&configs, &format!("{ssh}\\id_ed25519.pub")).is_none());
        // 私钥文件本身**有一行、但没被捕获**（决策 184：跳过的行照样在清单里，
        // 否则"跳过了什么"无处可查），而它的行里没有任何材料。
        let key = row(&configs, &format!("{ssh}\\id_ed25519")).expect("私钥要有一行");
        assert!(!key.captured);
        assert_eq!(key.skip_reason.as_deref(), Some(PRIVATE_KEY_FILE));
        assert!(key.bytes.is_none() && key.content_hash.is_none());
    }

    /// 决策 180：缓存目录只在跳过清单里，**不走进去、不报体积**，不存在的一条都没有。
    #[test]
    fn cache_directories_are_listed_but_never_entered_or_measured() {
        let mut description = machine();
        // 六个里再补四个存在的（`npm-cache` 已经在 `machine()` 里了）。
        for suffix in [
            r"\pnpm\store",
            r"\pip\Cache",
            r"\.cache\codex-runtimes",
            r"\.m2\repository",
        ] {
            let path = if suffix.starts_with(r"\.cache") || suffix.starts_with(r"\.m2") {
                format!("{HOME}{suffix}")
            } else {
                format!("{LOCALAPPDATA}{suffix}")
            };
            description.dirs.push(FixtureDir::new(&path, Vec::new()));
        }
        // `\uv\cache` 故意不声明（不存在 → 一条都不出）。
        let (configs, skips) = collect(&description);

        for (root, suffix) in CACHE_DIRECTORIES {
            let root = if root == "LOCALAPPDATA" {
                LOCALAPPDATA
            } else {
                HOME
            };
            let path = format!("{root}{suffix}");
            let expected = suffix != r"\uv\cache";
            let entry = skip(&skips, &path);
            assert_eq!(
                entry.is_some(),
                expected,
                "{path}：命中前缀且存在才有一条（六个里五个存在）"
            );
            if let Some(entry) = entry {
                assert_eq!(entry.kind, CACHE_DIRECTORY);
                assert_eq!(entry.section, "configs");
                assert_eq!(entry.scope.as_deref(), Some(SCOPE_FILE));
                // **不报体积**：理由里不许出现 "8 GB" 这种数字。
                assert!(
                    !entry.reason.contains("GB") && !entry.reason.contains("MB"),
                    "缓存目录的理由不许带体积：{}",
                    entry.reason
                );
                // 缓存目录**只在跳过清单里**：不进 `configs.toml`。
                assert!(row(&configs, &path).is_none(), "{path} 不该有一行");
            }
        }
    }

    /// 决策 182：产品目录标记行 + 三个密钥材料；非产品目录一个字都不出。
    #[test]
    fn jetbrains_product_directories_are_marked_and_their_secrets_skipped() {
        let (configs, skips) = collect(&machine());
        let product = format!("{APPDATA}\\JetBrains\\IntelliJIdea2026.1");

        let marker = row(&configs, &product).expect("产品目录要出一条标记行");
        assert!(marker.captured);
        assert_eq!(marker.kind, "jetbrains");
        assert!(
            marker.bytes.is_none() && marker.content_hash.is_none(),
            "目录不是文件：标记行没有 bytes / content_hash"
        );
        assert_eq!(marker.skip_reason, None);

        assert_eq!(
            skip(&skips, &format!("{product}\\c.kdbx"))
                .expect("c.kdbx")
                .kind,
            CREDENTIAL_DATABASE
        );
        assert_eq!(
            skip(&skips, &format!("{product}\\idea.key"))
                .expect("idea.key")
                .kind,
            PRIVATE_KEY_FILE
        );
        // 目录里的其它东西**不是候选**。
        assert!(skip(&skips, &format!("{product}\\consentOptions")).is_none());
        // 非产品目录（`acp-agents` 没有版本尾巴）既不出行也不出跳过项。
        let not_a_product = format!("{APPDATA}\\JetBrains\\acp-agents");
        assert!(row(&configs, &not_a_product).is_none());
        assert!(skip(&skips, &not_a_product).is_none());
    }

    /// 决策 182 的形状判据本身。
    #[test]
    fn only_product_plus_version_directories_are_products() {
        for name in ["IntelliJIdea2026.1", "PyCharm2026.1", "GoLand2025.3.1"] {
            assert!(is_product_directory(name), "{name} 是产品目录");
        }
        for name in ["acp-agents", "consentOptions", "2026.1", "IDEA"] {
            assert!(!is_product_directory(name), "{name} 不是产品目录");
        }
    }

    /// 决策 183：五层形态各一个 `identity_source`，而且身份与路径都跟着变。
    #[test]
    fn the_git_identity_names_the_layer_it_came_from() {
        let cases: [(&str, &str, &str); 5] = [
            (
                "system",
                "file:C:/Program Files/Git/etc/gitconfig\tuser.name=System Dev\nfile:C:/Program Files/Git/etc/gitconfig\tuser.email=dev@system.example\n",
                GIT_GLOBAL_OUT,
            ),
            (
                "global",
                GIT_SYSTEM_OUT,
                "file:C:/Users/dev/.gitconfig\tuser.name=Dev\nfile:C:/Users/dev/.gitconfig\tuser.email=dev@example.com\n",
            ),
            (
                "global",
                "file:C:/Program Files/Git/etc/gitconfig\tuser.name=Machine Dev\n",
                "file:C:/Users/dev/.gitconfig\tuser.name=Dev Local\n",
            ),
            ("missing", GIT_SYSTEM_OUT, GIT_GLOBAL_OUT),
            ("unknown", "", ""),
        ];

        for (source, system, global) in cases {
            let mut description = machine();
            description.processes = if source == "unknown" {
                Vec::new()
            } else {
                git_processes(system, global)
            };
            let (configs, _) = collect(&description);
            let git = configs.git.expect("`[git]` 表永远存在（决策 183）");
            assert_eq!(git.identity_source, source, "system={system:?}");

            match source {
                "system" => {
                    assert_eq!(git.user_name.as_deref(), Some("System Dev"));
                    assert_eq!(git.user_email.as_deref(), Some("dev@system.example"));
                }
                "global" if system.contains("Machine Dev") => {
                    // 两层都有身份 → **全局赢**。
                    assert_eq!(git.user_name.as_deref(), Some("Dev Local"));
                }
                "global" => {
                    assert_eq!(git.user_name.as_deref(), Some("Dev"));
                    assert_eq!(git.user_email.as_deref(), Some("dev@example.com"));
                }
                "missing" => {
                    assert!(
                        git.user_name.is_none() && git.user_email.is_none(),
                        "没有身份时两个键都不出（'没有身份'与'身份是空串'是两句话）"
                    );
                    // 但两份文件在哪**问得到**。
                    assert!(git.system_config.is_some() && git.global_config.is_some());
                }
                _ => {
                    assert!(
                        git.system_config.is_none() && git.global_config.is_none(),
                        "git 不可用时连路径都问不到"
                    );
                    assert!(git.user_name.is_none() && git.user_email.is_none());
                }
            }
        }
    }

    /// 决策 183：`--show-origin` 的路径**原样存**（不把 `/` 归一化成 `\`），
    /// 而系统级那一份**也是一条候选行**（`layer = "system"`）。
    #[test]
    fn the_git_config_files_come_from_gits_own_answer_and_are_not_duplicated() {
        let (configs, skips) = collect(&machine());
        let git = configs.git.as_ref().expect("[git]");

        assert_eq!(
            git.system_config.as_deref(),
            Some("C:/Program Files/Git/etc/gitconfig"),
            "原样：git 自己就是打正斜杠的"
        );
        assert_eq!(
            git.global_config.as_deref(),
            Some("C:/Users/dev/.gitconfig")
        );

        // 系统级配置文件：从 git 的回答里补出来的那一行（`layer = "system"`）。
        let system = row(&configs, "C:/Program Files/Git/etc/gitconfig").expect("系统级一行");
        assert_eq!(system.kind, "git");
        assert_eq!(system.layer.as_deref(), Some("system"));
        assert!(system.captured);
        assert_eq!(
            system.content_hash.as_deref(),
            Some(hash_of(SYSTEM_GITCONFIG).as_str())
        );

        // 全局级那一份**只有一行**（env 候选已经读过它，git 报的正斜杠路径不该再造一行）。
        let global_rows: Vec<&ConfigRow> = configs
            .config
            .iter()
            .filter(|row| normalized(&row.path) == normalized(r"C:\Users\dev\.gitconfig"))
            .collect();
        assert_eq!(global_rows.len(), 1, "同一份文件不许出两行");
        assert_eq!(global_rows[0].layer.as_deref(), Some("global"));

        // 路径里出现过的两个文件都在跳过清单之外（它们都读到了）。
        assert!(skip(&skips, "C:/Program Files/Git/etc/gitconfig").is_none());
    }

    /// 决策 184：`captured` 与 `skip_reason` 是**两个互斥子集**（逐行断言）。
    #[test]
    fn captured_and_skip_reason_are_two_disjoint_subsets() {
        let (configs, skips) = collect(&machine());
        assert!(!configs.config.is_empty());

        for row in &configs.config {
            if row.kind == "jetbrains" {
                continue; // 目录标记行是唯一的例外
            }
            if row.captured {
                assert!(
                    row.bytes.is_some() && row.content_hash.is_some(),
                    "{}：捕获了就要有 bytes 与哈希",
                    row.path
                );
                assert!(row.skip_reason.is_none(), "{}：不许既捕获又跳过", row.path);
                assert!(
                    skip(&skips, &row.path).is_none(),
                    "{}：捕获了就不该在跳过清单里",
                    row.path
                );
            } else {
                let reason = row.skip_reason.as_deref().unwrap_or_default();
                assert!(
                    !reason.trim().is_empty(),
                    "{}：跳过了就要有具体理由",
                    row.path
                );
                assert!(
                    row.bytes.is_none() && row.content_hash.is_none(),
                    "{}：没读到内容就不该有哈希",
                    row.path
                );
                let entry = skip(&skips, &row.path)
                    .unwrap_or_else(|| panic!("{}：跳过了就必须在跳过清单里", row.path));
                assert_eq!(entry.kind, reason, "{}：两处的 slug 必须一致", row.path);
            }
        }
        // 反过来：**文件**跳过项的每一条都有一条对应的行（不许只在一边出现）。
        // 缓存目录是唯一的不适用者 —— 它是**目录**，不是配置文件候选，
        // 所以它只在跳过清单里（决策 180："缓存目录只在跳过清单里"）。
        for entry in &skips {
            if entry.kind == CACHE_DIRECTORY {
                assert!(row(&configs, &entry.name).is_none(), "{}", entry.name);
                continue;
            }
            let row = row(&configs, &entry.name)
                .unwrap_or_else(|| panic!("{}：跳过项必须在 configs.toml 里有行", entry.name));
            assert!(!row.captured, "{}", entry.name);
        }
    }

    /// 根变量读的是**进程环境**，不是持久环境。
    ///
    /// 这条用例钉住一个真机事实：`USERPROFILE` / `APPDATA` / `LOCALAPPDATA`
    /// **不在注册表里**（它们是会话派生的量）。改成读持久环境的话，真机上这一节会
    /// 一行都拼不出来，而"注册表里也没有"这件事在固定装置里必须表达得出来。
    #[test]
    fn the_roots_come_from_the_process_environment() {
        let description = machine();
        // 进程环境里有 ⇒ 候选拼得出来。
        let (configs, _) = collect(&description);
        assert!(row(&configs, r"C:\Users\dev\.npmrc").is_some());

        // 把根从进程环境里拿掉 ⇒ 这一节**拼不出**这些候选（而不是"猜一个 C:\Users\<谁>"）。
        let mut without = machine();
        without.env.remove("USERPROFILE");
        let (configs, skips) = collect(&without);
        assert!(row(&configs, r"C:\Users\dev\.npmrc").is_none());
        assert!(row(&configs, r"C:\Users\dev\.m2\settings.xml").is_none());
        assert!(skip(&skips, r"C:\Users\dev\.ssh\id_ed25519").is_none());
        // 但 `~/.gitconfig` **照旧有一行** —— 它这次是从 **git 自己的回答**里来的
        // （`--show-origin` 给出的路径与 `USERPROFILE` 无关）。这正是"两个来源"的价值：
        // 一个根变量丢了，git 那一层仍然说得清自己的配置文件在哪。
        let gitconfig = row(&configs, r"C:\Users\dev\.gitconfig").expect("git 报出来的那一份");
        assert_eq!(gitconfig.layer.as_deref(), Some("global"));
        assert!(gitconfig.captured);
        // APPDATA 还在 ⇒ 那一半照旧。
        assert!(
            row(
                &configs,
                r"C:\Users\dev\AppData\Roaming\Code\User\settings.json"
            )
            .is_some()
        );
    }

    /// 同一个假机器跑两次逐条相同（确定性是幂等性的一部分）。
    #[test]
    fn two_runs_of_the_same_machine_are_identical() {
        let description = machine();
        let (first, first_skips) = collect(&description);
        let (second, second_skips) = collect(&description);
        assert_eq!(first, second);
        assert_eq!(first_skips, second_skips);
    }
}
