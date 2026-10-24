#!/bin/zsh
# Record one evaluation clip: ./record.sh 07-client-email
# Records 16 kHz mono WAV from the default mic (or $MIC) until you press q,
# then opens the matching transcript file in TextEdit.
set -e

name="$1"
if [[ -z "$name" ]]; then
  echo "Usage: ./record.sh <clip-name>   e.g. ./record.sh 07-client-email"
  exit 1
fi

dir="${0:A:h}/clips"
mkdir -p "$dir"
wav="$dir/$name.wav"
txt="$dir/$name.txt"

if [[ -e "$wav" ]]; then
  read "reply?$name.wav already exists. Overwrite? [y/N] "
  [[ "$reply" == [yY] ]] || exit 1
fi

if [[ -z "$MIC" ]]; then
  MIC=$(system_profiler SPAudioDataType 2>/dev/null \
    | awk '/^        [^ ].*:$/{n=$0} /Default Input Device: Yes/{print n}' \
    | sed 's/^ *//; s/:$//')
fi

echo "Recording from: $MIC"
echo "Speak now. Press q to stop."
ffmpeg -hide_banner -loglevel error -f avfoundation -i ":$MIC" -ac 1 -ar 16000 -c:a pcm_s16le -y "$wav"

secs=$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$wav")
printf "Saved %s (%.1f s)\n" "$wav" "$secs"

[[ -e "$txt" ]] || : > "$txt"
open -e "$txt"
echo "Type exactly what you said into the TextEdit window, then save."
