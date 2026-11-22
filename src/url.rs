//! URL の解析と、HTTP / HTTPS によるページの取得 (本の `URL` クラスに相当)。
//!
//! HTTP のメッセージの読み書きは `http` モジュール、暗号化は `tls` モジュールに任せ、
//! ここではどこへ接続するか (URL の解析、リダイレクトの追従) を受け持つ。

use std::borrow::Cow;
use std::error::Error;
use std::fmt;
use std::io;
use std::net::TcpStream;

use crate::http::{self, HttpError};
use crate::tls::{self, TlsError};

/// たどるリダイレクトの回数の上限 (Chrome や Firefox と同じ)。
const MAX_REDIRECTS: usize = 20;

/// 対応している URL スキーム。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    Http,
    Https,
}

impl Scheme {
    /// URL に書くときの名前。
    fn name(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }

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
        let url = url.trim();
        check_characters(url)?;

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
            path: request_path(path),
        })
    }

    /// `self` を基準に、リダイレクト先などの URL を解決する
    /// (本の第7章で作る `URL.resolve` のうち、練習問題 1-7 に必要な分)。
    ///
    /// 対応しているのは次の 3 つの形。`next.html` のような相対パスは第7章で対応する。
    /// - `https://example.org/a`: 完全な URL
    /// - `//example.org/a`: スキームだけを引き継ぐ
    /// - `/a`: スキーム・ホスト・ポートを引き継ぐ
    pub fn resolve(&self, url: &str) -> Result<Self, ParseError> {
        let url = url.trim();
        if let Some(rest) = url.strip_prefix("//") {
            return Self::parse(&format!("{}://{rest}", self.scheme.name()));
        }
        if url.starts_with('/') {
            check_characters(url)?;
            return Ok(Self {
                scheme: self.scheme,
                host: self.host.clone(),
                port: self.port,
                path: request_path(url),
            });
        }
        if has_scheme(url) {
            return Self::parse(url);
        }
        Err(ParseError::RelativeUrl(url.to_owned()))
    }

    /// サーバーに GET リクエストを送り、レスポンスの本文を返す。
    /// リダイレクトのレスポンスなら、`Location` ヘッダの URL をたどる (練習問題 1-7)。
    pub fn request(&self) -> Result<String, RequestError> {
        // 最初は `self` を借用し、リダイレクトしたときだけ新しい URL を持つ
        let mut url = Cow::Borrowed(self);
        let mut redirects = 0;
        loop {
            let response = url.fetch()?;
            let Some(location) = redirect_location(&response) else {
                return Ok(decode_text(response.body));
            };
            // 上限を超えたら、移動先の URL を解釈する前に打ち切る
            if redirects == MAX_REDIRECTS {
                return Err(RequestError::TooManyRedirects);
            }
            redirects += 1;
            let next = url
                .resolve(location)
                .map_err(|source| RequestError::InvalidRedirect {
                    location: location.to_owned(),
                    source,
                })?;
            url = Cow::Owned(next);
        }
    }

    /// 1 回だけリクエストを送り、レスポンスを受け取る (リダイレクトはたどらない)。
    ///
    /// HTTP と HTTPS の違いは「どんなストリームの上で読み書きするか」だけ。
    fn fetch(&self) -> Result<http::Response, RequestError> {
        let stream =
            TcpStream::connect((self.host.as_str(), self.port)).map_err(RequestError::Connect)?;
        let host = self.host_header();
        let response = match self.scheme {
            Scheme::Http => http::get(stream, &host, &self.path)?,
            Scheme::Https => http::get(tls::connect(&self.host, stream)?, &host, &self.path)?,
        };
        Ok(response)
    }

    /// Host ヘッダの値。既定以外のポートはポート番号も含める (RFC 9110 7.2 節)。
    fn host_header(&self) -> Cow<'_, str> {
        if self.port == self.scheme.default_port() {
            Cow::Borrowed(&self.host)
        } else {
            Cow::Owned(format!("{}:{}", self.host, self.port))
        }
    }
}

