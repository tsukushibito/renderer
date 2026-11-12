# renderer

ゲームエンジン向けの、RenderGraphを中心としたRust製3Dレンダラーです。
初期バックエンドにはVulkan（ash / vk-mem）を使います。
現在はM0の環境構築段階で、描画機能・公開APIは未実装です。

- [実装方針](docs/renderer-implementation-policy.md)
- [開発環境・ビルド手順](docs/development.md)
- [M0の確認結果](docs/m0-baseline.md)
- [初期環境と境界のADR](docs/adr/0001-development-baseline.md)

課題管理には [Beads](https://github.com/gastownhall/beads) を使用します。
導入・日常操作・同期の手順は [.beads/README.md](.beads/README.md) を参照してください。
