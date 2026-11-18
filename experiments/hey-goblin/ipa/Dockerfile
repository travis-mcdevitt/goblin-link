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
RUN echo '4609fb49b7e1d28aecb2840da1926c40bd915bc6f1120a940afacf7159bbfb13  config.json' | sha256sum -c - && \
    echo 'a2254a5b58f72cd4de3632f8eee64f3f098b7c1402128d2f419e7d00ae13e335  preprocessor_config.json' | sha256sum -c - && \
    echo 'd732ab2456c0c017930001dc9af0b41b3b93d25b2eb9740bf9d925508d7d87d0  vocab.json' | sha256sum -c -
COPY --from=goblin-link/ipa-runtime:120 /opt/ipa /opt/ipa
