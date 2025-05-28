#!/bin/bash
set -euo pipefail

if [[ -z "${TO_GATHER:-}" ]]; then
  echo "ERROR: TO_GATHER is not set. Please specify it, e.g. TO_GATHER=\"curl,btrfs,zfs\"" >&2
  exit 1
fi

OUTPUT_DIR="${OUTPUT_DIR:-/build/gather_deps}"
mkdir -p "${OUTPUT_DIR}/usr/bin" "${OUTPUT_DIR}/usr/lib" "${OUTPUT_DIR}/lib"

# Convert TO_GATHER var to array
IFS=',' read -ra BINARIES <<< "${TO_GATHER}"

for bin in "${BINARIES[@]}"; do
  # Gather binaries
  bin="$(echo -n "$bin" | xargs)" # trim whitespace
  bin_path="$(command -v "$bin" || true)"
  if [[ -z "$bin_path" ]]; then
    echo "ERROR: binary '$bin' not found in \$PATH" >&2
    exit 1
  fi

  echo "Collecting binary: $bin_path"
  cp --preserve=links "$bin_path" "${OUTPUT_DIR}/usr/bin/"

  # Gather dependencies
  ldd "$bin_path" | awk '/=>/ {print $3}' | while read -r lib; do
    case "$lib" in
      /usr/lib/*)
        cp -n --preserve=links "$lib" "${OUTPUT_DIR}/usr/lib/" ;;
      /lib/*)
        cp -n --preserve=links "$lib" "${OUTPUT_DIR}/lib/" ;;
    esac
  done

done

echo "[gather.sh] Dependencies collected in ${OUTPUT_DIR}"