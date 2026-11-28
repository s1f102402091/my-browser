//! URL の解析とページの取得 (本の `URL` クラスに相当)。
//!
//! 対応しているスキームは `http:` / `https:` / `file:` / `data:` と、
//! その前に付ける `view-source:`。
//! HTTP のメッセージの読み書きは `http` モジュール、暗号化は `tls` モジュール、
//! ローカルファイルは `file` モジュールに任せ、ここではどこから何を取ってくるかを受け持つ。

use std::borrow::Cow;
use std::error::Error;
use std::fmt;
use std::io;
use std::net::TcpStream;
use std::path::PathBuf;

use crate::file::{self, PathError, ReadError};
use crate::http::{self, HttpError};
use crate::percent;
use crate::tls::{self, TlsError};

/// たどるリダイレクトの回数の上限 (Chrome や Firefox と同じ)。
const MAX_REDIRECTS: usize = 20;

/// URL を指定せずに起動したときに開くページ (練習問題 1-2)。
/// パスはビルド時に埋め込むので、リポジトリを移動したらビルドし直す。
const DEFAULT_PAGE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/default.html");

/// 解析済みの URL。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    /// `view-source:` が付いていたか (練習問題 1-5)。取得の仕方は同じで、表示の仕方だけが変わる。
    view_source: bool,
    target: Target,
}

/// URL が指している取得先。スキームによって持つ情報が違う。
///
/// どの値も、解析した文字列を借用せず所有して持つ。`Url` は元の入力より長く使い、
/// パスや内容は `%XX` を戻すなどして新しく作った値でもあるため。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    /// `http:` / `https:`
    Http(HttpUrl),
    /// `file:`。ローカルファイルの絶対パス。
    File(PathBuf),
    /// `data:`。URL に埋め込まれた内容 (`%XX` を戻したもの)。
    Data(String),
}

impl Url {
    /// URL の文字列を解析する。
    pub fn parse(url: &str) -> Result<Self, ParseError> {
        let url = url.trim();
        let (view_source, url) = match strip_prefix_ignore_case(url, "view-source:") {
            // `view-source: https://...` のように空白を挟んでいてもよい
            Some(rest) => (true, rest.trim_start()),
            None => (false, url),
        };
        if view_source && strip_prefix_ignore_case(url, "view-source:").is_some() {
            return Err(ParseError::NestedViewSource);
        }

        let target = if let Some(rest) = strip_prefix_ignore_case(url, "file:") {
            Target::File(file::path_from_url(rest).map_err(ParseError::FilePath)?)
        } else if let Some(rest) = strip_prefix_ignore_case(url, "data:") {
            Target::Data(parse_data(rest)?)
        } else {
            Target::Http(HttpUrl::parse(url)?)
        };
        Ok(Self {
            view_source,
            target,
        })
    }

    /// URL を指定せずに起動したときに開く既定のページ (リポジトリの `assets/default.html`)。
    pub fn default_page() -> Self {
        Self {
            view_source: false,
            target: Target::File(PathBuf::from(DEFAULT_PAGE)),
        }
    }

    /// `view-source:` が付いていたか。
    pub fn is_view_source(&self) -> bool {
        self.view_source
    }

    /// ページの内容を取得する。
    ///
    /// data URL の内容は URL 自体が持っているので、コピーせずに借用して返す。
    pub fn request(&self) -> Result<Cow<'_, str>, RequestError> {
        match &self.target {
            Target::Http(url) => Ok(Cow::Owned(url.request()?)),
            Target::File(path) => Ok(Cow::Owned(decode_text(file::read(path)?))),
            Target::Data(content) => Ok(Cow::Borrowed(content)),
        }
    }
}

/// `s` が `prefix` で始まっていれば (大文字・小文字は区別しない)、残りを返す。
fn strip_prefix_ignore_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

