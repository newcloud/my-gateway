//! 参数值规则：类型、范围、长度、枚举与正则。
//!
//! 这里不依赖 nginx，也不读取原始字节；调用方负责把值处理成 `&str`
//! （Header 维度会先去掉首尾 OWS）。规则在契约加载时编译，运行时只做匹配。

use regex::Regex;

use crate::contract::{ParamRuleObject, ValueType};

/// 值的原始形态。Header / Query / form 只有 `Text`；JSON 保留数字/布尔/复合类型。
#[derive(Debug, Clone, Copy)]
pub(crate) enum Scalar<'a> {
    Text(&'a str),
    Int(i64),
    Bool,
    /// JSON 浮点数：对 `int` / `bool` / 字符串规则都非法；无类型规则时允许。
    Float,
    /// JSON 对象 / 数组：声明字段必须是标量。
    Composite,
}

/// 编译后的值规则。
#[derive(Debug, Clone)]
pub(crate) struct ValueRule {
    value_type: Option<ValueType>,
    min: Option<i64>,
    max: Option<i64>,
    min_len: Option<usize>,
    max_len: Option<usize>,
    len: Option<usize>,
    values: Option<Vec<String>>,
    ignore_case: bool,
    regex: Option<Regex>,
}

impl ValueRule {
    /// 把契约里的值规则编译成可执行形式；只声明 `required` 时返回 `None`。
    ///
    /// 返回的错误文本只描述原因，由调用方补上路由/字段位置。
    pub(crate) fn compile(object: &ParamRuleObject) -> Result<Option<Self>, String> {
        let has_value_rule = object.value_type.is_some()
            || object.min.is_some()
            || object.max.is_some()
            || object.min_len.is_some()
            || object.max_len.is_some()
            || object.len.is_some()
            || object.values.is_some()
            || object.ignore_case.is_some()
            || object.regex.is_some();

        if !has_value_rule {
            return Ok(None);
        }

        match (object.min.is_some(), object.max.is_some()) {
            (_, true) if object.value_type != Some(ValueType::Int) => {
                return Err("max only applies to type int".into());
            }
            (true, _) if object.value_type != Some(ValueType::Int) => {
                return Err("min only applies to type int".into());
            }
            _ => {}
        }

        if (object.min_len.is_some() || object.max_len.is_some() || object.len.is_some())
            && object.value_type != Some(ValueType::Char)
        {
            return Err("min_len/max_len/len only apply to type char".into());
        }

        if (object.values.is_some() || object.ignore_case.is_some())
            && object.value_type != Some(ValueType::Enum)
        {
            return Err("values/ignore_case only apply to type enum".into());
        }

        if object.value_type == Some(ValueType::Enum) && object.values.is_none() {
            return Err("type enum requires values".into());
        }

        if let (Some(min), Some(max)) = (object.min, object.max) {
            if min > max {
                return Err("min must be <= max".into());
            }
        }
        if let (Some(min_len), Some(max_len)) = (object.min_len, object.max_len) {
            if min_len > max_len {
                return Err("min_len must be <= max_len".into());
            }
        }
        if object.len.is_some() && (object.min_len.is_some() || object.max_len.is_some()) {
            return Err("len cannot be combined with min_len/max_len".into());
        }

        let ignore_case = object.ignore_case.unwrap_or(false);
        let values = match &object.values {
            Some(values) => {
                if values.is_empty() {
                    return Err("values must contain 1-64 entries".into());
                }
                if values.len() > 64 {
                    return Err("values must contain 1-64 entries".into());
                }

                let mut seen = std::collections::BTreeSet::new();
                for value in values {
                    let key = if ignore_case {
                        value.to_ascii_lowercase()
                    } else {
                        value.clone()
                    };
                    if !seen.insert(key) {
                        return Err(format!("duplicate enum value {value:?}"));
                    }
                }
                Some(values.clone())
            }
            None => None,
        };

        let regex = match &object.regex {
            Some(pattern) => {
                if object.value_type == Some(ValueType::Enum) {
                    return Err("regex cannot be combined with type enum".into());
                }
                if !matches!(object.value_type, None | Some(ValueType::Char)) {
                    return Err("regex only applies to strings (type char or omitted type)".into());
                }
                if pattern.chars().count() > 256 {
                    return Err("regex is longer than 256 characters".into());
                }
                Regex::new(&format!("^(?:{pattern})$"))
                    .map(Some)
                    .map_err(|e| format!("invalid regex: {e}"))?
            }
            None => None,
        };

        Ok(Some(Self {
            value_type: object.value_type,
            min: object.min,
            max: object.max,
            min_len: object.min_len,
            max_len: object.max_len,
            len: object.len,
            values,
            ignore_case,
            regex,
        }))
    }

