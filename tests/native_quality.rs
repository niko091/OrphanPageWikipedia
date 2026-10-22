use _native::dumps::QualityExtractor;
use serde_json::Value;
use std::collections::HashMap;

#[test]
fn frozen_reference_regex_semantics_keep_nested_empty_and_unexpanded_markup() {
    let spec: Value = serde_json::from_str(include_str!("../config/quality.json")).unwrap();
    let extractor = QualityExtractor::new("vec", &spec, &HashMap::new()).unwrap();
    for (text, counts) in [
        ("", [0, 0, 0, 0, 0]),
        ("[[]]x]]", [0, 0, 2, 0, 0]),
        ("[[]][[]]", [0, 0, 3, 0, 0]),
        ("[[[[[[]]]]]]", [0, 0, 3, 0, 0]),
        ("[[A[[B]]C]]", [0, 0, 1, 0, 0]),
        ("==A== ===B=== ====C====", [0, 2, 0, 0, 0]),
        ("===\nA\n===", [0, 0, 0, 0, 0]),
        (
            "<REF\x1cname=\"x\"/> <ref/> <ref>body<ref/>a</ref>",
            [4, 0, 0, 0, 0],
        ),
        (
            "<!--<ref>x</ref>--><nowiki>[[Foo]]</nowiki>",
            [1, 0, 1, 0, 0],
        ),
        (
            "[[Category:C]][[category:C]][[:Category:C]][[ File:X]][[File:X]]",
            [0, 0, 3, 1, 1],
        ),
    ] {
        let actual = extractor.features(text).unwrap();
        assert_eq!(actual[0], text.len() as u64);
        assert_eq!(actual[1..], counts, "{text:?}");
    }
    let text="è<ref>a</ref><ref name=\"a\"/>\n==A==\n===B===\n====C====\n[[Foo]][[Category:C]][[Imàjine:X]]";
    assert_eq!(
        extractor.features(text).unwrap(),
        [text.len() as u64, 2, 2, 1, 1, 1]
    );
}

#[test]
fn same_xml_namespaces_extend_frozen_aliases_without_case_or_prefix_inference() {
    let mut spec: Value = serde_json::from_str(include_str!("../config/quality.json")).unwrap();
    let extractor = QualityExtractor::new(
        "vec",
        &spec,
        &HashMap::from([
            (6, "PictureLocal".into()),
            (14, "TopicLocal".into()),
            (-2, "MediaLocal".into()),
        ]),
    )
    .unwrap();
    assert_eq!(
        extractor
            .features(
                "[[PictureLocal:X]][[TopicLocal:Y]][[MediaLocal:Z]][[Imàjine:A]][[topiclocal:B]]"
            )
            .unwrap()[3..],
        [1, 3, 1]
    );
    spec["weights"][0] = serde_json::json!(0.5);
    assert!(QualityExtractor::new("vec", &spec, &HashMap::new()).is_err());
}

#[cfg(feature = "cli")]
mod content {
    use super::*;
    use _native::dumps::{infer_quality, SQL_TABLES};
    use rusqlite::Connection;
    use serde_json::json;
    use sha2::Digest;
    use std::{
        fs,
        io::Write,
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
    };