/// 制御文字 (改行など) や空白を含んでいないか確かめる。
/// そのままリクエストに埋め込むと、リクエストを壊してしまうため。
fn check_characters(url: &str) -> Result<(), ParseError> {
    match url.chars().find(|c| c.is_ascii_control() || *c == ' ') {
        Some(c) => Err(ParseError::InvalidCharacter(c)),
        None => Ok(()),
    }
}

/// パス・クエリ・フラグメントの部分から、リクエストに使うパスを作る。
///
/// フラグメント (`#` 以降) はページ内の位置を表すもので、サーバーには送らない。
/// パスがなければ `/` とみなす (`?q=1` だけなら `/?q=1`)。
/// リクエスト行には ASCII しか書けないので、ASCII 以外の文字は UTF-8 のバイトごとに
/// `%XX` へ変換する (例: `日` → `%E6%97%A5`。RFC 3986 2.1 節)。
fn request_path(path: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let path = path.split_once('#').map_or(path, |(path, _fragment)| path);
    let mut encoded = String::with_capacity(path.len() + 1);
    if !path.starts_with('/') {
        encoded.push('/');
    }
    for &b in path.as_bytes() {
        if b.is_ascii() {
            encoded.push(char::from(b));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(b >> 4)]));
            encoded.push(char::from(HEX[usize::from(b & 0x0F)]));
        }
    }
    encoded
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

/// `scheme:` で始まる (相対 URL ではない) か (スキームの形式は RFC 3986 3.1 節)。
fn has_scheme(url: &str) -> bool {
    url.split_once(':').is_some_and(|(scheme, _)| {
        let mut chars = scheme.chars();
        chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    })
}

/// リダイレクトのレスポンスなら、`Location` ヘッダの値を返す。
///
/// 300 (Multiple Choices) と 304 (Not Modified) は移動先を示すものではないので含めない。
/// `Location` がなければ、リダイレクトではなく普通のページとして表示する。
fn redirect_location(response: &http::Response) -> Option<&str> {
    if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
        response.headers.get("location")
    } else {
        None
    }
}

/// 本文のバイト列を文字列にする。
///
/// 正しい UTF-8 ならバッファをそのまま String にする (コピーしない)。
/// 不正なバイト列があれば U+FFFD に置き換えて表示を続ける (文字コードの判定は Issue #2)。
fn decode_text(body: Vec<u8>) -> String {
    String::from_utf8(body)
        .unwrap_or_else(|err| String::from_utf8_lossy(err.as_bytes()).into_owned())
}

/// URL の解析に失敗した理由。
///
/// エラーは解析した文字列より長く使われる (呼び出し元へ返して表示する) ので、
/// 問題の部分は借用せず `String` で持つ。
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
    /// まだ解決できない形の相対 URL。
    RelativeUrl(String),
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
            Self::RelativeUrl(url) => {
                write!(f, "この形の相対 URL にはまだ対応していません: {url}")
            }
        }
    }
}

impl Error for ParseError {}

/// ページの取得に失敗した理由。
///
/// リダイレクト先の URL は、それを含んでいたレスポンスを捨てたあとも表示に使うので
/// `String` で持つ。
#[derive(Debug)]
pub enum RequestError {
    /// サーバーに接続できない (名前解決の失敗を含む)。
    Connect(io::Error),
    /// TLS の接続の失敗。
    Tls(TlsError),
    /// HTTP の送受信の失敗。
    Http(HttpError),
    /// リダイレクト先の URL を解釈できない。
    InvalidRedirect {
        location: String,
        source: ParseError,
    },
    /// リダイレクトが上限の回数を超えた (ループしている可能性が高い)。
    TooManyRedirects,
}

