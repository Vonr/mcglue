#!/bin/sh

cargo build || exit 1

docker compose down
docker compose up -d || exit 1
docker compose attach server
