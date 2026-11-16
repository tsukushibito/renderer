# Renderer / RenderGraph 設計ガイド

このディレクトリには現行の利用手順・設計・実装指針を置きます。
過去の版の検証記録や置き換え済みの設計判断はGit履歴から参照します。

RenderGraph は[図で読む設計ガイド](design/rendergraph-overview.md)から読み始められます。
[Mermaid の設計図集](design/rendergraph-diagrams.md)では、主要な型、依存解析、GPU実行、資源の寿命と容量を辿れます。
検索・章ナビゲーション付きの HTML 版は [mdBook の生成手順](guides/html-docs.md)に従って作成します。
生成先は `build/docs/index.html`、配布用ZIPは `build/rendergraph-html.zip`、CI の配布物は `rendergraph-html` です。

## 最初に読む

| 読みたいこと | 入口 |
|---|---|
| 全体像をつかみたい | [図で読むRenderGraph](design/rendergraph-overview.md)：短いコード例、依存関係の図、処理と資源の寿命 |
| 型や処理の関係を図で追いたい | [設計図集](design/rendergraph-diagrams.md)：型の関係、owned命令、compile、producer/hazard、範囲、投入・完了、import、容量 |
| APIの正確な動作を確認したい | [RenderGraphの契約](design/rendergraph.md)：宣言、初期化、不要パス除去、owned Raster/Computeと画像copy、所有と容量 |
| 準備容量と不足時の対処を知りたい | [容量不足の回復](guides/development.md#runtime-capacity)：Graph／両Runtimeの上限、型付き理由、poll・結果消費・owner返却と再準備の違い |
| 公開APIの方針と変更順序を知りたい | [汎用Graph・標準パス・標準グラフ](design/implementation-policy.md#public-api-layers)と[移行計画](design/implementation-policy.md#public-api-migration)：標準利用、部分交換、独自構成 |
| GPU上の実行を知りたい | [Vulkanの設計](design/vulkan.md)と[D3D12の設計](design/implementation-policy.md#d3d12-runtime)：同期、実行枠、資源管理、投入・完了 |
| 二APIのWindows表示を使いたい | [表示の共通契約](design/rendergraph.md#windows-presentation)と[操作・受入・計測](guides/development.md#windows-presentation)：自由camera、resize、VSync、pending終了 |
| Bistroを二APIで描画・計測したい | [cookとForward PBR](guides/development.md#bistro-pbr)：MikkTSpace、DDS mip、材質・alpha、画像比較と通常releaseの計測 |
| 対応済みと計画を見分けたい | [現在の検証状況](verification/current.md)と[今後の計画](design/implementation-policy.md#bistro-roadmap) |

## 文書一覧

| 分類 | 文書 | 内容 |
|---|---|---|
| 利用手順 | [開発手順](guides/development.md) | 環境構築、Bistro素材と再現条件の準備、ビルド、試験、性能測定 |
| 利用手順 | [開発ツール](guides/tooling.md) | 依存監査、D3D12 bindings生成、SPIR-V／DXIL検証、[Bistro cook・shader・比較](guides/tooling.md#bistro-tools)、[CPUプロファイル](guides/tooling.md#cpu-profiling)、RenderDoc |
| 利用手順 | [HTML文書](guides/html-docs.md) | フレームワーク比較とmdBook選定、ビルド、閲覧、配布・更新 |
| 実装規約 | [製品版と開発用の検証方針](guides/validation-policy.md) | 保証を置く境界、製品から外す検証、実行状態・失敗処理、ビルド構成と受入 |
| 実装指針 | [Vulkanベストプラクティス](guides/vulkan-best-practices.md) | 一次資料に基づく能力照会、同期・寿命、メモリ、描画・Compute、WSI、検証・性能 |
| 実装指針 | [RenderGraphベストプラクティス](guides/rendergraph-best-practices.md) | 公開設計の比較、依存・内容・寿命、コンパイル、同期、資源配置、並列化、検証 |
| 設計 | [実装方針](design/implementation-policy.md) | [通常フレームのメモリ規約](design/implementation-policy.md#steady-state-memory)、Bistro確定要件と素材検証、[三層の公開API方針](design/implementation-policy.md#public-api-layers)・[移行計画](design/implementation-policy.md#public-api-migration)、[公開API比較と第一案](design/implementation-policy.md#public-api-comparison)、[基盤比較と採用・移行範囲](design/implementation-policy.md#foundation-selection)、B0〜B5計画、現行D3D12の契約 |
| 設計 | [RenderGraph](design/rendergraph.md) | 共通宣言・依存解析、二APIのbuffer・画像・trusted Rasterとblend／depth、sRGB・2D mip chain、import/export・履歴、登録資源を保持する再準備、二APIの表示契約、エンジン入力と拡張 |
| 設計の導入 | [図で読むRenderGraph](design/rendergraph-overview.md) | 全体像、具体例、producerとhazard、GPU実行・CPU結果の寿命、現在の対応範囲 |
| 設計図 | [型・依存・寿命の設計図集](design/rendergraph-diagrams.md) | Mermaid定義を正本にした構造図・型関係図・フロー・シーケンス・状態遷移 |
| 設計 | [Vulkan基盤](design/vulkan.md) | 現在有効な契約、Windows表示、任意計測と採用理由 |
| 検証 | [現在の検証状況](verification/current.md) | 実行済みの確認、測定条件、未確認範囲 |
| 検証データ | [ソースレビューと効率整理の証拠](verification/source-review.json) | Graph依存・区間解析と資源探索の前後CPU比較・生sample、Graph／Runtime容量API、通常frameメモリ、表示/descriptorの整理、native再受入、shader故障/改行再現、Blender全量同等性。所見は[検証状況](verification/current.md) |
| 検証データ | [Bistro素材検証](verification/bistro-assets.json)・[比較画像](verification/bistro-assets-preview.png) | 公式入力hash、全mesh/材質/画像の対応、変換・再生成・CPU画像QA |
| 検証データ | [Bistro alpha解釈](verification/bistro-alpha.json)・[近景比較画像](verification/bistro-alpha-preview.png) | 2材質の元値・全mip、Falcor一次ソース、alpha保持と不透明化の比較・確定方針 |
| 検証データ | [Bistro描画条件](../tools/bistro/conditions.json)・[設定検証と実行記録](verification/bistro-conditions.json)・[CPUプレビュー](verification/bistro-conditions-preview.png) | 固定7視点・巡回・sun/IBL/露出、照明候補、二API比較と性能校正の条件 |
| 検証データ | [基盤比較の実機小試作](verification/foundation-comparison.json) | 固定wgpu、二APIのowned Raster/Compute、期待byte、検証flagsの負の対照、source hashと未確認範囲 |
| 素材 | [Bistro LFS素材](../assets/bistro/README.md)・[manifest](../assets/bistro/manifest.json) | 公式原本・検証済み屋外glTF・環境HDR、取得・ライセンスと全ファイルhash |
| 検証データ | [現在のフレーム計測](verification/current-metrics.json) | headless／二API Windows表示の環境、CPU／GPU・VSync別統計と生sample |
| 検証データ | [通常フレームのメモリ計測](verification/frame-memory.json) | CPU確保/再確保/解放とnativeイベントの観測範囲、変動フレームの基準値、ゼロ目標の未達状況 |
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
