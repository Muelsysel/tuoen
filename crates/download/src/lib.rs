//! tuoen 的下载层。
//!
//! ## 这一层的四件事，按重要性排
//!
//! 1. **如实报告失败原因。** 票据 #4 要求区分"网络不可达 / 镜像失败 / 哈希不符"。
//!    这三者对用户的下一步动作完全不同，所以 [`error::FailureKind`] 是公开契约。
//! 2. **SHA256 必须验，不符必须停。** 见 [`error::DownloadError::should_try_next_source`]：
//!    哈希不符**不触发换源** —— 换源去拿"另一份同样的制品"有可能拿到哈希对得上的
//!    （因为只有其中一个镜像被污染），但那是在**掩盖一次完整性事件**。
//! 3. **默认在中国大陆网络上就快。** 内置源优选 + 自定义模板 + 实测探测，见 [`source`]。
//! 4. **可缓存、可离线。** 内容寻址的缓存（[`cache`]）+ 本地归档文件作为输入。
//!
//! ## 不托管任何二进制
//!
//! 镜像模板只**指向**别处（决策 34）。不托管意味着不承担再分发责任 ——
//! 这是这个自建引擎能安全落地的前提。
//!
//! ## 测试不碰真实网络
//!
//! 层的语义全部靠 [`transport::FakeTransport`] 测（零 IO）；
//! "curl 真的能取回字节"由 `tests/loopback.rs` 的环回 HTTP 服务器覆盖
//! （真实 HTTP、真实进程，但不出本机）。

pub mod cache;
pub mod error;
pub mod sha256;
pub mod source;
pub mod transport;

use std::path::{Path, PathBuf};
use std::time::Duration;

pub use cache::{Cache, CacheMeta, CacheStats};
pub use error::{DownloadError, FailureKind};
pub use sha256::{Sha256, checksums_match, normalize_checksum, sha256_hex};
pub use source::{
    MirrorConfig, MirrorTemplate, Region, Source, SourceProbe, builtin_sources, candidates_for,
    rank_by_probe, usable_sources,
};
pub use transport::{FetchOutcome, HttpTransport, Transport};

/// 默认的单次请求超时。
///
/// **60 秒是有意的**：Temurin JDK 约 190 MB，在这个网络上从官方 API
/// （实测 1253ms 首字节）下来需要几十秒。设成 30 秒会让一个正常的大包
/// 被误判为超时。真正"到不了"的情况由 `--connect-timeout` 兜住（快得多）。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// 一个要取到本地的制品。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    /// 逻辑名（`node-24.19.0-win-x64`），用于文件名与报告。
    pub name: String,
    /// 它属于哪个工具（决定用哪组镜像）。
    pub tool: String,
    /// 上游 URL。
    pub url: String,
    /// 登记的 SHA256（已规整成小写、无前缀）。
    pub sha256: String,
    /// 登记的算法名（原样，用于报"不支持"）。
    pub algorithm: String,
}

impl Artifact {
    /// 从 recipe 的字段建一个。
    ///
    /// **`algorithm` 原样保留**：只有 `sha256` 会被 [`normalize_checksum`] 规整，
    /// 别的算法名留着是为了在 [`DownloadError::UnsupportedAlgorithm`] 里
    /// 报出用户真正写的那个词。
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        tool: impl Into<String>,
        url: impl Into<String>,
        algorithm: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        let algorithm = algorithm.into();
        Self {
            name: name.into(),
            tool: tool.into(),
            url: url.into(),
            sha256: normalize_checksum(&value.into()),
            algorithm,
        }
    }

    /// 检查登记的算法是不是我们能算的那个。
    ///
    /// **不认识就拒绝**，不"当成某种哈希放过去" —— 那是安全漏洞。
    ///
    /// # Errors
    ///
    /// 算法名不是 `sha256`（大小写、`sha256:` 前缀都容忍）。
    pub fn check_algorithm(&self) -> Result<(), DownloadError> {
        // 顺序有讲究：**先削前缀与冒号，再去掉 `-` / `_`**。
        //
        // 反过来的话 `sha256:` 去掉 `-`/`_` 之后还是 `sha256:`，
        // 与 `"sha256"` 比不相等 —— 于是 `sha256:` 这种写法被拒。
        // 上游清单里两种写法都有，拒掉一种等于拒掉一个好制品。
        let normalized = self
            .algorithm
            .trim()
            .to_lowercase()
            .trim_end_matches(':')
            .replace(['-', '_'], "");
        let ok = normalized == "sha256";
        if ok {
            Ok(())
        } else {
            Err(DownloadError::UnsupportedAlgorithm {
                algorithm: self.algorithm.clone(),
            })
        }
    }
}

