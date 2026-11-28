//! 自作ブラウザのエントリポイント。
//!
//! 「Web Browser Engineering」の章を進めるごとに機能を追加していく。

mod entity;
mod file;
mod http;
mod percent;
mod tls;
mod url;

use std::env;
use std::error::Error;
use std::io::{self, BufWriter, Write};
use std::process::ExitCode;

use url::Url;

fn main() -> ExitCode {
    // `env::args()` は Unicode として不正な引数で panic するので、`args_os()` で受け取る
    let url = match env::args_os().nth(1) {
        // URL を指定しなければ既定のページを開く (練習問題 1-2)
        None => Ok(Url::default_page()),
        Some(arg) => match arg.to_str() {
            Some(arg) => Url::parse(arg),
            None => {
                eprintln!("エラー: URL を文字列として解釈できません");
                return ExitCode::FAILURE;
            }
        },
    };

    match url
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
    match print_page(&body, url.is_view_source()) {
        // 出力先のパイプが先に閉じられた (`| Select-Object -First 1` など) だけなら、
        // ページの読み込み自体は成功しているので正常終了とする
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        result => Ok(result?),
    }
}

/// ページを標準出力に表示する。
fn print_page(body: &str, view_source: bool) -> io::Result<()> {
    let mut out = BufWriter::new(io::stdout().lock());
    render(body, view_source, &mut out)?;
    out.flush()
}

/// ページを `out` に書き出す。
///
/// `view-source:` が付いていれば、受け取った HTML をタグも文字参照もそのまま書き出す
/// (練習問題 1-5。端末を操作する制御文字だけは置き換える)。本のヒントのように
/// `<` を `&lt;` に置き換えてから `show` に通すと本文の分だけコピーが増えるので、
/// そのまま書き出す。
fn render(body: &str, view_source: bool, out: &mut impl Write) -> io::Result<()> {
    if view_source {
        write_to_terminal(body, out)
    } else {
        show(body, out)
    }
}

/// HTML からタグを取り除いたテキストを `out` に書き出す。
///
/// 本と同じく `<` から `>` までをタグとみなす。1 文字ずつ書き出す代わりに、
/// タグとタグの間のテキストをまとめて書き出す。
///
/// `<` と `>` は ASCII なので、UTF-8 の多バイト文字の途中に現れることはない。
/// そのため文字単位ではなくバイト単位で走査しても、文字を途中で切ることはない。
fn show(body: &str, out: &mut impl Write) -> io::Result<()> {
    let mut in_tag = false;
    // まだ書き出していないテキストの開始位置
    let mut text_start = 0;
    for (i, b) in body.bytes().enumerate() {
        match b {
            b'<' => {
                if !in_tag {
                    write_text(&body[text_start..i], out)?;
                }
                in_tag = true;
            }
            b'>' => {
                // タグの外にある `>` も本と同じく表示しない
                if !in_tag {
                    write_text(&body[text_start..i], out)?;
                }
                in_tag = false;
                text_start = i + 1;
            }
            _ => {}
        }
    }
    if !in_tag {
        write_text(&body[text_start..], out)?;
    }
    Ok(())
}

/// タグの外のテキストを、文字参照 (`&lt;` など) を元の文字に戻しながら書き出す (練習問題 1-4)。
fn write_text(text: &str, out: &mut impl Write) -> io::Result<()> {
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        write_to_terminal(&rest[..amp], out)?;
        rest = &rest[amp..];
        match entity::decode(rest) {
            Some((c, len)) => {
                // `&#27;` のように、文字参照から制御文字が現れることもある
                write_to_terminal(c.encode_utf8(&mut [0; 4]), out)?;
                rest = &rest[len..];
            }
            // 文字参照でない `&` はそのまま表示する
            None => {
                out.write_all(b"&")?;
                rest = &rest[1..];
            }
        }
    }
    write_to_terminal(rest, out)
}

/// テキストを端末に書き出す。
///
/// ESC などの制御文字は端末を操作できてしまう (画面を消す、タイトルを書き換えるなど) ので、
/// U+FFFD に置き換える。改行・タブ・復帰だけはそのまま残す。
fn write_to_terminal(text: &str, out: &mut impl Write) -> io::Result<()> {
    let mut start = 0;
    for (i, c) in text.char_indices() {
        if c.is_control() && !matches!(c, '\n' | '\t' | '\r') {
            out.write_all(&text.as_bytes()[start..i])?;
            out.write_all("\u{FFFD}".as_bytes())?;
            start = i + c.len_utf8();
        }
    }
    out.write_all(&text.as_bytes()[start..])
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

    fn rendered(body: &str, view_source: bool) -> String {
        let mut out = Vec::new();
        render(body, view_source, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn shown(body: &str) -> String {
        rendered(body, false)
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

    #[test]
    fn decodes_entities_in_text() {
        // 練習問題 1-4 の例
        assert_eq!(shown("&lt;div&gt;"), "<div>");
        assert_eq!(
            shown("<p>A &amp; B &#169; 日本&nbsp;語</p>"),
            "A & B © 日本\u{A0}語"
        );
        // 戻した結果をもう一度戻すことはない
        assert_eq!(shown("&amp;lt;"), "&lt;");
    }

    #[test]
    fn keeps_text_that_is_not_an_entity() {
        assert_eq!(shown("Q&A &unknown; &lt &"), "Q&A &unknown; &lt &");
    }

    #[test]
    fn entities_inside_tags_are_hidden_with_the_tag() {
        assert_eq!(shown("<a title=\"&lt;\">link</a>"), "link");
    }

    #[test]
    fn view_source_shows_html_as_is() {
        let html = "<p>A &amp; B</p>\r\n\t";
        assert_eq!(rendered(html, true), html);
    }

    #[test]
    fn replaces_terminal_control_characters() {
        // 生の ESC も、文字参照から現れた ESC (`&#27;`) や C1 制御文字 (U+009B) も置き換える
        assert_eq!(
            shown("a\x1b[2Jb&#27;]0;title&#7;c\u{9b}d"),
            "a\u{FFFD}[2Jb\u{FFFD}]0;title\u{FFFD}c\u{FFFD}d"
        );
        assert_eq!(rendered("<p>\x1b[2J</p>", true), "<p>\u{FFFD}[2J</p>");
    }
}
