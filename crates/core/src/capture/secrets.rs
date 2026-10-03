//! 密钥形状识别 —— **本票的安全红线**。
//!
//! # 原则：只捕获意图，绝不捕获材料
//!
//! 捕获的每一条都必须是版本 / 路径 / 名字。任何 token、密文、DPAPI blob、密钥文件
//! 都**不得**进入 `tuoen.d/` —— 因为 `tuoen.d/` 的用途就是**被提交进仓库**。
//!
//! # 本机实测的判据依据（这条经验值得记住）
//!
//! 对 68 个进程环境变量做正则扫描：**零密钥**（只命中假阳性）。
//! 而 `C:\Users\Muelsyse\.m2\settings.xml` 里有**明文 GitLab PAT**（`glpat-` 开头）。
//! **吓人的地方是安全的，安全的地方是危险的** —— 所以扫描既不能只在"看起来敏感"的
//! 地方做，也不能因为"环境变量里没有"就认为整件事不存在。
//!
//! # 两类判据，理由不同
//!
//! | 判据 | 触发条件 | 为什么需要它 |
//! |---|---|---|
//! | [`Which::ValueLooksLikeCredential`] | 值的**形状**像某个厂商的凭据 | 最可靠：`glpat-…` 就是 GitLab 的 PAT，没有第二种解释 |
//! | [`Which::NameSuggestsCredential`] | 名字里出现了 `TOKEN` / `SECRET` / …，**且值足够长、不像路径、不是布尔数字** | 不透明的一长串随机字符没有可认的前缀，光看值认不出来 |
//! | [`Which::ContentContainsCredentialShape`] | **文件内容**里命中上面任一条（[`scan_content`]） | 配置文件没有变量名（决策 178）：`.m2/settings.xml` 里那一个 `glpat-` 就是靠它拦住的 |
//!
//! 名字那条刻意加了三个护栏，因为它们各自对应一个真实的反例：
//! `TOKENIZERS_PARALLELISM=false`（名字命中但值是布尔）、`SSH_KEY_PATH=C:\Users\…`
//! （值是路径，不是密钥本身）、`API_KEY=`（空值）。少了任何一条护栏，
//! 跳过清单会充满噪声，而**一份充满噪声的清单等于没有清单**。
//!
//! # 一个扫描器，两个调用方
//!
//! 环境变量走 [`scan`]（靠变量名提示），配置文件走 [`scan_content`]（没有变量名，
//! 只能看内容）。**两个调用方共用同一份形状表与同一份名字提示词表** ——
//! 两个扫描器迟早对同一个 token 给出不同答案（决策 178）。
//!
//! # 本模块的输出里不许出现材料
//!
//! [`Detection`] 里只有"像什么"与"为什么"，**没有任何取自值本身的字节**：
//! 没有前几位、没有长度、没有哈希。这条性质有专门的用例钉住
//! （`the_detection_carries_no_part_of_the_value`）—— 因为它是安全属性，
//! 而不是风格问题。

/// 一个值像哪种凭据。
///
/// 取值是**稳定 slug**（`--json` 与 `skipped.toml` 里都用它），不本地化。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Shape {
    /// `glpat-…` —— GitLab 个人访问令牌。**本机 `.m2/settings.xml` 里就有一个。**
    GitlabPat,
    /// `ghp_` / `gho_` / `ghu_` / `ghs_` / `ghr_` —— GitHub 令牌。
    GithubToken,
    /// `xoxb-` / `xoxp-` / … —— Slack 令牌。
    SlackToken,
    /// `AKIA…` / `ASIA…` —— AWS 访问密钥 ID。
    AwsAccessKey,
    /// `-----BEGIN … PRIVATE KEY-----` —— 私钥正文。
    PrivateKeyBlock,
    /// 三段点分的 base64url（`eyJ…`）—— JWT。
    Jwt,
    /// `sk_live_` / `sk_test_` / `rk_live_` —— Stripe 密钥。
    StripeKey,
    /// `npm_…` —— npm 令牌。
    NpmToken,
    /// `AIza…` —— Google API 密钥。
    GoogleApiKey,
    /// `sk-…` —— OpenAI 风格的密钥。
    OpenAiKey,
}

