//! URL の解析と、HTTP / HTTPS によるページの取得 (本の `URL` クラスに相当)。

use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;

use crate::tls::{self, TlsError};

/// 対応している URL スキーム。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    Http,
    Https,
}

impl Scheme {
    /// ポートが省略されたときに使うポート番号。
    fn default_port(self) -> u16 {
        match self {
            Self::Http => 80,
            Self::Https => 443,
        }
    }
}

/// 解析済みの URL。
///
/// 元の文字列を借用せず `String` で持つ。後の章で履歴やリンク先として、
/// 元の入力より長く保持するため。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    scheme: Scheme,
    host: String,
    port: u16,
    /// 先頭の `/` を含むパスとクエリ (例: `/search?q=rust`)。フラグメントは含まない。
    path: String,
}

impl Url {
    /// `scheme://host[:port][/path][?query][#fragment]` 形式の文字列を解析する。
    pub fn parse(url: &str) -> Result<Self, ParseError> {
        // 前後の空白は取り除く。途中の制御文字 (改行など) や空白は、
        // そのままリクエストに埋め込むとリクエストを壊すので受け付けない
        let url = url.trim();
        if let Some(c) = url.chars().find(|c| c.is_ascii_control() || *c == ' ') {
            return Err(ParseError::InvalidCharacter(c));
        }

        let (scheme, rest) = url.split_once("://").ok_or(ParseError::MissingScheme)?;
        // スキームは大文字・小文字を区別しない (`HTTP://` も可)
        let scheme = if scheme.eq_ignore_ascii_case("http") {
            Scheme::Http
        } else if scheme.eq_ignore_ascii_case("https") {
            Scheme::Https
        } else {
            return Err(ParseError::UnsupportedScheme(scheme.to_owned()));
        };

        // ホストは最初の `/`・`?`・`#` の手前まで
        let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (host, path) = rest.split_at(host_end);
        // フラグメント (`#` 以降) はページ内の位置を表すもので、サーバーには送らない
        let path = path.split_once('#').map_or(path, |(path, _fragment)| path);
        // パスがなければ `/` とみなす (`?q=1` だけなら `/?q=1`)
        let path = if path.starts_with('/') {
            path.to_owned()
        } else {
            format!("/{path}")
        };

        let (host, port) = match host.split_once(':') {
            // `example.org:` のようにポート番号が空なら既定のポートを使う
            Some((host, "")) => (host, scheme.default_port()),
            Some((host, port)) => (host, parse_port(port)?),
            None => (host, scheme.default_port()),
        };
        if host.is_empty() {
            return Err(ParseError::EmptyHost);
        }

        Ok(Self {
            scheme,
            host: host.to_owned(),
            port,
            path,
        })
    }

    /// サーバーに GET リクエストを送り、レスポンスの本文を返す。
    pub fn request(&self) -> Result<String, RequestError> {
        let stream = TcpStream::connect((self.host.as_str(), self.port))?;
        match self.scheme {
            Scheme::Http => self.exchange(stream),
            Scheme::Https => self.exchange(tls::connect(&self.host, stream)?),
        }
    }

    /// 接続済みのストリームでリクエストを送り、レスポンスを読む。
    ///
    /// HTTP と HTTPS の違いは「どんなストリームか」だけなので、ここでは区別しない。
    fn exchange(&self, mut stream: impl Read + Write) -> Result<String, RequestError> {
        // 既定以外のポートは Host ヘッダにも含める (RFC 9110 7.2 節)
        let request = if self.port == self.scheme.default_port() {
            format!("GET {} HTTP/1.0\r\nHost: {}\r\n\r\n", self.path, self.host)
        } else {
            format!(
                "GET {} HTTP/1.0\r\nHost: {}:{}\r\n\r\n",
                self.path, self.host, self.port
            )
        };
        stream.write_all(request.as_bytes())?;
        stream.flush()?;
        read_response(BufReader::new(stream))
    }
}

/// ポート番号を解析する。
fn parse_port(port: &str) -> Result<u16, ParseError> {
    let invalid = || ParseError::InvalidPort(port.to_owned());
    // `str::parse` は `+80` のような符号付きも受け付けるので、数字だけか先に確かめる
    if !port.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    port.parse().map_err(|_| invalid())
}

/// ステータス行とヘッダの合計サイズの上限。
/// 改行を送ってこないサーバーなどに、際限なくメモリを使わせないため。
const MAX_HEADER_BYTES: u64 = 64 * 1024;

