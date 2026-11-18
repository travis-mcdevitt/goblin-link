FROM goblin-link/phrase-lab:118
RUN /opt/venv/bin/pip install --no-cache-dir torch==2.7.1+cpu --index-url https://download.pytorch.org/whl/cpu && \
    /opt/venv/bin/pip install --no-cache-dir transformers==4.51.3
WORKDIR /opt/phones
ARG PHONES=ae45363bf3413b374fecd9dc8bc1df0e24c3b7f4
RUN base="https://huggingface.co/facebook/wav2vec2-lv-60-espeak-cv-ft/resolve/$PHONES" && \
    for file in config.json preprocessor_config.json vocab.json pytorch_model.bin; do \
        curl -fsSL --retry 3 "$base/$file" -o "$file" || exit 1; \
    done && echo '3173bde9e9ce490fa0f989e413c42f25bc1820c020adc1e6b9b87025b3cfcc5e  pytorch_model.bin' | sha256sum -c -
ENV HF_HUB_OFFLINE=1 TOKENIZERS_PARALLELISM=false
