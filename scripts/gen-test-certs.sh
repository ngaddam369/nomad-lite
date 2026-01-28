#!/bin/bash
#
# Generate test certificates for Nomad-Lite mTLS.
#
# Usage:
#   ./scripts/gen-test-certs.sh [output_dir]
#
# Generates:
#   - ca.crt, ca.key: Certificate Authority
#   - node1.crt, node1.key: Node 1 certificate
#   - node2.crt, node2.key: Node 2 certificate
#   - node3.crt, node3.key: Node 3 certificate
#   - client.crt, client.key: Client certificate
#

set -e

CERT_DIR="${1:-./certs}"
mkdir -p "$CERT_DIR"

echo "Generating certificates in $CERT_DIR..."

# Generate CA private key
openssl genrsa -out "$CERT_DIR/ca.key" 4096 2>/dev/null

# Generate CA certificate
openssl req -new -x509 -days 365 -key "$CERT_DIR/ca.key" \
    -out "$CERT_DIR/ca.crt" \
    -subj "/CN=nomad-lite-ca/O=nomad-lite" 2>/dev/null

echo "Created CA certificate"

# Function to generate node certificate
gen_node_cert() {
    local name="$1"
    local san="$2"

    # Generate private key
    openssl genrsa -out "$CERT_DIR/${name}.key" 2048 2>/dev/null

    # Generate CSR
    openssl req -new -key "$CERT_DIR/${name}.key" \
        -out "$CERT_DIR/${name}.csr" \
        -subj "/CN=${name}/O=nomad-lite-cluster" 2>/dev/null

    # Create extension file for SAN
    cat > "$CERT_DIR/${name}.ext" << EOF
authorityKeyIdentifier=keyid,issuer
basicConstraints=CA:FALSE
keyUsage = digitalSignature, keyEncipherment
extendedKeyUsage = serverAuth, clientAuth
subjectAltName = ${san}
EOF

    # Sign with CA
    openssl x509 -req -in "$CERT_DIR/${name}.csr" \
        -CA "$CERT_DIR/ca.crt" -CAkey "$CERT_DIR/ca.key" \
        -CAcreateserial -out "$CERT_DIR/${name}.crt" \
        -days 365 -extfile "$CERT_DIR/${name}.ext" 2>/dev/null

    # Clean up
    rm "$CERT_DIR/${name}.csr" "$CERT_DIR/${name}.ext"

    echo "Created certificate for $name"
}

# Generate certificates for 3 nodes
# Include both localhost and node names for flexibility
gen_node_cert "node1" "DNS:localhost,DNS:node1,DNS:nomad-lite-cluster,IP:127.0.0.1"
gen_node_cert "node2" "DNS:localhost,DNS:node2,DNS:nomad-lite-cluster,IP:127.0.0.1"
gen_node_cert "node3" "DNS:localhost,DNS:node3,DNS:nomad-lite-cluster,IP:127.0.0.1"

# Generate client certificate
gen_node_cert "client" "DNS:localhost,DNS:nomad-lite-cluster,IP:127.0.0.1"

# Clean up CA serial file
rm -f "$CERT_DIR/ca.srl"

echo ""
echo "Certificates generated successfully:"
ls -la "$CERT_DIR"

echo ""
echo "Usage example:"
echo "  nomad-lite --node-id 1 --port 50051 \\"
echo "    --peers '2:127.0.0.1:50052,3:127.0.0.1:50053' \\"
echo "    --tls --ca-cert $CERT_DIR/ca.crt \\"
echo "    --cert $CERT_DIR/node1.crt --key $CERT_DIR/node1.key"
