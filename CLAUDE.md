# my-browser

「Web Browser Engineering」(Pavel Panchekha, Chris Harrelson 著) の日本語訳を、ユーザーが章ごとに学びながら Rust で実装していく自作ブラウザ。

- 目標の優先順位: (1) 動作するブラウザを完成させる → (2) 使用メモリを最小化する
- 本の Python 実装を、章の流れと用語はそのままに Rust へ移植する。
- ユーザーは学習中。実装を終えたら、本の Python コードとの対応と、Rust 側で下した設計判断を短く説明する。

## 開発フロー (1 章 = 1 ブランチ = 1 PR)

1. `main` から `ch01-downloading-web-pages` のような章ごとのブランチを切る。
2. クレート選定など設計の分岐点は、実装前に `/grill-me` でユーザーと決める。
3. テストを書きながら実装し、`mise run check` を通す。
4. `/code-review xhigh --fix` でレビューと修正を行う。
5. コミットして `gh pr create` で PR を出す。章末の練習問題は同じ PR か、別ブランチ `chNN-exercises` で扱う。

## 規約

- コミットは Conventional Commits。type と scope は英語、件名と本文は日本語 (例: `feat(http): HTTP/1.0 の GET リクエストを送る`)。scope にはモジュール名を使う。commit-msg フックの commitlint が検査する。
- コード中のコメントとドキュメントは日本語、識別子は英語。

## メモリ方針

- 依存クレートは、目的・代替案・メモリとバイナリサイズへの影響を示してユーザーの合意を得てから追加する。同じ役割なら軽量なものを選ぶ。
- 借用やスライスで足りる箇所では所有型を作らない。所有を選んだ箇所は理由をコメントに残す。
- メモリ使用量を計測できる段階になったら、計測方法を決めて変更前後の値を PR に記録する。

## 環境 (Windows 11)

- ツールは `mise.toml` で固定している。タスク一覧は `mise tasks`。
- mise が有効化されていないシェルでは `mise exec -- cargo ...` のように実行する。
- Rust ツールチェインは MSVC (`x86_64-pc-windows-msvc`)。Visual Studio 2022 の C++ ビルドツールに依存する。
