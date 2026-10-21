# Orphan Articles in Wikipedia

Analisi descrittiva delle pagine orfane di edizioni linguistiche complete di Wikipedia. I dati di ricerca provengono esclusivamente dai dump ufficiali Wikimedia: SQL per pagine, collegamenti e categorie; MediaWiki History per creazione ricostruita e cronologia degli autori; XML `mediawiki_content_current` per la qualità strutturale stimata con Johnson V2. L’età usa `page_creation_timestamp`; i conteggi degli autori si riferiscono alla cronologia pubblica e alle voci attribuite nel censimento. Le soglie di qualità sono relative alla singola edizione. La qualità usa la revisione dell'export XML ufficiale selezionato anche quando differisce da quella SQL; entrambe le identità e il disallineamento restano registrati. Il modello locale dei 64 temi usa i collegamenti del grafo e pesi preaddestrati verificati. Le proposte di collegamento sono suggerimenti da controllare manualmente, non modifiche automatiche. Con **Fix this**, la webapp legge su richiesta il wikitesto corrente delle pagine coinvolte dalle API ufficiali di Wikipedia e prepara prompt e fonti da copiare in una chat nuova; l’invio al modello è manuale. I testi restano in memoria fino alla chiusura della finestra e non aggiornano i dati del censimento. Revisioni, dimensioni e checksum accompagnano le fonti; quelle non verificabili sono segnalate. **Finished** apre la valutazione delle coppie incluse nel prompt: Fixato, Non fixato o Bad link. Le valutazioni condivise sono segnalazioni manuali separate dal censimento: Fixato e Bad link escludono la coppia dai prompt e la spostano in fondo a Connect, conservando tutte le evidenze; Non fixato la rende nuovamente suggeribile.

## Struttura

- `config/languages.json`: unico elenco delle edizioni da raccogliere. Aggiungere qui un codice valido per includere l'intera edizione in download, importazione, temi e confronto. `databases` contiene solo eventuali nomi tecnici non derivabili dal codice.
- `config/quality.json`: definizione verificabile di Johnson V2, pesi e alias dell’estrattore di riferimento. I nomi canonici dei namespace sono letti dallo stesso XML. La lunghezza segue la convenzione documentata dei byte UTF-8; le regex conservano i limiti del riferimento per commenti, nowiki e template.
- `orphanwiki/dumps.py`: ricerca, download verificato, importazione a blocchi, History con cache limitata e transazioni aggregate, export con merge di flussi indicizzati, qualità dal contenuto corrente e aggiornamento delle misure senza ricalcolare il grafo.
- `orphanwiki/core.py`: orchestrazione dell'analisi nativa, temi, confronto interlingua incrementale con indici persistenti e lookup degli archi a blocchi ed esportazioni con generazioni distinte dei trasporti; prepara indici compatti per le liste, record separati delle shortlist editoriali caricate solo per le schede visibili, dettagli Connect completi con metadati condivisi per edizione e temi del profilo in blocchi da 1024 ID pagina.
- `src/`: nucleo Rust indipendente da Python per grafo, categorie, input e soglia dei temi, ranking delle evidenze, età, attività History, osservazioni e join a blocchi di History/qualità/temi e statistiche della dashboard. La feature `python` aggiunge i binding PyO3; WebAssembly conserva l'API della dashboard. `src/main.rs` espone la CLI autonoma opzionale (`cli`): `fetch-dumps`, `fetch-content`, `import-dumps`, `import-history`, `quality`, `topics`, `analyze` e `dashboard` eseguono acquisizione, importazione, analisi ed export senza Python; le altre fasi terminano con errore finché non vengono portate. `src/dumps.rs` conserva checkpoint, merge indicizzato del grafo, deduplicazione esatta delle creazioni, qualità Johnson V2 con XML in streaming e inferenza fastText a blocchi sullo staging SQL; espone anche staging SQLite, cache persistente verificata e matching incrementale nativi con evidenze complete, adozione verificata delle coppie pubblicate e preparazione degli aggregati con riepiloghi e indici autori separati, ancora da collegare al comando `compare`. `src/dumps.cc` esegue il motore C++ ufficiale 0.9.3 in un worker locale con richieste a blocchi; `build.rs` verifica il sorgente e incorpora il worker nel binario soltanto per `cli`. Il Rust vieta `unsafe` in compilazione. L'estrattore della qualità conserva le semantiche delle regex congelate, senza espandere il wikitesto. `cargo test --locked --no-default-features --features cli` verifica questi percorsi. La pipeline di produzione resta per ora ibrida.
- `scripts/run_dumps.py`: esegue in sequenza l'intera raccolta, con log, benchmark delle fasi, monitoraggio delle risorse e ripresa dei passi verificati; il sottocomando `benchmark` confronta due implementazioni su input congelati e output dichiarati.
- `orphanwiki/editorial.py`: servizio WSGI per sito e valutazioni condivise; SQLite locale/Cloud VPS o MariaDB ToolsDB su Toolforge. Salva solo identità delle coppie, stato, versione e data; controlla le coppie nei dettagli approvati e rileva aggiornamenti concorrenti. Test in `tests/python/test_editorial.py`.
- `web/src/`: interfaccia TypeScript e CSS; `web/assets/` è prodotto dal build. Su invio esplicito dello username Wikimedia, la personalizzazione associa le ultime 100 modifiche pubbliche complessive ai temi dell’ultima raccolta disponibile per ogni lingua, anche diversa da quella delle proposte selezionate; mostra la copertura e mantiene in memoria il profilo. Legge i blocchi dei temi di più lingue con un limite condiviso di quattro richieste, limita a 200 record il buffer di unione dei contributi e salta il calcolo dei punteggi quando non emergono temi utilizzabili. Nel dialogo di Fix this, l’opzione DEBUG aggiunge al prompt domande diagnostiche separate dalla risposta normale; è disattivata di default.
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