/// 一次取的完整结果。
#[derive(Debug, Clone)]
pub struct FetchReport {
    /// 制品现在在哪。
    pub path: PathBuf,
    /// 实际算出来的 SHA256（**不是**登记的 —— 相等的，但这是实测值）。
    pub sha256: String,
    /// 多少字节。
    pub bytes: u64,
    /// 谁服务的（源 id）。
    pub source_id: String,
    /// 实际用的 URL（可能因重定向而不同于候选 URL）。
    pub source_url: String,
    /// 这是缓存命中还是真的下了。
    pub from_cache: bool,
    /// **每一次尝试**，按顺序。失败过的话这里是诊断材料。
    pub attempts: Vec<Attempt>,
}

impl FetchReport {
    /// 这次取失败过几次（成功之前的尝试）。
    #[must_use]
    pub fn failures_before_success(&self) -> usize {
        self.attempts.iter().filter(|attempt| !attempt.ok).count()
    }
}

/// 一次尝试。
#[derive(Debug, Clone)]
pub struct Attempt {
    /// 源 id。
    pub source_id: String,
    /// 试的 URL。
    pub url: String,
    /// 成了吗。
    pub ok: bool,
    /// 没成的原因。
    pub error: Option<String>,
    /// 这一类的失败。
    pub kind: Option<FailureKind>,
    /// 耗时（毫秒）。
    pub millis: u64,
}

/// 流式算一个文件的 SHA256。
///
/// **流式而不是读进内存**：制品是几百 MB（Temurin JDK 约 190 MB），
/// 把它整个读进来只为了算哈希是不可接受的。
///
/// 一次读 1 MiB：够大以摊薄系统调用，够小以不去挤别人的内存。
///
/// # Errors
///
/// 文件打不开或读到一半出错。
pub fn verify_file(path: &Path) -> Result<String, DownloadError> {
    use std::io::Read as _;

    let mut file = std::fs::File::open(path).map_err(|_| DownloadError::LocalFileUnavailable {
        path: path.to_path_buf(),
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| DownloadError::LocalFileUnavailable {
                path: path.to_path_buf(),
            })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finish())
}

/// 算一个文件的实际大小。
fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

/// 校验一个已经在本地的文件。
///
/// # Errors
///
/// 算法不支持、文件读不了、或哈希不符。
pub fn verify_local(path: &Path, artifact: &Artifact) -> Result<String, DownloadError> {
    artifact.check_algorithm()?;
    let actual = verify_file(path)?;
    if checksums_match(&artifact.sha256, &actual) {
        Ok(actual)
    } else {
        Err(DownloadError::ChecksumMismatch {
            expected: artifact.sha256.clone(),
            actual,
            url: artifact.url.clone(),
            bytes: file_size(path),
        })
    }
}

