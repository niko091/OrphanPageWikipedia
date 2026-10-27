use super::*;

impl Census {
    pub fn export_csv(&self, dataset: &str, writer: &mut impl std::io::Write) -> DumpResult<()> {
        let entry = self.entry(dataset)?;
        let language = entry["language"]
            .as_str()
            .filter(|l| *l != "all")
            .ok_or("Selezionare una singola edizione")?;
        writer.write_all(dumps::CSV_COLUMNS.join(",").as_bytes())?;
        writer.write_all(b"\r\n")?;
        let nonprintable = regex::Regex::new(r"[\p{C}\p{Z}]")?;
        let mut query=self.db.prepare("SELECT p.data,t.count,t.status,p.id,p.title,p.orphan FROM pages p LEFT JOIN targets t ON t.language=p.language AND t.id=p.id WHERE p.language=? ORDER BY p.position")?;
        let mut rows = query.query([language])?;
        while let Some(row) = rows.next()? {
            let page = decode_page(row.get(0)?, row.get(3)?, row.get(4)?, row.get(5)?)?;
            let count = json!(row.get::<_, Option<i64>>(1)?);
            let status = json!(row.get::<_, Option<String>>(2)?);
            dumps::write_csv_row(
                writer,
                dumps::CSV_COLUMNS.iter().map(|column| match *column {
                    "candidate_count" => &count,
                    "candidate_status" => &status,
                    _ => &page[column],
                }),
                &nonprintable,
            )?;
        }
        Ok(())
    }
}
