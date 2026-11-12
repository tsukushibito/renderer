# 開発環境

現時点の対象は[実装方針](renderer-implementation-policy.md)のM1基盤です。
privateなVulkan 1.3 Instanceの初期化・診断に加え、`renderer-7ro`で
デバイスの能力照会・自動選択、Deviceと単一Graphics＋Computeキューの作成・解放を扱います。
`renderer-yr6`ではVMAによるbufferの所有、CPUアクセス、解放を扱います。
`renderer-dj3`では単一キューでのbuffer転送、投入と完了待ち、実GPUからのreadbackを扱います。
`renderer-9wa`ではVMAによる転送用2D imageの作成、対応能力の検証、所有・解放を扱います。
`renderer-jya`では画像全体のbuffer → image → readback転送、layout遷移と完了後の再利用を扱います。
描画、RenderGraph、公開APIは後続の実装範囲です。
採用した契約は[ADR 0002](adr/0002-vulkan-instance-bootstrap.md)、
[ADR 0003](adr/0003-vulkan-device-bootstrap.md)、
[ADR 0004](adr/0004-vma-buffer-ownership.md)、
[ADR 0005](adr/0005-single-queue-buffer-transfer.md)、
[ADR 0006](adr/0006-vma-image-ownership.md)、
[ADR 0007](adr/0007-image-transfer.md)に記載しています。
環境構築時の実測記録は[M0の確認結果](m0-baseline.md)を参照してください。
qhgの実行結果と未検証範囲は[Instance初期化・診断の確認結果](m1-instance-baseline.md)に記載しています。
Deviceの実行結果と未検証範囲は[Device・単一キューの確認結果](m1-device-baseline.md)を参照してください。
bufferの実行結果と未検証範囲は[VMA bufferの確認結果](m1-buffer-baseline.md)を参照してください。
GPU転送の実行結果と未検証範囲は[buffer転送・完了待ちの確認結果](m1-transfer-baseline.md)を参照してください。
imageの作成・解放の実行結果と未検証範囲は[VMA imageの確認結果](m1-image-baseline.md)を参照してください。
画像転送とM1全体の対応は[画像転送・M1の確認結果](m1-image-transfer-baseline.md)を参照してください。

## 必要なツール

| 用途 | 必要なもの |
|---|---|
| CPU側の開発 | Git、rustup、Rust 1.99.0（edition 2024）と対象OSのリンカー |
| Vulkan依存のビルド | 上記に加えてC++コンパイラー。WindowsはMSVC x64、Linuxはg++ |
| 実Vulkan Instance試験 | Vulkan 1.3対応ドライバー／ローダー、`VK_LAYER_KHRONOS_validation`、`VK_EXT_debug_utils`、`VK_EXT_layer_settings` |
| 実Vulkan Device試験 | 上記に加え、Graphics＋Compute対応キューと必須3 featureを持つデバイス |
| 実Vulkan buffer試験 | Device試験の条件に加え、用途に対応するbufferの作成・CPUアクセスに必要なメモリ |
| 実Vulkan転送試験 | buffer試験の条件に加え、単一キューでのcopy・Synchronization2・Fence完了待ち |
| 実Vulkan image試験 | Device試験の条件に加え、RGBA8_UNORMのoptimal 2D imageのtransfer source / destination対応 |
| 実Vulkan画像転送試験 | imageとbuffer試験の条件に加え、image copy・layout遷移・Fence完了待ち |
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

Rustのモジュール配置は[AGENTS.md](../AGENTS.md)に従い、`mod.rs`を禁止します。
`foo.rs`と`foo/bar.rs`の形式を使います。workspaceの`clippy::mod_module_files`を
`deny`に設定しており、以下のclippyコマンドと既存CIで検出します。

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
通常のVulkan featureテストでは、要件判定、デバイス選択と作成設定、診断コールバック、
bufferの範囲・初期化済み範囲の検証、CPUアクセスの手順と失敗時の後始末を、Vulkan実行環境なしで検証します。
転送では、投入・完了待ちの失敗時の状態と、未完了リソースの保持・後始末も確認します。
試験用のash関数テーブルやCPUメモリによる確認は、実ドライバー・VMAとの接続確認と区別します。

