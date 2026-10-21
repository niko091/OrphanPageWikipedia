#![cfg(feature = "cli")]

use _native::dumps::{DumpClient, DumpHttp, DumpResponse, DUMP_BASE, SQL_TABLES};
use serde_json::{json, Value};
use sha2::Digest;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{self, Cursor, Read, Write};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Workspace(PathBuf);
impl Workspace {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "orphanwiki-acquisition-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[derive(Clone)]
struct Reply {
    url: String,
    offset: u64,
    head: bool,
    status: u16,
    headers: HashMap<String, String>,
    bytes: Vec<u8>,
    interrupted: bool,
}
impl Reply {
    fn get(url: &str, bytes: &[u8]) -> Self {
        Self {
            url: url.into(),
            offset: 0,
            head: false,
            status: 200,
            headers: HashMap::new(),
            bytes: bytes.to_vec(),
            interrupted: false,
        }
    }
    fn head(url: &str, size: usize) -> Self {
        Self {
            head: true,
            headers: [("content-length".into(), size.to_string())]
                .into_iter()
                .collect(),
            ..Self::get(url, b"")
        }
    }
    fn range(url: &str, offset: u64, total: usize, bytes: &[u8]) -> Self {
        Self {
            offset,
            status: 206,
            headers: [(
                "content-range".into(),
                format!("bytes {offset}-{}/{total}", total - 1),
            )]
            .into_iter()
            .collect(),
            ..Self::get(url, bytes)
        }
    }
}

struct BrokenReader(Cursor<Vec<u8>>);
impl Read for BrokenReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if self.0.position() as usize == self.0.get_ref().len() {
            Err(io::Error::other("fixture interrupted"))
        } else {
            self.0.read(bytes)
        }
    }
}

struct Fixture(Rc<RefCell<VecDeque<Reply>>>);
impl Fixture {
    fn new(replies: Vec<Reply>) -> (Self, Rc<RefCell<VecDeque<Reply>>>) {
        let queue = Rc::new(RefCell::new(replies.into()));
        (Self(queue.clone()), queue)
    }
}
impl DumpHttp for Fixture {
    fn open(&mut self, url: &str, offset: u64, head: bool) -> Result<DumpResponse, String> {
        let reply = self
            .0
            .borrow_mut()
            .pop_front()
            .expect("unplanned HTTP request");
        assert_eq!(
            (url, offset, head),
            (reply.url.as_str(), reply.offset, reply.head)
        );
        let body: Box<dyn Read> = if reply.interrupted {
            Box::new(BrokenReader(Cursor::new(reply.bytes)))
        } else {
            Box::new(Cursor::new(reply.bytes))
        };
        Ok(DumpResponse {
            status: reply.status,
            headers: reply.headers,
            body,
        })
    }
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", sha2::Sha256::digest(bytes))
}
fn sha1(bytes: &[u8]) -> String {
    format!("{:x}", sha1::Sha1::digest(bytes))
}
fn compress(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::best());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

fn metadata(history: &[u8]) -> Vec<Reply> {
    let mut files = serde_json::Map::new();
    for table in SQL_TABLES {
        let name = format!("vecwiki-20261001-{table}.sql.gz");
        files.insert(name.clone(),json!({"url":format!("/vecwiki/20261001/{name}"),"size":table.len(),"sha1":sha1(table.as_bytes())}));
    }
    let status =
        json!({"jobs":{"tables":{"status":"done","updated":"2026-10-02 01:02:03","files":files}}});
    vec![
        Reply::get(&format!("{DUMP_BASE}/vecwiki/20261001/dumpstatus.json"),&serde_json::to_vec(&status).unwrap()),
        Reply::get(&format!("{DUMP_BASE}/other/mediawiki_history/2026-09/vecwiki/"), b"<html><a href=\"../\">Parent</a><A title='x > y' HREF='2026-09.vecwiki.all-time.tsv.bz2'>History</A><a href=\"2026-09.vecwiki.all-time.tsv.bz2\">duplicate</a></html>"),
        Reply::head(&format!("{DUMP_BASE}/other/mediawiki_history/2026-09/vecwiki/2026-09.vecwiki.all-time.tsv.bz2"),history.len()),
    ]
}

