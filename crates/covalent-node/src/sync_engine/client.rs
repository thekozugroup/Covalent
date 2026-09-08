use std::fmt;
use std::net::SocketAddr;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use http_body_util::{BodyExt as _, Full};
use hyper::body::Bytes;
use hyper::client::conn::http1;
use hyper::http::{HeaderValue, Method, Request, header};
use hyper_util::rt::TokioIo;
use rustls::pki_types::{CertificateDer, ServerName};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpStream, UnixStream};
use tokio_rustls::TlsConnector;
use uuid::Uuid;
use zeroize::Zeroizing;

const MAX_CONFIGURATION_BYTES: usize = 8 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Fixed redacted errors. Upstream bodies, paths and credentials are never
/// included, even when a broken or hostile local server returns them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineApiError {
    /// The private endpoint or credential does not satisfy the adapter contract.
    InvalidConfiguration,
    /// The socket, its directory or its peer is not owned and private.
    UnsafeEndpoint,
    /// The worker was unavailable or the HTTP exchange was invalid.
    Unavailable,
    /// The whole exchange exceeded its deadline, including body streaming.
    Timeout,
    /// A request or response exceeded its fixed byte budget.
    BodyTooLarge,
    /// The server returned a non-success status. Redirects are never followed.
    HttpStatus(u16),
    /// The response was not an uncompressed JSON value of the expected shape.
    InvalidResponse,
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;

impl fmt::Display for EngineApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidConfiguration => "invalid private engine configuration",
            Self::UnsafeEndpoint => "private engine endpoint is not secure",
            Self::Unavailable => "folder sync engine is unavailable",
            Self::Timeout => "folder sync engine request timed out",
            Self::BodyTooLarge => "folder sync engine message exceeded its limit",
            Self::HttpStatus(_) => "folder sync engine rejected the request",
            Self::InvalidResponse => "folder sync engine returned an invalid response",
        })
    }
}

impl std::error::Error for EngineApiError {}

/// The small control surface required by the adapter. Paths cannot come from
/// arbitrary URLs, filenames or peer input. In-place version restore is absent.
#[derive(Clone, Copy, Debug)]
pub enum EngineEndpoint {
    /// Engine identity and process status.
    SystemStatus,
    /// Exact upstream build version.
    SystemVersion,
    /// Current private engine configuration.
    Configuration,
    /// Connection state for configured engine peers.
    Connections,
    /// Configured folder's current scan/pull status.
    FolderStatus(Uuid),
    /// Reported scan and pull errors for one configured folder.
    ///
    /// The page size is fixed here so callers cannot accidentally request an
    /// unbounded list whose entries may contain private local paths.
    FolderErrors(Uuid),
    /// Archive metadata for one configured folder. This is read-only; the
    /// unsafe upstream in-place restore endpoint is deliberately absent.
    FolderVersions(Uuid),
    /// Ask the engine to scan one configured folder.
    ScanFolder(Uuid),
    /// Stop the engine gracefully.
    Shutdown,
}

impl EngineEndpoint {
    fn method(self) -> Method {
        match self {
            Self::ScanFolder(_) | Self::Shutdown => Method::POST,
            _ => Method::GET,
        }
    }

    const fn response_limit(self) -> usize {
        if matches!(self, Self::Configuration) {
            MAX_CONFIGURATION_BYTES
        } else {
            MAX_RESPONSE_BYTES
        }
    }

    fn path(self) -> String {
        match self {
            Self::SystemStatus => "/rest/system/status".to_owned(),
            Self::SystemVersion => "/rest/system/version".to_owned(),
            Self::Configuration => "/rest/config".to_owned(),
            Self::Connections => "/rest/system/connections".to_owned(),
            Self::FolderStatus(id) => format!("/rest/db/status?folder={id}"),
            Self::FolderErrors(id) => format!("/rest/folder/errors?folder={id}&page=1&perpage=128"),
            Self::FolderVersions(id) => format!("/rest/folder/versions?folder={id}"),
            Self::ScanFolder(id) => format!("/rest/db/scan?folder={id}"),
            Self::Shutdown => "/rest/system/shutdown".to_owned(),
        }
    }
}

/// An owning-process-only REST client. It opens a fresh bounded HTTP/1
/// connection for each request. No driver tasks survive completion, timeout or
/// cancellation; no DNS, proxy, TCP fallback or redirect handler is involved.
trait ClientIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> ClientIo for T {}

enum EngineTransport {
    Unix(PathBuf),
    LoopbackTls {
        address: SocketAddr,
        expected_leaf: Vec<u8>,
        config: Arc<rustls::ClientConfig>,
    },
}

pub struct EngineApiClient {
    #[cfg(test)]
    socket_path: PathBuf,
    transport: EngineTransport,
    api_key: Zeroizing<String>,
    timeout: Duration,
}

impl fmt::Debug for EngineApiClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EngineApiClient([PRIVATE])")
    }
}

