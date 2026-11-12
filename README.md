# renderer

ゲームエンジン向けの、RenderGraphを中心としたRust製3Dレンダラーです。
初期バックエンドにはVulkan（ash / vk-mem）を使います。
現在はM1の基盤として、検証必須のVulkan 1.3 Instance、デバイスの能力照会・自動選択、
Deviceと単一Graphics＋Computeキューの初期化・診断・解放を扱います。
VMAによる用途別bufferの作成、範囲を検証したCPUからのwrite/read、メモリの解放も扱います。
単一キューでのbuffer転送、投入と完了待ち、実GPUからのreadback照合までを扱います。
VMAによる転送用2D imageの作成、対応能力の検証、所有・解放も扱います。
image転送、描画機能・公開APIは未実装で、M1全体の完了ではありません。

- [実装方針](docs/renderer-implementation-policy.md)
- [開発環境・ビルド手順](docs/development.md)
- [M0の確認結果](docs/m0-baseline.md)
- [Instance初期化・診断の確認結果](docs/m1-instance-baseline.md)
- [Device・単一キューの確認結果](docs/m1-device-baseline.md)
- [VMA bufferの確認結果](docs/m1-buffer-baseline.md)
- [buffer転送・完了待ちの確認結果](docs/m1-transfer-baseline.md)
- [VMA imageの確認結果](docs/m1-image-baseline.md)
- [初期環境と境界のADR](docs/adr/0001-development-baseline.md)
- [Instance初期化・診断・試験のADR](docs/adr/0002-vulkan-instance-bootstrap.md)
- [Device・自動選択・単一キューのADR](docs/adr/0003-vulkan-device-bootstrap.md)
- [VMA bufferの所有・CPUアクセスのADR](docs/adr/0004-vma-buffer-ownership.md)
- [単一キューのbuffer転送・完了待ちのADR](docs/adr/0005-single-queue-buffer-transfer.md)
- [VMA 2D imageの作成・所有のADR](docs/adr/0006-vma-image-ownership.md)

課題管理には [Beads](https://github.com/gastownhall/beads) を使用します。
導入・日常操作・同期の手順は [.beads/README.md](.beads/README.md) を参照してください。
