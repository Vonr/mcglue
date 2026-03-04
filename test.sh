#!/bin/sh

RUSTFLAGS="--cfg tokio_unstable" cargo build || exit 1

docker compose down
docker compose up -d || exit 1
docker compose attach server
