`cert.der` and `key.der` are a self-signed certificate and its key for the fake plugin in
`src/testsupport.rs`; `other-key.der` is an unrelated key, for a server that shows the
certificate without holding its key. They protect nothing: the keys are public by being here.
The certificate's SHA-256 is `testsupport::FINGERPRINT`.