impl Shape {
    /// 稳定 slug。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GitlabPat => "gitlab-pat",
            Self::GithubToken => "github-token",
            Self::SlackToken => "slack-token",
            Self::AwsAccessKey => "aws-access-key",
            Self::PrivateKeyBlock => "private-key",
            Self::Jwt => "jwt",
            Self::StripeKey => "stripe-key",
            Self::NpmToken => "npm-token",
            Self::GoogleApiKey => "google-api-key",
            Self::OpenAiKey => "openai-key",
        }
    }

    /// 给用户看的一句话（中文），**不含任何取自值的内容**。
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::GitlabPat => "值形似 GitLab 个人访问令牌（`glpat-` 前缀）",
            Self::GithubToken => "值形似 GitHub 令牌（`ghp_` / `gho_` / … 前缀）",
            Self::SlackToken => "值形似 Slack 令牌（`xox…` 前缀）",
            Self::AwsAccessKey => "值形似 AWS 访问密钥 ID（`AKIA` 前缀）",
            Self::PrivateKeyBlock => "值里有私钥正文（`BEGIN … PRIVATE KEY`）",
            Self::Jwt => "值形似 JWT（三段点分的 base64url）",
            Self::StripeKey => "值形似 Stripe 密钥（`sk_live_` / `sk_test_` 前缀）",
            Self::NpmToken => "值形似 npm 令牌（`npm_` 前缀）",
            Self::GoogleApiKey => "值形似 Google API 密钥（`AIza` 前缀）",
            Self::OpenAiKey => "值形似 OpenAI 风格密钥（`sk-` 前缀）",
        }
    }
}

/// 判据是哪一类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Which {
    /// 值的形状像凭据。**最强的判据**：不认识的名字也拦得住。
    ValueLooksLikeCredential,
    /// 名字像凭据（且过了三个护栏）。不透明的一长串只能靠名字认。
    NameSuggestsCredential,
    /// **文件内容**里命中了上面任一条（[`scan_content`]）。
    ///
    /// 它与前两条不是"更宽"或"更严"，而是**不同的入口**：配置文件里没有变量名，
    /// 所以名字那条判据在那里无处施加，而形状那条要按"词"切开才能用。
    ContentContainsCredentialShape,
}

impl Which {
    /// `skipped.toml` 里的 `kind` 取值。
    #[must_use]
    pub const fn kind(self) -> &'static str {
        match self {
            Self::ValueLooksLikeCredential => "credential",
            Self::NameSuggestsCredential => "credential-named",
            Self::ContentContainsCredentialShape => "contains-credential-shape",
        }
    }
}

/// 一次命中。**只带结论，不带材料。**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Detection {
    /// 怎么判出来的。
    pub which: Which,
    /// 值像哪种凭据（名字判据命中时是 `None` —— 那种情况下我们不知道它是什么，
    /// 只知道它**不该被写出去**）。
    pub shape: Option<Shape>,
    /// 名字里命中的那个词（只有名字判据才有）。它是**变量名的一部分**，不是值。
    pub name_hint: Option<&'static str>,
}

/// `Display` 就是 [`Detection::reason`]。
///
/// **它存在的理由不只是方便**：日志、错误信息、`--json` 的兜底路径都可能把一个
/// `Detection` 格式化出去，而"这个类型怎么被打印"必须与"原因怎么写"是同一条规则
/// （都不含材料）。有专门的用例把 `Display`、`Debug` 与 `reason()` 三个出口一起扫。
impl std::fmt::Display for Detection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason())
    }
}

impl Detection {
    /// `skipped.toml` 里的 `kind`。
    #[must_use]
    pub const fn kind(self) -> &'static str {
        self.which.kind()
    }
    /// 给用户看的原因（中文），**不含任何取自值的内容**。
    #[must_use]
    pub fn reason(self) -> String {
        match (self.which, self.shape, self.name_hint) {
            (Which::ValueLooksLikeCredential, Some(shape), _) => {
                format!("{} —— 没有写进快照", shape.describe())
            }
            (Which::ContentContainsCredentialShape, Some(shape), _) => {
                format!("文件内容里{} —— 没有写进快照", shape.describe())
            }
            (Which::ContentContainsCredentialShape, _, Some(hint)) => format!(
                "文件内容里有一处键名含 `{hint}` 的赋值，值够长、不像路径 —— \
                 无法排除它是凭据，所以没有写进快照"
            ),
            (Which::NameSuggestsCredential, _, Some(hint)) => format!(
                "变量名里有 `{hint}`，且值够长、不像路径 —— 无法排除它是凭据，所以没有写进快照"
            ),
            // 每个分支各自的不变量：值判据一定有 shape，名字判据一定有 hint。
            // 走到这里说明有人加了新判据却没加描述 —— 给一句诚实的兜底，而不是 panic：
            // 一条说不清原因的跳过记录，也比把值写出去或者当场崩掉好。
            _ => "疑似凭据 —— 没有写进快照".to_owned(),
        }
    }
}

