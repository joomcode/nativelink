// Copyright 2024 The NativeLink Authors. All rights reserved.
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
use nativelink_error::{Code, Error, make_err, make_input_err};
use tonic::transport::Uri;
use tracing::{info, warn};

pub fn load_client_config(
    config: &Option<ClientTlsConfig>,
) -> Result<Option<tonic::transport::ClientTlsConfig>, Error> {
    let Some(config) = config else {
        return Ok(None);
    };

    if config.use_native_roots == Some(true) {
        if config.ca_file.is_some() {
            warn!("Native root certificates are being used, all certificate files will be ignored");
        }
        return Ok(Some(
            tonic::transport::ClientTlsConfig::new().with_native_roots(),
        ));
    }

    let Some(ca_file) = &config.ca_file else {
        return Err(make_err!(
            Code::Internal,
            "CA certificate must be provided if not using native root certificates"
        ));
    };

    let read_config = tonic::transport::ClientTlsConfig::new().ca_certificate(
        tonic::transport::Certificate::from_pem(std::fs::read_to_string(ca_file)?),
    );
    let config = if let Some(client_certificate) = &config.cert_file {
        let Some(client_key) = &config.key_file else {
            return Err(make_err!(
                Code::Internal,
                "Client certificate specified, but no key"
            ));
        };
        read_config.identity(tonic::transport::Identity::from_pem(
            std::fs::read_to_string(client_certificate)?,
            std::fs::read_to_string(client_key)?,
        ))
    } else {
        if config.key_file.is_some() {
            return Err(make_err!(
                Code::Internal,
                "Client key specified, but no certificate"
            ));
        }
        read_config
    };

    Ok(Some(config))
}

pub fn endpoint_from(
    endpoint: &str,
    tls_config: Option<tonic::transport::ClientTlsConfig>,
) -> Result<tonic::transport::Endpoint, Error> {
    let endpoint = Uri::try_from(endpoint).map_err(|e| {
        Error::from_std_err(Code::Internal, &e)
            .append(format!("Unable to parse endpoint {endpoint}"))
    })?;

    // Tonic uses the TLS configuration if the scheme is "https", so replace
    // grpcs with https.
    let endpoint = if endpoint.scheme_str() == Some("grpcs") {
        let mut parts = endpoint.into_parts();
        parts.scheme = Some("https".parse().map_err(|e| {
            Error::from_std_err(Code::Internal, &e).append("https is an invalid scheme apparently?")
        })?);
        parts.try_into().map_err(|e| {
            Error::from_std_err(Code::Internal, &e).append("Error changing Uri from grpcs to https")
        })?
    } else {
        endpoint
    };

    let endpoint_transport = if let Some(tls_config) = tls_config {
        let Some(authority) = endpoint.authority() else {
            return Err(make_input_err!(
                "Unable to determine authority of endpoint: {endpoint}"
            ));
        };
        if endpoint.scheme_str() != Some("https") {
            return Err(make_input_err!(
                "You have set TLS configuration on {endpoint}, but the scheme is not https or grpcs"
            ));
        }
        let tls_config = tls_config.domain_name(authority.host());
        tonic::transport::Endpoint::from(endpoint)
            .tls_config(tls_config)
            .map_err(|e| {
                Error::from_std_err(Code::InvalidArgument, &e).append("Setting mTLS configuration")
            })?
    } else {
        if endpoint.scheme_str() == Some("https") {
            return Err(make_input_err!(
                "The scheme of {endpoint} is https or grpcs, but no TLS configuration was provided"
            ));
        }
        tonic::transport::Endpoint::from(endpoint)
    };

    Ok(endpoint_transport)
}

/// Configuration for the DNS-balanced connection path, used when a store or
/// scheduler sets `load_balanced_channel: true`.
///
/// The plain path dials the configured URI once per pooled connection and
/// lets the OS resolver pick an address. This path instead re-resolves DNS on
/// every (re)connect and spreads the pooled connections across the resolved
/// addresses. Every connection is a regular tonic channel built from the full
/// `GrpcEndpoint` configuration, so HTTP/2 and TCP keepalive apply — that is
/// what detects a peer that died without closing the connection (e.g. a
/// scaled-in pod) and lets the connection manager replace it.
///
/// This used to be backed by ginepro's `LoadBalancedChannel`, which builds
/// its own internal endpoints with **no keepalive setter**. A silently dead
/// connection was therefore only bounded by `rpc_timeout_s` (often disabled),
/// which is how a CAS scale-in once wedged the worker fleet until the CAS
/// pods were restarted.
#[derive(Clone, Debug)]
pub struct LoadBalancedOptions {
    /// The endpoint configuration that per-address endpoints are built from.
    endpoint_config: GrpcEndpoint,
    /// Per-request timeout applied to every per-address endpoint. Zero
    /// leaves it unset. Comes from `GrpcSpec::rpc_timeout_s`.
    request_timeout: Duration,
}

impl LoadBalancedOptions {
    /// Timeout for DNS resolution and connection establishment.
    #[must_use]
    pub const fn connect_timeout(&self) -> Duration {
        connect_timeout(&self.endpoint_config)
    }

    /// The per-request timeout, zero if disabled.
    #[must_use]
    pub const fn request_timeout(&self) -> Duration {
        self.request_timeout
    }
}