Per preparare la CLI con Rust 1.89 o successivo eseguire dalla radice `sh scripts/prepare-fasttext.sh`, poi `cargo build --release --locked --no-default-features --features cli --bin orphanwiki`. Lo script conserva nella cache ignorata il sorgente ufficiale fastText 0.9.3 verificato con SHA-256; non scarica pesi. Il build controlla nuovamente il sorgente e non scarica il motore; il binario non richiede Python. Il worker fastText incorporato viene estratto in una directory temporanea privata, avviato localmente e rimosso alla chiusura; probabilità e tassonomia mantengono il contratto originale. La licenza del motore è in `LICENSE.fasttext`. `target/release/orphanwiki --help` elenca il contratto dei comandi. `fetch-dumps` e `fetch-content` eseguono piani e download; `import-dumps --manifest FILE` importa grafo e History; `import-history --manifest FILE` aggiorna solo il supplemento dallo staging SQL esistente; `quality --content-release YYYY-MM-DD --languages vec lmo` calcola Johnson V2 dai file locali verificati; `topics --input-dir data/imported --output-dir data/imported --work-dir data/dumps/staging --languages vec lmo` applica il modello approvato all'intero censimento. Batch, budget totale e `topic-run.json` mantengono il contratto Python: le edizioni rinviate non pubblicano predizioni parziali. Le altre fasi usano ancora Python. `--repo-root DIR` risolve configurazioni e percorsi relativi dalla radice indicata; senza opzione cerca `config/languages.json` risalendo dalla directory corrente, senza incorporare il percorso del computer usato per compilare. I download parziali Rust richiedono una sidecar che ne identifica URL, dimensione e checksum disponibile; un parziale precedente privo di identità viene riscaricato, mentre gli archivi completi verificati sono riutilizzati.

Per compilare la feature `cli` su Ubuntu installare `sudo apt-get install libsqlite3-dev pkg-config g++ curl tar`. L'importatore usa la libreria SQLite di sistema, come il riferimento Python; in CI vengono installati gli stessi pacchetti e preparato il sorgente nativo. Le feature `python` e `dashboard` non compilano né richiedono fastText C++.

`target/release/orphanwiki analyze data/imported/vec-raw.json --history data/imported/vec-history.json --rules config/categories.json --work-dir data/dumps/staging --output web/data/vec.json` prepara JSON canonico, CSV e tutti i trasporti in una directory privata e pubblica il manifest per ultimo. I supplementi temi/qualità adiacenti sono rilevati automaticamente; `--topics` e `--quality` li selezionano esplicitamente. Il percorso a blocchi per SQL da almeno 128 MiB mantiene le identità precedenti; i grafi più piccoli conservano il checksum JSON canonico. Un risultato diverso richiede un nome di output distinto: i file scientifici approvati non sono sovrascritti. Un risultato identico può essere ripubblicato con nuovi trasporti.

`target/release/orphanwiki dashboard --data-dir web/data` rigenera tutti i trasporti delle edizioni approvate senza Python e senza riscrivere i JSON/CSV canonici. Prepara una generazione distinta, conserva i file precedenti e sposta il manifest per ultimo; un errore di conversione lascia intatta la raccolta approvata. I writer sono serializzati e una modifica concorrente del manifest interrompe la pubblicazione.

#### Analisi delle lingue:

