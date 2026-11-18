# Synthetic evaluation

## Three pronunciation-only heads

We selected a shared operating point on 353 development clips from seven Piper and two eSpeak voices. Adding phonetic alternatives and constraining competition to English increased the best development recall under the screening objective. The selected full-precision heads caught 50/63 Goblin and 51/63 Hermes examples. On the 54 plural or possessive controls they fired twice. On the separate 18 “Hey gobbling” controls they fired seven times. The calibration includes an optional initial /h/ with a cost of 5.5, an endpoint cost ceiling of 1.75, a suffix cost ceiling of 4, a 360 ms suffix horizon and a mean path cutoff of −0.95. The sweep, objective and tie-breaking order are in [tune.py](tune.py).

The three compiled head files were frozen before scoring held-out audio. Their SHA-256 digests remained identical through the later changes to the compiler. Blue Lantern had no development examples. On held-out audio we compared three models on exactly the same PCM. The spelling comparison uses the previously shipped Goblin and Hermes heads with their aliases and original cutoff, and a literal spelling head for Blue Lantern. [Frozen head commit](https://github.com/travis-mcdevitt/goblin-link/commit/cb087cb) · [Spelling runbook](../README.md)

| Held-out target | Clips | IPA, 4-bit | IPA, full precision | Spelling |
| --- | ---: | ---: | ---: | ---: |
| Hey Goblin | 119 | 89 | 88 | 89 |
| Hey Hermes | 119 | 86 | 90 | 88 |
| Blue Lantern | 119 | 79 | 82 | 64 |
| **Total** | **357** | **254** | **260** | **241** |

| Context | Hey Goblin, 4-bit | Hey Hermes, 4-bit | Blue Lantern, 4-bit |
| --- | ---: | ---: | ---: |
| Isolated | 55/68 | 49/68 | 49/68 |
| Followed by “stop” | 21/34 | 23/34 | 16/34 |
| Inside “I said … just now” | 13/17 | 14/17 | 14/17 |

| Held-out negatives, any head firing | Clips | IPA, 4-bit | IPA, full precision | Spelling |
| --- | ---: | ---: | ---: | ---: |
| Similar utterances, fragments and earlier wake phrase | 323 | 16 | 13 | 36 |
| Inflected forms and “Hey gobbling” | 136 | 15 | 15 | 86 |
| Unrelated sentences | 85 | 0 | 0 | 2 |
| Generated silence, hum, noise and sweeps | 20 | 0 | 0 | 0 |
| Wrong head on a target utterance | 357 | 0 | 0 | 0 |

The 4-bit IPA head fired on 2/34 “Hey goblins,” 0/34 “Hey goblin's,” 0/34 “Hey Hermes's” and 13/34 “Hey gobbling.” The spelling heads fired on 19, 19, 23 and 25, respectively. The two Goblin inflection spellings are separate synthesis inputs with the same usual spoken ending. “Say Hermes” accounts for 11/16 near-miss fires. A phone-based substring detector can accept the /eɪ/ in “say” when the initial /h/ has weak evidence. Some synthetic pronunciations of “gobbling” ended in an acoustic /n/.

The 921 held-out clips used 12 new Piper speaker IDs, two eSpeak voices and three Flite voices. Counts by engine were 197/252 Piper, 20/42 eSpeak and 37/63 Flite. Quiet speech was 80/94 and clean speech 139/165. Added colored noise at 6–16 dB SNR reduced detection to 35/98. The spelling model caught 52/98 noisy positives. The 4-bit IPA model caught 52 positives that spelling missed and missed 39 that spelling caught. Each speaker appears multiple times. [Fixture definitions](fixtures.py) · [Paired evaluation](evaluate.py)

## Integrated streaming model

The 4-bit export uses the same 392 phone IDs as the Meta reference. On the initial 32-clip development benchmark its six-thread acoustic real-time factor was 0.071, with 0/64 changed phrase decisions and 1.4% changed greedy frame labels. The unsigned 8-bit export measured 0.071 and changed 1/64 decisions. The installed x86 CPU provider lacked the signed 8-bit convolution kernel. The 4-bit encoder uses 241,691,639 bytes of weights and graph. The full-precision checkpoint is 1,263,535,127 bytes. Measurements were on an AMD Ryzen 7 255 and depend on thread count and host load.

We exercised the single-file three-phrase bank on six continuous scenes, totaling 234.3 seconds. The runner processed real 100 ms PCM chunks and finished in 0.40 times audio duration, excluding model load. Twenty of 36 scheduled occurrences matched and seven events were extra. Goblin matched 10/12 with five extras, Hermes 5/12 with one extra and Lantern 5/12 with one extra. Median decision time after synthesized speech ended was 546 ms, p90 was 683 ms, and the observed range was 224–723 ms. A match required the correct phrase, endpoint within −300 to +350 ms of the known speech end, and decision within −50 to +1,250 ms. This measures audio-time latency. Inference wall time is represented separately by the real-time factor.

The CLI also generated each of the two original phrases directly from IPA in the default US eSpeak voice. The Goblin WAV produced a correctly named event. The Hermes WAV produced no event. The separate full-utterance table uses future context from the whole clip. The streaming measurement limits the encoder to audio received by each decision. [Stream evaluation](stream_eval.py)

## Programming hard negatives

After examining the first 15 minutes of continuous synthetic negatives, we wrote explicit rival pronunciations for “the goblin,” “hey gobbling” and the beginning of “blew land inward.” The guarded recipe was frozen before scoring the 921-clip split. It cut Goblin positives from 89 to 86 and Blue Lantern positives from 79 to 72. Suffix-group false wakes fell from 15/136 to 6/136, including a drop from 13 to 5 on “Hey gobbling.” The 323 near-miss clips remained at 16 false wakes. Hermes used its original head in this comparison. [Guarded recipe](guarded.json)

The continuous negative challenge used 545 generated segments in 30 minutes. Roughly a quarter of the draws deliberately resembled the wake phrases. The simple three-head bank fired 19 times, including 16 events on deliberate confusions and three in ordinary sentences about a goblin figurine. On the last 15 minutes, which were reserved for the guarded comparison, explicit rivals reduced false wakes from nine to four. These are challenge-set counts. Field false wakes per hour remain unmeasured.

## English inventory and invented phrases

Ten new two-word phrases first served as a stress test. “Brown juice” fired the Brown Goose head in 5/8 voices, and Hot Kettle was caught in 1/16 clean or noisy clips. We added support for alternate acoustic segmentations such as /ə l/ versus the single label /əl/. We also programmed an explicit “brown juice” competitor. The following confirmation used six fresh Piper voices, a different eSpeak voice and a different Flite voice. Dizzy Zebra and Bright Torch were additional new phrases. The twelve heads ran simultaneously. Together their transcriptions exercise the 41 broad English consonant and vowel categories in the runbook, counting /uː/ as /u/.

| Phrase | Clean targets /8 | Noisy targets /8 | Similar-phrase false wakes /8 |
| --- | ---: | ---: | ---: |
| Quiet Feather | 7 | 1 | 0 |
| Silver Finch | 7 | 5 | 0 |
| Judge Willow | 7 | 3 | 0 |
| Shy Badger | 8 | 5 | 0 |
| North Star | 2 | 0 | 0 |
| Measure Joy | 6 | 6 | 0 |
| Brown Goose | 7 | 5 | 6 |
| Young Rook | 4 | 0 | 0 |
| Paper Kite | 5 | 3 | 0 |
| Hot Kettle | 5 | 3 | 0 |
| Dizzy Zebra | 7 | 6 | 5 |
| Bright Torch | 8 | 4 | 0 |
| **Total** | **73/96** | **41/96** | **11/96** |

There were zero wrong-head fires on 192 positives, zero fires on 96 bare-word clips, and two on 96 inflected clips. Adding the “brown juice” rival suppressed all six Brown Goose near-miss fires while retaining all 12/16 detections. North Star and Young Rook illustrate the limits of a global cutoff even on familiar English words. [Transfer phrases](transfer.json) · [Inventory evaluation](transfer.py)

Three invented phrases used literal phoneme synthesis throughout, with no spelling lookup. At the global cutoff, five eSpeak voices yielded 3/45 hits and 4/45 false wakes on minimally altered or reversed controls. Eight neural Piper voices yielded 18/72 hits and 9/72 false wakes on the same phonetic contrasts at three rates and conditions. Neither engine produced a wrong head on a target clip. The best Piper phrase, /ˈmɪpə zuːv/, reached 14/24 hits. The weakest, /ˈθɛmbə dʒɔɪp/, had 0/24. The ability to compile an IPA sequence is substantially broader than the set of phrases verified to work reliably. [IPA synthesis](synth_ipa.py) · [Pseudoword evaluation](nonce.py)

In the generic recipe qualification trial, each invented phrase had one explicit IPA rival. Six development voices selected the same −0.95 cutoff for all three. Six different confirmation voices produced 4/18, 1/18 and 0/18 hits, in the order above. False wakes were 0/48, 4/48 and 0/48. This test added previously unseen close pronunciations and cross-phrase controls. It gives a practical reason to decline these recipes at this operating point. [Qualification recipe](qualification.json) · [Operating-point selection](qualify.py)

## Refining Goblin and Hermes

The first held-out test identified “Say Hermes” as the most frequent Hermes confusion. We added that pronunciation as an explicit competitor, kept the Goblin rivals, and included alternate English vowels in both positives. We then evaluated the changes on 18 additional Piper speakers and four eSpeak voices. Each speaker supplied isolated clean, quiet and noisy phrases, a wake-plus-command, and 14 control utterances. The optional spectral subtraction front end was selected on the original development data. On its noisy subset it raised the two-phrase hit count from 16/38 to 23/38, with the same four false wakes on 74 noisy speech controls. [Focused recipe](focused.json) · [Noise selection](noise_eval.py)

| Fresh-voice trial | Trials | Simple IPA | Positive variants only | Variants and rivals | Variants, rivals and filter |
| --- | ---: | ---: | ---: | ---: | ---: |
| Hey Goblin hits | 88 | 66 | 68 | 67 | 71 |
| Hey Hermes hits | 88 | 71 | 71 | 69 | 73 |
| Control false wakes | 308 | 55 | 55 | 37 | 40 |
| Wrong head on a target | 176 | 0 | 0 | 0 | 0 |
| Noisy target hits across both phrases | 44 | 21 | 23 | 22 | 30 |

The “Say Hermes” count fell from 12/22 to 0/22. “Hi Hermes” and “OK Hermes” together accounted for 28/37 remaining false wakes before filtering. In 119.01 seconds of separate continuous speech, the simple and focused banks each detected 15/24 scheduled wakes. The focused bank reduced extra events from four to one. Median decision delay was 536 ms. Filtering preserved those streaming counts. [Fresh-voice evaluation](focused_eval.py) · [Continuous comparison](focused_stream.py)

The final refinement programmed “Hi Hermes” and “OK Hermes” as additional rival paths. A further 14 voices, including 12 new Piper speaker IDs, supplied 70 target clips per phrase and 252 controls. The targets included embedded phrases as well as isolated and command-followed speech. Four additional prefix controls were new to this comparison.

| Final paired trial | Simple IPA | Balanced | Stricter | Stricter plus filter |
| --- | ---: | ---: | ---: | ---: |
| Hey Goblin hits /70 | 57 | 56 | 56 | 61 |
| Hey Hermes hits /70 | 63 | 63 | 44 | 48 |
| Any false wake /252 controls | 71 | 57 | 32 | 36 |
| Wrong head /140 targets | 0 | 0 | 0 | 0 |

The stricter bank eliminated all 13 “Hi Hermes” and all 10 “OK Hermes” fires from the balanced bank. It also missed 19 additional genuine Hermes examples. Most remaining strict-profile false wakes came from “Pay Hermes” (11/14), “Play Hermes” (11/14) and “Hail Hermes” (9/14). For the original two phrases, the balanced recipe is the starting point when preserving Hermes sensitivity matters. The stricter profile trades that sensitivity for fewer close-prefix activations. [Balanced recipe](focused.json) · [Stricter recipe](english-wakes.json)

In a final 140.35 seconds of new mixed synthetic speech, simple IPA detected 17/24 scheduled wakes with 12 extra events. The stricter bank detected 13/24 with four extra events. Median delay for the matched strict events was 406 ms. Its comparison run, scoring both sets of heads together, took 0.67 times audio duration. Filtering preserved the counts in those scenes. The actual two-phrase ONNX artifact also detected both example WAVs synthesized directly from IPA with Piper speaker 810 through the normal CLI.

A user-supplied balanced Hermes recipe also went through the qualification entry point. Its clip screen found 13/18 confirmation targets and 0/54 control fires. A separate integrated rolling check measured 6/9 scheduled wakes and zero extras in three additional synthetic voices.

All results are local CPU measurements. The integrated models are approximately 230 MiB and use ONNX Runtime's 4-bit CPU operators. Handheld timing, Muse or ESPHome format compatibility, and performance on human speech remain unmeasured.
