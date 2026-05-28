# TLS test fixtures

Phase 6 line 236 slice 4. Checked-in self-signed certificate + private
key used by TLS / WebSocket-over-TLS tests and the
`examples/ws_idle_holder` Demo 1 (slice 2 onwards).

## Files

- `cert.pem` — X.509 certificate. CN=`localhost`; SANs `DNS:localhost`
  and `IP:127.0.0.1`. Valid for ~10 years from the date the fixture
  was generated.
- `key.pem` — RSA 2048-bit private key matching the cert. Unencrypted
  PKCS#8 format.
- `gen_test_cert.sh` — regenerates `cert.pem` + `key.pem` via
  `openssl req`. Run when the cert expires or to validate the recipe.

## Loading from kara source

```kara
let cert_pem: String = std.file.read_to_string("tests/fixtures/tls/cert.pem")
    .expect("read cert");
let key_pem: String = std.file.read_to_string("tests/fixtures/tls/key.pem")
    .expect("read key");
let listener: TlsListener =
    TlsListener.bind_tls("127.0.0.1:0", cert_pem, key_pem);
```

## Security caveat

The private key is committed to source control. Anyone with access to
this repository can forge a server identity for `localhost` /
`127.0.0.1`. **This is acceptable for tests against the loopback
interface but would be a security disaster deployed to any reachable
address.** Production deployments must use real certificates issued by
a public CA (Let's Encrypt) or an internal PKI.

## Regenerating

```sh
cd tests/fixtures/tls
./gen_test_cert.sh
```

Requires OpenSSL CLI. After regeneration, run the TLS test suite to
verify the new cert wires up cleanly:

```sh
cargo test --features llvm tls
```
