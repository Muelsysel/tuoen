//! 内容寻址的下载缓存。
//!
//! ## 为什么按哈希存，而不是按 URL
//!
//! 按 URL 存的缓存会在**换源**时全部失效：同一个制品从阿里下与从官方下
//! 是**同一份字节**，但 URL 不同。按内容存则：
//!
//! - 换源命中同一个缓存项；
//! - **缓存项的存在本身就是一次校验**：文件名就是它的 SHA256，
//!   写进去的时候验证过，读出来还能再验一次；
//! - 离线 bundle（L3）天然能复用这个目录。
//!
//! ## 布局
//!
//! ```text
//! %APPDATA%\tuoen\cache\sha256\
//!     ab\abcdef…（前两位做分片，避免一个目录里上万个文件）
//!         abcdef…            ← 制品本身
//!         abcdef….meta.json  ← 它从哪来（可诊断）
//! ```
//!
//! `.meta.json` 是**可选的**：删了它缓存照样能用（它是诊断信息，不是索引）。
//! 这一点是有意的 —— 一个"删了索引就认不出文件"的缓存，会在用户清理磁盘后
//! 变成一堆无法回收的垃圾。
//!
//! ## 缓存失败不能让下载失败
//!
//! 缓存是**优化**，不是功能。`%APPDATA%` 不可写（容器、被策略锁定的机器）
//! 时我们**降级到不缓存**，而不是让用户装不了东西。
//! 这是 [`crate::fetch_to_cache`] 里的一个显式分支，不是"漏了错误处理"。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::DownloadError;

/// 一个缓存项的元信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheMeta {
    /// 它的 SHA256。
    pub sha256: String,
    /// **第一次**取到它的 URL。
    pub source_url: String,
    /// 哪个源 id（`cn-npmmirror` / `official` / `custom:…`）。
    pub source_id: String,
    /// 第一次取到时的大小（字节）。
    pub bytes: u64,
}

/// 内容寻址的缓存目录。
#[derive(Debug, Clone)]
pub struct Cache {
    root: PathBuf,
}

impl Cache {
    /// 用这个根目录建一个缓存。**不创建目录** —— 创建推迟到真的要写的时候，
    /// 这样"只读地看一眼缓存里有没有"不会产生副作用。
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// 默认位置：`%APPDATA%\tuoen\cache`。
    ///
    /// 取不到 `APPDATA` 时退回**当前目录下的 `.tuoen-cache`**，
    /// 而不是失败：一个取不到 `APPDATA` 的环境（服务账户、容器）
    /// 仍然应该能下载东西。
    #[must_use]
    pub fn default_root() -> PathBuf {
        match std::env::var_os("APPDATA") {
            Some(appdata) if !appdata.is_empty() => Path::new(&appdata).join("tuoen").join("cache"),
            _ => PathBuf::from(".tuoen-cache"),
        }
    }

    /// 默认缓存。
    #[must_use]
    pub fn at_default_location() -> Self {
        Self::new(Self::default_root())
    }

    /// 缓存根目录。
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 一个哈希对应分片目录。
    fn shard_dir(&self, sha256: &str) -> PathBuf {
        // 空哈希没法分片。调用方保证了它是 64 个十六进制字符，
        // 但这里也要有一个不 panic 的答案。
        let prefix = sha256.get(..2).unwrap_or("00");
        self.root.join("sha256").join(prefix)
    }

    /// 制品文件的路径。
    #[must_use]
    pub fn artifact_path(&self, sha256: &str) -> PathBuf {
        self.shard_dir(sha256).join(sha256)
    }

    /// 元信息文件的路径。
    #[must_use]
    pub fn meta_path(&self, sha256: &str) -> PathBuf {
        self.shard_dir(sha256).join(format!("{sha256}.meta.json"))
    }

    /// 缓存里有这个制品吗（**只看文件在不在且非空**）。
    ///
    /// **不在这里验哈希。** 调用方拿到路径后会用 [`crate::verify_file`] 验 ——
    /// 那个函数是流式的，可以处理几百 MB 的文件而不吃内存。
    /// 这里验会让"只看一眼"变成一次全文件读。
    #[must_use]
    pub fn lookup(&self, sha256: &str) -> Option<PathBuf> {
        let path = self.artifact_path(sha256);
        let metadata = std::fs::metadata(&path).ok()?;
        // 长度 0 的制品在任何真实场景下都是坏的（我们下载的都是压缩包）。
        // 但**不在这里判"太短"** —— 那是一个阈值，属于调用方的策略。
        (metadata.is_file() && metadata.len() > 0).then_some(path)
    }