Scegliere release ufficiali esistenti e modificare solo `config/languages.json` per cambiare le edizioni. Il piano elenca i file e lo spazio compresso stimato senza scaricare:

```sh
.venv/bin/python -m orphanwiki fetch-dumps --sql-release 20261001 --history-release 2026-09 --plan
```

Per un'esecuzione locale più piccola, selezionare le lingue da acquisire con `--languages`:

```sh
.venv/bin/python scripts/run_dumps.py --sql-release 20261001 --history-release 2026-09 --languages vec lmo
```

Per estendere una raccolta, aggiungere i nuovi codici in `config/languages.json` e passare a `--languages` soltanto le nuove edizioni. Il confronto include automaticamente anche tutte le edizioni configurate già pubblicate nella stessa directory di output, con le rispettive release originali: produce suggerimenti dalle nuove verso le precedenti, dalle precedenti verso le nuove e tra le nuove. Conservare i grafi `*-raw.json`, i supplementi locali e la directory `--work-dir`.

Gli indici e le coppie ordinate complete, comprese quelle senza candidati, restano in `<work-dir>/comparison-cache/`. Ogni ripresa verifica gli SHA-256 degli input e della cache. Un nuovo grafo invalida solo le coppie che coinvolgono la sua lingua; aggiornare History, qualità o categorie conserva le coppie quando il grafo coincide. Gli export precedenti alla cache possono fornire le coppie già pubblicate dopo verifica delle identità e delle evidenze; in questa prima migrazione occorre comunque costruire gli indici dei grafi. Le generazioni SQLite precedenti restano disponibili e il manifest della cache cambia soltanto a completamento dell'aggiornamento. Restano necessari I/O per verifica e copia della cache, spazio per le generazioni conservate e l'unione/esportazione dei suggerimenti aggiornati per tutte le lingue; i log distinguono coppie nuove e riutilizzate. Il comando diretto `compare` usa gli stessi indici; senza `--work-dir` li salva in `staging` accanto ai grafi di input.

Per scaricare, importare, calcolare età, autori, temi e qualità, quindi confrontare tutte le edizioni configurate:

```sh
.venv/bin/python scripts/run_dumps.py --sql-release 20261001 --history-release 2026-09
```

La pipeline include automaticamente il contenuto XML e Johnson V2. La release del contenuto è la data SQL (`20261001` → `2026-10-01`); `--content-release YYYY-MM-DD` permette di sceglierne un’altra. Se la release non è disponibile, il passo si arresta senza sostituirla con dati diversi. La revisione XML selezionata fornisce il punteggio quando il testo è disponibile, anche se differisce dalla revisione SQL; il censimento conserva entrambe le revisioni e la loro relazione. Aggiungere `--plan` allo stesso comando per il piano SQL, History e XML. Le fasi completate e verificate sono riutilizzate; per gli export esistenti vengono aggiornate le misure senza ripetere grafo, temi e confronto quando gli input coincidono.

Su un server raggiunto via SSH, avviare il comando dentro `tmux` per poter chiudere la connessione. Dalla radice del progetto, dopo aver installato `tmux` sul server:

```sh
tmux new -s orphanwiki
.venv/bin/python scripts/run_dumps.py --sql-release 20261001 --history-release 2026-09 --languages vec lmo --metrics-interval 5
```

Premere `Ctrl+B`, poi `D` per staccarsi; dopo una nuova connessione SSH usare `tmux attach -t orphanwiki`. Il processo continua sul server, che deve restare acceso. Dopo un arresto, rilanciare il comando per riutilizzare i checkpoint verificati.

Ogni esecuzione normale salva in `data/logs/` (`--log-dir` per cambiarlo): `.log` con comandi, output, errori e messaggi periodici; `.benchmark.json` con tempi, CPU ed esiti per fase; `.benchmark.metrics.jsonl` con eventi e campioni leggibili durante il lavoro; `.benchmark.spans.jsonl` con inizio/fine monotonic delle sottofasi, ID di esecuzione, fase e padre, righe e byte disponibili. Gli intervalli distinguono hash, proiezione SQL, JSON del grafo, scansioni/indici/misure History, inferenza del modello, XML/feature/percentili della qualità, indice degli archi, matching per lingua/blocco, CSV, trasporti e aggregati. I tempi includono i figli: per ricostruire il tempo esclusivo sottrarre l'unione dei loro intervalli, senza sommarli al padre. `--no-phase-spans` disabilita questi soli intervalli dettagliati. I report includono hardware, affinità CPU, limiti cgroup alla radice del mount, configurazioni e loro checksum, release richieste e identità del codice. Errori e interruzioni gestite producono il riepilogo; gli intervalli dei worker terminati bruscamente sono chiusi con un limite temporale dichiarato al rilevamento. Dopo un arresto forzato dell'intera pipeline possono restare soltanto log ed eventi parziali. Il piano non produce benchmark. Le fasi riutilizzate sono indicate con `SALTA` e il motivo del checkpoint nel log e non misurano un ricalcolo. Tutte queste misure sono operative e restano fuori dai dataset di ricerca.

