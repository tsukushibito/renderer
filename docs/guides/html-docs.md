# HTML 文書の生成とフレームワーク選定

RenderGraph の HTML 版は **mdBook 0.5.4** と **mdbook-mermaid 0.17.1** で生成する。
[最初の解説](../design/rendergraph-overview.md)から読み始め、詳細な契約、backend、検証結果へ進める構成である。
本文の正本は `docs/design/` と `docs/guides/` の Markdown、検証結果の正本は
`docs/verification/current.md` とする。HTML だけを手で修正しない。

## 選定前に比較したもの

2026-11-15 に各プロジェクトの公式資料を確認した。下表の「今回の判断」は本リポジトリへの適合判断である。

| 候補 | 公式に提供される仕組み | 今回の判断 |
|---|---|---|
| [mdBook](https://rust-lang.github.io/mdBook/) | Markdown から HTML、章ナビゲーション、検索、コード強調、テーマ切替、全章印刷 | **採用。** Rust 製の CLI と既存 Markdown を使い、通常の生成は npm の実行なしで行える |
| [VitePress](https://vitepress.dev/guide/what-is-vitepress) | Vite/Vue による静的 HTML、技術文書用テーマ、Vue を使った対話的な例 | Vue コンポーネント中心の文書へ広げる場合の候補。今回は静的な説明と図が中心 |
| [Starlight](https://starlight.astro.build/) | Astro による文書サイト、検索、ナビゲーション、多言語、Markdown/MDX | Astro のサイト構成や UI 統合を必要とする場合の候補 |
| [Docusaurus](https://docusaurus.io/docs) | React/MDX、文書のバージョン管理、多言語、静的 HTML | 複数版の公開マニュアルへ広げる場合の候補。今回は現在の設計を一つの版として読む |

mdBook は [Rust 言語の公式文書にも使われるツール](https://rust-lang.github.io/mdBook/)であり、
今回必要な閲覧機能を標準で提供する。図は Mermaid 定義を正本にし、同梱した Mermaid 12.3.0 がブラウザーで SVG へ描画する。
図の表示には JavaScript を使う。外部 CDN へは接続しない。
mdBook が同梱する CSS・JavaScript・フォントを含めて配布するため、閲覧時のサーバーやインターネット接続は必須ではない。
GitHub 上のソースや外部の一次資料を開く場合は接続が必要になる。

## ビルド

Python 3.11 以降、Git、mdBook **0.5.4**、mdbook-mermaid **0.17.1** を使用する。
Python はリポジトリへのリンク変換と HTML の検査に使う。
GPU・Vulkan SDK・Bistro シーン/HDR の実入力は文書生成に必要ない。
全文書に含める既存の検証プレビューだけは、次のコマンドで LFS 実体を取得する。CI もこの範囲だけを取得する。

```sh
git lfs pull --include="docs/verification/*-preview.png" --exclude=""
```

各コマンドはリポジトリのルートで実行する。

mdBook は[公式リリース v0.5.4](https://github.com/rust-lang/mdBook/releases/tag/v0.5.4)の
対応するバイナリを取得して PATH に置くか、[公式の導入方法](https://rust-lang.github.io/mdBook/guide/installation.html)に従い、版を固定して導入する。

```sh
cargo install mdbook --version 0.5.4 --locked
cargo install mdbook-mermaid --version 0.17.1 --locked
mdbook --version
mdbook build docs
python -B tools/docs/check_html.py target/rendergraph-book
```

この Windows 環境では公式 ZIP を SHA-256 照合後に `.tools/mdbook-0.5.4/` へ展開している。

```powershell
$env:PATH = (Resolve-Path .tools/mdbook-mermaid-0.17.1).Path + [IO.Path]::PathSeparator + $env:PATH
& ./.tools/mdbook-0.5.4/mdbook.exe build docs
python -B tools/docs/check_html.py target/rendergraph-book
Start-Process ./target/rendergraph-book/index.html
```

Windows x64 MSVC ZIP の SHA-256 は
`8a6b2421aa522de06d746871d2b8fff9c8d71773467e310a4516d2566e7f2de4`。
CI は Linux x64 musl tarball の SHA-256
`5222beabd3e37dc5be0d18ff99b79058469354db5c220153a1b92db5ba12be89` を固定する。
いずれも調査時の公式 GitHub Release asset の digest と照合した値である。

mdbook-mermaid の[公式 v0.17.1](https://github.com/badboy/mdbook-mermaid/releases/tag/v0.17.1)も版とハッシュを固定する。
Windows x64 MSVC ZIP は `bb1e3d99d06e799d4a95a7e6beb6e8e041ba2c3b1528185e873f3f751e52feb5`、
CI の Linux x64 musl tarball は `42aca57eea8521971cc595fe4bb853c5b8b7d9f7138a2ec9f14a0a72c210922d`。
この版は mdBook 0.5.0 に対してビルドされているため 0.5.4 の呼出し時に版差の警告を出す。
本リポジトリでは下記の実描画検査により、この組合せの出力を確認する。

生成先は `target/rendergraph-book/`。`index.html` が閲覧の入口で、
図解の本文は `design/rendergraph-overview.html`、設計図集は `design/rendergraph-diagrams.html`、全章印刷用は `print.html`。
**配布時はディレクトリ全体を ZIP 等で渡す。** HTML ファイル単体ではテーマ・検索・図が不足する。
生成物とダウンロードしたツールは Git の追跡対象に含めない。

更新をすぐ確認する場合は `mdbook serve docs --hostname 127.0.0.1 --port 3000` を使い、
`http://127.0.0.1:3000` で読む。これはローカル確認用で、公開ホスティングの設定ではない。

## Mermaid 図の管理と描画検査

[設計図集](../design/rendergraph-diagrams.md)の12図は `docs/design/rendergraph-diagrams/*.mmd` を正本とする。
Markdown の `mermaid` コードブロック内で mdBook の `include` を使い、同じ図を導入と図集で共有する。
`accTitle` と `accDescr` を付け、図の前提・制限は本文にも記す。
型の実内包、Arc 共有保持、作成・識別・変換は矢印とラベルを区別する。

組み込み `links` → リポジトリリンク変換 → `mermaid` の順に処理する。
ブラウザーでは strict 設定、HTML label 無効、Dagre layout を使う。
図を小さく縮めて文字を潰さず、原寸と横スクロールで表示する。
「拡大して表示」で大きな画面を開き、拡大・縮小・原寸・全体表示を選べる。Esc または「閉じる」で本文へ戻る。
キーボードで図の領域へフォーカスしてスクロールでき、明暗テーマ変更時は図も再描画する。
「Mermaid 定義」でテキストの原本も読める。

表示用 Mermaid 12.3.0 は npm の固定依存から単一 JS へ bundle して追跡する。
版・ライセンス・ハッシュは [vendor 情報](../theme/vendor/README.txt)と[MIT License](../theme/vendor/mermaid-LICENSE.txt)にある。
通常の `mdbook build` に Node.js は不要。ライブラリ更新と自動描画検査では Node.js 24 と固定依存を使用する。

```sh
npm ci --ignore-scripts --prefix tools/docs
npm run bundle --prefix tools/docs -- --check
node tools/docs/node_modules/playwright/cli.js install chromium --only-shell
npm run check --prefix tools/docs -- /absolute/path/to/target/rendergraph-book
```

Windows では npm を `npm.cmd` で呼べる。
ブラウザー保存先をリポジトリ内へ限定する場合は、install と check の両方に同じ
`PLAYWRIGHT_BROWSERS_PATH` を設定する。
`bundle --check` は版固定した依存から再生成した JS・license・ハッシュ情報が追跡中のファイルと一致することを確認する。
Mermaid を更新するときは `tools/docs/package.json` と lockfile を更新し、`npm run bundle --prefix tools/docs` で再生成する。
mdbook-mermaid の `install` が同梱する別版の JS で vendor ファイルを置き換えない。

自動検査は生成済みの導入・図集・全章印刷を headless Chromium で開く。
全図の構文・描画、SVG title/description、拡大と返却、テーマ変更、390pxの横幅、印刷時の操作ボタン非表示を確認する。
文書の asset 要求は localhost だけを許可し、外部 CDN 等への依存を失敗にする。

## 読み方

左の章一覧から移動できる。狭い画面では左上のメニューで開閉する。
上部のテーマ切替で明暗を選び、検索アイコン、`S` または `/` で検索を開く。
`GraphPool`、`CompiledGraph`、`RAW` など、英語の API 名・用語で探せる。
標準検索には日本語の分かち書きがなく、日本語の語句は一致しない場合がある。
日本語は章一覧・章内の見出しとブラウザーのページ内検索を使う。
章内見出しへのリンクはブラウザーの戻る・進むで辿れる。
印刷アイコンは全章を一つのページへまとめる。
操作の詳細は [mdBook の閲覧ガイド](https://rust-lang.github.io/mdBook/guide/reading.html)を参照する。

## 文書の更新と CI

| ファイル | 役割 |
|---|---|
| [book.toml](../book.toml) | 日本語、出力先、テーマ、検索、preprocessor の設定 |
| [SUMMARY.md](../SUMMARY.md) | 章の順序と分類。新しい章はここにも追加する |
| [導入解説](../design/rendergraph-overview.md) | 全体像・具体例・図・対応範囲。詳細な契約を読むための案内 |
| [設計図集](../design/rendergraph-diagrams.md) | 主要な型・命令・解析・GPU実行・寿命・容量のMermaid図 |
| [詳細設計](../design/rendergraph.md) | 現在有効な RenderGraph の契約 |
| [repository_links.py](../../tools/docs/repository_links.py) | `docs/` 外の相対リンクを、ビルド時の Git HEAD を固定した GitHub リンクに変換 |
| [check_html.py](../../tools/docs/check_html.py) | 全 HTML のローカルリンク・fragment・画像・script・CSS の実在を検査 |
| [check_mermaid.mjs](../../tools/docs/check_mermaid.mjs) | 生成物の実描画と拡大・テーマ・モバイル・印刷を検査 |
| [CI](../../.github/workflows/ci.yml) | 固定CLIの取得・ハッシュ照合、bundle一致、生成、リンク・実描画検査、`rendergraph-html` artifact の保存 |

`docs/` 内の章と検証画像・JSON は生成物内に残る。
ソースや素材など `docs/` 外のリンクは配布物の外を相対参照せず、ビルド時点の Git コミットへ向ける。
そのため、コミット前のプレビューで外部ソースリンクを辿ると、最後のコミットの内容になる。
追跡対象ソースをコミットした後に生成し直すと、そのコミットに一致する。

章は既存 Markdown を直接使う。最初の `README.md` を入口にし、図解の本文へ案内する。
組み込みの `index` preprocessor は無効にして、本文中からの `README.md` へのリンクも
同じファイル名の `README.html` に対応させる。入口の `index.html` は mdBook が同内容で生成する。
サイドバーの章順序、検索、図、暗色テーマ、狭い画面、オフラインでの相対リンクを生成後にも確認する。
本書の追加は Renderer の同期・寿命・validation 配置・メモリ方針を変更しない。