fn downloads(history: &[u8]) -> Vec<Reply> {
    let mut replies = SQL_TABLES
        .iter()
        .map(|table| {
            Reply::get(
                &format!("{DUMP_BASE}/vecwiki/20261001/vecwiki-20261001-{table}.sql.gz"),
                table.as_bytes(),
            )
        })
        .collect::<Vec<_>>();
    replies.push(Reply::get(
        &format!(
            "{DUMP_BASE}/other/mediawiki_history/2026-09/vecwiki/2026-09.vecwiki.all-time.tsv.bz2"
        ),
        history,
    ));
    replies
}

#[test]
fn plans_validate_all_sources_without_creating_files() {
    let workspace = Workspace::new();
    let root = workspace.0.join("absent");
    let history = compress(b"complete history");
    let (http, queue) = Fixture::new(metadata(&history));
    let result = DumpClient::new(http)
        .fetch_dumps(
            &[("vec".into(), "vecwiki".into())],
            "20261001",
            "2026-09",
            &root,
            true,
        )
        .unwrap();
    assert_eq!(
        result,
        [
            json!({"language":"vec","database":"vecwiki","sql_release":"20261001","history_release":"2026-09","compressed_bytes":SQL_TABLES.iter().map(|table| table.len()).sum::<usize>()+history.len(),"history_partitions":1})
        ]
    );
    assert!(queue.borrow().is_empty());
    assert!(!root.exists());
    let name = "vecwiki-2026-10-01-p0.xml.bz2";
    let base = format!("{DUMP_BASE}/other/mediawiki_content_current/vecwiki/2026-10-01/xml/bzip2/");
    let publisher = format!("{}  ./{name}\n", sha256(b"XML"));
    let (http, queue) = Fixture::new(vec![
        Reply::get(&format!("{base}SHA256SUMS"), publisher.as_bytes()),
        Reply::head(&format!("{base}{name}"), 3),
    ]);
    let result = DumpClient::new(http)
        .fetch_content(
            &[("vec".into(), "vecwiki".into())],
            "2026-10-01",
            &root,
            true,
        )
        .unwrap();
    assert_eq!(result[0]["publisher_sha256"], sha256(publisher.as_bytes()));
    assert_eq!(result[0]["files"][name]["bytes"], 3);
    assert!(queue.borrow().is_empty());
    assert!(!root.exists());
}

#[test]
fn verified_manifests_match_python_fields_and_reuse_existing_approved_bytes() {
    let workspace = Workspace::new();
    let history = [compress(b"first"), compress(b"second")].concat();
    let (http, queue) = Fixture::new([metadata(&history), downloads(&history)].concat());
    let editions = [("vec".into(), "vecwiki".into())];
    let result = DumpClient::new(http)
        .fetch_dumps(&editions, "20261001", "2026-09", &workspace.0, false)
        .unwrap();
    let mut expected = json!({"language":"vec","database":"vecwiki","sql_release":"20261001","history_release":"2026-09","sql_status_url":format!("{DUMP_BASE}/vecwiki/20261001/dumpstatus.json"),"history_index_url":format!("{DUMP_BASE}/other/mediawiki_history/2026-09/vecwiki/"),"files":{}});
    for table in SQL_TABLES {
        expected["files"][table] = json!({"path":workspace.0.join(format!("vecwiki/20261001/vecwiki-20261001-{table}.sql.gz")),"bytes":table.len(),"sha1_publisher":sha1(table.as_bytes()),"sha256_local":sha256(table.as_bytes()),"job_updated":"2026-10-02 01:02:03"});
    }
    let name = "2026-09.vecwiki.all-time.tsv.bz2";
    expected["files"][name] = json!({"path":workspace.0.join("vecwiki/2026-09").join(name),"bytes":history.len(),"sha256_local":sha256(&history),"checksum_scope":"local_only"});
    assert_eq!(result, [expected.clone()]);
    assert!(queue.borrow().is_empty());
    let manifest = workspace.0.join("vecwiki/manifest-20261001-2026-09.json");
    let original = format!("  {}\n\n", serde_json::to_string(&expected).unwrap());
    fs::write(&manifest, &original).unwrap();
    let (http, queue) = Fixture::new(metadata(&history));
    let repeated = DumpClient::new(http)
        .fetch_dumps(&editions, "20261001", "2026-09", &workspace.0, false)
        .unwrap();
    assert_eq!(repeated, result);
    assert_eq!(fs::read_to_string(manifest).unwrap(), original);
    assert!(queue.borrow().is_empty());
}

