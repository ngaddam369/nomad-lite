# Security

## mTLS

All gRPC communication (node-to-node, client-to-node) can be secured with mutual TLS:

- Both parties authenticate via certificates signed by cluster CA
- All traffic encrypted with TLS 1.2+
- Generate certs: `./scripts/gen-test-certs.sh ./certs`

## Docker Sandboxing

Jobs run in isolated containers with:

| Restriction | Setting |
|-------------|---------|
| Network | `--network=none` |
| Capabilities | `--cap-drop=ALL` |
| Filesystem | `--read-only` |
| Privileges | `--security-opt=no-new-privileges` |
| Memory | `--memory=256m` |
| CPU | `--cpus=0.5` |
