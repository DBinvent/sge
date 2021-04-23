#!/bin/bash
if [ $# -lt 3 ]; then
  echo "Run a fresh instance of Postgres on port. Script params: "
  echo " 1: start | stop"
  echo " 2: working dir for postgres DB (like 'data')"
  echo " 3: port to bind and appending working folder name (like 5434)"
  echo " 4: database name to create (default: $USER)"
  echo " 5: optional version of postgres (default 12)"
  exit 1
fi

case $1 in
"start")

db=$4
if [ $# -lt 4 ]; then
  db=$USER
fi

pver=$5
if [ $# -lt 5 ]; then
  pver=12 # default postgres version is 12
fi

# shellcheck disable=SC2046
wd=$(dirname $(realpath "$0"))/$2/$3
# shellcheck disable=SC2046
mkdir -p $(dirname "$0")/$2
pdir=$(dirname "$(find /usr  -type f -executable -name initdb | grep $pver | head -n 1)")
echo "Working dir for this DB[$db] instance set to $wd Postgres$pver bin folder is $pdir"
# find a initdb
cd "$pdir" || exit 1
# run if not
if [ ! -f "$wd/PG_VERSION" ]; then
  ./initdb "$wd"
fi

if [ ! -f "$wd/pg_hba.conf" ]; then
 echo "local all all trust" > "$wd/pg_hba.conf"
# echo "host all all 127.0.0.1/0 trust" >> "$wd/pg_hba.conf"
fi

# --listen_addresses="$2" \x

if [ ! -f "$wd/postmaster.pid" ]; then
  echo "Starting Postgres$pver on port $3. use '$0 stop $2 $3 && rm -rf $wd' to stop and clean it."
  ./postgres -p "$3" --hba_file="$wd/pg_hba.conf" -k "$wd" -D "$wd" &
  sleep 2
fi

;;

"stop")
# shellcheck disable=SC2046
wd=$(dirname $(realpath "$0"))/$2/$3
if [ -f "$wd/postmaster.pid" ]; then
# shellcheck disable=SC2046
 kill $(head -n 1 $wd/postmaster.pid)
else
 echo "no pid [$wd/postmaster.pid] to stop postgres"
fi
;;
esac