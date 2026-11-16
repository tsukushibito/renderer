# renderer

Bistroの原本・検証済み素材と比較画像はGit LFSで共有します。
取得・ライセンス・容量は[素材ガイド](assets/bistro/README.md)を参照してください。

ゲームエンジン向けの、RenderGraphを中心としたRust製3Dレンダラーです。
GPUに依存しないRenderGraphの宣言・解析と、Vulkan 1.3（ash / vk-mem）および
WindowsのD3D12による実行を提供します。両バックエンドでbuffer・RGBA8/RGBA16F画像の転送、
indexed meshと深度、利用側shaderによるRaster/Compute、import/export・履歴、readback、
上限付きの資源再利用とWindows表示を扱います。各バックエンドは単一キューで実行します。

標準構成は`standard::graphs`、部分交換は`standard::passes`、独自構成は`graph`を使います。
いずれも同じGraphの同期・寿命・不要パス除去を通り、標準構成の使用は必須ではありません。
現在の標準forwardはunlit描画です。利用側の`bistro`例はcook済みのBistroを
テクスチャ付きForward PBRで描画し、同じ公開Graphを二APIへ接続します。
IBL・影・最終tone mapping・Bloomは後続段階です。
native実行のmip/layer等の制約は[対応範囲](docs/design/rendergraph.md#対応範囲と処理の境界)、
実行済みの確認と未確認範囲は[検証状況](docs/verification/current.md)を参照してください。

既定featureではGPU・ウィンドウに依存しません。Graphの構築はGPU処理を開始せず、
実行する場合はRuntimeの作成、working setとProgramの準備、submit、完了確認を明示します。
通常frameでは準備済み容量を再利用し、不足時に暗黙の増設やGPU待機は行いません。

- [ドキュメント一覧](docs/README.md)
- [図で読むRenderGraph設計](docs/design/rendergraph-overview.md)・[HTML版の生成](docs/guides/html-docs.md)
- [Mermaidで読む型・依存・寿命の設計図集](docs/design/rendergraph-diagrams.md)
- [開発・ビルド・試験](docs/guides/development.md)
- [開発ツール](docs/guides/tooling.md)
- [Vulkanベストプラクティス](docs/guides/vulkan-best-practices.md)
- [実装方針](docs/design/implementation-policy.md)
- [RenderGraphの契約](docs/design/rendergraph.md)
- [Vulkan基盤の設計判断](docs/design/vulkan.md)
- [現在の検証状況](docs/verification/current.md)

最初に、GPUなしで利用側のパス合成を確認できます。

```powershell
cargo run --locked -p renderer --no-default-features --example graph_composition
```

ヘッドレスの描画例は、Validation Layerを使えるVulkan環境で実行します。

```powershell
New-Item -ItemType Directory -Force .tools | Out-Null
cargo run --locked -p renderer --features vulkan,development-validation,graph-diagnostics --example headless -- .tools/m2.ppm
```

引数は出力先PPMファイルです。Windows表示は次の例を使います。

```powershell
cargo run --locked -p renderer --no-default-features --features vulkan,presentation --example windowed -- --api vulkan
cargo run --locked -p renderer --no-default-features --features d3d12,presentation --example windowed -- --api d3d12
```

準備容量、検証の切替、操作・受入手順は[開発ガイド](docs/guides/development.md)を参照してください。

Bistroのcookと材質描画は[専用の手順](docs/guides/development.md#bistro-pbr)へ進んでください。
原本素材を使わない小fixtureも同じ例で実行できます。

```powershell
cargo run --locked --release -p renderer --no-default-features --features vulkan --example bistro -- --api vulkan --fixture --output .tools/bistro-pbr/fixture-vulkan
```

課題管理には [Beads](https://github.com/gastownhall/beads) を使用します。
導入・日常操作・同期の手順は [.beads/README.md](.beads/README.md) を参照してください。
