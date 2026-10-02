//! 版本目录旁边的那份记录（`<version>.json`）。
//!
//! # 它不是事实来源 —— 目录才是
//!
//! 记录回答的是"**是谁装的、从哪来的**"：源 id、URL、SHA256、装的时间、
//! 落盘后的文件数与字节数、解压布局。它**不**回答"装了什么" ——
//! 那个问题由磁盘上的目录回答（见 [`crate::installed_versions`]）。
//!
//! 分成两份的理由是**顺序**：载荷只能一次 `rename` 搬过来（原子），而它的真实
//! 文件数与字节数要搬完才数得出来。如果记录必须与载荷一起落位，我们就得先猜一个
//! 数字再落地，那意味着记录会**撒谎**，而一份会撒谎的记录比没有记录更糟。
//!
//! 所以：载荷先落位（事实），记录后写（附注）。后者失败**不回滚前者** ——
//! 那个版本仍然是装好的、可以激活的、可以卸载的，只是"从哪来的"暂时不知道。
//!
//! # 时间戳为什么是自己算的
//!
//! 见 [`now_rfc3339`]：不引日期库，用 Howard Hinnant 的公历算法自己算 ——
//! 它的好处不是省一个依赖，而是**能对已知秒数断言**（决策 41 的同一条理由）。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{StoreError, TEXT_NOT_A_FILE};

/// 记录的 schema 版本。
///
/// 校验是**双向**的：读的时候不认识的版本不猜（见 [`InstallRecord::from_json`]），
/// 写的时候也拒绝写一个自己读不懂的版本（见 [`crate::write_record`]）。
/// 单向校验的后果是"我们写出了一份自己下次读不出来的记录"。
pub const RECORD_SCHEMA_VERSION: u32 = 1;

/// 版本目录旁边的那份记录。
///
/// **字段全是"意图"**：源 id、URL、哈希、布局。没有任何字段是路径之外的机器状态，
/// 所以这份 JSON 可以安全地进 `--json` 输出、进 issue、进日志
/// （`GLOSSARY.md`：只捕获意图，绝不捕获材料）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallRecord {
    /// 格式版本。见 [`RECORD_SCHEMA_VERSION`]。
    pub schema_version: u32,
    /// 工具 id（与路径里的那一段一致）。
    pub tool: String,
    /// 版本号（与路径里的那一段一致）。
    pub version: String,
    /// 人类可读的显示名。
    pub display_name: String,
    /// 实际服务了这次下载的源 id（`cn-npmmirror` / `official` / 用户模板名）。
    ///
    /// **不是"候选源列表"**，是**真的用了哪一个** —— 排查"下载慢了/哈希不符"时
    /// 这是第一个要看的东西（决策 40/42）。
    pub source_id: String,
    /// 下载地址。
    pub url: String,
    /// 制品哈希（小写十六进制）。
    pub sha256: String,
    /// 归档文件名或扩展名（`node-v24.19.0-win-x64.zip`）。
    pub archive: String,
    /// RFC3339 UTC，由 [`now_rfc3339`] 产出。
    pub installed_at: String,
    /// 落盘后的**真实**文件数，由 `adopt_payload` 填 —— 调用方填的值不算数。
    pub payload_files: u64,
    /// 落盘后的**真实**字节数，由 `adopt_payload` 填。
    pub payload_bytes: u64,
    /// 解压布局（剥了几层、暴露哪些命令、环境根在哪）。
    pub layout: tuoen_manifest::Layout,
}

impl InstallRecord {
    /// 序列化成**人读得懂**的 JSON（两空格缩进 + 末尾换行）。
    ///
    /// 末尾换行不是洁癖：这份文件会被人用编辑器打开、被 `git diff` 看、
    /// 被 `cat` 到终端 —— 少一个换行会让最后一行与 shell 提示符粘在一起。
    ///
    /// **不返回 `Result`**：字段全是字符串与整数，serde 在这里没有可失败的分支
    /// （没有浮点、没有非字符串 key、没有自定义序列化）。真有失败就是编程错误，
    /// 而把它降级成一个错误返回意味着调用方要处理一个不会发生的情况。
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut text = serde_json::to_string_pretty(self)
            .expect("InstallRecord 全是字符串与整数，serde 序列化不可能失败");
        text.push('\n');
        text
    }

    /// 从 JSON 文本读回。
    ///
    /// # Errors
    ///
    /// * JSON 畸形或字段缺失/类型不符 → [`StoreError::RecordBroken`]；
    /// * `schema_version` 不是 [`RECORD_SCHEMA_VERSION`] → [`StoreError::RecordBroken`]。
    ///   **不认识的版本一律不猜**：猜错的记录会让 `list` 显示一个错的来源，
    ///   而"少显示一个来源"是用户看得出来、查得动的。
    ///
    /// `RecordBroken.path` 这里是占位串 `<文本>` —— 一段文本没有文件路径。
    /// 读**文件**的那条路径（`ops` 内部）填的是真实路径。
    pub fn from_json(text: &str) -> Result<Self, StoreError> {
        let record: Self =
            serde_json::from_str(text).map_err(|error| StoreError::RecordBroken {
                path: PathBuf::from(TEXT_NOT_A_FILE),
                reason: format!("JSON 解析失败：{error}"),
            })?;
        if record.schema_version != RECORD_SCHEMA_VERSION {
            return Err(StoreError::RecordBroken {
                path: PathBuf::from(TEXT_NOT_A_FILE),
                reason: format!(
                    "记录声明的 schema 是 v{}，本版本只认 v{RECORD_SCHEMA_VERSION} —— 不猜",
                    record.schema_version
                ),
            });
        }
        Ok(record)
    }
}

