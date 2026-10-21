fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(feature = "cli")]
    build_fasttext()?;
    Ok(())
}

#[cfg(feature = "cli")]
fn build_fasttext() -> Result<(), Box<dyn std::error::Error>> {
    use sha2::Digest;
    use std::{env, fs, path::PathBuf, process::Command};
    let archive = PathBuf::from(".cache/fasttext-0.9.3.tar.gz");
    println!("cargo:rerun-if-changed={}", archive.display());
    println!("cargo:rerun-if-changed=src/dumps.cc");
    let bytes = fs::read(&archive)
        .map_err(|e| format!("Eseguire prima sh scripts/prepare-fasttext.sh: {e}"))?;
    if format!("{:x}", sha2::Sha256::digest(&bytes))
        != "eb03f2ef6340c6ac9e4398a30026f05471da99381b307aafe2f56e4cd26baaef"
    {
        return Err(
            "Sorgente fastText 0.9.3 alterato; eseguire sh scripts/prepare-fasttext.sh".into(),
        );
    }
    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR mancante")?);
    // Extract only pinned upstream source; builds never access the network.
    let status = Command::new("tar")
        .args(["-xzf"])
        .arg(&archive)
        .arg("-C")
        .arg(&output)
        .arg("fasttext-0.9.3/src")
        .status()?;
    if !status.success() {
        return Err("Estrazione del sorgente fastText fallita".into());
    }
    let source = output.join("fasttext-0.9.3/src");
    let mut compiler = cc::Build::new();
    // Upstream is frozen; do not replay its unrelated compiler warnings on
    // every Cargo command. Our bridge is checked separately with warnings on.
    compiler
        .cpp(true)
        .std("c++17")
        .include(&source)
        .warnings(false);
    // Upstream setup.py replaces extra_compile_args with its C++17 flags.
    // Keep portable compiler defaults: no fast-math, native CPU or FMA flags.
    compiler.flag_if_supported("-pthread");
    for name in [
        "args",
        "autotune",
        "matrix",
        "dictionary",
        "loss",
        "productquantizer",
        "densematrix",
        "quantmatrix",
        "vector",
        "model",
        "utils",
        "meter",
        "fasttext",
    ] {
        compiler.file(source.join(format!("{name}.cc")));
    }
    compiler.try_compile("orphanwiki_fasttext")?;
    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .flag("-isystem")
        .flag(source.as_os_str())
        .file("src/dumps.cc")
        .try_compile("orphanwiki_topic_bridge")?;
    Ok(())
}
