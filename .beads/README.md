# Beads の利用手順

このリポジトリは **Beads v1.3.4** を使って課題を管理します。
課題 ID の接頭辞は `renderer`、保存先は組み込み Dolt DB です。
別途 Dolt サーバーや Go をインストールする必要はありません。

## CLI のインストール

Windows の PowerShell で公式インストーラーを実行します。

```powershell
irm https://raw.githubusercontent.com/gastownhall/beads/v1.3.4/install.ps1 | iex
```

インストーラーが取得するのは実行時点の最新リリースです。
v1.3.4 に揃える場合は、[公式リリース](https://github.com/gastownhall/beads/releases/tag/v1.3.4)
の Windows 用 ZIP と `checksums.txt` を取得し、SHA-256 を検証してから展開してください。

Windows の標準インストール先は `%LOCALAPPDATA%\Programs\bd` です。
`bd` が見つからない場合はこのフォルダーをユーザーの PATH に追加し、
ターミナルを開き直してください。この環境では PATH を設定済みです。

```powershell
bd version
```

## この環境で使う

ローカル DB と Git hooks は初期化済みです。

```powershell
bd prime
bd ready
bd create "課題のタイトル" -t task -p 2 -d "目的と完了条件"
bd show renderer-xxxxx
bd update renderer-xxxxx --claim
bd close renderer-xxxxx
```

`renderer-xxxxx` は実際に作成された課題 ID に置き換えてください。
依存関係は `bd dep add <依存する課題ID> <先に完了すべき課題ID>` で記録します。

## 同期と別の clone のセットアップ

Git が管理するのは設定ファイルと hooks です。
課題 DB 自体（`.beads/embeddeddolt/`）は Git の通常のコミット対象外で、
Dolt の履歴として別途同期します。JSONL の手動コピーを共有手順には使いません。

同期先は `git+ssh://git@github.com/tsukushibito/renderer.git` です。
SSH 認証とリポジトリへのアクセス権が必要です。
今回の導入ではリモートへの push は実行していません。

管理者は共有開始時に、この環境から設定ファイルを通常の Git 操作で公開し、
課題 DB を `bd dolt push` で公開してください。
課題データは Git リモートの `refs/dolt/data` に保存されます。

その後、別の clone では CLI のインストール後に次を実行します。

```powershell
bd bootstrap --yes
bd hooks install
bd ready
```

同期先に課題 DB がまだ公開されていない場合、`bd bootstrap` は失敗することがあります。
その場合は元の環境から先に `bd dolt push` を実行してください。

共有開始後は作業前に `bd dolt pull`、作業後に `bd dolt push` で同期します。
通常の `git push` と課題 DB の同期は別の操作です。

## 動作確認

```powershell
bd ready --json
bd hooks list
bd dolt status
bd doctor --check=artifacts
```

v1.3.4 の組み込みモードでは `bd doctor` 全体の診断は未対応です。
DB の状態と個別の診断で確認してください。