/// data URL の `data:` より後ろ (例: `text/html,Hello world!`) から内容を取り出す (練習問題 1-3)。
///
/// メディアタイプ (`,` の前) は今は使わず、内容はすべて HTML として表示する。
fn parse_data(rest: &str) -> Result<String, ParseError> {
    // フラグメント (`#` 以降) は内容に含めない (WHATWG の URL Standard と同じ)
    let rest = rest.split_once('#').map_or(rest, |(rest, _fragment)| rest);
    let (media_type, data) = rest.split_once(',').ok_or(ParseError::MissingDataComma)?;
    // base64 で符号化された内容 (`;base64` で終わるもの) にはまだ対応しない
    let is_base64 = media_type
        .rsplit_once(';')
        .is_some_and(|(_, param)| param.trim().eq_ignore_ascii_case("base64"));
    if is_base64 {
        return Err(ParseError::UnsupportedBase64);
    }
    Ok(decode_text(percent::decode(data)))
}

/// 対応している HTTP のスキーム。
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

/// `http:` / `https:` の URL。
///
/// 元の文字列を借用せず `String` で持つ。後の章で履歴やリンク先として、
/// 元の入力より長く保持するため。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpUrl {
    scheme: Scheme,
    host: String,
    port: u16,
    /// 先頭の `/` を含むパスとクエリ (例: `/search?q=rust`)。フラグメントは含まない。
    path: String,
}

impl HttpUrl {
    /// `scheme://host[:port][/path][?query][#fragment]` 形式の文字列を解析する。
    fn parse(url: &str) -> Result<Self, ParseError> {
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
        // 国際化ドメイン名 (IDN) の変換にはまだ対応しない (Issue #8)
        if !host.is_ascii() {
            return Err(ParseError::NonAsciiHost(host.to_owned()));
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
    ///
    /// 戻り値は `HttpUrl` なので、リダイレクトで `file:` や `data:` へ移ることは型の上でありえない。
    fn resolve(&self, url: &str) -> Result<Self, ParseError> {
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
        if let Some(scheme) = scheme_of(url) {
            // Web ページの側からローカルファイルなどを開かせないよう、http と https 以外へは移らない
            if !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")) {
                return Err(ParseError::NotHttp(scheme.to_owned()));
            }
            return Self::parse(url);
        }
        Err(ParseError::RelativeUrl(url.to_owned()))
    }

    /// サーバーに GET リクエストを送り、レスポンスの本文を返す。
    /// リダイレクトのレスポンスなら、`Location` ヘッダの URL をたどる (練習問題 1-7)。
    fn request(&self) -> Result<String, RequestError> {
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
/// リクエスト行には ASCII しか書けないので、ASCII 以外の文字は `%XX` に変換する。
fn request_path(path: &str) -> String {
    let path = path.split_once('#').map_or(path, |(path, _fragment)| path);
    let mut encoded = String::with_capacity(path.len() + 1);
    if !path.starts_with('/') {
        encoded.push('/');
    }
    percent::encode_non_ascii(path, &mut encoded);
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

/// `scheme:` で始まる (相対 URL ではない) なら、そのスキームを返す
/// (スキームの形式は RFC 3986 3.1 節)。
fn scheme_of(url: &str) -> Option<&str> {
    let (scheme, _) = url.split_once(':')?;
    let mut chars = scheme.chars();
    let is_scheme = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    is_scheme.then_some(scheme)
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
    /// ASCII 以外の文字を含むホスト名 (国際化ドメイン名)。
    NonAsciiHost(String),
    /// まだ解決できない形の相対 URL。
    RelativeUrl(String),
    /// リダイレクトなどで、http / https 以外の URL へ移ろうとした。
    NotHttp(String),
    /// `view-source:` が二重に付いている。
    NestedViewSource,
    /// data URL に、メディアタイプと内容を区切る `,` がない。
    MissingDataComma,
    /// base64 で符号化された data URL。
    UnsupportedBase64,
    /// file URL をローカルファイルのパスにできない。
    FilePath(PathError),
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
            Self::NonAsciiHost(host) => write!(
                f,
                "国際化ドメイン名にはまだ対応していません。`xn--` の形で指定してください: {host}"
            ),
            Self::RelativeUrl(url) => {
                write!(f, "この形の相対 URL にはまだ対応していません: {url}")
            }
            Self::NotHttp(scheme) => write!(
                f,
                "移動できるのは http と https の URL だけです ({scheme}: には移動できません)"
            ),
            Self::NestedViewSource => write!(f, "`view-source:` は 1 つだけ付けてください"),
            Self::MissingDataComma => write!(f, "data URL に `,` がありません"),
            Self::UnsupportedBase64 => {
                write!(f, "base64 の data URL にはまだ対応していません")
            }
            Self::FilePath(_) => write!(f, "file URL を解釈できません"),
        }
    }
}

impl Error for ParseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::FilePath(err) => Some(err),
            _ => None,
        }
    }
}

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
    /// ローカルファイルを読めない。
    File(ReadError),
}

