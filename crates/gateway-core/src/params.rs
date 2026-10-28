//! 参数规则编译与运行时校验（Header / Query / Body 共用）。
//!
//! 三个维度只在“名字如何比较、值如何预处理、是否检查未声明字段”上不同；
//! 必填、`required_if`、重复行/重复键与值规则的顺序完全一致，因此放在这里统一实现。

use std::collections::BTreeSet;
use std::fmt::Write;

use crate::contract::{ParamRule, RequiredIf, UnknownMode};
use crate::value::{Scalar, ValueRule};

/// 编译后的一个参数块。
#[derive(Debug, Clone)]
pub(crate) struct ParamSet {
    dimension: &'static str,
    /// Header 名字大小写不敏感；Query / Body 区分大小写。
    case_insensitive: bool,
    /// 只有 Header 需要去首尾 OWS。
    trim_ows: bool,
    unknown: UnknownMode,
    rules: Vec<CompiledParam>,
}

#[derive(Debug, Clone)]
struct CompiledParam {
    /// 匹配用名字：Header 统一小写，Query / Body 保留原样。
    name: String,
    required: bool,
    required_if: Option<RequiredIfRule>,
    value: Option<ValueRule>,
}

#[derive(Debug, Clone)]
struct RequiredIfRule {
    /// 被引用字段的匹配用名字。
    field: String,
    /// `None` 表示“出现即成立”；`Some` 表示还要按处理后的值完全相等。
    equals: Option<String>,
}

/// 契约编译错误。调用方补上路由序号与字段路径。
#[derive(Debug)]
pub(crate) enum ParamCompileError {
    /// 规则对象里出现未知字段。
    UnknownRuleField { name: String, field: String },
    /// `required_if` 对象里出现未知字段。
    UnknownRequiredIfField { name: String, field: String },
    /// 其他非法规则。
    Invalid {
        name: Option<String>,
        detail: String,
    },
}

/// 运行时参数值。Query / form 总是 `Text`；Header 可能是 `NonUtf8`；JSON 保留标量类型。
#[derive(Debug, Clone, Copy)]
pub(crate) enum ParamValue<'a> {
    Text(&'a str),
    /// Header 值存在但不是合法 UTF-8。
    NonUtf8,
    /// JSON 整数。
    Int(i64),
    /// JSON 布尔。
    Bool,
    /// JSON 浮点数（声明 `int` / `bool` / 字符串类型时都非法）。
    Float,
    /// JSON 对象 / 数组；声明字段必须是标量。
    Composite,
    /// JSON `null`：对声明字段等于“未提供”（不参与 required / 值规则 / 过滤），
    /// 但对 `unknown: "reject"` 仍然算“出现”。
    Null,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ParamInput<'a> {
    pub name: &'a str,
    pub value: ParamValue<'a>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParamErrorKind {
    Missing,
    Invalid,
}

#[derive(Debug)]
pub(crate) struct ParamError {
    pub kind: ParamErrorKind,
    pub field: String,
    pub rule: &'static str,
}

/// `ParamSet::compile` 的配置。字段较多，用结构体避免一长串位置参数。
pub(crate) struct ParamSetSpec<'a> {
    pub dimension: &'static str,
    pub case_insensitive: bool,
    pub trim_ows: bool,
    pub unknown: UnknownMode,
    pub params: &'a [(String, ParamRule)],
    pub allow_required: bool,
    pub name_validator: fn(&str) -> bool,
    pub name_rule: &'a str,
}

