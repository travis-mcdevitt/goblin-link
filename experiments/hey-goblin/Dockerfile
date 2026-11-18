FROM ubuntu:24.04
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    ca-certificates curl g++ unzip python3-venv espeak-ng flite && rm -rf /var/lib/apt/lists/*
RUN python3 -m venv /opt/venv && /opt/venv/bin/pip install --no-cache-dir \
    numpy==2.2.6 scipy==1.15.3 onnxruntime==1.22.1 piper-tts==1.3.0
ENV PATH="/opt/venv/bin:$PATH" OPENBLAS_NUM_THREADS=1 OMP_NUM_THREADS=1

ARG SDK=6a44074014daad1a623f54e62a169f514741836b
WORKDIR /opt/sdk
RUN curl -fsSL --retry 3 "https://codeload.github.com/facebookincubator/muse-gadget-sdk/tar.gz/$SDK" -o /tmp/sdk.tgz && \
    echo '5e639406eb165f6642ab02a5a5d3ed619ec6d6f8c108aff5731204be0a73f34e  /tmp/sdk.tgz' | sha256sum -c - && \
    tar -xzf /tmp/sdk.tgz --strip-components=1 "muse-gadget-sdk-$SDK/esp32/components/muse_wakeword" \
        "muse-gadget-sdk-$SDK/linux/wakeword" "muse-gadget-sdk-$SDK/linux/src/musegadget" && rm /tmp/sdk.tgz && \
    echo 'ad31331ef5d1782b60952df634c32a4c065492a8c635b240982bb681b6b95cbc  esp32/components/muse_wakeword/models/hey_muse.pte' | sha256sum -c - && \
    bash linux/wakeword/build.sh --out /opt/runner
ENV PYTHONPATH=/opt/sdk/linux/src MUSE_WAKEWORD_LIBRARY=/opt/runner/libmuse_wakeword.so

# Only the calibration library exposes the input to the classifier.
COPY read_state.cpp /tmp/read_state.cpp
RUN cat /tmp/read_state.cpp >> esp32/components/muse_wakeword/ww_runner_espnn.cpp && \
    sed -i 's/{ global: ww_runner_create;/{ global: goblin_read_step; ww_runner_create;/' linux/wakeword/build.sh && \
    bash linux/wakeword/build.sh --deps /opt/runner/archives --offline --out /opt/probe

ARG VOICES=6b9cfaacb4fb2e02a9621227e3c1a48bec9710c3
WORKDIR /opt/voice
RUN base="https://huggingface.co/rhasspy/piper-voices/resolve/$VOICES/en/en_US/libritts/high" && \
    curl -fsSL --retry 3 "$base/en_US-libritts-high.onnx" -o voice.onnx && \
    curl -fsSL --retry 3 "$base/en_US-libritts-high.onnx.json" -o voice.json && \
    echo '9127a559e11603f10b366d1a20ac7426826081dbc521de4c2420c57728d73f0f  voice.onnx' | sha256sum -c - && \
    echo '2efdc6d7f954588b8180132cbd9b8001933fdd00932c92bc92fd0d2028a9eb3d  voice.json' | sha256sum -c -

# Independent letter-based comparison for spelling-defined phrase heads.
RUN /opt/venv/bin/pip install --no-cache-dir onnx==1.17.0
ARG CTC=a19f851b3d42865797e410752b4c570c871e4825
WORKDIR /opt/ctc
RUN base="https://huggingface.co/Xenova/wav2vec2-base-960h/resolve/$CTC" && \
    curl -fsSL --retry 3 "$base/onnx/model_quantized.onnx" -o acoustic.onnx && \
    curl -fsSL --retry 3 "$base/vocab.json" -o vocab.json && \
    echo 'cd5040c147381580ed73258143dd8e0c28e800a09e74ee42ee2b3e8cb4d760a3  acoustic.onnx' | sha256sum -c - && \
    echo '4178db26b3c7570f6a47f14ac6a1c7b32950b8c2800fb097287e53776934f1c5  vocab.json' | sha256sum -c -
