# Orphan Articles in Wikipedia

Analisi descrittiva delle pagine orfane di edizioni linguistiche complete di Wikipedia. I dati di ricerca provengono esclusivamente dai dump ufficiali Wikimedia: SQL per pagine, collegamenti e categorie; MediaWiki History per creazione ricostruita e cronologia degli autori; XML `mediawiki_content_current` per la qualità strutturale stimata con Johnson V2. L’età usa `page_creation_timestamp`; i conteggi degli autori si riferiscono alla cronologia pubblica e alle voci attribuite nel censimento. Le soglie di qualità sono relative alla singola edizione. Il modello locale dei 64 temi usa i collegamenti del grafo e pesi preaddestrati verificati. Le proposte di collegamento sono suggerimenti da controllare manualmente, non modifiche automatiche. Con **Fix this**, la webapp legge su richiesta il wikitesto corrente delle pagine coinvolte dalle API ufficiali di Wikipedia e prepara prompt e fonti da copiare in una chat nuova; l’invio al modello è manuale. I testi restano in memoria fino alla chiusura della finestra e non aggiornano i dati del censimento. Revisioni, dimensioni e checksum accompagnano le fonti; quelle non verificabili sono segnalate.

## Struttura

- `config/languages.json`: unico elenco delle edizioni da raccogliere. Aggiungere qui un codice valido per includere l'intera edizione in download, importazione, temi e confronto. `databases` contiene solo eventuali nomi tecnici non derivabili dal codice.
- `config/quality.json`: definizione verificabile di Johnson V2, pesi e alias dell’estrattore di riferimento. I nomi canonici dei namespace sono letti dallo stesso XML. La lunghezza segue la convenzione documentata dei byte UTF-8; le regex conservano i limiti del riferimento per commenti, nowiki e template.
- `orphanwiki/dumps.py`: ricerca, download verificato, importazione a blocchi, qualità dal contenuto corrente e aggiornamento delle misure senza ricalcolare il grafo.
- `orphanwiki/core.py`: orchestrazione dell'analisi nativa, temi, confronto interlingua ed esportazioni.
- `src/`: calcoli Rust per grafo, cronologia e statistiche della dashboard; PyO3 e WebAssembly condividono il nucleo pertinente.
- `scripts/run_dumps.py`: esegue in sequenza l'intera raccolta, con log, benchmark delle fasi, monitoraggio delle risorse e ripresa dei passi verificati.
- `web/src/`: interfaccia TypeScript e CSS; `web/assets/` è prodotto dal build. Nel dialogo di Fix this, l’opzione DEBUG aggiunge al prompt domande diagnostiche separate dalla risposta normale; è disattivata di default.
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

Il modello dei temi deve essere già presente in `data/models/johnson-outlink-20220727.bin` (`--model` per un altro percorso). Procurarsi il file dall'URL `model_url` di `config/topics.json`: la pipeline non scarica i pesi e verifica il loro SHA-512 prima dell'inferenza.

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

Su un server raggiunto via SSH, avviare il comando dentro `tmux` per poter chiudere la connessione. Dalla radice del progetto, dopo aver installato `tmux` sul server:

```sh
tmux new -s orphanwiki
.venv/bin/python scripts/run_dumps.py --sql-release 20261001 --history-release 2026-09 --languages vec lmo --metrics-interval 5
```

Premere `Ctrl+B`, poi `D` per staccarsi; dopo una nuova connessione SSH usare `tmux attach -t orphanwiki`. Il processo continua sul server, che deve restare acceso. Dopo un arresto, rilanciare il comando per riutilizzare i checkpoint verificati.

Ogni esecuzione normale salva in `data/logs/` (`--log-dir` per cambiarlo): `.log` con comandi, output, errori e messaggi periodici; `.benchmark.json` con tempi, CPU ed esiti per fase; `.benchmark.metrics.jsonl` con eventi e campioni leggibili durante il lavoro. I report includono hardware, affinità CPU, limiti cgroup alla radice del mount, configurazioni e loro checksum, release richieste e identità del codice. Errori e interruzioni gestite producono il riepilogo; dopo un arresto forzato possono restare soltanto log ed eventi parziali. Il piano non produce benchmark. Le fasi riutilizzate sono indicate con `SALTA` nel log e non misurano un ricalcolo.

`--metrics-interval` regola il campionamento (10 secondi di default): CPU, somma RSS e swap dei processi, memoria disponibile, carico e pressione delle risorse del sistema, spazio libero e I/O. `100%` CPU equivale a un core. La RSS può perdere picchi brevi e contare pagine condivise più volte; non garantisce il fabbisogno di RAM. La CPU per fase comprende i worker attesi; quella periodica è una stima. L'I/O comprende il processo principale e i discendenti già attesi: i worker attivi vengono contabilizzati alla terminazione; rete e cache sono escluse dai byte fisici. Le misure Linux mancanti restano `null`; carico e memoria disponibile includono gli altri programmi del server.

Per confrontare i server, usare prima lo stesso censimento completo `vec lmo`, identiche release e directory nuove su ciascuna macchina, distinguendo download, elaborazione e cache/checkpoint. Le edizioni piccole non dimensionano l'italiano. La pipeline è sequenziale e usa SQLite su disco: più RAM non attiva più core. `--work-dir` sceglie il disco di staging. `--topic-batch-size` regola i blocchi dei temi (256 voci, intervallo 1–10000), senza ridurre il censimento. `--topic-budget-seconds` è il budget complessivo dei temi (86400 secondi); aumentarlo, ad esempio a `604800`, se necessario. Se si esaurisce, il controllo di completezza ferma la pipeline. Due server non condividono automaticamente RAM o calcolo; usare directory indipendenti e riunire gli input selezionati su una macchina per il confronto finale.

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

