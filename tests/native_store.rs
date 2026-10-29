#![cfg(feature = "cli")]

use orphanwiki::{
    dumps,
    store::{self, Census, PageQuery},
};
use serde_json::{json, Value};
use std::{
    fs,
    io::BufRead,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

struct Fixture {
    root: PathBuf,
    entries: Value,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
fn write(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}
impl Fixture {
    fn new(release: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "orphanwiki-store-{}",
            orphanwiki::runtime::unique_id()
        ));
        fs::create_dir_all(&root).unwrap();
        let mut entries = Vec::new();
        let mut members = Vec::new();
        for (language, reference) in [("vec", "lmo"), ("lmo", "vec")] {
            let meta = json!({"language":language,"complete":true,"started_at":"2026-10-01T00:00:00Z","finished_at":"2026-10-02T00:00:00Z",
                "sql_release":release,"history":{"status":"collected","history_release":"2026-09"},
                "quality":{"status":"collected","content_release":"2026-10-01"},"topics":{"status":"predicted"},"measures_rule":"creation_quality_creators_v3"});
            let pages=(1..=61).chain([101,102]).map(|id|json!({"page_id":id,"title":format!("Page {id:03}"),"orphan":id<=61,
                "category":if id==2&&language=="vec"{"Páginas de desambiguação"}else if id==2{"Paginas de desambiguac\u{0327}a\u{0303}o"}else if id==3{"Redireccions"}else{"Musica"},"length_bytes":if id==1{None}else{Some(id*10)},
                "created_at":if id==60{Some("2026-10-01T00:00:00+02:00")}else if id==61{Some("2026-09-30T23:00:00Z")}else{None},
                "topic_status":if id==1{"missing"}else{"predicted"},"topic_labels":if id==1{Value::Null}else if id==2{json!([])}else if id==61{json!(["Culture.Media.Music","STEM.STEM*"])}else if id%2==1{json!(["Culture.Media.Music"])}else{json!(["STEM.STEM*"])},
                "creator_continuity_months_before_creation":if id==1{None}else{Some(if id==102{113}else{id%14})},
                "history_creator_key":format!("local-{}",id%4),"quality_score":if id==1{None}else{Some(0.5)}})).collect::<Vec<_>>();
            let mut records = serde_json::Map::new();
            for id in 1..=61 {
                let candidates = if id == 4 || id == 5 {
                    vec![]
                } else {
                    [101,102].iter().map(|source|json!({"page_id":source,"title":format!("Page {source:03}"),"dead_end":*source==101,"out_degree":if *source==101{0}else{2},
                    "evidence":[{"language":reference,"source_page_id":source,"source_title":"","target_page_id":id,"target_title":format!("Page {id:03}")},
                    {"language":reference,"source_page_id":source,"source_title":format!("Page {source:03}"),"target_page_id":id,"target_title":format!("Page {id:03}")}]})).collect()
                };
                records.insert(id.to_string(),json!({"status":if id==5{"missing_item"}else{"compared"},"candidate_count":if id==5{None}else if id==4{Some(0)}else{Some(2)},"counterparts":[reference],"candidates":candidates}));
            }
            let file = format!("{language}-{release}.json");
            fs::write(
                root.join(&file),
                format!(
                    "{{\"metadata\":{meta},\"pages\":{},\"link_candidates\":{}}}",
                    json!(pages),
                    Value::Object(records)
                ),
            )
            .unwrap();
            let temporary = root.join(format!("prepared-{language}"));
            let mut names =
                dumps::prepare_dashboard_exports(&root.join(&file), &temporary, None, None)
                    .unwrap();
            for entry in fs::read_dir(&temporary).unwrap() {
                let entry = entry.unwrap();
                fs::rename(entry.path(), root.join(entry.file_name())).unwrap();
            }
            fs::remove_dir(temporary).unwrap();
            names["language"] = json!(language);
            names["file"] = json!(file);
            names["date"] = meta["finished_at"].clone();
            entries.push(names);
            members.push(json!({"language":language,"file":file,"path":root.join(&file),"page_count":pages.len(),"metadata":meta}));
        }
        members.sort_by(|a, b| a["language"].as_str().cmp(&b["language"].as_str()));
        let index = dumps::aggregate_index(&members).unwrap();
        write(&root.join("all.json"), &index);
        dumps::write_aggregate_summary(
            &members,
            &root.join("all-summary.json"),
            &index["metadata"],
            true,
        )
        .unwrap();
        entries.push(json!({"language":"all","kind":"aggregate","file":"all.json","date":index["metadata"]["finished_at"],"summary_file":"all-summary.json","creator_index_file":"all-creators.json"}));
        let entries = json!(entries);
        write(&root.join("manifest.json"), &entries);
        Self { root, entries }
    }
    fn db(&self) -> PathBuf {
        self.root.join("collection.sqlite")
    }
    fn publish(&self) {
        store::publish(&self.root, &self.db(), &json!({"test":true})).unwrap();
    }
    fn request(&self, kind: &str) -> PageQuery {
        serde_json::from_value(json!({"dataset":self.entries[0]["file"],"kind":kind,"group":"all","missing_category":"Senza categoria idonea","missing_topics":"Temi non disponibili","empty_topics":"Nessun tema sopra soglia"})).unwrap()
    }
}

