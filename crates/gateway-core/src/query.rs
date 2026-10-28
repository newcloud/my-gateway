//! Query 与 `application/x-www-form-urlencoded` 共用的解析。
//!
//! 规则见 `docs/PROTOCOL.md` 5.1 / 5.2：`&` 分隔、空段忽略、`a` 等价 `a=`、
//! 一次百分号解码、`+` 转空格、解码后必须是 UTF-8、同名参数拒绝、总数上限 256。

const MAX_PAIRS: usize = 256;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum QueryError {
    /// 非法 `%` 序列或解码后不是合法 UTF-8。
    Malformed,
    /// 空 key（`=x`）。
    EmptyKey,
    /// 解码后同名。
    Duplicate(String),
    /// 解码后参数对超过 256。
    TooMany,
}

pub(crate) fn parse(raw: &str) -> Result<Vec<(String, String)>, QueryError> {
    let mut pairs: Vec<(String, String)> = Vec::new();

    for segment in raw.split('&') {
        if segment.is_empty() {
            continue;
        }

        let (key, value) = segment.split_once('=').unwrap_or((segment, ""));
        let key = decode(key).ok_or(QueryError::Malformed)?;
        if key.is_empty() {
            return Err(QueryError::EmptyKey);
        }
        let value = decode(value).ok_or(QueryError::Malformed)?;

        if pairs.iter().any(|(existing, _)| existing == &key) {
            return Err(QueryError::Duplicate(key));
        }

        pairs.push((key, value));
        if pairs.len() > MAX_PAIRS {
            return Err(QueryError::TooMany);
        }
    }

    Ok(pairs)
}

/// 一次百分号解码；`+` 转空格。`%` 后必须是两个十六进制字符。
fn decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            b'%' => {
                if index + 2 >= bytes.len() {
                    return None;
                }
                let high = hex_value(bytes[index + 1])?;
                let low = hex_value(bytes[index + 2])?;
                decoded.push((high << 4) | low);
                index += 3;
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }

    String::from_utf8(decoded).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