`--metrics-interval` regola il campionamento (10 secondi di default): CPU, somma RSS e swap dei processi, memoria disponibile, carico e pressione delle risorse del sistema, spazio libero e I/O. `100%` CPU equivale a un core. La RSS può perdere picchi brevi e contare pagine condivise più volte; non garantisce il fabbisogno di RAM. La CPU per fase comprende i worker attesi; quella periodica è una stima. L'I/O comprende il processo principale e i discendenti già attesi: i worker attivi vengono contabilizzati alla terminazione; rete e cache sono escluse dai byte fisici. Le misure Linux mancanti restano `null`; carico e memoria disponibile includono gli altri programmi del server.

Per confrontare i server, usare prima lo stesso censimento completo `vec lmo`, identiche release e directory nuove su ciascuna macchina, distinguendo download, elaborazione e cache/checkpoint. Le edizioni piccole non dimensionano l'italiano. La pipeline è sequenziale e usa SQLite su disco: più RAM non attiva più core. `--work-dir` sceglie il disco di staging. Il matching interlingua interroga gli archi per blocchi fino a 512 orfane, ridotti secondo il fanout osservato; ordina su SQLite e costruisce un record finale per orfana, conservando tutti i candidati e tutte le evidenze. `--topic-batch-size` regola i blocchi dei temi (256 voci, intervallo 1–10000), senza ridurre il censimento. `--topic-budget-seconds` è il budget complessivo dei temi (86400 secondi); aumentarlo, ad esempio a `604800`, se necessario. Se si esaurisce, il controllo di completezza ferma la pipeline. Due server non condividono automaticamente RAM o calcolo; usare directory indipendenti e riunire gli input selezionati su una macchina per il confronto finale.

Per misurare la migrazione, `.venv/bin/python scripts/run_dumps.py benchmark --spec benchmarks/rust/analyze-vec.json --output-dir benchmarks/rust/analyze-vec-run-01` esegue prove alternate in directory nuove e confronta gli output dichiarati, tempi, CPU e memoria. La specifica JSON ha `version: 1`, `inputs` immutabili, eventuali `copies` private, `baseline` e `candidate` con `cwd` e `commands` (liste di argomenti senza shell), e `outputs` con `kind` e `path`. `{repo}` indica la radice del progetto e `{run}` una directory nuova per ogni prova. Gli output `canonical` confrontano tutti i record ordinati; `manifest` confronta i membri canonici; `bytes` confronta byte esatti. Includere configurazioni, modello e file referenziati, non soltanto i manifest. Il report `comparison.json` distingue miglioramento, mancato miglioramento, risultato inconcludente ed errore. Il criterio predefinito richiede parità esatta, almeno 1,10× di speedup mediano, miglioramento in ogni coppia e RSS al massimo 1,10× in ogni coppia; hash e copie riscaldano la cache del sistema operativo. Non dimostra letture a cache fredda né prestazioni dei censimenti reali a partire da fixture sintetiche. Specifiche e report restano sotto `benchmarks/`, ignorati da Git.

History usa una cache SQLite di 256 MiB e commit ogni circa 250.000 tentativi di inserimento, con buffer fino a 5.000 record per tipo e una cache esatta di 100.000 identità di creazione per partizione. `--history-cache-mib` (1–65536) e `--history-commit-rows` (1–10000000) sono disponibili nello script e nei comandi Python/Rust `import-dumps` e `import-history`. Con molta RAM si può provare `--history-cache-mib 2048`, confrontando i benchmark. Censimento, definizioni, checksum e checkpoint completi restano validi. Il log mostra righe lette e utili, duplicati evitati, commit e tempo SQLite, circa ogni 30 secondi quando il parser riprende il controllo.