#[test]
fn older_collections_gain_the_discrete_continuity_view_without_rewriting_the_database() {
    let w = Fixture::new("20261001");
    let mut expected = Vec::new();
    for entry in w.entries.as_array().unwrap() {
        let path = w.root.join(entry["summary_file"].as_str().unwrap());
        let mut summary: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let report =
            &mut summary["reports"]["numeric"]["creator_continuity_months_before_creation"];
        let months = report
            .as_object_mut()
            .unwrap()
            .remove("month_histogram")
            .unwrap();
        expected.push((
            entry["summary_file"].as_str().unwrap().to_owned(),
            months,
            report.clone(),
        ));
        write(&path, &summary);
    }
    w.publish();
    let before = fs::read(w.db()).unwrap();
    let census = Census::open(&w.db()).unwrap();
    for (path, expected_months, original_report) in expected {
        let mut summary = census.resource(&path).unwrap();
        let report =
            &mut summary["reports"]["numeric"]["creator_continuity_months_before_creation"];
        assert_eq!(
            report
                .as_object_mut()
                .unwrap()
                .remove("month_histogram")
                .unwrap(),
            expected_months
        );
        assert_eq!(*report, original_report);
    }
    census.verify().unwrap();
    assert_eq!(fs::read(w.db()).unwrap(), before);
}

