# Orphan Articles in Wikipedia

A web app for analyzing **orphan articles on Wikipedia**. Inspired by the paper [Orphan Articles: The Dark Matter of Wikipedia](https://ojs.aaai.org/index.php/ICWSM/article/view/31300), it lets you explore the characteristics of articles and the editors who created them.

Research data comes exclusively from official Wikimedia dumps: SQL for the page graph, MediaWiki History for reconstructed creation dates and editor histories, and `mediawiki_content_current` XML for Johnson V2 structural quality, normalized per edition. A local model classifies articles into 64 topics based on their links.

The deorphanization tool identifies possible links based on those present in other language editions. **Fix this** retrieves the relevant pages’ wikitext on request and prepares a prompt with source material to copy into a chat with a large language model (LLM). The model suggests links or text additions, which users must review and manually apply on Wikipedia. The web app does not edit Wikipedia.

## Installation

Currently configured editions:

`vec`, `lmo`, `fur`, `roa-tara`, `nap`, `pms`, `scn`, `sc`, `lij`, `eml`, `co`, `lld`, `it`, `ca`, `pt`, `fr`.

To analyze other editions, download the project locally and edit the `languages` list in [config/languages.json](config/languages.json). Suggestions based on comparisons between languages require at least two processed editions.

### Dependencies

Requires **Rust 1.89 or later**, installed through rustup, and **Node.js 22.18 or later**, with npm. Download the repository and run commands from its root directory. On Ubuntu/Debian, install the build dependencies first:

```sh
sudo apt-get update
sudo apt-get install build-essential libsqlite3-dev pkg-config curl tar ca-certificates coreutils
```

Build the CLI and dashboard:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.105 --locked
sh scripts/prepare-fasttext.sh
cargo build --release --locked
npm ci
npm run build
```

For analysis, download the model from `model_url` in `config/topics.json` and save it as `data/models/johnson-outlink-20220727.bin`. The pipeline verifies its checksum but does not download the weights.

### Language analysis

Data is not included in the repository. The following command downloads and analyzes all languages in `config/languages.json`. Releases must be specified explicitly: choose available official release dates; the dates below are examples:

```sh
target/release/orphanwiki pipeline --sql-release 20261001 --history-release 2026-09
```

The pipeline downloads and verifies the dumps, analyzes complete editions, compares languages, and publishes one collection in `web/data/collection.sqlite`. All selected editions use the same SQL release, History release and current-content XML release (the SQL date by default). SQL tables are independently produced snapshots. A new collection replaces the previous one only after every selected edition and comparison has passed verification.

Downloads are processed one edition at a time and removed after their verified import; temporary graphs, supplements, comparison caches and JSON/CSV exports are removed after the database is published. Failed runs retain their private checkpoints for retry. The local topic model and operational logs remain on disk. Allow space for imports and for both the active and incoming collection during updates; final database size alone does not determine the required disk capacity.

### Starting the dashboard

Once the data is available, the script prepares the build dependencies, builds the project, and starts the local service. The Rust service reads the SQLite collection and performs article, orphan-card and creator filtering and pagination on the server, returning 25 records per request. Editorial review state stays in a separate database. The service requires a verified collection database and returns an error when a required resource is missing.

```sh
bash scripts/start-dashboard.sh
```

Open `http://127.0.0.1:8000` and stop the service with `Ctrl+C`.
