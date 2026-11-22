//! HTTP/1.1 のメッセージの読み書き (本の `URL.request` のうち、ソケットに書いて読む部分)。
//!
//! 接続先の決定や TLS は扱わない。渡された `Read + Write` なストリームにリクエストを書き、
//! レスポンスを読むことだけを受け持つ。

use std::error::Error;
use std::fmt;
use std::io::{self, BufRead, BufReader, Read, Write};

/// User-Agent ヘッダの値 (例: `my-browser/0.1.0`)。バージョンは Cargo.toml から埋め込む。
///
/// OS や CPU の種類は含めない。ブラウザの指紋 (利用者を見分ける手がかり) になる情報を
/// 増やさないため (Chrome の User-Agent Reduction と同じ考え方)。
const USER_AGENT: &str = concat!("my-browser/", env!("CARGO_PKG_VERSION"));

/// ステータス行とヘッダの合計サイズの上限 (チャンクのサイズ行とトレーラーにも使う)。
/// 改行を送ってこないサーバーなどに、際限なくメモリを使わせないため。
const MAX_HEADER_BYTES: u64 = 64 * 1024;

/// サーバーから受け取ったレスポンス。
#[derive(Debug)]
pub struct Response {
    /// ステータスコード (例: 200, 404)。
    pub status: u16,
    pub headers: Headers,
    /// 転送のための符号化 (チャンク転送など) を戻したあとの本文。
    pub body: Vec<u8>,
}

/// レスポンスヘッダ。
///
/// 本では辞書 (`dict`) に入れるが、1 つのレスポンスのヘッダは多くても数十個なので、
/// `HashMap` ではなく `Vec` に入れて先頭から探す。ハッシュ表のための領域を使わずに済む。
#[derive(Debug, Default)]
pub struct Headers(Vec<(String, String)>);

impl Headers {
    /// 名前でヘッダの値を探す。名前の大文字・小文字は区別しない。
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// ヘッダを加え、値を入れた要素の位置を返す。名前は小文字にそろえる (本の `casefold()` に相当)。
    ///
    /// 同じ名前のヘッダが既にあれば、値を `, ` でつないで 1 つにまとめる (RFC 9110 5.3 節)。
    /// `Set-Cookie` だけはまとめると意味が変わるので、クッキーを扱う章で見直す。
    fn append(&mut self, name: &str, value: &str) -> usize {
        match self
            .0
            .iter()
            .position(|(n, _)| n.eq_ignore_ascii_case(name))
        {
            Some(index) => {
                let existing = &mut self.0[index].1;
                existing.push_str(", ");
                existing.push_str(value);
                index
            }
            None => {
                // 行バッファは次の行の読み込みで上書きするので、借用ではなく String として持つ
                self.0.push((name.to_ascii_lowercase(), value.to_owned()));
                self.0.len() - 1
            }
        }
    }

    /// 折り返された行 (obs-fold) の続きを、`index` の位置のヘッダの値に空白 1 つでつなぐ。
    fn continue_value(&mut self, index: usize, continuation: &str) {
        if !continuation.is_empty() {
            let value = &mut self.0[index].1;
            value.push(' ');
            value.push_str(continuation);
        }
    }
}

/// GET リクエストを送り、レスポンスを読む。
///
/// `host` は Host ヘッダの値 (既定以外のポートなら `host:port`)、
/// `path` はリクエストするパスとクエリ (例: `/search?q=rust`)。
pub fn get(mut stream: impl Read + Write, host: &str, path: &str) -> Result<Response, HttpError> {
    stream.write_all(request_text(host, path).as_bytes())?;
    stream.flush()?;
    read_response(&mut BufReader::new(stream))
}