impl fmt::Display for RequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(_) => write!(f, "サーバーに接続できませんでした"),
            Self::Tls(_) => write!(f, "TLS の接続に失敗しました"),
            // HTTP の失敗はそれ自体が説明になっているので、そのまま見せる
            Self::Http(err) => err.fmt(f),
            Self::InvalidRedirect { location, .. } => {
                write!(f, "リダイレクト先の URL が不正です: {location}")
            }
            Self::TooManyRedirects => {
                write!(f, "リダイレクトが多すぎます (上限 {MAX_REDIRECTS} 回)")
            }
        }
    }
}

impl Error for RequestError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Connect(err) => Some(err),
            Self::Tls(err) => Some(err),
            Self::Http(err) => err.source(),
            Self::InvalidRedirect { source, .. } => Some(source),
            Self::TooManyRedirects => None,
        }
    }
}

impl From<TlsError> for RequestError {
    fn from(err: TlsError) -> Self {
        Self::Tls(err)
    }
}

impl From<HttpError> for RequestError {
    fn from(err: HttpError) -> Self {
        Self::Http(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
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
    fn percent_encodes_non_ascii_path() {
        assert_eq!(
            Url::parse("https://ja.wikipedia.org/wiki/日本?q=あ#節"),
            Ok(url(
                Scheme::Https,
                "ja.wikipedia.org",
                443,
                "/wiki/%E6%97%A5%E6%9C%AC?q=%E3%81%82"
            ))
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
    fn resolves_absolute_url() {
        let base = url(Scheme::Https, "example.org", 8443, "/a");
        assert_eq!(
            base.resolve("http://example.com/b"),
            Ok(url(Scheme::Http, "example.com", 80, "/b"))
        );
    }

    #[test]
    fn resolves_scheme_relative_url() {
        let base = url(Scheme::Https, "example.org", 8443, "/a");
        assert_eq!(
            base.resolve("//example.com/b"),
            Ok(url(Scheme::Https, "example.com", 443, "/b"))
        );
    }

    #[test]
    fn resolves_path_absolute_url() {
        let base = url(Scheme::Https, "example.org", 8443, "/a");
        assert_eq!(
            base.resolve("/b?c=d#e"),
            Ok(url(Scheme::Https, "example.org", 8443, "/b?c=d"))
        );
    }

    #[test]
    fn rejects_unsupported_redirect_targets() {
        let base = url(Scheme::Http, "example.org", 80, "/a/b");
        assert_eq!(
            base.resolve("next.html"),
            Err(ParseError::RelativeUrl("next.html".to_owned()))
        );
        assert_eq!(
            base.resolve("/a\r\nX-Injected: 1"),
            Err(ParseError::InvalidCharacter('\r'))
        );
        assert_eq!(
            base.resolve("ftp://example.org/"),
            Err(ParseError::UnsupportedScheme("ftp".to_owned()))
        );
    }

    #[test]
    fn host_header_omits_default_port() {
        assert_eq!(
            url(Scheme::Https, "example.org", 443, "/").host_header(),
            "example.org"
        );
        assert_eq!(
            url(Scheme::Http, "example.org", 8080, "/").host_header(),
            "example.org:8080"
        );
    }

    #[test]
    fn shows_http_errors_as_is() {
        let err = RequestError::from(HttpError::MalformedResponse("x".to_owned()));
        assert_eq!(err.to_string(), "不正なレスポンスです (x)");
    }

    #[test]
    fn replaces_invalid_utf8_in_body() {
        assert_eq!(decode_text(b"a\xffb".to_vec()), "a\u{FFFD}b");
    }

    /// テスト用の HTTP サーバーを立て、接続ごとに `responses` を先頭から 1 つずつ返す。
    ///
    /// `responses` にはポート番号が渡る (リダイレクト先の URL を作るため)。
    /// 戻り値はポート番号と、受け取ったリクエストを順に返すスレッド。
    fn serve(responses: impl FnOnce(u16) -> Vec<String>) -> (u16, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let responses = responses(port);
        let handle = thread::spawn(move || {
            responses
                .iter()
                .map(|response| {
                    let (stream, _) = listener.accept().unwrap();
                    let mut reader = BufReader::new(stream);
                    let mut request = String::new();
                    while !request.ends_with("\r\n\r\n") {
                        if reader.read_line(&mut request).unwrap() == 0 {
                            break;
                        }
                    }
                    reader.get_mut().write_all(response.as_bytes()).unwrap();
                    request
                })
                .collect()
        });
        (port, handle)
    }

    #[test]
    fn sends_get_request_and_returns_body() {
        let (port, server) = serve(|_| vec!["HTTP/1.1 200 OK\r\n\r\nHello".to_owned()]);
        let url = Url::parse(&format!("http://127.0.0.1:{port}/index.html")).unwrap();

        assert_eq!(url.request().unwrap(), "Hello");
        // 既定以外のポートなので Host ヘッダにもポート番号が付く
        assert_eq!(
            server.join().unwrap(),
            [format!(
                "GET /index.html HTTP/1.1\r\n\
                 Host: 127.0.0.1:{port}\r\n\
                 Connection: close\r\n\
                 User-Agent: my-browser/{}\r\n\
                 \r\n",
                env!("CARGO_PKG_VERSION")
            )]
        );
    }

    #[test]
    fn follows_redirects() {
        let (port, server) = serve(|port| {
            vec![
                "HTTP/1.1 301 Moved Permanently\r\nLocation: /second\r\nContent-Length: 0\r\n\r\n"
                    .to_owned(),
                format!(
                    "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{port}/third\r\n\r\nmoved"
                ),
                format!(
                    "HTTP/1.1 307 Temporary Redirect\r\nLocation: //127.0.0.1:{port}/last\r\n\r\n"
                ),
                "HTTP/1.1 200 OK\r\n\r\narrived".to_owned(),
            ]
        });
        let url = Url::parse(&format!("http://127.0.0.1:{port}/first")).unwrap();

        assert_eq!(url.request().unwrap(), "arrived");
        let paths: Vec<_> = server
            .join()
            .unwrap()
            .iter()
            .map(|request| request.split(' ').nth(1).unwrap().to_owned())
            .collect();
        assert_eq!(paths, ["/first", "/second", "/third", "/last"]);
    }

    #[test]
    fn shows_redirect_without_location_as_page() {
        let (port, _server) = serve(|_| vec!["HTTP/1.1 302 Found\r\n\r\nno location".to_owned()]);
        let url = Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();
        assert_eq!(url.request().unwrap(), "no location");
    }

    #[test]
    fn gives_up_after_too_many_redirects() {
        // 自分自身へのリダイレクトを繰り返すサーバー。上限を超えた回の移動先は
        // 解釈できない形にして、解釈する前に打ち切っていることも確かめる
        let (port, server) = serve(|_| {
            let mut responses =
                vec!["HTTP/1.1 302 Found\r\nLocation: /loop\r\n\r\n".to_owned(); MAX_REDIRECTS];
            responses.push("HTTP/1.1 302 Found\r\nLocation: next.html\r\n\r\n".to_owned());
            responses
        });
        let url = Url::parse(&format!("http://127.0.0.1:{port}/loop")).unwrap();

        assert!(matches!(url.request(), Err(RequestError::TooManyRedirects)));
        // 最初の 1 回 + リダイレクト 20 回で打ち切っている
        assert_eq!(server.join().unwrap().len(), MAX_REDIRECTS + 1);
    }

    #[test]
    fn reports_unsupported_redirect_target() {
        let (port, _server) =
            serve(|_| vec!["HTTP/1.1 301 Moved\r\nLocation: next.html\r\n\r\n".to_owned()]);
        let url = Url::parse(&format!("http://127.0.0.1:{port}/a/")).unwrap();

        assert!(matches!(
            url.request(),
            Err(RequestError::InvalidRedirect {
                location,
                source: ParseError::RelativeUrl(_),
            }) if location == "next.html"
        ));
    }
}
