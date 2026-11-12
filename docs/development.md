# 開発環境

現時点の対象は[実装方針](renderer-implementation-policy.md)のM1先頭課題
`renderer-qhg`です。privateなVulkan 1.3 Instanceの初期化・診断・解放を扱い、
Device、描画、公開APIはまだありません。
採用した契約は[ADR 0002](adr/0002-vulkan-instance-bootstrap.md)、
環境構築時の実測記録は[M0の確認結果](m0-baseline.md)を参照してください。
qhgの実行結果と未検証範囲は[Instance初期化・診断の確認結果](m1-instance-baseline.md)に記載しています。

## 必要なツール

| 用途 | 必要なもの |
|---|---|
| CPU側の開発 | Git、rustup、Rust 1.99.0（edition 2024）と対象OSのリンカー |
| Vulkan依存のビルド | 上記に加えてC++コンパイラー。WindowsはMSVC x64、Linuxはg++ |
| 実Vulkan Instance試験 | Vulkan 1.3対応ドライバー／ローダー、`VK_LAYER_KHRONOS_validation`、`VK_EXT_debug_utils` |
| 実行環境の調査 | Vulkan SDKのvulkaninfo |
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

実行環境の調査・Validation Layerの用意・シェーダー開発ではVulkan SDK **1.4.341.1**を使います。
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
以下の通常のテスト実行では、Vulkanローダーの読み込み・Instance作成を行いません。
`--features vulkan`は実装をビルド対象へ含める指定で、実Vulkan試験の実行指定とは分かれています。

```sh
cargo check --locked --workspace --all-targets --no-default-features --features vulkan
cargo clippy --locked --workspace --all-targets --no-default-features --features vulkan -- -D warnings
cargo test --locked --workspace --no-default-features --features vulkan
```

`.github/workflows/ci.yml`はWindows / LinuxとCPU / Vulkan依存の組み合わせを確認します。
通常のVulkan featureテストでは、要件判定、診断コールバック、失敗時の所有権を
Vulkan実行環境なしで検証します。試験用のash関数テーブルによる解放順序の確認は、
実ドライバーの動作検証とは区別します。

## 実Vulkan Instance試験

次のコマンドで、`#[ignore]`を付けた実Vulkan試験だけを明示実行します。
通常の`cargo test`ではこれらをスキップします。

```sh
cargo test --locked -p renderer --no-default-features --features vulkan backends::vulkan::tests::vulkan_instance_ -- --ignored --nocapture
```

正常な初期化・終了の反復、INFOメッセージによるmessengerの受信確認、
Instance作成後の失敗と後始末を対象にします。失敗試験はmessenger作成前の
ホスト側の失敗注入であり、実ドライバーのメモリ不足を再現したものではありません。
qhgはDeviceを選択・作成せず、GPUへ描画・転送命令も投入しません。

ローダー、API、必須Layer／extensionが不足した場合は、理由を報告して試験を失敗させます。
実Vulkan受入試験ではwarning／errorが一件でもあれば失敗です。
例外にできるのは確認済みの誤検出だけで、メッセージID・対象バージョン・発生条件・根拠を
個別に記録し、ログにも残します。現時点の例外はありません。
診断コールバック内部の失敗も試験失敗に含め、終了中のメッセージまで集計します。

## 診断ログ

ライブラリは`tracing`へログを発行し、global subscriberや出力先を設定しません。
利用側は初期化前にsubscriberを設定します。実Vulkan試験は専用のsubscriberを用意し、
既定では`renderer=info,renderer::vulkan::validation=warn`で、初期化の情報と
Vulkanのwarning／errorをテスト出力へ表示します。受信・集計は全重要度のままです。
`--nocapture`で成功時も表示できます。
PowerShellで詳細ログを出す場合は、プロセス内で次のように指定します。

```powershell
$rendererPreviousRustLog = $env:RUST_LOG
try {
    $env:RUST_LOG = 'renderer=trace'
    cargo test --locked -p renderer --no-default-features --features vulkan backends::vulkan::tests::vulkan_instance_ -- --ignored --nocapture
} finally {
    $env:RUST_LOG = $rendererPreviousRustLog
}
```

VulkanからはGENERAL／VALIDATION／PERFORMANCEの全重要度を受信します。
ERRORは`error`、WARNINGは`warn`、INFOは`info`、VERBOSEは`trace`へ対応させます。
初期化の段階と必要条件、Layer／extension情報、検証メッセージID・種別・本文を記録し、
表示の絞り込みはsubscriber側で行います。ログを非表示にしても試験の件数判定は変わりません。

診断は直近64件と最初のwarning／error 128件を保持します。件数は保持上限とは独立して数え、
保持できなかった問題メッセージの件数も報告します。これは全ログの保存先ではありません。
集計結果は、対象のVulkan操作とInstanceの解放が終わった後に確認します。
コールバックから呼ばれるsubscriberはVulkan APIへ再入してはいけません。
通常のpanicはFFI境界で封じ込めますが、`panic=abort`やOOMによるabortからは回復しません。

## 依存関係と生成物

`cargo tree --locked --features vulkan`で、ashが一つのバージョンに解決されることを確認できます。
`tracing`はVulkan featureのoptional依存、ログ出力設定用の`tracing-subscriber`はテスト用依存です。
依存更新は独立した変更としてlockfile差分と両経路のビルドを検証します。
`target/`と一時ツール用の`.tools/`はGit管理対象外です。
シェーダーのビルド接続はM2までに実装し、手元だけのSPIR-Vファイルに依存しません。
