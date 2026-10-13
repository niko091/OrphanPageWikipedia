//! Bounded MySQL dump row decoding. The caller supplies one INSERT statement
//! at a time; this module never retains a table or reads files into memory.

pub fn parse_insert(statement: &str) -> Result<Vec<Vec<Option<String>>>, String> {
    let offset = if statement.trim_start().starts_with('(') {
        statement.len() - statement.trim_start().len()
    } else {
        let marker = " VALUES";
        statement
            .find(marker)
            .ok_or_else(|| "SQL dump INSERT without VALUES".to_owned())?
            + marker.len()
    };
    let bytes = statement.as_bytes();
    let mut position = offset;
    let mut rows = Vec::new();
    while position < bytes.len() {
        match bytes[position] {
            b'(' => position += 1,
            b';' | b'\n' | b'\r' | b' ' => {
                position += 1;
                continue;
            }
            b',' => {
                position += 1;
                continue;
            }
            _ => return Err("invalid SQL dump tuple boundary".to_owned()),
        }
        if position >= bytes.len() {
            break;
        }
        let mut row = Vec::new();
        loop {
            if position >= bytes.len() {
                return Err("truncated SQL dump tuple".to_owned());
            }
            if bytes[position] == b'\'' {
                position += 1;
                let mut value = Vec::new();
                let mut closed = false;
                while position < bytes.len() {
                    match bytes[position] {
                        b'\\' => {
                            position += 1;
                            let escaped = *bytes
                                .get(position)
                                .ok_or_else(|| "truncated SQL escape".to_owned())?;
                            value.push(match escaped {
                                b'0' => 0,
                                b'n' => b'\n',
                                b'r' => b'\r',
                                b't' => b'\t',
                                b'b' => 8,
                                b'Z' => 26,
                                other => other,
                            });
                            position += 1;
                        }
                        b'\'' => {
                            position += 1;
                            closed = true;
                            break;
                        }
                        byte => {
                            value.push(byte);
                            position += 1;
                        }
                    }
                }
                if !closed {
                    return Err("unterminated SQL dump string".to_owned());
                }
                // SQL varbinary fields such as category sort keys can contain
                // arbitrary bytes. The importer never uses those columns as
                // identities; relevant page/link titles must be valid UTF-8.
                row.push(Some(String::from_utf8_lossy(&value).into_owned()));
            } else {
                let start = position;
                while position < bytes.len() && !matches!(bytes[position], b',' | b')') {
                    position += 1;
                }
                let raw = std::str::from_utf8(&bytes[start..position])
                    .map_err(|_| "invalid SQL dump scalar".to_owned())?
                    .trim();
                if raw.is_empty() {
                    return Err("empty SQL dump scalar".to_owned());
                }
                row.push((raw != "NULL").then(|| raw.to_owned()));
            }
            match bytes.get(position) {
                Some(b',') => position += 1,
                Some(b')') => {
                    position += 1;
                    break;
                }
                _ => return Err("invalid SQL dump field boundary".to_owned()),
            }
        }
        rows.push(row);
    }
    Ok(rows)
}

#[cfg(feature = "python")]
pub fn history_activity(
    events: Vec<(String, i32)>,
    creations: Vec<String>,
) -> Result<Vec<(usize, usize, usize, String)>, String> {
    use chrono::{Datelike, NaiveDateTime};
    use std::collections::HashMap;

    fn parse(value: &str) -> Result<NaiveDateTime, String> {
        NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S%.f")
            .map_err(|_| "invalid MediaWiki History timestamp".to_owned())
    }
    let mut edits = events
        .into_iter()
        .map(|(time, namespace)| parse(&time).map(|parsed| (parsed, namespace)))
        .collect::<Result<Vec<_>, _>>()?;
    edits.sort_unstable_by_key(|row| row.0);
    let mut month_counts = HashMap::<i32, usize>::new();
    for (time, _) in &edits {
        let month = time.year() * 12 + i32::try_from(time.month()).map_err(|e| e.to_string())?;
        *month_counts.entry(month).or_default() += 1;
    }
    creations
        .into_iter()
        .map(|value| {
            let creation = parse(&value)?;
            let earlier = edits.partition_point(|(time, _)| *time < creation);
            let main = edits[..earlier].iter().filter(|(_, ns)| *ns == 0).count();
            let other = earlier - main;
            // The creation month is unfinished at the event. Only complete
            // previous calendar months enter the Vital Signs stability streak.
            let mut month = creation.year() * 12
                + i32::try_from(creation.month()).map_err(|e| e.to_string())?
                - 1;
            let mut active_months = 0;
            while month_counts.get(&month).copied().unwrap_or(0) >= 5 {
                active_months += 1;
                month -= 1;
            }
            let bucket = match active_months {
                0 => "none",
                1 => "one",
                2 => "two",
                3..=6 => "three_to_six",
                7..=12 => "seven_to_twelve",
                13..=24 => "thirteen_to_twenty_four",
                _ => "over_twenty_four",
            };
            Ok((main, other, active_months, bucket.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::parse_insert;

    #[cfg(feature = "python")]
    use super::history_activity;

    #[cfg(feature = "python")]
    #[test]
    fn prior_activity_uses_strict_time_and_completed_months() {
        let mut events = Vec::new();
        for day in 1..=5 {
            events.push((format!("2026-01-{day:02} 10:00:00.0"), 0));
            events.push((format!("2026-02-{day:02} 10:00:00.0"), 1));
        }
        events.push(("2026-03-01 12:00:00.0".to_owned(), 0));
        let metrics = history_activity(events, vec!["2026-03-01 12:00:00.0".to_owned()]).unwrap();
        assert_eq!(metrics, vec![(5, 5, 2, "two".to_owned())]);
    }

    #[test]
    fn parses_mysql_values_and_escapes() {
        let sql = "INSERT INTO `page` VALUES (1,'A\\'B',NULL,-2),(2,'Vèneto\\n',3,4);\n";
        assert_eq!(
            parse_insert(sql).unwrap(),
            vec![
                vec![
                    Some("1".into()),
                    Some("A'B".into()),
                    None,
                    Some("-2".into())
                ],
                vec![
                    Some("2".into()),
                    Some("Vèneto\n".into()),
                    Some("3".into()),
                    Some("4".into())
                ]
            ]
        );
    }

    #[test]
    fn parses_multiline_dump_tuple() {
        assert_eq!(
            parse_insert("(5,0,'Łéngua_vèneta',0,NULL),\n").unwrap(),
            vec![vec![
                Some("5".into()),
                Some("0".into()),
                Some("Łéngua_vèneta".into()),
                Some("0".into()),
                None
            ]]
        );
    }

    #[test]
    fn rejects_truncated_or_malformed_input() {
        for sql in [
            "INSERT INTO `page` VALUES (1,'broken",
            "INSERT INTO `page` VALUES (1,)",
        ] {
            assert!(parse_insert(sql).is_err(), "{sql}");
        }
    }
}