/// 现在时刻的 RFC3339 UTC（**秒精度**，形如 `2025-10-02T13:45:01Z`）。
///
/// **为什么不引 `chrono`**：这里要的只是一个固定宽度、可比较、可排序的 UTC 时间戳。
/// 算法是 Howard Hinnant 的 `days_from_civil` / `civil_from_days`（公历、无闰秒、
/// 纯整数），因此它可以对**已知的固定秒数**断言 —— 而可断言正是它比引一个日期库
/// 更好的地方（决策 41 的同一条理由：站在信任边界上的那一段代码应当能自己核对）。
///
/// 时钟早于 1970-01-01（本机不会发生）时返回 epoch 而不是 panic：
/// 一个时间戳字段不该让安装失败。
#[must_use]
pub fn now_rfc3339() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        });
    format_utc_seconds(seconds)
}

/// 秒数 → `YYYY-MM-DDTHH:MM:SSZ`。
///
/// 用 [`i64::div_euclid`] 而不是 `/`：Rust 的整数除法**向零截断**，
/// 于是 `-1` 会落到 1970-01-01 而不是 1969-12-31。
fn format_utc_seconds(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let time_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (
        time_of_day / 3_600,
        (time_of_day % 3_600) / 60,
        time_of_day % 60,
    );
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// 天数（1970-01-01 起，可为负）→ `(年, 月, 日)`。
///
/// Howard Hinnant, *chrono-Compatible Low-Level Date Algorithms* 的
/// `civil_from_days`。注意 `/` 在这里必须是**向下取整**（负数先借一位），
/// 所以用的是 [`i64::div_euclid`]。
///
/// 函数末尾有一条 [`debug_assert_eq!`]：它是 [`days_from_civil`] 的**逆运算自检**，
/// 也是这段代码唯一的内部依据（release 下编译掉，零成本）。
pub(crate) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = (mp + if mp < 10 { 3 } else { -9 }) as u32; // [1, 12]
    let (year, month) = if month <= 2 {
        (year + 1, month)
    } else {
        (year, month)
    };
    debug_assert_eq!(
        days_from_civil(year, month, day),
        days,
        "civil_from_days 与 days_from_civil 必须互逆：{days} → {year}-{month}-{day}"
    );
    (year, month, day)
}