impl fmt::Display for RequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(_) => write!(f, "サーバーに接続できませんでした"),
            Self::Tls(_) => write!(f, "TLS の接続に失敗しました"),
            // HTTP とファイルの失敗はそれ自体が説明になっているので、そのまま見せる
            Self::Http(err) => err.fmt(f),
            Self::File(err) => err.fmt(f),
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
            Self::File(err) => err.source(),
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

impl From<ReadError> for RequestError {
    fn from(err: ReadError) -> Self {
        Self::File(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::{env, fs, thread};

    fn http(scheme: Scheme, host: &str, port: u16, path: &str) -> HttpUrl {
        HttpUrl {
            scheme,
            host: host.to_owned(),
            port,
            path: path.to_owned(),
        }
    }

    fn url(scheme: Scheme, host: &str, port: u16, path: &str) -> Url {
        Url {
            view_source: false,
            target: Target::Http(http(scheme, host, port, path)),
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
    fn rejects_non_ascii_host() {
        assert_eq!(
            Url::parse("https://日本語.jp/"),
            Err(ParseError::NonAsciiHost("日本語.jp".to_owned()))
        );
        // punycode の形 (`xn--`) ならそのまま使う
        assert_eq!(
            Url::parse("https://xn--wgv71a119e.jp/"),
            Ok(url(Scheme::Https, "xn--wgv71a119e.jp", 443, "/"))
        );
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
    fn parses_view_source() {
        let mut expected = url(Scheme::Http, "example.org", 80, "/");
        expected.view_source = true;
        assert_eq!(
            Url::parse("view-source:http://example.org/"),
            Ok(expected.clone())
        );
        assert_eq!(Url::parse("VIEW-SOURCE:http://example.org/"), Ok(expected));
        assert_eq!(
            Url::parse("view-source:view-source:http://example.org/"),
            Err(ParseError::NestedViewSource)
        );
        // 空白を挟んでも、file: や data: と組み合わせられる
        let source = Url::parse("view-source: data:text/html,x").unwrap();
        assert!(source.is_view_source());
        assert_eq!(source.target, Target::Data("x".to_owned()));
        assert_eq!(
            Url::parse("view-source: view-source:http://example.org/"),
            Err(ParseError::NestedViewSource)
        );
        assert!(!Url::parse("http://example.org/").unwrap().is_view_source());
    }

    #[test]
    fn parses_data_url() {
        let data = |content: &str| Url {
            view_source: false,
            target: Target::Data(content.to_owned()),
        };
        // 本の例 (空白を含んでいてもよい)
        assert_eq!(
            Url::parse("data:text/html,Hello world!"),
            Ok(data("Hello world!"))
        );
        assert_eq!(
            Url::parse("DATA:text/html;charset=utf-8,%3Cb%3E%E6%97%A5%3C/b%3E 100%"),
            Ok(data("<b>日</b> 100%"))
        );
        // `,` は最初のものだけが区切り。フラグメントは含めない
        assert_eq!(Url::parse("data:,a,b#c"), Ok(data("a,b")));
    }

    #[test]
    fn rejects_unsupported_data_urls() {
        assert_eq!(
            Url::parse("data:text/html"),
            Err(ParseError::MissingDataComma)
        );
        assert_eq!(
            Url::parse("data:text/html;base64,PGI+"),
            Err(ParseError::UnsupportedBase64)
        );
        assert_eq!(
            Url::parse("data:text/html; Base64 ,PGI+"),
            Err(ParseError::UnsupportedBase64)
        );
        // `;` がなければ base64 ではない (メディアタイプが `base64` という名前なだけ)
        assert!(matches!(
            Url::parse("data:base64,hello").map(|url| url.target),
            Ok(Target::Data(content)) if content == "hello"
        ));
    }

    #[test]
    fn data_url_is_borrowed_without_copy() {
        let url = Url::parse("data:text/html,<p>hi</p>").unwrap();
        assert!(matches!(url.request(), Ok(Cow::Borrowed("<p>hi</p>"))));
    }

    #[test]
    fn reports_invalid_file_url() {
        let err = Url::parse("file://server/share/a.html").unwrap_err();
        assert_eq!(
            err,
            ParseError::FilePath(PathError::Network("server".to_owned()))
        );
        // 原因 (共有フォルダは開けない) まで表示できる
        assert!(err.source().is_some());
    }

    #[test]
    fn reads_file_url() {
        let path = env::temp_dir().join(format!("my-browser-{}-url.html", std::process::id()));
        fs::write(&path, "<p>ファイル</p>").unwrap();
        // `C:\Users\...` を `file:///C:/Users/...` の形にする (Unix では `file:///tmp/...`)
        let path_part = path.to_str().unwrap().replace('\\', "/");
        let url = Url::parse(&format!("file:///{}", path_part.trim_start_matches('/'))).unwrap();
        let body = url.request().map(Cow::into_owned);
        fs::remove_file(&path).unwrap();
        assert_eq!(body.unwrap(), "<p>ファイル</p>");
    }

    #[test]
    fn default_page_exists() {
        let url = Url::default_page();
        assert!(url.request().unwrap().contains("my-browser"));
    }

    #[test]
    fn resolves_absolute_url() {
        let base = http(Scheme::Https, "example.org", 8443, "/a");
        assert_eq!(
            base.resolve("http://example.com/b"),
            Ok(http(Scheme::Http, "example.com", 80, "/b"))
        );
    }

    #[test]
    fn resolves_scheme_relative_url() {
        let base = http(Scheme::Https, "example.org", 8443, "/a");
        assert_eq!(
            base.resolve("//example.com/b"),
            Ok(http(Scheme::Https, "example.com", 443, "/b"))
        );
    }

    #[test]
    fn resolves_path_absolute_url() {
        let base = http(Scheme::Https, "example.org", 8443, "/a");
        assert_eq!(
            base.resolve("/b?c=d#e"),
            Ok(http(Scheme::Https, "example.org", 8443, "/b?c=d"))
        );
    }

    #[test]
    fn rejects_unsupported_redirect_targets() {
        let base = http(Scheme::Http, "example.org", 80, "/a/b");
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
            Err(ParseError::NotHttp("ftp".to_owned()))
        );
    }

    #[test]
    fn redirects_never_open_local_files_or_other_schemes() {
        let base = http(Scheme::Https, "example.org", 443, "/");
        for (location, scheme) in [
            ("file:///C:/Users/a/.ssh/id_ed25519", "file"),
            ("FILE:///etc/passwd", "FILE"),
            ("data:text/html,<p>phishing</p>", "data"),
            ("view-source:https://example.org/", "view-source"),
            ("javascript:alert(1)", "javascript"),
        ] {
            assert_eq!(
                base.resolve(location),
                Err(ParseError::NotHttp(scheme.to_owned())),
                "{location}"
            );
        }
    }

    #[test]
    fn host_header_omits_default_port() {
        assert_eq!(
            http(Scheme::Https, "example.org", 443, "/").host_header(),
            "example.org"
        );
        assert_eq!(
            http(Scheme::Http, "example.org", 8080, "/").host_header(),
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

    #[test]
    fn redirect_to_local_file_is_refused() {
        let (port, _server) = serve(|_| {
            vec!["HTTP/1.1 302 Found\r\nLocation: file:///C:/Windows/win.ini\r\n\r\n".to_owned()]
        });
        let url = Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();

        assert!(matches!(
            url.request(),
            Err(RequestError::InvalidRedirect {
                source: ParseError::NotHttp(_),
                ..
            })
        ));
    }
}
