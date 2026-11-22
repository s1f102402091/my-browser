//! 自作ブラウザのエントリポイント。
//!
//! 「Web Browser Engineering」の章を進めるごとに機能を追加していく。

use std::env;
use std::process::ExitCode;

fn main() -> ExitCode {
    let Some(url) = env::args().nth(1) else {
        eprintln!("使い方: my-browser <URL>");
        return ExitCode::FAILURE;
    };

    // TODO(第1章): URL を解析し、HTTP でページを取得して表示する
    println!("{url}");
    ExitCode::SUCCESS
}
