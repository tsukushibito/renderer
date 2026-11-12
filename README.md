# renderer

ゲームエンジン向けの、RenderGraphを中心としたRust製3Dレンダラーです。
初期バックエンドにはVulkan（ash / vk-mem）を使います。
現在のVulkan基盤は、Instance・Deviceの初期化と診断、VMAによるbuffer／imageの所有、
単一キューでの転送と完了待ち、実行資源の再利用、共有staging領域を扱います。
CPU範囲管理の性能比較にはCriterionを使います。
次の実装段階はRenderGraphと描画・Compute処理です。

- [ドキュメント一覧](docs/README.md)
- [開発・ビルド・試験](docs/guides/development.md)
- [開発ツール](docs/guides/tooling.md)
- [実装方針](docs/design/implementation-policy.md)
- [Vulkan基盤の設計判断](docs/design/vulkan.md)
- [現在の検証状況](docs/verification/current.md)

課題管理には [Beads](https://github.com/gastownhall/beads) を使用します。
導入・日常操作・同期の手順は [.beads/README.md](.beads/README.md) を参照してください。