/// レスポンスを読み、ステータス行とヘッダを確かめてから本文を返す。
fn read_response(mut reader: impl BufRead) -> Result<String, RequestError> {
    // 各行の読み込みで同じバッファを使い回す
    let mut line = Vec::new();
    let mut header_budget = MAX_HEADER_BYTES;

    read_line(&mut reader, &mut line, &mut header_budget)?;
    check_status_line(&String::from_utf8_lossy(&line))?;

    // 行バッファを使い回すので、ヘッダの名前と値は借用ではなく String として持つ
    let mut headers = HashMap::new();
    loop {
        read_line(&mut reader, &mut line, &mut header_budget)?;
        let header_line = String::from_utf8_lossy(&line);
        if header_line.is_empty() {
            break;
        }
        let (name, value) = header_line.split_once(':').ok_or_else(|| {
            RequestError::MalformedResponse(format!("ヘッダ行に `:` がない: {header_line}"))
        })?;
        // ヘッダ名は大文字・小文字を区別しない (本の `casefold()` に相当)
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
    }

    // 圧縮・チャンク転送にはまだ対応していない
    for name in ["transfer-encoding", "content-encoding"] {
        if let Some(value) = headers.get(name) {
            return Err(RequestError::UnsupportedEncoding {
                header: name,
                value: value.clone(),
            });
        }
    }

    let mut body = Vec::new();
    reader.read_to_end(&mut body)?;
    // 正しい UTF-8 ならバッファをそのまま String にする (コピーしない)。
    // 不正なバイト列があれば U+FFFD に置き換えて表示を続ける
    Ok(String::from_utf8(body)
        .unwrap_or_else(|err| String::from_utf8_lossy(err.as_bytes()).into_owned()))
}

/// 1 行を `line` に読み込み、末尾の改行 (`\r\n` または `\n`) を取り除く。
///
/// 読んだバイト数だけ `budget` を減らす。改行の前に接続が切れたとき、
/// または `budget` を使い切ったときはエラーにする。
fn read_line(
    reader: &mut impl BufRead,
    line: &mut Vec<u8>,
    budget: &mut u64,
) -> Result<(), RequestError> {
    line.clear();
    let read = reader.by_ref().take(*budget).read_until(b'\n', line)?;
    *budget -= read as u64;
    if line.pop() != Some(b'\n') {
        let reason = if *budget == 0 {
            format!("ヘッダが大きすぎる (上限 {} KiB)", MAX_HEADER_BYTES / 1024)
        } else {
            "ヘッダの途中で接続が切れた".to_owned()
        };
        return Err(RequestError::MalformedResponse(reason));
    }
    if line.ends_with(b"\r") {
        line.pop();
    }
    Ok(())
}

/// ステータス行 (例: `HTTP/1.0 200 OK`) が HTTP の形式になっているか確かめる。
///
/// 本では `version, status, explanation` に分けるが、第1章ではどれも使わない。
/// リダイレクト (練習問題 1-7) でステータスコードが必要になったら値を返すようにする。
fn check_status_line(line: &str) -> Result<(), RequestError> {
    let mut parts = line.splitn(3, ' ');
    let version = parts.next().unwrap_or_default();
    let status = parts.next().unwrap_or_default();
    let is_valid = version.starts_with("HTTP/")
        && status.len() == 3
        && status.bytes().all(|b| b.is_ascii_digit());
    if is_valid {
        Ok(())
    } else {
        Err(RequestError::MalformedResponse(format!(
            "ステータス行が不正: {line}"
        )))
    }
}

/// URL の解析に失敗した理由。
#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    /// 制御文字や空白など、URL に使えない文字を含む。
    InvalidCharacter(char),
    /// `://` が見つからない。
    MissingScheme,
    /// 対応していないスキーム。
    UnsupportedScheme(String),
    /// ポート番号として解釈できない。
    InvalidPort(String),
    /// ホスト名が空。
    EmptyHost,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCharacter(c) => write!(f, "URL に使えない文字が含まれています: {c:?}"),
            Self::MissingScheme => write!(f, "URL にスキーム (`http://` など) がありません"),
            Self::UnsupportedScheme(scheme) => {
                write!(f, "対応していないスキームです: {scheme}")
            }
            Self::InvalidPort(port) => write!(f, "ポート番号が不正です: {port}"),
            Self::EmptyHost => write!(f, "ホスト名が空です"),
        }
    }
}

impl Error for ParseError {}

