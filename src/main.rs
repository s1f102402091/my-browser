//! 自作ブラウザのエントリポイント。
//!
//! 「Web Browser Engineering」の章を進めるごとに機能を追加していく。

mod tls;
mod url;

use std::env;
use std::error::Error;
use std::io::{self, BufWriter, Write};
use std::process::ExitCode;

use url::Url;

fn main() -> ExitCode {
    // `env::args()` は Unicode として不正な引数で panic するので、`args_os()` で受け取る
    let Some(arg) = env::args_os().nth(1) else {
        eprintln!("使い方: my-browser <URL>");
        return ExitCode::FAILURE;
    };
    let Some(arg) = arg.to_str() else {
        eprintln!("エラー: URL を文字列として解釈できません");
        return ExitCode::FAILURE;
    };

    match Url::parse(arg)
        .map_err(Box::<dyn Error>::from)
        .and_then(|url| load(&url))
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            report(err.as_ref());
            ExitCode::FAILURE
        }
    }
}

/// ページを取得して表示する。
fn load(url: &Url) -> Result<(), Box<dyn Error>> {
    let body = url.request()?;
    match print_page(&body) {
        // 出力先のパイプが先に閉じられた (`| Select-Object -First 1` など) だけなら、
        // ページの読み込み自体は成功しているので正常終了とする
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        result => Ok(result?),
    }
}

/// ページの本文を標準出力に表示する。
fn print_page(body: &str) -> io::Result<()> {
    let mut out = BufWriter::new(io::stdout().lock());
    show(body, &mut out)?;
    out.flush()
}

/// HTML からタグを取り除いたテキストを `out` に書き出す。
///
/// 本と同じく `<` から `>` までをタグとみなす。1 文字ずつ書き出す代わりに、
/// タグとタグの間のテキストをまとめて書き出す。
///
/// `<` と `>` は ASCII なので、UTF-8 の多バイト文字の途中に現れることはない。
/// そのため文字単位ではなくバイト単位で走査しても、文字を途中で切ることはない。
fn show(body: &str, out: &mut impl Write) -> io::Result<()> {
    let bytes = body.as_bytes();
    let mut in_tag = false;
    // まだ書き出していないテキストの開始位置
    let mut text_start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'<' => {
                if !in_tag {
                    out.write_all(&bytes[text_start..i])?;
                }
                in_tag = true;
            }
            b'>' => {
                // タグの外にある `>` も本と同じく表示しない
                if !in_tag {
                    out.write_all(&bytes[text_start..i])?;
                }
                in_tag = false;
                text_start = i + 1;
            }
            _ => {}
        }
    }
    if !in_tag {
        out.write_all(&bytes[text_start..])?;
    }
    Ok(())
}

/// エラーを原因までさかのぼって標準エラー出力に書き出す。
fn report(err: &dyn Error) {
    eprintln!("エラー: {err}");
    let mut source = err.source();
    while let Some(cause) = source {
        eprintln!("  原因: {cause}");
        source = cause.source();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(body: &str) -> String {
        let mut out = Vec::new();
        show(body, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn strips_tags() {
        assert_eq!(shown("<p>Hello, <b>world</b>!</p>"), "Hello, world!");
    }

    #[test]
    fn keeps_non_ascii_text() {
        assert_eq!(shown("<h1>こんにちは</h1>世界"), "こんにちは世界");
    }

    #[test]
    fn drops_stray_closing_bracket_like_the_book() {
        assert_eq!(shown("a>b"), "ab");
    }

    #[test]
    fn hides_text_after_unclosed_tag() {
        assert_eq!(shown("text<unclosed"), "text");
        assert_eq!(shown("<<a>b"), "b");
    }
}
