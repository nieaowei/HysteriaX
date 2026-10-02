#!/bin/sh
set -eu

: "${HYSTERIAX_DB_USER:?HYSTERIAX_DB_USER is required}"
: "${HYSTERIAX_DB_PASSWORD:?HYSTERIAX_DB_PASSWORD is required}"

psql -v ON_ERROR_STOP=1 \
  --username "$POSTGRES_USER" \
  --dbname "$POSTGRES_DB" \
  --set=app_user="$HYSTERIAX_DB_USER" \
  --set=app_password="$HYSTERIAX_DB_PASSWORD" <<'SQL'
SELECT format('CREATE ROLE %I LOGIN PASSWORD %L', :'app_user', :'app_password') \gexec
SELECT format('ALTER DATABASE %I OWNER TO %I', current_database(), :'app_user') \gexec
SQL
