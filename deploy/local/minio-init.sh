#!/bin/sh
set -eu

mc alias set local http://minio:9000 "$MINIO_ROOT_USER" "$MINIO_ROOT_PASSWORD"
mc mb --ignore-existing "local/$MINIO_BUCKET"
mc version enable "local/$MINIO_BUCKET"
mc anonymous set download "local/$MINIO_BUCKET"

# Compose --wait expects every selected service to remain running.
exec tail -f /dev/null
