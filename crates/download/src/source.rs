//! 镜像源与模板：**把"从哪儿下"变成可配置、可探测、可回退的东西**。
//!
//! ## 为什么内置清单不够
//!
//! 本机对这个网络的实测（`scripts/probe-mirrors.ps1` 的输出在
//! `docs/acceptance/L0-04-download.md`）：
//!
//! ```text
//! node    cn-npmmirror cn     206  True   155ms
//! node    official     global 206  True   346ms
//! node    cn-tencent   cn     206  True   441ms
//! node    cn-tuna      cn     403  False  127ms   ← 清华
//! node    cn-ustc      cn     404  False  110ms   ← 中科大
//! temurin official-api global 206  True  1253ms
//! temurin cn-tuna      cn     403  False  141ms
//! temurin cn-bfsu      cn     403  False  503ms
//! temurin cn-aliyun    cn     404  False  366ms
//! ```
//!
//! **八个"国内镜像"里只有一个能用。** 抄一份 URL 清单的做法在这里直接失效 ——
//! 清单本身不是可用性。所以：
//!
//! - 内置清单只是**默认的候选顺序**，不是承诺；
//! - [`probe_sources`] 对候选发真实的 range GET，把可达性与耗时量出来；
//! - 每次下载失败都**按顺序回退到下一个**，并把每一次尝试记下来给用户看。
//!
//! ## 模板
//!
//! 自定义镜像用模板表达，占位符与 manifest 层一致（`{version}` / `{platform}` /
//! `{file}`），另外多一个 `{url}` —— 它代表**上游的原始 URL**，
//! 于是"把所有请求转到我自己的代理"只要一条模板：
//!
//! ```toml
//! [[mirror]]
//! match = "https://nodejs.org/dist/"
//! template = "https://cache.corp.example/{url}"
//! ```
//!
//! ## 不托管任何二进制
//!
//! 模板只**指向**别处，我们不重新托管（决策 34）。这是"能安全落地"的前提：
//! 不托管意味着不承担再分发责任。

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::DownloadError;
use crate::transport::Transport;

/// 一个候选源。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    /// 稳定 id（`cn-npmmirror` / `official` / 用户给的）。**不本地化**。
    pub id: String,
    /// 它属于哪个工具（`node` / `temurin` / …）。空表示通用。
    pub tool: String,
    /// 该镜像**替代掉**的上游前缀。
    ///
    /// 这一条是镜像映射的键：`candidates_for` 先看上游 URL 是不是以它开头，
    /// 是的话就把这一段换成 [`Source::mirror_prefix`]。
    ///
    /// **它必须写官方地址，不能写镜像自己的地址。** 早先这里写的是
    /// `https://cdn.npmmirror.com/binaries/node/`（镜像自己的前缀），
    /// 于是 `strip_prefix` 永远不匹配 —— 阿里源在候选列表里**从来没出现过**，
    /// 而它是本机最快的源。那是"配了镜像但一个都没生效"的静默故障。
    pub upstream: String,
    /// 镜像自己的前缀。替换的结果就是 `mirror_prefix + 上游前缀之后的部分`。
    pub mirror_prefix: String,
    /// 把上游 URL 映射到这个源的 URL。
    ///
    /// 对内置镜像它是**替换之后算出来的具体 URL**（调用方填），
    /// 对用户模板与官方它是原样。
    pub url: String,
    /// 一个**小的、已知可用的真实文件**，只用来测"这个源到不到得了"。
    ///
    /// ## 三条硬要求（每条都是实测换来的）
    ///
    /// **① 它必须是一个文件，不能是一个目录。** 一个不认区间请求的服务器
    /// 会把 `-r 0-0` 当普通 GET；如果目标是目录，那意味着**为了问一句
    /// "能不能到"，把整份目录列举下下来**。本机实测过这件事：
    ///
    /// ```text
    /// mirrors.tuna.tsinghua.edu.cn/nodejs-release/   200  155794 字节（整份列举）
    /// mirrors.cloud.tencent.com/nodejs-release/      200  115893 字节（整份列举）
    /// ```
    ///
    /// 而且目录列举回答的是"这个目录能不能列"，不是"制品路径对不对" ——
    /// 探测的判决是用来**淘汰源**的，测错了东西就会误杀。
    ///
    /// **② 它不能带版本号。** 带版本号的路径会过期（镜像会剪枝旧版本），
    /// 那时探测会把一个**能用的**源判成不可用。node 的 `index.json` 正好
    /// 是根目录下的一个固定文件名，所以它同时满足 ① 和 ②。
    ///
    /// **③ 它必须与 `url` 分开。** 早先的实现拿 `url`（镜像前缀）当探测
    /// 目标，结果本机实测：
    ///
    /// ```text
    /// cdn.npmmirror.com/binaries/node/                   404  ← 目录不可列举
    /// cdn.npmmirror.com/binaries/node/v24.19.0/…zip      206  ← 制品完全正常
    /// ```
    ///
    /// 也就是说**阿里源会被自己的探测淘汰掉** —— 而它是本机最快的源。
    /// "前缀不可列举"与"制品取不到"是两件事，混在一起就是一个假阴性。
    ///
    /// 把 ①②③ 合起来看：`<mirror_prefix>index.json` 就是那个目标 ——
    /// 本机实测它在官方、阿里、腾讯三家都是 **206 + 1 字节**，而清华
    /// （403 反爬）与中科大（404）被正确判为不可用。
    pub probe_url: String,
    /// `cn` / `global` / `custom`。给"优先国内"的策略用。
    pub region: Region,
    /// 一条人类可读的说明（为什么它在列表里）。
    pub note: String,
}

