# my-browser

[Web Browser Engineering](https://browser.engineering/) (Pavel Panchekha, Chris Harrelson 著) の日本語訳を読みながら、Rust で一から作っている自作ブラウザです。

- ひとまずの目標: 動作するブラウザを完成させる
- 最終的な目標: 使用メモリを最小化する

## 必要なもの

- [mise](https://mise.jdx.dev/) (Rust・Node.js・lefthook・typos をまとめて導入します)
- Visual Studio 2022 または Build Tools の「C++ によるデスクトップ開発」(Rust の MSVC リンカとして使用)
- Git / [GitHub CLI](https://cli.github.com/)

## セットアップ

```powershell
mise install      # mise.toml に書かれたツールを導入
mise run setup    # commitlint の導入と Git フックの登録
mise run check    # 整形・lint・テストがすべて通ることを確認
```

## よく使うコマンド

| コマンド | 内容 |
| --- | --- |
| `mise run run -- <URL>` | ブラウザを起動する |
| `mise run fmt` | コードを整形する |
| `mise run lint` | 整形チェック・clippy・スペルチェック |
| `mise run test` | テストを実行する |
| `mise run check` | lint とテストをまとめて実行する |

## Git フック (lefthook)

| タイミング | 実行内容 |
| --- | --- |
| コミット前 | `cargo fmt --check`・`cargo clippy`・`typos` |
| コミットメッセージ | [commitlint](https://commitlint.js.org/) で [Conventional Commits](https://www.conventionalcommits.org/ja/) 形式か検査 |
| プッシュ前 | `cargo test` |

コミットメッセージの例: `feat(url): URL をスキーム・ホスト・パスに分解する`

## 進捗

### Part 1: Loading Pages

- [ ] 1. Downloading Web Pages
- [ ] 2. Drawing to the Screen
- [ ] 3. Formatting Text

### Part 2: Viewing Documents

- [ ] 4. Constructing an HTML Tree
- [ ] 5. Laying Out Pages
- [ ] 6. Applying Author Styles
- [ ] 7. Handling Buttons and Links

### Part 3: Running Applications

- [ ] 8. Sending Information to Servers
- [ ] 9. Running Interactive Scripts
- [ ] 10. Keeping Data Private

### Part 4: Modern Browsers

- [ ] 11. Adding Visual Effects
- [ ] 12. Scheduling Tasks and Threads
- [ ] 13. Animating and Compositing
- [ ] 14. Making Content Accessible
- [ ] 15. Supporting Embedded Content
- [ ] 16. Reusing Previous Computations