impl EngineApiClient {
    /// Accepts an absolute socket beneath a controller-owned private directory
    /// and a freshly generated 256-bit hexadecimal key. The directory must stay
    /// owned by the controller for this client's lifetime, with a canonical
    /// path beneath an OS-protected state root. Ancestors must not be
    /// replaceable by another user. Path, owner, socket type and inode checks
    /// run before and after connecting, before any credential is sent. This is
    /// not a defense against a malicious process running as the same user.
    pub fn new(socket_path: PathBuf, api_key: Zeroizing<String>) -> Result<Self, EngineApiError> {
        if !socket_path.is_absolute()
            || socket_path.as_os_str().as_bytes().len() > 100
            || socket_path.as_os_str().as_bytes().contains(&0)
            || socket_path
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
            || api_key.len() != 64
            || !api_key.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(EngineApiError::InvalidConfiguration);
        }
        let parent = socket_path
            .parent()
            .ok_or(EngineApiError::InvalidConfiguration)?;
        if std::fs::canonicalize(parent).map_err(|_| EngineApiError::Unavailable)? != parent {
            return Err(EngineApiError::UnsafeEndpoint);
        }
        Ok(Self {
            #[cfg(test)]
            socket_path: socket_path.clone(),
            transport: EngineTransport::Unix(socket_path),
            api_key,
            timeout: REQUEST_TIMEOUT,
        })
    }

    /// Connects only to a numeric loopback GUI endpoint, with ordinary
    /// localhost certificate verification and an exact leaf DER pin before an
    /// HTTP request can be constructed.
    pub fn new_loopback_tls(
        address: SocketAddr,
        api_key: Zeroizing<String>,
        expected_leaf: Vec<u8>,
    ) -> Result<Self, EngineApiError> {
        if !address.ip().is_loopback()
            || address.port() == 0
            || matches!(address, SocketAddr::V6(value) if value.scope_id() != 0 || value.flowinfo() != 0)
            || expected_leaf.is_empty()
            || expected_leaf.len() > 16 * 1024
            || api_key.len() != 64
            || !api_key.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(EngineApiError::InvalidConfiguration);
        }
        let certificate = CertificateDer::from(expected_leaf.clone());
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(certificate)
            .map_err(|_| EngineApiError::InvalidConfiguration)?;
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(Self {
            #[cfg(test)]
            socket_path: PathBuf::new(),
            transport: EngineTransport::LoopbackTls {
                address,
                expected_leaf,
                config: Arc::new(config),
            },
            api_key,
            timeout: REQUEST_TIMEOUT,
        })
    }

    /// Decode a JSON response with an explicit expected type. Unknown or
    /// malformed shapes are rejected by that type's Deserialize implementation.
    pub async fn json<T: DeserializeOwned>(
        &self,
        endpoint: EngineEndpoint,
    ) -> Result<T, EngineApiError> {
        let bytes = self
            .exchange(
                endpoint.method(),
                endpoint.path(),
                Bytes::new(),
                endpoint.response_limit(),
            )
            .await?;
        serde_json::from_slice(&bytes).map_err(|_| EngineApiError::InvalidResponse)
    }

    /// Request a scan or graceful shutdown. Endpoints that return data must use
    /// `json`; this method cannot accidentally accept a failed JSON decode.
    pub async fn command(&self, endpoint: EngineEndpoint) -> Result<(), EngineApiError> {
        if !matches!(
            endpoint,
            EngineEndpoint::ScanFolder(_) | EngineEndpoint::Shutdown
        ) {
            return Err(EngineApiError::InvalidConfiguration);
        }
        self.exchange(
            endpoint.method(),
            endpoint.path(),
            Bytes::new(),
            endpoint.response_limit(),
        )
        .await
        .map(|_| ())
    }

    /// Apply the controller's complete desired config. Callers must serialize
    /// this with reconciliation and verify the resulting effective config.
    /// Neither this value nor returned configuration may be logged or sent to a
    /// peer; the engine GUI key lives inside this private boundary.
    pub async fn replace_configuration<T: Serialize>(
        &self,
        configuration: &T,
    ) -> Result<(), EngineApiError> {
        let mut body = LimitedWriter {
            bytes: Vec::new(),
            limit: MAX_CONFIGURATION_BYTES,
        };
        if serde_json::to_writer(&mut body, configuration).is_err() {
            return Err(EngineApiError::BodyTooLarge);
        }
        self.exchange(
            Method::PUT,
            "/rest/config".to_owned(),
            Bytes::from(body.bytes),
            MAX_CONFIGURATION_BYTES,
        )
        .await
        .map(|_| ())
    }

    fn verify_endpoint(&self, socket_path: &Path) -> Result<(u64, u64, u64, u64), EngineApiError> {
        let parent = socket_path
            .parent()
            .ok_or(EngineApiError::InvalidConfiguration)?;
        if std::fs::canonicalize(parent).map_err(|_| EngineApiError::Unavailable)? != parent {
            return Err(EngineApiError::UnsafeEndpoint);
        }
        let directory =
            std::fs::symlink_metadata(parent).map_err(|_| EngineApiError::Unavailable)?;
        let socket =
            std::fs::symlink_metadata(socket_path).map_err(|_| EngineApiError::Unavailable)?;
        let uid = rustix::process::geteuid().as_raw();
        if !directory.is_dir()
            || directory.uid() != uid
            || directory.mode() & 0o077 != 0
            || !socket.file_type().is_socket()
            || socket.uid() != uid
            || socket.mode() & 0o077 != 0
        {
            return Err(EngineApiError::UnsafeEndpoint);
        }
        Ok((directory.dev(), directory.ino(), socket.dev(), socket.ino()))
    }

    async fn exchange(
        &self,
        method: Method,
        path: String,
        body: Bytes,
        response_limit: usize,
    ) -> Result<Vec<u8>, EngineApiError> {
        tokio::time::timeout(self.timeout, async {
            let io = match &self.transport {
                EngineTransport::Unix(socket_path) => {
                    let endpoint_identity = self.verify_endpoint(socket_path)?;
                    let stream = UnixStream::connect(socket_path)
                        .await
                        .map_err(|_| EngineApiError::Unavailable)?;
                    if self.verify_endpoint(socket_path)? != endpoint_identity {
                        return Err(EngineApiError::UnsafeEndpoint);
                    }
                    let credential = stream
                        .peer_cred()
                        .map_err(|_| EngineApiError::UnsafeEndpoint)?;
                    if credential.uid() != rustix::process::geteuid().as_raw() {
                        return Err(EngineApiError::UnsafeEndpoint);
                    }
                    TokioIo::new(Box::new(stream) as Box<dyn ClientIo>)
                }
                EngineTransport::LoopbackTls {
                    address,
                    expected_leaf,
                    config,
                } => {
                    let stream = TcpStream::connect(*address)
                        .await
                        .map_err(|_| EngineApiError::Unavailable)?;
                    let name = ServerName::try_from("localhost")
                        .map_err(|_| EngineApiError::InvalidConfiguration)?;
                    let tls = TlsConnector::from(config.clone())
                        .connect(name, stream)
                        .await
                        .map_err(|_| EngineApiError::Unavailable)?;
                    let certs = tls
                        .get_ref()
                        .1
                        .peer_certificates()
                        .ok_or(EngineApiError::Unavailable)?;
                    if certs.len() != 1 || certs[0].as_ref() != expected_leaf.as_slice() {
                        return Err(EngineApiError::Unavailable);
                    }
                    TokioIo::new(Box::new(tls) as Box<dyn ClientIo>)
                }
            };
            let (mut sender, connection) = http1::Builder::new()
                .max_headers(32)
                .max_buf_size(16 * 1024)
                .handshake(io)
                .await
                .map_err(|_| EngineApiError::Unavailable)?;
            let mut key = HeaderValue::from_str(self.api_key.as_str())
                .map_err(|_| EngineApiError::InvalidConfiguration)?;
            key.set_sensitive(true);
            let request = Request::builder()
                .method(method)
                .uri(path)
                .header(header::HOST, "localhost")
                .header(header::CONNECTION, "close")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/json")
                .header("x-api-key", key)
                .body(Full::new(body))
                .map_err(|_| EngineApiError::InvalidConfiguration)?;
            let exchange = async move {
                let response = sender
                    .send_request(request)
                    .await
                    .map_err(|_| EngineApiError::Unavailable)?;
                if !response.status().is_success() {
                    return Err(EngineApiError::HttpStatus(response.status().as_u16()));
                }
                if let Some(encoding) = response.headers().get(header::CONTENT_ENCODING)
                    && encoding != "identity"
                {
                    return Err(EngineApiError::InvalidResponse);
                }
                if let Some(length) = response.headers().get(header::CONTENT_LENGTH) {
                    let length = length
                        .to_str()
                        .ok()
                        .and_then(|value| value.parse::<u64>().ok())
                        .ok_or(EngineApiError::InvalidResponse)?;
                    if length > response_limit as u64 {
                        return Err(EngineApiError::BodyTooLarge);
                    }
                }
                let mut body = response.into_body();
                let mut bytes = Vec::new();
                while let Some(frame) = body.frame().await {
                    let frame = frame.map_err(|_| EngineApiError::Unavailable)?;
                    if let Some(data) = frame.data_ref() {
                        if data.len() > response_limit.saturating_sub(bytes.len()) {
                            return Err(EngineApiError::BodyTooLarge);
                        }
                        bytes.extend_from_slice(data);
                    }
                }
                Ok(bytes)
            };
            tokio::pin!(exchange);
            // A peer may close immediately after its complete response. The
            // driver can finish before the response future consumes buffered
            // frames, so the exchange determines success (and detects a short
            // body). Both futures stay scoped to this request and its deadline.
            tokio::select! {
                biased;
                result = &mut exchange => result,
                _ = connection => exchange.await,
            }
        })
        .await
        .map_err(|_| EngineApiError::Timeout)?
    }
}

struct LimitedWriter {
    bytes: Vec<u8>,
    limit: usize,
}

impl std::io::Write for LimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("engine request limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