/// 把字节写到一个文件，**先暂存再改名**。
///
/// 直接写目标的话，中断会留下半份文件 —— 而半份文件比"没有文件"更糟，
/// 因为它会让下一次 `lookup` 或用户的检查把它当成一个完整的制品。
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), DownloadError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| DownloadError::CacheUnavailable {
            path: parent.to_path_buf(),
            source: error,
        })?;
    }
    let staging = path.with_extension("part");
    std::fs::write(&staging, bytes).map_err(|error| DownloadError::CacheUnavailable {
        path: staging.clone(),
        source: error,
    })?;
    std::fs::rename(&staging, path).map_err(|error| {
        let _ = std::fs::remove_file(&staging);
        DownloadError::CacheUnavailable {
            path: path.to_path_buf(),
            source: error,
        }
    })
}

/// 下载的配置。
#[derive(Debug, Clone)]
pub struct FetchOptions {
    /// 镜像配置（用户模板 + 是否用内置镜像 + 是否含官方）。
    pub mirrors: MirrorConfig,
    /// 单次请求超时。
    pub timeout: Duration,
    /// 缓存。
    pub cache: Cache,
    /// 要不要用缓存（`--no-cache`）。
    pub use_cache: bool,
    /// 要不要把结果收进缓存。
    pub store_in_cache: bool,
    /// 下载完把制品留在哪个目录（`None` 表示只放缓存 + 返回缓存路径）。
    pub destination: Option<PathBuf>,
    /// 每个源最多试几次。
    ///
    /// **默认 1**：换源比重试同一个源更可能成功（本机实测清华 403 是稳定的，
    /// 重试三次只是让用户多等三次）。把它调大只对"偶发抖动"有意义。
    pub attempts_per_source: usize,
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            mirrors: MirrorConfig::default(),
            timeout: DEFAULT_TIMEOUT,
            cache: Cache::at_default_location(),
            use_cache: true,
            store_in_cache: true,
            destination: None,
            attempts_per_source: 1,
        }
    }
}

