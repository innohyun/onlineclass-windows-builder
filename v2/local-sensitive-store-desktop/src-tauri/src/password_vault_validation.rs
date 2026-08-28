use serde_json::Value;

const CATEGORIES: [&str; 6] = [
    "academic",
    "research",
    "information",
    "classroom",
    "facilities_admin",
    "other",
];

pub(super) fn string(
    value: Option<&Value>,
    label: &str,
    min: usize,
    max: usize,
    trim: bool,
) -> Result<String, String> {
    let raw = value
        .and_then(Value::as_str)
        .ok_or_else(|| format!("password_vault_{label}_invalid"))?;
    let normalized = if trim { raw.trim() } else { raw };
    let count = normalized.chars().count();
    if count < min || count > max || normalized.chars().any(|character| character == '\0') {
        return Err(format!("password_vault_{label}_invalid"));
    }
    Ok(normalized.to_string())
}

pub(super) fn school_code(value: Option<&Value>) -> Result<String, String> {
    let code = string(value, "school_code", 2, 40, true)?;
    if code
        .chars()
        .any(|character| matches!(character, '/' | '\\' | '|'))
    {
        return Err("password_vault_school_code_invalid".to_string());
    }
    Ok(code)
}

pub(super) fn category(value: Option<&Value>) -> Result<String, String> {
    let category = string(value, "category", 1, 32, true)?;
    if !CATEGORIES.contains(&category.as_str()) {
        return Err("password_vault_category_invalid".to_string());
    }
    Ok(category)
}

pub(super) fn opaque(value: Option<&Value>, label: &str, min: usize) -> Result<String, String> {
    let id = string(value, label, min, 128, true)?;
    if !id.chars().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | ':' | '-')
    }) {
        return Err(format!("password_vault_{label}_invalid"));
    }
    Ok(id)
}

pub(super) fn positive(
    value: Option<&Value>,
    label: &str,
    allow_zero: bool,
) -> Result<i64, String> {
    let number = value
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("password_vault_{label}_invalid"))?;
    if number < if allow_zero { 0 } else { 1 } {
        return Err(format!("password_vault_{label}_invalid"));
    }
    Ok(number)
}
