# 開発環境

現時点の対象は[実装方針](renderer-implementation-policy.md)のM0です。
描画APIはまだなく、`cargo test`はテスト実行基盤の確認（テスト0件）です。
対応状況と実測環境は[M0の確認結果](m0-baseline.md)を参照してください。

## 必要なツール

| 用途 | 必要なもの |
|---|---|
| CPU側の開発 | Git、rustup、Rust 1.99.0（edition 2024）と対象OSのリンカー |
| Vulkan依存のビルド | 上記に加えてC++コンパイラー。WindowsはMSVC x64、Linuxはg++ |
| Vulkan実行の調査・今後のGPU試験 | Vulkan 1.3対応ドライバー／ローダー、Khronos Validation Layer、vulkaninfo |
| 今後のシェーダー生成 | Vulkan SDK 1.4.341.1のglslc（shaderc v2026.1） |
| 課題管理 | Beads v1.3.4。[導入・同期手順](../.beads/README.md)を参照 |

`rust-toolchain.toml`がRust・rustfmt・clippyを固定します。
初回のCargo実行時にrustupが取得するため、ネットワーク接続が必要です。
`Cargo.lock`もコミットし、通常の確認では`--locked`を使います。
MSRVはまず検証済みの1.99に設定し、それより古いRustへの対応は約束しません。

### Windows

rustupの`x86_64-pc-windows-msvc`を使います。
Visual Studio 2022またはBuild Tools 2022の「C++によるデスクトップ開発」から、
MSVC x64/x86ビルドツールとWindows SDKをインストールしてください。
`vk-mem`は同梱VMAのC++をコンパイルするため、Rustのインストールだけでは不足します。
通常のPowerShellからビルドできます。MSVCが検出されない場合は
Developer PowerShell for VS 2022で再実行してください。

GPU調査・シェーダー開発ではVulkan SDK **1.4.341.1**を追加します。
PowerShellから固定版を使う例は次のとおりです。

```powershell
& C:/VulkanSDK/1.4.341.1/Bin/vulkaninfoSDK.exe --summary
& C:/VulkanSDK/1.4.341.1/Bin/glslc.exe --version
```

`vulkaninfo`だけで起動すると、別バージョンのSDKやドライバー付属版が
選ばれる場合があります。調査結果には実行したパスも記録します。

### Linux（CI用のビルド対象）

Ubuntu 24.04でrustupを用意し、ネイティブビルドツールを追加します。

```sh
sudo apt-get update
sudo apt-get install -y build-essential
rustup show active-toolchain
```

CIのVulkanジョブでもg++を用意します。Windowsジョブはrunner付属のMSVCを使います。
これらはビルド対象であり、Linux実GPUの動作確認済みという意味ではありません。

## ビルド・品質確認

リポジトリのルートで実行します。CPU経路にはVulkan SDK、ローダー、GPU、
ウィンドウシステムは不要です。最初はoptional依存も含むlockfileの解決・取得に
ネットワークが必要になる場合があります。

```sh
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets --no-default-features
cargo clippy --locked --workspace --all-targets --no-default-features -- -D warnings
cargo test --locked --workspace --no-default-features
```

Vulkan依存（ash / vk-memとVMAのC++）も確認します。
ashは`loaded`で実行時ロードする構成のため、ビルドにSDKやVulkanリンクライブラリは不要です。
M0では以下のコマンドもGPUへ命令を投入しません。

```sh
cargo check --locked --workspace --all-targets --no-default-features --features vulkan
cargo clippy --locked --workspace --all-targets --no-default-features --features vulkan -- -D warnings
cargo test --locked --workspace --no-default-features --features vulkan
```

`.github/workflows/ci.yml`はWindows / LinuxとCPU / Vulkan依存の組み合わせを確認します。
実GPU・画像・性能テストは別経路としてM1以降で追加します。
デフォルトの`cargo test`へGPU必須のテストを混在させません。

## 依存関係と生成物

`cargo tree --locked --features vulkan`で、ashが一つのバージョンに解決されることを確認できます。
依存更新は独立した変更としてlockfile差分と両経路のビルドを検証します。
`target/`と一時ツール用の`.tools/`はGit管理対象外です。
シェーダーのビルド接続はM2までに実装し、手元だけのSPIR-Vファイルに依存しません。