/// 取一个制品到本地。
///
/// ## 顺序
///
/// 1. **先看缓存。** 命中就验一次哈希（防止缓存被动手脚或磁盘损坏），
///    然后直接返回 —— 不产生任何网络流量。
/// 2. 否则按 [`source::candidates_for`] 的顺序逐个源试。
/// 3. 每一次尝试都记进 [`FetchReport::attempts`]。
/// 4. **哈希不符立刻停**（见 [`DownloadError::should_try_next_source`]）。
/// 5. 成功之后收进缓存（除非 `store_in_cache` 是 `false`）。
///
/// # Errors
///
/// [`DownloadError::AllSourcesFailed`]（全都试过）、
/// [`DownloadError::ChecksumMismatch`]（拿到手但不对）、
/// [`DownloadError::UnsupportedAlgorithm`]。
pub fn fetch<T: Transport + ?Sized>(
    transport: &T,
    artifact: &Artifact,
    options: &FetchOptions,
) -> Result<FetchReport, DownloadError> {
    artifact.check_algorithm()?;

    // ── 1. 缓存 ──────────────────────────────────────────────────────────────
    if options.use_cache {
        if let Some(hit) = options.cache.lookup(&artifact.sha256) {
            // **命中也要验。** "写进去的时候验过"不能替代"用的时候验" ——
            // 磁盘会坏、文件会被改、缓存目录会被同步工具弄乱。
            //
            // 但不符时**不报 ChecksumMismatch**：那会让用户以为上游有问题，
            // 而实际是本地缓存坏了。删掉缓存项、继续下载，才是对的行为。
            match verify_file(&hit) {
                Ok(actual) if checksums_match(&artifact.sha256, &actual) => {
                    let bytes = file_size(&hit);
                    let meta = options.cache.read_meta(&artifact.sha256);
                    return Ok(FetchReport {
                        path: hit,
                        sha256: actual,
                        bytes,
                        source_id: meta
                            .as_ref()
                            .map_or_else(|| "cache".to_owned(), |meta| meta.source_id.clone()),
                        source_url: meta
                            .as_ref()
                            .map_or_else(|| artifact.url.clone(), |meta| meta.source_url.clone()),
                        from_cache: true,
                        attempts: Vec::new(),
                    });
                }
                _ => {
                    // 坏缓存项：拿掉它，别留着继续骗人。
                    let _ = std::fs::remove_file(&hit);
                }
            }
        }
    } else {
        // 用户要求不用缓存 —— 但仍然要能复用"已经在目的地的那份文件"。
    }

    // ── 2. 逐个源试 ──────────────────────────────────────────────────────────
    let candidates = source::dedupe_by_url(&source::candidates_for(
        &artifact.tool,
        &artifact.url,
        &options.mirrors,
    ));

    let mut attempts: Vec<Attempt> = Vec::new();
    let mut failures: Vec<DownloadError> = Vec::new();
    let per_source = options.attempts_per_source.max(1);

    for candidate in &candidates {
        for round in 0..per_source {
            let watch = std::time::Instant::now();
            // 真下载**没有区间**（`None`）—— 区间只用于探测。
            let outcome = transport.fetch(&candidate.url, None, options.timeout);
            let millis = u64::try_from(watch.elapsed().as_millis()).unwrap_or(u64::MAX);

            match outcome {
                Ok(fetched) => {
                    let actual = sha256_hex(&fetched.bytes);
                    if !checksums_match(&artifact.sha256, &actual) {
                        let error = DownloadError::ChecksumMismatch {
                            expected: artifact.sha256.clone(),
                            actual,
                            url: candidate.url.clone(),
                            bytes: fetched.bytes.len() as u64,
                        };
                        attempts.push(Attempt {
                            source_id: candidate.id.clone(),
                            url: candidate.url.clone(),
                            ok: false,
                            error: Some(error.to_string()),
                            kind: Some(FailureKind::Integrity),
                            millis,
                        });
                        // **立刻返回，不继续试。** 见 `should_try_next_source` 的说明。
                        failures.push(error);
                        return Err(if failures.len() == 1 {
                            failures.pop().expect("刚放进去的")
                        } else {
                            DownloadError::AllSourcesFailed {
                                attempts: attempts.len(),
                                failures,
                            }
                        });
                    }

                    attempts.push(Attempt {
                        source_id: candidate.id.clone(),
                        url: fetched
                            .final_url
                            .clone()
                            .unwrap_or_else(|| candidate.url.clone()),
                        ok: true,
                        error: None,
                        kind: None,
                        millis,
                    });

                    // 落在哪：优先用户给的目的地，否则缓存。
                    let landed = if let Some(destination) = &options.destination {
                        let target = destination.join(format!(
                            "{}-{}",
                            artifact.name,
                            artifact.sha256.get(..12).unwrap_or("unknown")
                        ));
                        write_atomically(&target, &fetched.bytes)?;
                        target
                    } else {
                        let target = options.cache.artifact_path(&artifact.sha256);
                        write_atomically(&target, &fetched.bytes)?;
                        target
                    };

                    if options.store_in_cache {
                        let meta = CacheMeta {
                            sha256: artifact.sha256.clone(),
                            source_url: candidate.url.clone(),
                            source_id: candidate.id.clone(),
                            bytes: fetched.bytes.len() as u64,
                        };
                        // **缓存失败不让下载失败。** 缓存是优化，不是功能。
                        let _ = options.cache.store(&artifact.sha256, &landed, &meta);
                    }

                    return Ok(FetchReport {
                        path: landed,
                        sha256: actual,
                        bytes: fetched.bytes.len() as u64,
                        source_id: candidate.id.clone(),
                        source_url: candidate.url.clone(),
                        from_cache: false,
                        attempts,
                    });
                }
                Err(error) => {
                    let kind = error.kind();
                    let retryable = error.should_try_next_source();
                    attempts.push(Attempt {
                        source_id: candidate.id.clone(),
                        url: candidate.url.clone(),
                        ok: false,
                        error: Some(error.to_string()),
                        kind: Some(kind),
                        millis,
                    });

                    // 完整性类失败**立刻停**，不换源（见模块文档第 2 条）。
                    if !retryable {
                        failures.push(error);
                        return Err(if failures.len() == 1 {
                            failures.pop().expect("刚放进去的")
                        } else {
                            DownloadError::AllSourcesFailed {
                                attempts: attempts.len(),
                                failures,
                            }
                        });
                    }

                    let last_round = round + 1 == per_source;
                    if last_round {
                        failures.push(error);
                    }
                }
            }
        }
    }

    Err(DownloadError::AllSourcesFailed {
        attempts: attempts.len(),
        failures,
    })
}

