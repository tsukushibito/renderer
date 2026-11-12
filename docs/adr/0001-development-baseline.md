# ADR 0001: M0の開発基準と初期境界

- 日付: 2026-11-12
- 状態: 採用（GPU実行経路とシェーダー規約はM1〜M2で検証）
- 関連課題: `renderer-jv5`、後続 `renderer-qhg`

## 背景

既存リポジトリにはRust実装・manifest・CIがなく、Beadsと実装方針のみ存在する。
M0では再現可能なビルドとCPUテストの入口を作り、汎用RHIや未使用の公開型は作らない。
確認した実機はWindows x64 / RTX 3060で、Vulkan 1.4.341を報告する。

## 採用案

- 単一ライブラリ`crates/renderer`を持つworkspaceとする。edition 2024、
  Rust / MSRV 1.99、固定ツールチェーン1.99.0を使う。
- `default = []`でCPU側の解析・テストをGPUから独立させる。
  `vulkan` featureでash 0.38とvk-mem 0.5を有効化し、解決版はCargo.lockへ保存する。
  M0のfeatureは依存ビルドの入口であり、バックエンド実装済みを意味しない。
- ashは`loaded`でVulkanローダーを実行時に読み込む。SDKをビルド時の必須依存にしない。
  C++コンパイラーはVMAビルドの必須依存とする。
- 初期実行対象はWindows x64とVulkan **1.3**。
  M1でAPIバージョン、単一Graphics + Computeキュー、timelineSemaphore、
  synchronization2、dynamicRenderingの対応を問い合わせ、必要なfeatureを明示的に有効化する。
  低いAPIバージョンへのフォールバックは初期対象外とする。
  Validation Layerとdebug_utilsはGPU試験で必須とし、不足を成功扱いしない。
- 所有権は、ランタイムがInstance / Device / Queue / VMAと永続物理リソースを所有し、
  Graphは論理リソースとアクセス宣言を扱う方向とする。
  通常の公開APIにash / vk-mem型を出さず、GPU完了後に破棄する責任をランタイムへ集約する。
  外部ネイティブリソースのimport契約は必要になるまで公開しない。
  M0ではこれらの所有型・unsafeコードを実装しない。
- 初期シェーダー候補はGLSL 450、Vulkan SDK 1.4.341.1のglslc（shaderc v2026.1）、
  `--target-env=vulkan1.3`とする。初期は実行時コンパイルを必須にしない。
  entry pointは`main`、明示的な`set` / `binding`を使い、bufferレイアウトは
  `std140`（uniform）/ `std430`（storage）を出発点にCPUとの対応を検証する。
  M2で最小シェーダーと生成手順を追加し、第二APIで移植性を検証してから規約を固定する。

## 比較した案

多数の責務別クレートや全API共通のBackend traitは、具体的な実装がない段階では固定しない。
Rustの追従する`stable`指定は将来の再現性を失うため採用しない。
Vulkanの静的リンクはCPU用ビルドにもSDKの影響を広げるため避ける。
古いMSRVの保証は、必要性と継続検証なしに追加しない。
シェーダー言語はHLSL / Slangも候補だが、M0では新たなコンパイラー依存を増やさない。

## 影響・検証・見直し

GPU不要でfmt / check / clippy / testを実行でき、Vulkan依存のビルドも別に確認できる。
実測結果と未検証項目は[M0の確認結果](../m0-baseline.md)に記載する。
テストケースやGPU実装がない段階の成功を、描画・同期・寿命の検証成功とは扱わない。
MSRVを下げる要求、対象機材のVulkan制約、第二APIのシェーダー要件が判明した時点で再評価する。

## 一次資料

- [ash 0.38.0のロード方式とfeature](https://docs.rs/ash/0.38.0+1.3.281/ash/)
- [vk-mem 0.5.0の依存定義](https://docs.rs/crate/vk-mem/0.5.0/source/Cargo.toml.orig)
- [vk-mem 0.5.0のC++ビルド](https://docs.rs/crate/vk-mem/0.5.0/source/build.rs)