#[test]
fn relational_queries_preserve_full_census_missing_values_filters_and_every_edge() {
    let w = Fixture::new("20261001");
    w.publish();
    let db = Census::open(&w.db()).unwrap();
    let mut request = w.request("articles");
    let first = db.query(&request, &[]).unwrap();
    assert_eq!(first["total"], 63);
    assert_eq!(first["pages"].as_array().unwrap().len(), 25);
    assert_eq!(first["pages"][0]["length_bytes"], Value::Null);
    request.offset = 25;
    let second = db.query(&request, &[]).unwrap();
    assert_eq!(second["pages"].as_array().unwrap().len(), 25);
    assert_eq!(second["pages"][0]["page_id"], 26);
    request.offset = 20;
    assert!(db.query(&request, &[]).is_err());
    request.offset = 0;
    request.search = "Page 06".into();
    assert_eq!(db.query(&request, &[]).unwrap()["total"], 2);
    request.search.clear();
    request.order = "length-desc".into();
    request.offset = 50;
    let last = db.query(&request, &[]).unwrap();
    assert_eq!(last["pages"].as_array().unwrap().len(), 13);
    assert_eq!(
        last["pages"].as_array().unwrap().last().unwrap()["page_id"],
        1
    );
    request.offset = 0;
    request.order = "date-desc".into();
    let dated = db.query(&request, &[]).unwrap();
    assert_eq!(dated["pages"][0]["page_id"], 61);
    assert_eq!(dated["pages"][1]["page_id"], 60);
    request.classification = "topics".into();
    request.category = "Culture · Media.Music".into();
    assert!(db.query(&request, &[]).unwrap()["total"].as_u64().unwrap() > 0);
    request.category = "Temi non disponibili".into();
    assert_eq!(db.query(&request, &[]).unwrap()["total"], 1);
    request.category = "Nessun tema sopra soglia".into();
    assert_eq!(db.query(&request, &[]).unwrap()["total"], 1);
    request.category = "Music · STEM".into();
    assert_eq!(db.query(&request, &[]).unwrap()["total"], 0);
    request = w.request("articles");
    request.category = "PÁGINAS".into();
    assert_eq!(db.query(&request, &[]).unwrap()["total"], 1);
    let mut cards = w.request("cards");
    cards.order = "candidates-desc".into();
    let visible = db.query(&cards, &[]).unwrap();
    assert_eq!(visible["total"], 57);
    for p in visible["pages"].as_array().unwrap() {
        assert!(![2, 3, 4, 5].contains(&p["page_id"].as_i64().unwrap()));
    }
    let closed = vec![("vec".into(), 1, 101), ("vec".into(), 1, 102)];
    cards.offset = 50;
    let sorted = db.query(&cards, &closed).unwrap();
    assert_eq!(
        sorted["pages"].as_array().unwrap().last().unwrap()["page_id"],
        1
    );
    cards.offset = 0;
    cards.show_disambiguation = true;
    cards.show_redirects = true;
    cards.show_without_connections = true;
    assert_eq!(db.query(&cards, &[]).unwrap()["total"], 61);
    cards.hidden_categories = vec!["musica".into()];
    assert_eq!(db.query(&cards, &[]).unwrap()["total"], 2);
    cards.hidden_categories = vec!["musica' OR 1=1 --".into()];
    assert_eq!(db.query(&cards, &[]).unwrap()["total"], 61);
    cards.hidden_categories = vec!["x".into(); 129];
    assert!(db.query(&cards, &[]).is_err());
    let mut personalized = w.request("cards");
    personalized.order = "personalized".into();
    personalized.edits = vec![
        json!({"language":"vec","topic_labels":["Culture.Media.Music"]}),
        json!({"language":"vec","topic_labels":["STEM.STEM*"]}),
    ];
    let ranked = db.query(&personalized, &[]).unwrap();
    assert_eq!(ranked["pages"][0]["page_id"], 61);
    personalized.dataset = "all.json".into();
    let pooled = db.query(&personalized, &[]).unwrap();
    assert_eq!(pooled["pages"][0]["page_id"], 61);
    assert_eq!(pooled["pages"][0]["language"], "vec");
    assert_eq!(pooled["pages"][1]["page_id"], 61);
    assert_eq!(pooled["pages"][1]["language"], "lmo");
    let entry = &w.entries[0];
    let detail = db
        .resource(&format!(
            "{}/1.json",
            entry["connections_dir"].as_str().unwrap()
        ))
        .unwrap();
    assert_eq!(
        detail["link_candidates"]["1"]["candidates"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        detail["link_candidates"]["1"]["candidates"][0]["evidence"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        detail["link_candidates"]["1"]["counterparts"],
        json!(["lmo"])
    );
    let card = db
        .resource(&format!("{}/1.json", entry["cards_dir"].as_str().unwrap()))
        .unwrap();
    assert_eq!(
        card["candidate_record"]["candidates"][0]["evidence"][0]["source_title"],
        "Page 101"
    );
    assert_eq!(
        card["candidate_record"]["candidates"][0]["category"],
        "Musica"
    );
    assert_eq!(
        detail["link_candidates"]["1"]["candidates"][0]["category"],
        "Musica"
    );
    db.verify().unwrap();
    let shard = db
        .resource(&format!(
            "{}/0.json",
            entry["profile_topics_dir"].as_str().unwrap()
        ))
        .unwrap();
    assert_eq!(shard["rows"][0], json!([1, "missing", null]));
    assert_eq!(shard["rows"][1], json!([2, "predicted", []]));
    request = w.request("articles");
    request.dataset = "all.json".into();
    assert_eq!(db.query(&request, &[]).unwrap()["total"], 126);
    let mut csv = Vec::new();
    db.export_csv(w.entries[0]["file"].as_str().unwrap(), &mut csv)
        .unwrap();
    dumps::export_csv(
        &w.root.join(w.entries[0]["file"].as_str().unwrap()),
        &w.root.join("expected.csv"),
    )
    .unwrap();
    assert_eq!(csv, fs::read(w.root.join("expected.csv")).unwrap());
    assert!(db.resource("collection.sqlite").is_err());
    request.order = "title; DROP TABLE pages".into();
    assert!(db.query(&request, &[]).is_err());
}

#[test]
fn incomplete_or_mixed_releases_never_replace_the_active_database_and_old_readers_finish() {
    let old = Fixture::new("20261001");
    old.publish();
    let reader = Census::open(&old.db()).unwrap();
    let original = fs::read(old.db()).unwrap();
    let next = Fixture::new("20261101");
    let mut entries = next.entries.clone();
    entries.as_array_mut().unwrap().remove(0);
    write(&next.root.join("manifest.json"), &entries);
    assert!(store::publish(&next.root, &old.db(), &json!({"test":true})).is_err());
    assert_eq!(fs::read(old.db()).unwrap(), original);
    write(&next.root.join("manifest.json"), &next.entries);
    let path = next.root.join(next.entries[1]["file"].as_str().unwrap());
    let raw = fs::read_to_string(&path).unwrap();
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    fs::write(&path, raw.replace("20261101", "20261201")).unwrap();
    assert!(store::publish(&next.root, &old.db(), &json!({"test":true})).is_err());
    assert_eq!(fs::read(old.db()).unwrap(), original);
    fs::write(&path, raw).unwrap();
    fs::File::open(path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    store::publish(&next.root, &old.db(), &json!({"new":true})).unwrap();
    assert_eq!(reader.identity().unwrap(), json!({"test":true}));
    assert_eq!(
        Census::open(&old.db()).unwrap().identity().unwrap(),
        json!({"new":true})
    );
    assert_eq!(
        Census::open(&old.db())
            .unwrap()
            .manifest()
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn stale_derived_indices_and_modified_database_content_are_detected() {
    let w = Fixture::new("20261001");
    w.publish();
    Census::open(&w.db()).unwrap().verify().unwrap();
    let approved = fs::read(w.db()).unwrap();
    for (field, change) in [
        ("summary_file", "sources"),
        ("creator_index_file", "metadata"),
    ] {
        let path = w.root.join(w.entries[0][field].as_str().unwrap());
        let original = fs::read(&path).unwrap();
        let mut value: Value = serde_json::from_slice(&original).unwrap();
        if change == "sources" {
            value["sources"][0]["mtime_ns"] = json!(0);
        } else {
            value["metadata"]["language"] = json!("lmo");
        }
        write(&path, &value);
        assert!(store::publish(&w.root, &w.db(), &json!({"test":true})).is_err());
        assert_eq!(fs::read(w.db()).unwrap(), approved);
        fs::write(path, original).unwrap();
    }
    let db = rusqlite::Connection::open(w.db()).unwrap();
    db.execute(
        "UPDATE pages SET length=length+1 WHERE language='vec' AND id=2",
        [],
    )
    .unwrap();
    drop(db);
    assert!(Census::open(&w.db()).unwrap().verify().is_err());
}

#[test]
fn http_dashboard_queries_downloads_and_reviews_work_with_only_the_database() {
    struct Server(std::process::Child);
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let w = Fixture::new("20261001");
    w.publish();
    let entry = w.entries[0].clone();
    // Remove only this test's generated exports. This proves that no server
    // endpoint secretly depends on the temporary publication transports.
    for item in fs::read_dir(&w.root).unwrap() {
        let item = item.unwrap();
        if item.path() == w.db() {
            continue;
        }
        if item.file_type().unwrap().is_dir() {
            fs::remove_dir_all(item.path()).unwrap();
        } else {
            fs::remove_file(item.path()).unwrap();
        }
    }
    fs::create_dir(w.root.join("web")).unwrap();
    fs::write(w.root.join("web/index.html"), "test dashboard").unwrap();
    let mut server = Server(
        Command::new(env!("CARGO_BIN_EXE_orphanwiki"))
            .args([
                "serve",
                "--web-root",
                "web",
                "--collection-database",
                "collection.sqlite",
                "--port",
                "0",
            ])
            .env_clear()
            .current_dir(&w.root)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut line = String::new();
    std::io::BufReader::new(server.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let url = line.trim().strip_prefix("Dashboard: ").unwrap();
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap();
    let manifest: Value = serde_json::from_slice(
        &client
            .get(format!("{url}/data/manifest.json"))
            .send()
            .unwrap()
            .bytes()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(manifest[0]["storage"], store::RULE);
    let response = client
        .post(format!("{url}/api/pages"))
        .header("content-type", "application/json")
        .body(json!({"dataset":entry["file"],"kind":"cards"}).to_string())
        .send()
        .unwrap();
    assert_eq!(response.status(), 200);
    let result: Value = serde_json::from_slice(&response.bytes().unwrap()).unwrap();
    assert_eq!(result["pages"].as_array().unwrap().len(), 25);
    let response = client
        .get(format!(
            "{url}/api/export?dataset={}",
            entry["file"].as_str().unwrap()
        ))
        .send()
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.text().unwrap().starts_with("page_id,title,"));
    let review = json!({"dataset":entry["file"],"language":"vec","source_id":101,"target_id":1,"status":"fixed","version":0});
    assert_eq!(
        client
            .post(format!("{url}/api/reviews"))
            .header("content-type", "application/json")
            .body(review.to_string())
            .send()
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .get(format!("{url}/data/collection.sqlite"))
            .send()
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        client
            .get(format!("{url}/api/pages"))
            .send()
            .unwrap()
            .status(),
        405
    );
}

#[test]
fn missing_required_resources_and_comparison_rows_are_errors() {
    let w = Fixture::new("20261001");
    w.publish();
    let approved = fs::read(w.db()).unwrap();
    for field in [
        "summary_file",
        "creator_index_file",
        "connections_metadata_file",
    ] {
        let mut entries = w.entries.clone();
        entries[0].as_object_mut().unwrap().remove(field);
        write(&w.root.join("manifest.json"), &entries);
        assert!(
            store::publish(&w.root, &w.db(), &json!({"test":true})).is_err(),
            "{field}"
        );
        assert_eq!(fs::read(w.db()).unwrap(), approved);
    }
    write(&w.root.join("manifest.json"), &w.entries);
    let db = rusqlite::Connection::open(w.db()).unwrap();
    db.execute("DELETE FROM targets WHERE language='vec' AND id=1", [])
        .unwrap();
    drop(db);
    let db = Census::open(&w.db()).unwrap();
    let manifest = db.manifest().unwrap();
    let entry = manifest
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["language"] == "vec")
        .unwrap();
    assert!(db
        .resource(&format!(
            "{}/1.json",
            entry["connections_dir"].as_str().unwrap()
        ))
        .is_err());
    assert!(db
        .resource(&format!(
            "{}/999999.json",
            entry["profile_topics_dir"].as_str().unwrap()
        ))
        .is_err());
}
