use super::*;

impl ReviewApplication {
    pub(super) fn data_json(&self, relative: &Path, missing: ReviewError) -> ReviewResult<Value> {
        let path = relative.to_str().ok_or_else(ReviewError::invalid)?;
        let census = crate::store::Census::open(&self.collection_database)
            .map_err(|_| ReviewError::unavailable())?;
        census.resource(path).map_err(|_| missing)
    }
    pub(super) fn members(&self, dataset: &str) -> ReviewResult<Vec<Value>> {
        if !filename(dataset) {
            return Err(ReviewError::new(
                StatusCode::BAD_REQUEST,
                "Raccolta non valida",
            ));
        }
        let entries = self.data_json(Path::new("manifest.json"), ReviewError::unavailable())?;
        let entries = entries.as_array().ok_or_else(ReviewError::invalid)?;
        let entry = entries
            .iter()
            .find(|row| row["file"] == dataset && row["demo"] != true)
            .ok_or_else(|| ReviewError::new(StatusCode::NOT_FOUND, "Raccolta non approvata"))?;
        if entry["kind"] != "aggregate" {
            return Ok(vec![entry.clone()]);
        }
        let index = self.data_json(Path::new(dataset), ReviewError::invalid())?;
        let members = index["members"]
            .as_array()
            .filter(|m| !m.is_empty())
            .ok_or_else(ReviewError::invalid)?;
        members
            .iter()
            .map(|member| {
                entries
                    .iter()
                    .find(|row| {
                        row["file"] == member["file"]
                            && row["language"] == member["language"]
                            && row["demo"] != true
                            && row["kind"] != "aggregate"
                    })
                    .cloned()
                    .ok_or_else(ReviewError::invalid)
            })
            .collect()
    }
    fn validate_edge(&self, body: &Submission) -> ReviewResult<()> {
        body.validate()?;
        let members = self.members(&body.dataset)?;
        let entry = members
            .iter()
            .find(|e| e["language"] == body.language)
            .ok_or_else(ReviewError::missing_edge)?;
        let folder = entry["connections_dir"]
            .as_str()
            .filter(|s| directory(s))
            .ok_or_else(ReviewError::missing_edge)?;
        let detail = self.data_json(
            &Path::new(folder).join(format!("{}.json", body.target_id)),
            ReviewError::missing_edge(),
        )?;

        let name = entry["connections_metadata_file"]
            .as_str()
            .filter(|s| filename(s))
            .ok_or_else(ReviewError::missing_edge)?;
        let shared = self.data_json(Path::new(name), ReviewError::missing_edge())?;
        let identity = shared["connection_identity"].as_str().filter(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        });
        if detail["format"] != "connections_v2"
            || shared["format"] != "connections_metadata_v1"
            || identity.is_none()
            || shared["connection_identity"] != detail["connection_identity"]
            || !shared["census_page_count"].as_u64().is_some_and(valid_id)
            || !detail["census_page_count"].as_u64().is_some_and(valid_id)
            || shared["census_page_count"] != detail["census_page_count"]
            || detail.get("metadata").is_some()
        {
            return Err(ReviewError::missing_edge());
        }
        let metadata = shared["metadata"].clone();
        let pages = detail["pages"]
            .as_array()
            .ok_or_else(ReviewError::missing_edge)?;
        let candidates =
            detail["link_candidates"][body.target_id.to_string()]["candidates"].as_array();
        if metadata["language"] != body.language
            || metadata["finished_at"] != entry["date"]
            || metadata["complete"] != true
            || metadata["demo"] == true
            || pages.len() != 1
            || pages[0]["page_id"].as_u64() != Some(body.target_id)
            || pages[0]["orphan"] != true
            || !candidates.is_some_and(|rows| {
                rows.iter()
                    .any(|row| row["page_id"].as_u64() == Some(body.source_id))
            })
        {
            return Err(ReviewError::missing_edge());
        }
        Ok(())
    }
    pub(super) fn reviews(&self, dataset: &str) -> ReviewResult<Vec<Review>> {
        let members = self.members(dataset)?;
        let languages = members
            .iter()
            .map(|m| {
                m["language"]
                    .as_str()
                    .filter(|s| language(s))
                    .ok_or_else(ReviewError::invalid)
            })
            .collect::<ReviewResult<BTreeSet<_>>>()?;
        let markers = vec!["?"; languages.len()].join(",");
        let values = languages
            .into_iter()
            .map(SqlValue::from)
            .collect::<Vec<_>>();
        self.database.connect()?.read(&format!("SELECT {COLUMNS} FROM editorial_reviews WHERE language IN ({markers}) ORDER BY language, target_id, source_id"), &values)
    }
    pub(super) fn save(&self, body: Submission, reviewer: &str) -> ReviewResult<Review> {
        self.validate_edge(&body)?;
        self.database.connect()?.save(&body, reviewer)
    }
}
