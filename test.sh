#!/bin/sh

cargo build || exit 1

docker compose down
docker compose up -d

mkfifo test-fifo
cargo run -- test-fifo | docker compose attach server > test-fifo
unlink test-fifo

