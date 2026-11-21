#!/usr/bin/env bash
set -euo pipefail

fixtures_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_dir=$(cd -- "$fixtures_dir/../.." && pwd)
data_dir="$repo_dir/test/data"
reference_dir="$repo_dir/test/reference"
blast_revision='f4b567e1248ae97f97f7c528e935c102da6bcd83'

"$fixtures_dir/fetch.sh"
mkdir -p -- "$reference_dir"
build_dir=''
dbf_stage=''
reference_stage=''
trap 'rm -rf -- ${build_dir:+"$build_dir"} ${dbf_stage:+"$dbf_stage"} ${reference_stage:+"$reference_stage"}' EXIT
build_dir=$(mktemp -d -- "$fixtures_dir/.build.XXXXXX")
dbf_stage=$(mktemp -d -- "$data_dir/.dbf.XXXXXX")
reference_stage=$(mktemp -d -- "$reference_dir/.reference.XXXXXX")

cargo build --locked --release --manifest-path "$fixtures_dir/dbc2dbf/Cargo.toml" \
    --target-dir "$build_dir/cargo"

(
    cd -- "$build_dir"
    while read -r digest filename; do
        curl --fail --silent --show-error --connect-timeout 10 --max-time 60 \
            --proto '=https' --output "$filename" \
            "https://raw.githubusercontent.com/eaglebh/blast-dbf/$blast_revision/$filename"
        printf '%s  %s\n' "$digest" "$filename" | sha256sum --check --strict
    done <<'CHECKSUMS'
2b2b18e10f4c0b95ed7a9862688aa14f1ef12d343a6ae585d6f13f522a20a6af  blast-dbf.c
34cad3a3c7081ee0e329b74f4b648d5404094fb804b50d7b30cd4f1b4aad8c2e  blast.c
825e2f90c605133c96dc134ace6b62b70cc08156c7306a299bc8847f8fbb8ecb  blast.h
CHECKSUMS
    cc -O2 -o blast-dbf blast-dbf.c blast.c
)

dbfs=()
while read -r _ filename; do
    stem=${filename%.dbc}
    dbc="$data_dir/$filename"
    rust_dbf="$dbf_stage/$stem.dbf"
    blast_dbf="$dbf_stage/$stem.blast.dbf"

    "$build_dir/cargo/release/fixture-dbc2dbf" "$dbc" "$rust_dbf"
    "$build_dir/blast-dbf" "$dbc" "$blast_dbf"
    if ! cmp -- "$rust_dbf" "$blast_dbf"; then
        printf 'DBF decompressors disagreed for %s (Rust vs C)\n' "$filename" >&2
        wc -c -- "$rust_dbf" "$blast_dbf" >&2
        exit 1
    fi
    printf '%s: Rust and C outputs match byte for byte\n' "$filename"
    dbfs+=("$rust_dbf")
done < "$fixtures_dir/SHA256SUMS"

uv run --no-project --script "$fixtures_dir/write-reference.py" \
    --data-dir "$data_dir" --output-dir "$reference_stage" "${dbfs[@]}"
for dbf in "${dbfs[@]}"; do
    mv -T -- "$dbf" "$data_dir/${dbf##*/}"
done
for output in "$reference_stage"/*; do
    mv -T -- "$output" "$reference_dir/${output##*/}"
done
