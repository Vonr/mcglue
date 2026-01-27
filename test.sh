#!/bin/sh

cargo build || exit 1

docker compose down
docker compose up -d

cargo run -- docker compose attach server
