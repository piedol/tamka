#!/bin/zsh
# Run every contender over clips/ one after another, then print the scoreboard.
#   ./bench.sh                     all contenders
#   ./bench.sh parakeet-v2 apple   only these (names below)
#   SET=public ./bench.sh          use sets/public/ instead of clips/
set -e
cd "${0:A:h}"

# Whisper baseline: fetched once into eval/models/ (git-ignored) when it's selected.
WHISPER_MODEL=models/ggml-large-v3-turbo-q8_0.bin
whisper_model() {
  if [[ ! -f $WHISPER_MODEL ]]; then
    mkdir -p models
    curl -L --fail -o $WHISPER_MODEL https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q8_0.bin
  fi
}
SW=../engine/.build/release/tamka-engine
if [[ ! -x $SW ]]; then
  (cd ../engine && swift build -c release)
fi

typeset -A CONTENDERS
CONTENDERS=(
  whisper-turbo           "whispercpp --model $WHISPER_MODEL"
  parakeet-v2             "worker -- $SW parakeet v2"
  parakeet-v2-boost       "worker -- $SW parakeet v2 --boost-safe"
  parakeet-v3             "worker -- $SW parakeet v3"
  parakeet-ultra          "worker -- $SW parakeet ultra"
  parakeet-ultra-boost    "worker -- $SW parakeet ultra --boost-safe"
  parakeet-phonon2        "worker -- $SW parakeet phonon2"
  apple-transcriber       "worker -- $SW apple"
  apple-dictation         "worker -- $SW apple-dictation"
  qwen3-asr-1.7b          "worker -- uv run -q engines/mlx_worker.py qwen3 mlx-community/Qwen3-ASR-1.7B-8bit"
  qwen3-asr-0.6b          "worker -- uv run -q engines/mlx_worker.py qwen3 mlx-community/Qwen3-ASR-0.6B-8bit"
  granite-5-2b            "worker -- uv run -q engines/mlx_worker.py granite ibm-granite/granite-speech-5.0-2b"
  granite-5-2b-keywords   "worker -- uv run -q engines/mlx_worker.py granite ibm-granite/granite-speech-5.0-2b --keywords"
)
ORDER=(whisper-turbo parakeet-v2 parakeet-v2-boost parakeet-v3 parakeet-ultra parakeet-ultra-boost parakeet-phonon2
       apple-transcriber apple-dictation qwen3-asr-1.7b qwen3-asr-0.6b granite-5-2b granite-5-2b-keywords)

selected=(${@:-$ORDER})
setarg=()
[[ -n $SET ]] && setarg=(--set $SET)
for name in $selected; do
  spec=${CONTENDERS[$name]}
  if [[ -z $spec ]]; then echo "unknown contender: $name"; exit 1; fi
  [[ $name == whisper-turbo ]] && whisper_model
  echo "\n=== $name"
  eval "uv run -q run.py $setarg ${spec%% *} --name $name ${spec#* }" 2> >(grep -v -E "E5RT|Fetching|it/s\]" >&2)
done

echo
uv run -q score.py $setarg 2> >(grep -v -E "SyntaxWarning|split_letters" >&2)