/// ページの取得に失敗した理由。
#[derive(Debug)]
pub enum RequestError {
    /// 接続・送受信の失敗。
    Io(io::Error),
    /// TLS の接続の失敗。
    Tls(TlsError),
    /// HTTP のレスポンスとして解釈できない。
    MalformedResponse(String),
    /// まだ対応していない転送方式・圧縮方式。
    UnsupportedEncoding { header: &'static str, value: String },
}

impl fmt::Display for RequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(_) => write!(f, "サーバーとの通信に失敗しました"),
            Self::Tls(_) => write!(f, "TLS の接続に失敗しました"),
            Self::MalformedResponse(reason) => {
                write!(f, "不正なレスポンスです ({reason})")
            }
            Self::UnsupportedEncoding { header, value } => {
                write!(f, "未対応のエンコーディングです ({header}: {value})")
            }
        }
    }
}

impl Error for RequestError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Tls(err) => Some(err),
            Self::MalformedResponse(_) | Self::UnsupportedEncoding { .. } => None,
        }
    }
}

impl From<io::Error> for RequestError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<TlsError> for RequestError {
    fn from(err: TlsError) -> Self {
        Self::Tls(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    fn url(scheme: Scheme, host: &str, port: u16, path: &str) -> Url {
        Url {
            scheme,
            host: host.to_owned(),
            port,
            path: path.to_owned(),
        }
    }

    #[test]
    fn parses_http_url() {
        assert_eq!(
            Url::parse("http://example.org/index.html"),
            Ok(url(Scheme::Http, "example.org", 80, "/index.html"))
        );
    }

    #[test]
    fn https_defaults_to_port_443() {
        assert_eq!(
            Url::parse("https://example.org/"),
            Ok(url(Scheme::Https, "example.org", 443, "/"))
        );
    }

    #[test]
    fn missing_path_becomes_root() {
        assert_eq!(
            Url::parse("http://example.org"),
            Ok(url(Scheme::Http, "example.org", 80, "/"))
        );
    }

    #[test]
    fn explicit_port_overrides_default() {
        assert_eq!(
            Url::parse("http://localhost:8000/a/b?c=d"),
            Ok(url(Scheme::Http, "localhost", 8000, "/a/b?c=d"))
        );
    }

    #[test]
    fn empty_port_uses_default() {
        assert_eq!(
            Url::parse("http://example.org:/"),
            Ok(url(Scheme::Http, "example.org", 80, "/"))
        );
    }

    #[test]
    fn scheme_is_case_insensitive() {
        assert_eq!(
            Url::parse("HTTPS://example.org/"),
            Ok(url(Scheme::Https, "example.org", 443, "/"))
        );
    }

    #[test]
    fn host_ends_at_query_or_fragment() {
        assert_eq!(
            Url::parse("https://example.org?q=1"),
            Ok(url(Scheme::Https, "example.org", 443, "/?q=1"))
        );
        assert_eq!(
            Url::parse("https://example.org#top"),
            Ok(url(Scheme::Https, "example.org", 443, "/"))
        );
    }

    #[test]
    fn fragment_is_not_part_of_path() {
        assert_eq!(
            Url::parse("http://example.org/a?b=c#d"),
            Ok(url(Scheme::Http, "example.org", 80, "/a?b=c"))
        );
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(
            Url::parse("  http://example.org/\n"),
            Ok(url(Scheme::Http, "example.org", 80, "/"))
        );
    }

    #[test]
    fn rejects_invalid_urls() {
        assert_eq!(Url::parse("example.org"), Err(ParseError::MissingScheme));
        assert_eq!(
            Url::parse("ftp://example.org/"),
            Err(ParseError::UnsupportedScheme("ftp".to_owned()))
        );
        assert_eq!(
            Url::parse("http://example.org:http/"),
            Err(ParseError::InvalidPort("http".to_owned()))
        );
        assert_eq!(
            Url::parse("http://example.org:70000/"),
            Err(ParseError::InvalidPort("70000".to_owned()))
        );
        assert_eq!(
            Url::parse("http://example.org:+80/"),
            Err(ParseError::InvalidPort("+80".to_owned()))
        );
        assert_eq!(Url::parse("http:///index.html"), Err(ParseError::EmptyHost));
    }

    #[test]
    fn rejects_characters_that_would_break_the_request() {
        assert_eq!(
            Url::parse("http://example.org/a\r\nX-Injected: 1"),
            Err(ParseError::InvalidCharacter('\r'))
        );
        assert_eq!(
            Url::parse("http://example.org/a b"),
            Err(ParseError::InvalidCharacter(' '))
        );
    }

    #[test]
    fn reads_body_after_headers() {
        let response = b"HTTP/1.0 200 OK\r\nContent-Type: text/html\r\n\r\n<p>hi</p>";
        assert_eq!(read_response(&response[..]).unwrap(), "<p>hi</p>");
    }

    #[test]
    fn accepts_bare_lf_line_endings() {
        let response = b"HTTP/1.1 404 Not Found\nServer: test\n\nnot found";
        assert_eq!(read_response(&response[..]).unwrap(), "not found");
    }

    #[test]
    fn replaces_invalid_utf8_in_body() {
        let response = b"HTTP/1.0 200 OK\r\n\r\na\xffb";
        assert_eq!(read_response(&response[..]).unwrap(), "a\u{FFFD}b");
    }

    #[test]
    fn rejects_encoded_responses_regardless_of_header_case() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Encoding:  gzip \r\n\r\n";
        assert!(matches!(
            read_response(&response[..]),
            Err(RequestError::UnsupportedEncoding { header: "content-encoding", value })
                if value == "gzip"
        ));

        let response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n";
        assert!(matches!(
            read_response(&response[..]),
            Err(RequestError::UnsupportedEncoding {
                header: "transfer-encoding",
                ..
            })
        ));

        // ヘッダ名と `:` の間に空白があっても見逃さない
        let response = b"HTTP/1.1 200 OK\r\nContent-Encoding : gzip\r\n\r\n";
        assert!(matches!(
            read_response(&response[..]),
            Err(RequestError::UnsupportedEncoding {
                header: "content-encoding",
                ..
            })
        ));
    }

    #[test]
    fn rejects_malformed_responses() {
        let cases: [&[u8]; 5] = [
            b"",                                    // 何も返ってこない
            b"hello\r\n\r\n",                       // HTTP ではない
            b"HTTP/1.0 200 OK\r\nno-colon\r\n\r\n", // `:` のないヘッダ
            b"HTTP/1.0 200 OK\r\nServer: test\r\n", // 空行の前に切れた
            b"HTTP/1.0 200 OK\r\nServer: test",     // 改行の前に切れた
        ];
        for response in cases {
            assert!(
                matches!(
                    read_response(response),
                    Err(RequestError::MalformedResponse(_))
                ),
                "{:?}",
                String::from_utf8_lossy(response)
            );
        }
    }

    #[test]
    fn rejects_oversized_headers() {
        let mut response = b"HTTP/1.0 200 OK\r\nX-Long: ".to_vec();
        let header_limit = usize::try_from(MAX_HEADER_BYTES).unwrap();
        response.resize(header_limit + 1, b'a');
        response.extend_from_slice(b"\r\n\r\nbody");
        assert!(matches!(
            read_response(&response[..]),
            Err(RequestError::MalformedResponse(reason)) if reason.contains("大きすぎる")
        ));
    }

    /// テスト用の HTTP サーバーを 1 回だけ応答する形で立てる。
    /// 戻り値はポート番号と、受け取ったリクエストを返すスレッド。
    fn serve_once(response: &'static [u8]) -> (u16, thread::JoinHandle<String>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut request = String::new();
            while !request.ends_with("\r\n\r\n") {
                if reader.read_line(&mut request).unwrap() == 0 {
                    break;
                }
            }
            reader.get_mut().write_all(response).unwrap();
            request
        });
        (port, handle)
    }

    #[test]
    fn sends_get_request_and_returns_body() {
        let (port, server) = serve_once(b"HTTP/1.0 200 OK\r\n\r\nHello");
        let url = Url::parse(&format!("http://127.0.0.1:{port}/index.html")).unwrap();

        assert_eq!(url.request().unwrap(), "Hello");
        // 既定以外のポートなので Host ヘッダにもポート番号が付く
        assert_eq!(
            server.join().unwrap(),
            format!("GET /index.html HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n")
        );
    }

    /// 書き込まれた内容を記録し、決まったレスポンスを返すテスト用のストリーム。
    struct MockStream {
        response: io::Cursor<&'static [u8]>,
        written: Vec<u8>,
    }

    impl Read for MockStream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.response.read(buf)
        }
    }

    impl Write for MockStream {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.written.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn host_header_omits_default_port() {
        let mut stream = MockStream {
            response: io::Cursor::new(b"HTTP/1.0 200 OK\r\n\r\n"),
            written: Vec::new(),
        };
        let url = Url::parse("https://example.org/a?b=c#d").unwrap();

        url.exchange(&mut stream).unwrap();
        assert_eq!(
            String::from_utf8(stream.written).unwrap(),
            "GET /a?b=c HTTP/1.0\r\nHost: example.org\r\n\r\n"
        );
    }
}
