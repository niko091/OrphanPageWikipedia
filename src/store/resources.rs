use super::*;

impl Census {
    pub(super) fn record(&self, language: &str, id: i64, short: bool) -> DumpResult<Value> {
        let stored: Option<String> = self
            .db
            .query_row(
                "SELECT data FROM targets WHERE language=? AND id=?",
                params![language, id],
                |r| r.get(0),
            )
            .optional()?;
        let stored = stored.ok_or("Risultato del confronto mancante")?;
        let mut record: Value = serde_json::from_str(&stored)?;
        let sql = if short {
            "SELECT c.source,c.data,t.value,p.category FROM candidates c JOIN texts t ON t.id=c.title_id LEFT JOIN pages p ON p.language=c.language AND p.id=c.source JOIN shortlists s ON s.language=c.language AND s.target=c.target AND s.source=c.source WHERE c.language=? AND c.target=? ORDER BY s.position"
        } else {
            "SELECT c.source,c.data,t.value,p.category FROM candidates c JOIN texts t ON t.id=c.title_id LEFT JOIN pages p ON p.language=c.language AND p.id=c.source WHERE c.language=? AND c.target=? ORDER BY c.position"
        };
        let mut query = self.db.prepare(sql)?;
        let mut rows = query.query(params![language, id])?;
        let mut candidates = Vec::new();
        while let Some(row) = rows.next()? {
            let source: i64 = row.get(0)?;
            let mut candidate: Value = serde_json::from_str(&row.get::<_, String>(1)?)?;
            candidate["page_id"] = json!(source);
            candidate["title"] = json!(row.get::<_, String>(2)?);
            // Read existing census metadata for UI filters; never rewrite the collection.
            candidate["category"] = json!(row.get::<_, Option<String>>(3)?);
            let join = if short {
                " JOIN shortlists s ON s.language=e.language AND s.target=e.target AND s.source=e.source AND s.evidence_position=e.position"
            } else {
                ""
            };
            let mut edges=self.db.prepare(&format!("SELECT e.data,e.reference_language,a.value,b.value FROM evidence e JOIN texts a ON a.id=e.source_title_id JOIN texts b ON b.id=e.target_title_id{join} WHERE e.language=? AND e.target=? AND e.source=? ORDER BY e.position"))?;
            let mut rows = edges.query(params![language, id, source])?;
            let mut evidence = Vec::new();
            while let Some(row) = rows.next()? {
                let mut edge: Value = serde_json::from_str(&row.get::<_, String>(0)?)?;
                edge["language"] = json!(row.get::<_, String>(1)?);
                edge["source_title"] = json!(row.get::<_, String>(2)?);
                edge["target_title"] = json!(row.get::<_, String>(3)?);
                evidence.push(edge);
            }
            candidate["evidence"] = json!(evidence);
            candidates.push(candidate);
        }
        record["candidates"] = json!(candidates);
        Ok(record)
    }
    pub fn resource(&self, path: &str) -> DumpResult<Value> {
        let direct: Option<String> = self
            .db
            .query_row("SELECT data FROM documents WHERE path=?", [path], |r| {
                r.get(0)
            })
            .optional()?;
        if let Some(value) = direct {
            return Ok(serde_json::from_str(&value)?);
        }
        for entry in self.manifest()?.as_array().ok_or("Manifest non valido")? {
            if entry["kind"] == "aggregate" {
                continue;
            }
            let language = entry["language"].as_str().ok_or("Lingua mancante")?;
            let (meta, count) = self.meta(language)?;
            for key in ["cards_dir", "connections_dir", "profile_topics_dir"] {
                let Some(folder) = entry[key].as_str() else {
                    continue;
                };
                let Some(leaf) = path.strip_prefix(&format!("{folder}/")) else {
                    continue;
                };
                if key == "profile_topics_dir" {
                    if leaf == "index.json" {
                        let mut query = self.db.prepare(
                            "SELECT DISTINCT id/1024 FROM pages WHERE language=? ORDER BY id/1024",
                        )?;
                        let shards = query
                            .query_map([language], |r| r.get::<_, i64>(0))?
                            .collect::<Result<Vec<_>, _>>()?;
                        return Ok(
                            json!({"format":"profile_topics_index_v1","metadata":meta,"census_page_count":count,"shard_size":1024,"shards":shards}),
                        );
                    }
                    let bucket = leaf
                        .strip_suffix(".json")
                        .ok_or("Shard non valido")?
                        .parse::<i64>()?;
                    if !(0..=9_007_199_254_740_991 / 1024).contains(&bucket) {
                        return Err("Shard non valido".into());
                    }
                    let mut query = self.db.prepare(
                        "SELECT data,id FROM pages WHERE language=? AND id>=? AND id<? ORDER BY id",
                    )?;
                    let mut rows =
                        query.query(params![language, bucket * 1024, (bucket + 1) * 1024])?;
                    let mut values = Vec::new();
                    while let Some(row) = rows.next()? {
                        let p: Value = serde_json::from_str(&row.get::<_, String>(0)?)?;
                        values.push(json!([
                            row.get::<_, i64>(1)?,
                            p["topic_status"],
                            p["topic_labels"]
                        ]));
                    }
                    if values.is_empty() {
                        return Err("Shard non presente nella raccolta".into());
                    }
                    return Ok(
                        json!({"format":"profile_topics_shard_v1","metadata":meta,"bucket":bucket,"rows":values}),
                    );
                }
                let id = leaf
                    .strip_suffix(".json")
                    .ok_or("Pagina non valida")?
                    .parse::<i64>()?;
                let page = self.page(language, id)?;
                if page["orphan"] != true {
                    return Err("Pagina non orfana".into());
                }
                let record = self.record(language, id, key == "cards_dir")?;
                if key == "cards_dir" {
                    let identity = dumps::canonical_json_sha256(&json!([RULE, meta]))?;
                    return Ok(
                        json!({"format":"orphan_card_v1","card_identity":identity,"census_page_count":count,"page":page,"candidate_record":record}),
                    );
                }
                let shared = self.resource(filename(&entry["connections_metadata_file"])?)?;
                return Ok(
                    json!({"format":"connections_v2","connection_identity":shared["connection_identity"],"census_page_count":count,"pages":[page],"link_candidates":{id.to_string():record}}),
                );
            }
        }
        Err("Risorsa non approvata".into())
    }
}