#[test]
fn interrupted_download_resumes_only_the_verified_range_and_clears_sidecars() {
    let workspace = Workspace::new();
    let path = workspace.0.join("file.bz2");
    let url = format!("{DUMP_BASE}/fixture/file.bz2");
    let bytes = b"abcdefghi";
    let (http, queue) = Fixture::new(vec![
        Reply {
            interrupted: true,
            ..Reply::get(&url, &bytes[..3])
        },
        Reply::range(&url, 3, bytes.len(), &bytes[3..]),
    ]);
    let mut client = DumpClient::new(http);
    assert!(client
        .fetch(&url, &path, bytes.len() as u64, None, Some(&sha256(bytes)))
        .is_err());
    assert!(!path.exists());
    assert_eq!(
        fs::read(path.with_file_name("file.bz2.part")).unwrap(),
        &bytes[..3]
    );
    assert_eq!(
        client
            .fetch(&url, &path, bytes.len() as u64, None, Some(&sha256(bytes)))
            .unwrap(),
        sha256(bytes)
    );
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert!(!path.with_file_name("file.bz2.part").exists());
    assert!(!path.with_file_name("file.bz2.part-source.json").exists());
    assert!(queue.borrow().is_empty());
}

#[test]
fn ignored_mismatched_stale_and_unsatisfiable_ranges_cannot_duplicate_bytes() {
    for mode in ["ignored", "mismatch", "stale", "416"] {
        let workspace = Workspace::new();
        let path = workspace.0.join("file.gz");
        let url = format!("{DUMP_BASE}/fixture/file.gz");
        let bytes = b"abcdef";
        let partial = path.with_file_name("file.gz.part");
        let source = path.with_file_name("file.gz.part-source.json");
        fs::write(&partial, b"abc").unwrap();
        fs::write(&source,json!({"url":if mode=="stale" {"old"} else {&url},"bytes":bytes.len(),"sha1":sha1(bytes)}).to_string()).unwrap();
        let replies = match mode {
            "ignored" => vec![Reply {
                offset: 3,
                ..Reply::get(&url, bytes)
            }],
            "mismatch" => vec![Reply::range(&url, 3, bytes.len() + 1, &bytes[3..])],
            "stale" => vec![Reply::get(&url, bytes)],
            _ => vec![
                Reply {
                    offset: 3,
                    status: 416,
                    ..Reply::get(&url, b"")
                },
                Reply::get(&url, bytes),
            ],
        };
        let (http, queue) = Fixture::new(replies);
        let result =
            DumpClient::new(http).fetch(&url, &path, bytes.len() as u64, Some(&sha1(bytes)), None);
        if mode == "mismatch" {
            assert!(result.unwrap_err().contains("Range"));
            assert!(!path.exists());
            assert_eq!(fs::read(partial).unwrap(), b"abc");
        } else {
            assert_eq!(result.unwrap(), sha256(bytes));
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
        assert!(queue.borrow().is_empty());
    }
}

#[test]
fn corrupted_checksums_and_history_never_replace_the_previous_manifest() {
    let workspace = Workspace::new();
    let path = workspace.0.join("bad.gz");
    let url = format!("{DUMP_BASE}/fixture/bad.gz");
    let (http, queue) = Fixture::new(vec![Reply::get(&url, b"wrong")]);
    assert!(DumpClient::new(http)
        .fetch(&url, &path, 5, Some(&sha1(b"right")), None)
        .unwrap_err()
        .contains("Checksum"));
    assert!(!path.exists());
    assert!(!path.with_file_name("bad.gz.part").exists());
    assert!(queue.borrow().is_empty());
    let history = b"not a bzip2 archive";
    let manifest = workspace.0.join("vecwiki/manifest-20261001-2026-09.json");
    fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    fs::write(&manifest, b"previous approved manifest").unwrap();
    let mut replies = metadata(history);
    replies.extend(downloads(history));
    replies.push(Reply::get(
        &format!(
            "{DUMP_BASE}/other/mediawiki_history/2026-09/vecwiki/2026-09.vecwiki.all-time.tsv.bz2"
        ),
        history,
    ));
    let (http, queue) = Fixture::new(replies);
    assert!(DumpClient::new(http)
        .fetch_dumps(
            &[("vec".into(), "vecwiki".into())],
            "20261001",
            "2026-09",
            &workspace.0,
            false
        )
        .is_err());
    assert_eq!(fs::read(&manifest).unwrap(), b"previous approved manifest");
    assert!(queue.borrow().is_empty());
}

#[test]
fn content_downloads_verify_publisher_sha256_before_publishing_all_partitions() {
    let workspace = Workspace::new();
    let base = format!("{DUMP_BASE}/other/mediawiki_content_current/vecwiki/2026-10-01/xml/bzip2/");
    let names = [
        "vecwiki-2026-10-01-p0.xml.bz2",
        "vecwiki-2026-10-01-p1.xml.bz2",
    ];
    let bodies = [compress(b"xml zero"), compress(b"xml one")];
    let publisher = format!(
        "{}  {}\n{} *./{}\n",
        sha256(&bodies[0]),
        names[0],
        sha256(&bodies[1]),
        names[1]
    );
    let mut replies = vec![Reply::get(
        &format!("{base}SHA256SUMS"),
        publisher.as_bytes(),
    )];
    for index in 0..2 {
        replies.push(Reply::head(
            &format!("{base}{}", names[index]),
            bodies[index].len(),
        ));
    }
    for index in 0..2 {
        replies.push(Reply::get(
            &format!("{base}{}", names[index]),
            &bodies[index],
        ));
    }
    let (http, queue) = Fixture::new(replies);
    let result = DumpClient::new(http)
        .fetch_content(
            &[("vec".into(), "vecwiki".into())],
            "2026-10-01",
            &workspace.0,
            false,
        )
        .unwrap();
    let manifest = workspace.0.join("vecwiki/content-2026-10-01.json");
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(manifest).unwrap()).unwrap(),
        result[0]
    );
    assert_eq!(result[0]["publisher_sha256"], sha256(publisher.as_bytes()));
    for index in 0..2 {
        assert_eq!(
            result[0]["files"][names[index]]["sha256_publisher"],
            sha256(&bodies[index])
        );
        assert_eq!(
            fs::read(
                workspace
                    .0
                    .join("vecwiki/content/2026-10-01")
                    .join(names[index])
            )
            .unwrap(),
            bodies[index]
        );
    }
    assert!(queue.borrow().is_empty());
}

