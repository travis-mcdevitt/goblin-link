# Program a wake phrase from English IPA

Write the pronunciation, then compile it into a detector. A recipe needs no training audio, spelling dictionary, gradients or fitted phrase embedding. The output can be a single ONNX file that accepts mono 16 kHz signed 16-bit PCM and scores several phrases in one acoustic pass. All measurements in this experiment used locally generated synthetic audio.

## Compile a detector

From the repository root on Linux with Docker, build the CPU runtime. The build downloads the pinned 4-bit phoneme checkpoint and checks its SHA-256. Compilation and inference then work offline.

```sh
lab="$PWD/experiments/hey-goblin"
ipa="$lab/ipa"
out="$ipa/target/wake"
mkdir -p "$out"
docker build -t goblin-link/ipa-runtime:120 - < "$ipa/Dockerfile.portable"

runtime() {
  docker run --rm --network none --user "$(id -u):$(id -g)" \
    -v "$ipa:/src:ro" -v "$out:/out" -w /src goblin-link/ipa-runtime:120 "$@"
}
runtime python bank.py focused.json /out/goblin-hermes-balanced.onnx
runtime python bank.py recipes.json /out/demo.onnx
```

The first command builds the two requested phrases with their pronunciation variants and rivals. Start with this balanced model for better Hermes sensitivity. The second command builds a pronunciation-only comparison.

The demo contains **Hey Goblin** /heɪ ˈɡɑblɪn/, **Hey Hermes** /heɪ ˈhɝmiz/ and **Blue Lantern** /bluː ˈlæntɚn/. The first two supplied development examples. Blue Lantern was reserved for transfer. The default bank is 241,349,161 bytes and took 3.07 seconds to build on this host with the image cached.

To program a new phrase, put its display name and IPA in a JSON object. Only the IPA determines the weights. A label can be a name for a made-up word. Add another entry to detect both phrases in one file.

```sh
cat > "$out/my-phrases.json" <<'JSON'
{
  "My wake phrase": {"ipa": "/ˈmɪpə zuːv/"},
  "Hey Hermes": {"ipa": "/heɪ ˈhɝmiz/"}
}
JSON
runtime python bank.py /out/my-phrases.json /out/custom.onnx
```

For an existing shared encoder, compile just the phrase head. The three example heads in `models/` are 41–55 KiB. They consume the 392-column logits of the pinned phoneme model. Head compilation itself requires only Python, NumPy and ONNX.

```sh
runtime python compile_ipa.py '/heɪ ˈɡɑblɪn/' /out/goblin-head.onnx \
  --variant '/heɪ ˈɡɑblən/' --reject '/heɪ ˈɡɑblɪŋ/'
```

## Choose the pronunciation

| Input | Supported broad English sounds |
| --- | --- |
| Consonants | p b t d k ɡ f v θ ð s z ʃ ʒ h tʃ dʒ m n ŋ l ɹ j w |
| Vowels | i ɪ eɪ ɛ æ ɑ ɒ ɔ oʊ ʊ u ʌ ə ɚ aɪ aʊ ɔɪ |
| Common conventions | /ɝ/ and /ɜː/, British /əʊ/, length marks on supported vowels, dark /ɫ/, /ɾ/, syllabic consonants, tied affricates, ordinary g and r |

Use slashes, brackets or bare IPA. Spaces, stress and syllable dots aid transcription. The detector scores ordered sounds. The synthesizer also uses stress and word breaks. The compiler recognizes common joined acoustic labels such as /əl/ and /ɔːɹ/ alongside the split-phone paths. It bounds automatic alternatives to eight segmentations per transcription. An unsupported symbol produces an error identifying its position. Each pronunciation accepts up to 48 acoustic phones and a bank accepts up to 32 named phrases. The complete spelling and acoustic mappings are in [phones.py](phones.py).

For another accent or a reduced vowel, add a `variants` array. For a known near miss, add its pronunciation to `reject`. The positive path must beat the best nearby rival by more than 0.15 mean score units. Include the sound that distinguishes the two utterances. A bare noun used as a rival can also match the end of a genuine wake. Each head accepts up to eight competitor pronunciations and seven additional positive pronunciations.

```json
{
  "Brown Goose": {
    "ipa": "/bɹaʊn ɡuːs/",
    "reject": ["/bɹaʊn dʒuːs/"]
  }
}
```

