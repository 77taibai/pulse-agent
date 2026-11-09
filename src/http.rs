//! A tiny blocking HTTP/1.1 client.
//!
//! Written on top of `std::net::TcpStream` so the only external crate needed is
//! a TLS implementation (`rustls`). It deliberately supports just what the
//! Pulse API requires:
//!
//! * one request per connection (`Connection: close`),
//! * `Content-Length` and `chunked` response bodies,
//! * `http://` and `https://` endpoints.
//!
//! TLS certificates are verified against the Mozilla root store bundled by
//! `webpki-roots`; `--insecure` installs a verifier that accepts any chain,
//! which is useful for panels behind a self-signed certificate.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme, StreamOwned,
};

const USER_AGENT: &str = concat!("pulse-agent/", env!("CARGO_PKG_VERSION"));

pub struct Response {
    pub status: u16,
    pub body: String,
}

/// A parsed `http(s)://host[:port]/path` URL.
#[derive(Debug, Clone)]
pub struct Url {
    pub https: bool,
    pub host: String,
    pub port: u16,
    pub path: String,
}

impl Url {
    pub fn parse(input: &str) -> Result<Url, String> {
        let (https, rest) = if let Some(r) = input.strip_prefix("https://") {
            (true, r)
        } else if let Some(r) = input.strip_prefix("http://") {
            (false, r)
        } else {
            return Err(format!(
                "URL 必须以 http:// 或 https:// 开头，收到: {}",
                input
            ));
        };

        let default_port = if https { 443 } else { 80 };

        let (authority, path) = match rest.find(|c| c == '/' || c == '?' || c == '#') {
            Some(i) => (&rest[..i], rest[i..].to_string()),
            None => (rest, "/".to_string()),
        };

        let (host, port) = if let Some(after_bracket) = authority.strip_prefix('[') {
            // IPv6 literal: [::1] or [::1]:8443
            let end = after_bracket
                .find(']')
                .ok_or_else(|| format!("IPv6 地址缺少 ']': {}", authority))?;
            let host = after_bracket[..end].to_string();
            let tail = &after_bracket[end + 1..];
            let port = if let Some(p) = tail.strip_prefix(':') {
                p.parse::<u16>()
                    .map_err(|_| format!("无效端口: {}", p))?
            } else if tail.is_empty() {
                default_port
            } else {
                return Err(format!("无效的 URL 主机部分: {}", authority));
            };
            (host, port)
        } else if let Some(idx) = authority.rfind(':') {
            let host = authority[..idx].to_string();
            let port_str = &authority[idx + 1..];
            let port = port_str
                .parse::<u16>()
                .map_err(|_| format!("无效端口: {}", port_str))?;
            (host, port)
        } else {
            (authority.to_string(), default_port)
        };

        if host.is_empty() {
            return Err(format!("URL 缺少主机名: {}", input));
        }

        // Normalise an empty path to "/".
        let path = if path.is_empty() {
            "/".to_string()
        } else if path.starts_with('?') {
            format!("/{}", path)
        } else {
            path
        };

        Ok(Url {
            https,
            host,
            port,
            path,
        })
    }

    fn host_header(&self) -> String {
        let default_port = if self.https { 443 } else { 80 };
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        if self.port == default_port {
            host
        } else {
            format!("{}:{}", host, self.port)
        }
    }
}

/// Read/write wrapper that hides whether the socket is plaintext or TLS.
enum Stream {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

pub struct Client {
    tls: Arc<ClientConfig>,
    timeout: Duration,
}

impl Client {
    pub fn new(insecure: bool, timeout: Duration) -> Result<Client, String> {
        Ok(Client {
            tls: build_tls_config(insecure)?,
            timeout,
        })
    }

    pub fn get(&self, url: &str, api_key: &str) -> Result<Response, String> {
        self.request("GET", url, api_key, None)
    }

    pub fn post_json(&self, url: &str, api_key: &str, body: &str) -> Result<Response, String> {
        self.request("POST", url, api_key, Some(body))
    }

    fn request(
        &self,
        method: &str,
        url: &str,
        api_key: &str,
        body: Option<&str>,
    ) -> Result<Response, String> {
        let parsed = Url::parse(url)?;

        let addrs = (parsed.host.as_str(), parsed.port)
            .to_socket_addrs()
            .map_err(|e| format!("无法解析主机 {}: {}", parsed.host, e))?;

        let mut last_err = format!("无法连接 {}", parsed.host);
        let mut connected: Option<TcpStream> = None;
        for addr in addrs {
            match TcpStream::connect_timeout(&addr, self.timeout) {
                Ok(sock) => {
                    connected = Some(sock);
                    break;
                }
                Err(e) => last_err = format!("连接 {} 失败: {}", addr, e),
            }
        }
        let tcp = connected.ok_or(last_err)?;

        tcp.set_read_timeout(Some(self.timeout)).ok();
        tcp.set_write_timeout(Some(self.timeout)).ok();
        tcp.set_nodelay(true).ok();

        let mut stream = if parsed.https {
            let server_name = ServerName::try_from(parsed.host.clone())
                .map_err(|e| format!("无效的 TLS 主机名 {}: {}", parsed.host, e))?;
            let conn = ClientConnection::new(self.tls.clone(), server_name)
                .map_err(|e| format!("TLS 初始化失败: {}", e))?;
            Stream::Tls(Box::new(StreamOwned::new(conn, tcp)))
        } else {
            Stream::Plain(tcp)
        };

        let mut head = String::new();
        head.push_str(method);
        head.push(' ');
        head.push_str(&parsed.path);
        head.push_str(" HTTP/1.1\r\nHost: ");
        head.push_str(&parsed.host_header());
        head.push_str("\r\nUser-Agent: ");
        head.push_str(USER_AGENT);
        head.push_str("\r\nAccept: application/json\r\nConnection: close\r\n");
        if !api_key.is_empty() {
            head.push_str("Authorization: Bearer ");
            head.push_str(api_key);
            head.push_str("\r\n");
        }
        if let Some(body) = body {
            head.push_str("Content-Type: application/json; charset=utf-8\r\nContent-Length: ");
            head.push_str(&body.len().to_string());
            head.push_str("\r\n");
        }
        head.push_str("\r\n");

        let mut request_bytes = head.into_bytes();
        if let Some(body) = body {
            request_bytes.extend_from_slice(body.as_bytes());
        }

        stream
            .write_all(&request_bytes)
            .map_err(|e| format!("发送请求失败: {}", e))?;
        stream
            .flush()
            .map_err(|e| format!("发送请求失败: {}", e))?;

        let mut reader = BufReader::new(stream);
        read_response(&mut reader)
    }
}

fn read_response<R: BufRead>(reader: &mut R) -> Result<Response, String> {
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .map_err(|e| format!("读取响应失败: {}", e))?;
    if status_line.is_empty() {
        return Err("服务器未返回任何数据".to_string());
    }

    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| format!("无法解析状态行: {}", status_line.trim()))?;

