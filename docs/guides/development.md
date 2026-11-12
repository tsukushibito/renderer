# 開発・検証手順

以下のコマンドはリポジトリのルートで実行します。
実装範囲と設計上の契約は[Vulkan基盤の設計](../design/vulkan.md)、
今後の方針は[実装方針](../design/implementation-policy.md)を参照してください。
現在の実行結果と未確認範囲は[最新の検証結果](../verification/current.md)にまとめます。

## 必要なツール

| 用途 | 必要なもの |
|---|---|
| 通常のCPUライブラリビルド | Git、rustup、Rust 1.99.0（edition 2024）、対象OSのリンカー |
| テスト・benchmark・全target確認 | 上記に加えてCコンパイラー。Criterionの依存allocaがC helperをビルドする |
| Vulkan featureのビルド | C++コンパイラー。vk-memが同梱VMAのC++をビルドする |
| 実Vulkan試験 | Vulkan 1.3対応ドライバー／ローダー、Validation Layer、debug_utils、layer_settings |
| 実行環境・シェーダーの調査 | Vulkan SDK 1.4.341.1のvulkaninfo、glslc、spirv-val |
| 依存・GPU内容の調査 | cargo-deny 0.20.2、RenderDoc 1.47。[ツールガイド](tooling.md)を参照 |
| 課題管理 | Beads v1.3.4。[導入・同期手順](../../.beads/README.md)を参照 |

[rust-toolchain.toml](../../rust-toolchain.toml)がRust・rustfmt・clippyを固定します。
初回のrustupやCargoによる取得にはネットワーク接続が必要です。
[Cargo.lock](../../Cargo.lock)も管理し、通常の確認では`--locked`を使います。
通常のCPUライブラリビルドは開発用依存を使わず、GPU・native依存を持ちません。
テストやbenchmarkのビルド条件はそれと区別します。

### Windows

rustupの`x86_64-pc-windows-msvc`を使います。
Visual Studio 2022またはBuild Tools 2022の「C++によるデスクトップ開発」から、
MSVC x64/x86ビルドツールとWindows SDKを用意してください。
この構成で、CriterionのC helperとVMAのC++をビルドできます。
MSVCが検出されない場合はDeveloper PowerShell for VS 2022で実行します。

実Vulkan試験とシェーダー開発ではVulkan SDK **1.4.341.1**を使います。
複数SDKがある環境では、固定版のパスを明示してください。

```powershell
& C:/VulkanSDK/1.4.341.1/Bin/vulkaninfoSDK.exe --summary
& C:/VulkanSDK/1.4.341.1/Bin/glslc.exe --version
```

### Linux

Ubuntu 24.04でrustupとネイティブビルドツールを用意します。

```sh
sudo apt-get update
sudo apt-get install -y build-essential
rustup show active-toolchain
```

CPU側のテスト・benchmarkにはgcc、Vulkan featureにはg++も必要です。
CIのLinux確認ジョブではgccを明示的に用意し、Vulkanジョブではg++も追加します。
Windowsジョブはrunner付属のMSVCを使います。
ビルド確認とLinux実GPUでの確認は別の結果として扱います。

## ビルド・品質確認

Rustのモジュール配置とテスト方針は[AGENTS.md](../../AGENTS.md)に従います。
`mod.rs`は禁止し、`foo.rs`と`foo/bar.rs`の形式を使います。
workspaceの`clippy::mod_module_files`を`deny`に設定しています。

CPU経路の確認にはVulkan SDK、ローダー、GPU、ウィンドウシステムは不要です。
テスト・全target確認には前述のCコンパイラーが必要です。

```sh
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets --no-default-features
cargo clippy --locked --workspace --all-targets --no-default-features -- -D warnings
cargo test --locked --workspace --no-default-features
```

Vulkan実装もビルド対象へ含めます。
ashは実行時ロードするため、ビルドにVulkan SDKやリンクライブラリは不要です。
以下の通常テストはVulkanローダーを読み込まず、Instanceも作成しません。

```sh
cargo check --locked --workspace --all-targets --no-default-features --features vulkan
cargo clippy --locked --workspace --all-targets --no-default-features --features vulkan -- -D warnings
cargo test --locked --workspace --no-default-features --features vulkan
```

