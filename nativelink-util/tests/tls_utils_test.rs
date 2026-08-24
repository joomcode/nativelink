// Copyright 2025 The NativeLink Authors. All rights reserved.
//
// Licensed under the Functional Source License, Version 1.1, Apache 2.0 Future License (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//    See LICENSE file for details
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use core::time::Duration;

use nativelink_config::stores::{ClientTlsConfig, GrpcEndpoint};
use nativelink_error::Error;
use nativelink_macro::nativelink_test;
use nativelink_util::tls_utils::{
    balanced_endpoint, endpoint_from, load_balanced_options, load_client_config,
};
use tempfile::NamedTempFile;

#[nativelink_test]
async fn test_load_client_config_none() -> Result<(), Error> {
    let config = load_client_config(&None)?;
    assert!(config.is_none());
    Ok(())
}

#[nativelink_test]
async fn test_load_client_config_native_roots() -> Result<(), Error> {
    let config = load_client_config(&Some(ClientTlsConfig {
        use_native_roots: Some(true),
        ca_file: None,
        cert_file: None,
        key_file: None,
    }))?;
    assert!(config.is_some());
    Ok(())
}

#[nativelink_test]
async fn test_load_client_config_missing_ca() -> Result<(), Error> {
    let result = load_client_config(&Some(ClientTlsConfig {
        use_native_roots: None,
        ca_file: None,
        cert_file: None,
        key_file: None,
    }));
    assert!(matches!(
        result,
        Err(e) if e.to_string().contains("CA certificate must be provided")
    ));
    Ok(())
}

#[nativelink_test]
async fn test_load_client_config_cert_without_key() -> Result<(), Error> {
    let temp_file = NamedTempFile::new()?;
    let result = load_client_config(&Some(ClientTlsConfig {
        use_native_roots: None,
        ca_file: Some(temp_file.path().to_str().unwrap().to_string()),
        cert_file: Some("tls.crt".to_string()),
        key_file: None,
    }));
    assert!(matches!(
        result,
        Err(e) if e.to_string().contains("Client certificate specified, but no key")
    ));
    Ok(())
}

#[nativelink_test]
async fn test_load_client_config_key_without_cert() -> Result<(), Error> {
    let temp_file = NamedTempFile::new()?;
    let result = load_client_config(&Some(ClientTlsConfig {
        use_native_roots: None,
        ca_file: Some(temp_file.path().to_str().unwrap().to_string()),
        cert_file: None,
        key_file: Some("tls.key".to_string()),
    }));
    assert!(matches!(
        result,
        Err(e) if e.to_string().contains("Client key specified, but no certificate")
    ));
    Ok(())
}

#[nativelink_test]
async fn test_load_client_config_with_cert_files() -> Result<(), Error> {
    let temp_file = NamedTempFile::new()?;
    let config = load_client_config(&Some(ClientTlsConfig {
        use_native_roots: None,
        ca_file: Some(temp_file.path().to_str().unwrap().to_string()),
        cert_file: Some(temp_file.path().to_str().unwrap().to_string()),
        key_file: Some(temp_file.path().to_str().unwrap().to_string()),
    }))?;
    assert!(config.is_some());
    Ok(())
}

#[nativelink_test]
async fn test_endpoint_from_http() -> Result<(), Error> {
    let endpoint = endpoint_from("http://localhost:50051", None)?;
    assert_eq!(endpoint.uri().scheme_str(), Some("http"));
    assert_eq!(endpoint.uri().host(), Some("localhost"));
    assert_eq!(endpoint.uri().port_u16(), Some(50051));
    Ok(())
}

#[nativelink_test]
async fn test_endpoint_from_https_with_tls() -> Result<(), Error> {
    let tls_config = load_client_config(&Some(ClientTlsConfig {
        use_native_roots: Some(true),
        ca_file: None,
        cert_file: None,
        key_file: None,
    }))?;
    let endpoint = endpoint_from("https://example.com", tls_config)?;
    assert_eq!(endpoint.uri().scheme_str(), Some("https"));
    assert_eq!(endpoint.uri().host(), Some("example.com"));
    Ok(())
}

#[nativelink_test]
async fn test_endpoint_from_grpcs_with_tls() -> Result<(), Error> {
    let tls_config = load_client_config(&Some(ClientTlsConfig {
        use_native_roots: Some(true),
        ca_file: None,
        cert_file: None,
        key_file: None,
    }))?;
    let endpoint = endpoint_from("grpcs://example.com", tls_config)?;
    assert_eq!(endpoint.uri().scheme_str(), Some("https"));
    assert_eq!(endpoint.uri().host(), Some("example.com"));
    Ok(())
}