/// Build the balancing settings for `endpoint_config`, used when the store
/// sets `load_balanced_channel: true`. `rpc_timeout` comes from
/// `GrpcSpec::rpc_timeout_s`. Fails at startup if the address cannot be
/// balanced (missing host or port) rather than on first connect.
pub fn load_balanced_options(
    endpoint_config: &GrpcEndpoint,
    rpc_timeout: Duration,
) -> Result<LoadBalancedOptions, Error> {
    let uri = Uri::try_from(endpoint_config.address.as_str()).map_err(|e| {
        Error::from_std_err(Code::InvalidArgument, &e).append(format!(
            "Unable to parse load-balanced endpoint {}",
            endpoint_config.address
        ))
    })?;
    if uri.host().is_none() || uri.port_u16().is_none() {
        return Err(make_input_err!(
            "load_balanced_channel requires an explicit host and port in {}",
            endpoint_config.address
        ));
    }
    // Surface TLS misconfiguration now instead of on first connect.
    load_client_config(&endpoint_config.tls_config)?;
    Ok(LoadBalancedOptions {
        endpoint_config: endpoint_config.clone(),
        request_timeout: rpc_timeout,
    })
}

/// Build an `Endpoint` for one resolved address of a load-balanced target,
/// carrying all transport settings (keepalive, timeouts, TLS) from the
/// original configuration. TLS still validates against the configured
/// hostname, not the dialed address.
pub fn balanced_endpoint(
    options: &LoadBalancedOptions,
    addr: core::net::SocketAddr,
) -> Result<tonic::transport::Endpoint, Error> {
    let config = &options.endpoint_config;
    // Validated in `load_balanced_options`.
    let original = Uri::try_from(config.address.as_str()).map_err(|e| {
        Error::from_std_err(Code::InvalidArgument, &e)
            .append(format!("Unable to parse endpoint {}", config.address))
    })?;
    let is_tls = matches!(original.scheme_str(), Some("grpcs" | "https"));
    let scheme = if is_tls { "https" } else { "http" };
    let mut endpoint = tonic::transport::Endpoint::from_shared(format!("{scheme}://{addr}"))
        .map_err(|e| {
            Error::from_std_err(Code::InvalidArgument, &e).append(format!(
                "Invalid resolved address {addr} for {}",
                config.address
            ))
        })?;
    if is_tls {
        let Some(tls_config) = load_client_config(&config.tls_config)? else {
            return Err(make_input_err!(
                "The scheme of {} is https or grpcs, but no TLS configuration was provided",
                config.address
            ));
        };
        let Some(host) = original.host() else {
            return Err(make_input_err!(
                "Unable to determine host of endpoint: {}",
                config.address
            ));
        };
        endpoint = endpoint
            .tls_config(tls_config.domain_name(host))
            .map_err(|e| {
                Error::from_std_err(Code::InvalidArgument, &e).append("Setting TLS configuration")
            })?;
    } else if config.tls_config.is_some() {
        return Err(make_input_err!(
            "You have set TLS configuration on {}, but the scheme is not https or grpcs",
            config.address
        ));
    }
    endpoint = apply_transport_settings(endpoint, config);
    if !options.request_timeout.is_zero() {
        endpoint = endpoint.timeout(options.request_timeout);
    }
    Ok(endpoint)
}

const fn connect_timeout(endpoint_config: &GrpcEndpoint) -> Duration {
    if endpoint_config.connect_timeout_s > 0 {
        Duration::from_secs(endpoint_config.connect_timeout_s)
    } else {
        Duration::from_secs(30)
    }
}

/// Apply the transport settings from `endpoint_config` — connect timeout,
/// TCP and HTTP/2 keepalive, concurrency limit — to `endpoint`. Used for
/// both the directly-dialed endpoint and every per-address endpoint of a
/// load-balanced target.
fn apply_transport_settings(
    endpoint: tonic::transport::Endpoint,
    endpoint_config: &GrpcEndpoint,
) -> tonic::transport::Endpoint {
    let tcp_keepalive = if endpoint_config.tcp_keepalive_s > 0 {
        Duration::from_secs(endpoint_config.tcp_keepalive_s)
    } else {
        Duration::from_secs(30)
    };
    let http2_keepalive_interval = if endpoint_config.http2_keepalive_interval_s > 0 {
        Duration::from_secs(endpoint_config.http2_keepalive_interval_s)
    } else {
        Duration::from_secs(30)
    };
    let http2_keepalive_timeout = if endpoint_config.http2_keepalive_timeout_s > 0 {
        Duration::from_secs(endpoint_config.http2_keepalive_timeout_s)
    } else {
        Duration::from_secs(20)
    };

    let mut endpoint = endpoint
        .connect_timeout(connect_timeout(endpoint_config))
        .tcp_keepalive(Some(tcp_keepalive))
        .http2_keep_alive_interval(http2_keepalive_interval)
        .keep_alive_timeout(http2_keepalive_timeout)
        .keep_alive_while_idle(true);

    if let Some(concurrency_limit) = endpoint_config.concurrency_limit {
        endpoint = endpoint.concurrency_limit(concurrency_limit);
    }

    endpoint
}

pub fn endpoint(endpoint_config: &GrpcEndpoint) -> Result<tonic::transport::Endpoint, Error> {
    let endpoint = endpoint_from(
        &endpoint_config.address,
        load_client_config(&endpoint_config.tls_config)?,
    )?;

    info!(
        address = %endpoint_config.address,
        concurrency_limit = ?endpoint_config.concurrency_limit,
        connect_timeout_s = connect_timeout(endpoint_config).as_secs(),
        "tls_utils::endpoint: creating gRPC endpoint with keepalive",
    );

    Ok(apply_transport_settings(endpoint, endpoint_config))
}