#[test]
fn missing_overlapping_and_incomplete_release_inputs_stop_before_download() {
    let workspace = Workspace::new();
    let (http, queue) = Fixture::new(vec![]);
    let mut client = DumpClient::new(http);
    let editions = [("vec".into(), "vecwiki".into())];
    assert!(client
        .fetch_dumps(&editions, "latest", "2026-09", &workspace.0, true)
        .is_err());
    assert!(client
        .fetch_dumps(&editions, "20261001", "2026-13", &workspace.0, true)
        .is_err());
    assert!(client
        .fetch_content(&editions, "2026-02-30", &workspace.0, true)
        .is_err());
    assert!(client
        .fetch_content(&[], "2026-10-01", &workspace.0, true)
        .is_err());
    assert!(client
        .fetch(
            "https://example.org/file",
            &workspace.0.join("bad"),
            1,
            None,
            None
        )
        .is_err());
    assert!(queue.borrow().is_empty());
    assert_eq!(fs::read_dir(&workspace.0).unwrap().count(), 0);
    for periods in [
        vec![],
        vec!["all-time", "2026"],
        vec!["2026", "2026-01"],
        vec!["2025-12", "2026-02"],
        vec!["2026-13"],
    ] {
        let names = periods
            .iter()
            .map(|period| format!("2026-09.vecwiki.{period}.tsv.bz2"))
            .collect::<Vec<_>>();
        assert!(DumpClient::<Fixture>::history_partitions(&names, "2026-09", "vecwiki").is_err());
    }
    let names = [
        "2026-09.vecwiki.2026-01.tsv.bz2".into(),
        "2026-09.vecwiki.2025-12.tsv.bz2".into(),
    ];
    assert_eq!(
        DumpClient::<Fixture>::history_partitions(&names, "2026-09", "vecwiki").unwrap(),
        [names[1].clone(), names[0].clone()]
    );
    let (http, queue) = Fixture::new(vec![Reply::get(
        &format!("{DUMP_BASE}/vecwiki/20261001/dumpstatus.json"),
        b"{\"jobs\":{}}",
    )]);
    assert!(DumpClient::new(http)
        .fetch_dumps(&editions, "20261001", "2026-09", &workspace.0, true)
        .is_err());
    assert!(queue.borrow().is_empty());
    assert_eq!(fs::read_dir(&workspace.0).unwrap().count(), 0);
}

