#!/usr/bin/env bash

set -ex

echo "Install latest dependencies and build both maker and taker frontends"

cd maker-frontend
yarn install
yarn build
cd ..
cd taker-frontend
yarn install
yarn build
cd ..
echo "Maker and taker frontends built successfully"
