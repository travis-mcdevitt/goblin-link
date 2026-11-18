# Program a wake phrase

This experiment builds two detectors for each English phrase in `phrases.json`.
The examples are “Hey Goblin” and “Hey Hermes.”

| Output in `models/` | How the phrase gets into the weights | Runtime |
| --- | --- | --- |
| `.onnx` | The compiler writes a sequence of allowed letter emissions and transitions from the spelling. It can include alternate spellings of the same sounds. | ONNX Runtime with one shared 91 MiB quantized Wav2Vec2 English acoustic model. All phrase heads can score the same logits. |
| `.pte` | The editor writes fixed acoustic delays into the Muse checkpoint and fits its output weights and bias from synthetic target and contrast phrases. | The SDK's native 16 kHz streaming runner. Each complete detector is 23,492 bytes. |

The ONNX route is the direct weight-programming experiment. Generating a new
head takes no speech examples. The Muse route combines a hand-programmed edit
with supervised learning. Each route uses one pinned starting point for all
phrases. The two routes have independent acoustic models.

## Build and try a phrase

Run these commands from the Goblin Link repository root on Linux with Docker and
Cargo. The image build downloads and verifies the SDK checkpoint, its runtime
components, a Piper voice and the shared letter recognizer. Subsequent commands
run offline.

```sh
lab="$PWD/experiments/hey-goblin"
image=goblin-link/phrase-lab:118
docker build -t "$image" "$lab"
```

**For a spelling-only head**, supply the phrase and a destination. An optional
`--alias` adds another spelling the recognizer might assign to the same sounds.
The compiler accepts English letters, spaces and apostrophes, up to 80 characters.

```sh
mkdir -p "$lab/target/custom"
docker run --rm --network none --user "$(id -u):$(id -g)" \
  -v "$lab:/src:ro" -v "$lab/target/custom:/out" -w /src "$image" \
  python letters.py "Hey Lantern" /out/lantern.onnx --alias "Hay Lantern"
```

**To build and evaluate both types**, copy the recipe into a fresh case directory
under the worktree. Set `targets` to the spoken phrases. Put the bare names,
similar sounds and incomplete phrases in `near`. Add representative sentences to
`other`. Reserve different near misses and sentences in `test_near` and
`test_other`. An optional `aliases` object maps a target to alternative spellings.
Other targets and the previous “Hey Muse” phrase automatically act as negatives.
Use disjoint target and control strings. The model filenames include a digest of
the input name. The supplied recipe reproduces the two examples.

```sh
case_dir="$lab/target/my-phrases"
mkdir -p "$case_dir/models"
cp "$lab/phrases.json" "$case_dir/phrases.json"
# Edit "$case_dir/phrases.json" for a new case.
docker run --rm --network none --user "$(id -u):$(id -g)" --tmpfs /tmp \
  -e HOME=/tmp -e WAKE_RECIPE=/case/phrases.json -v "$lab:/src:ro" \
  -v "$case_dir:/case" -w /src "$image" python edit.py /case/models
GOBLIN_WAKE_CASE="$case_dir" GOBLIN_TEST_IMAGE="$image" tools/test-in-container.sh \
  --manifest-path experiments/hey-goblin/Cargo.toml --locked -- --nocapture
```

To check the committed models directly, run the last command with
`GOBLIN_WAKE_CASE` unset. The harness compiles on the host and executes inside
the local network-isolated container. It prints per-phrase detections,
cross-triggers, individual near misses, unrelated speech, nonspeech, and
detections in carrier sentences. Inspect those counts before adopting a phrase.
A passing smoke test allows substantial misses and false accepts.

## Synthetic results

The confirmation set used nine Piper speaker IDs, two eSpeak variants and one
Flite voice held out from design, calibration and the first benchmark. Each
target was spoken six times per voice with varied timing and level, including
colored noise at 6–16 dB SNR. Positive Muse events had to fall from 150 ms
before to 600 ms after the end of speech. CTC reports one decision per clip.

| Input | Clips | Stock Muse | Muse Goblin | Muse Hermes | Spelling Goblin | Spelling Hermes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Hey Goblin | 72 | 0 | 48 | 0 | 56 | 0 |
| Hey Hermes | 72 | 0 | 10 | 45 | 1 | 57 |
| Hey Muse | 24 | 21 | 0 | 1 | 0 | 0 |
| Near misses | 168 | 0 | 52 | 8 | 22 | 9 |
| Unrelated speech | 48 | 0 | 4 | 0 | 0 | 0 |
| Generated nonspeech | 16 | 0 | 0 | 0 | 0 | 0 |

“Hey goblins” triggered the spelling Goblin head on 8/12 clips and the Muse
Goblin model on 10/12. The spelling heads caught 3/3 carrier sentences each
(“I said [phrase] just now”). The Muse models caught 3/3 for Goblin and 0/3
for Hermes. On 474.9 seconds of concatenated synthetic negative speech the
two Muse models fired 37 and 7 times, respectively. Both yielded zero events
in 30 continuous minutes of generated nonspeech. This contrast makes speech
confusions the priority for further work. Real speakers, microphone audio,
handheld performance and real-world false wakes per hour remain unmeasured.

