# ドキュメント

このディレクトリには現行の利用手順・設計・実装指針を置きます。
過去の版の検証記録や置き換え済みの設計判断はGit履歴から参照します。

| 分類 | 文書 | 内容 |
|---|---|---|
| 利用手順 | [開発手順](guides/development.md) | 環境構築、ビルド、試験、性能測定 |
| 利用手順 | [開発ツール](guides/tooling.md) | 依存監査、SPIR-V検証、RenderDoc |
| 実装指針 | [Vulkanベストプラクティス](guides/vulkan-best-practices.md) | 一次資料に基づく能力照会、同期・寿命、メモリ、描画・Compute、WSI、検証・性能 |
| 実装指針 | [RenderGraphベストプラクティス](guides/rendergraph-best-practices.md) | 公開設計の比較、依存・内容・寿命、コンパイル、同期、資源配置、並列化、検証 |
| 設計 | [実装方針](design/implementation-policy.md) | 全体構成、ロードマップ、実装・検証の方針 |
| 設計 | [RenderGraph](design/rendergraph.md) | 公開宣言、producer・不要パス除去、import/export、GPU実行と所有 |
| 設計 | [Vulkan基盤](design/vulkan.md) | 現在有効な契約と採用理由 |
| 検証 | [現在の検証状況](verification/current.md) | 実行済みの確認、測定条件、未確認範囲 |

## 更新方針

- コマンドや導入方法は`guides/`、仕様・制約・設計の理由は`design/`へ反映します。
- 最新の検証結果は`verification/current.md`を更新します。未実行の確認を成功として扱いません。
- 段階別のbaselineや日付順のADRファイルは増やさず、現在の文書へ内容を統合します。
  置き換える前の内容はコミットへ残します。
- 新しい文書が必要な場合も、まず既存の文書にまとめられるかを確認します。
  追加・移動時はこの一覧と参照リンクを更新します。
- 課題と今後の作業はBeadsで管理します。

過去の状態は、例えば`git log --all -- docs`や`git show <commit>:<path>`で参照できます。
