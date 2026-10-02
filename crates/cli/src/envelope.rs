//! `--json` 输出信封。
//!
//! **为什么需要信封**：每个命令都只输出一个 `data` 会让"成功但没数据"和"失败"无法区分，
//! 而 `--json` 的消费者（脚本、未来的 GUI）需要**不需要解析人类文本**就知道成败。
//!
//! 形状固定为：
//!
//! ```json
//! {"schemaVersion":1,"command":"list","ok":true,"data":{"tools":[]}}
//! {"schemaVersion":1,"command":"list","ok":false,"error":{"code":"io","message":"…"}}
//! ```
//!
//! **`schemaVersion` 是破坏性变更的哨兵**：键名、命令名、错误码的改动都要递增它。
//! 错误码是**稳定的机器可读字符串**，与中文消息分开 —— 中文消息可以改，错误码不能。

use serde::Serialize;

/// 当前的 JSON schema 版本。改动键名 / 命令名 / 错误码时**必须**递增。
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize)]
pub struct Envelope {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub command: &'static str,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    /// **稳定的机器可读错误码**。中文消息可以改，这个不能。
    pub code: &'static str,
    /// 中文人类可读消息。
    pub message: String,
}

impl Envelope {
    #[must_use]
    pub fn ok<T: Serialize>(command: &'static str, data: &T) -> Self {
        // 把序列化失败推迟到调用方处理，而不是在这里 panic。
        let data = serde_json::to_value(data).ok();
        Self {
            schema_version: SCHEMA_VERSION,
            command,
            ok: true,
            data,
            error: None,
        }
    }

    /// 构造一个失败信封。
    ///
    /// ticket #2 阶段没有会失败的子命令，但**错误形状必须现在就定型** ——
    /// 否则第一个需要报错的子命令会临时发明一套，而 `--json` 的消费者已经在解析它了。
    #[allow(
        dead_code,
        reason = "公开契约的一部分：错误形状先于第一个会失败的命令定型"
    )]
    #[must_use]
    pub fn err(command: &'static str, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            command,
            ok: false,
            data: None,
            error: Some(ErrorBody {
                code,
                message: message.into(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_envelope_has_no_error_key() {
        let json =
            serde_json::to_string(&Envelope::ok("list", &serde_json::json!({ "tools": [] })))
                .expect("serialise");
        assert_eq!(
            json,
            r#"{"schemaVersion":1,"command":"list","ok":true,"data":{"tools":[]}}"#
        );
        assert!(!json.contains("error"), "{json}");
    }

    #[test]
    fn error_envelope_has_no_data_key() {
        let json =
            serde_json::to_string(&Envelope::err("list", "io", "读不到文件")).expect("serialise");
        assert_eq!(
            json,
            r#"{"schemaVersion":1,"command":"list","ok":false,"error":{"code":"io","message":"读不到文件"}}"#
        );
        assert!(!json.contains("\"data\""), "{json}");
    }

    #[test]
    fn schema_version_is_pinned() {
        // 改这个数字是破坏性变更，必须是有意为之。
        assert_eq!(SCHEMA_VERSION, 1);
    }
}
