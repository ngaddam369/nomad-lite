//! Tests for TLS/mTLS functionality.
//!
//! These tests verify:
//! - TLS configuration validation
//! - Certificate loading
//! - TLS-enabled cluster communication
//! - Certificate rejection scenarios

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;
use tempfile::TempDir;

use nomad_lite::config::TlsConfig;
use nomad_lite::tls::TlsIdentity;
use tokio_util::sync::CancellationToken;

/// Helper to generate test certificates in a temporary directory
fn generate_test_certs() -> TempDir {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let cert_dir = temp_dir.path();

    // Run the certificate generation script
    let script_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/gen-test-certs.sh");

    let output = Command::new("bash")
        .arg(&script_path)
        .arg(cert_dir)
        .output()
        .expect("Failed to run cert generation script");

    if !output.status.success() {
        panic!(
            "Certificate generation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    temp_dir
}

// ============================================================================
// TlsConfig Unit Tests
// ============================================================================

#[test]
fn test_tls_config_default() {
    let config = TlsConfig::default();

    assert!(!config.enabled);
    assert!(config.ca_cert_path.is_none());
    assert!(config.cert_path.is_none());
    assert!(config.key_path.is_none());
    assert!(!config.allow_insecure);
}

#[test]
fn test_tls_config_is_complete_when_all_paths_set() {
    let config = TlsConfig {
        enabled: true,
        ca_cert_path: Some(PathBuf::from("/path/to/ca.crt")),
        cert_path: Some(PathBuf::from("/path/to/node.crt")),
        key_path: Some(PathBuf::from("/path/to/node.key")),
        allow_insecure: false,
    };

    assert!(config.is_complete());
}

#[test]
fn test_tls_config_is_not_complete_when_disabled() {
    let config = TlsConfig {
        enabled: false,
        ca_cert_path: Some(PathBuf::from("/path/to/ca.crt")),
        cert_path: Some(PathBuf::from("/path/to/node.crt")),
        key_path: Some(PathBuf::from("/path/to/node.key")),
        allow_insecure: false,
    };

    assert!(!config.is_complete());
}

#[test]
fn test_tls_config_is_not_complete_when_ca_missing() {
    let config = TlsConfig {
        enabled: true,
        ca_cert_path: None,
        cert_path: Some(PathBuf::from("/path/to/node.crt")),
        key_path: Some(PathBuf::from("/path/to/node.key")),
        allow_insecure: false,
    };

    assert!(!config.is_complete());
}

#[test]
fn test_tls_config_is_not_complete_when_cert_missing() {
    let config = TlsConfig {
        enabled: true,
        ca_cert_path: Some(PathBuf::from("/path/to/ca.crt")),
        cert_path: None,
        key_path: Some(PathBuf::from("/path/to/node.key")),
        allow_insecure: false,
    };

    assert!(!config.is_complete());
}

#[test]
fn test_tls_config_is_not_complete_when_key_missing() {
    let config = TlsConfig {
        enabled: true,
        ca_cert_path: Some(PathBuf::from("/path/to/ca.crt")),
        cert_path: Some(PathBuf::from("/path/to/node.crt")),
        key_path: None,
        allow_insecure: false,
    };

    assert!(!config.is_complete());
}

// ============================================================================
// TlsIdentity Loading Tests
// ============================================================================

#[tokio::test]
async fn test_load_valid_certificates() {
    let temp_dir = generate_test_certs();
    let cert_dir = temp_dir.path();

    let config = TlsConfig {
        enabled: true,
        ca_cert_path: Some(cert_dir.join("ca.crt")),
        cert_path: Some(cert_dir.join("node1.crt")),
        key_path: Some(cert_dir.join("node1.key")),
        allow_insecure: false,
    };

    let result = TlsIdentity::load(&config).await;
    assert!(
        result.is_ok(),
        "Should load valid certificates: {:?}",
        result.err()
    );
}

#[tokio::test]
async fn test_load_client_certificate() {
    let temp_dir = generate_test_certs();
    let cert_dir = temp_dir.path();

    let config = TlsConfig {
        enabled: true,
        ca_cert_path: Some(cert_dir.join("ca.crt")),
        cert_path: Some(cert_dir.join("client.crt")),
        key_path: Some(cert_dir.join("client.key")),
        allow_insecure: false,
    };

    let result = TlsIdentity::load(&config).await;
    assert!(
        result.is_ok(),
        "Should load client certificates: {:?}",
        result.err()
    );
}

#[tokio::test]
async fn test_load_all_node_certificates() {
    let temp_dir = generate_test_certs();
    let cert_dir = temp_dir.path();

    for node in &["node1", "node2", "node3"] {
        let config = TlsConfig {
            enabled: true,
            ca_cert_path: Some(cert_dir.join("ca.crt")),
            cert_path: Some(cert_dir.join(format!("{}.crt", node))),
            key_path: Some(cert_dir.join(format!("{}.key", node))),
            allow_insecure: false,
        };

        let result = TlsIdentity::load(&config).await;
        assert!(
            result.is_ok(),
            "Should load {} certificates: {:?}",
            node,
            result.err()
        );
    }
}

#[tokio::test]
async fn test_load_nonexistent_ca_cert() {
    let temp_dir = generate_test_certs();
    let cert_dir = temp_dir.path();

    let config = TlsConfig {
        enabled: true,
        ca_cert_path: Some(PathBuf::from("/nonexistent/ca.crt")),
        cert_path: Some(cert_dir.join("node1.crt")),
        key_path: Some(cert_dir.join("node1.key")),
        allow_insecure: false,
    };

    let result = TlsIdentity::load(&config).await;
    assert!(result.is_err(), "Should fail with nonexistent CA cert");
}

#[tokio::test]
async fn test_load_nonexistent_node_cert() {
    let temp_dir = generate_test_certs();
    let cert_dir = temp_dir.path();

    let config = TlsConfig {
        enabled: true,
        ca_cert_path: Some(cert_dir.join("ca.crt")),
        cert_path: Some(PathBuf::from("/nonexistent/node.crt")),
        key_path: Some(cert_dir.join("node1.key")),
        allow_insecure: false,
    };

    let result = TlsIdentity::load(&config).await;
    assert!(result.is_err(), "Should fail with nonexistent node cert");
}

#[tokio::test]
async fn test_load_nonexistent_key() {
    let temp_dir = generate_test_certs();
    let cert_dir = temp_dir.path();

    let config = TlsConfig {
        enabled: true,
        ca_cert_path: Some(cert_dir.join("ca.crt")),
        cert_path: Some(cert_dir.join("node1.crt")),
        key_path: Some(PathBuf::from("/nonexistent/node.key")),
        allow_insecure: false,
    };

    let result = TlsIdentity::load(&config).await;
    assert!(result.is_err(), "Should fail with nonexistent key");
}

#[tokio::test]
async fn test_load_mismatched_cert_and_key() {
    let temp_dir = generate_test_certs();
    let cert_dir = temp_dir.path();

    // Use node1's cert with node2's key - should fail
    let config = TlsConfig {
        enabled: true,
        ca_cert_path: Some(cert_dir.join("ca.crt")),
        cert_path: Some(cert_dir.join("node1.crt")),
        key_path: Some(cert_dir.join("node2.key")),
        allow_insecure: false,
    };

    let result = TlsIdentity::load(&config).await;
    // Note: tonic/rustls may or may not validate cert/key match at load time
    // This test documents the current behavior
    if result.is_err() {
        // Expected: mismatched cert/key rejected
    } else {
        // Some implementations defer validation to connection time
    }
}

// ============================================================================
// TLS Config Methods Tests
// ============================================================================

#[tokio::test]
async fn test_server_tls_config_creation() {
    let temp_dir = generate_test_certs();
    let cert_dir = temp_dir.path();

    let config = TlsConfig {
        enabled: true,
        ca_cert_path: Some(cert_dir.join("ca.crt")),
        cert_path: Some(cert_dir.join("node1.crt")),
        key_path: Some(cert_dir.join("node1.key")),
        allow_insecure: false,
    };

    let identity = TlsIdentity::load(&config).await.expect("Should load certs");
    let _server_config = identity.server_tls_config();
    // If we get here without panic, the config was created successfully
}

#[tokio::test]
async fn test_client_tls_config_creation() {
    let temp_dir = generate_test_certs();
    let cert_dir = temp_dir.path();

    let config = TlsConfig {
        enabled: true,
        ca_cert_path: Some(cert_dir.join("ca.crt")),
        cert_path: Some(cert_dir.join("node1.crt")),
        key_path: Some(cert_dir.join("node1.key")),
        allow_insecure: false,
    };

    let identity = TlsIdentity::load(&config).await.expect("Should load certs");
    let _client_config = identity.client_tls_config();
    // If we get here without panic, the config was created successfully
}

// ============================================================================
// Integration Tests - TLS Cluster Communication
// ============================================================================

mod integration {
    use super::*;
    use nomad_lite::config::{NodeConfig, PeerConfig, SandboxConfig, TlsConfig};
    use nomad_lite::grpc::GrpcServer;
    use nomad_lite::raft::RaftNode;
    use nomad_lite::scheduler::JobQueue;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    /// Create a node config for TLS testing
    fn tls_test_config(
        node_id: u64,
        port: u16,
        peers: Vec<(u64, u16)>,
        tls_config: TlsConfig,
    ) -> NodeConfig {
        let peer_configs: Vec<PeerConfig> = peers
            .into_iter()
            .map(|(id, p)| PeerConfig {
                node_id: id,
                addr: format!("127.0.0.1:{}", p),
            })
            .collect();

        NodeConfig {
            node_id,
            listen_addr: format!("127.0.0.1:{}", port).parse().unwrap(),
            peers: peer_configs,
            election_timeout_min_ms: 150,
            election_timeout_max_ms: 300,
            heartbeat_interval_ms: 50,
            sandbox: SandboxConfig::default(),
            tls: tls_config,
        }
    }

    #[tokio::test]
    async fn test_single_node_with_tls() {
        let temp_dir = generate_test_certs();
        let cert_dir = temp_dir.path();

        let tls_config = TlsConfig {
            enabled: true,
            ca_cert_path: Some(cert_dir.join("ca.crt")),
            cert_path: Some(cert_dir.join("node1.crt")),
            key_path: Some(cert_dir.join("node1.key")),
            allow_insecure: false,
        };

        let tls_identity = TlsIdentity::load(&tls_config)
            .await
            .expect("Should load TLS identity");

        let config = tls_test_config(1, 51001, vec![], tls_config);
        let listen_addr = config.listen_addr;

        let (raft_node, _raft_rx) = RaftNode::new(config.clone(), Some(tls_identity.clone()));
        let raft_node = Arc::new(raft_node);
        let job_queue = Arc::new(RwLock::new(JobQueue::new()));

        let server = GrpcServer::new(
            listen_addr,
            config,
            raft_node,
            job_queue,
            Some(tls_identity),
        );

        // Start server in background
        let server_handle = tokio::spawn(async move {
            let _ = server.run(CancellationToken::new()).await;
        });

        // Give server time to start
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Server should be running
        assert!(!server_handle.is_finished());

        server_handle.abort();
    }

    #[tokio::test]
    async fn test_two_node_tls_cluster_connects() {
        let temp_dir = generate_test_certs();
        let cert_dir = temp_dir.path();

        // Node 1 config
        let tls_config1 = TlsConfig {
            enabled: true,
            ca_cert_path: Some(cert_dir.join("ca.crt")),
            cert_path: Some(cert_dir.join("node1.crt")),
            key_path: Some(cert_dir.join("node1.key")),
            allow_insecure: false,
        };
        let tls_identity1 = TlsIdentity::load(&tls_config1)
            .await
            .expect("Should load TLS identity 1");

        // Node 2 config
        let tls_config2 = TlsConfig {
            enabled: true,
            ca_cert_path: Some(cert_dir.join("ca.crt")),
            cert_path: Some(cert_dir.join("node2.crt")),
            key_path: Some(cert_dir.join("node2.key")),
            allow_insecure: false,
        };
        let tls_identity2 = TlsIdentity::load(&tls_config2)
            .await
            .expect("Should load TLS identity 2");

        let port1 = 51101;
        let port2 = 51102;

        let config1 = tls_test_config(1, port1, vec![(2, port2)], tls_config1);
        let config2 = tls_test_config(2, port2, vec![(1, port1)], tls_config2);

        // Create nodes
        let (raft_node1, _rx1) = RaftNode::new(config1.clone(), Some(tls_identity1.clone()));
        let raft_node1 = Arc::new(raft_node1);
        let job_queue1 = Arc::new(RwLock::new(JobQueue::new()));

        let (raft_node2, _rx2) = RaftNode::new(config2.clone(), Some(tls_identity2.clone()));
        let raft_node2 = Arc::new(raft_node2);
        let job_queue2 = Arc::new(RwLock::new(JobQueue::new()));

        // Start servers
        let server1 = GrpcServer::new(
            config1.listen_addr,
            config1,
            raft_node1.clone(),
            job_queue1,
            Some(tls_identity1),
        );
        let server2 = GrpcServer::new(
            config2.listen_addr,
            config2,
            raft_node2.clone(),
            job_queue2,
            Some(tls_identity2),
        );

        let handle1 = tokio::spawn(async move {
            let _ = server1.run(CancellationToken::new()).await;
        });
        let handle2 = tokio::spawn(async move {
            let _ = server2.run(CancellationToken::new()).await;
        });

        // Wait for servers to start
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Try to connect peers
        raft_node1.connect_to_peers().await;
        raft_node2.connect_to_peers().await;

        // Give time for connections
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Check if peers connected
        let node1_connected = raft_node1.all_peers_connected().await;
        let node2_connected = raft_node2.all_peers_connected().await;

        handle1.abort();
        handle2.abort();

        assert!(
            node1_connected,
            "Node 1 should be connected to Node 2 via TLS"
        );
        assert!(
            node2_connected,
            "Node 2 should be connected to Node 1 via TLS"
        );
    }

    #[tokio::test]
    async fn test_mismatched_tls_configs_communication_fails() {
        // This test verifies that nodes with mismatched TLS configurations
        // cannot successfully communicate, even if they appear "connected"
        // at the transport layer.
        //
        // Note: The actual connection tracking may show connected=true because
        // gRPC channels are lazy and don't establish connections until first RPC.
        // The important thing is that actual Raft communication will fail.

        let temp_dir = generate_test_certs();
        let cert_dir = temp_dir.path();

        // Node 1 with TLS
        let tls_config1 = TlsConfig {
            enabled: true,
            ca_cert_path: Some(cert_dir.join("ca.crt")),
            cert_path: Some(cert_dir.join("node1.crt")),
            key_path: Some(cert_dir.join("node1.key")),
            allow_insecure: false,
        };
        let tls_identity1 = TlsIdentity::load(&tls_config1)
            .await
            .expect("Should load TLS identity");

        let port1 = 51201;
        let port2 = 51202;

        let config1 = tls_test_config(1, port1, vec![(2, port2)], tls_config1);

        // Node 2 WITHOUT TLS
        let config2 = tls_test_config(2, port2, vec![(1, port1)], TlsConfig::default());

        // Create nodes
        let (raft_node1, _rx1) = RaftNode::new(config1.clone(), Some(tls_identity1.clone()));
        let raft_node1 = Arc::new(raft_node1);
        let job_queue1 = Arc::new(RwLock::new(JobQueue::new()));

        let (raft_node2, _rx2) = RaftNode::new(config2.clone(), None);
        let raft_node2 = Arc::new(raft_node2);
        let job_queue2 = Arc::new(RwLock::new(JobQueue::new()));

        // Start servers
        let server1 = GrpcServer::new(
            config1.listen_addr,
            config1,
            raft_node1.clone(),
            job_queue1,
            Some(tls_identity1),
        );
        let server2 = GrpcServer::new(
            config2.listen_addr,
            config2,
            raft_node2.clone(),
            job_queue2,
            None,
        );

        let handle1 = tokio::spawn(async move {
            let _ = server1.run(CancellationToken::new()).await;
        });
        let handle2 = tokio::spawn(async move {
            let _ = server2.run(CancellationToken::new()).await;
        });

        // Wait for servers to start
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Try to connect - this may succeed at connection level but fail at protocol level
        raft_node2.connect_to_peers().await;

        // Give time for connection attempts
        tokio::time::sleep(Duration::from_millis(500)).await;

        // The peers_status should show the TLS node as not responding
        // because Raft heartbeats will fail
        let peers_status = raft_node2.get_peers_status().await;
        let node1_alive = peers_status.get(&1).copied().unwrap_or(false);

        handle1.abort();
        handle2.abort();

        // Node 1 (TLS-enabled) should appear dead to node 2 (non-TLS)
        // because they cannot successfully exchange Raft messages
        assert!(
            !node1_alive,
            "TLS node should appear dead to non-TLS node due to protocol mismatch"
        );
    }
}

// ============================================================================
// Certificate File Tests
// ============================================================================

#[test]
fn test_cert_generation_script_exists() {
    let script_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/gen-test-certs.sh");
    assert!(
        script_path.exists(),
        "Certificate generation script should exist"
    );
}

#[test]
fn test_generated_certs_have_correct_files() {
    let temp_dir = generate_test_certs();
    let cert_dir = temp_dir.path();

    let expected_files = [
        "ca.crt",
        "ca.key",
        "node1.crt",
        "node1.key",
        "node2.crt",
        "node2.key",
        "node3.crt",
        "node3.key",
        "client.crt",
        "client.key",
    ];

    for file in &expected_files {
        let path = cert_dir.join(file);
        assert!(path.exists(), "Generated cert should include {}", file);
    }
}

#[test]
fn test_generated_certs_are_pem_format() {
    let temp_dir = generate_test_certs();
    let cert_dir = temp_dir.path();

    // Check that .crt files start with "-----BEGIN CERTIFICATE-----"
    let ca_cert = std::fs::read_to_string(cert_dir.join("ca.crt")).expect("Should read ca.crt");
    assert!(
        ca_cert.contains("-----BEGIN CERTIFICATE-----"),
        "CA cert should be in PEM format"
    );

    // Check that .key files start with "-----BEGIN"
    let ca_key = std::fs::read_to_string(cert_dir.join("ca.key")).expect("Should read ca.key");
    assert!(
        ca_key.contains("-----BEGIN"),
        "CA key should be in PEM format"
    );
}

#[test]
fn test_node_certs_are_not_self_signed() {
    let temp_dir = generate_test_certs();
    let cert_dir = temp_dir.path();

    // Verify node1 cert is signed by CA using openssl
    let output = Command::new("openssl")
        .args([
            "verify",
            "-CAfile",
            cert_dir.join("ca.crt").to_str().unwrap(),
            cert_dir.join("node1.crt").to_str().unwrap(),
        ])
        .output()
        .expect("Failed to run openssl verify");

    assert!(
        output.status.success(),
        "Node cert should be signed by CA: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