#[nativelink_test]
async fn test_endpoint_from_https_without_tls() -> Result<(), Error> {
    let result = endpoint_from("https://example.com", None);
    assert!(matches!(
        result,
        Err(e) if e.to_string().contains("is https or grpcs, but no TLS configuration was provided")
    ));
    Ok(())
}

#[nativelink_test]
async fn test_endpoint_from_http_with_tls() -> Result<(), Error> {
    let tls_config = load_client_config(&Some(ClientTlsConfig {
        use_native_roots: Some(true),
        ca_file: None,
        cert_file: None,
        key_file: None,
    }))?;
    let result = endpoint_from("http://example.com:8080", tls_config);
    assert!(matches!(
        result,
        Err(e) if e.to_string().contains("but the scheme is not https or grpcs")
    ));
    Ok(())
}

#[nativelink_test]
async fn test_endpoint_from_invalid_uri() -> Result<(), Error> {
    let result = endpoint_from("not a valid uri", None);
    assert!(matches!(
        result,
        Err(e) if e.to_string().contains("Unable to parse endpoint")
    ));
    Ok(())
}

#[nativelink_test]
async fn test_endpoint_from_missing_authority() -> Result<(), Error> {
    let tls_config = load_client_config(&Some(ClientTlsConfig {
        use_native_roots: Some(true),
        ca_file: None,
        cert_file: None,
        key_file: None,
    }))?;
    let result = endpoint_from("/path/no/authority", tls_config);
    assert!(matches!(
        result,
        Err(e) if e.to_string().contains("Unable to determine authority of endpoint")
    ));
    Ok(())
}

fn grpc_endpoint(connect_timeout_s: u64) -> GrpcEndpoint {
    GrpcEndpoint {
        address: "grpc://example.com:50051".to_string(),
        tls_config: None,
        concurrency_limit: None,
        connect_timeout_s,
        tcp_keepalive_s: 0,
        http2_keepalive_interval_s: 0,
        http2_keepalive_timeout_s: 0,
    }
}

#[nativelink_test]
async fn test_load_balanced_options_carry_endpoint_configuration() -> Result<(), Error> {
    let options = load_balanced_options(&grpc_endpoint(7), Duration::from_mins(1))?;

    assert_eq!(options.connect_timeout(), Duration::from_secs(7));
    assert_eq!(options.request_timeout(), Duration::from_mins(1));
    Ok(())
}

#[nativelink_test]
async fn test_load_balanced_options_default_connect_timeout() -> Result<(), Error> {
    let options = load_balanced_options(&grpc_endpoint(0), Duration::ZERO)?;

    assert_eq!(options.connect_timeout(), Duration::from_secs(30));
    assert_eq!(options.request_timeout(), Duration::ZERO);
    Ok(())
}

#[nativelink_test]
async fn test_load_balanced_options_require_explicit_port() -> Result<(), Error> {
    let mut config = grpc_endpoint(0);
    config.address = "grpc://example.com".to_string();
    let result = load_balanced_options(&config, Duration::ZERO);
    assert!(matches!(
        result,
        Err(e) if e.to_string().contains("requires an explicit host and port")
    ));
    Ok(())
}

/// The whole point of the DNS-balanced path is that each per-address
/// endpoint carries the full transport configuration (most importantly the
/// HTTP/2 keepalive that detects silently dead peers), unlike the previous
/// ginepro-based implementation which dropped it.
#[nativelink_test]
async fn test_balanced_endpoint_dials_resolved_address() -> Result<(), Error> {
    let options = load_balanced_options(&grpc_endpoint(7), Duration::ZERO)?;
    let endpoint = balanced_endpoint(&options, "127.0.0.1:1234".parse().unwrap())?;
    assert_eq!(endpoint.uri().to_string(), "http://127.0.0.1:1234/");
    Ok(())
}

#[nativelink_test]
async fn test_balanced_endpoint_rejects_tls_config_on_plaintext_scheme() -> Result<(), Error> {
    let mut config = grpc_endpoint(0);
    config.tls_config = Some(ClientTlsConfig {
        use_native_roots: Some(true),
        ca_file: None,
        cert_file: None,
        key_file: None,
    });
    let options = load_balanced_options(&config, Duration::ZERO)?;
    let result = balanced_endpoint(&options, "127.0.0.1:1234".parse().unwrap());
    assert!(matches!(
        result,
        Err(e) if e.to_string().contains("but the scheme is not https or grpcs")
    ));
    Ok(())
}
