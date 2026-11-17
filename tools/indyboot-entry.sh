#!/usr/bin/env bash
# Print the manifest section for one image, with its sha256 and size.
#
# Usage:
#   tools/indyboot-entry.sh NAME TITLE IMAGE_FILE [ARGS...] > indyboot.cfg
#
# NAME    section name: letters, digits, '.', '_' and '-', at most 32
# TITLE   menu text, at most 60 bytes
# IMAGE_FILE  the ECOFF netboot image. The section refers to it by its file
#         name, so publish indyboot.cfg in the same directory as the image.
# ARGS    words passed to the image as argv[1..], at most 120 bytes in total
#
# Run it once per image and concatenate the output for several entries.
set -euo pipefail

if [ "$#" -lt 3 ]; then
  sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//' >&2
  exit 2
fi
NAME="$1"
TITLE="$2"
IMAGE="$3"
shift 3

# 0x0160 is the magic of a big-endian MIPS ECOFF file.
MAGIC="$(od -An -N2 -tx1 "$IMAGE" | tr -d ' \n')"
if [ "$MAGIC" != "0160" ]; then
  echo "indyboot-entry: $IMAGE is not a big-endian MIPS ECOFF file (magic $MAGIC, want 0160)" >&2
  exit 1
fi

echo "[$NAME]"
echo "title = $TITLE"
echo "image = $(basename "$IMAGE")"
echo "sha256 = $(sha256sum "$IMAGE" | cut -d' ' -f1)"
echo "size = $(stat -c %s "$IMAGE")"
if [ "$#" -gt 0 ]; then
  echo "args = $*"
fi
echo