/// `(年, 月, 日)` → 天数（1970-01-01 起）。
///
/// 同一个序列的另一半：`days_from_civil`。
pub(crate) fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let month = i64::from(month);
    let day = i64::from(day);
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400; // [0, 399]
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一份字段齐全、可以往磁盘上写的记录。
    fn sample_record() -> InstallRecord {
        InstallRecord {
            schema_version: RECORD_SCHEMA_VERSION,
            tool: "node".to_owned(),
            version: "24.19.0".to_owned(),
            display_name: "Node.js".to_owned(),
            source_id: "cn-npmmirror".to_owned(),
            url: "https://cdn.npmmirror.com/binaries/node/v24.19.0/node-v24.19.0-win-x64.zip"
                .to_owned(),
            sha256: "0f9d2e".to_owned(),
            archive: "node-v24.19.0-win-x64.zip".to_owned(),
            installed_at: "2025-10-02T13:45:01Z".to_owned(),
            payload_files: 3,
            payload_bytes: 23,
            layout: tuoen_manifest::Layout::default(),
        }
    }

    #[test]
    fn civil_from_days_is_correct_for_known_instants() {
        // 这三条是**真机算出来的固定值**（`[DateTimeOffset]::FromUnixTimeSeconds(…)`），
        // 不是用 `now()` 生成的 —— 一条用"实现自己算的现在"做断言的测试，
        // 实现错了它也跟着错。
        for (seconds, expected) in [
            (0_i64, "1970-01-01T00:00:00Z"),
            // 闰日：2000-02-29 真的存在（2000 是世纪闰年）。
            (951_782_400, "2000-02-29T00:00:00Z"),
            (1_759_412_701, "2025-10-02T13:45:01Z"),
        ] {
            assert_eq!(format_utc_seconds(seconds), expected, "秒 {seconds}");
        }
        // 1970 之前也要落到**正确的那一天**（`div_euclid` 而不是 `/`）。
        assert_eq!(format_utc_seconds(-1), "1969-12-31T23:59:59Z");
        assert_eq!(format_utc_seconds(-86_400), "1969-12-31T00:00:00Z");
    }

    #[test]
    fn the_calendar_handles_leap_years_and_the_century_rule() {
        // 闰年：2024 能被 4 整除且不能被 100 整除 → 有 2 月 29 日。
        assert_eq!(
            days_from_civil(2024, 3, 1) - days_from_civil(2024, 2, 29),
            1
        );
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
        assert_eq!(
            days_from_civil(2025, 1, 1) - days_from_civil(2024, 1, 1),
            366
        );

        // 世纪闰年：2000 能被 400 整除 → **是**闰年，一年 366 天。
        assert_eq!(
            days_from_civil(2001, 1, 1) - days_from_civil(2000, 1, 1),
            366
        );
        assert_eq!(
            days_from_civil(2000, 3, 1) - days_from_civil(2000, 2, 29),
            1
        );

        // 世纪平年：1900 能被 100 整除但不能被 400 整除 → **不是**闰年。
        // 这条是"朴素闰年规则"（`year % 4 == 0`）与我们实现的唯一分界点。
        assert_eq!(
            days_from_civil(1901, 1, 1) - days_from_civil(1900, 1, 1),
            365
        );
        assert_eq!(
            days_from_civil(1900, 3, 1) - days_from_civil(1900, 2, 28),
            1
        );

        // 普通平年。
        assert_eq!(
            days_from_civil(2024, 1, 1) - days_from_civil(2023, 1, 1),
            365
        );
    }

    #[test]
    fn days_from_civil_and_civil_from_days_are_inverses_over_130_years() {
        // 130 年逐日往返（约 47,000 次）。它同时把 `civil_from_days` 里那条
        // `debug_assert` 的内部自检跑满 —— 两条入口必须一直互逆。
        let start = days_from_civil(1970, 1, 1);
        let end = days_from_civil(2100, 1, 1);
        assert_eq!(start, 0, "1970-01-01 就是第 0 天");
        for days in start..end {
            let (year, month, day) = civil_from_days(days);
            assert_eq!(
                days_from_civil(year, month, day),
                days,
                "{days} 往返失败 → {year}-{month:02}-{day:02}"
            );
        }
    }

    #[test]
    fn now_produces_a_fixed_width_utc_stamp() {
        // 取值不能用 `now()` 断言（上面那条测试用固定秒数），但**形状**可以：
        // 固定宽度 + `Z` 结尾是这套格式的全部价值（可排序、可字符串比较）。
        let stamp = now_rfc3339();
        assert_eq!(stamp.len(), 20, "{stamp}");
        assert!(stamp.ends_with('Z'), "{stamp}");
        assert_eq!(stamp.as_bytes()[10], b'T', "{stamp}");
        assert_eq!(stamp.as_bytes()[4], b'-', "{stamp}");
        assert_eq!(stamp.as_bytes()[13], b':', "{stamp}");
        assert!(stamp.starts_with("20"), "本机的时钟应当在 21 世纪：{stamp}");
        assert!(stamp[..4].chars().all(|c| c.is_ascii_digit()), "{stamp}");
    }

    #[test]
    fn a_record_round_trips_through_pretty_json() {
        let record = sample_record();
        let text = record.to_json();
        assert!(text.ends_with('\n'), "末尾要有换行");
        // **人读得懂**：缩进 + 每个字段一行。
        assert!(text.contains("\n  \"schema_version\": 1,"), "{text}");
        assert!(
            text.contains("\n  \"source_id\": \"cn-npmmirror\","),
            "{text}"
        );
        assert_eq!(InstallRecord::from_json(&text).expect("读回"), record);
    }

    #[test]
    fn unknown_fields_are_tolerated_so_a_newer_version_can_add_them() {
        // 前向兼容：未来的版本可能加字段，而我们这份读得懂的旧字段仍然可信
        // （`schema_version` 才是那道真正的闸）。
        let mut text = sample_record().to_json();
        text = text.replace("{\n", "{\n  \"future_field\": 42,\n");
        let record = InstallRecord::from_json(&text).expect("多一个字段不该读不出来");
        assert_eq!(record.tool, "node");
    }

    #[test]
    fn broken_json_and_unknown_schemas_are_refused_with_a_reason() {
        let error = InstallRecord::from_json("{ not json").expect_err("畸形 JSON 必须被拒");
        assert_eq!(error.kind(), "record-broken");
        assert!(error.to_string().contains("JSON"), "{error}");

        // schema 不认识 → **不猜**（猜错会让 `list` 显示一个错的来源）。
        let mut record = sample_record();
        record.schema_version = RECORD_SCHEMA_VERSION + 1;
        let text = serde_json::to_string_pretty(&record).expect("序列化");
        let error = InstallRecord::from_json(&text).expect_err("未来 schema 必须被拒");
        assert_eq!(error.kind(), "record-broken");
        assert!(error.to_string().contains("schema"), "{error}");
        assert!(
            error
                .to_string()
                .contains(&format!("v{}", RECORD_SCHEMA_VERSION + 1)),
            "消息里要说明它是哪个版本：{error}"
        );

        // 缺字段也是坏记录（`schema_version` 没有默认值）。
        let error = InstallRecord::from_json(r#"{"tool":"node"}"#).expect_err("缺字段必须被拒");
        assert_eq!(error.kind(), "record-broken");
    }
}