Dopo un'interruzione si riusano le partizioni complete e si ripete quella incompleta. Mantenere gli stessi percorsi; attendere la chiusura della pipeline prima di aggiornare il codice e ricompilare l'estensione Rust. Il merge degli export evita le query per articolo, secondo il principio di elaborazione a blocchi discusso in [Database Supported Haskell](https://db.cs.uni-tuebingen.de/publications/2013/analysing-the-entire-wikipedia-history-with-database-supported-haskell/padl2013.pdf); [Hedera](https://arxiv.org/abs/1701.03937) e [WikiBrain](https://toby.li/files/WikiBrain-WikiSym2014.pdf) trattano streaming, partizioni e riuso. I tempi sintetici non prevedono la durata di un censimento reale.

Per aggiornare età e autori dai dump locali già verificati, mantenendo grafo, temi e candidate degli export pubblicati:

```sh
.venv/bin/python -m orphanwiki import-history --sql-release 20261001 --history-release 2026-09 --languages vec lmo
.venv/bin/python -m orphanwiki refresh-measures --languages vec lmo
```

Per aggiungere la qualità separatamente, scegliere una release di contenuto disponibile e controllare prima il piano. La revisione XML selezionata è valutata quando il testo è disponibile, conservando la sua identità e la relazione con la revisione SQL:

```sh
.venv/bin/python -m orphanwiki fetch-content --content-release 2026-10-01 --languages vec lmo --plan
.venv/bin/python -m orphanwiki fetch-content --content-release 2026-10-01 --languages vec lmo
.venv/bin/python -m orphanwiki quality --content-release 2026-10-01 --languages vec lmo
.venv/bin/python -m orphanwiki refresh-measures --languages vec lmo
```

Omettere `--languages vec lmo` nei comandi precedenti per tutte le edizioni di `config/languages.json`. Ogni edizione selezionata è completa. I checkpoint della qualità riutilizzano le partizioni verificate e le feature SQLite; non richiedono di ripetere il modello dei temi. I nuovi export datati mantengono disponibili quelli precedenti. `.venv/bin/python -m orphanwiki dashboard --data-dir web/data` ricostruisce soltanto i trasporti da export approvati, inclusi i blocchi dei temi necessari per velocizzare i profili nelle raccolte già pubblicate: non acquisisce date o qualità mancanti e non riscrive i dataset JSON/CSV originali. Ogni conversione prepara una nuova generazione dei trasporti e aggiorna il manifest per ultimo; le generazioni precedenti restano disponibili. `npm test` include benchmark deterministici della personalizzazione su dati sintetici: confronta byte JSON, righe lette, dimensioni del buffer di unione e turni di richieste a risposte controllate, verifica risultati identici e il riuso in memoria dei temi al cambio di raccolta; non misura la latenza reale delle API.

#### Avvio Dashboard

```sh
.venv/bin/python -m orphanwiki.editorial --web-root web --database data/editorial.sqlite --bind 127.0.0.1 --port 8000
```

Questo server locale serve anche le valutazioni condivise; il database resta fuori dalla cartella pubblica e dai dataset di ricerca. Gli stati vengono letti all’apertura delle pagine e con **Aggiorna valutazioni**. I pulsanti **Valuta coppia** in Connect permettono anche di riaprire coppie già valutate. Nessuna risposta del modello, wikitesto o username viene salvato. Senza il servizio API resta possibile consultare i dati e preparare i prompt, ma le valutazioni non possono essere pubblicate.

Per un servizio WSGI su Cloud VPS o Toolforge installare le dipendenze web e avviare Gunicorn dalla radice del repository, dopo aver costruito gli asset:

```sh
.venv/bin/python -m pip install -e '.[web]'
.venv/bin/gunicorn --bind "0.0.0.0:${PORT:-8000}" --workers 2 --threads 4 orphanwiki.editorial:app
```

Su Cloud VPS SQLite deve risiedere su disco locale persistente: `ORPHANWIKI_REVIEW_SQLITE` ne configura il percorso; `ORPHANWIKI_WEB_ROOT` configura la cartella pubblica. Su Toolforge creare prima un database [ToolsDB](https://wikitech.wikimedia.org/wiki/Help:Toolforge/ToolsDB) con il prefisso delle credenziali del tool, poi impostare `ORPHANWIKI_REVIEW_MYSQL_DATABASE` al suo nome. Il servizio usa `TOOL_TOOLSDB_USER` e `TOOL_TOOLSDB_PASSWORD`, forniti dal [Build Service](https://wikitech.wikimedia.org/wiki/Help:Toolforge/Build_service), oppure il file credenziali indicato da `ORPHANWIKI_REVIEW_MYSQL_DEFAULTS_FILE` nei runtime che lo montano. L’host predefinito è `tools.db.svc.wikimedia.cloud`; `ORPHANWIKI_REVIEW_MYSQL_HOST` lo può configurare. Su Toolforge il servizio rifiuta di ripiegare su SQLite quando manca la configurazione MariaDB. La scelta del runtime, dei mount per gli export e dell’avvio pubblico va completata sull’hosting scelto; i test locali non verificano un deployment Wikimedia reale.