## 実Vulkan試験

次のコマンドで、`#[ignore]`を付けた実Vulkan試験だけを明示実行します。
通常の`cargo test`ではこれらをスキップします。

### Instance初期化・診断

```sh
cargo test --locked -p renderer --no-default-features --features vulkan backends::vulkan::instance::tests::vulkan_instance_ -- --ignored --nocapture
```

正常な初期化・終了の反復、INFOメッセージによるmessengerの受信確認、
Instance作成後の失敗と後始末を対象にします。失敗試験はmessenger作成前の
ホスト側の失敗注入であり、実ドライバーのメモリ不足を再現したものではありません。
qhgはDeviceを選択・作成せず、GPUへ描画・転送命令も投入しません。

### Device・単一キュー

```sh
cargo test --locked -p renderer --no-default-features --features vulkan backends::vulkan::device::tests::vulkan_device_ -- --ignored --nocapture
```

デバイスの能力照会・自動選択、必須featureを有効にしたDeviceの作成、
単一Graphics＋Computeキューの取得、正常な解放を反復します。
必要条件はAPI variant 0のVulkan 1.3以上、`timelineSemaphore`、`synchronization2`、
`dynamicRendering`です。対応確認と作成時の有効化を分け、必要な3 featureだけを有効にします。

適合候補は専用GPU、内蔵GPU、仮想GPU、その他、CPU型の順に優先し、同順位は列挙順で選びます。
CPU型も最後の候補として利用できますが、選択をログで明示します。
CPU型での成功はソフトウェア実装上の確認として記録し、実GPUでの確認と分けます。
上記の実GPU受入試験はCPU型を選んだ場合に失敗します。
選択したデバイスの作成が失敗した場合は、そのデバイスと原因を報告して停止します。
このDevice試験ではbuffer／imageを作成せず、GPUへコマンドを投入しません。

### VMA bufferの所有・CPUアクセス

```sh
cargo test --locked -p renderer --no-default-features --features vulkan backends::vulkan::buffer::tests::vulkan_buffer_ -- --ignored --nocapture
```

Allocatorとupload用・GPU転送先用・readback用のbufferを作成し、CPUアクセスと解放を反復します。
uploadはCPU書き込み専用、GPU転送先はCPUアクセス禁止、readbackはCPUから読み書きできる契約です。
`write`／`read`へ渡したバイト列をコピーし、map／unmapと必要なflush／invalidateは内部で行います。
マップした領域や生のポインターを呼び出し元へ返しません。
このbuffer試験では成功したwriteで初期化した範囲を読み出し、範囲外や未初期化範囲のreadはエラーにします。

bufferはAllocationを単独で所有し、Allocator・Deviceの所有者を参照カウントで保持します。
Allocatorの操作用ハンドルを先に解放してもbufferは有効です。すべてのbufferと操作用ハンドルを
解放すると、Allocator、Device、Instanceの順で破棄します。これはGPU完了の保証とは別の契約です。
このbuffer試験の照合はCPUで書き込んだ内容を対象にします。
GPU転送後のreadback照合は次の転送試験で扱います。

実際に選ばれたメモリのtypeとpropertyをログへ残し、coherent／non-coherent経路の実行状況を区別します。
non-coherent向けのflush／invalidateと失敗時の後始末はCPU試験でも確認しますが、
該当するメモリがないGPUでの成功をnon-coherentの実機確認とは扱いません。
CPU型を選択した場合も、この実GPU受入試験の成功には数えません。

### 単一キューのbuffer転送・完了待ち

```sh
cargo test --locked -p renderer --no-default-features --features vulkan backends::vulkan::transfer::tests::vulkan_transfer_ -- --ignored --nocapture
```

UploadからDeviceTransfer、さらにReadbackへbuffer全体をcopyし、完了待ち後に全byteを照合します。
各bufferは同じAllocatorの所有者に属し、対応する用途と同じサイズを持ち、
Uploadの全範囲が初期化済みであることを、記録前に確認します。
image転送、RenderGraph、描画、フレーム並列化はこの試験に含めません。

