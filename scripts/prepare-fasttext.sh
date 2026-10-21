#!/bin/sh
# Run from the repository root. This fetches library source, never model weights.
set -eu
archive=.cache/fasttext-0.9.3.tar.gz
checksum=eb03f2ef6340c6ac9e4398a30026f05471da99381b307aafe2f56e4cd26baaef
verify() { printf '%s  %s\n' "$checksum" "$1" | sha256sum --check --status; }
if test -f "$archive" && verify "$archive"; then
    exit 0
fi
mkdir -p .cache
temporary=$(mktemp .cache/fasttext-0.9.3.XXXXXX)
trap 'rm -f "$temporary"' EXIT HUP INT TERM
curl --fail --location --proto '=https' --proto-redir '=https' --retry 3 \
    https://files.pythonhosted.org/packages/9f/3b/9a10b95eaf565358339162848863197c3f0a29b540ca22b2951df2d66a48/fasttext-0.9.3.tar.gz \
    --output "$temporary"
verify "$temporary"
mv "$temporary" "$archive"
