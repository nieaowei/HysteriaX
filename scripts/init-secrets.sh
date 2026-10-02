#!/bin/sh
set -eu

if [ -e .env ]; then
  echo ".env already exists; refusing to overwrite it" >&2
  exit 1
fi

domain=${1:-}
if [ -z "$domain" ]; then
  echo "Usage: scripts/init-secrets.sh management.example.com" >&2
  exit 2
fi

admin_token=$(openssl rand -base64 32 | tr -d '\n=')
master_key=$(openssl rand -base64 32 | tr -d '\n=')
database_admin_password=$(openssl rand -hex 32)
database_password=$(openssl rand -hex 32)
service_uid=$(id -u)
service_gid=$(id -g)
umask 077
mkdir -p backups
chmod 700 backups
cat > .env <<EOF
HYSTERIAX_DOMAIN=$domain
HYSTERIAX_ADMIN_TOKEN=$admin_token
HYSTERIAX_MASTER_KEY=$master_key
HYSTERIAX_DB_ADMIN_PASSWORD=$database_admin_password
HYSTERIAX_DB_USER=hysteriax
HYSTERIAX_DB_PASSWORD=$database_password
HYSTERIAX_UID=$service_uid
HYSTERIAX_GID=$service_gid
HYSTERIAX_VERSION=dev
HYSTERIAX_IMAGE=hysteriax-server
EOF
echo "Created .env with a 256-bit administrator token and encryption key. Store a separate backup of HYSTERIAX_MASTER_KEY."