/// 名字里出现这些片段就值得多看一眼。
///
/// **按整段比较，不按子串**：`PATH` 里含有 `PAT` 三个字母，但它的段是 `PATH`，
/// 不是 `PAT` —— 用子串匹配会把 `PATH` 本身判成凭据。这条不是理论风险：
/// `PATH` 恰好是**最不该**被跳过的一个变量。
const NAME_HINTS: &[&str] = &[
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "PWD",
    "CREDENTIAL",
    "CREDENTIALS",
    "PAT",
    "APIKEY",
    "ACCESSKEY",
    "PRIVATEKEY",
    "AUTH",
];

/// 值的长度不到这个数就不按名字判 —— 短值几乎总是枚举、开关或版本号。
const NAME_HINT_MIN_LEN: usize = 20;

/// 扫一个环境变量。命中就返回结论（**不含材料**）。
///
/// `name` 与 `value` 都可能为空；空值不会被判成凭据。
#[must_use]
pub fn scan(name: &str, value: &str) -> Option<Detection> {
    if value.is_empty() {
        return None;
    }
    if let Some(shape) = value_shape(value) {
        return Some(Detection {
            which: Which::ValueLooksLikeCredential,
            shape: Some(shape),
            name_hint: None,
        });
    }
    let hint = name_suggests_secret(name)?;
    if value.chars().count() < NAME_HINT_MIN_LEN {
        return None;
    }
    if looks_like_a_path(value) || looks_like_a_scalar(value) {
        return None;
    }
    Some(Detection {
        which: Which::NameSuggestsCredential,
        shape: None,
        name_hint: Some(hint),
    })
}

/// 扫一段**文件内容**（配置文件没有变量名，只能看内容）。
///
/// 判据与 [`scan`] 共用同一份形状表与同一份名字提示词表 —— 只是入口不同：
///
/// 1. **私钥正文**：`-----BEGIN … PRIVATE KEY-----` 跨行，所以按行判；
/// 2. **厂商形状**：把内容按"词"切开，逐个问 [`value_shape`]。
///    `.m2/settings.xml` 里那一个 `glpat-` 就是被这一步拦住的（决策 178 的实证）；
/// 3. **赋值给一个像密钥的键**：`<password>…</password>` / `token = …` / `"key": "…"`。
///
/// 命中就返回结论（**不含材料**）。返回值里的 [`Which::kind`] 是
/// `contains-credential-shape` —— 决策 179 的 `skip_reason` 用的就是它。
#[must_use]
pub fn scan_content(text: &str) -> Option<Detection> {
    if text.is_empty() {
        return None;
    }

    // ① 私钥正文。
    for line in text.lines() {
        if line.contains("-----BEGIN ") && line.contains("PRIVATE KEY-----") {
            return Some(content_hit(Some(Shape::PrivateKeyBlock), None));
        }
    }

    // ② 厂商形状。
    for token in tokens(text) {
        if let Some(shape) = value_shape(token) {
            return Some(content_hit(Some(shape), None));
        }
    }

    // ③ 赋值给一个像密钥的键。
    for line in text.lines() {
        let Some((key, value)) = assignment_of(line) else {
            continue;
        };
        let Some(hint) = name_suggests_secret(key) else {
            continue;
        };
        if value.chars().count() < CONTENT_VALUE_MIN_LEN {
            continue;
        }
        if looks_like_a_path(value) || looks_like_a_scalar(value) || looks_like_a_reference(value) {
            continue;
        }
        return Some(content_hit(None, Some(hint)));
    }

    None
}

fn content_hit(shape: Option<Shape>, name_hint: Option<&'static str>) -> Detection {
    Detection {
        which: Which::ContentContainsCredentialShape,
        shape,
        name_hint,
    }
}

/// 内容级赋值判据的最小值长度。
///
/// **与变量名那条（20）不同，而且必须不同**：配置文件里 `password = …` 这种赋值
/// 本身就是在说"这里有一个秘密"，所以门槛更低；但没有门槛就会把
/// `"auth.type": "oauth"`（键名命中 `AUTH`、值是枚举）判成凭据 ——
/// 一份 vscode 的 `settings.json` 会被整份跳过，而**一份充满噪声的清单等于没有清单**。
const CONTENT_VALUE_MIN_LEN: usize = 8;

