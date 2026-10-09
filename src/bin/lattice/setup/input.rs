//! Input semantics are independent of presentation language and prompt implementation.
use super::{discovery, i18n::Id, validate_target};
use serde_json::json;

#[derive(Clone)]
pub(super) enum Field {
    Url,
    Model,
    Name(Vec<String>),
    Env,
    Key,
    Tokens,
    OutputTokens(u64),
    Effort,
    Usage { required: bool },
}
impl Field {
    pub fn validate(&self, raw: &str) -> std::result::Result<(), Id> {
        let text = raw.trim();
        let error = match self {
            Self::Url => {
                validate_target(&json!({"adapter":"openai","model":"draft","baseUrl":text}))
                    .err()
                    .map(|_| Id::BadUrl)
            }
            Self::Model => (!discovery::valid_id(text)).then_some(Id::BadModel),
            Self::Name(taken) => {
                if lattice::models::catalog::validate_name(text).is_err() {
                    Some(Id::BadName)
                } else {
                    taken.iter().any(|n| n == text).then_some(Id::DuplicateName)
                }
            }
            Self::Env => {
                let valid = !text.is_empty()
                    && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && !text.starts_with(|c: char| c.is_ascii_digit());
                (!valid || !std::env::var(text).is_ok_and(|v| !v.is_empty())).then_some(Id::BadEnv)
            }
            Self::Key => {
                (text.is_empty() || text == "[redacted]" || text.chars().any(char::is_control))
                    .then_some(Id::BadKey)
            }
            Self::Tokens | Self::OutputTokens(_) => match parse_tokens(text) {
                None => Some(Id::BadTokens),
                Some(n) => {
                    let invalid = match self {
                        Self::OutputTokens(context) => n >= *context,
                        _ => n <= 1,
                    };
                    invalid.then_some(Id::LimitsMissing)
                }
            },
            Self::Effort => {
                let names: Vec<_> = text
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .collect();
                (names.iter().any(|s| {
                    !s.chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                }) || names
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != names.len())
                .then_some(Id::BadEffort)
            }
            Self::Usage { required } => ((text.is_empty() && *required)
                || text.chars().any(char::is_control))
            .then_some(Id::BadUsage),
        };
        error.map_or(Ok(()), Err)
    }
    pub fn answer(&self, text: &str) -> String {
        if matches!(self, Self::Tokens | Self::OutputTokens(_)) {
            parse_tokens(text)
                .map(format_tokens)
                .unwrap_or_else(|| text.into())
        } else {
            text.into()
        }
    }
}

pub(super) fn format_tokens(n: u64) -> String {
    let digits = n.to_string();
    let grouped = digits
        .chars()
        .enumerate()
        .fold(String::new(), |mut s, (i, c)| {
            if i > 0 && (digits.len() - i).is_multiple_of(3) {
                s.push(',');
            }
            s.push(c);
            s
        });
    if n.is_multiple_of(1_000_000) {
        format!("{}M ({grouped} tokens)", n / 1_000_000)
    } else if n.is_multiple_of(1_000) {
        format!("{}k ({grouped} tokens)", n / 1_000)
    } else {
        format!("{grouped} tokens")
    }
}

/// Decimal suffixes are exact arithmetic, never binary units or floating-point rounding.
pub(super) fn parse_tokens(text: &str) -> Option<u64> {
    let text = text.trim();
    if text.is_empty() || text.len() > 64 {
        return None;
    }
    let lower = text.to_ascii_lowercase();
    let (number, multiplier) = if let Some(number) = lower.strip_suffix('m') {
        (number.trim(), 1_000_000u128)
    } else if let Some(number) = lower.strip_suffix('k') {
        (number.trim(), 1_000u128)
    } else {
        (lower.as_str(), 1u128)
    };
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    if (whole.is_empty() && fraction.is_empty())
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
        || (number.contains('.') && fraction.is_empty())
    {
        return None;
    }
    let scale = 10u128.checked_pow(fraction.len().try_into().ok()?)?;
    let whole = if whole.is_empty() {
        0
    } else {
        whole.parse::<u128>().ok()?
    };
    let fraction = if fraction.is_empty() {
        0
    } else {
        fraction.parse::<u128>().ok()?
    };
    let scaled = whole
        .checked_mul(scale)?
        .checked_add(fraction)?
        .checked_mul(multiplier)?;
    if scaled % scale != 0 {
        return None;
    }
    u64::try_from(scaled / scale).ok().filter(|n| *n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validation_is_language_independent_and_formatting_is_exact() {
        assert_eq!(
            Field::Url.validate("https://key@example.invalid"),
            Err(Id::BadUrl)
        );
        assert_eq!(
            Field::Name(vec!["taken".into()]).validate("taken"),
            Err(Id::DuplicateName)
        );
        assert_eq!(Field::Name(vec![]).validate("Invalid"), Err(Id::BadName));
        assert_eq!(Field::Key.validate("[redacted]"), Err(Id::BadKey));
        assert_eq!(Field::Effort.validate("low, low"), Err(Id::BadEffort));
        assert_eq!(
            Field::Usage { required: true }.validate(""),
            Err(Id::BadUsage)
        );
        assert_eq!(Field::Tokens.answer("1M"), "1M (1,000,000 tokens)");
        assert_eq!(Field::Tokens.answer("1048576"), "1,048,576 tokens");
    }
}