The stricter `english-wakes.json` also suppresses “Hi Hermes” and “OK Hermes.” Compile it with `runtime python bank.py english-wakes.json /out/hey-goblin-hermes.onnx`. The resulting model is 241,641,748 bytes, retains 129 acoustic output rows, and built in 2.26 seconds on this host. Its SHA-256 is `add7b2618e7ec61ec18f621ea407ab0879d4f6554e0f0eece658a7666d4cc222`. The precompiled strict heads are available individually. [Hey Goblin](models/final/hey_goblin.onnx) · [Hey Hermes](models/final/hey_hermes.onnx)

Keep a simple pronunciation-only bank for a paired comparison. Measure plausible confusions, inflections and target-plus-command utterances on new voices before choosing an operating recipe. [Synthetic results](RESULTS.md)

## Generate a synthetic WAV and run it

The lab image adds Piper, eSpeak NG, Flite, the original full-precision phoneme model and the earlier spelling baseline. The compact runtime above is sufficient to build and run the detector.

```sh
docker build -t goblin-link/phrase-lab:118 "$lab"
docker build -t goblin-link/ipa-lab:120 - < "$ipa/Dockerfile"

lab_run() {
  docker run --rm --network none --user "$(id -u):$(id -g)" --tmpfs /tmp -e HOME=/tmp \
    -e PYTHONPATH=/lab:/opt/sdk/linux/src -v "$lab:/lab:ro" -v "$out:/out" \
    -w /lab/ipa goblin-link/ipa-lab:120 "$@"
}
lab_run python synth_ipa.py '/heɪ ˈɡɑblɪn/' /out/goblin.wav
runtime python detect.py /out/goblin.wav --bank /out/demo.onnx
```

Each event is one JSON line with the phrase, estimated acoustic endpoint, the amount of audio received when the decision became available, and the score. Times are seconds from the beginning of the WAV. The runner feeds actual PCM in chunks, waits for 18 future frames of suffix evidence, and applies a one-second cooldown per phrase. It recomputes a bounded window every 480 ms because the acoustic encoder uses bidirectional attention. The minimum window is 2.4 s and the window grows with the longest pronunciation. At end of input it uses only audio already supplied. The window must contain the spoken phrase and its right context.

For a custom IPA-only trial, pass the very same pronunciation to `synth_ipa.py` and `bank.py`. The synthesizer translates IPA to eSpeak NG phoneme input directly. Add `--piper-speaker 810` to synthesize through a neural Piper voice with literal phonemes. This also works for pronunciations with no dictionary entry. For strength across synthesis engines, the evaluation separately uses ordinary written prompts through Piper, eSpeak and Flite.

## Qualify a new phrase

Add close IPA confusions to the recipe's `controls` array. Reserve additional confusions in `test_controls`. These two fields provide test prompts. Only `reject` writes competing paths into the weights. The harness also tests an inflection, the remaining words after dropping the first word, reversed word order where applicable, and the other phrases in the recipe.

Run from the repository root after building the lab image. Keep the recipe under the worktree so the test container can read it. For a large bank, set `GOBLIN_IPA_NAME` to score one named head per invocation.

```sh
GOBLIN_IPA_RECIPE="$out/my-phrases.json" GOBLIN_TEST_IMAGE=goblin-link/ipa-lab:120 \
  tools/test-in-container.sh --manifest-path experiments/hey-goblin/ipa/Cargo.toml \
  --locked -- --ignored qualify_ipa --nocapture
```

Read the JSON line for each phrase. It reports hits and false wakes at the starting cutoff, and tests a candidate cutoff on six separate confirmation voices. The candidate was chosen on six development voices from a fixed grid, maximizing hits under a 5% empirical false-wake limit on the supplied controls. A null candidate means no tested cutoff met the limit with any development hits. A low confirmation hit count is a reason to revise the phrase or its pronunciations. The test leaves your recipe untouched. To adopt a measured operating point, add a numeric `cutoff` to that phrase and rebuild. This changes decision metadata. The emission and transition tensors are identical.

For stationary background noise, the runner also accepts `--denoise`. This applies spectral subtraction to the PCM window already available. This optional front end is independent of the IPA weights. The results record its effect on fresh speech. [Synthetic results](RESULTS.md)

## What the compiler writes

The English phone IDs are taken from the acoustic checkpoint's own vocabulary. For each pronunciation the compiler writes a phone-to-state emission matrix and a state-to-state transition matrix. Exact phone evidence has weight 1, alternatives receive smaller positive weights, and a prohibited transition has score −10,000. A max-plus CTC scan accumulates the best ordered path. Separate numerical paths enforce a terminal phone, check a short following suffix and, when requested, compare confusable pronunciations ending nearby. The shared cutoff and scalar costs were selected on development voices.

