#!/usr/bin/env bash
set -euo pipefail

if [[ ${1:-} == --help || ${1:-} == -h ]]; then
    cat <<'HELP'
Uso: bash scripts/start-dashboard.sh [opzioni di orphanwiki serve]

Installa le dipendenze, compila e avvia http://127.0.0.1:8000.
Richiede Node.js >=22.18, npm e Rust >=1.89 con rustup.
Su Ubuntu/Debian installa i pacchetti di sistema mancanti tramite apt-get
(con sudo se necessario). Non scarica dump o modelli e non converte i dati.

Esempio: bash scripts/start-dashboard.sh --port 8080
Arresto: Ctrl+C. I percorsi relativi si riferiscono alla radice del repository.
HELP
    exit 0
fi

cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
repo_root=$PWD

fail() { printf 'Errore: %s\n' "$*" >&2; exit 1; }
for tool in node npm rustup rustc cargo; do
    command -v "$tool" >/dev/null 2>&1 || fail "Installare $tool prima di avviare lo script (vedere README.md)."
done
node -e 'const [major, minor] = process.versions.node.split(".").map(Number); process.exit(major > 22 || (major === 22 && minor >= 18) ? 0 : 1)' \
    || fail 'È necessario Node.js 22.18 o successivo.'
read -r _ rust_version _ < <(rustc --version)
IFS=. read -r rust_major rust_minor _ <<< "$rust_version"
(( rust_major > 1 || (rust_major == 1 && rust_minor >= 89) )) \
    || fail 'È necessario Rust 1.89 o successivo (rustup update stable).'

missing_system=false
for tool in cc c++ make pkg-config curl tar sha256sum; do
    command -v "$tool" >/dev/null 2>&1 || missing_system=true
done
if ! command -v pkg-config >/dev/null 2>&1 || ! pkg-config --exists sqlite3; then
    missing_system=true
fi
if [[ $missing_system == true ]]; then
    command -v apt-get >/dev/null 2>&1 \
        || fail 'Installare compilatori C/C++, make, pkg-config, SQLite (header), curl, tar e sha256sum con il gestore di pacchetti di sistema.'
    elevate=()
    if (( EUID != 0 )); then
        command -v sudo >/dev/null 2>&1 || fail 'Serve sudo per installare i pacchetti di sistema.'
        elevate=(sudo)
    fi
    printf '\nInstallazione dei pacchetti di compilazione…\n'
    "${elevate[@]}" apt-get update
    "${elevate[@]}" apt-get install -y build-essential libsqlite3-dev pkg-config curl tar ca-certificates coreutils
fi

printf '\nPreparazione di WebAssembly e fastText…\n'
rustup target add wasm32-unknown-unknown
bindgen_version=0.2.105
if [[ -n ${WASM_BINDGEN:-} ]]; then
    [[ $("$WASM_BINDGEN" --version) == "wasm-bindgen $bindgen_version" ]] \
        || fail "WASM_BINDGEN deve indicare wasm-bindgen $bindgen_version."
elif command -v wasm-bindgen >/dev/null 2>&1 \
    && [[ $(wasm-bindgen --version) == "wasm-bindgen $bindgen_version" ]]; then
    WASM_BINDGEN=$(command -v wasm-bindgen)
else
    bindgen_root="$repo_root/.cache/wasm-bindgen-cli"
    WASM_BINDGEN="$bindgen_root/bin/wasm-bindgen"
    if [[ ! -x $WASM_BINDGEN ]] \
        || [[ $("$WASM_BINDGEN" --version) != "wasm-bindgen $bindgen_version" ]]; then
        cargo install wasm-bindgen-cli --version "$bindgen_version" --locked --root "$bindgen_root"
    fi
fi
export WASM_BINDGEN
# Il build web legge il Wasm da target/ anche se l'ambiente configura un altro percorso.
export CARGO_TARGET_DIR="$repo_root/target"
sh scripts/prepare-fasttext.sh

printf '\nInstallazione dipendenze npm e compilazione…\n'
npm ci
cargo build --release --locked --bin orphanwiki
npm run build

printf '\nAvvio della dashboard (Ctrl+C per fermare il server).\n'
exec "$repo_root/target/release/orphanwiki" serve "$@"