記録済みオブジェクトから`submit`すると、完了待ち用のオブジェクトへ所有権が移ります。
bufferは記録開始から完了確認まで内部で保持され、呼び出し元はCPUアクセス・解放できません。
`wait`には待機時間を指定します。期限切れは未完了として保持し、再度待つことができます。
成功した完了確認後にbufferを返し、GPUが書いた範囲を読み出せる状態にします。
copy間とCPU read前の依存をSynchronization2で記録し、必要なinvalidateは従来の`read`内で行います。
正常経路では対象のFenceだけを待ち、queue／Device全体のidle待ちは使いません。

未完了オブジェクトの破棄は、10秒の待機予算を指定して後始末します。
Vulkanの待機時間は実装の精度で調整されるため、壁時計で厳密に10秒以内とは保証しません。
完了を確認できなければ、原因をログに残し、GPUが使い得るリソースと依存所有者を保持したまま、
そのDeviceの継続利用を止めます。この異常経路では保持した資源を回収できません。
`VK_ERROR_DEVICE_LOST`もDeviceを停止扱いにし、データ照合の成功とは扱いません。
Device停止後は新たな投入・通常のbuffer作成やCPUアクセスを拒否し、既存処理の後始末を行います。

転送試験ではパターンを変えた再投入と完了後のbuffer再利用、明示wait前の破棄も確認します。
失敗時の所有権と後始末はCPU境界試験で確認し、実driverのDevice lossやOOMを再現した結果とは区別します。

### VMA 2D imageの作成・所有・解放

```sh
cargo test --locked -p renderer --no-default-features --features vulkan backends::vulkan::image::tests::vulkan_image_ -- --ignored --nocapture
```

サイズを指定してRGBA8_UNORMの2D imageを作成します。optimal tiling、transfer source / destination、
ミップ1段・配列1層・サンプル数1に限定し、初期layoutはUNDEFINEDです。
作成条件の記述を所有・解放から分け、後でformatや用途の対応範囲を広げられる形にします。
既に作成したimageの用途やミップ数を変更するAPIではありません。

寸法0を拒否し、実際のformat・type・tiling・usage・flagsの組み合わせをDeviceへ照会して、
寸法などの上限を確認します。未対応と問い合わせ・作成の失敗を区別して報告します。
optimal imageの実メモリサイズを`幅 × 高さ × 4`と仮定せず、作成時のメモリ不足も扱います。
VMAにはAutoPreferDeviceを指定し、作成条件、debug object名、Allocationサイズ、
選択されたメモリのtype・propertyをログへ残します。

imageとAllocationは単独所有し、bufferと同じAllocator・Device所有者を保持します。
Allocatorの操作用ハンドルを先に破棄してもimageを維持し、最後のリソースを解放してから
VMA、Device、Instanceを破棄します。作成後の命名などで失敗しても所有型が後始末します。
Deviceが停止・lostの場合、新たなimage作成を拒否します。

実Vulkan試験は複数サイズの作成・解放と依存所有者の寿命を反復確認し、終了後まで診断を集計します。
この試験ではGPUへ命令を投入せず、imageの内容は未初期化のままです。
ImageView、CPUからのimage直接アクセスは後続で扱います。
レイアウト遷移、image転送・画素のreadbackは次の転送試験で扱います。

### 画像全体の転送・再利用

```sh
cargo test --locked -p renderer --no-default-features --features vulkan backends::vulkan::transfer::image::tests::vulkan_image_transfer_ -- --ignored --nocapture
```

Upload buffer → RGBA8_UNORM image → Readback bufferを通し、Fence待機後に全byteを照合します。
転送は画像全体、mip・layer・sample各1、buffer offset 0で、行間の余白を持たないRGBA列に限定します。
bufferの要求サイズは`幅 × 高さ × 4`と一致させ、overflow、用途違い、所有者違い、
Uploadの未初期化を記録前に拒否します。optimal imageのAllocationサイズとは区別します。

