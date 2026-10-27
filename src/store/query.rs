use super::*;

impl Census {
    pub fn query(&self, request: &PageQuery, closed: &[(String, u64, u64)]) -> DumpResult<Value> {
        request.validate()?;
        let entry = self.entry(&request.dataset)?;
        let aggregate = entry["kind"] == "aggregate";
        let language = entry["language"].as_str().ok_or("Lingua mancante")?;
        let summary = self.resource(filename(&entry["summary_file"])?)?;
        self.db.execute_batch("CREATE TEMP TABLE IF NOT EXISTS closed(language TEXT,target INTEGER,source INTEGER,PRIMARY KEY(language,target,source)) WITHOUT ROWID;
            CREATE TEMP TABLE IF NOT EXISTS weights(label TEXT PRIMARY KEY,weight REAL) WITHOUT ROWID;
            CREATE TEMP TABLE IF NOT EXISTS affinities(language TEXT PRIMARY KEY,weight REAL) WITHOUT ROWID;
            DELETE FROM closed; DELETE FROM weights; DELETE FROM affinities;")?;
        {
            let tx = self.db.unchecked_transaction()?;
            for (language, target, source) in closed {
                tx.execute(
                    "INSERT OR IGNORE INTO closed VALUES(?,?,?)",
                    params![language, target, source],
                )?;
            }
            // Reuse the scientific Rust scoring function on singleton labels. The
            // SQL sum is exactly the same additive score, without loading a census.
            if !request.edits.is_empty() {
                let labels: Vec<String> =
                    serde_json::from_str(&crate::dashboard::calculate("taxonomy", "null")?)?;
                let candidates = labels
                    .iter()
                    .map(|label| json!({"key":label,"language":"","topic_labels":[label]}))
                    .collect::<Vec<_>>();
                let input =
                    json!({"edits":request.edits,"candidates":candidates,"aggregate":false});
                let report: Value = serde_json::from_str(&crate::dashboard::calculate(
                    "interests",
                    &encode(&input)?,
                )?)?;
                for score in report["scores"].as_array().ok_or("Profilo non valido")? {
                    tx.execute(
                        "INSERT INTO weights VALUES(?,?)",
                        params![score["key"].as_str(), score["topic"].as_f64()],
                    )?;
                }
                let langs = request
                    .edits
                    .iter()
                    .filter_map(|e| e["language"].as_str())
                    .collect::<BTreeSet<_>>();
                for lang in langs {
                    let count = request
                        .edits
                        .iter()
                        .filter(|e| e["language"] == lang)
                        .count();
                    tx.execute(
                        "INSERT INTO affinities VALUES(?,?)",
                        params![lang, count as f64 / request.edits.len() as f64],
                    )?;
                }
            }
            tx.commit()?;
        }
        if request.kind == "creators" {
            return self.creators(request, &summary);
        }
        let mut values = Vec::<rusqlite::types::Value>::new();
        let mut conditions = Vec::new();
        if !aggregate {
            conditions.push("p.language=?".to_owned());
            values.push(language.to_owned().into());
        }
        if request.kind == "cards" || request.group == "orphans" {
            conditions.push("p.orphan=1".into());
        } else if request.group == "non-orphans" {
            conditions.push("p.orphan=0".into());
        }
        if request.kind == "cards" {
            if !request.show_disambiguation {
                conditions.push("p.special=0".into());
            }
            if !request.show_redirects {
                conditions.push("p.redirect=0".into());
            }
            if !request.show_without_connections {
                conditions.push("t.count>0".into());
            }
            if !request.hidden_categories.is_empty() {
                let placeholders = vec!["?"; request.hidden_categories.len()].join(",");
                conditions.push(format!(
                    "coalesce(replace(trim(p.category_fold),'_',' '),'') NOT IN ({placeholders})"
                ));
                values.extend(request.hidden_categories.iter().cloned().map(Into::into));
            }
        }
        if !request.search.is_empty() {
            conditions.push("instr(p.title_fold,?)>0".into());
            values.push(request.search.to_lowercase().into());
        }
        // Classification labels preserve empty and unavailable predictions, and
        // use the browser's translated missing labels without persisting them.
        let label = if request.classification == "categories" {
            let prefix = if aggregate {
                "upper(p.language)||' · '||"
            } else {
                ""
            };
            format!("{prefix}coalesce(p.category,?)")
        } else {
            let topic = if request.classification == "macro" {
                "substr(label,1,instr(label,'.')-1)"
            } else {
                "substr(label,1,instr(label,'.')-1)||' · '||substr(label,instr(label,'.')+1)"
            };
            format!("CASE WHEN p.topic_status IS NULL OR p.topic_status!='predicted' THEN ? WHEN NOT EXISTS(SELECT 1 FROM topics x WHERE x.language=p.language AND x.page_id=p.id) THEN ? ELSE (SELECT group_concat(label,' · ') FROM (SELECT DISTINCT {topic} AS label FROM topics x WHERE x.language=p.language AND x.page_id=p.id ORDER BY label)) END")
        };
        let labels = if request.classification == "categories" {
            vec![request.missing_category.clone()]
        } else {
            vec![request.missing_topics.clone(), request.empty_topics.clone()]
        };
        // A CTE binds translated labels once, shared by filter and ordering.
        let prefix=format!("WITH listed AS (SELECT p.*,t.count AS candidate_count,{label} AS display_label,
            EXISTS(SELECT 1 FROM shortlists s WHERE s.language=p.language AND s.target=p.id) AND NOT EXISTS(SELECT 1 FROM shortlists s WHERE s.language=p.language AND s.target=p.id AND NOT EXISTS(SELECT 1 FROM closed c WHERE c.language=s.language AND c.target=s.target AND c.source=s.source)) AS reviewed,
            coalesce((SELECT sum(w.weight) FROM topics x JOIN weights w ON w.label=x.label WHERE x.language=p.language AND x.page_id=p.id),0) AS topic_score,
            coalesce((SELECT weight FROM affinities a WHERE a.language=p.language),0) AS language_score
            FROM pages p LEFT JOIN targets t ON t.language=p.language AND t.id=p.id{} )",if conditions.is_empty(){String::new()}else{format!(" WHERE {}",conditions.join(" AND "))});
        let mut bindings = labels
            .into_iter()
            .map(rusqlite::types::Value::from)
            .collect::<Vec<_>>();
        bindings.extend(values);
        let mut filter = String::new();
        if !request.category.is_empty() {
            if request.classification == "categories" {
                bindings.push(request.missing_category.to_lowercase().into());
                filter = if aggregate {
                    " WHERE instr(lower(language)||' · '||coalesce(category_fold,?),?)>0"
                } else {
                    " WHERE instr(coalesce(category_fold,?),?)>0"
                }
                .into();
            } else {
                let topic = if request.classification == "macro" {
                    "substr(x.label,1,instr(x.label,'.')-1)"
                } else {
                    "substr(x.label,1,instr(x.label,'.')-1)||' · '||substr(x.label,instr(x.label,'.')+1)"
                };
                let contains=" WHERE ((topic_status IS NULL OR topic_status!='predicted') AND instr(?,?)>0 OR topic_status='predicted' AND NOT EXISTS(SELECT 1 FROM topics x WHERE x.language=listed.language AND x.page_id=listed.id) AND instr(?,?)>0 OR EXISTS(SELECT 1 FROM topics x WHERE x.language=listed.language AND x.page_id=listed.id AND instr(lower(";
                filter = format!("{contains}{topic}),?)>0))");
                bindings.push(request.missing_topics.to_lowercase().into());
                bindings.push(request.category.to_lowercase().into());
                bindings.push(request.empty_topics.to_lowercase().into());
                bindings.push(request.category.to_lowercase().into());
            }
            bindings.push(request.category.to_lowercase().into());
        }
        let total: u64 = self.db.query_row(
            &format!("{prefix} SELECT count(*) FROM listed{filter}"),
            rusqlite::params_from_iter(&bindings),
            |r| r.get(0),
        )?;
        let direction = if request.order.ends_with("asc") {
            "ASC"
        } else {
            "DESC"
        };
        let field = if request.order.starts_with("length") {
            Some("length")
        } else if request.order.starts_with("date") {
            Some("created")
        } else if request.order.starts_with("candidates") {
            Some("candidate_count")
        } else {
            None
        };
        let mut order = if let Some(field) = field {
            format!("{field} IS NULL,{field} {direction},")
        } else if request.order == "category-asc" {
            "display_label,".into()
        } else {
            String::new()
        };
        if request.order == "personalized" {
            order = format!(
                "topic_score DESC,{}coalesce(candidate_count,0) DESC,",
                if aggregate {
                    "language_score DESC,"
                } else {
                    ""
                }
            );
        }
        if request.kind == "cards" {
            order = format!("reviewed,{order}");
        }
        bindings.push(PAGE_SIZE.into());
        bindings.push(request.offset.into());
        let mut query=self.db.prepare(&format!("{prefix} SELECT language,id,data,title,orphan FROM listed{filter} ORDER BY {order}title_fold,title,id,language LIMIT ? OFFSET ?"))?;
        let mut rows = query.query(rusqlite::params_from_iter(bindings))?;
        let mut pages = Vec::with_capacity(PAGE_SIZE as usize);
        let mut records = serde_json::Map::new();
        let mut transports = serde_json::Map::new();
        while let Some(row) = rows.next()? {
            let language: String = row.get(0)?;
            let id: i64 = row.get(1)?;
            let mut page = decode_page(row.get(2)?, id, row.get(3)?, row.get(4)?)?;
            page["language"] = json!(language);
            let record = if request.kind == "cards" {
                let mut record = self.record(&language, id, true)?;
                let ids = record["candidates"]
                    .as_array()
                    .ok_or("Shortlist mancante")?
                    .iter()
                    .map(|c| c["page_id"].clone())
                    .collect::<Vec<_>>();
                record
                    .as_object_mut()
                    .ok_or("Record non valido")?
                    .remove("candidates");
                record["candidate_ids"] = json!(ids);
                record
            } else if page["orphan"] == true {
                self.db.query_row("SELECT count,status FROM targets WHERE language=? AND id=?",params![language,id],|r|Ok(json!({"candidate_count":r.get::<_,Option<i64>>(0)?,"status":r.get::<_,Option<String>>(1)?})))?
            } else {
                Value::Null
            };
            records.insert(
                if aggregate {
                    format!("{language}:{id}")
                } else {
                    id.to_string()
                },
                record,
            );
            if request.kind == "cards" && !transports.contains_key(&language) {
                let (metadata, count) = self.meta(&language)?;
                let entry =
                    self.json("SELECT entry FROM datasets WHERE language=?", [&language])?;
                transports.insert(language,json!({"directory":entry["cards_dir"],"identity":dumps::canonical_json_sha256(&json!([RULE,metadata]))?,"metadata":metadata,"census_page_count":count}));
            }
            pages.push(page);
        }
        Ok(
            json!({"format":"page_results_v1","dataset":request.dataset,"metadata":summary["metadata"],"census_page_count":summary["census_page_count"],"offset":request.offset,"total":total,"pages":pages,"link_candidates":records,"card_transports":transports}),
        )
    }
    fn creators(&self, request: &PageQuery, summary: &Value) -> DumpResult<Value> {
        let field = if request.order.starts_with("articles") {
            if !matches!(
                summary["metadata"]["creators"]["status"].as_str(),
                Some("collected" | "partially_collected")
            ) && matches!(
                summary["metadata"]["history"]["status"].as_str(),
                Some("collected" | "partially_collected")
            ) {
                "total"
            } else {
                "articles"
            }
        } else {
            "orphans"
        };
        let direction = if request.order.ends_with("asc") {
            "ASC"
        } else {
            "DESC"
        };
        let total: u64 = self.db.query_row(
            "SELECT count(*) FROM creators WHERE dataset=? AND instr(key_fold,?)>0",
            params![request.dataset, request.search.to_lowercase()],
            |r| r.get(0),
        )?;
        let mut query=self.db.prepare(&format!("SELECT data FROM creators WHERE dataset=? AND instr(key_fold,?)>0 ORDER BY {field} IS NULL,{field} {direction},key LIMIT {PAGE_SIZE} OFFSET ?"))?;
        let rows = query
            .query_map(
                params![
                    request.dataset,
                    request.search.to_lowercase(),
                    request.offset
                ],
                |r| r.get::<_, String>(0),
            )?
            .map(|r| Ok(serde_json::from_str::<Value>(&r?)?))
            .collect::<DumpResult<Vec<_>>>()?;
        Ok(
            json!({"format":"creator_results_v1","dataset":request.dataset,"offset":request.offset,"total":total,"creators":rows}),
        )
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageQuery {
    pub dataset: String,
    pub kind: String,
    #[serde(default)]
    pub search: String,
    #[serde(default)]
    pub category: String,
    #[serde(default = "default_group")]
    pub group: String,
    #[serde(default = "default_classification")]
    pub classification: String,
    #[serde(default = "default_order")]
    pub order: String,
    #[serde(default)]
    pub offset: i64,
    #[serde(default)]
    pub show_disambiguation: bool,
    #[serde(default)]
    pub show_redirects: bool,
    #[serde(default)]
    pub show_without_connections: bool,
    #[serde(default)]
    pub hidden_categories: Vec<String>,
    #[serde(default)]
    pub edits: Vec<Value>,
    #[serde(default)]
    pub closed_pairs: Vec<(String, u64, u64)>,
    #[serde(default)]
    pub missing_category: String,
    #[serde(default)]
    pub missing_topics: String,
    #[serde(default)]
    pub empty_topics: String,
}
fn default_group() -> String {
    "orphans".into()
}
fn default_classification() -> String {
    "categories".into()
}
fn default_order() -> String {
    "title-asc".into()
}
impl PageQuery {
    pub(super) fn validate(&self) -> DumpResult<()> {
        if !matches!(self.kind.as_str(), "articles" | "cards" | "creators")
            || !matches!(
                self.classification.as_str(),
                "categories" | "macro" | "topics"
            )
            || !matches!(self.group.as_str(), "all" | "orphans" | "non-orphans")
            || !matches!(
                self.order.as_str(),
                "title-asc"
                    | "category-asc"
                    | "length-asc"
                    | "length-desc"
                    | "date-asc"
                    | "date-desc"
                    | "candidates-asc"
                    | "candidates-desc"
                    | "orphans-asc"
                    | "orphans-desc"
                    | "articles-asc"
                    | "articles-desc"
                    | "personalized"
            )
            || self.offset < 0
            || self.offset % PAGE_SIZE != 0
            || self.edits.len() > 100
            || self.closed_pairs.len() > 100000
            || self.hidden_categories.len() > 128
            || self
                .hidden_categories
                .iter()
                .any(|category| category.len() > 256)
            || self.closed_pairs.iter().any(|(language, target, source)| {
                crate::wiki_database(language, None).is_err()
                    || *target == 0
                    || *source == 0
                    || *target > 9_007_199_254_740_991
                    || *source > 9_007_199_254_740_991
            })
            || [
                &self.search,
                &self.category,
                &self.missing_category,
                &self.missing_topics,
                &self.empty_topics,
            ]
            .iter()
            .any(|s| s.len() > 2048)
        {
            return Err("Parametri di selezione non validi".into());
        }
        Ok(())
    }
}