    struct Workspace(PathBuf);
    impl Workspace {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "orphanwiki-quality-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(path.join("config")).unwrap();
            fs::write(
                path.join("config/quality.json"),
                include_bytes!("../config/quality.json"),
            )
            .unwrap();
            fs::write(
                path.join("config/languages.json"),
                r#"{"version":1,"languages":["vec","lmo"]}"#,
            )
            .unwrap();
            let files = SQL_TABLES
                .into_iter()
                .map(|name| (name.to_owned(), json!({"sha256_local":name})))
                .collect::<serde_json::Map<_, _>>();
            let sql_manifest = path.join("sql.json");
            fs::write(&sql_manifest, json!({"files":files}).to_string()).unwrap();
            let metadata = json!({"language":"vec","database":"vecwiki","source":"Wikimedia SQL dumps / vecwiki","complete":true,"sql_release":"20261001",
                "dump_manifest":sql_manifest,"dump_manifest_sha256":format!("{:x}",sha2::Sha256::digest(fs::read(&sql_manifest).unwrap())),"finished_at":"2026-10-02T01:02:03+00:00"});
            let raw = format!(
                "{{\"metadata\":{},\"pages\":{}}}",
                metadata,
                json!((1..=1101)
                    .map(|id| json!({"page_id":id,"title":format!("P{id}"),"revision_id":id}))
                    .collect::<Vec<_>>())
            );
            fs::write(path.join("vec-raw.json"), raw).unwrap();
            let db = Connection::open(path.join("vecwiki-20261001.sqlite")).unwrap();
            db.execute_batch("CREATE TABLE dump_page(id INTEGER,ns INTEGER,redirect INTEGER,revision INTEGER);CREATE TABLE imported(table_name TEXT PRIMARY KEY,sha256 TEXT)").unwrap();
            for name in SQL_TABLES {
                db.execute("INSERT INTO imported VALUES(?,?)", [name, name])
                    .unwrap();
            }
            let mut query = db.prepare("INSERT INTO dump_page VALUES(?,0,0,?)").unwrap();
            for id in 1..=1101 {
                query.execute([id, id]).unwrap();
            }
            Self(path)
        }
        fn db(&self) -> Connection {
            Connection::open(self.0.join("vecwiki-20261001-quality.sqlite")).unwrap()
        }
        fn archive(&self, name: &str, body: &str) -> Value {
            let mut encoder =
                bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
            encoder.write_all(body.as_bytes()).unwrap();
            let bytes = encoder.finish().unwrap();
            let path = self.0.join(name);
            fs::write(&path, &bytes).unwrap();
            let digest = format!("{:x}", sha2::Sha256::digest(&bytes));
            json!({"path":path,"bytes":bytes.len(),"sha256_local":digest,"sha256_publisher":digest})
        }
        fn manifest(&self, files: Value) {
            fs::write(self.0.join("content.json"),json!({"language":"vec","database":"vecwiki","source":"mediawiki_content_current","complete":true,"content_release":"2026-10-01","files":files}).to_string()).unwrap();
        }
        fn import(&self) -> Result<Value, String> {
            infer_quality(
                &self.0.join("vec-raw.json"),
                &self.0.join("content.json"),
                &self.0.join("vec-quality.json"),
                &self.0,
                &self.0.join("config/quality.json"),
            )
            .map_err(|e| e.to_string())?;
            Ok(
                serde_json::from_slice(&fs::read(self.0.join("vec-quality.json")).unwrap())
                    .unwrap(),
            )
        }
        fn fixtures(&self) {
            let header="<mediawiki xmlns=\"http://www.mediawiki.org/xml/export-0.11/\"><siteinfo><dbname>vecwiki</dbname><namespaces><namespace key=\"6\">PictureLocal</namespace><namespace key=\"14\">TopicLocal</namespace></namespaces></siteinfo>";
            let first=format!("{header}{}{}{}</mediawiki>",page(1,1,"<model>wikitext</model><text>è&amp;字&lt;ref&gt;x&lt;/ref&gt; [[PictureLocal:X]][[TopicLocal:Y]]</text>"),page(2,22,"<text/>"),page(3,33,"<text deleted=\"deleted\"/>"));
            let mut second = header.to_owned();
            second.push_str(&page(4,44,"<slots><slot role=\"main\"><model>wikitext</model><text><![CDATA[==A==\r\n[[Foo]]]]></text></slot></slots>"));
            second.push_str(&page(5, 55, "<model>json</model><text>unavailable</text>"));
            second.push_str(&page(6, 66, "<model>wikitext</model>"));
            for id in 8..=1101 {
                second.push_str(&page(
                    id,
                    id,
                    &format!("<text>{}[[Foo]]</text>", "text ".repeat(id as usize % 110)),
                ));
            }
            second.push_str(&page(99999, 1, "<text>outside census</text>"));
            second.push_str("</mediawiki>");
            self.manifest(json!({"a.xml.bz2":self.archive("a.xml.bz2",&first),"b.xml.bz2":self.archive("b.xml.bz2",&second)}));
        }
    }
    impl Drop for Workspace {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn page(id: i64, revision: i64, body: &str) -> String {
        format!(
            "<page><id>{id}</id><ns>0</ns><revision><id>{revision}</id>{body}</revision></page>"
        )
    }

    #[test]
    fn complete_quality_census_keeps_revision_relations_empty_text_and_sqlite_only_features() {
        let workspace = Workspace::new();
        workspace.fixtures();
        let result = workspace.import().unwrap();
        assert_eq!(result["pages"].as_array().unwrap().len(), 1101);
        let pages = &result["pages"];
        assert!(pages[0]["quality_score"].as_f64().unwrap() > 0.0);
        assert_eq!(pages[1]["quality_score"], 0.0);
        assert_eq!(pages[1]["quality_revision_relation"], "different");
        assert_eq!(pages[2]["quality_status"], "missing_wikitext");
        assert!(pages[2]["quality_score"].is_null());
        assert_eq!(pages[3]["quality_status"], "scored");
        assert_eq!(pages[4]["quality_status"], "missing_wikitext");
        assert_eq!(pages[5]["quality_status"], "missing_wikitext");
        assert_eq!(pages[6]["quality_status"], "missing_current_page");
        assert_eq!(pages[6]["quality_revision_relation"], "unavailable");
        assert_eq!(result["metadata"]["same_revision_pages"], 1095);
        assert_eq!(result["metadata"]["different_revision_pages"], 5);
        assert_eq!(result["metadata"]["scored_pages"], 1097);
        // The frozen minimum wins ties and keeps its JSON number type;
        // changing 100 to 100.0 would change every downstream quality digest.
        for (index, minimum) in [(0, 100), (4, 2), (5, 5)] {
            assert_eq!(
                result["metadata"]["normalizers"][index].as_i64(),
                Some(minimum)
            );
        }

        let db = workspace.db();
        let first: String = db
            .query_row("SELECT features FROM quality_pages WHERE id=1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&first).unwrap(),
            json!([53, 1, 0, 0, 1, 1])
        );
        let slot: String = db
            .query_row("SELECT features FROM quality_pages WHERE id=4", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&slot).unwrap(),
            json!([13, 0, 1, 1, 0, 0])
        );
        let count: u64 = db
            .query_row("SELECT COUNT(*) FROM quality_pages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1100);
        db.execute_batch("CREATE TRIGGER reject_parse BEFORE INSERT ON quality_pages BEGIN SELECT RAISE(ABORT,'unexpected parse'); END;").unwrap();
        assert_eq!(workspace.import().unwrap(), result);
    }

    #[test]
    fn partition_publication_failure_reuses_completed_parts_and_does_not_duplicate_rows() {
        let workspace = Workspace::new();
        workspace.fixtures();
        let result = workspace.import().unwrap();
        let approved = fs::read(workspace.0.join("vec-quality.json")).unwrap();
        workspace
            .db()
            .execute_batch(
                "DELETE FROM quality_parts WHERE name='b.xml.bz2';
            CREATE TRIGGER reject_part BEFORE INSERT ON quality_parts WHEN NEW.name='b.xml.bz2'
            BEGIN SELECT RAISE(ABORT,'partition checkpoint failed'); END;",
            )
            .unwrap();
        assert!(workspace
            .import()
            .unwrap_err()
            .contains("partition checkpoint failed"));
        assert_eq!(
            fs::read(workspace.0.join("vec-quality.json")).unwrap(),
            approved
        );
        assert_eq!(
            workspace
                .db()
                .query_row("SELECT COUNT(*) FROM quality_parts", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        workspace.db().execute_batch("DROP TRIGGER reject_part;CREATE TRIGGER reject_completed BEFORE INSERT ON quality_pages WHEN NEW.file='a.xml.bz2' BEGIN SELECT RAISE(ABORT,'completed part reparsed'); END;").unwrap();
        assert_eq!(workspace.import().unwrap(), result);
        assert_eq!(
            workspace
                .db()
                .query_row("SELECT COUNT(*) FROM quality_parts", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
    }

    #[test]
    fn invalid_xml_and_wrong_wiki_cannot_replace_an_approved_export() {
        for body in ["<mediawiki><siteinfo><dbname>lmowiki</dbname></siteinfo></mediawiki>".to_owned(),
            "<mediawiki><siteinfo><dbname>vecwiki</dbname></siteinfo><page>".into(),
            format!("<mediawiki><siteinfo><dbname>vecwiki</dbname></siteinfo>{}</mediawiki>",page(1,1,"<text>&#1;</text>")),
            "<mediawiki><siteinfo><dbname>vecwiki</dbname></siteinfo><page><id>1</id><ns>0</ns><revision><id>1</id></revision><revision><id>2</id></revision></page></mediawiki>".into(),
            "<mediawiki><page><id>1</id><ns>0</ns><revision><id>1</id></revision></page></mediawiki>".into(),
            "<mediawiki><siteinfo><dbname>vecwiki</dbname></siteinfo><!--bad--comment--></mediawiki>".into(),
            "<mediawiki key=\"a\" key=\"b\"><siteinfo><dbname>vecwiki</dbname></siteinfo></mediawiki>".into(),
            "<mediawiki xmlns:a=\"urn:x\" xmlns:b=\"urn:x\" a:key=\"a\" b:key=\"b\"><siteinfo><dbname>vecwiki</dbname></siteinfo></mediawiki>".into(),
            "<mediawiki bad:key=\"a\"><siteinfo><dbname>vecwiki</dbname></siteinfo></mediawiki>".into()] {
            let workspace=Workspace::new();workspace.fixtures();workspace.import().unwrap();
            let approved=fs::read(workspace.0.join("vec-quality.json")).unwrap();
            workspace.manifest(json!({"bad.xml.bz2":workspace.archive("bad.xml.bz2",&body)}));
            assert!(workspace.import().is_err(),"{body}");
            assert_eq!(fs::read(workspace.0.join("vec-quality.json")).unwrap(),approved);
        }
    }

    #[test]
    fn cli_quality_selects_from_pool_and_needs_no_python_runtime() {
        let workspace = Workspace::new();
        workspace.fixtures();
        let expected = workspace.import().unwrap();
        fs::create_dir_all(workspace.0.join("dumps/vecwiki")).unwrap();
        fs::copy(
            workspace.0.join("content.json"),
            workspace.0.join("dumps/vecwiki/content-2026-10-01.json"),
        )
        .unwrap();
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_orphanwiki"))
            .arg("--repo-root")
            .arg(&workspace.0)
            .args([
                "quality",
                "--content-release",
                "2026-10-01",
                "--root",
                "dumps",
                "--input-dir",
                ".",
                "--output-dir",
                "cli",
                "--work-dir",
                ".",
                "--languages",
                "vec",
            ])
            .env_clear()
            .current_dir(std::env::temp_dir())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let actual: Value =
            serde_json::from_slice(&fs::read(workspace.0.join("cli/vec-quality.json")).unwrap())
                .unwrap();
        assert_eq!(actual, expected);
        assert!(!workspace.0.join("cli/lmo-quality.json").exists());
    }

    #[test]
    fn changed_inputs_unverified_sql_and_output_collisions_preserve_approved_files() {
        let workspace = Workspace::new();
        workspace.fixtures();
        workspace.import().unwrap();
        let approved = fs::read(workspace.0.join("vec-quality.json")).unwrap();
        for relative in [
            "vec-raw.json",
            "content.json",
            "sql.json",
            "a.xml.bz2",
            "config/quality.json",
            "vecwiki-20261001.sqlite",
            "vecwiki-20261001-quality.sqlite",
        ] {
            let path = workspace.0.join(relative);
            let before = fs::read(&path).unwrap();
            assert!(infer_quality(
                &workspace.0.join("vec-raw.json"),
                &workspace.0.join("content.json"),
                &path,
                &workspace.0,
                &workspace.0.join("config/quality.json")
            )
            .is_err());
            assert_eq!(fs::read(path).unwrap(), before);
        }
        let db = Connection::open(workspace.0.join("vecwiki-20261001.sqlite")).unwrap();
        db.execute(
            "UPDATE imported SET sha256='changed' WHERE table_name='user_groups'",
            [],
        )
        .unwrap();
        assert!(workspace
            .import()
            .unwrap_err()
            .contains("Staging SQL incompleto"));
        db.execute(
            "UPDATE imported SET sha256='user_groups' WHERE table_name='user_groups'",
            [],
        )
        .unwrap();
        let path = workspace.0.join("a.xml.bz2");
        let mut bytes = fs::read(&path).unwrap();
        bytes.push(0);
        fs::write(path, bytes).unwrap();
        assert!(workspace
            .import()
            .unwrap_err()
            .contains("Checksum contenuto XML errato"));
        assert_eq!(
            fs::read(workspace.0.join("vec-quality.json")).unwrap(),
            approved
        );
    }

    #[test]
    fn empty_xml_census_missing_scores_and_prefixed_namespace_identity_are_exact() {
        let workspace = Workspace::new();
        let xml="<m:mediawiki xmlns:m=\"urn:synthetic\"><m:siteinfo><m:base>https://vec.wikipedia.org/wiki/Main_Page</m:base></m:siteinfo></m:mediawiki>";
        workspace.manifest(json!({"all.xml.bz2":workspace.archive("all.xml.bz2",xml)}));
        let result = workspace.import().unwrap();
        assert_eq!(result["metadata"]["scored_pages"], 0);
        assert_eq!(result["metadata"]["missing_pages"], 1101);
        assert_eq!(
            result["metadata"]["normalizers"],
            json!([100, 0.15, 0.1, 0.1, 2, 5])
        );
        assert!(
            result["pages"]
                .as_array()
                .unwrap()
                .iter()
                .all(|p| p["quality_status"] == "missing_current_page"
                    && p["quality_score"].is_null())
        );
        let xml="<mediawiki><siteinfo><dbname>vecwiki</dbname></siteinfo><page><id>1</id><ns>0</ns><revision><id>1</id><text/></revision></page></mediawiki>";
        workspace.manifest(json!({"all.xml.bz2":workspace.archive("all.xml.bz2",xml)}));
        let result = workspace.import().unwrap();
        assert_eq!(result["metadata"]["scored_pages"], 1);
        assert_eq!(result["pages"][0]["quality_score"], 0.0);
        let xml="<mediawiki><siteinfo><dbname>vecwiki</dbname><namespaces><namespace key=\"06\">WrongPrefix</namespace></namespaces></siteinfo><page><id>1</id><ns>0</ns><revision><id>1</id><text>[[WrongPrefix:X]]</text></revision></page></mediawiki>";
        workspace.manifest(json!({"all.xml.bz2":workspace.archive("all.xml.bz2",xml)}));
        workspace.import().unwrap();
        let features: String = workspace
            .db()
            .query_row("SELECT features FROM quality_pages WHERE id=1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            serde_json::from_str::<[u64; 6]>(&features).unwrap()[3..],
            [1, 0, 0]
        );
    }
}
