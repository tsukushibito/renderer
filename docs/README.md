# ドキュメント

このディレクトリには現行の利用手順・設計・実装指針を置きます。
過去の版の検証記録や置き換え済みの設計判断はGit履歴から参照します。

| 分類 | 文書 | 内容 |
|---|---|---|
| 利用手順 | [開発手順](guides/development.md) | 環境構築、Bistro素材と再現条件の準備、ビルド、試験、性能測定 |
| 利用手順 | [開発ツール](guides/tooling.md) | 依存監査、D3D12 bindings生成、SPIR-V／DXIL検証、RenderDoc |
| 実装規約 | [製品版と開発用の検証方針](guides/validation-policy.md) | 保証を置く境界、製品から外す検証、実行状態・失敗処理、ビルド構成と受入 |
| 実装指針 | [Vulkanベストプラクティス](guides/vulkan-best-practices.md) | 一次資料に基づく能力照会、同期・寿命、メモリ、描画・Compute、WSI、検証・性能 |
| 実装指針 | [RenderGraphベストプラクティス](guides/rendergraph-best-practices.md) | 公開設計の比較、依存・内容・寿命、コンパイル、同期、資源配置、並列化、検証 |
| 設計 | [実装方針](design/implementation-policy.md) | Bistro確定要件と素材検証、APIのUXと基盤選定、B0〜B5計画、現行D3D12の契約と実装・検証の方針 |
| 設計 | [RenderGraph](design/rendergraph.md) | 共通宣言・依存解析、二APIのbuffer実行、Vulkanのimport/export・履歴、エンジン入力と拡張 |
| 設計 | [Vulkan基盤](design/vulkan.md) | 現在有効な契約、Windows表示、任意計測と採用理由 |
| 検証 | [現在の検証状況](verification/current.md) | 実行済みの確認、測定条件、未確認範囲 |
| 検証データ | [Bistro素材検証](verification/bistro-assets.json)・[比較画像](verification/bistro-assets-preview.png) | 公式入力hash、全mesh/材質/画像の対応、変換・再生成・CPU画像QA、未確定のalpha解釈 |
| 素材 | [Bistro LFS素材](../assets/bistro/README.md)・[manifest](../assets/bistro/manifest.json) | 公式原本・検証済み屋外glTF・環境HDR、取得・ライセンスと全ファイルhash |
| 検証データ | [現在のフレーム計測](verification/current-metrics.json) | 固定ソースの環境、各runの統計と生sample |
| 検証データ | [RenderGraph比較計測](verification/rendergraph-metrics.json) | 資源cache、標準draw batchの共有／異なるmesh比較、統計・生sample、RGBA照合 |
| 検証データ | [エンジン負荷の計測](verification/engine-workloads-metrics.json) | 大メッシュ・複数Viewの受入、製品／検証構成の比較、CPU保持量probe・生sample |

## 更新方針

- コマンドや導入方法は`guides/`、仕様・制約・設計の理由は`design/`へ反映します。
- 最新の検証結果は`verification/current.md`を更新します。未実行の確認を成功として扱いません。
- 段階別のbaselineや日付順のADRファイルは増やさず、現在の文書へ内容を統合します。
  置き換える前の内容はコミットへ残します。
- 新しい文書が必要な場合も、まず既存の文書にまとめられるかを確認します。
  追加・移動時はこの一覧と参照リンクを更新します。
- 課題と今後の作業はBeadsで管理します。

過去の状態は、例えば`git log --all -- docs`や`git show <commit>:<path>`で参照できます。
