#!/usr/bin/env bash
set -euo pipefail

fixtures_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
manifest="$fixtures_dir/families"
data_dir="$fixtures_dir/../../test/data/families"
ftp_dir='ftp://ftp.datasus.gov.br/dissemin/publicos'

mkdir -p -- "$data_dir"
cd -- "$data_dir"
partial=''
trap 'if [[ -n "$partial" ]]; then rm -f -- "$partial"; fi' EXIT

verify_fixture() {
    local digest=$1 file=$2 label=$3
    if ! printf '%s  %s\n' "$digest" "$file" | sha256sum --check --strict --status; then
        printf 'DATASUS fixture checksum mismatch for %s\n' "$label" >&2
        sha256sum -- "$file" >&2
        exit 1
    fi
}

while IFS=$'\t' read -r family path; do
    filename=${path##*/}
    digest=$(awk -v name="$filename" '$2 == name {print $1}' "$manifest/SHA256SUMS")
    if [[ -z "$family" || ! "$path" =~ ^[A-Za-z0-9_/]+/[A-Z]{4}[0-9]{4}\.dbc$ || ! "$digest" =~ ^[a-f0-9]{64}$ ]]; then
        printf 'Invalid DATASUS family fixture entry: %s %s\n' "$family" "$path" >&2
        exit 1
    fi
    if [[ -e "$filename" || -L "$filename" ]]; then
        if [[ ! -f "$filename" || -L "$filename" ]]; then
            printf 'Expected a regular fixture file: %s\n' "$filename" >&2
            exit 1
        fi
        verify_fixture "$digest" "$filename" "$filename (existing file left in place)"
        printf '%s: present, checksum OK\n' "$filename"
        continue
    fi

    partial=$(mktemp -- ".$filename.XXXXXX")
    printf 'Downloading %s (%s) from DATASUS FTP\n' "$filename" "$family"
    curl --fail --silent --show-error --connect-timeout 10 --max-time 600 \
        --proto '=ftp' --output "$partial" "$ftp_dir/$path"
    verify_fixture "$digest" "$partial" "$filename"
    mv -T -- "$partial" "$filename"
    partial=''
done < "$manifest/files.tsv"

sha256sum --check --strict "$manifest/SHA256SUMS"
