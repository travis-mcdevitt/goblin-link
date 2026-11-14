`cert.der` and `key.der` are a self-signed certificate and its key for the fake plugin in
`src/testsupport.rs`. They protect nothing: the key is public by being here. The certificate's
SHA-256 is `testsupport::FINGERPRINT`.
