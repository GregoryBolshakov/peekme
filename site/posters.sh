#!/bin/sh
# Poster for each demo video: its first frame, so the switch from poster to
# playback shows no jump. Run after replacing any static/media/*.mp4.
# The plain <name>.webp next to it is a still from the middle (agent cards, og images).
set -eu
cd "$(dirname "$0")/static/media"
for f in *.mp4; do
  ffmpeg -v error -y -i "$f" -frames:v 1 -c:v libwebp -quality 90 -compression_level 6 "${f%.mp4}-start.webp"
done
