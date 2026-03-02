#!/bin/sh

cargo build || exit 1

docker compose up