/// 源的地理/网络归属。**取值是公开契约**，不本地化。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Region {
    /// 中国大陆可达的镜像。
    Cn,
    /// 上游官方。
    Global,
    /// 用户自己加的（企业内网、本地目录）。
    Custom,
}

impl Region {
    /// `--json` 与固定装置里的稳定取值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cn => "cn",
            Self::Global => "global",
            Self::Custom => "custom",
        }
    }
}

/// 一条镜像模板。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MirrorTemplate {
    /// 稳定 id，用于在报告里点名是哪一条模板生效了。
    pub id: String,
    /// 匹配哪些上游 URL（前缀匹配）。空表示匹配所有。
    pub match_prefix: String,
    /// 模板。可用 `{url}`（上游原始 URL）、`{file}`（文件名）、`{version}`。
    pub template: String,
    /// 是否只用于某个工具（空表示所有工具）。
    #[serde(default)]
    pub tool: String,
    /// 会不会把**认证信息**转发给另一台主机。
    ///
    /// **默认 `false`，而且这是安全默认值。** 一个把 `https://user:pass@upstream/...`
    /// 转成 `https://mycache/...` 的模板会**丢掉凭据**（好事），
    /// 但如果模板把 URL **内嵌**进查询串（`?u={url}`），凭据就会跟着过去。
    /// 我们不试图去猜哪条模板安全 —— 我们只在对得上、且用户显式打开这一项时
    /// 才允许**跨主机**转发，并在报告里标出来。
    #[serde(default)]
    pub forward_credentials: bool,
}

impl MirrorTemplate {
    /// 把一条上游 URL 套进模板。
    ///
    /// **返回 `None` 表示这条模板不适用**（前缀不匹配、或工具不匹配）——
    /// 不是错误。调用方会把所有不适用的模板跳过。
    #[must_use]
    pub fn apply(&self, upstream: &str, tool: &str) -> Option<String> {
        if !self.tool.is_empty() && !self.tool.eq_ignore_ascii_case(tool) {
            return None;
        }
        if !self.match_prefix.is_empty() && !upstream.starts_with(&self.match_prefix) {
            return None;
        }
        let file = upstream.rsplit('/').next().unwrap_or_default();
        let rendered = self
            .template
            .replace("{url}", upstream)
            .replace("{file}", file);
        Some(rendered)
    }

    /// 这条模板会把请求送到与上游**不同的主机**吗。
    ///
    /// 用于在报告里标注"凭据会去哪"。**不做安全判断，只做事实陈述** ——
    /// 判断需要知道 URL 里到底有没有凭据，而那要解析它。
    #[must_use]
    pub fn crosses_hosts(&self, upstream: &str, rendered: &str) -> bool {
        host_of(upstream) != host_of(rendered)
    }
}

/// 从 URL 里取主机名（不含凭据）。取不到就返回空串。
#[must_use]
pub fn host_of(url: &str) -> String {
    let after_scheme = url.split("://").nth(1).unwrap_or(url);
    let authority = after_scheme.split('/').next().unwrap_or_default();
    // 削掉 `user:pass@`
    let host = authority.rsplit('@').next().unwrap_or(authority);
    // 削掉 `:port`
    host.split(':').next().unwrap_or(host).to_lowercase()
}

/// 用户配置里的镜像设置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MirrorConfig {
    /// 模板列表，**按顺序**尝试。
    #[serde(default, rename = "mirror")]
    pub mirrors: Vec<MirrorTemplate>,
    /// 要不要用内置的国内镜像。
    ///
    /// **默认 `true`**：这个项目的目标用户就是中国大陆的开发者，
    /// 默认就该快。想只走官方源的人把它关掉。
    #[serde(default = "default_true")]
    pub use_builtin_mirrors: bool,
    /// 要不要把"上游官方地址"也放进候选列表。
    ///
    /// **默认 `true`**：镜像全挂时官方往往是唯一能用的那个
    /// （本机实测清华/中科大/Temurin 镜像全挂，官方可用）。
    #[serde(default = "default_true")]
    pub include_official: bool,
}