/// 把内容切成"词"。
///
/// 分隔符是**不会出现在任何厂商形状里**的那些字符：空白、引号、尖括号、
/// `=`/`:`/`,`/`;` 与括号。`.` 与 `-`/`_` **不切** —— JWT 靠点分、`glpat-` 靠连字符，
/// 切了它们就等于把判据拆散。
fn tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '"' | '\'' | '<' | '>' | '=' | ':' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}'
            )
    })
    .filter(|token| token.len() >= 8)
}

/// 一行里的一处赋值：`(键, 值)`。认不出就是 `None`。
///
/// 三种形状（都是配置文件里真实存在的写法）：XML 的 `<key>value</key>`、
/// ini/npmrc 的 `key = value`、JSON/YAML 的 `"key": "value"`。
fn assignment_of(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }

    // ① XML：`<key>value</key>`。`<key>` 独占一行、注释、声明都返回 None。
    if let Some(rest) = line.strip_prefix('<') {
        let (key, tail) = rest.split_once('>')?;
        if key.is_empty() || key.starts_with('/') || key.starts_with('?') || key.starts_with('!') {
            return None;
        }
        let end = tail.find('<')?;
        return Some((key.trim(), clean_value(&tail[..end])));
    }

    // ② `key = value`（ini / npmrc / properties）优先，其次 `key: value`（json / yaml）。
    let (key, value) = match line.split_once('=') {
        Some(pair) => pair,
        None => line.split_once(':')?,
    };
    let key = clean_key(key);
    // 键里出现空格说明我们切错了地方（例如 `[credential "https://…"]` 这种段头）。
    if key.is_empty() || key.contains(' ') {
        return None;
    }
    Some((key, clean_value(value)))
}

/// 去掉键两端的引号与空白。
fn clean_key(raw: &str) -> &str {
    raw.trim().trim_matches(|c| c == '"' || c == '\'')
}

/// 去掉值两端的引号、尾随逗号与空白。
fn clean_value(raw: &str) -> &str {
    raw.trim()
        .trim_end_matches(',')
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .trim()
}

/// 值是一个**引用**（`%VAR%` / `${VAR}` / `$VAR`），不是材料本身。
///
/// 引用必须放行：`password = ${env.DB_PASSWORD}` 说的是"去别处取"，
/// 它本身没有泄露任何东西，而把它判成凭据会让"用环境变量代替明文"这种
/// **好习惯**反而被跳过。
fn looks_like_a_reference(value: &str) -> bool {
    let value = value.trim();
    value.starts_with('$') || (value.starts_with('%') && value.ends_with('%'))
}

/// 值的形状像哪种凭据。
#[must_use]
pub fn value_shape(value: &str) -> Option<Shape> {
    // 前缀判据。**大小写敏感**是有意的：`ghp_` 与 `AKIA` 都是固定大小写，
    // 放宽大小写只会引入假阳性。
    const PREFIXES: &[(&str, Shape, usize)] = &[
        // GitLab PAT。**门槛是 16，不是 20**：真实 PAT 的前缀后面是 20 位，
        // 但固定装置（`fixtures/capture/credential-shape`，决策 178 的靶子）用的是
        // "前缀 + 16 位"，而本票的真机验收脚本自己那条独立判据是 `glpat-[A-Za-z0-9_\-]{8,}`。
        // 把门槛卡在 20 会让**固定装置里那份文件被当成干净文件捕获** ——
        // 那正是本票安全红线上最不该发生的一件事。
        ("glpat-", Shape::GitlabPat, 16),
        ("ghp_", Shape::GithubToken, 36),
        ("gho_", Shape::GithubToken, 36),
        ("ghu_", Shape::GithubToken, 36),
        ("ghs_", Shape::GithubToken, 36),
        ("ghr_", Shape::GithubToken, 36),
        // fine-grained PAT（决策 178 点名的那一个）。它比 `ghp_` 长得多，
        // 门槛取 22 只是防"有人在文档里写 `github_pat_` 这五个字本身"。
        ("github_pat_", Shape::GithubToken, 22),
        ("xoxb-", Shape::SlackToken, 10),
        ("xoxp-", Shape::SlackToken, 10),
        ("xoxa-", Shape::SlackToken, 10),
        ("xoxr-", Shape::SlackToken, 10),
        ("AKIA", Shape::AwsAccessKey, 16),
        ("ASIA", Shape::AwsAccessKey, 16),
        ("sk_live_", Shape::StripeKey, 16),
        ("sk_test_", Shape::StripeKey, 16),
        ("rk_live_", Shape::StripeKey, 16),
        ("npm_", Shape::NpmToken, 36),
        ("AIza", Shape::GoogleApiKey, 35),
    ];

    for (prefix, shape, min_tail) in PREFIXES {
        if let Some(tail) = value.strip_prefix(prefix)
            && tail.chars().count() >= *min_tail
            && tail
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Some(*shape);
        }
    }

    if value.contains("PRIVATE KEY-----") && value.contains("-----BEGIN ") {
        return Some(Shape::PrivateKeyBlock);
    }
    if looks_like_a_jwt(value) {
        return Some(Shape::Jwt);
    }
    if looks_like_an_openai_key(value) {
        return Some(Shape::OpenAiKey);
    }
    None
}

