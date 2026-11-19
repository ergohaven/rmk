#!/usr/bin/env bash
# Install workflow-owned packages without accepting an incomplete index refresh.
set -euo pipefail

if (( $# == 0 )); then
    printf 'Usage: bash .github/ci/install-system-deps.sh PACKAGE...\n' >&2
    exit 2
fi

# Hosted images use short acquire deadlines; the calling step bounds the total.
acquire=(
    -o Acquire::Retries=3
    -o Acquire::http::Timeout=60
    -o Acquire::https::Timeout=60
)

sudo apt-get "${acquire[@]}" -o APT::Update::Error-Mode=any update
sudo apt-get "${acquire[@]}" install -y --no-install-recommends -- "$@"
