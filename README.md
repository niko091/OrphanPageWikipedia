# Orphan Articles in Wikipedia

Analisi descrittiva delle pagine orfane di edizioni linguistiche complete di Wikipedia. I dati di ricerca provengono esclusivamente dai dump ufficiali Wikimedia: SQL per pagine, collegamenti e categorie; MediaWiki History per creazione ricostruita e cronologia degli autori; XML `mediawiki_content_current` per la qualità strutturale stimata con Johnson V2. L’età usa `page_creation_timestamp`; i conteggi degli autori si riferiscono alla cronologia pubblica e alle voci attribuite nel censimento. Le soglie di qualità sono relative alla singola edizione. Il modello locale dei 64 temi usa i collegamenti del grafo e pesi preaddestrati verificati. Le proposte di collegamento sono suggerimenti da controllare manualmente, non modifiche automatiche.

## Struttura

- `config/languages.json`: unico elenco delle edizioni da raccogliere. Aggiungere qui un codice valido per includere l'intera edizione in download, importazione, temi e confronto. `databases` contiene solo eventuali nomi tecnici non derivabili dal codice.
- `config/quality.json`: definizione verificabile di Johnson V2, pesi e alias dell’estrattore di riferimento. I nomi canonici dei namespace sono letti dallo stesso XML. La lunghezza segue la convenzione documentata dei byte UTF-8; le regex conservano i limiti del riferimento per commenti, nowiki e template.
- `orphanwiki/dumps.py`: ricerca, download verificato, importazione a blocchi, qualità dal contenuto corrente e aggiornamento delle misure senza ricalcolare il grafo.
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

Per scaricare, importare, calcolare età, autori, temi e qualità, quindi confrontare tutte le edizioni configurate:

```sh
.venv/bin/python scripts/run_dumps.py --sql-release 20261001 --history-release 2026-09
```

La pipeline include automaticamente il contenuto XML e Johnson V2. La release del contenuto è la data SQL (`20261001` → `2026-10-01`); `--content-release YYYY-MM-DD` permette di sceglierne un’altra. Se la release non è disponibile, il passo si arresta senza sostituirla con dati diversi. Aggiungere `--plan` allo stesso comando per il piano SQL, History e XML. Le fasi completate e verificate sono riutilizzate; per gli export esistenti vengono aggiornate le misure senza ripetere grafo, temi e confronto quando gli input coincidono.

Per aggiornare età e autori dai dump locali già verificati, mantenendo grafo, temi e candidate degli export pubblicati:

```sh
.venv/bin/python -m orphanwiki import-history --sql-release 20261001 --history-release 2026-09 --languages vec lmo
.venv/bin/python -m orphanwiki refresh-measures --languages vec lmo
```

Per aggiungere la qualità separatamente, scegliere una release di contenuto disponibile e controllare prima il piano. Una revisione XML diversa da quella SQL resta indisponibile, anche con la stessa data di release:

```sh
.venv/bin/python -m orphanwiki fetch-content --content-release 2026-10-01 --languages vec lmo --plan
.venv/bin/python -m orphanwiki fetch-content --content-release 2026-10-01 --languages vec lmo
.venv/bin/python -m orphanwiki quality --content-release 2026-10-01 --languages vec lmo
.venv/bin/python -m orphanwiki refresh-measures --languages vec lmo
```

Omettere `--languages vec lmo` nei comandi precedenti per tutte le edizioni di `config/languages.json`. Ogni edizione selezionata è completa. I checkpoint della qualità riutilizzano le partizioni verificate e le feature SQLite; non richiedono di ripetere il modello dei temi. I nuovi export datati mantengono disponibili quelli precedenti. `dashboard --data-dir web/data` ricostruisce soltanto i trasporti da export approvati: non acquisisce date o qualità mancanti.

#### Avvio Dashboard

```sh
.venv/bin/python -m http.server 8000 --bind 127.0.0.1 --directory web
```

