# Sashiko

Sashiko（刺し子）は、AI エージェントと人がゲームを共同開発するための Rust 製ゲームエンジンを目指しています。シーンやアセットを編集し、その結果を再現可能な実行・検証で確かめられることを設計の軸にします。

最初のゲームは、研究施設を舞台にした 3D 見下ろし型ローグライトサバイバーです。Windows では `cargo run --locked --release -p survivor` で起動します。描画とアセットの回帰にはサンプルアプリ [training-yard](crates/training-yard/) を使います。[ゲームの操作とリプレイ](docs/guides/examples.md#survivor)

## ゲームを作る

ゲーム画面を既存のアプリに組み込めます。WinForms のサンプルは子ウィンドウに Survivor の3Dシーンを表示し、Web のサンプルは同じ固定更新を WebAssembly で実行して Canvas に俯瞰図を描きます。どちらも一時停止したまま座標を編集し、1 tick ずつ進められます。[起動手順](docs/guides/examples.md#embedded-game)

[sashiko クレート](crates/sashiko/src/lib.rs)は固定更新、入力とリプレイ、シーンと原型、アニメーション、空間検索、HUD、音声、セーブを提供します。GPU なしで固定更新を使う最小例は `cargo run --locked -p sashiko --example fixed_game` で実行できます。

研究施設ではシーンの JSON を編集すると、実験台を動かしたり、既存のモデルと動作を組み合わせて敵を追加したりできます。新しい動作はゲームの Rust 型として登録します。[敵を追加する例](docs/design/game-engine.md#研究施設で敵を追加して再生する)・[GUI と CLI の編集手順](docs/guides/examples.md#配置プロパティクリップを編集する)

## 描画サブシステム

Bistroの原本・検証済み素材と比較画像はGit LFSで共有します。
取得・ライセンス・容量は[素材ガイド](assets/bistro/README.md)を参照してください。

`renderer` と `renderer-core` は RenderGraph を中心とした 3D 描画を担当します。
GPUに依存しないRenderGraphの宣言・解析と、Vulkan 1.3（ash / vk-mem）および
WindowsのD3D12による実行を提供します。どちらも単一キューで、メッシュ描画、深度、Raster・Compute、
資源の転送、import/export、履歴、読み戻し、Windows表示に対応します。

設計上、シーン階層・アニメーション・カメラ・アセットのロードとフレーム進行はエンジン側の責務です。
用途に合わせて次の入口を選べます。

| 用途 | 入口 |
| --- | --- |
| メッシュ・材質を登録し、カメラごとに描く | `renderer` の `ViewRenderer` に標準または独自の `ViewPipeline` を登録します。複数の方式・解像度のビューを一つのGraphに記録できます。[組み込み手順](docs/guides/tooling.md#scene-materials) |
| 標準PBRに独自パスを組み込む | `GeometryPipeline` からHDRと必要な深度を受け取り、Bloomや独自エフェクト、SDR出力をつなぎます。[パス合成](docs/guides/tooling.md#rust-pipeline) |
| 描画をエンジン独自に定義する | `renderer_core::graph` で依存と描画を宣言し、`renderer_core::runtime::Runtime` で準備・投入・完了確認を行います。[RenderGraphの契約](docs/design/rendergraph.md) |

標準PBRはForward／Forward+の1×・4×とDeferredの1×に対応し、点光源、IBL、任意の4カスケードの影を扱います。
`renderer-bistro`の`bistro`は標準レシピ、`bistro-graph`は公開Graph APIの実例です。
native実行のmip/layer等の制約は[対応範囲](docs/design/rendergraph.md#対応範囲と処理の境界)、
実行済みの確認と未確認範囲は[検証状況](docs/verification/current.md)を参照してください。

既定featureはGPU・ウィンドウに依存しません。アプリは依存するクレート（`renderer` または `renderer-core`）の `vulkan`、`d3d12`、必要なら `presentation` を選びます。
独自パイプラインのクレートはバックエンドを指定せずに実装できます。起動や構成変更時に `Preparation` で容量を集約し、通常フレームで再利用します。

- [ドキュメント一覧](docs/README.md)
- [図で読むRenderGraph設計](docs/design/rendergraph-overview.md)・[HTML版の生成](docs/guides/html-docs.md)
- [Mermaidで読む型・依存・寿命の設計図集](docs/design/rendergraph-diagrams.md)
- [Examplesの実行と選び方](docs/guides/examples.md)
- [開発・ビルド・試験](docs/guides/development.md)
- [開発ツール](docs/guides/tooling.md)
- [Vulkanベストプラクティス](docs/guides/vulkan-best-practices.md)
- [実装方針](docs/design/implementation-policy.md)
- [RenderGraphの契約](docs/design/rendergraph.md)
- [Vulkan基盤の設計判断](docs/design/vulkan.md)
- [現在の検証状況](docs/verification/current.md)

最初に、GPUなしで利用側のパス合成を確認できます。

```powershell
cargo run --locked -p renderer-core --no-default-features --example graph_composition
```

小さなPBRシーン、ヘッドレス描画、Windows表示、2つのBistroサンプルは[Examplesガイド](docs/guides/examples.md)から実行できます。

課題管理には [Beads](https://github.com/gastownhall/beads) を使用します。
導入・日常操作・同期の手順は [.beads/README.md](.beads/README.md) を参照してください。