#[test]
fn truncated_oversized_and_http_error_bodies_remain_unpublished() {
    for mode in ["truncated", "oversized", "http"] {
        let workspace = Workspace::new();
        let path = workspace.0.join("file.gz");
        let url = format!("{DUMP_BASE}/fixture/file.gz");
        let reply = match mode {
            "truncated" => Reply::get(&url, b"ab"),
            "oversized" => Reply::get(&url, b"abcdefg"),
            _ => Reply {
                status: 404,
                ..Reply::get(&url, b"")
            },
        };
        let (http, queue) = Fixture::new(vec![reply]);
        assert!(DumpClient::new(http)
            .fetch(&url, &path, 6, Some(&sha1(b"abcdef")), None)
            .is_err());
        assert!(!path.exists());
        assert!(queue.borrow().is_empty());
    }
}

#[test]
fn complete_partial_is_verified_locally_but_unidentified_partial_is_restarted() {
    for identified in [true, false] {
        let workspace = Workspace::new();
        let path = workspace.0.join("file.gz");
        let url = format!("{DUMP_BASE}/fixture/file.gz");
        let bytes = b"abcdef";
        fs::write(path.with_file_name("file.gz.part"), bytes).unwrap();
        if identified {
            fs::write(
                path.with_file_name("file.gz.part-source.json"),
                json!({"url":url,"bytes":bytes.len(),"sha1":sha1(bytes)}).to_string(),
            )
            .unwrap();
        }
        let replies = if identified {
            vec![]
        } else {
            vec![Reply::get(&url, bytes)]
        };
        let (http, queue) = Fixture::new(replies);
        assert_eq!(
            DumpClient::new(http)
                .fetch(&url, &path, 6, Some(&sha1(bytes)), None)
                .unwrap(),
            sha256(bytes)
        );
        assert_eq!(fs::read(path).unwrap(), bytes);
        assert!(queue.borrow().is_empty());
    }
}

#[test]
fn unfinished_sql_jobs_and_duplicate_xml_names_never_publish_manifests() {
    let workspace = Workspace::new();
    let editions = [("vec".into(), "vecwiki".into())];
    let mut replies = metadata(&compress(b"history"));
    let mut status: Value = serde_json::from_slice(&replies[0].bytes).unwrap();
    status["jobs"]["tables"]["status"] = json!("in-progress");
    replies[0].bytes = serde_json::to_vec(&status).unwrap();
    let (http, _) = Fixture::new(replies);
    assert!(DumpClient::new(http)
        .fetch_dumps(&editions, "20261001", "2026-09", &workspace.0, false)
        .is_err());
    assert_eq!(fs::read_dir(&workspace.0).unwrap().count(), 0);
    let base = format!("{DUMP_BASE}/other/mediawiki_content_current/vecwiki/2026-10-01/xml/bzip2/");
    let name = "vecwiki-2026-10-01-p0.xml.bz2";
    let line = format!("{}  {name}\n", sha256(b"xml"));
    let publisher = line.repeat(2);
    let (http, queue) = Fixture::new(vec![
        Reply::get(&format!("{base}SHA256SUMS"), publisher.as_bytes()),
        Reply::head(&format!("{base}{name}"), 3),
    ]);
    assert!(DumpClient::new(http)
        .fetch_content(&editions, "2026-10-01", &workspace.0, false)
        .is_err());
    assert!(queue.borrow().is_empty());
    assert_eq!(fs::read_dir(&workspace.0).unwrap().count(), 0);
}
