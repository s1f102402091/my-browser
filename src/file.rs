//! ローカルファイルの読み込み (練習問題 1-2 の file URL)。
//!
//! ローカルファイルに触れるのはこのファイルだけにし、安全のための規則もここにまとめる。
//! - 開くのは、利用者が読める「通常のファイル」だけ。フォルダやデバイス (`CON` など) は、
//!   中身を読む前にエラーにする
//! - ネットワーク上の共有フォルダ (UNC パス) は開かない。Windows は共有フォルダにつなぐとき
//!   ログイン中のアカウントの認証情報 (NTLM ハッシュ) を自動で送るので、漏れるおそれがあるため
//! - file URL を開くのは、利用者が直接指定したときだけ。リダイレクトで開かないことは
//!   `url` モジュールが型で保証する
//!
//! 共有フォルダかどうかは URL の文字列で判断している。そのため、共有フォルダを指す
//! シンボリックリンクや、ネットワークドライブに割り当てたドライブ文字 (`Z:` など) を
//! 利用者が自分で開いた場合は、そのままたどる (一般的なブラウザと同じ)。

use std::error::Error;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::percent;

/// file URL の `file:` より後ろ (例: `///C:/Users/a.html`) を、ローカルファイルのパスにする。
///
/// 受け付けるのは `file:///パス` と `file://localhost/パス` の形。
/// Windows では打ち間違えやすい `file://C:/パス` も受け付ける。
pub fn path_from_url(url: &str) -> Result<PathBuf, PathError> {
    // `\` は `/` と同じに扱う (WHATWG の URL Standard と同じ。`file:///C:\Users\...` も受け付ける)。
    // 置き換えた結果を持つために String を作る
    let url = url.replace('\\', "/");
    let rest = url.strip_prefix("//").ok_or(PathError::MissingSlashes)?;
    // クエリとフラグメントはファイルの場所とは関係ないので捨てる
    let rest = rest.split(['?', '#']).next().unwrap_or_default();

    let (host, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let path = if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
        path
    } else if cfg!(windows) && is_drive_letter(host) {
        // `file://C:/a.html` の `C:` はホストではなく、パスの先頭とみなす
        rest
    } else {
        return Err(PathError::Network(host.to_owned()));
    };

    if !percent::is_well_formed(path) {
        return Err(PathError::InvalidEscape(path.to_owned()));
    }
    let decoded = String::from_utf8(percent::decode(path))
        .map_err(|_| PathError::InvalidEscape(path.to_owned()))?;
    // `%00` や `%0A` から現れた制御文字は、パスとして意味をなさないので受け付けない
    if decoded.chars().any(|c| c.is_ascii_control()) {
        return Err(PathError::InvalidEscape(path.to_owned()));
    }
    // `%5C` から現れた `\` も `/` として扱う。共有フォルダの判定をすり抜けさせないため
    to_local_path(&decoded.replace('\\', "/"))
}

/// `C:` のようなドライブ文字か。
fn is_drive_letter(s: &str) -> bool {
    matches!(s.as_bytes(), [letter, b':'] if letter.is_ascii_alphabetic())
}

/// URL のパス (例: `/C:/Users/a.html`) を Windows のパスにする。
///
/// ドライブ文字から始まる絶対パス (`C:/...`) だけを受け付ける。これで共有フォルダ
/// (`//server/share`) やデバイスのパス (`//./COM1`、`//?/C:/...`) も受け付けなくなる。
#[cfg(windows)]
fn to_local_path(url_path: &str) -> Result<PathBuf, PathError> {
    let path = url_path.strip_prefix('/').unwrap_or(url_path);
    if path.starts_with('/') {
        // エラーには、先頭の `/` を取る前の `//server/share` の形を見せる
        return Err(PathError::Network(url_path.to_owned()));
    }
    let is_absolute =
        matches!(path.as_bytes(), [letter, b':', b'/', ..] if letter.is_ascii_alphabetic());
    if is_absolute {
        Ok(PathBuf::from(path))
    } else {
        Err(PathError::NotAbsolute(path.to_owned()))
    }
}

/// URL のパス (例: `/home/user/a.html`) を Unix のパスにする。
///
/// `/` から始まる絶対パスだけを受け付ける。`//` から始まるパスは、処理系によっては
/// ネットワーク上の場所を指す (POSIX で意味が決まっていない) ので受け付けない。
#[cfg(not(windows))]
fn to_local_path(path: &str) -> Result<PathBuf, PathError> {
    if path.starts_with("//") {
        Err(PathError::Network(path.to_owned()))
    } else if path.starts_with('/') {
        Ok(PathBuf::from(path))
    } else {
        Err(PathError::NotAbsolute(path.to_owned()))
    }
}

/// ローカルファイルを読む。
///
/// 通常のファイルでなければ (フォルダやデバイスなど)、中身を読まずにエラーにする。
pub fn read(path: &Path) -> Result<Vec<u8>, ReadError> {
    let io_error = |source| ReadError::Io {
        path: path.to_owned(),
        source,
    };
    // 開く前に種類を確かめる。デバイスや名前付きパイプは、開くだけで止まることがあるため
    if !fs::metadata(path).map_err(io_error)?.is_file() {
        return Err(ReadError::NotAFile(path.to_owned()));
    }
    let mut file = File::open(path).map_err(io_error)?;
    // 確かめてから開くまでの間にすり替えられていないか、開いたもので確かめ直す
    if !file.metadata().map_err(io_error)?.is_file() {
        return Err(ReadError::NotAFile(path.to_owned()));
    }
    let mut body = Vec::new();
    file.read_to_end(&mut body).map_err(io_error)?;
    Ok(body)
}

