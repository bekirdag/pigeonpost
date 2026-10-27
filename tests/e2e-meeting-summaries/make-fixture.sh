#!/bin/sh
# Builds a synthetic three-speaker meeting from macOS text-to-speech voices.
# Nobody's real voice is used. Output: one 16 kHz mono WAV per speaker (each
# speaker's own "microphone": their lines, silence while others talk) plus a
# mixed file, in $1 (default ./fixture).
set -eu
out=${1:-fixture}
mkdir -p "$out/lines"
# speaker|voice|text
cat > "$out/script.txt" <<'S'
ada|Samantha|Good morning everyone. This is the weekly release meeting for the Lighthouse project. We need to decide on the launch date and who prepares the release notes.
ben|Daniel|Thanks Ada. The build is green since Tuesday. I propose we launch on Thursday the twelfth of November.
ada|Samantha|Thursday works for me. Let's decide that the launch is on Thursday the twelfth of November.
deniz|Yelda|Merhaba. Türkçe çeviriler hazır. Ben sürüm notlarını cuma gününe kadar hazırlayacağım.
ben|Daniel|Great. I will update the status page and send the customer announcement on Wednesday.
ada|Samantha|One open question remains. Do we need a second review of the billing screens? Let's come back to that next week.
deniz|Yelda|Tamam. Bir sonraki toplantıda konuşalım. Teşekkürler.
ben|Daniel|Thank you all. That's everything for today.
S
i=0
: > "$out/timeline.txt"
while IFS='|' read -r who voice text; do
  i=$((i+1))
  say -v "$voice" -o "$out/lines/$i.aiff" "$text"
  ffmpeg -loglevel error -y -i "$out/lines/$i.aiff" -ar 16000 -ac 1 "$out/lines/$i.wav"
  echo "$i $who" >> "$out/timeline.txt"
done < "$out/script.txt"
# Build each speaker's track: their line where they speak, silence (same length) otherwise,
# with 0.6 s gaps between turns.
for who in ada ben deniz; do
  args=""; n=0; filt=""
  while read -r idx spk; do
    if [ "$spk" = "$who" ]; then src="$out/lines/$idx.wav"; else
      d=$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$out/lines/$idx.wav")
      ffmpeg -loglevel error -y -f lavfi -i "anullsrc=r=16000:cl=mono" -t "$d" "$out/lines/silence-$idx.wav"
      src="$out/lines/silence-$idx.wav"; fi
    args="$args -i $src -f lavfi -t 0.6 -i anullsrc=r=16000:cl=mono"
    filt="$filt[$n:a][$((n+1)):a]"; n=$((n+2))
  done < "$out/timeline.txt"
  # shellcheck disable=SC2086
  ffmpeg -loglevel error -y $args -filter_complex "${filt}concat=n=$n:v=0:a=1[o]" -map "[o]" -ar 16000 -ac 1 "$out/$who.wav"
done
ffmpeg -loglevel error -y -i "$out/ada.wav" -i "$out/ben.wav" -i "$out/deniz.wav" \
  -filter_complex "amix=inputs=3:normalize=0" -ar 16000 -ac 1 "$out/mixed.wav"
for f in ada ben deniz mixed; do
  printf '%s %ss\n' "$f" "$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$out/$f.wav")"
done
