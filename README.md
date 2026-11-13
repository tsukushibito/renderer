# renderer

Bistroの原本・検証済み素材と比較画像はGit LFSで共有します。
取得・ライセンス・容量は[素材ガイド](assets/bistro/README.md)を参照してください。

ゲームエンジン向けの、RenderGraphを中心としたRust製3Dレンダラーです。
初期バックエンドにはVulkan（ash / vk-mem）を使います。
GPUに依存しないRenderGraphの宣言・解析と、Vulkan 1.3のオフスクリーン実行を扱います。
indexed meshと深度、Computeによる画像変換、別画像への最終合成、export・readbackを
単一キューで実行し、実行資源・画像・pipeline・descriptorと共有staging領域を再利用します。
CPU範囲管理の性能比較にはCriterionを使います。

Windowsの`d3d12` featureでは、同じGPU非依存Graphのbuffer upload・copy・readbackを
単一queueで実行します。Device・GPU完了・資源寿命を明示的に扱う最初の基準経路で、
画像、描画、import／export、資源poolは後続の実装範囲です。
[D3D12の設計と制約](docs/design/implementation-policy.md#d3d12-runtime)と
[二APIの実GPU受入手順](docs/guides/development.md#d3d12-buffer-acceptance)を参照してください。

- [ドキュメント一覧](docs/README.md)
- [開発・ビルド・試験](docs/guides/development.md)
- [開発ツール](docs/guides/tooling.md)
- [Vulkanベストプラクティス](docs/guides/vulkan-best-practices.md)
- [実装方針](docs/design/implementation-policy.md)
- [RenderGraphの契約](docs/design/rendergraph.md)
- [Vulkan基盤の設計判断](docs/design/vulkan.md)
- [現在の検証状況](docs/verification/current.md)

ヘッドレスの描画例は、Validation Layerを使えるVulkan環境で実行します。

```powershell
New-Item -ItemType Directory -Force .tools | Out-Null
cargo run --locked -p renderer --features vulkan,development-validation,graph-diagnostics --example headless -- .tools/m2.ppm
```

引数は出力先PPMファイルです。CPUだけのGraph構築・解析は既定featureで利用できます。

課題管理には [Beads](https://github.com/gastownhall/beads) を使用します。
導入・日常操作・同期の手順は [.beads/README.md](.beads/README.md) を参照してください。