impl Default for MirrorConfig {
    /// **手写而不是 derive。**
    ///
    /// `#[derive(Default)]` 会把两个 `bool` 都设成 `false`，而 serde 侧的
    /// `#[serde(default = "default_true")]` 让**从配置文件读出来的**是 `true`。
    /// 两份"默认值"不一致的后果是：跑测试用 `MirrorConfig::default()` 时
    /// 一个候选源都没有（内置镜像被关、官方也被关），而真实运行时却正常 ——
    /// 也就是说这个 bug **只在测试里出现**，于是它的表现是"测试挂了但代码没问题"，
    /// 极易被误判成测试写错了。
    ///
    /// 更糟的方向是反过来的：如果哪天把 serde 的默认值改成 `false`，
    /// 那用户在配置文件里不写这两项就等于把镜像全关了 —— 而这正是
    /// 本项目最不想要的默认。
    fn default() -> Self {
        Self {
            mirrors: Vec::new(),
            use_builtin_mirrors: true,
            include_official: true,
        }
    }
}

const fn default_true() -> bool {
    true
}

/// 内置的 Node.js 源。
///
/// **顺序即默认优先级**，依据是 `scripts/probe-mirrors.ps1` 的实测：
/// 阿里最快（226ms）、官方（346ms）、腾讯（118ms 但目录可列举）。
/// 清华与中科大**在这台机器上 403/404**，但它们在别的网络里通常可用，
/// 所以**留在列表里但排在实测可用的后面** —— 探测会把它们淘汰掉。
///
/// 每行的字段是 `(id, upstream, mirror_prefix, probe_url, region, note)`。
/// `upstream` 必须是**官方发布的真实前缀**（替换的键），
/// `mirror_prefix` 是镜像自己的前缀（替换的值）。见 [`Source::upstream`]。
const NODE_SOURCES: &[(&str, &str, &str, &str, Region, &str)] = &[
    (
        "cn-npmmirror",
        "https://nodejs.org/dist/",
        "https://cdn.npmmirror.com/binaries/node/",
        // 探测打 **`index.json`**：它是一个**真实的文件**，而且在所有
        // 正经的 node 镜像上都存在（**不带版本号**，所以不会过期）。
        "https://cdn.npmmirror.com/binaries/node/index.json",
        Region::Cn,
        "阿里 npmmirror 的 node 二进制镜像；本机实测 index.json 94ms",
    ),
    (
        "cn-tencent",
        "https://nodejs.org/dist/",
        "https://mirrors.cloud.tencent.com/nodejs-release/",
        "https://mirrors.cloud.tencent.com/nodejs-release/index.json",
        Region::Cn,
        "腾讯云 node 镜像；本机实测 index.json 267ms",
    ),
    (
        "cn-tuna",
        "https://nodejs.org/dist/",
        "https://mirrors.tuna.tsinghua.edu.cn/nodejs-release/",
        "https://mirrors.tuna.tsinghua.edu.cn/nodejs-release/index.json",
        Region::Cn,
        "清华 TUNA；本机实测 **403 反爬**（响应体写『您访问使用的软件带有非常用软件的特征』），不是镜像缺失",
    ),
    (
        "cn-ustc",
        "https://nodejs.org/dist/",
        "https://mirrors.ustc.edu.cn/nodejs-release/",
        "https://mirrors.ustc.edu.cn/nodejs-release/index.json",
        Region::Cn,
        "中科大；本机实测 404（`index.json` 与制品路径都不存在）",
    ),
    // **官方排在最后。** 表内顺序就是候选顺序，而"官方是兜底"是
    // `candidates_for` 注释里写死的承诺 —— 把官方插在中间会让那个承诺
    // 只存在于文档里。反过来说，镜像必须排在官方前面才有意义：
    // 排在后面就等于永远轮不到它们。
    (
        "official",
        "https://nodejs.org/dist/",
        "https://nodejs.org/dist/",
        "https://nodejs.org/dist/index.json",
        Region::Global,
        "Node.js 官方发布站；本机实测 index.json 280ms",
    ),
];