初回はUNDEFINED → TRANSFER_DST_OPTIMAL → TRANSFER_SRC_OPTIMALへ遷移します。
同じimage・bufferを完了後に再利用し、次回はTRANSFER_SRC_OPTIMALから開始します。
記録中は完了済みlayoutを変更せず、成功した待機後だけ新しい状態を確定します。
copy間とCPU read前の依存はSynchronization2で記録し、既存のflush / invalidateを使います。

submit / waitと未完了時のDrop・保持・Device停止はbuffer転送と共通の処理です。
imageとbufferは記録から完了まで移動所有され、成功後に返却されます。
複数寸法・パターンの照合、再利用、記録キャンセル、ゼロ時間pollと未完了時の再待機、Drop待機、
Allocatorハンドル先行破棄と終了後の診断を確認します。
部分領域や行長指定、ImageView、描画、Compute、画像ファイル出力はこの試験に含めません。

### 判定条件

ローダー、API、必須Layer／extensionが不足した場合は、理由を報告して試験を失敗させます。
Instance作成時に`VK_EXT_layer_settings`の`validate_sync=true`を指定し、
通常のcore validationにSynchronization Validationを追加するよう要求します。
レイヤー提供分を含めて必要なextensionを照会し、不足を黙って無効化しません。
環境変数や設定ファイルはAPI指定より優先されるため、要求ログだけで有効とは判断できません。
実Vulkan受入では外部設定を確認し、レイヤー起動時のINFOメッセージで同期検証の有効状態も確認します。
設定の優先順位は[LunarGのレイヤー設定](https://vulkan.lunarg.com/doc/view/latest/windows/layer_configuration.html)を参照してください。
Synchronization Validationはhost memory accessを追跡しないため、CPUアクセスの寿命契約はCPU試験でも確認します。
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
    cargo test --locked -p renderer --no-default-features --features vulkan backends::vulkan::device::tests::vulkan_device_ -- --ignored --nocapture
} finally {
    $env:RUST_LOG = $rendererPreviousRustLog
}
```

VulkanからはGENERAL／VALIDATION／PERFORMANCEの全重要度を受信します。
ERRORは`error`、WARNINGは`warn`、INFOは`info`、VERBOSEは`trace`へ対応させます。
初期化の段階と必要条件、Layer／extension情報、検証メッセージID・種別・本文を記録し、
表示の絞り込みはsubscriber側で行います。ログを非表示にしても試験の件数判定は変わりません。
デバイス選択では候補と除外理由、選択理由、デバイス名・種別・API、vendor／device ID、
driver名・追加情報・ID・生のバージョン値、選択したqueue familyとindexを記録します。
`driverVersion`の符号化はドライバー依存のため、汎用のVulkan APIバージョンとして分解しません。
bufferの作成では用途・要求サイズ・ハンドルと、選択されたメモリのtype・propertyを記録します。
imageの作成では寸法・format・usage・ハンドルと、対応上限・Allocationサイズ・メモリのtype・propertyを記録します。
失敗時は操作名と元のVulkanエラーを保持し、CPUアクセス時の範囲や用途のエラーも報告します。
転送ではdebug object名とcommand labelを付け、buffer・command資源・Fenceを診断と対応づけます。
投入と完了待ち、期限切れ、Device停止、回収できず保持した資源もログに残します。

診断は直近64件と最初のwarning／error 128件を保持します。件数は保持上限とは独立して数え、
保持できなかった問題メッセージの件数も報告します。これは全ログの保存先ではありません。
集計結果は、対象のVulkan操作とDevice・Instanceの解放が終わった後に確認します。
コールバックから呼ばれるsubscriberはVulkan APIへ再入してはいけません。
通常のpanicはFFI境界で封じ込めますが、`panic=abort`やOOMによるabortからは回復しません。

## 依存関係と生成物

`cargo tree --locked --features vulkan`で、ashが一つのバージョンに解決されることを確認できます。
`tracing`はVulkan featureのoptional依存、ログ出力設定用の`tracing-subscriber`はテスト用依存です。
依存更新は独立した変更としてlockfile差分と両経路のビルドを検証します。
`target/`と一時ツール用の`.tools/`はGit管理対象外です。
シェーダーのビルド接続はM2までに実装し、手元だけのSPIR-Vファイルに依存しません。