    /// 读一项的元信息。没有就返回 `None`（**不是错误**）。
    #[must_use]
    pub fn read_meta(&self, sha256: &str) -> Option<CacheMeta> {
        let text = std::fs::read_to_string(self.meta_path(sha256)).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// 把一份已经落在 `source` 的文件**收进缓存**。
    ///
    /// 用"复制到临时文件再改名"而不是直接写目标：改名在同一个卷上是原子的，
    /// 所以**缓存里永远不会出现半份制品**（那是比"没有缓存"更糟的状态，
    /// 因为它会让下一次 `lookup` 命中一个坏文件）。
    ///
    /// # Errors
    ///
    /// 目录建不出来、复制失败、改名失败。
    pub fn store(
        &self,
        sha256: &str,
        source: &Path,
        meta: &CacheMeta,
    ) -> Result<PathBuf, DownloadError> {
        let shard = self.shard_dir(sha256);
        std::fs::create_dir_all(&shard).map_err(|error| DownloadError::CacheUnavailable {
            path: shard.clone(),
            source: error,
        })?;

        let destination = self.artifact_path(sha256);
        let staging = shard.join(format!("{sha256}.part"));

        std::fs::copy(source, &staging).map_err(|error| DownloadError::CacheUnavailable {
            path: staging.clone(),
            source: error,
        })?;

        // **先写元信息，再改名制品。** 顺序有意义：如果元信息写失败，
        // 制品还没进缓存，于是不会留下一个"有制品没元信息"的项。
        // 反过来的话，一个失败的元信息写入会留下一份**没有来源信息**的缓存，
        // 而那种缓存无法诊断（"这个文件是谁放进来的？"）。
        let meta_text = serde_json::to_string_pretty(meta).unwrap_or_else(|_| "{}".to_owned());
        let meta_staging = shard.join(format!("{sha256}.meta.part"));
        std::fs::write(&meta_staging, meta_text).map_err(|error| {
            let _ = std::fs::remove_file(&staging);
            DownloadError::CacheUnavailable {
                path: meta_staging.clone(),
                source: error,
            }
        })?;

        std::fs::rename(&staging, &destination).map_err(|error| {
            let _ = std::fs::remove_file(&staging);
            DownloadError::CacheUnavailable {
                path: destination.clone(),
                source: error,
            }
        })?;
        // 元信息用改名落地；失败不影响制品的可用性，所以不当作错误。
        let _ = std::fs::rename(&meta_staging, self.meta_path(sha256));

        Ok(destination)
    }

    /// 缓存里现在有几项、占多少字节。
    ///
    /// 给 `tuoen cache` 之类的命令用；也让"缓存真的生效了"可以被断言。
    #[must_use]
    pub fn stats(&self) -> CacheStats {
        let mut stats = CacheStats::default();
        let Ok(shards) = std::fs::read_dir(self.root.join("sha256")) else {
            return stats;
        };
        for shard in shards.flatten() {
            let Ok(entries) = std::fs::read_dir(shard.path()) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.ends_with(".meta.json") || name.ends_with(".part") {
                    continue;
                }
                if let Ok(metadata) = entry.metadata()
                    && metadata.is_file()
                {
                    stats.entries += 1;
                    stats.bytes += metadata.len();
                }
            }
        }
        stats
    }
}