[CI](../../.github/workflows/ci.yml)はWindows／LinuxとCPU／Vulkan featureを確認し、
別jobで[cargo-deny](tooling.md#cargo-deny)の依存検査を実行します。
依存更新ではlockfile差分と両feature構成を確認します。

## 実Vulkan試験

実Vulkan試験は`#[ignore]`で分離し、明示実行します。
`--features vulkan`はビルド対象を増やす指定で、GPU試験の実行指定とは別です。
正しさ試験をまとめて実行する場合は、関数名の`::vulkan_`で絞ります。
後述の性能試験はこのfilterに含まれません。

```sh
cargo test --locked -p renderer --no-default-features --features vulkan ::vulkan_ -- --ignored --nocapture
```

一部だけを確認するときは、上の`::vulkan_`を次のfilterへ置き換えます。

| 対象 | filter |
|---|---|
| Instance初期化・診断・部分失敗 | `backends::vulkan::instance::tests::vulkan_instance_` |
| Device・単一キュー | `backends::vulkan::device::tests::vulkan_device_` |
| VMA buffer・CPUアクセス | `backends::vulkan::buffer::tests::vulkan_buffer_` |
| buffer転送・完了待ち | `backends::vulkan::transfer::tests::vulkan_transfer_` |
| VMA imageの作成・所有 | `backends::vulkan::image::tests::vulkan_image_` |
| 画像転送・再利用 | `backends::vulkan::transfer::image::tests::vulkan_image_transfer_` |
| 実行資源の再利用 | `::vulkan_execution_pool_smoke` |
| 共有staging・転送バッチ | `::vulkan_batch_smoke` |

Deviceを使う試験にはGraphics＋Computeキュー、`timelineSemaphore`、
`synchronization2`、`dynamicRendering`が必要です。
image試験にはRGBA8_UNORMのoptimal 2D imageのtransfer source／destination対応も必要です。
必要条件の不足は理由を報告して失敗し、黙ってskipしません。
CPU型のソフトウェア実装は通常の候補になれますが、実GPU受入の成功には数えません。

### Validationの判定条件

既定は`ValidationMode::Required`で、`VK_LAYER_KHRONOS_validation`と
`VK_EXT_debug_utils`、`VK_EXT_layer_settings`を要求します。
`validate_sync=true`により通常のcore validationへSynchronization Validationを追加します。
外部設定はAPI指定を上書きできるため、起動INFOからSyncValとcore validationの実効状態も確認します。
設定の優先順位は[LunarGのレイヤー設定](https://vulkan.lunarg.com/doc/view/latest/windows/layer_configuration.html)を参照してください。

実Vulkan受入ではwarning／errorが一件でもあれば失敗です。
例外にできるのは確認済みの誤検出だけで、メッセージID・対象バージョン・発生条件・根拠を
個別に記録し、ログにも残します。現時点の例外はありません。
診断コールバック内部の失敗も含め、Device・Instanceの終了後まで集計します。
SyncValはhost memory accessを追跡しないため、CPUアクセスの寿命・失敗時の保持はCPU試験でも確認します。
CPU境界での失敗注入を、実ドライバーのOOM・Device loss・non-coherentメモリの確認とは扱いません。

## CPU転送計画の性能測定（Criterion）

同じbufferへの多数の部分転送について、同期判定と初期化済み範囲の管理を測定します。
専用targetは本番のRustソースを組み込み、内部APIを公開せずGPUも初期化しません。
ビルドにはCriterionのC helper用コンパイラーが必要ですが、SDKやVMA用C++コンパイラーは不要です。

```sh
cargo bench --locked -p renderer --no-default-features --bench transfer_planning
```

測定・統計解析をせず全caseを短く確認する場合は、末尾に`-- --test`を付けます。
比較用Vec実装と現行の順序付き範囲集合へ、同じ入力を渡します。
8・128・1,024・8,192件の隙間を持つ範囲を、順方向・逆方向・固定seedの並べ替え順で使います。
N件のwrite後にN件のreadを計画し、throughputは2N件のメタデータアクセスを表します。
入力生成、初期状態の構築、最終所有者の破棄は計測外です。
検索・挿入・依存判定と、それに伴う割り当て・追跡集合の破棄は計測に含めます。
buffer作成、stagingへのコピー、submit、GPU実行時間は含みません。
既定は30 samples、250 msのwarm-up、1秒のmeasurementで、CLIから変更できます。

同じ機材・負荷・設定で名前付き基準値を保存し、変更後に比較します。

```sh
cargo bench --locked -p renderer --no-default-features --bench transfer_planning -- --save-baseline planning-main
cargo bench --locked -p renderer --no-default-features --bench transfer_planning -- --baseline planning-main
```

保存先の既定はGit対象外の`target/criterion/`です。
[Cargo設定](../../.cargo/config.toml)がworkspace内の絶対パスを`CRITERION_HOME`で渡します。
既存の環境変数`CRITERION_HOME`があればそちらを優先します。
出力先を変える場合は`CRITERION_HOME`を指定してください。`CARGO_TARGET_DIR`とは独立した設定です。
名前付き基準値の実行間比較と、同一実行内の比較用実装との比較は区別します。
同じbenchmark IDの基準値がない場合、`--baseline`は失敗します。
詳細は[CriterionのCLI文書](https://criterion-rs.github.io/book/user_guide/command_line_options.html)を参照してください。

推定時間と信頼区間は個々の転送のp95とは異なります。
件数に対する増え方と小件数の管理コストを確認し、通常CIに壁時計時間の固定合否閾値を設けません。
機材・OS負荷による揺れも含め、結果は[最新の検証結果](../verification/current.md)で確認します。

## GPU転送の性能測定

性能試験だけが`ValidationMode::Disabled`を明示指定します。
debug_utilsと診断経路は維持し、releaseビルドだけで自動的に検証を外しません。
外部loader設定によるLayerの強制注入も確認してください。
VVLの起動状態を受信した場合は測定を失敗させ、ログ非表示だけを検証なしとは扱いません。

releaseと`RENDERER_RUN_PERFORMANCE=1`を指定します。

```powershell
$rendererPreviousPerformance = $env:RENDERER_RUN_PERFORMANCE
try {
    $env:RENDERER_RUN_PERFORMANCE = '1'
    cargo test --release --locked -p renderer --no-default-features --features vulkan performance_transfer_measurements -- --ignored --nocapture --test-threads=1
} finally {
    $env:RENDERER_RUN_PERFORMANCE = $rendererPreviousPerformance
}
```

Linuxでは次のように実行します。

```sh
RENDERER_RUN_PERFORMANCE=1 cargo test --release --locked -p renderer --no-default-features --features vulkan performance_transfer_measurements -- --ignored --nocapture --test-threads=1
```

固定負荷のwarm-up後、CPUのwrite・record・submit・wait（回収を含む）・readbackを分けて測定します。
中央値・p95、資源の作成・再利用回数を報告し、パターン生成と全byte照合は計測から外します。
GPU timestampはqueueの対応と精度を確認し、実Fence完了後に取得します。
非対応環境はGPU時間を未取得と報告します。

native資源の毎回作成と再利用の比較に加え、複数submitと単一バッチの比較があります。
後者も個別側の全件をsubmitしてから待ち、GPU時間は測定しません。
これらを複数フレームのthroughputや描画FPSの結果とは扱いません。
正しさ試験の検証有効結果と性能結果は別に記録します。

## 診断ログと生成物

ライブラリは`tracing`へログを発行し、global subscriberや出力先を設定しません。
実Vulkan試験の既定filterは`renderer=info,renderer::vulkan::validation=warn`です。
`--nocapture`で成功時も表示できます。詳細な転送ログは`debug`で有効になります。

```powershell
$rendererPreviousRustLog = $env:RUST_LOG
try {
    $env:RUST_LOG = 'renderer=trace'
    cargo test --locked -p renderer --no-default-features --features vulkan ::vulkan_ -- --ignored --nocapture
} finally {
    $env:RUST_LOG = $rendererPreviousRustLog
}
```

表示filterにかかわらず診断の全重要度を受信・集計します。
問題メッセージはID・本文・対象操作とともに確認し、終了中の診断も含めて判定します。
callbackから呼ばれるsubscriberはVulkan APIへ再入してはいけません。
保持上限やエラー時の所有契約は[Vulkan基盤の設計](../design/vulkan.md)を参照してください。

`target/`とローカルツール・確認用ファイルの`.tools/`はGit管理対象外です。
現在の設定・結果は対応する文書を更新し、過去版の記録はGit履歴から参照します。