/// 内置的 Temurin（Adoptium）源。
///
/// **这个工具的镜像情况比 Node 差得多**：本机实测三个国内 Adoptium 镜像
/// 全部 403/404，只有官方 API 可用（327ms）。这条事实本身是有价值的
/// —— 它说明"国内镜像"不是普遍真理，按工具分别探测是必要的。
///
/// ## `cn-tuna` / `cn-bfsu` 的 URL 形状是**已知可疑**的
///
/// Adoptium 的 API 返回的是指向 GitHub Releases 的**重定向**，而不是
/// `mirror/api/...` 这种可前缀替换的路径。这两个镜像的目录布局
/// （`<major>/jdk/<arch>/<os>/OpenJDK…zip`）**前缀替换表达不了** ——
/// 它需要把版本号从 API 路径里解析出来，而 `MirrorTemplate` 只有
/// `{url}` / `{file}`。
///
/// 本机实测能给出的证据：
///
/// * BFSU：`/Adoptium/` 是 **403**，但 `/Adoptium/v3/info/available_releases`
///   是 **404** —— 状态码不同，说明 403 只是拒绝目录列举，而 **API 的
///   路径形状在那边确实不存在**；
/// * TUNA：`/Adoptium/` 下**所有**路径都是 403，所以拿不到形状的证据；
/// * 中科大有一个 `/adoptium/`（小写）但目录是**空的**。
///
/// **所以这两个条目确实会作为候选出现，而且大概率会失败。** 早先这里的
/// 注释写的是"不匹配就不出现" —— 那是错的：它们的 `upstream` 就是官方
/// API 前缀，`strip_prefix` 一定匹配。
///
/// ## 顺序：**官方排在最前**（与 Node 表相反，这是有意的）
///
/// Node 表把官方放在最后，因为那里的国内镜像**实测可用**，镜像优先才有
/// 意义。Temurin 表反过来，因为这里的两个镜像**已知形状可疑**：
///
/// * 官方在前时，代价是**零** —— 官方可用，镜像根本不会被碰到；
/// * 官方在后时，代价是**每次 JDK 安装都先白等两次失败**（本机 114ms +
///   90ms），换来的却是两个大概率 404 的请求。
///
/// 它们仍然留在表里而不是删掉，因为 TUNA 的 403 是**整站拒绝**（形状
/// 未被证伪），而官方 API 在有些网络里是被墙的 —— 那种情况下它们是仅有的
/// 长尾希望。`--probe-sources`（决策 40）拿到真实数据后会重新排序，
/// 那时镜像能不能用就有答案了，而不是靠这里的猜测。
const TEMURIN_SOURCES: &[(&str, &str, &str, &str, Region, &str)] = &[
    (
        "official-api",
        "https://api.adoptium.net/",
        "https://api.adoptium.net/",
        "https://api.adoptium.net/v3/info/available_releases",
        Region::Global,
        "Adoptium 官方 API；本机实测 327ms（国内镜像是 403/404）",
    ),
    (
        "cn-tuna",
        "https://api.adoptium.net/",
        "https://mirrors.tuna.tsinghua.edu.cn/Adoptium/",
        // 探测打**我们真正会用的那个路径形状** —— 这比打目录更忠实：
        // 目录 403 只说明"拒绝列举"，而 API 路径 403 说明"这条路走不通"。
        "https://mirrors.tuna.tsinghua.edu.cn/Adoptium/v3/info/available_releases",
        Region::Cn,
        "清华 TUNA；本机实测 403（响应体是反爬页『您访问使用的软件带有非常用软件的特征』，整站拒绝）。注意它的目录布局与 API 路径不同，前缀替换表达不了",
    ),
    (
        "cn-bfsu",
        "https://api.adoptium.net/",
        "https://mirrors.bfsu.edu.cn/Adoptium/",
        "https://mirrors.bfsu.edu.cn/Adoptium/v3/info/available_releases",
        Region::Cn,
        "北外；`/Adoptium/v3/info/available_releases` 本机实测 **403 与 404 都出现过**（两次运行不同）—— 两个码都不是『这个文件在』，而它的目录 `/Adoptium/` 回 403 说明 403 也可能只是拒绝列举。**判决稳定，状态码不稳定**",
    ),
];

/// 按工具取内置源。
#[must_use]
pub fn builtin_sources(tool: &str) -> Vec<Source> {
    let table: &[(&str, &str, &str, &str, Region, &str)] = match tool {
        "node" => NODE_SOURCES,
        "temurin" => TEMURIN_SOURCES,
        _ => &[],
    };
    table
        .iter()
        .map(
            |(id, upstream, mirror_prefix, probe_url, region, note)| Source {
                id: (*id).to_owned(),
                tool: tool.to_owned(),
                upstream: (*upstream).to_owned(),
                mirror_prefix: (*mirror_prefix).to_owned(),
                // 这里先放镜像前缀；`candidates_for` 会替换成具体制品 URL。
                url: (*mirror_prefix).to_owned(),
                probe_url: (*probe_url).to_owned(),
                region: *region,
                note: (*note).to_owned(),
            },
        )
        .collect()
}