The earlier, separate 13-voice test used just the canonical spellings.
It yielded 49/78 Goblin and 40/78 Hermes for the spelling heads with 11/182
and 7/182 near-miss accepts. The Muse models caught 49/78 and 47/78 with
57/182 and 14/182 near-miss accepts. The confirmation set above was opened
after adding the alternate spellings. The two sets differ in voices, so their
difference alone cannot measure an alias effect.

## Use the weights

Inside the experiment image, the SDK's `WakeWordDetector(model_path=pte_path)`
accepts 16 kHz signed little-endian mono PCM bytes through `feed`. A true result
is a streaming wake event. Keep one instance per stream and phrase. The SDK
applies its five-score mean, default cutoff of 0.22 and two-second refractory
period.

For the ONNX route, import the helpers from `letters.py` in the experiment
directory. Here `pcm` is a mono 16 kHz NumPy int16 array of a short generated
utterance. The head returns the best phrase alignment anywhere in that clip.
The cutoff is shared by the example phrases. Continuous listening would also
need a buffering and repeat-trigger policy.

```python
from letters import BASE, CUTOFF, acoustic_logits, session

acoustic = session(BASE)
head = session("models/hey_hermes_67f37a59.onnx")
logits = acoustic_logits(acoustic, pcm)
score = head.run(None, {"logits": logits})[0].item()
detected = score >= CUTOFF
```

## What the edits do

**Spelling compiler.** The acoustic model outputs 32 scores per frame. Each
head column selects one letter, a CTC blank, or a word boundary. A fixed
transition matrix permits holding a state or advancing along the spelling.
Consecutive equal letters require a separating blank. An ONNX Scan keeps the
best-scoring alignment. The score measures the logit cost per letter relative
to the most likely acoustic path. Alternate spellings compete by their best
score. The compiler writes the numerical weights directly. We selected the
shared cutoff of −1.5 using synthetic calibration voices. Homophones and
ambiguous acoustic evidence can still trigger the same head.
[Acoustic checkpoint](https://huggingface.co/facebook/wav2vec2-base-960h/tree/22aad52d435eb6dbaf354bdad9b0da84ce7d6156)
· [Pinned ONNX conversion](https://huggingface.co/Xenova/wav2vec2-base-960h/tree/a19f851b3d42865797e410752b4c570c871e4825)
· [Weight-space programming](https://rikiyat.github.io/blog/weight-space-programming/)

**Muse editor.** An initial output-only refit caught 4/126 “Hey Goblin” clips
at its calibration false-trigger limit. The new edit exposes earlier acoustic
features. It writes unit taps into the last depthwise convolution to delay
successive channel groups by 0, 90, 180 or 270 ms. The pointwise layer copies
those channels. The probe checks the chosen delays against the preceding
layer's state and verifies that the reconstructed readout reproduces every
SDK score byte on the design and calibration clips. Three regularized linear
discriminant solves fit the phrase-specific 544 coefficients, emphasizing
the strongest wrong windows. Separate voices set the bias. Both example
PTEs were independently rebuilt byte-for-byte. The stock SDK library loads
the finished files for evaluation. [SDK model contract](https://github.com/facebookincubator/muse-gadget-sdk/blob/6a44074014daad1a623f54e62a169f514741836b/esp32/components/muse_wakeword/models/hey_muse.json)
· [Synthetic keyword spotting](https://arxiv.org/abs/2002.01322)

**ESPHome comparison.** We inspected the published TFLite graphs for Hey Jarvis
(70 tensors, 45 operators), Hey Mycroft (71, 45), and Okay Nabu (94, 55).
Their notice explicitly covers the weights under Apache 2.0. These are family
references. The differing graphs and lack of an established common checkpoint
give us no basis for copying the same channel index between them.
[Models](https://github.com/esphome/micro-wake-word-models/tree/40ff33f57f8fc6ad71a75ef085abbf742495b225/models/v2)
· [License notice](https://github.com/esphome/micro-wake-word-models/blob/40ff33f57f8fc6ad71a75ef085abbf742495b225/NOTICE)

## Provenance

Meta Platforms, Inc. and affiliates published the original Muse checkpoint under
Apache 2.0. The Goblin Link authors modified its last convolutional block and
readout. Upstream training credits are included verbatim. The additional
synthetic speech uses Piper's en_US-libritts-high, trained from scratch on the
LibriTTS corpus by Heiga Zen et al. (CC BY 4.0), eSpeak NG, and CMU Flite.
The Wav2Vec2 base model card lists Apache 2.0. Its acoustic weights are fetched
only for the experiment. The ONNX files in this directory contain the generated
phrase heads. [Project license](../../LICENSE) · [Original training credits](ATTRIBUTION.md)
· [Piper voice card](https://huggingface.co/rhasspy/piper-voices/blob/6b9cfaacb4fb2e02a9621227e3c1a48bec9710c3/en/en_US/libritts/high/MODEL_CARD)
· [LibriTTS](https://www.openslr.org/60/)
