# Orphan Articles in Wikipedia

Analisi descrittiva delle pagine orfane di edizioni linguistiche complete di Wikipedia. I dati di ricerca provengono esclusivamente dai dump ufficiali Wikimedia: SQL per pagine, collegamenti e categorie; MediaWiki History per la cronologia ricostruita degli utenti. Il modello locale dei 64 temi usa i collegamenti del grafo e pesi preaddestrati verificati. Le proposte di collegamento sono suggerimenti da controllare manualmente, non modifiche automatiche.

## Struttura

- `config/languages.json`: unico elenco delle edizioni da raccogliere. Aggiungere qui un codice valido per includere l'intera edizione in download, importazione, temi e confronto. `databases` contiene solo eventuali nomi tecnici non derivabili dal codice.
- `orphanwiki/dumps.py`: ricerca, download verificato, importazione a blocchi e analisi su disco dei dump grandi.
- `orphanwiki/core.py`: orchestrazione dell'analisi nativa, temi, confronto interlingua ed esportazioni.
- `src/`: calcoli Rust per grafo, cronologia e statistiche della dashboard; PyO3 e WebAssembly condividono il nucleo pertinente.
- `scripts/run_dumps.py`: esegue in sequenza l'intera raccolta, con log e ripresa dei passi verificati.
- `web/src/`: interfaccia TypeScript e CSS; `web/assets/` è prodotto dal build.
- `tests/`: test Rust, Python e browser DOM. Le fixture sintetiche non sono dati di ricerca.
- `.github/workflows/test.yml`: stessi controlli automatici su Ubuntu 24.04 per push e pull request.

## Comandi

#### Download dipendenze:

```sh
python3 -m venv .venv
.venv/bin/python -m pip install 'maturin>=1.12,<2' -e '.[topics]'
.venv/bin/maturin develop --release --locked
npm install
npm run build
```

#### Analisi delle lingue:

Scegliere release ufficiali esistenti e modificare solo `config/languages.json` per cambiare le edizioni. Il piano elenca i file e lo spazio compresso stimato senza scaricare:

```sh
.venv/bin/python -m orphanwiki fetch-dumps --sql-release 20261001 --history-release 2026-09 --plan
```

Per un'esecuzione locale più piccola, selezionare le lingue con il flag --languages:

```sh
.venv/bin/python scripts/run_dumps.py --sql-release 20261001 --history-release 2026-09 --languages vec lmo
```

Per scaricare, importare, classificare e confrontare tutte le edizioni configurate:

```sh
.venv/bin/python scripts/run_dumps.py --sql-release 20261001 --history-release 2026-09
```

#### Avvio Dashboard

```sh
.venv/bin/python -m http.server 8000 --bind 127.0.0.1 --directory web
```

