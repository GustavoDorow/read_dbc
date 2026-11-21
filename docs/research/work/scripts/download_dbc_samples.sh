#!/usr/bin/env bash
set -euo pipefail

cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
mkdir -p data
ftp_root=ftp://ftp.datasus.gov.br/dissemin/publicos
while IFS= read -r path; do
    curl -q -fsS --connect-timeout 10 --max-time 60 --remove-on-error \
        -o "data/${path##*/}" "$ftp_root/$path"
done <<'DATASUS_PATHS'
SIHSUS/200801_/Dados/RDAC0801.dbc
SIHSUS/200801_/Dados/RDAC2401.dbc
SIHSUS/200801_/Dados/RDRR2401.dbc
CNES/200508_/Dados/ST/STAC2401.dbc
SIM/CID10/DORES/DOAC2022.dbc
SINASC/1996_/Dados/DNRES/DNAC2022.dbc
SINASC/1996_/Dados/DNRES/DNAC1996.DBC
SINASC/1994_1995/Dados/DNRES/DNRAC1994.dbc
SINASC/1994_1995/Dados/DNRES/DNRRR1995.dbc
SINASC/ANT/DNRES/DNRRR95.DBC
SIASUS/200801_/Dados/PAAC2401.dbc
DATASUS_PATHS