impl ParamSet {
    /// 编译参数块。`name_validator` 决定名字是否合法（Header 是 HTTP token，Query/Body 是普通字段名）。
    pub(crate) fn compile(spec: ParamSetSpec<'_>) -> Result<Self, ParamCompileError> {
        let ParamSetSpec {
            dimension,
            case_insensitive,
            trim_ows,
            unknown,
            params,
            allow_required,
            name_validator,
            name_rule,
        } = spec;
        if params.len() > 128 {
            return Err(ParamCompileError::Invalid {
                name: None,
                detail: format!("params has {} rules, limit is 128", params.len()),
            });
        }

        let mut seen = BTreeSet::new();
        let mut rules = Vec::with_capacity(params.len());

        for (name, rule) in params {
            if !name_validator(name) {
                return Err(ParamCompileError::Invalid {
                    name: Some(name.clone()),
                    detail: name_rule.to_owned(),
                });
            }

            let normalized = normalize(name, case_insensitive);
            if !seen.insert(normalized.clone()) {
                return Err(ParamCompileError::Invalid {
                    name: Some(name.clone()),
                    detail: "duplicate parameter name (case-insensitive for headers)".into(),
                });
            }

            let compiled = compile_rule(name, &normalized, case_insensitive, allow_required, rule)?;
            rules.push(compiled);
        }

        // `required_if` 只能引用同一参数块里已声明的字段，且不能自引用。
        for rule in &rules {
            let Some(condition) = &rule.required_if else {
                continue;
            };
            if condition.field == rule.name {
                return Err(ParamCompileError::Invalid {
                    name: Some(rule.name.clone()),
                    detail: "required_if must not reference itself".into(),
                });
            }
            if !seen.contains(&condition.field) {
                return Err(ParamCompileError::Invalid {
                    name: Some(rule.name.clone()),
                    detail: format!(
                        "required_if references undeclared parameter \"{}\"",
                        condition.field
                    ),
                });
            }
        }

        Ok(Self {
            dimension,
            case_insensitive,
            trim_ows,
            unknown,
            rules,
        })
    }

