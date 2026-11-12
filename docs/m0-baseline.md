# M0: 現状確認と実行可能な基準

確認日: 2026-11-12。対象課題: `renderer-jv5`。

## 着手時と今回の対応

着手時のHEADは`ebe2f8d`。Rustコード、Cargo manifest、テスト、CIは存在せず、
Beadsの設定とREADMEがあった。依頼で指定された実装方針は未追跡ファイルだったため、
内容を変更せず、この環境構築とともにGit管理へ追加した。

| 実装方針の要件 | M0で用意したもの・現状 |
|---|---|
| 最小workspaceとCPUテスト入口 | `crates/renderer`の単一ライブラリ。公開型・描画処理は未実装 |
| GPUなしの解析テスト経路 | デフォルト依存なし。`cargo test --no-default-features`で実行可能。テストケースは0件 |
| ash / vk-mem、C++環境、依存固定 | optionalな`vulkan` feature、Cargo.lock、Rust 1.99.0固定 |
| CIのtarget / backend分離 | Windows MSVC / Linux GNUとCPU / Vulkan依存の4構成、独立したfmtジョブ |
| API baseline・所有権・shader方針 | [ADR 0001](adr/0001-development-baseline.md) |
| 再現手順 | [開発環境](development.md) |
| M1以降のGPU実装・Graph・表示 | 未実装。実装方針との衝突や既存実装の移行はない |

## 確認した環境

| 項目 | 実測・導入済みの内容 |
|---|---|
| OS / target | Windows x64、25H2、build 26200.9711 / `x86_64-pc-windows-msvc` |
| Rust | rustc 1.99.0 (`b940084d7`)、cargo 1.99.0 (`5f94df478`) |
| C++ | Visual Studio 2022 Community、MSVCツールセット14.44.35207 |
| SDK / shaderツール | Vulkan SDK 1.4.341.1、glslcはshaderc v2026.1を報告 |
| ローダー | `C:/Windows/System32/vulkan-1.dll` 1.4.341.1、Instance API 1.4.341 |
| GPU / driver | NVIDIA GeForce RTX 3060、Vulkan device API 1.4.341、driverInfo 610.88 |
| 検証環境 | `VK_LAYER_KHRONOS_validation` 1.4.341と`VK_EXT_debug_utils`を列挙できた |
| キュー | Graphics / Compute / Transferを備えるqueue familyあり（queueCount 16） |
| 初期候補feature | timelineSemaphore / synchronization2 / dynamicRenderingはtrueを報告 |
| 解決した主な依存 | ash 0.38.0+1.3.281、vk-mem 0.5.0。ashの重複解決なし |

SDKの有無、GPUの対応、Rendererでの有効化・実行は別の確認である。
上記featureはvulkaninfoによる問い合わせ結果であり、RendererのDeviceではまだ有効化していない。
Linux / 別GPU / 別APIはローカルでは未検証。

## 実行した検証

リポジトリルートで[開発環境](development.md)のコマンドを実行した。

| 確認 | 結果 |
|---|---|
| `cargo fmt --all -- --check` | 成功 |
| `cargo check --locked --workspace --all-targets --no-default-features` | 成功 |
| `cargo clippy --locked --workspace --all-targets --no-default-features -- -D warnings` | 成功 |
| `cargo test --locked --workspace --no-default-features` | 成功。unit / doc testとも0件 |
| 上記check / clippy / testに`--features vulkan`を追加 | 成功。VMAのC++ビルドを含む。unit / doc testとも0件 |
| `cargo tree --locked --no-default-features` | rendererのみ。GPU・ネイティブ依存なし |
| `cargo tree --locked --features vulkan` | ash / vk-memの依存構成を確認 |
| `C:/VulkanSDK/1.4.341.1/Bin/vulkaninfoSDK.exe --summary` | 成功。ローダー、GPU、layerを列挙 |
| `vulkaninfo`のfeature / queue情報 | 上記の対応状況を確認 |
| `C:/VulkanSDK/1.4.341.1/Bin/glslc.exe --version` | 成功。バージョンの確認のみ |

実装がないため、描画・転送・readback・GPU完了・寿命・画像回帰・性能測定は未実行。
Validation Layer / Synchronization ValidationをRendererへ適用した試験も未実行。
CPU / GPU時間やメモリ量の基準値はまだない。
CI定義は追加したが、この記録はローカルWindowsでの実行結果である。
リモートCIの結果はGitHub ActionsとBeadsの作業記録で確認する。

## 次の最小作業

Beadsの`renderer-qhg`（M1: Vulkan buffer transfer and readback smoke test）へ引き継ぐ。
privateなVulkan初期化、検証、単一キュー、VMA buffer、既知データの転送・readback、
完了待ちと解放までを一つの縦切りとして実装する。
受入条件は実機でのbyte一致、Validation errorゼロ、warningの調査、
未対応環境の明示的なエラーと部分初期化の安全な解放。
GPU試験は明示実行にし、CPU経路はGPUなしで維持する。詳細な進捗と依存はBeadsで管理する。