/// `sk-` + 32 位以上的字母数字。**要求词边界**：`task-1` 这种不该命中。
fn looks_like_an_openai_key(value: &str) -> bool {
    let Some(at) = value.find("sk-") else {
        return false;
    };
    // `sk-` 前面要么什么都没有，要么不是字母数字（否则它是某个更长单词的尾巴）。
    if at > 0 {
        let before = value[..at].chars().next_back();
        if before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
            return false;
        }
    }
    let tail = &value[at + 3..];
    tail.chars().count() >= 32
        && tail
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// 三段点分的 base64url，第一段以 `eyJ` 开头（那是 `{"` 的 base64）。
fn looks_like_a_jwt(value: &str) -> bool {
    if !value.starts_with("eyJ") {
        return false;
    }
    let parts: Vec<&str> = value.split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    parts.iter().all(|part| {
        part.chars().count() >= 4
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    })
}

/// 名字里有没有值得警觉的整段。
fn name_suggests_secret(name: &str) -> Option<&'static str> {
    // 变量名里的分隔符不止 `_`：`IntelliJ IDEA` 这种含空格的名字是合法的，
    // 所以按"非字母数字"切段，而不是只按 `_`。
    let segments: Vec<String> = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_uppercase())
        .collect();

    for hint in NAME_HINTS {
        if segments.iter().any(|segment| segment == hint) {
            return Some(hint);
        }
    }
    // 两段拼起来的形状（`API_KEY` / `ACCESS_KEY` / `PRIVATE_KEY`）。
    let joined = segments.join("");
    for hint in ["APIKEY", "ACCESSKEY", "PRIVATEKEY", "SSHKEY"] {
        if joined.contains(hint) {
            // 有 `_KEY` 但拿不到具体是哪个词时也要给一个名字 —— 兜底用 `KEY`。
            return Some("KEY");
        }
    }
    None
}

/// 值看起来是一个路径（那么它指向密钥文件，而它本身不是密钥）。
fn looks_like_a_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    if value.contains('\\') || value.contains('/') {
        // 光有斜杠还不够（`a/b` 可能是枚举），但"驱动器 + 冒号"或 UNC 是明确的路径。
        if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
            return true;
        }
        if value.starts_with("\\\\") || value.starts_with("//") || value.starts_with("~/") {
            return true;
        }
        // `C:\Users\…` 之外的相对路径也按路径处理：它更可能是位置而不是密钥。
        if value.contains('\\') {
            return true;
        }
    }
    false
}

