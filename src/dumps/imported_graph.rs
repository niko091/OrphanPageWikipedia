#[cfg(feature = "cli")]
use super::{
    file_signature, first_metadata, next_relation, resolved_path, write_atomic, DumpClient,
    WikimediaHttp,
};
use super::{DumpResult, SQL_TABLES};

#[cfg(feature = "cli")]
pub(super) struct RecordSpool(std::path::PathBuf);

#[cfg(feature = "cli")]
impl Drop for RecordSpool {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(feature = "cli")]
impl RecordSpool {
    pub(super) fn new(output: &std::path::Path) -> DumpResult<(Self, std::fs::File)> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let parent = output.parent().ok_or("Directory dei record mancante")?;
        std::fs::create_dir_all(parent)?;
        let path = parent.join(format!(
            ".orphanwiki-records-{}-{}.jsonl",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok((Self(path), file))
    }

    pub(super) fn publish(
        &self,
        output: &std::path::Path,
        metadata: &serde_json::Value,
    ) -> DumpResult<()> {
        use std::io::{BufRead, Write};
        write_atomic(output, |file| {
            let mut writer = std::io::BufWriter::with_capacity(64 * 1024, file);
            writer.write_all(b"{\"metadata\":")?;
            serde_json::to_writer(&mut writer, metadata)?;
            writer.write_all(b",\"pages\":[")?;
            for (position, line) in std::io::BufReader::new(std::fs::File::open(&self.0)?)
                .lines()
                .enumerate()
            {
                if position != 0 {
                    writer.write_all(b",")?;
                }
                writer.write_all(line?.as_bytes())?;
            }
            writer.write_all(b"]}")?;
            writer.flush()?;
            Ok(())
        })
    }
}

#[cfg(feature = "cli")]
pub(crate) fn file_sha256(path: &std::path::Path) -> DumpResult<String> {
    DumpClient::<WikimediaHttp>::verified_sha256(path, None, None)?
        .ok_or_else(|| "Input mancante".into())
}

#[cfg(feature = "cli")]
pub(super) struct ImportedGraph {
    pub(super) metadata: serde_json::Value,
    manifest: serde_json::Value,
    pub(super) manifest_path: std::path::PathBuf,
    stage_path: std::path::PathBuf,
    pub(super) source: rusqlite::Connection,
    pub(super) expected: usize,
    pub(super) raw_signature: (u64, i64),
}

#[cfg(feature = "cli")]
impl ImportedGraph {
    pub(super) fn open(
        raw: &std::path::Path,
        work: Option<&std::path::Path>,
        items: bool,
    ) -> DumpResult<Self> {
        use serde_json::{json, Value};
        use std::{collections::HashMap, path::Path};
        let raw_signature = file_signature(raw)?;
        let metadata = first_metadata(raw)?;
        let language = metadata["language"]
            .as_str()
            .ok_or("Lingua del grafo mancante")?;
        let database = crate::wiki_database(language, metadata["database"].as_str())?;
        let release = metadata["sql_release"]
            .as_str()
            .ok_or("Release SQL mancante")?;
        if metadata["complete"] != true
            || metadata["source"] != format!("Wikimedia SQL dumps / {database}")
            || metadata["finished_at"].as_str().is_none()
            || (items
                && metadata["item_mapping"]
                    != json!({"source":"page_props.wikibase_item","complete":true}))
            || release.len() != 8
            || !release.bytes().all(|c| c.is_ascii_digit())
        {
            return Err("Serve un grafo SQL importato e completo".into());
        }
        let manifest_path = Path::new(
            metadata["dump_manifest"]
                .as_str()
                .ok_or("Manifest SQL mancante")?,
        )
        .to_owned();
        let manifest: Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
        if file_sha256(&manifest_path)? != metadata["dump_manifest_sha256"]
            || manifest["language"] != language
            || manifest["database"] != database
            || manifest["sql_release"] != release
        {
            return Err("Manifest SQL non corrispondente al grafo importato".into());
        }
        let parent = manifest_path.parent().unwrap_or(Path::new("."));
        let default_work = parent.parent().unwrap_or(parent).join("staging");
        let stage_path = work
            .unwrap_or(&default_work)
            .join(format!("{database}-{release}.sqlite"));
        if !stage_path.is_file() {
            return Err("Staging SQL mancante".into());
        }
        let source = rusqlite::Connection::open_with_flags(
            &stage_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        source.execute_batch("BEGIN")?;
        let imported = source
            .prepare("SELECT table_name,sha256 FROM imported")?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<Result<HashMap<_, _>, _>>()?;
        if SQL_TABLES.iter().any(
            |table| match manifest["files"][table]["sha256_local"].as_str() {
                Some(expected) => imported.get(*table).map(String::as_str) != Some(expected),
                None => true,
            },
        ) {
            return Err("Staging SQL incompleto o non corrispondente al manifest".into());
        }
        let expected = source.query_row(
            "SELECT COUNT(*) FROM dump_page WHERE ns=0 AND redirect=0",
            [],
            |r| r.get::<_, usize>(0),
        )?;
        if expected == 0
            || source.query_row("SELECT COUNT(*) FROM pages", [], |r| r.get::<_, usize>(0))?
                != expected
        {
            return Err("Popolazione dello staging SQL incompleta".into());
        }
        Ok(Self {
            metadata,
            manifest,
            manifest_path,
            stage_path,
            source,
            expected,
            raw_signature,
        })
    }

    pub(super) fn check_output(
        &self,
        raw: &std::path::Path,
        output: &std::path::Path,
        supplements: &[&std::path::Path],
    ) -> DumpResult<()> {
        use std::path::Path;
        let output = resolved_path(output)?;
        for input in [raw, self.stage_path.as_path(), self.manifest_path.as_path()]
            .into_iter()
            .chain(supplements.iter().copied())
            .chain(
                self.manifest["files"]
                    .as_object()
                    .ok_or("File SQL mancanti")?
                    .values()
                    .filter_map(|r| r["path"].as_str().map(Path::new)),
            )
        {
            if resolved_path(input)? == output {
                return Err("Output sovrapposto agli input".into());
            }
        }
        Ok(())
    }
}

#[cfg(feature = "cli")]
pub(super) fn sql_redirects(
    source: &rusqlite::Connection,
) -> DumpResult<std::collections::HashMap<String, String>> {
    Ok(source.prepare("SELECT p.title,r.title FROM dump_redirect r JOIN dump_page p ON p.id=r.page_id WHERE p.ns=0 AND p.redirect=1 AND r.ns=0 AND COALESCE(r.interwiki,'')=''")?.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<Result<_,_>>()?)
}

#[cfg(feature = "cli")]
pub(super) fn sql_category_parents(
    source: &rusqlite::Connection,
) -> DumpResult<std::collections::BTreeMap<String, Vec<String>>> {
    let mut parents = std::collections::BTreeMap::<String, Vec<String>>::new();
    let mut query = source.prepare("SELECT p.title,COALESCE(t.title,c.title) FROM dump_category c JOIN dump_page p ON p.id=c.source_id LEFT JOIN dump_target t ON t.id=c.target_id WHERE p.ns=14 AND (t.ns=14 OR c.title IS NOT NULL) AND NOT EXISTS (SELECT 1 FROM dump_page h JOIN dump_prop hp ON hp.page_id=h.id WHERE h.ns=14 AND hp.name='hiddencat' AND h.title=COALESCE(t.title,c.title))")?;
    let mut rows = query.query([])?;
    while let Some(row) = rows.next()? {
        parents.entry(row.get(0)?).or_default().push(row.get(1)?);
    }
    Ok(parents)
}

#[cfg(feature = "cli")]
pub(super) struct StagePage {
    pub(super) id: i64,
    pub(super) data: serde_json::Value,
    pub(super) links: Vec<String>,
    pub(super) categories: Vec<String>,
}

#[cfg(feature = "cli")]
pub(super) fn stage_page_batches(
    source: &rusqlite::Connection,
    batch_size: usize,
    include_links: bool,
    mut visit: impl FnMut(Vec<StagePage>) -> DumpResult<()>,
) -> DumpResult<()> {
    crate::runtime::phase("sql.page_batches", || {
        crate::runtime::check_cancelled()?;
        let mut pages_query = source.prepare("SELECT id,data FROM pages ORDER BY id")?;
        let mut cats_query =
            source.prepare("SELECT id,target FROM categories ORDER BY id,rowid")?;
        let mut links_query = source.prepare("SELECT id,target FROM links ORDER BY id,rowid")?;
        let mut pages = pages_query.query([])?;
        let mut cats = cats_query.query([])?;
        let mut links = if include_links {
            Some(links_query.query([])?)
        } else {
            None
        };
        let mut cat = next_relation(&mut cats)?;
        let mut link = links.as_mut().map(next_relation).transpose()?.flatten();
        let mut batch = Vec::with_capacity(batch_size);
        while let Some(row) = pages.next()? {
            let id = row.get::<_, i64>(0)?;
            if cat.as_ref().is_some_and(|(source, _)| *source < id)
                || link.as_ref().is_some_and(|(source, _)| *source < id)
            {
                return Err("Staging con relazione senza la voce sorgente".into());
            }
            let mut labels = Vec::new();
            while cat.as_ref().is_some_and(|(source, _)| *source == id) {
                if let Some((_, label)) = cat.take() {
                    labels.push(label);
                }
                cat = next_relation(&mut cats)?;
            }
            let mut targets = Vec::new();
            while link.as_ref().is_some_and(|(source, _)| *source == id) {
                if let Some((_, title)) = link.take() {
                    targets.push(title);
                }
                link = links.as_mut().map(next_relation).transpose()?.flatten();
            }
            let data: serde_json::Value = serde_json::from_str(&row.get::<_, String>(1)?)?;
            if data["page_id"].as_i64() != Some(id) || !data.is_object() {
                return Err("Identità delle pagine SQL non corrispondenti".into());
            }
            batch.push(StagePage {
                id,
                data,
                links: targets,
                categories: labels,
            });
            if batch.len() == batch_size {
                visit(std::mem::replace(
                    &mut batch,
                    Vec::with_capacity(batch_size),
                ))?;
            }
        }
        if link.is_some() || cat.is_some() {
            return Err("Relazioni SQL non associate a voci".into());
        }
        if !batch.is_empty() {
            visit(batch)?;
        }
        Ok(())
    })
}

#[cfg(feature = "cli")]
pub(super) struct StagedRecords<'a> {
    rows: rusqlite::Rows<'a>,
    next: Option<(i64, String)>,
}

#[cfg(feature = "cli")]
impl<'a> StagedRecords<'a> {
    pub(super) fn new(query: &'a mut rusqlite::Statement<'_>) -> DumpResult<Self> {
        let mut rows = query.query([])?;
        let next = next_relation(&mut rows)?;
        Ok(Self { rows, next })
    }
    pub(super) fn take(&mut self, id: i64) -> DumpResult<serde_json::Value> {
        if self.next.as_ref().map(|(next, _)| *next) != Some(id) {
            return Err("Supplemento con ID mancanti o estranei al censimento".into());
        }
        let (_, encoded) = self.next.take().ok_or("Record supplemento mancante")?;
        self.next = next_relation(&mut self.rows)?;
        Ok(serde_json::from_str(&encoded)?)
    }
    pub(super) fn finish(&self) -> DumpResult<()> {
        if self.next.is_some() {
            return Err("Record supplemento oltre il censimento".into());
        }
        Ok(())
    }
}
