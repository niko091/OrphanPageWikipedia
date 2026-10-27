use super::*;

/// Resolve only syntactically valid Wikipedia edition and database identities.
pub fn wiki_database(language: &str, database: Option<&str>) -> Result<String, String> {
    let lower = |byte: &u8| byte.is_ascii_lowercase();
    let valid_language = !language.is_empty()
        && language.len() <= 20
        && language.as_bytes().first().is_some_and(lower)
        && language
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && language != "all";
    if !valid_language {
        return Err("Codice lingua non valido".into());
    }
    let database = database
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{}wiki", language.replace('-', "_")));
    if database.len() < 5
        || database.len() > 45
        || !database.ends_with("wiki")
        || !database.as_bytes().first().is_some_and(lower)
        || !database
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err("Nome database Wikipedia non valido (senza suffisso _p)".into());
    }
    Ok(database)
}

/// Validate the entire sole edition pool, including editions outside a subset.
pub fn configured_languages(config: &serde_json::Value) -> Result<Vec<(String, String)>, String> {
    use serde_json::Value;
    let invalid = || String::from("Configurazione lingue non valida");
    let config = config.as_object().ok_or_else(invalid)?;
    if config
        .keys()
        .any(|key| !["version", "languages", "databases"].contains(&key.as_str()))
        || config.get("version").and_then(Value::as_u64) != Some(1)
    {
        return Err(invalid());
    }
    let languages = config
        .get("languages")
        .and_then(Value::as_array)
        .filter(|values| !values.is_empty())
        .ok_or_else(invalid)?;
    let mut seen = HashSet::new();
    for code in languages {
        if !seen.insert(code.as_str().ok_or_else(invalid)?) {
            return Err(invalid());
        }
    }
    let empty = serde_json::Map::new();
    let databases = match config.get("databases") {
        Some(value) => value.as_object().ok_or_else(invalid)?,
        None => &empty,
    };
    if databases.keys().any(|code| !seen.contains(code.as_str())) {
        return Err(invalid());
    }
    languages
        .iter()
        .map(|value| {
            let code = value.as_str().ok_or_else(invalid)?;
            let database = match databases.get(code) {
                Some(Value::Null) | None => None,
                Some(Value::String(value)) => Some(value.as_str()),
                Some(_) => {
                    return Err("Nome database Wikipedia non valido (senza suffisso _p)".into())
                }
            };
            Ok((code.to_owned(), wiki_database(code, database)?))
        })
        .collect()
}

/// Select a distinct subset while retaining the edition pool's order.
pub fn select_languages(
    editions: &[(String, String)],
    selected: Option<&[String]>,
) -> Result<Vec<(String, String)>, String> {
    let Some(selected) = selected else {
        return Ok(editions.to_vec());
    };
    let requested = selected.iter().collect::<HashSet<_>>();
    if selected.is_empty()
        || requested.len() != selected.len()
        || selected
            .iter()
            .any(|code| !editions.iter().any(|(language, _)| language == code))
    {
        return Err(
            "Le lingue richieste devono essere distinte e presenti nella configurazione".into(),
        );
    }
    Ok(editions
        .iter()
        .filter(|(code, _)| requested.contains(code))
        .cloned()
        .collect())
}