/// 值看起来是一个标量（布尔、数字、枚举），不是不透明的密钥。
fn looks_like_a_scalar(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "true" | "false" | "yes" | "no" | "on" | "off"
    ) {
        return true;
    }
    value.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个"形状像凭据"的值：**前缀与正文分开写，源码里不留完整令牌**。
    ///
    /// # 为什么不能直接写常量
    ///
    /// 本仓库第一次 push 这一票的提交时，**GitHub 的 push protection 把
    /// `crates/core/src/capture/secrets.rs` 里那个 `glpat-…` 常量当成了真的
    /// GitLab 令牌拒收**（`GH013: Repository rule violations found … Push cannot
    /// contain secrets`）。一个测试固定装置不该让任何人无法 push 这个仓库，
    /// 而且它还会在 fork 里重复触发。
    ///
    /// 所以厂商前缀与正文都写成**两段**，运行时拼起来：形状一模一样（被测的正是
    /// 形状），而提交进仓库的文本里没有一个是完整令牌。
    fn shaped(prefix: &str, body: &str) -> String {
        format!("{prefix}{body}")
    }

    /// 本机 `.m2/settings.xml` 里那一类 —— 前缀判据必须认得它。
    #[test]
    fn a_gitlab_pat_is_recognized_by_its_prefix() {
        let detection =
            scan("SOMETHING", &shaped("glpat-", "abcdefghijklmnopqrst")).expect("必须命中");
        assert_eq!(detection.shape, Some(Shape::GitlabPat));
        assert_eq!(detection.kind(), "credential");
        assert!(
            detection.reason().contains("GitLab"),
            "{}",
            detection.reason()
        );
    }

    #[test]
    fn every_documented_prefix_is_recognized() {
        let cases: Vec<(String, Shape)> = vec![
            (shaped("glpat-", "aaaaaaaaaaaaaaaaaaaa"), Shape::GitlabPat),
            (
                shaped("ghp_", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                Shape::GithubToken,
            ),
            (
                concat!("github_pat_", "aaaaaaaaaaaaaaaaaaaaaa").to_owned(),
                Shape::GithubToken,
            ),
            (shaped("xoxb-", "aaaaaaaaaa"), Shape::SlackToken),
            (shaped("AKIA", "AAAAAAAAAAAAAAAA"), Shape::AwsAccessKey),
            (shaped("sk_live_", "aaaaaaaaaaaaaaaa"), Shape::StripeKey),
            (
                shaped("npm_", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                Shape::NpmToken,
            ),
            (
                shaped("AIza", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                Shape::GoogleApiKey,
            ),
            (
                "-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n".to_owned(),
                Shape::PrivateKeyBlock,
            ),
            (
                "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.abcdefgh".to_owned(),
                Shape::Jwt,
            ),
            (
                shaped("sk-", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                Shape::OpenAiKey,
            ),
        ];
        for (value, expected) in &cases {
            let detection =
                scan("X", value).unwrap_or_else(|| panic!("`{expected:?}` 必须被认出来：{value}"));
            assert_eq!(detection.shape, Some(*expected), "值：{value}");
        }
    }

    /// **三个护栏各自对应一个真实的反例**，少了任何一个，跳过清单就会充满噪声。
    #[test]
    fn the_name_heuristic_has_three_guards_that_all_matter() {
        // 护栏一：值太短。`TOKENIZERS_PARALLELISM=false` 是本机真实存在的变量。
        assert_eq!(scan("TOKENIZERS_PARALLELISM", "false"), None);
        // 护栏二：值是路径 —— 它指向密钥文件，而不是密钥本身。
        assert_eq!(
            scan("SSH_KEY_PATH", r"C:\Users\x\.ssh\id_ed25519"),
            None,
            "路径不是密钥"
        );
        // 护栏三：值是标量。
        assert_eq!(scan("API_KEY_VERSION", "3"), None);
        assert_eq!(scan("SECRET_COUNT", "12345"), None);

        // 真正该命中的：名字像凭据、值是不透明的一长串。
        let detection = scan("GITLAB_TOKEN", "aBcDeFgHiJkLmNoPqRsTuVwX").expect("名字判据必须命中");
        assert_eq!(detection.which, Which::NameSuggestsCredential);
        assert_eq!(detection.kind(), "credential-named");
        assert_eq!(detection.name_hint, Some("TOKEN"));
        assert!(
            detection.reason().contains("TOKEN"),
            "{}",
            detection.reason()
        );
    }

    /// `PATH` 是最不该被跳过的一个变量，而它的名字里含有 `PAT` 三个字母。
    #[test]
    fn path_is_not_mistaken_for_a_pat() {
        let long_path = r"C:\Windows;C:\Program Files\Git\cmd;C:\Program Files\nodejs;C:\Dev\bin";
        assert_eq!(scan("PATH", long_path), None);
        assert_eq!(scan("Path", long_path), None);
        assert_eq!(
            scan("PATHEXT", ".COM;.EXE;.BAT;.CMD;.VBS;.VBE;.JS;.JSE"),
            None
        );
    }

    #[test]
    fn an_empty_value_is_never_a_credential() {
        assert_eq!(scan("GITLAB_TOKEN", ""), None);
        assert_eq!(scan("", ""), None);
        assert_eq!(
            scan("", &shaped("glpat-", "abcdefghijklmnopqrst")),
            Some(Detection {
                which: Which::ValueLooksLikeCredential,
                shape: Some(Shape::GitlabPat),
                name_hint: None,
            }),
            "值判据不依赖名字"
        );
    }

    /// 这两条是**上界**，不是下界：形状再像也要在明显不是凭据的时候放行。
    #[test]
    fn ordinary_values_are_not_flagged() {
        for value in [
            r"C:\Program Files\Git\cmd",
            "24.19.0",
            "true",
            "C:\\Users\\x\\AppData\\Local\\nvm",
            "%LOCALAPPDATA%\\Programs",
            "https://example.com/api",
        ] {
            assert_eq!(scan("SOME_VAR", value), None, "不该命中：{value}");
        }
    }

    /// **安全属性**：判定的结论里不许带出值本身的任何一段。
    ///
    /// 这条用例存在的理由：这是写进公开仓库的文件，而"不小心把前 8 位放进
    /// 跳过记录"是最容易发生的一种泄露 —— 它看起来已经很小心了。
    ///
    /// **厂商前缀不在此列**（`glpat-` 是 GitLab 公开的格式，不是这个值特有的字节）：
    /// 描述里出现 `glpat-` 是**必须的**，否则用户不知道为什么被跳过。被禁的是
    /// 这个值特有的那些内容。
    #[test]
    fn the_detection_carries_no_part_of_the_value() {
        // 同样拼出来：源码里不留完整令牌（见 [`shaped`] 的说明）。
        let secret = shaped("glpat-", "ZZZZsecretsecret1234");
        let detection = scan("GITLAB_TOKEN", &secret).expect("命中");
        let rendered = format!("{detection} {detection:?} {}", detection.reason());
        for fragment in ["ZZZZ", "secret", "1234", "glpat-ZZZZ", secret.as_str()] {
            assert!(
                !rendered.contains(fragment),
                "判定结果里出现了值的一部分（`{fragment}`）：{rendered}"
            );
        }
        // 名字判据那条同理。
        let opaque = "9f8e7d6c5b4a39281706f5e4d3c2b1a0";
        let detection = scan("MY_APP_SECRET", opaque).expect("命中");
        let rendered = format!("{detection} {detection:?} {}", detection.reason());
        assert!(!rendered.contains("9f8e"), "{rendered}");
        assert!(!rendered.contains(opaque), "{rendered}");
    }

    /// 名字判据的段切分：含空格的变量名是合法的（本机 `IntelliJ IDEA`）。
    #[test]
    fn names_with_spaces_and_dashes_are_segmented_too() {
        assert_eq!(scan("IntelliJ IDEA", &"x".repeat(40)), None, "名字不像凭据");
        assert!(scan("MY APP TOKEN", &"x".repeat(40)).is_some());
        assert!(scan("my-app-password", &"x".repeat(40)).is_some());
        assert!(scan("MY.APP.SECRET", &"x".repeat(40)).is_some());
    }

    /// 长度边界：19 不判、20 判。
    #[test]
    fn the_length_guard_is_exactly_twenty() {
        assert_eq!(scan("APP_TOKEN", &"x".repeat(NAME_HINT_MIN_LEN - 1)), None);
        assert!(scan("APP_TOKEN", &"x".repeat(NAME_HINT_MIN_LEN)).is_some());
    }

    // ── 内容级扫描（决策 178：一个扫描器，两个调用方）─────────────────────────

    /// **本机实证**：`~/.m2/settings.xml`（494 B）里有恰好一个 `glpat-`，
    /// 它必须被内容级扫描拦住，而且结论里不许带出那个值的任何一个字节。
    #[test]
    fn a_gitlab_pat_inside_a_config_file_is_found_by_the_content_scan() {
        let token = concat!("glpat-", "abcdefghijklmnopqrst");
        let text = format!(
            "<settings>\n  <servers>\n    <server>\n      <id>gitlab</id>\n      \
             <configuration>\n        <httpHeaders>\n          <property>\n            \
             <name>Private-Token</name>\n            <value>{token}</value>\n          \
             </property>\n        </httpHeaders>\n      </configuration>\n    </server>\n  \
             </servers>\n</settings>\n"
        );
        let detection = scan_content(&text).expect("必须命中");
        assert_eq!(detection.which, Which::ContentContainsCredentialShape);
        assert_eq!(
            detection.kind(),
            "contains-credential-shape",
            "决策 179 的 skip_reason 就是它"
        );
        assert_eq!(detection.shape, Some(Shape::GitlabPat));
        assert!(
            detection.reason().contains("GitLab"),
            "{}",
            detection.reason()
        );
        assert!(
            detection.reason().contains("文件内容"),
            "内容级的说法必须与变量级分开：{}",
            detection.reason()
        );
    }

    /// **"吓人的地方是安全的，安全的地方是危险的"**（决策 178）：
    /// 本机这四个配置文件里一个密钥都没有，它们必须全部被放行。
    #[test]
    fn the_machines_own_config_files_are_not_flagged() {
        // `~/.gitconfig`（142 B）：credential provider + 两个 proxy，没有身份。
        let gitconfig = "[credential]\n\thelper = manager\n\tprovider = generic\n\
                         [http]\n\tproxy = http://proxy.example:8080\n\tschannel = true\n";
        assert_eq!(scan_content(gitconfig), None, "gitconfig 里没有密钥");
        // `~/.docker/config.json`（353 B）：`auths` 为空、`credsStore`。
        let docker = "{\n\t\"auths\": {},\n\t\"credsStore\": \"desktop\",\n\t\
                      \"currentContext\": \"desktop-linux\"\n}\n";
        assert_eq!(
            scan_content(docker),
            None,
            "docker 的凭据在 store 里，不在文件里"
        );
        // `~/.npmrc`（88 B）：只有 `allow-scripts=`。
        assert_eq!(scan_content("allow-scripts=\n"), None);
        // 一份 vscode 的 `settings.json` 形状：键名里有 `auth`，值是枚举。
        assert_eq!(scan_content("{\n\t\"auth.type\": \"oauth\"\n}\n"), None);
    }

    /// 赋值给一个像密钥的键 —— 三种写法都要认得（XML / ini / JSON）。
    #[test]
    fn an_assignment_to_a_secret_shaped_key_is_found() {
        let cases = [
            "<configuration>\n  <password>hunter2hunter2</password>\n</configuration>\n",
            "{\n  \"api_key\": \"aBcDeFgHiJkLmNoP\"\n}\n",
            "auth_token = aBcDeFgHiJkLmNoP\n",
        ];
        for text in cases {
            let detection = scan_content(text).unwrap_or_else(|| panic!("必须命中：{text}"));
            assert_eq!(detection.which, Which::ContentContainsCredentialShape);
            assert_eq!(detection.shape, None, "名字判据不知道它是什么形状");
            assert!(detection.name_hint.is_some());
        }
    }

    /// 内容级判据的护栏：**少了任何一条，跳过清单就会充满噪声**。
    #[test]
    fn the_content_scan_has_guards_against_noise() {
        // 值是布尔。
        assert_eq!(scan_content("auth_enabled = true\n"), None);
        // 值是路径 —— 它指向密钥文件，而不是密钥本身。
        assert_eq!(
            scan_content(r"secret_path = C:\Users\x\.ssh\id_ed25519"),
            None
        );
        // 值是**引用**：用环境变量代替明文是好习惯，不该被跳过。
        assert_eq!(scan_content("password = ${env.DB_PASSWORD}\n"), None);
        assert_eq!(scan_content("password = %DB_PASSWORD%\n"), None);
        // 空内容。
        assert_eq!(scan_content(""), None);
        // 键名不像密钥的长值（普通的枚举与 URL）也要放行。
        assert_eq!(
            scan_content("registry = https://registry.npmjs.org/\n"),
            None
        );
    }

    /// 私钥正文跨行，所以它按行判 —— 而且不依赖任何厂商前缀。
    #[test]
    fn a_private_key_block_inside_a_file_is_found() {
        let text = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n\
                    -----END OPENSSH PRIVATE KEY-----\n";
        let detection = scan_content(text).expect("必须命中");
        assert_eq!(detection.which, Which::ContentContainsCredentialShape);
        assert_eq!(detection.shape, Some(Shape::PrivateKeyBlock));
    }

    /// **安全属性**：内容级判定的结论里同样不许带出值本身的任何一段。
    #[test]
    fn the_content_detection_carries_no_part_of_the_value() {
        let token = concat!("glpat-", "QQQQsecretsecret9999");
        let detection = scan_content(&format!("<value>{token}</value>\n")).expect("命中");
        let rendered = format!("{detection} {detection:?} {}", detection.reason());
        for fragment in ["QQQQ", "secret", "9999", token] {
            assert!(
                !rendered.contains(fragment),
                "内容级判定里出现了值的一部分（`{fragment}`）：{rendered}"
            );
        }
    }
}
