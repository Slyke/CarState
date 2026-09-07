#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
: "${USERNAME:?Set USERNAME to the registry namespace}"
: "${DOMAIN:?Set DOMAIN to the second registry host}"
IMAGE_NAME=carstate
VERSION="v$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
if [[ -n "$(git status --porcelain)" ]]; then
  echo 'Release requires a clean, committed working tree.' >&2
  exit 1
fi
SHA=$(git rev-parse --short=12 HEAD)
if [[ "$(git rev-parse "$VERSION^{commit}")" != "$(git rev-parse HEAD)" ]]; then
  echo 'Create the release tag on the exact current commit before publishing.' >&2
  exit 1
fi
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
docker build --target production --build-arg "BUILD_HASH=$SHA" --build-arg CARSTATE_RELEASE=true -t "$IMAGE_NAME:build" .
for TAG in latest "$VERSION" "$VERSION-$SHA"; do
  docker tag "$IMAGE_NAME:build" "$USERNAME/$IMAGE_NAME:$TAG"
  docker tag "$IMAGE_NAME:build" "$DOMAIN/$USERNAME/$IMAGE_NAME:$TAG"
  docker push "$USERNAME/$IMAGE_NAME:$TAG"
  docker push "$DOMAIN/$USERNAME/$IMAGE_NAME:$TAG"
done