/// GET リクエストの文字列を組み立てる。
fn request_text(host: &str, path: &str) -> String {
    // 送るヘッダの一覧。ヘッダを増やすときはここに足す (練習問題 1-1)
    let headers = [
        ("Host", host),
        // HTTP/1.1 は既定で接続を使い回すので、1 回の応答で閉じてもらう
        // (使い回しは練習問題 1-6 で扱う)
        ("Connection", "close"),
        ("User-Agent", USER_AGENT),
    ];
    let mut request = format!("GET {path} HTTP/1.1\r\n");
    for (name, value) in headers {
        request.push_str(name);
        request.push_str(": ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    request
}

/// レスポンスを読む。
fn read_response(reader: &mut impl BufRead) -> Result<Response, HttpError> {
    // 各行の読み込みで同じバッファを使い回す
    let mut line = Vec::new();
    let mut budget = MAX_HEADER_BYTES;

    let (status, headers) = loop {
        read_line(reader, &mut line, &mut budget, "ステータス行")?;
        let status = parse_status_line(&String::from_utf8_lossy(&line))?;
        // 101 の後ろは HTTP ではない別のプロトコルになる。`Upgrade` を送っていないのに
        // 切り替えられたら、続きを HTTP として読んでも意味がない
        if status == 101 {
            return Err(HttpError::MalformedResponse(
                "要求していないプロトコルの切り替え (101)".to_owned(),
            ));
        }
        let headers = read_headers(reader, &mut line, &mut budget, "ヘッダ")?;
        // 1xx (103 Early Hints など) は途中経過の知らせなので読み飛ばし、
        // 最終的なレスポンスを待つ (RFC 9110 15.2 節)。
        // 上限 `budget` は共有しているので、1xx を送り続けられても止まる
        if !(100..200).contains(&status) {
            break (status, headers);
        }
    };

    let body = read_body(reader, status, &headers)?;
    Ok(Response {
        status,
        headers,
        body,
    })
}

/// ステータス行 (例: `HTTP/1.1 200 OK`) からステータスコードを取り出す。
///
/// 本では `version, status, explanation` に分けるが、使うのはステータスコードだけ。
fn parse_status_line(line: &str) -> Result<u16, HttpError> {
    let mut parts = line.splitn(3, ' ');
    let version = parts.next().unwrap_or_default();
    let status = parts.next().unwrap_or_default();
    let is_valid = version.starts_with("HTTP/")
        && status.len() == 3
        && status.bytes().all(|b| b.is_ascii_digit());
    if !is_valid {
        return Err(HttpError::MalformedResponse(format!(
            "ステータス行が不正: {line}"
        )));
    }
    Ok(status.parse().expect("3 桁の数字は u16 に収まる"))
}

/// 空行までのヘッダ行を読む。`what` はエラーメッセージに使う名前 (ヘッダかトレーラーか)。
fn read_headers(
    reader: &mut impl BufRead,
    line: &mut Vec<u8>,
    budget: &mut u64,
    what: &str,
) -> Result<Headers, HttpError> {
    let mut headers = Headers::default();
    // 直前に読んだヘッダの位置 (折り返された行をつなぐため)
    let mut last = None;
    loop {
        read_line(reader, line, budget, what)?;
        if line.is_empty() {
            return Ok(headers);
        }
        let header_line = String::from_utf8_lossy(line);
        // 空白で始まる行は、前の行の値の続き (今は廃止された折り返し「obs-fold」)。
        // ブラウザは空白に置き換えて受け付けなければならない (RFC 9112 5.2 節)
        if header_line.starts_with([' ', '\t']) {
            let index = last.ok_or_else(|| {
                HttpError::MalformedResponse(format!("最初の{what}行が空白で始まっている"))
            })?;
            headers.continue_value(index, header_line.trim());
            continue;
        }
        let (name, value) = header_line.split_once(':').ok_or_else(|| {
            HttpError::MalformedResponse(format!("{what}行に `:` がない: {header_line}"))
        })?;
        last = Some(headers.append(name.trim(), value.trim()));
    }
}

/// ヘッダに従って本文を読む (本文の長さの決め方は RFC 9112 6.3 節)。
fn read_body(
    reader: &mut impl BufRead,
    status: u16,
    headers: &Headers,
) -> Result<Vec<u8>, HttpError> {
    // 204 No Content と 304 Not Modified には本文がない
    if status == 204 || status == 304 {
        return Ok(Vec::new());
    }
    // 圧縮にはまだ対応していない (練習問題 1-9 で対応する)
    if let Some(value) = headers.get("content-encoding") {
        return Err(HttpError::UnsupportedEncoding {
            header: "content-encoding",
            value: value.to_owned(),
        });
    }

    let mut body = Vec::new();
    if let Some(value) = headers.get("transfer-encoding") {
        // 転送のための符号化はチャンク転送だけに対応する (`gzip, chunked` などは練習問題 1-9)
        if !value.eq_ignore_ascii_case("chunked") {
            return Err(HttpError::UnsupportedEncoding {
                header: "transfer-encoding",
                value: value.to_owned(),
            });
        }
        // Transfer-Encoding は Content-Length より優先する
        read_chunked(reader, &mut body)?;
    } else if let Some(value) = headers.get("content-length") {
        let length = parse_content_length(value)?;
        // 申告された長さで先に領域を確保しない。巨大な値を申告されても、
        // 実際に届いた分だけ伸ばしていけばメモリを使い切らない
        let read = reader.by_ref().take(length).read_to_end(&mut body)?;
        if (read as u64) < length {
            return Err(HttpError::MalformedResponse(
                "本文の途中で接続が切れた".to_owned(),
            ));
        }
    } else {
        // 長さの情報がなければ、接続が閉じるまでが本文
        reader.read_to_end(&mut body)?;
    }
    Ok(body)
}

/// Content-Length の値を解析する。
///
/// ヘッダが重複して `5, 5` のようにまとめられていても、値がすべて同じなら受け付ける
/// (RFC 9110 8.6 節)。値が食い違うときは本文の終わりが決まらないのでエラーにする。
fn parse_content_length(value: &str) -> Result<u64, HttpError> {
    let invalid = || HttpError::MalformedResponse(format!("Content-Length が不正: {value}"));
    let mut length = None;
    for part in value.split(',') {
        let part = part.trim();
        // `str::parse` は `+5` のような符号付きも受け付けるので、数字だけか先に確かめる
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid());
        }
        let parsed: u64 = part.parse().map_err(|_| invalid())?;
        if length.is_some_and(|length| length != parsed) {
            return Err(invalid());
        }
        length = Some(parsed);
    }
    length.ok_or_else(invalid)
}