    let mut headers: Vec<(String, String)> = Vec::new();
    loop {
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .map_err(|e| format!("读取响应头失败: {}", e))?;
        if n == 0 {
            break;
        }
        let trimmed = line.trim_end_matches(|c| c == '\r' || c == '\n');
        if trimmed.is_empty() {
            break;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }

    let chunked = headers.iter().any(|(k, v)| {
        k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked")
    });

    let mut body: Vec<u8> = Vec::new();

    if chunked {
        loop {
            let mut size_line = String::new();
            let n = reader
                .read_line(&mut size_line)
                .map_err(|e| format!("读取分块长度失败: {}", e))?;
            if n == 0 {
                break;
            }
            let size_field = size_line.trim();
            let size_field = size_field.split(';').next().unwrap_or("").trim();
            if size_field.is_empty() {
                continue;
            }
            let size = usize::from_str_radix(size_field, 16)
                .map_err(|_| format!("无效的分块长度: {}", size_field))?;
            if size == 0 {
                // Consume optional trailers up to the terminating blank line.
                loop {
                    let mut trailer = String::new();
                    let n = reader
                        .read_line(&mut trailer)
                        .map_err(|e| format!("读取分块结尾失败: {}", e))?;
                    if n == 0 || trailer.trim().is_empty() {
                        break;
                    }
                }
                break;
            }
            let mut chunk = vec![0u8; size];
            reader
                .read_exact(&mut chunk)
                .map_err(|e| format!("读取分块内容失败: {}", e))?;
            body.extend_from_slice(&chunk);
            let mut crlf = String::new();
            reader
                .read_line(&mut crlf)
                .map_err(|e| format!("读取分块结束符失败: {}", e))?;
        }
    } else if let Some(len) = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse::<usize>().ok())
    {
        let mut buf = vec![0u8; len];
        reader
            .read_exact(&mut buf)
            .map_err(|e| format!("读取响应体失败: {}", e))?;
        body = buf;
    } else {
        reader
            .read_to_end(&mut body)
            .map_err(|e| format!("读取响应体失败: {}", e))?;
    }

    Ok(Response {
        status,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

fn build_tls_config(insecure: bool) -> Result<Arc<ClientConfig>, String> {
    // With `default-features = false` there is no process-wide default
    // provider, so it must be supplied explicitly.
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let schemes = provider.signature_verification_algorithms.supported_schemes();

    let builder = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS 配置失败: {}", e))?;

    let config = if insecure {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerifier { schemes }))
            .with_no_client_auth()
    } else {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        builder.with_root_certificates(roots).with_no_client_auth()
    };

    Ok(Arc::new(config))
}

/// Certificate verifier that accepts everything. Only used with `--insecure`.
#[derive(Debug)]
struct NoVerifier {
    schemes: Vec<SignatureScheme>,
}

impl ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};

    fn parse(raw: &[u8]) -> Response {
        let mut reader = BufReader::new(Cursor::new(raw.to_vec()));
        read_response(&mut reader).expect("response should parse")
    }

    #[test]
    fn parses_content_length_response() {
        let raw = b"HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: 11\r\n\r\n{\"ok\":true}";
        let resp = parse(raw);
        assert_eq!(resp.status, 201);
        assert_eq!(resp.body, "{\"ok\":true}");
    }

    #[test]
    fn parses_chunked_response() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\n\r\n";
        let resp = parse(raw);
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, "{\"a\":1}");
    }

    #[test]
    fn parses_urls() {
        let a = Url::parse("https://panel.example.com").unwrap();
        assert!(a.https);
        assert_eq!(a.host, "panel.example.com");
        assert_eq!(a.port, 443);
        assert_eq!(a.path, "/");

        let b = Url::parse("http://127.0.0.1:8080/api/v1/report?x=1").unwrap();
        assert!(!b.https);
        assert_eq!(b.port, 8080);
        assert_eq!(b.path, "/api/v1/report?x=1");

        let c = Url::parse("https://[::1]:8443/x").unwrap();
        assert_eq!(c.host, "::1");
        assert_eq!(c.port, 8443);
    }

    #[test]
    fn rejects_bad_scheme() {
        assert!(Url::parse("ftp://example.com").is_err());
        assert!(Url::parse("example.com").is_err());
    }
}
