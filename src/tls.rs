//! TLS による暗号化通信 (本の `ssl.create_default_context().wrap_socket(...)` に相当)。
//!
//! TLS の実装 (現在は native-tls。Windows では OS の SChannel を使う) に依存するのはこのファイルだけ。
//! 呼び出し側には `Read + Write` なストリームとしてだけ見せるので、
//! rustls などに差し替えるときはこのファイルの中を書き換えれば済む。

use std::error::Error;
use std::fmt;
use std::io::{self, Read, Write};
use std::net::TcpStream;

use native_tls::{HandshakeError, TlsConnector};

/// TCP 接続を TLS で包む。証明書は OS の証明書ストアで検証する。
///
/// `host` は証明書の検証と SNI に使うホスト名 (ポート番号を含めない)。
pub fn connect(host: &str, stream: TcpStream) -> Result<impl Read + Write, TlsError> {
    let connector = TlsConnector::new().map_err(TlsError::new)?;
    connector.connect(host, stream).map_err(|err| match err {
        HandshakeError::Failure(err) => TlsError::new(err),
        // ブロッキングソケットでは通常起こらないが、起きてもページの読み込み失敗として扱う
        HandshakeError::WouldBlock(_) => TlsError::new(io::Error::new(
            io::ErrorKind::WouldBlock,
            "TLS ハンドシェイクが中断された",
        )),
    })
}

/// TLS の接続に失敗した理由。
///
/// 実装ごとのエラー型を外に出さないよう、中身は `Box<dyn Error>` で包む。
#[derive(Debug)]
pub struct TlsError(Box<dyn Error + Send + Sync>);

impl TlsError {
    fn new(err: impl Error + Send + Sync + 'static) -> Self {
        Self(Box::new(err))
    }
}

impl fmt::Display for TlsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl Error for TlsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.0.source()
    }
}