    /// 校验字符串或 JSON 标量。Query / form / Header 都是 `Text`，JSON body 保留原始类型。
    ///
    /// 成功返回 `Ok(())`；失败返回日志用的规则名（如 `int` / `max_len` / `regex`）。
    pub(crate) fn validate_scalar(&self, scalar: &Scalar<'_>) -> Result<(), &'static str> {
        // 声明的 body 字段必须是标量；对象/数组直接拒绝（规则名统一写 `type`）。
        if matches!(scalar, Scalar::Composite) {
            return Err("type");
        }

        match self.value_type {
            Some(ValueType::Char) => {
                let Scalar::Text(value) = scalar else {
                    return Err("char");
                };
                if value.contains('\0') {
                    return Err("char");
                }
                let length = value.chars().count();
                if let Some(len) = self.len {
                    if length != len {
                        return Err("len");
                    }
                }
                if let Some(min_len) = self.min_len {
                    if length < min_len {
                        return Err("min_len");
                    }
                }
                if let Some(max_len) = self.max_len {
                    if length > max_len {
                        return Err("max_len");
                    }
                }
            }
            Some(ValueType::Int) => {
                let number = match scalar {
                    Scalar::Text(value) => parse_int(value).ok_or("int")?,
                    Scalar::Int(number) => *number,
                    _ => return Err("int"),
                };
                if let Some(min) = self.min {
                    if number < min {
                        return Err("min");
                    }
                }
                if let Some(max) = self.max {
                    if number > max {
                        return Err("max");
                    }
                }
            }
            Some(ValueType::Bool) => match scalar {
                Scalar::Text(value) if is_bool(value) => {}
                Scalar::Bool => {}
                Scalar::Int(0 | 1) => {}
                _ => return Err("bool"),
            },
            Some(ValueType::Enum) => {
                let Scalar::Text(value) = scalar else {
                    return Err("values");
                };
                let values = self.values.as_deref().unwrap_or(&[]);
                let matched = values.iter().any(|allowed| {
                    if self.ignore_case {
                        allowed.eq_ignore_ascii_case(value)
                    } else {
                        allowed == value
                    }
                });
                if !matched {
                    return Err("values");
                }
            }
            None => {}
        }

        if let Some(regex) = &self.regex {
            let Scalar::Text(value) = scalar else {
                return Err("regex");
            };
            if !regex.is_match(value) {
                return Err("regex");
            }
        }

        Ok(())
    }

    /// 非 UTF-8 值被拒绝时日志使用的规则名。
    pub(crate) fn encoding_rule(&self) -> &'static str {
        match self.value_type {
            Some(ValueType::Int) => "int",
            Some(ValueType::Bool) => "bool",
            Some(ValueType::Enum) => "values",
            Some(ValueType::Char) | None => "char",
        }
    }
}

/// 协议规定的十进制整数语法：`-?(0|[1-9][0-9]*)`，范围 i64。
fn parse_int(value: &str) -> Option<i64> {
    let digits = value.strip_prefix('-').unwrap_or(value);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if digits.len() > 1 && digits.starts_with('0') {
        return None;
    }
    value.parse::<i64>().ok()
}

fn is_bool(value: &str) -> bool {
    value.eq_ignore_ascii_case("true")
        || value.eq_ignore_ascii_case("false")
        || value == "1"
        || value == "0"
}