/// チャンク転送 (RFC 9112 7.1 節) の本文を読み、チャンクをつないで `body` に書き足す。
///
/// 形式は「16 進数のサイズ行 → その長さのデータ → 改行」の繰り返しで、
/// サイズ 0 のチャンクと、トレーラー (ヘッダと同じ形式) と、空行で終わる。
///
/// ```text
/// 5\r\n
/// Hello\r\n
/// 0\r\n
/// \r\n
/// ```
fn read_chunked(reader: &mut impl BufRead, body: &mut Vec<u8>) -> Result<(), HttpError> {
    let mut line = Vec::new();
    loop {
        // サイズ行の長さの上限はチャンクごとに数える
        let mut budget = MAX_HEADER_BYTES;
        read_line(reader, &mut line, &mut budget, "チャンクのサイズ行")?;
        let size = parse_chunk_size(&line)?;
        if size == 0 {
            break;
        }
        // Content-Length と同じく、申告されたサイズで先に領域を確保しない
        let read = reader.by_ref().take(size).read_to_end(body)?;
        if (read as u64) < size {
            return Err(HttpError::MalformedResponse(
                "チャンクの途中で接続が切れた".to_owned(),
            ));
        }
        // データの直後は改行だけ
        read_line(reader, &mut line, &mut budget, "チャンク")?;
        if !line.is_empty() {
            return Err(HttpError::MalformedResponse(
                "チャンクのデータがサイズより長い".to_owned(),
            ));
        }
    }
    // 最後の空行を送らずに接続を閉じるサーバーもある。本文はすべて届いているので受け付ける
    if reader.fill_buf()?.is_empty() {
        return Ok(());
    }
    // トレーラーは使わないので読み捨てる
    let mut budget = MAX_HEADER_BYTES;
    read_headers(reader, &mut line, &mut budget, "トレーラー")?;
    Ok(())
}