The single-file compiler also edits the pretrained acoustic output projection itself. It selects the packed 4-bit weight rows, scale blocks and biases used by English competition and the requested phrase paths, rewrites the output width and remaps the programmed matrices. The three-phrase demo retains 120 of 392 rows. The rest of the shared acoustic encoder retains its pretrained parameters. The ONNX graph embeds waveform normalization and outputs a frame-by-phrase score matrix. The host reads the decision cutoff and phrase order from model metadata. [Compiler](compile_ipa.py) · [Checkpoint specialization](bank.py)

This is direct weight construction for a phonetic state machine, with projection specialization for the integrated model. The underlying techniques are CTC alignment and weighted automata. Tracr provides a related example of compiling programs into numerical weights. The phoneme recognizer supplies the trained mapping from audio to phone evidence. The Muse checkpoint has a different output space and runtime. The earlier native Muse experiment and spelling-based comparison have their own runbook. [Weighted automata in speech recognition](https://cs.nyu.edu/~mohri/pub/csl01.pdf) · [CTC](https://www.cs.toronto.edu/~graves/icml_2006.pdf) · [Tracr](https://arxiv.org/abs/2301.05062) · [Phoneme recognition paper](https://arxiv.org/abs/2109.11680) · [Muse experiment](../README.md)

## Reproduce the measurements

Use fresh output directories for the caches. The preparer saves each synthetic waveform and its logits from three backbones together. It publishes the manifest after all files have been written. The comparisons check the source fingerprint and the three checked-in heads against a fresh compilation.

```sh
work="$ipa/target/work"
mkdir -p "$work"
for split in development holdout; do
  docker run --rm --network none --user "$(id -u):$(id -g)" --tmpfs /tmp -e HOME=/tmp \
    -e PYTHONPATH=/lab:/opt/sdk/linux/src -v "$lab:/lab:ro" -v "$work:/work" \
    -w /lab/ipa goblin-link/ipa-lab:120 python prepare.py "$split" "/work/$split"
done

check() {
  GOBLIN_TEST_IMAGE=goblin-link/ipa-lab:120 tools/test-in-container.sh \
    --manifest-path experiments/hey-goblin/ipa/Cargo.toml --locked -- --nocapture "$@"
}
check --test-threads=1
for suite in tune_development development_confusers mixed_events confirm_inventory_a \
  confirm_inventory_b nonce_phrases piper_pseudowords noise_probe focused_a focused_b \
  focused_stream prefix_a prefix_b prefix_stream qualify_ipa cpu_benchmark; do
  check --ignored "$suite"
done
for shard in 0 1 2 3 4 5; do check --ignored "longform_$shard"; done
```

The default checks include numeric parity against an independent NumPy recurrence, a comparison of integrated and separate inference, a synthetic WAV through the CLI, chunk-boundary behavior and the same-audio spelling comparison. The additional suites cover independent phrases and speakers, explicit competitors, pseudowords, streaming latency and continuous false wakes. Synthetic speakers and controls have correlated errors. Report counts with their denominators.

## Model sources and licenses

Meta's `facebook/wav2vec2-lv-60-espeak-cv-ft` checkpoint and the byte-identical vocabulary copied into `phoneme_vocab.json` are published under Apache 2.0. The CPU recipe uses the Apache-2.0 ONNX Community export of the same model, revision `c69750f5043e5e1f8a71ab95dd3b98338c280c92`. Its 4-bit weights have SHA-256 `f020386a4f730675b59f833af1581c7a0fa2f1e1c48adcf526e5c3c96115f772`. The lab checks the original Meta weights, configuration and vocabulary at revision `ae45363bf3413b374fecd9dc8bc1df0e24c3b7f4`. The bank is a modified export with the projection and phrase weights described above. The previous experiment documents the separate speech synthesis and Muse resources. [Meta model card](https://huggingface.co/facebook/wav2vec2-lv-60-espeak-cv-ft/tree/ae45363bf3413b374fecd9dc8bc1df0e24c3b7f4) · [ONNX export](https://huggingface.co/onnx-community/wav2vec2-lv-60-espeak-cv-ft-ONNX/tree/c69750f5043e5e1f8a71ab95dd3b98338c280c92) · [Apache 2.0 license](../../../LICENSE) · [Prior experiment](../README.md)