/// 把所有候选拼成一张"上游 URL → 候选源列表"的表。
///
/// **顺序就是尝试顺序**，而且它是有理由的：
///
/// 1. **用户的自定义模板最先** —— 用户显式配的东西优先于我们的猜测
///    （企业内网场景下它是唯一能用的）；
/// 2. 然后内置的国内镜像（按实测速度）；
/// 3. 最后官方。**官方排最后不是因为它差，而是因为它是兜底** ——
///    本机实测三个 Temurin 国内镜像全挂，那时官方是唯一能用的。
///
/// `--no-mirror` 会把 2 整个去掉，只留 1 与 3。
#[must_use]
pub fn candidates_for(tool: &str, upstream: &str, config: &MirrorConfig) -> Vec<Source> {
    let mut out = Vec::new();

    // 1) 用户模板。
    for template in &config.mirrors {
        let Some(rendered) = template.apply(upstream, tool) else {
            continue;
        };
        out.push(Source {
            id: format!("custom:{}", template.id),
            tool: tool.to_owned(),
            upstream: upstream.to_owned(),
            mirror_prefix: template.template.clone(),
            url: rendered.clone(),
            // 用户模板没有独立的探测端点，只能用渲染出来的制品 URL 本身。
            // 探测是 range GET（`-r 0-0`），所以**不会真的下整包**。
            probe_url: rendered,
            region: Region::Custom,
            note: if template.crosses_hosts(upstream, &template.template) {
                "用户自定义模板（跨主机）".to_owned()
            } else {
                "用户自定义模板".to_owned()
            },
        });
    }

    // 2) 内置源里**能覆盖这个上游**的那些。
    //
    //    官方也在内置表里（`region == Global`），所以"这个源适用于这个工具吗"
    //    只有一个判断点：`strip_prefix` 过不过。不会出现"官方无条件加进来，
    //    于是加了一个与上游无关的地址"。
    //
    //    镜像是兜在官方**前面**的（官方在表里就排在镜像之后），
    //    所以优先级仍然是"用户模板 → 国内镜像 → 官方"。
    let builtin = builtin_sources(tool);
    let mut official_covered = false;

    for source in &builtin {
        let is_official = source.region == Region::Global;
        if is_official {
            if !config.include_official {
                continue;
            }
        } else if !config.use_builtin_mirrors {
            continue;
        }

        let Some(rest) = upstream.strip_prefix(&source.upstream) else {
            continue;
        };
        if is_official {
            official_covered = true;
        }
        out.push(Source {
            url: format!("{}{rest}", source.mirror_prefix),
            upstream: upstream.to_owned(),
            ..source.clone()
        });
    }

    // 3) 官方兜底：内置表里没有官方条目能覆盖这个上游时补一个。
    //
    //    **`official_covered` 而不是"候选里有没有一个 URL 等于上游"。**
    //    后者会让"官方本来就在候选里"与"官方根本不在表里"两种情况混在一起，
    //    在 `--no-mirror` 下还会得出"什么都不用加"，于是候选列表**空的** ——
    //    一次静默的空下载。
    if config.include_official && !official_covered {
        out.push(Source {
            id: "official".to_owned(),
            tool: tool.to_owned(),
            upstream: upstream.to_owned(),
            mirror_prefix: String::new(),
            url: upstream.to_owned(),
            // 兜底源就是上游本身，探测打制品 URL（range GET，不下整包）。
            probe_url: upstream.to_owned(),
            region: Region::Global,
            note: "上游官方地址（兜底）".to_owned(),
        });
    }

    out
}

/// 一次探测的结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceProbe {
    /// 被探测的源。
    pub source: Source,
    /// HTTP 码（`0` 表示根本没连上）。
    pub status: u16,
    /// 可达吗。
    pub reachable: bool,
    /// 墙钟耗时（毫秒）。**这是选优的依据**。
    pub millis: u64,
    /// 失败原因（可达时是空）。
    pub detail: String,
}

/// 对候选源发一次**小的**真实请求，量出可达性与耗时。
///
/// ## 为什么是真的发请求
///
/// 因为"清单上有"与"这个网络能到"是两件事。本机实测八个国内镜像里
/// 只有一个能用 —— 任何静态判断都会给出错误答案。
///
/// ## 为什么打 `probe_url` 而不是 `url`
///
/// 见 [`Source::probe_url`]：阿里源的**制品前缀**不可列举（目录 404），
/// 而制品本身完全正常。拿前缀去探测会把本机最快的源淘汰掉。
///
/// ## 为什么是 range GET 而不是 HEAD
///
/// 实测发现：有些镜像对 `HEAD` 回 405 而 `GET` 正常。**用 HEAD 测会误判**，
/// 而误判的代价是"把一个能用的源淘汰掉"。
///
/// 而且**不能真的下整包**（Temurin JDK 约 190 MB），所以发
/// [`ByteRange::FirstByte`]（`-r 0-0`）—— 只要一个字节。
/// 真正会失败的东西（DNS、TLS、403、404、超时）在第一个字节之前就失败了。
pub fn probe_sources<T: Transport + ?Sized>(
    transport: &T,
    sources: &[Source],
    timeout: Duration,
) -> Vec<SourceProbe> {
    sources
        .iter()
        .map(|source| {
            let watch = std::time::Instant::now();
            let outcome = transport.fetch(
                &source.probe_url,
                Some(crate::transport::ByteRange::FirstByte),
                timeout,
            );
            let millis = u64::try_from(watch.elapsed().as_millis()).unwrap_or(u64::MAX);
            match outcome {
                Ok(fetched) => {
                    // **不假定状态码。** 一个不认区间请求的服务器会把
                    // `-r 0-0` 当普通 GET，回 200 + 整个文件 —— 那意味着
                    // "探测"实际上把 190 MB 的 JDK 下了一遍。把真实状态码
                    // 记下来，这种源才会在报告里露出来。
                    let status = fetched.status.unwrap_or(0);
                    let bytes = fetched.bytes.len();
                    let ignored_range = status == 200 && bytes > 1;
                    SourceProbe {
                        source: source.clone(),
                        status,
                        reachable: true,
                        millis,
                        detail: if ignored_range {
                            format!(
                                "取回 {bytes} 字节：**这个源不认区间请求**，探测会下整包（响应 200 而不是 206）"
                            )
                        } else {
                            format!("取回 {bytes} 字节（区间请求，{status}）")
                        },
                    }
                }
                Err(error) => {
                    let status = match &error {
                        DownloadError::Http { status, .. } => *status,
                        _ => 0,
                    };
                    SourceProbe {
                        source: source.clone(),
                        status,
                        reachable: false,
                        millis,
                        detail: error.to_string(),
                    }
                }
            }
        })
        .collect()
}

