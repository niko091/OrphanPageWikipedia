#!/bin/sh
# Test the pipeline in a Toolforge job that uses the tool image, for example:
#   toolforge jobs run pipeline-test --image tool-NAME/tool-NAME:latest --mem 4Gi \
#       --mount=all --command "sh /workspace/scripts/toolforge-pipeline.sh 20261001 2026-09"
# Usage: sh scripts/toolforge-pipeline.sh SQL_RELEASE HISTORY_RELEASE [LANGUAGE...]
# The default languages are fur and co. The comparison step needs two or more.
# The pipeline keeps its work files in the container file system. These files
# are lost when the job stops. With --mount=all, the script copies the
# published collection to $TOOL_DATA_DIR, where the web service reads it.
set -eu
if [ $# -lt 2 ]; then
    echo 'Usage: sh scripts/toolforge-pipeline.sh SQL_RELEASE HISTORY_RELEASE [LANGUAGE...]' >&2
    exit 2
fi
sql_release=$1
history_release=$2
shift 2
[ $# -gt 0 ] || set -- fur co
# orphanwiki finds the repository root from the current directory.
cd -- "$(dirname -- "$0")/.."
model=data/models/johnson-outlink-20220727.bin
if [ ! -f "$model" ]; then
    url=$(sed -n 's/^ *"model_url": *"\([^"]*\)".*/\1/p' config/topics.json)
    [ -n "$url" ] || { echo 'config/topics.json: model_url missing' >&2; exit 1; }
    mkdir -p data/models
    temporary=$(mktemp data/models/model.XXXXXX)
    trap 'rm -f "$temporary"' EXIT HUP INT TERM
    curl --fail --location --proto '=https' --proto-redir '=https' --retry 3 \
        "$url" --output "$temporary"
    mv "$temporary" "$model"
    trap - EXIT HUP INT TERM
fi
set -- --languages "$@"
if [ -d /public/dumps/public ]; then
    # With --mount=all, read the dumps from the Toolforge copy. Do not download them.
    set -- --dump-mirror /public/dumps/public "$@"
fi
# The pipeline verifies the model checksum from config/topics.json.
target/release/orphanwiki pipeline --sql-release "$sql_release" \
    --history-release "$history_release" "$@"
if [ ! -d "${TOOL_DATA_DIR:-}" ]; then
    echo 'TOOL_DATA_DIR is not mounted. The collection stays in the container.' >&2
    exit 0
fi
# Copy to a temporary name first, so that readers never see a partial file.
destination=$TOOL_DATA_DIR/collection.sqlite
cp web/data/collection.sqlite "$destination.tmp"
mv "$destination.tmp" "$destination"
echo "Collection copied to $destination"
