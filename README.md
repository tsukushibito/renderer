# renderer

ゲームエンジン向けの、RenderGraphを中心としたRust製3Dレンダラーです。
初期バックエンドにはVulkan（ash / vk-mem）を使います。
現在はM1の先頭課題`renderer-qhg`として、検証必須のVulkan 1.3 Instanceの
初期化・診断・解放を扱います。描画機能・公開APIは未実装で、M1全体の完了ではありません。

- [実装方針](docs/renderer-implementation-policy.md)
- [開発環境・ビルド手順](docs/development.md)
- [M0の確認結果](docs/m0-baseline.md)
- [Instance初期化・診断の確認結果](docs/m1-instance-baseline.md)
- [初期環境と境界のADR](docs/adr/0001-development-baseline.md)
- [Instance初期化・診断・試験のADR](docs/adr/0002-vulkan-instance-bootstrap.md)

課題管理には [Beads](https://github.com/gastownhall/beads) を使用します。
導入・日常操作・同期の手順は [.beads/README.md](.beads/README.md) を参照してください。