    /// 按协议 5.5 的维度内顺序校验：无条件必填 → 条件必填 → 重复 → 值 → 未声明字段。
    pub(crate) fn check(&self, values: &[ParamInput<'_>]) -> Result<(), ParamError> {
        // JSON `null` 对“声明字段”等于未提供（协议 5.0）：这里直接跳过，于是 required /
        // required_if 视为缺失、值规则与过滤也跳过；但它仍留在 `values` 里，
        // 最后一步的未声明字段检查照常能看见它（`unknown: "reject"` 不会被 null 绕过）。
        let count = |rule: &CompiledParam| {
            values
                .iter()
                .filter(|value| !matches!(value.value, ParamValue::Null))
                .filter(|value| self.matches(value.name, &rule.name))
                .count()
        };

        // 1. 无条件必填。
        for rule in &self.rules {
            if rule.required && count(rule) == 0 {
                return Err(ParamError {
                    kind: ParamErrorKind::Missing,
                    field: format_field(self.dimension, &rule.name),
                    rule: "required",
                });
            }
        }

        // 2. 条件必填。
        for rule in &self.rules {
            let Some(condition) = &rule.required_if else {
                continue;
            };
            if count(rule) == 0 && self.condition_holds(values, condition) {
                return Err(ParamError {
                    kind: ParamErrorKind::Missing,
                    field: format_field(self.dimension, &rule.name),
                    rule: "required_if",
                });
            }
        }

        // 3. 同名重复。
        for rule in &self.rules {
            if count(rule) > 1 {
                return Err(ParamError {
                    kind: ParamErrorKind::Invalid,
                    field: format_field(self.dimension, &rule.name),
                    rule: "duplicate",
                });
            }
        }

        // 4. 值规则（按契约顺序）。
        for rule in &self.rules {
            let Some(value_rule) = &rule.value else {
                continue;
            };
            let Some(value) = self.first_value(values, &rule.name) else {
                continue;
            };

            let scalar = match value {
                ParamValue::Text(text) => {
                    Scalar::Text(if self.trim_ows { trim_ows(text) } else { text })
                }
                ParamValue::Int(number) => Scalar::Int(*number),
                ParamValue::Bool => Scalar::Bool,
                ParamValue::Float => Scalar::Float,
                ParamValue::Composite => Scalar::Composite,
                // `first_value` 已过滤掉 null；这里保留分支只为穷尽 match。
                ParamValue::Null => continue,
                ParamValue::NonUtf8 => {
                    return Err(ParamError {
                        kind: ParamErrorKind::Invalid,
                        field: format_field(self.dimension, &rule.name),
                        rule: value_rule.encoding_rule(),
                    });
                }
            };

            if let Err(rule_name) = value_rule.validate_scalar(&scalar) {
                return Err(ParamError {
                    kind: ParamErrorKind::Invalid,
                    field: format_field(self.dimension, &rule.name),
                    rule: rule_name,
                });
            }
        }

        // 5. 未声明字段（只有 Query / Body 可能开启）。
        if self.unknown == UnknownMode::Reject {
            for value in values {
                let declared = self
                    .rules
                    .iter()
                    .any(|rule| self.matches(value.name, &rule.name));
                if !declared {
                    return Err(ParamError {
                        kind: ParamErrorKind::Invalid,
                        field: format_field(self.dimension, value.name),
                        rule: "unknown",
                    });
                }
            }
        }

        Ok(())
    }

    /// Header 过滤：只取契约里声明过的头，按契约顺序，值去 OWS。
    pub(crate) fn declared_text_values(&self, values: &[ParamInput<'_>]) -> Vec<(String, String)> {
        self.rules
            .iter()
            .filter_map(|rule| {
                values
                    .iter()
                    .find(|value| self.matches(value.name, &rule.name))
                    .and_then(|value| match value.value {
                        ParamValue::Text(text) => Some((
                            rule.name.clone(),
                            if self.trim_ows {
                                trim_ows(text).to_owned()
                            } else {
                                text.to_owned()
                            },
                        )),
                        _ => None,
                    })
            })
            .collect()
    }

    /// Query / Body 过滤：取所有已解析的字符串值（含未声明字段）。
    pub(crate) fn all_text_values(&self, values: &[ParamInput<'_>]) -> Vec<(String, String)> {
        values
            .iter()
            .filter_map(|value| match value.value {
                ParamValue::Text(text) => Some((
                    value.name.to_owned(),
                    if self.trim_ows {
                        trim_ows(text).to_owned()
                    } else {
                        text.to_owned()
                    },
                )),
                _ => None,
            })
            .collect()
    }

    fn matches(&self, left: &str, right: &str) -> bool {
        if self.case_insensitive {
            left.eq_ignore_ascii_case(right)
        } else {
            left == right
        }
    }

    fn first_value<'v, 'a>(
        &self,
        values: &'v [ParamInput<'a>],
        name: &str,
    ) -> Option<&'v ParamValue<'a>> {
        // `null` 对声明字段等于未提供：值规则、required_if 的条件取值都不应看到它。
        values
            .iter()
            .find(|value| {
                !matches!(value.value, ParamValue::Null) && self.matches(value.name, name)
            })
            .map(|value| &value.value)
    }

    fn condition_holds(&self, values: &[ParamInput<'_>], condition: &RequiredIfRule) -> bool {
        let Some(value) = self.first_value(values, &condition.field) else {
            return false;
        };

        match &condition.equals {
            None => true,
            Some(expected) => match value {
                ParamValue::Text(text) => {
                    let text = if self.trim_ows { trim_ows(text) } else { text };
                    text == expected
                }
                // 非 UTF-8、JSON 数字/布尔/对象都不等于字符串。
                _ => false,
            },
        }
    }
}

fn compile_rule(
    name: &str,
    normalized: &str,
    case_insensitive: bool,
    allow_required: bool,
    rule: &ParamRule,
) -> Result<CompiledParam, ParamCompileError> {
    let (required, required_if, value) = match rule {
        ParamRule::Presence(_) if !allow_required => {
            return Err(ParamCompileError::Invalid {
                name: Some(name.to_owned()),
                detail: "path params are always present; do not use `true` / `false` shorthand"
                    .into(),
            });
        }
        ParamRule::Presence(true) => (true, None, None),
        ParamRule::Presence(false) => {
            return Err(ParamCompileError::Invalid {
                name: Some(name.to_owned()),
                detail: "rule must not be `false`; omit the rule for an optional parameter".into(),
            });
        }
        ParamRule::Object(object) => {
            if let Some(field) = object.extra.keys().next() {
                return Err(ParamCompileError::UnknownRuleField {
                    name: name.to_owned(),
                    field: field.clone(),
                });
            }

            if !allow_required && (object.required.is_some() || object.required_if.is_some()) {
                return Err(ParamCompileError::Invalid {
                    name: Some(name.to_owned()),
                    detail:
                        "path params are always present; do not declare `required` / `required_if`"
                            .into(),
                });
            }

            let value =
                ValueRule::compile(object).map_err(|detail| ParamCompileError::Invalid {
                    name: Some(name.to_owned()),
                    detail,
                })?;
            let required_if = if allow_required {
                compile_required_if(name, case_insensitive, object.required_if.as_ref())?
            } else {
                None
            };

            if object.required == Some(true) && required_if.is_some() {
                return Err(ParamCompileError::Invalid {
                    name: Some(name.to_owned()),
                    detail: "must not combine `required: true` with `required_if`".into(),
                });
            }

            if object.required.is_none() && required_if.is_none() && value.is_none() {
                return Err(ParamCompileError::Invalid {
                    name: Some(name.to_owned()),
                    detail: "is empty; set `required`, `required_if`, a value rule, or use `true`"
                        .into(),
                });
            }

            (object.required.unwrap_or(false), required_if, value)
        }
    };

    Ok(CompiledParam {
        name: normalized.to_owned(),
        required,
        required_if,
        value,
    })
}

fn compile_required_if(
    name: &str,
    case_insensitive: bool,
    required_if: Option<&RequiredIf>,
) -> Result<Option<RequiredIfRule>, ParamCompileError> {
    let Some(required_if) = required_if else {
        return Ok(None);
    };

    let invalid = |detail: String| ParamCompileError::Invalid {
        name: Some(name.to_owned()),
        detail,
    };

    match required_if {
        RequiredIf::Field(field) => {
            if field.is_empty() {
                return Err(invalid("`required_if` string must not be empty".into()));
            }
            Ok(Some(RequiredIfRule {
                field: normalize(field, case_insensitive),
                equals: None,
            }))
        }
        RequiredIf::Object(object) => {
            if let Some(field) = object.extra.keys().next() {
                return Err(ParamCompileError::UnknownRequiredIfField {
                    name: name.to_owned(),
                    field: field.clone(),
                });
            }

            let Some(field) = object.field.as_deref() else {
                return Err(invalid(
                    "`required_if.field` must be a non-empty string".into(),
                ));
            };
            if field.is_empty() {
                return Err(invalid(
                    "`required_if.field` must be a non-empty string".into(),
                ));
            }

            let Some(equals) = object.equals.as_deref() else {
                return Err(invalid(
                    "`required_if.equals` is required in object form".into(),
                ));
            };

            Ok(Some(RequiredIfRule {
                field: normalize(field, case_insensitive),
                equals: Some(equals.to_owned()),
            }))
        }
    }
}

fn normalize(name: &str, case_insensitive: bool) -> String {
    if case_insensitive {
        name.to_ascii_lowercase()
    } else {
        name.to_owned()
    }
}

/// 只去除 SP / HTAB，与协议对 Header OWS 的定义一致。
pub(crate) fn trim_ows(value: &str) -> &str {
    value.trim_matches(|c| c == ' ' || c == '\t')
}

/// `<dimension>.<name>`，名字里的非 unreserved 字节按 UTF-8 百分号编码，保证响应头是 ASCII。
pub(crate) fn format_field(dimension: &str, name: &str) -> String {
    let mut field = String::with_capacity(dimension.len() + 1 + name.len());
    field.push_str(dimension);
    field.push('.');

    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            field.push(byte as char);
        } else {
            let _ = write!(field, "%{byte:02X}");
        }
    }

    field
}