/// 用一份**已经在本地**的归档文件当输入。
///
/// 这是"为将来的离线 bundle 留出接口"（票据 #4 的验收项之一）。
/// 它**不下载任何东西**，但会验哈希 —— 否则"离线安装"就成了唯一一条
/// 绕过完整性检查的路径，而那正是最需要检查的场景（U 盘拷贝）。
///
/// # Errors
///
/// 文件不存在、算法不支持、哈希不符。
pub fn use_local_file(path: &Path, artifact: &Artifact) -> Result<FetchReport, DownloadError> {
    if !path.is_file() {
        return Err(DownloadError::LocalFileUnavailable {
            path: path.to_path_buf(),
        });
    }
    let actual = verify_local(path, artifact)?;
    let bytes = file_size(path);
    Ok(FetchReport {
        path: path.to_path_buf(),
        sha256: actual,
        bytes,
        source_id: "local".to_owned(),
        source_url: path.to_string_lossy().into_owned(),
        from_cache: false,
        attempts: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::FakeTransport;

    const HELLO: &[u8] = b"hello tuoen";
    /// `sha256_hex(HELLO)`
    fn hello_sha() -> String {
        sha256_hex(HELLO)
    }

    fn artifact(sha: &str) -> Artifact {
        Artifact::new(
            "node-24.19.0-win-x64",
            "node",
            "https://nodejs.org/dist/v24.19.0/node-v24.19.0-win-x64.zip",
            "sha256",
            sha,
        )
    }

    /// 一个自清理的临时目录。
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let unique = format!(
                "tuoen-fetch-test-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            );
            let path = std::env::temp_dir().join(unique);
            std::fs::create_dir_all(&path).expect("建临时目录");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn options_in(temp: &TempDir) -> FetchOptions {
        FetchOptions {
            cache: Cache::new(temp.path()),
            // 只留官方，让候选列表短而可预测。
            mirrors: MirrorConfig {
                mirrors: Vec::new(),
                use_builtin_mirrors: false,
                include_official: true,
            },
            ..FetchOptions::default()
        }
    }

    #[test]
    fn a_successful_fetch_verifies_and_stores() {
        let temp = TempDir::new("ok");
        let transport = FakeTransport::new().serves("https://nodejs.org/", HELLO.to_vec());
        let report =
            fetch(&transport, &artifact(&hello_sha()), &options_in(&temp)).expect("应当成功");

        assert_eq!(report.bytes, HELLO.len() as u64);
        assert_eq!(report.sha256, hello_sha());
        assert_eq!(report.source_id, "official");
        assert!(!report.from_cache);
        assert_eq!(report.failures_before_success(), 0);
        assert_eq!(std::fs::read(&report.path).expect("读回"), HELLO);
    }

    #[test]
    fn a_checksum_mismatch_stops_instead_of_falling_back_to_another_mirror() {
        // **这是这一层最重要的安全判断。**
        //
        // 换源去拿"另一份同样的制品"有可能拿到哈希对得上的（因为只有其中一个
        // 镜像被污染），但那是在掩盖一次完整性事件。正确行为是停下来、报出来。
        let temp = TempDir::new("mismatch");
        let transport = FakeTransport::new()
            // 第一个源给的是被改过的字节
            .serves("https://cdn.npmmirror.com/", b"tampered payload".to_vec())
            // 第二个源给的是"对"的（模拟"只有一个镜像被污染"）
            .serves("https://nodejs.org/", HELLO.to_vec());

        let options = FetchOptions {
            cache: Cache::new(temp.path()),
            mirrors: MirrorConfig::default(),
            ..FetchOptions::default()
        };

        let error =
            fetch(&transport, &artifact(&hello_sha()), &options).expect_err("哈希不符必须失败");

        assert_eq!(error.kind(), FailureKind::Integrity);
        assert!(!error.should_try_next_source(), "完整性失败不该触发换源");
        assert!(
            matches!(error, DownloadError::ChecksumMismatch { .. }),
            "必须是 ChecksumMismatch 而不是 AllSourcesFailed：{error:?}"
        );

        // **而且它没有去试官方源。** 这才是"停下来"的可观测含义。
        let attempts = transport.attempts();
        assert_eq!(attempts.len(), 1, "只该试一个源就停：{attempts:?}");
        assert!(
            !attempts.iter().any(|url| url.contains("nodejs.org")),
            "不该回退到官方源去'再拿一份'：{attempts:?}"
        );
    }

    #[test]
    fn the_error_names_both_the_expected_and_the_actual_hash() {
        // 只说"校验失败"会让用户无法判断"是我本地的问题"还是"上游换了包"。
        let temp = TempDir::new("naming");
        let transport = FakeTransport::new().serves("https://nodejs.org/", b"wrong".to_vec());
        let error =
            fetch(&transport, &artifact(&hello_sha()), &options_in(&temp)).expect_err("必须失败");

        let text = error.to_string();
        assert!(text.contains(&hello_sha()), "必须给期望值：{text}");
        assert!(text.contains(&sha256_hex(b"wrong")), "必须给实际值：{text}");
    }

    #[test]
    fn a_failing_mirror_falls_back_to_the_next_source() {
        let temp = TempDir::new("fallback");
        let transport = FakeTransport::new()
            // 阿里：连不上
            .unreachable("https://cdn.npmmirror.com/")
            // 腾讯：404
            .fails_with_status("https://mirrors.cloud.tencent.com/", 404)
            // 清华、中科大：也是 404（本机实测它们就是 403/404）
            .fails_with_status("https://mirrors.tuna.tsinghua.edu.cn/", 403)
            .fails_with_status("https://mirrors.ustc.edu.cn/", 404)
            // 官方：有
            .serves("https://nodejs.org/", HELLO.to_vec());

        let options = FetchOptions {
            cache: Cache::new(temp.path()),
            // 显式写全，不靠 `MirrorConfig::default()` 的隐式默认 ——
            // 这条测试断言的是"候选顺序 = 内置表顺序，官方垫底"，
            // 那个顺序本身才是被测对象。
            mirrors: MirrorConfig {
                mirrors: Vec::new(),
                use_builtin_mirrors: true,
                include_official: true,
            },
            ..FetchOptions::default()
        };
        let report = fetch(&transport, &artifact(&hello_sha()), &options).expect("应当回退成功");

        assert_eq!(report.source_id, "official");
        assert_eq!(
            report.failures_before_success(),
            4,
            "四个内置镜像全部失败之后才轮到官方"
        );
        // 每一次尝试都要在报告里，**按顺序** —— 用户需要看到
        // "阿里连不上、腾讯 404、清华 403、中科大 404"，最后官方成功。
        let ids: Vec<&str> = report
            .attempts
            .iter()
            .map(|attempt| attempt.source_id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec![
                "cn-npmmirror",
                "cn-tencent",
                "cn-tuna",
                "cn-ustc",
                "official"
            ],
            "候选顺序必须与内置表的顺序一致，官方垫底"
        );
        let kinds: Vec<Option<FailureKind>> =
            report.attempts.iter().map(|attempt| attempt.kind).collect();
        assert_eq!(
            kinds,
            vec![
                Some(FailureKind::SourceUnavailable),
                Some(FailureKind::SourceUnavailable),
                Some(FailureKind::SourceUnavailable),
                Some(FailureKind::SourceUnavailable),
                None
            ]
        );
        assert_eq!(report.attempts[0].source_id, "cn-npmmirror");
        assert_eq!(report.attempts[4].source_id, "official");
    }

    #[test]
    fn all_sources_failing_reports_every_attempt() {
        let temp = TempDir::new("allfail");
        let transport = FakeTransport::new(); // 没配路由 = 全都不可达
        let options = FetchOptions {
            cache: Cache::new(temp.path()),
            mirrors: MirrorConfig::default(),
            ..FetchOptions::default()
        };
        let error =
            fetch(&transport, &artifact(&hello_sha()), &options).expect_err("全失败必须报错");

        match &error {
            DownloadError::AllSourcesFailed { attempts, failures } => {
                assert!(*attempts >= 2, "至少试了官方与一个镜像：{attempts}");
                assert_eq!(
                    failures.len(),
                    *attempts,
                    "每一次尝试的原因都要保留 —— '都失败了'没有诊断价值"
                );
            }
            other => panic!("应当是 AllSourcesFailed，实际是 {other:?}"),
        }
        assert_eq!(error.kind(), FailureKind::SourceUnavailable);
    }

    #[test]
    fn the_second_fetch_hits_the_cache_and_does_not_touch_the_network() {
        let temp = TempDir::new("cachehit");
        let transport = FakeTransport::new().serves("https://nodejs.org/", HELLO.to_vec());
        let options = options_in(&temp);

        let first = fetch(&transport, &artifact(&hello_sha()), &options).expect("第一次");
        assert!(!first.from_cache);
        let network_calls_after_first = transport.attempts().len();

        let second = fetch(&transport, &artifact(&hello_sha()), &options).expect("第二次");
        assert!(second.from_cache, "第二次必须命中缓存");
        assert_eq!(second.bytes, HELLO.len() as u64);
        assert_eq!(
            transport.attempts().len(),
            network_calls_after_first,
            "缓存命中不能产生任何网络请求"
        );
        assert!(second.attempts.is_empty(), "缓存命中没有'尝试'");
    }

    #[test]
    fn a_corrupted_cache_entry_is_discarded_and_refetched() {
        // 缓存命中也要验。"写进去的时候验过"不能替代"用的时候验" ——
        // 磁盘会坏、文件会被改、缓存目录会被同步工具弄乱。
        let temp = TempDir::new("corruptcache");
        let transport = FakeTransport::new().serves("https://nodejs.org/", HELLO.to_vec());
        let options = options_in(&temp);

        fetch(&transport, &artifact(&hello_sha()), &options).expect("第一次");

        // 把缓存里的字节改掉（模拟磁盘损坏或有人动过它）。
        let cached = options.cache.artifact_path(&hello_sha());
        std::fs::write(&cached, b"corrupted").expect("弄坏缓存");

        let report = fetch(&transport, &artifact(&hello_sha()), &options).expect("应当重新下载");
        assert!(!report.from_cache, "坏缓存项不能被当成命中");
        assert_eq!(std::fs::read(&report.path).expect("读回"), HELLO);
    }

    #[test]
    fn an_unknown_hash_algorithm_is_refused_rather_than_ignored() {
        // 把 `sha512` 当成"某种哈希然后放过去"是安全漏洞。
        let temp = TempDir::new("algorithm");
        let transport = FakeTransport::new().serves("https://nodejs.org/", HELLO.to_vec());
        let mut spec = artifact(&hello_sha());
        spec.algorithm = "sha512".to_owned();

        let error = fetch(&transport, &spec, &options_in(&temp)).expect_err("必须拒绝");
        assert!(matches!(error, DownloadError::UnsupportedAlgorithm { .. }));
        assert_eq!(error.kind(), FailureKind::Integrity);
        assert_eq!(
            transport.attempts().len(),
            0,
            "算法不认识就不该发任何请求 —— 白下一份几百 MB 才发现算不了"
        );
    }

    #[test]
    fn algorithm_names_are_accepted_in_the_spellings_people_actually_write() {
        for name in ["sha256", "SHA256", "SHA-256", "sha_256", "sha256:"] {
            let mut spec = artifact(&hello_sha());
            spec.algorithm = name.to_owned();
            assert!(spec.check_algorithm().is_ok(), "`{name}` 应当被接受");
        }
        for name in ["sha512", "md5", "blake3", ""] {
            let mut spec = artifact(&hello_sha());
            spec.algorithm = name.to_owned();
            assert!(spec.check_algorithm().is_err(), "`{name}` 应当被拒绝");
        }
    }

    #[test]
    fn a_local_archive_is_verified_too() {
        // 离线安装是**最**需要校验的场景（U 盘拷贝），不能成为绕过它的路径。
        let temp = TempDir::new("local");
        let good = temp.path().join("good.zip");
        std::fs::write(&good, HELLO).expect("写");

        let report = use_local_file(&good, &artifact(&hello_sha())).expect("应当通过");
        assert_eq!(report.source_id, "local");
        assert_eq!(report.sha256, hello_sha());

        let bad = temp.path().join("bad.zip");
        std::fs::write(&bad, b"not the same").expect("写");
        let error = use_local_file(&bad, &artifact(&hello_sha())).expect_err("哈希不符必须失败");
        assert!(matches!(error, DownloadError::ChecksumMismatch { .. }));
    }

    #[test]
    fn a_missing_local_file_says_so_instead_of_reporting_a_hash_problem() {
        let temp = TempDir::new("nolocal");
        let missing = temp.path().join("nope.zip");
        let error = use_local_file(&missing, &artifact(&hello_sha())).expect_err("必须失败");
        assert!(matches!(error, DownloadError::LocalFileUnavailable { .. }));
        assert_eq!(error.kind(), FailureKind::LocalInput);
    }

    #[test]
    fn a_destination_directory_receives_the_artifact() {
        let temp = TempDir::new("dest");
        let destination = temp.path().join("out");
        let transport = FakeTransport::new().serves("https://nodejs.org/", HELLO.to_vec());
        let options = FetchOptions {
            destination: Some(destination.clone()),
            ..options_in(&temp)
        };

        let report = fetch(&transport, &artifact(&hello_sha()), &options).expect("应当成功");
        assert!(
            report.path.starts_with(&destination),
            "必须落在目的地：{}",
            report.path.display()
        );
        assert!(
            report
                .path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains("node-24.19.0")
        );
    }

    #[test]
    fn verify_file_is_streaming_so_a_result_can_exceed_any_buffer() {
        // 这条用一个比内部缓冲区大的文件证明"没有把整个文件读进一个固定缓冲"。
        // 缓冲是 1 MiB，这里写 3 MiB。
        let temp = TempDir::new("streaming");
        let big = temp.path().join("big.bin");
        let payload: Vec<u8> = (0..3 * 1024 * 1024)
            .map(|index| (index % 251) as u8)
            .collect();
        std::fs::write(&big, &payload).expect("写大文件");

        assert_eq!(verify_file(&big).expect("算哈希"), sha256_hex(&payload));
    }

    #[test]
    fn a_partial_write_never_appears_at_the_destination_path() {
        // `write_atomically` 走"暂存 + 改名"，所以目标路径上永远不会出现半份文件。
        let temp = TempDir::new("atomic");
        let target = temp.path().join("nested").join("artifact.bin");
        write_atomically(&target, b"complete").expect("写");
        assert_eq!(std::fs::read(&target).expect("读"), b"complete");
        assert!(!target.with_extension("part").exists(), "暂存文件不能留下");
    }
}