/// 缓存里有多少东西。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// 制品个数。
    pub entries: u64,
    /// 总字节数。
    pub bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个自清理的临时目录。
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let unique = format!(
                "tuoen-cache-test-{label}-{}-{}",
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

    fn meta(sha: &str) -> CacheMeta {
        CacheMeta {
            sha256: sha.to_owned(),
            source_url: "https://cdn.example/x.zip".to_owned(),
            source_id: "cn-npmmirror".to_owned(),
            bytes: 5,
        }
    }

    #[test]
    fn storing_then_looking_up_round_trips() {
        let temp = TempDir::new("roundtrip");
        let cache = Cache::new(temp.path());
        let sha = "a".repeat(64);

        assert!(cache.lookup(&sha).is_none(), "空缓存不该命中");

        let payload = temp.path().join("payload.bin");
        std::fs::write(&payload, b"hello").expect("写 payload");

        let stored = cache.store(&sha, &payload, &meta(&sha)).expect("收进缓存");
        assert!(stored.is_file());
        assert_eq!(std::fs::read(&stored).expect("读回"), b"hello");

        let hit = cache.lookup(&sha).expect("必须命中");
        assert_eq!(hit, stored);

        let read_back = cache.read_meta(&sha).expect("元信息");
        assert_eq!(read_back.source_id, "cn-npmmirror");
        assert_eq!(cache.stats().entries, 1);
        assert_eq!(cache.stats().bytes, 5);
    }

    #[test]
    fn artifacts_are_sharded_by_the_first_two_hex_digits() {
        // 不分片的话，一个下载了几百个版本的用户会在一个目录里堆上万个文件。
        let cache = Cache::new("C:\\cache");
        let sha = format!("ab{}", "c".repeat(62));
        let path = cache.artifact_path(&sha);
        assert!(
            path.to_string_lossy().contains(r"sha256\ab\"),
            "必须落在 sha256\\ab\\ 下：{}",
            path.display()
        );
    }

    #[test]
    fn a_zero_byte_artifact_does_not_count_as_a_hit() {
        // 长度 0 的制品在任何真实场景下都是坏的（我们下的都是压缩包）。
        // 把它当命中会让"上次下载被中断"变成一个永久的假成功。
        let temp = TempDir::new("zerobyte");
        let cache = Cache::new(temp.path());
        let sha = "b".repeat(64);

        let path = cache.artifact_path(&sha);
        std::fs::create_dir_all(path.parent().expect("父目录")).expect("建目录");
        std::fs::write(&path, b"").expect("写空文件");

        assert!(cache.lookup(&sha).is_none(), "0 字节文件不能被当成缓存命中");
    }

    #[test]
    fn metadata_can_be_missing_and_the_cache_still_works() {
        // `.meta.json` 是诊断信息，不是索引。用户清理磁盘时删掉它
        // 不该让缓存里那些文件变成无法回收的垃圾。
        let temp = TempDir::new("nometa");
        let cache = Cache::new(temp.path());
        let sha = "c".repeat(64);

        let payload = temp.path().join("p.bin");
        std::fs::write(&payload, b"data").expect("写 payload");
        cache.store(&sha, &payload, &meta(&sha)).expect("存");

        std::fs::remove_file(cache.meta_path(&sha)).expect("删元信息");

        assert!(cache.lookup(&sha).is_some(), "制品还在，就该命中");
        assert_eq!(cache.read_meta(&sha), None, "元信息没了就返回 None");
        // 而且不能因此报错（`read_meta` 返回 `Option` 而不是 `Result` 就是这个理由）。
        assert_eq!(cache.stats().entries, 1);
    }

    #[test]
    fn an_existing_artifact_is_not_left_half_written() {
        // 原地写会让中断留下半份制品，而半份制品比"没有缓存"更糟 ——
        // 它会让下一次 lookup 命中一个坏文件。所以必须走"暂存 + 改名"。
        let temp = TempDir::new("atomic");
        let cache = Cache::new(temp.path());
        let sha = "d".repeat(64);

        let payload = temp.path().join("p.bin");
        std::fs::write(&payload, b"complete payload").expect("写 payload");
        cache.store(&sha, &payload, &meta(&sha)).expect("第一次");

        // 再存一次同样的内容（模拟重复下载被收进缓存）。
        cache.store(&sha, &payload, &meta(&sha)).expect("第二次");

        assert_eq!(
            std::fs::read(cache.artifact_path(&sha)).expect("读回"),
            b"complete payload"
        );
        // 暂存文件不能留下。
        let shard = cache
            .artifact_path(&sha)
            .parent()
            .expect("分片")
            .to_path_buf();
        let leftovers: Vec<String> = std::fs::read_dir(&shard)
            .expect("列目录")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".part"))
            .collect();
        assert!(leftovers.is_empty(), "不该留下暂存文件：{leftovers:?}");
    }

    #[test]
    fn stats_ignore_metadata_files_and_partials() {
        let temp = TempDir::new("stats");
        let cache = Cache::new(temp.path());
        let sha = "e".repeat(64);
        let payload = temp.path().join("p.bin");
        std::fs::write(&payload, b"1234").expect("写");
        cache.store(&sha, &payload, &meta(&sha)).expect("存");

        let stats = cache.stats();
        assert_eq!(stats.entries, 1, "元信息不该被算成一个制品");
        assert_eq!(stats.bytes, 4, "只算制品本身的字节");
    }
}
