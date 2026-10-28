//! Body 解析：`application/json` 与 `application/x-www-form-urlencoded`。
//!
//! 只负责把已读到的字节解析成“字段名 → 标量”，媒体类型/大小/字段集合与值规则由上层处理。
//! 规则见 `docs/PROTOCOL.md` 5.2。

use std::collections::BTreeSet;
use std::fmt;

use serde::de::{self, MapAccess, Visitor};
use serde::Deserializer;

use crate::query;

/// 适配层已经读到的请求体与相关头。
#[derive(Debug, Clone, Copy)]
pub struct BodyInput<'a> {
    pub content_type: Option<&'a str>,
    pub content_encoding: Option<&'a str>,
    pub bytes: &'a [u8],
}

/// JSON 扫描上限；`None` 表示该维度不限制。
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct BodyLimits {
    pub max_depth: Option<usize>,
    pub max_string_length: Option<usize>,
    pub max_array_entries: Option<usize>,
}

/// 解析后的 JSON / form 标量。字符串借由调用方持有的 `String` 再转成 `ParamValue::Text`。
#[derive(Debug, Clone)]
pub(crate) enum OwnedValue {
    Text(String),
    Int(i64),
    Bool,
    Float,
    Composite,
    /// JSON `null`：对**声明字段**等于“未提供”，但对**未声明字段**仍然是“出现”。
    /// 必须保留下来，否则 `{"is_admin": null}` 会绕过 `unknown: "reject"`。
    Null,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BodyError {
    /// 媒体类型、charset 或 Content-Encoding 不支持。
    Unsupported { rule: &'static str },
    /// JSON / form 解析失败（没有可定位字段）。
    Invalid { rule: &'static str },
    /// JSON / form 同名键。
    Duplicate(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BodyKind {
    Json,
    Form,
}

pub(crate) fn parse(
    input: &BodyInput<'_>,
    allowed: &[BodyKind],
    limits: BodyLimits,
) -> Result<Vec<(String, OwnedValue)>, BodyError> {
    // 空 body 按空对象处理，不要求 Content-Type。
    if input.bytes.is_empty() {
        return Ok(Vec::new());
    }

    if let Some(encoding) = input.content_encoding {
        if !encoding.is_empty() && !encoding.eq_ignore_ascii_case("identity") {
            return Err(BodyError::Unsupported {
                rule: "content-encoding",
            });
        }
    }

    let content_type = input.content_type.ok_or(BodyError::Unsupported {
        rule: "content-type",
    })?;
    let kind = parse_media_type(content_type)?;

    if !allowed.contains(&kind) {
        return Err(BodyError::Unsupported {
            rule: "content-type",
        });
    }

    match kind {
        BodyKind::Json => parse_json(input.bytes, limits),
        BodyKind::Form => parse_form(input.bytes),
    }
}

/// 把契约里的 `content_types` 编译成允许的媒体类型；缺省两种都允许。
pub(crate) fn allowed_kinds(content_types: Option<&[String]>) -> Result<Vec<BodyKind>, String> {
    let Some(content_types) = content_types else {
        return Ok(vec![BodyKind::Json, BodyKind::Form]);
    };

    let mut kinds = Vec::new();
    for content_type in content_types {
        let kind = match content_type.trim().to_ascii_lowercase().as_str() {
            "application/json" => BodyKind::Json,
            "application/x-www-form-urlencoded" => BodyKind::Form,
            other => {
                return Err(format!(
                    "unsupported content_types entry {other:?}; only application/json and application/x-www-form-urlencoded"
                ));
            }
        };
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }

    Ok(kinds)
}

fn parse_media_type(content_type: &str) -> Result<BodyKind, BodyError> {
    let mut parts = content_type.split(';');
    let media = parts.next().unwrap_or("").trim().to_ascii_lowercase();
    let kind = match media.as_str() {
        "application/json" => BodyKind::Json,
        "application/x-www-form-urlencoded" => BodyKind::Form,
        _ => {
            return Err(BodyError::Unsupported {
                rule: "content-type",
            });
        }
    };

    for parameter in parts {
        let Some((name, value)) = parameter.split_once('=') else {
            continue;
        };
        if !name.trim().eq_ignore_ascii_case("charset") {
            continue;
        }

        let charset = value.trim().trim_matches('"');
        if !(charset.eq_ignore_ascii_case("utf-8") || charset.eq_ignore_ascii_case("utf8")) {
            return Err(BodyError::Unsupported { rule: "charset" });
        }
    }

    Ok(kind)
}

/// 单遍字节扫描：只统计结构深度、字符串原始长度、数组元素数。
///
/// 不作为 JSON 校验器——非法 JSON 仍由 `serde_json` 拒绝；这里只负责在分配 `Value` 之前
/// 挡掉深度/长度/元素数异常，避免深层嵌套造成递归和内存放大。字符串内的括号与转义引号
/// 会被正确忽略；固定栈大小 = 128，和 serde_json 默认递归上限一致。
fn scan_json(bytes: &[u8], limits: BodyLimits) -> Result<(), BodyError> {
    const MAX_STACK: usize = 128;

    #[derive(Clone, Copy)]
    struct Level {
        is_array: bool,
        entries: usize,
    }

    let mut stack = [Level {
        is_array: false,
        entries: 0,
    }; MAX_STACK];
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut string_len = 0usize;

    for &byte in bytes {
        if in_string {
            if byte == b'"' && !escaped {
                in_string = false;
                continue;
            }
            if let Some(max) = limits.max_string_length {
                string_len += 1;
                if string_len > max {
                    return Err(BodyError::Invalid {
                        rule: "json-string-length",
                    });
                }
            }
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            }
            continue;
        }

        match byte {
            b'"' => {
                in_string = true;
                escaped = false;
                string_len = 0;
            }
            b'{' | b'[' => {
                if depth >= MAX_STACK {
                    return Err(BodyError::Invalid { rule: "json-depth" });
                }
                if let Some(max) = limits.max_depth {
                    if depth + 1 > max {
                        return Err(BodyError::Invalid { rule: "json-depth" });
                    }
                }
                stack[depth] = Level {
                    is_array: byte == b'[',
                    entries: 0,
                };
                depth += 1;
            }
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
            }
            b',' if depth > 0 => {
                let level = &mut stack[depth - 1];
                if level.is_array {
                    level.entries = level.entries.saturating_add(1);
                    if let Some(max) = limits.max_array_entries {
                        if level.entries >= max {
                            return Err(BodyError::Invalid {
                                rule: "json-array-entries",
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }

    Ok(())
}

fn parse_form(bytes: &[u8]) -> Result<Vec<(String, OwnedValue)>, BodyError> {
    let text = std::str::from_utf8(bytes).map_err(|_| BodyError::Invalid { rule: "form" })?;
    let pairs = query::parse(text).map_err(|error| match error {
        query::QueryError::Duplicate(name) => BodyError::Duplicate(name),
        _ => BodyError::Invalid { rule: "form" },
    })?;

    Ok(pairs
        .into_iter()
        .map(|(name, value)| (name, OwnedValue::Text(value)))
        .collect())
}

fn parse_json(bytes: &[u8], limits: BodyLimits) -> Result<Vec<(String, OwnedValue)>, BodyError> {
    scan_json(bytes, limits)?;

    let mut duplicate = None;
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);

    let parsed = deserializer
        .deserialize_map(BodyVisitor {
            duplicate: &mut duplicate,
        })
        .and_then(|value| {
            deserializer
                .end()
                .map(|()| value)
                .map_err(serde::de::Error::custom)
        });

    match parsed {
        Ok(value) => Ok(value),
        Err(_) => match duplicate {
            Some(name) => Err(BodyError::Duplicate(name)),
            None => Err(BodyError::Invalid { rule: "json" }),
        },
    }
}

struct BodyVisitor<'a> {
    duplicate: &'a mut Option<String>,
}

impl<'de> Visitor<'de> for BodyVisitor<'_> {
    type Value = Vec<(String, OwnedValue)>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut seen = BTreeSet::new();
        let mut fields = Vec::new();

        while let Some(name) = map.next_key::<String>()? {
            if !seen.insert(name.clone()) {
                *self.duplicate = Some(name);
                return Err(de::Error::custom("duplicate JSON key"));
            }

            let value: serde_json::Value = map.next_value()?;
            let value = match value {
                serde_json::Value::Null => OwnedValue::Null,
                serde_json::Value::String(value) => OwnedValue::Text(value),
                serde_json::Value::Bool(_) => OwnedValue::Bool,
                serde_json::Value::Number(value) => match value.as_i64() {
                    Some(value) => OwnedValue::Int(value),
                    None => OwnedValue::Float,
                },
                serde_json::Value::Array(_) | serde_json::Value::Object(_) => OwnedValue::Composite,
            };
            fields.push((name, value));
        }

        Ok(fields)
    }
}
