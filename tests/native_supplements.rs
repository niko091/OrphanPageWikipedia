#![cfg(feature = "cli")]
use _native::dumps::{canonical_json_sha256, write_canonical_json};
use serde_json::json;
use sha2::{Digest, Sha256};

#[test]
fn canonical_numbers_strings_and_order_match_the_frozen_python_identity() {
    let tie = "1255183068570128.25".parse::<f64>().unwrap();
    let value = json!({"z":[0,-0.0,1.0,1e-4,1e-5,1e15,1e16,1e20,1e-100,5e-324,tie,0.9859704508633369],"È":"🌙\n\u{7f}\u{1b}","a":true});
    let expected = r#"{"a": true, "z": [0, -0.0, 1.0, 0.0001, 1e-05, 1000000000000000.0, 1e+16, 1e+20, 1e-100, 5e-324, 1255183068570128.2, 0.9859704508633369], "\u00c8": "\ud83c\udf19\n\u007f\u001b"}"#;
    let mut encoded = Vec::new();
    write_canonical_json(&value, &mut encoded).unwrap();
    assert_eq!(String::from_utf8(encoded).unwrap(), expected);
    assert_eq!(
        canonical_json_sha256(&value).unwrap(),
        format!("{:x}", Sha256::digest(expected))
    );
}

#[test]
fn canonical_extremes_and_notation_boundaries_do_not_round_or_change_types() {
    let original = "[1.7976931348623157e+308, 2.2250738585072014e-308, 9.999999999999999e-05, 9999999999999998.0, 1.0000000000000002e+16, -9223372036854775808, 18446744073709551615]";
    let value: serde_json::Value = serde_json::from_str(original).unwrap();
    let mut encoded = Vec::new();
    write_canonical_json(&value, &mut encoded).unwrap();
    assert_eq!(String::from_utf8(encoded).unwrap(), original);
}

#[test]
fn canonical_writer_streams_large_arrays_and_preserves_sink_errors() {
    use std::io::{self, Write};
    struct Sink {
        calls: usize,
        bytes: usize,
        fail_at: usize,
    }
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.bytes >= self.fail_at {
                return Err(io::Error::other("synthetic sink failure"));
            }
            assert!(bytes.len() <= 65536);
            self.bytes += bytes.len();
            self.calls += 1;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let value = json!({"metadata":{"complete":true},"pages":(0..10000).map(|i|json!({"page_id":i,"title":"È 🌙","score":1e-7})).collect::<Vec<_>>()});
    let mut sink = Sink {
        calls: 0,
        bytes: 0,
        fail_at: usize::MAX,
    };
    {
        let mut buffered = io::BufWriter::with_capacity(65536, &mut sink);
        write_canonical_json(&value, &mut buffered).unwrap();
        buffered.flush().unwrap();
    }
    assert!(sink.calls > 8);
    assert!(sink.bytes > 500000);
    let mut broken = Sink {
        calls: 0,
        bytes: 0,
        fail_at: 16,
    };
    assert!(write_canonical_json(&value, &mut broken).is_err());
}