/// チャンクのサイズ行 (例: `1a2b`、拡張付きなら `1a2b;name=value`) からサイズを取り出す。
fn parse_chunk_size(line: &[u8]) -> Result<u64, HttpError> {
    let invalid = || {
        HttpError::MalformedResponse(format!(
            "チャンクのサイズが不正: {}",
            String::from_utf8_lossy(line)
        ))
    };
    let line = std::str::from_utf8(line).map_err(|_| invalid())?;
    // `;` から後ろはチャンク拡張。使わないので無視する
    let size = line.split(';').next().unwrap_or_default().trim();
    // `from_str_radix` も `+` の符号を受け付けるので、16 進数の数字だけか先に確かめる
    if size.is_empty() || !size.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid());
    }
    // 桁が多すぎて u64 に収まらないときもエラーになる
    u64::from_str_radix(size, 16).map_err(|_| invalid())
}

/// 1 行を `line` に読み込み、末尾の改行 (`\r\n` または `\n`) を取り除く。
///
/// 読んだバイト数だけ `budget` を減らす。改行の前に接続が切れたとき、
/// または `budget` を使い切ったときはエラーにする。`what` はエラーメッセージに使う行の名前。
fn read_line(
    reader: &mut impl BufRead,
    line: &mut Vec<u8>,
    budget: &mut u64,
    what: &str,
) -> Result<(), HttpError> {
    line.clear();
    let read = reader.by_ref().take(*budget).read_until(b'\n', line)?;
    *budget -= read as u64;
    if line.pop() != Some(b'\n') {
        let reason = if *budget == 0 {
            format!("{what}が大きすぎる (上限 {} KiB)", MAX_HEADER_BYTES / 1024)
        } else {
            format!("{what}の途中で接続が切れた")
        };
        return Err(HttpError::MalformedResponse(reason));
    }
    if line.ends_with(b"\r") {
        line.pop();
    }
    Ok(())
}

/// HTTP の送受信に失敗した理由。
///
/// エラーは読み込み用のバッファより長く使われる (呼び出し元へ返して表示する) ので、
/// 説明の文字列は借用せず `String` で持つ。
#[derive(Debug)]
pub enum HttpError {
    /// 送受信の失敗。
    Io(io::Error),
    /// HTTP のレスポンスとして解釈できない。
    MalformedResponse(String),
    /// まだ対応していない転送方式・圧縮方式。
    UnsupportedEncoding { header: &'static str, value: String },
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(_) => write!(f, "サーバーとの通信に失敗しました"),
            Self::MalformedResponse(reason) => {
                write!(f, "不正なレスポンスです ({reason})")
            }
            Self::UnsupportedEncoding { header, value } => {
                write!(f, "未対応のエンコーディングです ({header}: {value})")
            }
        }
    }
}

impl Error for HttpError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::MalformedResponse(_) | Self::UnsupportedEncoding { .. } => None,
        }
    }
}

