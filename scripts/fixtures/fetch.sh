#!/usr/bin/env bash
set -euo pipefail

fixtures_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
data_dir="$fixtures_dir/../../test/data"
ftp_dir='ftp://ftp.datasus.gov.br/dissemin/publicos/SIHSUS/200801_/Dados'

mkdir -p -- "$data_dir"
cd -- "$data_dir"
partial=''
trap 'if [[ -n "$partial" ]]; then rm -f -- "$partial"; fi' EXIT

while read -r digest filename; do
    if [[ ! "$digest" =~ ^[a-f0-9]{64}$ || ! "$filename" =~ ^RD[A-Z]{2}[0-9]{4}\.dbc$ ]]; then
        printf 'Invalid fixture checksum entry: %s %s\n' "$digest" "$filename" >&2
        exit 1
    fi
    if [[ -e "$filename" || -L "$filename" ]]; then
        if [[ ! -f "$filename" || -L "$filename" ]]; then
            printf 'Expected a regular fixture file: %s\n' "$filename" >&2
            exit 1
        fi
        if ! printf '%s  %s\n' "$digest" "$filename" | sha256sum --check --strict --status; then
            printf 'DATASUS fixture checksum mismatch for existing %s (left in place for inspection)\n' "$filename" >&2
            sha256sum -- "$filename" >&2
            exit 1
        fi
        printf '%s: present, checksum OK\n' "$filename"
        continue
    fi

    partial=$(mktemp -- ".$filename.XXXXXX")
    printf 'Downloading %s from DATASUS FTP\n' "$filename"
    curl --fail --silent --show-error --connect-timeout 10 --max-time 600 \
        --proto '=ftp' --output "$partial" "$ftp_dir/$filename"
    if ! printf '%s  %s\n' "$digest" "$partial" | sha256sum --check --strict --status; then
        printf 'DATASUS fixture checksum mismatch: %s\n' "$filename" >&2
        sha256sum -- "$partial" >&2
        exit 1
    fi
    mv -T -- "$partial" "$filename"
    partial=''
done < "$fixtures_dir/SHA256SUMS"
