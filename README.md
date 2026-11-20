# renderer

Bistroの原本・検証済み素材と比較画像はGit LFSで共有します。
取得・ライセンス・容量は[素材ガイド](assets/bistro/README.md)を参照してください。

ゲームエンジン向けの、RenderGraphを中心としたRust製3Dレンダラーです。
GPUに依存しないRenderGraphの宣言・解析と、Vulkan 1.3（ash / vk-mem）および
WindowsのD3D12による実行を提供します。両バックエンドでbuffer・RGBA8/RGBA16F画像の転送、
indexed meshと深度、利用側shaderによるRaster/Compute、import/export・履歴、readback、
上限付きの資源再利用とWindows表示を扱います。各バックエンドは単一キューで実行します。

`renderer-pipeline`は`pbr_forward`と`unlit_forward`の描画レシピ、再利用できる`passes`を提供します。
`renderer::capacity`でレシピと独自パスの要求を集約し、同じ`renderer::graph`に[Rustで合成](docs/guides/tooling.md#rust-pipeline)できます。
同期、資源の寿命、不要パスの除去はGraphが扱います。
PBR ForwardはIBL、4カスケードの影、HDRとSDR出力を構築します。
`renderer-bistro`の`bistro`はこのレシピを使う描画に加え、
Forward+、Deferred、MSAA、Bloomなどの比較構成をVulkanとD3D12で実行します。
同じパッケージの`bistro-graph`は公開Graph APIでパスを組み立てます。
native実行のmip/layer等の制約は[対応範囲](docs/design/rendergraph.md#対応範囲と処理の境界)、
実行済みの確認と未確認範囲は[検証状況](docs/verification/current.md)を参照してください。

既定featureではGPU・ウィンドウに依存しません。Graphを構築してから、`runtime::Runtime`で
両バックエンド共通の準備・投入・完了確認を行います。Windows表示も同じ実行経路を使います。
通常frameでは準備済み容量を再利用します。転送領域の物理容量は選択したバックエンドの初期設定で調整できます。

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
cargo run --locked -p renderer --no-default-features --example graph_composition
```

小さなPBRシーン、ヘッドレス描画、Windows表示、2つのBistroサンプルは[Examplesガイド](docs/guides/examples.md)から実行できます。

課題管理には [Beads](https://github.com/gastownhall/beads) を使用します。
導入・日常操作・同期の手順は [.beads/README.md](.beads/README.md) を参照してください。