/// file URL をローカルファイルのパスにできない理由。
///
/// エラーは呼び出し元へ返して表示するので、問題の部分は借用せず `String` で持つ。
#[derive(Debug, PartialEq, Eq)]
pub enum PathError {
    /// `file:` の後ろが `//` で始まっていない。
    MissingSlashes,
    /// ネットワーク上の共有フォルダ (UNC パス) やデバイスを指している。
    Network(String),
    /// `%XX` の形が不正、または戻した結果が UTF-8 として不正か、制御文字を含む。
    InvalidEscape(String),
    /// 絶対パスになっていない。
    NotAbsolute(String),
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSlashes => write!(f, "file URL は `file:///` で始めてください"),
            Self::Network(target) => write!(
                f,
                "ネットワーク上の共有フォルダやデバイスは開けません: {target}"
            ),
            Self::InvalidEscape(path) => write!(f, "パスの `%XX` が不正です: {path}"),
            Self::NotAbsolute(path) => write!(f, "絶対パスではありません: {path}"),
        }
    }
}

impl Error for PathError {}

/// ローカルファイルを読めなかった理由。
///
/// エラーを表示するときにパスを示すので、パスは借用せず `PathBuf` で持つ。
#[derive(Debug)]
pub enum ReadError {
    /// 開けない、または読めない (見つからない、権限がないなど)。
    Io { path: PathBuf, source: io::Error },
    /// フォルダやデバイスなど、通常のファイルではない。
    NotAFile(PathBuf),
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, .. } => {
                write!(f, "ファイルを読み込めませんでした: {}", path.display())
            }
            Self::NotAFile(path) => write!(
                f,
                "通常のファイルではないので開けません (フォルダやデバイスなど): {}",
                path.display()
            ),
        }
    }
}

impl Error for ReadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::NotAFile(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn rejects_network_paths() {
        for url in [
            "//server/share/a.html",
            "//SERVER/share",
            "\\\\server\\share\\a.html",
            "////server/share/a.html",
            "///%5C%5Cserver/share/a.html",
            "///%2F%2Fserver/share/a.html",
        ] {
            assert!(
                matches!(path_from_url(url), Err(PathError::Network(_))),
                "{url}: {:?}",
                path_from_url(url)
            );
        }
    }

    #[test]
    fn rejects_malformed_urls() {
        assert_eq!(path_from_url("a.html"), Err(PathError::MissingSlashes));
        assert_eq!(path_from_url("/tmp/a.html"), Err(PathError::MissingSlashes));
        for url in ["///tmp/100%", "///tmp/%FF", "///tmp/a%00b", "///tmp/a%0Ab"] {
            assert!(
                matches!(path_from_url(url), Err(PathError::InvalidEscape(_))),
                "{url}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn parses_windows_paths() {
        let expected = Ok(PathBuf::from("C:/Users/a b/日本.html"));
        assert_eq!(
            path_from_url("///C:/Users/a%20b/%E6%97%A5%E6%9C%AC.html"),
            expected
        );
        assert_eq!(path_from_url("///C:/Users/a b/日本.html"), expected);
        assert_eq!(
            path_from_url("//localhost/C:/Users/a b/日本.html"),
            expected
        );
        assert_eq!(path_from_url("///C:\\Users\\a b\\日本.html"), expected);
        assert_eq!(path_from_url("//C:/Users/a b/日本.html"), expected);
        assert_eq!(path_from_url("///C:/Users/a b/日本.html?q=1#top"), expected);
    }

    #[cfg(windows)]
    #[test]
    fn rejects_non_drive_paths_on_windows() {
        // `\\?\C:\a.html` (`?` はクエリの始まりとして扱われる) のようなデバイスのパスも含む
        for url in [
            "///Users/a.html",
            "///C:a.html",
            "///C:",
            "///",
            "\\\\?\\C:\\a.html",
        ] {
            assert!(path_from_url(url).is_err(), "{url}");
        }
        assert!(matches!(
            path_from_url("\\\\.\\COM1"),
            Err(PathError::Network(_))
        ));
    }

    #[cfg(not(windows))]
    #[test]
    fn parses_unix_paths() {
        let expected = Ok(PathBuf::from("/home/user/a b/日本.html"));
        assert_eq!(
            path_from_url("///home/user/a%20b/%E6%97%A5%E6%9C%AC.html"),
            expected
        );
        assert_eq!(
            path_from_url("//localhost/home/user/a b/日本.html"),
            expected
        );
        assert_eq!(
            path_from_url("///home/user/a b/日本.html?q=1#top"),
            expected
        );
        assert_eq!(
            path_from_url("//C:/a.html"),
            Err(PathError::Network("C:".to_owned()))
        );
    }

    /// テスト用の一時ファイル。名前はテストごとに変え、終わったら消す。
    struct TempFile(PathBuf);

    impl TempFile {
        fn new(name: &str, contents: &[u8]) -> Self {
            let path = env::temp_dir().join(format!("my-browser-{}-{name}", std::process::id()));
            fs::write(&path, contents).unwrap();
            Self(path)
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    #[test]
    fn reads_regular_file() {
        let file = TempFile::new("read.html", "<p>こんにちは</p>".as_bytes());
        assert_eq!(read(&file.0).unwrap(), "<p>こんにちは</p>".as_bytes());
    }

    #[test]
    fn rejects_directories() {
        assert!(matches!(
            read(&env::temp_dir()),
            Err(ReadError::NotAFile(_))
        ));
    }

    #[test]
    fn reports_missing_files_with_path() {
        let path = env::temp_dir().join("my-browser-no-such-file.html");
        let err = read(&path).unwrap_err();
        assert!(matches!(err, ReadError::Io { .. }));
        assert!(err.to_string().contains("my-browser-no-such-file.html"));
        assert!(err.source().is_some());
    }
}