/// 把探测结果排成"该先试谁"的顺序：**可达的在前（快的更前），不可达的在后**。
///
/// 不可达的**不删掉**：它的失败详情是用户诊断网络的线索
/// （"清华 403" 说明有东西在拦，而不是源本身没有）。
#[must_use]
pub fn rank_by_probe(probes: &[SourceProbe]) -> Vec<SourceProbe> {
    let mut ranked = probes.to_vec();
    ranked.sort_by(|left, right| {
        right
            .reachable
            .cmp(&left.reachable)
            .then(left.millis.cmp(&right.millis))
            .then(left.source.id.cmp(&right.source.id))
    });
    ranked
}

/// 从探测结果里挑出可用的源，按快慢排。
#[must_use]
pub fn usable_sources(probes: &[SourceProbe]) -> Vec<Source> {
    rank_by_probe(probes)
        .into_iter()
        .filter(|probe| probe.reachable)
        .map(|probe| probe.source)
        .collect()
}

/// 把一组源按 `id` 去重，保留第一次出现（即最高优先级）。
#[must_use]
pub fn dedupe_by_url(sources: &[Source]) -> Vec<Source> {
    let mut seen: BTreeMap<String, ()> = BTreeMap::new();
    sources
        .iter()
        .filter(|source| seen.insert(source.url.clone(), ()).is_none())
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_extraction_ignores_credentials_and_port() {
        assert_eq!(host_of("https://nodejs.org/dist/x.zip"), "nodejs.org");
        assert_eq!(
            host_of("https://user:pass@example.com:8443/x"),
            "example.com"
        );
        assert_eq!(host_of("HTTPS://EXAMPLE.COM/x"), "example.com");
        assert_eq!(host_of("not a url"), "not a url");
    }

    #[test]
    fn a_template_that_does_not_match_is_skipped_not_an_error() {
        let template = MirrorTemplate {
            id: "corp".to_owned(),
            match_prefix: "https://internal.corp/".to_owned(),
            template: "https://cache.corp/{url}".to_owned(),
            tool: String::new(),
            forward_credentials: false,
        };
        // 前缀不匹配 → 跳过（`None`），不是失败。
        assert_eq!(
            template.apply("https://nodejs.org/dist/v24/x.zip", "node"),
            None
        );
        assert_eq!(
            template
                .apply("https://internal.corp/x.zip", "node")
                .as_deref(),
            Some("https://cache.corp/https://internal.corp/x.zip")
        );
    }

    #[test]
    fn a_template_can_be_scoped_to_one_tool() {
        let template = MirrorTemplate {
            id: "node-only".to_owned(),
            match_prefix: String::new(),
            template: "https://proxy/{file}".to_owned(),
            tool: "node".to_owned(),
            forward_credentials: false,
        };
        assert!(
            template
                .apply("https://nodejs.org/dist/x.zip", "node")
                .is_some()
        );
        assert!(
            template
                .apply("https://api.adoptium.net/x.zip", "temurin")
                .is_none(),
            "限定工具的模板不该套到别的工具上"
        );
    }

    #[test]
    fn candidates_put_user_templates_first_then_mirrors_then_official() {
        let config = MirrorConfig {
            mirrors: vec![MirrorTemplate {
                id: "corp".to_owned(),
                match_prefix: "https://nodejs.org/dist/".to_owned(),
                template: "https://cache.corp/{file}".to_owned(),
                tool: String::new(),
                forward_credentials: false,
            }],
            use_builtin_mirrors: true,
            include_official: true,
        };
        let candidates =
            candidates_for("node", "https://nodejs.org/dist/v24.19.0/node.zip", &config);
        let ids: Vec<&str> = candidates.iter().map(|source| source.id.as_str()).collect();

        assert_eq!(
            ids.first(),
            Some(&"custom:corp"),
            "用户模板必须最先：{ids:?}"
        );
        assert_eq!(ids.last(), Some(&"official"), "官方必须是兜底：{ids:?}");
        // 内置镜像里能对上的只有 cn-npmmirror（它声明的前缀是 nodejs.org/dist 的镜像形态）
        assert!(
            candidates.iter().any(|source| source.id == "cn-npmmirror"),
            "内置镜像必须被展开成具体 URL：{candidates:#?}"
        );
        // 每个候选都必须是**具体的 URL**，不能还是前缀。
        for source in &candidates {
            assert!(
                source.url.ends_with("node.zip"),
                "候选源必须指向具体制品：{}",
                source.url
            );
        }
    }

    #[test]
    fn turning_off_mirrors_leaves_only_the_official_url() {
        let config = MirrorConfig {
            mirrors: Vec::new(),
            use_builtin_mirrors: false,
            include_official: true,
        };
        let candidates = candidates_for("node", "https://nodejs.org/dist/v24/x.zip", &config);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].url, "https://nodejs.org/dist/v24/x.zip");
    }

    #[test]
    fn a_source_that_does_not_cover_this_upstream_is_not_offered() {
        // `temurin` 的源不该被拿来下 node 的包。
        let config = MirrorConfig::default();
        let candidates = candidates_for("temurin", "https://api.adoptium.net/v3/binary/x", &config);
        assert!(
            candidates
                .iter()
                .all(|source| !source.id.starts_with("cn-npmmirror")),
            "跨工具的镜像不该被套用：{candidates:#?}"
        );
        assert!(
            candidates.iter().any(|source| source.id == "official-api"),
            "官方 Adoptium API 必须在内：{candidates:#?}"
        );
        assert!(
            !candidates.iter().any(|source| source.id == "official"),
            "**不该再补一个叫 `official` 的兜底条目** —— Adoptium 的官方 \
             在表里叫 `official-api`，再补一个就是同一份地址出现两次，\
             用户看到两次失败的同一个源：{candidates:#?}"
        );
    }

    #[test]
    fn ranking_puts_reachable_and_fast_first_but_keeps_failures() {
        let probes = vec![
            SourceProbe {
                source: Source {
                    id: "slow".to_owned(),
                    tool: "node".to_owned(),
                    upstream: String::new(),
                    mirror_prefix: String::new(),
                    url: "https://slow/".to_owned(),
                    probe_url: "https://slow/".to_owned(),
                    region: Region::Cn,
                    note: String::new(),
                },
                status: 200,
                reachable: true,
                millis: 900,
                detail: String::new(),
            },
            SourceProbe {
                source: Source {
                    id: "dead".to_owned(),
                    tool: "node".to_owned(),
                    upstream: String::new(),
                    mirror_prefix: String::new(),
                    url: "https://dead/".to_owned(),
                    probe_url: "https://dead/".to_owned(),
                    region: Region::Cn,
                    note: String::new(),
                },
                status: 403,
                reachable: false,
                millis: 10,
                detail: "HTTP 403".to_owned(),
            },
            SourceProbe {
                source: Source {
                    id: "fast".to_owned(),
                    tool: "node".to_owned(),
                    upstream: String::new(),
                    mirror_prefix: String::new(),
                    url: "https://fast/".to_owned(),
                    probe_url: "https://fast/".to_owned(),
                    region: Region::Cn,
                    note: String::new(),
                },
                status: 200,
                reachable: true,
                millis: 100,
                detail: String::new(),
            },
        ];

        let ranked = rank_by_probe(&probes);
        let ids: Vec<&str> = ranked
            .iter()
            .map(|probe| probe.source.id.as_str())
            .collect();
        assert_eq!(ids, vec!["fast", "slow", "dead"], "可达的在前且快的更前");

        // 不可达的**不能**被删掉 —— 它的失败详情是用户诊断网络的线索。
        assert_eq!(ranked.len(), 3);
        // 但挑"能用的"时它要被排除。
        let usable = usable_sources(&probes);
        assert_eq!(
            usable
                .iter()
                .map(|source| source.id.as_str())
                .collect::<Vec<_>>(),
            vec!["fast", "slow"]
        );
    }

    /// 造一个只用于探测的源。
    fn probe_only(id: &str, probe_url: &str) -> Source {
        Source {
            id: id.to_owned(),
            tool: "node".to_owned(),
            upstream: String::new(),
            mirror_prefix: String::new(),
            url: probe_url.to_owned(),
            probe_url: probe_url.to_owned(),
            region: Region::Cn,
            note: String::new(),
        }
    }

    #[test]
    fn every_builtin_probe_target_is_a_file_not_a_directory() {
        // **这条守的是"探测别把整份目录列举下下来"。**
        //
        // 一个不认区间请求的服务器会把 `-r 0-0` 当普通 GET。目标是目录时，
        // 本机实测真的发生过：清华 200 + 155794 字节、腾讯 200 + 115893 字节
        // —— 为了问一句"能不能到"，下了整份列举，而且那份列举回答的是
        // "这个目录能不能列"，不是"制品路径对不对"。
        for tool in ["node", "temurin"] {
            for source in builtin_sources(tool) {
                assert!(
                    !source.probe_url.ends_with('/'),
                    "{} 的 probe_url 是个目录，会诱使服务器整份列出来：{}",
                    source.id,
                    source.probe_url
                );
                assert!(
                    source.probe_url.starts_with(&source.mirror_prefix),
                    "{} 的 probe_url 必须落在它自己的镜像前缀下（否则测的是别人）：{} vs {}",
                    source.id,
                    source.probe_url,
                    source.mirror_prefix
                );
            }
        }
    }

    #[test]
    fn temurin_mirrors_do_appear_as_candidates_even_though_their_layout_is_suspect() {
        // **这条测试存在的唯一目的是防止有人按错误的直觉"修"回去。**
        //
        // `TEMURIN_SOURCES` 的注释一度写着"布局对不上所以不会出现"，
        // 那是错的：那两个条目的 `upstream` 就是官方 API 前缀，
        // `strip_prefix` 一定匹配。它们**会**出现，也**大概率会失败**，
        // 而那是有意接受的代价（见 `TEMURIN_SOURCES` 的文档）。
        let candidates = candidates_for(
            "temurin",
            "https://api.adoptium.net/v3/binary/latest/21/ga/windows/x64/jdk/hotspot/normal/eclipse",
            &MirrorConfig::default(),
        );
        let ids: Vec<&str> = candidates.iter().map(|source| source.id.as_str()).collect();
        assert!(
            ids.contains(&"cn-tuna") && ids.contains(&"cn-bfsu"),
            "它们会作为候选出现（探测负责淘汰它们）：{ids:?}"
        );
        // **官方在 Temurin 表里排最前，与 Node 表相反**（见 `TEMURIN_SOURCES`
        // 的文档）。这条断言把它钉住：谁要是"为了对称"把官方挪到后面，
        // 就会让每次 JDK 安装先白等两次注定失败的请求。
        assert_eq!(ids.first(), Some(&"official-api"), "{ids:?}");
    }

    #[test]
    fn a_probe_reports_the_status_the_server_actually_sent() {
        let transport =
            crate::transport::FakeTransport::new().serves("https://ok/", vec![0u8, 1, 2, 3]);
        let probes = probe_sources(
            &transport,
            &[probe_only("ok", "https://ok/x.zip")],
            Duration::from_secs(5),
        );

        assert!(probes[0].reachable);
        assert_eq!(
            probes[0].status, 206,
            "服务器回了 206，报告就该写 206（**不是写死 206**）"
        );
        assert_eq!(
            probes[0].source.url, "https://ok/x.zip",
            "探测不该改动源本身的 URL"
        );
    }

    #[test]
    fn a_source_that_ignores_range_requests_is_flagged_not_called_healthy() {
        // 一个把 `-r 0-0` 当普通 GET 的服务器：回 200 + **整包**。
        // 探测打到它上面，代价是"为了问一句能不能到，把 190 MB 下完了"。
        let payload = vec![7u8; 4096];
        let transport = crate::transport::FakeTransport::new().route(
            "https://bad/",
            crate::transport::FakeOutcome::IgnoresRange(payload.clone()),
        );
        let probes = probe_sources(
            &transport,
            &[probe_only("bad", "https://bad/x.zip")],
            Duration::from_secs(5),
        );

        // 它**确实可达**，这一点不能报错 —— 报错会让用户以为网络坏了。
        assert!(probes[0].reachable, "它到得了，只是不尊重区间");
        assert_eq!(probes[0].status, 200, "真实状态码是 200，报告就该是 200");
        assert!(
            probes[0].detail.contains("不认区间请求"),
            "必须点出这件事，否则用户只会看到一个'正常'的源：{}",
            probes[0].detail
        );
        // 而且必须说清楚代价。
        assert!(
            probes[0].detail.contains("4096"),
            "应当报出实际取回的字节数：{}",
            probes[0].detail
        );
    }

    #[test]
    fn dedupe_keeps_the_first_occurrence() {
        let source = |id: &str, url: &str| Source {
            id: id.to_owned(),
            tool: "node".to_owned(),
            upstream: String::new(),
            mirror_prefix: String::new(),
            url: url.to_owned(),
            probe_url: url.to_owned(),
            region: Region::Cn,
            note: String::new(),
        };
        // `candidates_for` 里"官方"那一步就是靠这个避免重复插入的。
        let deduped = dedupe_by_url(&[
            source("first", "https://a/"),
            source("second", "https://a/"),
            source("third", "https://b/"),
        ]);
        assert_eq!(
            deduped
                .iter()
                .map(|source| source.id.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "third"]
        );
    }
}