impl From<io::Error> for HttpError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(response: &[u8]) -> Result<Response, HttpError> {
        read_response(&mut &response[..])
    }

    fn body(response: &[u8]) -> String {
        String::from_utf8(read(response).unwrap().body).unwrap()
    }

    #[test]
    fn request_has_http11_headers() {
        assert_eq!(
            request_text("example.org", "/a?b=c"),
            format!(
                "GET /a?b=c HTTP/1.1\r\n\
                 Host: example.org\r\n\
                 Connection: close\r\n\
                 User-Agent: my-browser/{}\r\n\
                 \r\n",
                env!("CARGO_PKG_VERSION")
            )
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
    fn get_writes_request_and_reads_response() {
        let mut stream = MockStream {
            response: io::Cursor::new(b"HTTP/1.1 404 Not Found\r\nContent-Length: 4\r\n\r\nnope"),
            written: Vec::new(),
        };
        let response = get(&mut stream, "example.org:8080", "/").unwrap();

        assert_eq!(response.status, 404);
        assert_eq!(response.body, b"nope");
        assert!(
            String::from_utf8(stream.written)
                .unwrap()
                .starts_with("GET / HTTP/1.1\r\nHost: example.org:8080\r\n")
        );
    }

    #[test]
    fn reads_status_headers_and_body() {
        let response =
            read(b"HTTP/1.0 200 OK\r\nContent-Type: text/html\r\n\r\n<p>hi</p>").unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.headers.get("content-type"), Some("text/html"));
        assert_eq!(response.body, b"<p>hi</p>");
    }

    #[test]
    fn accepts_bare_lf_line_endings() {
        assert_eq!(
            body(b"HTTP/1.1 404 Not Found\nServer: test\n\nnot found"),
            "not found"
        );
    }

    #[test]
    fn header_names_are_case_insensitive() {
        let response = read(b"HTTP/1.1 200 OK\r\nX-Test : value \r\n\r\n").unwrap();
        assert_eq!(response.headers.get("x-test"), Some("value"));
        assert_eq!(response.headers.get("X-TEST"), Some("value"));
    }

    #[test]
    fn joins_folded_header_lines() {
        let response =
            read(b"HTTP/1.1 200 OK\r\nX-Long: a\r\n  b\r\n\tc\r\n \r\nX-Next: d\r\n\r\n").unwrap();
        assert_eq!(response.headers.get("x-long"), Some("a b c"));
        assert_eq!(response.headers.get("x-next"), Some("d"));
    }

    #[test]
    fn combines_repeated_headers() {
        let response = read(b"HTTP/1.1 200 OK\r\nVary: a\r\nvary: b\r\n\r\n").unwrap();
        assert_eq!(response.headers.get("vary"), Some("a, b"));
    }

    #[test]
    fn reads_only_content_length_bytes() {
        assert_eq!(
            body(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nHello, world"),
            "Hello"
        );
        // 重複して同じ値が並んでいる場合も受け付ける
        assert_eq!(
            body(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\nok"),
            "ok"
        );
    }

    #[test]
    fn rejects_invalid_content_length() {
        let cases: [&[u8]; 4] = [
            b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort", // 途中で切れた
            b"HTTP/1.1 200 OK\r\nContent-Length: 1, 2\r\n\r\nab",  // 値が食い違う
            b"HTTP/1.1 200 OK\r\nContent-Length: +1\r\n\r\na",     // 符号付き
            b"HTTP/1.1 200 OK\r\nContent-Length: \r\n\r\n",        // 空
        ];
        for response in cases {
            assert!(
                matches!(read(response), Err(HttpError::MalformedResponse(_))),
                "{:?}",
                String::from_utf8_lossy(response)
            );
        }
    }

    #[test]
    fn decodes_chunked_body() {
        let response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
            5\r\nHello\r\n\
            7;name=value\r\n, world\r\n\
            A\r\n!!!!!!!!!!\r\n\
            0\r\n\
            Trailer-Field: ignored\r\n\
            \r\n";
        assert_eq!(body(response), "Hello, world!!!!!!!!!!");
    }

    #[test]
    fn transfer_encoding_overrides_content_length() {
        let response =
            b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nTransfer-Encoding: Chunked\r\n\r\n\
            2\r\nok\r\n0\r\n\r\n";
        assert_eq!(body(response), "ok");
    }

    #[test]
    fn rejects_malformed_chunks() {
        let cases: [&[u8]; 6] = [
            b"zz\r\nHello\r\n0\r\n\r\n", // 16 進数ではない
            b"+5\r\nHello\r\n0\r\n\r\n", // 符号付き
            b"\r\nHello\r\n0\r\n\r\n",   // サイズが空
            b"10000000000000000\r\n",    // u64 に収まらない
            b"3\r\nHello\r\n0\r\n\r\n",  // データがサイズより長い
            b"5\r\nHello\r\n",           // 最後のチャンクの前に切れた
        ];
        for chunks in cases {
            let mut response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
            response.extend_from_slice(chunks);
            assert!(
                matches!(read(&response), Err(HttpError::MalformedResponse(_))),
                "{:?}",
                String::from_utf8_lossy(chunks)
            );
        }
    }

    #[test]
    fn accepts_close_right_after_last_chunk() {
        let response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nHello\r\n0\r\n";
        assert_eq!(body(response), "Hello");
    }

    #[test]
    fn rejects_truncated_trailer() {
        let response =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\nTrailer-Field: x";
        assert!(matches!(
            read(response),
            Err(HttpError::MalformedResponse(reason)) if reason.contains("トレーラー")
        ));
    }

    #[test]
    fn rejects_truncated_chunk_data() {
        let response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n10\r\nshort";
        assert!(matches!(
            read(response),
            Err(HttpError::MalformedResponse(reason)) if reason.contains("チャンクの途中")
        ));
    }

    #[test]
    fn skips_informational_responses() {
        let response = b"HTTP/1.1 103 Early Hints\r\nLink: </style.css>\r\n\r\n\
            HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
        let response = read(response).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.headers.get("link"), None);
        assert_eq!(response.body, b"ok");
    }

    #[test]
    fn no_content_has_no_body() {
        assert_eq!(body(b"HTTP/1.1 204 No Content\r\n\r\nignored"), "");
    }

    #[test]
    fn rejects_unsupported_encodings() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Encoding:  gzip \r\n\r\n";
        assert!(matches!(
            read(response),
            Err(HttpError::UnsupportedEncoding { header: "content-encoding", value })
                if value == "gzip"
        ));

        // ヘッダ名と `:` の間に空白があっても見逃さない
        let response = b"HTTP/1.1 200 OK\r\nContent-Encoding : gzip\r\n\r\n";
        assert!(matches!(
            read(response),
            Err(HttpError::UnsupportedEncoding {
                header: "content-encoding",
                ..
            })
        ));

        let response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip, chunked\r\n\r\n";
        assert!(matches!(
            read(response),
            Err(HttpError::UnsupportedEncoding { header: "transfer-encoding", value })
                if value == "gzip, chunked"
        ));
    }

    #[test]
    fn rejects_malformed_responses() {
        let cases: [&[u8]; 8] = [
            b"",                                    // 何も返ってこない
            b"hello\r\n\r\n",                       // HTTP ではない
            b"HTTP/1.1 2000 OK\r\n\r\n",            // ステータスコードが 3 桁でない
            b"HTTP/1.1 101 Switching\r\n\r\n",      // 要求していないプロトコルの切り替え
            b"HTTP/1.0 200 OK\r\n folded\r\n\r\n",  // 続きの行なのに前の行がない
            b"HTTP/1.0 200 OK\r\nno-colon\r\n\r\n", // `:` のないヘッダ
            b"HTTP/1.0 200 OK\r\nServer: test\r\n", // 空行の前に切れた
            b"HTTP/1.0 200 OK\r\nServer: test",     // 改行の前に切れた
        ];
        for response in cases {
            assert!(
                matches!(read(response), Err(HttpError::MalformedResponse(_))),
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
            read(&response),
            Err(HttpError::MalformedResponse(reason)) if reason.contains("大きすぎる")
        ));
    }
}
