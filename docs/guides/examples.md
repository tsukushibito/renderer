# Examplesの実行と選び方

リポジトリのルートでコマンドを実行します。RustとC/C++のセットアップは
[必要なツール](development.md#必要なツール)、GPUの検証設定は
[製品・開発用の検証設定](development.md#validation-settings)を参照してください。

## 最初に動かす

**Graphの合成をCPUだけで確認する**

`graph_composition`は公開Graph APIで利用側のRasterパスをつなぎます。
`rust_pipeline`は`renderer-pipeline`の標準レシピと独自パスを合成します。どちらもCPUでGraphをcompileします。

```sh
cargo run --locked -p renderer --no-default-features --example graph_composition
cargo run --locked -p renderer-pipeline --no-default-features --example rust_pipeline
```

**Vulkanで画像に描画する**

`headless`はウィンドウを作らず、indexed mesh、深度、Compute、readbackを通してPPMを保存します。
次の構成はValidation Layerを使い、Graphの診断を表示します。

```powershell
New-Item -ItemType Directory -Force .tools | Out-Null
cargo run --locked -p renderer --no-default-features --features vulkan,development-validation,graph-diagnostics --example headless -- .tools/headless.ppm
```

**Windowsで描画する**

`windowed`は標準Graphと独自Graphを切り替えられます。WASDとQ/Eで移動し、右ボタンを押して視点を操作します。
両APIとも`runtime::Runtime`と`WindowPresenter`で作業セットの準備、フレーム投入、完了確認、表示を行います。
画面操作と検証手順は[Windows表示](development.md#windows-presentation)を参照してください。

```powershell
cargo run --locked -p renderer --no-default-features --features vulkan,presentation --example windowed -- --api vulkan
cargo run --locked -p renderer --no-default-features --features d3d12,presentation --example windowed -- --api d3d12
```

**Bistroの小さなシーンを描画する**

`renderer`の`bistro`は公開Graph APIを使い、Rustでパスを定義して描画パイプラインを組み立てます。
`renderer-pipeline`の`bistro`は標準レシピとシーン固有のパスをRustで合成します。どちらも`--fixture`で小さなシーンを描画し、
レポートとキャプチャを出力します。実行ごとに新しい出力先を指定し、ビルド成果物もパッケージごとに保存します。

```powershell
cargo run --locked --release -p renderer --no-default-features --features vulkan --example bistro --target-dir target/bistro-rust -- --api vulkan --fixture --output .tools/bistro-rust-fixture
cargo run --locked --release -p renderer-pipeline --no-default-features --features vulkan --example bistro --target-dir target/bistro-pipeline -- --api vulkan --fixture --output .tools/bistro-pipeline-fixture
```

`renderer-pipeline`のBistroで全シーンを描画するには[Bistroのcook・描画手順](development.md#bistro-pbr)を参照してください。

## Windows版に含めるバックエンドを選ぶ

配布ビルドでは、アプリのCargo featureで利用できるバックエンドを指定します。
`windowed`をVulkan専用、D3D12専用、両対応でビルドする例です。
`--target-dir`で各構成の出力を個別に保持します。

```powershell
cargo build --locked --release -p renderer --no-default-features --features vulkan,presentation --example windowed --target-dir target/dist-vulkan
cargo build --locked --release -p renderer --no-default-features --features d3d12,presentation --example windowed --target-dir target/dist-d3d12
cargo build --locked --release -p renderer --no-default-features --features vulkan,d3d12,presentation --example windowed --target-dir target/dist-dual

& ./target/dist-dual/release/examples/windowed.exe --api vulkan
& ./target/dist-dual/release/examples/windowed.exe --api d3d12
```

単独版は対応形式のシェーダーを埋め込みます。両対応版は同じ実行ファイルで起動時にAPIを選びます。
`--api`を省略するとVulkanが利用可能なビルドではVulkanを、D3D12専用ビルドではD3D12を選びます。
ここでの利用可否はビルド構成に基づき、実機への適合は起動時に確認します。

## ターゲット一覧

| ターゲット | 確認できること | 実行条件 |
| --- | --- | --- |
| `graph_composition` | 利用側のRasterパスの合成と不要分岐の除去 | CPU。追加feature不要 |
| `rust_pipeline` | [`renderer-pipeline`の準備APIとパス合成](tooling.md#rust-pipeline)、標準レシピへの独自パスの接続 | CPU。追加feature不要 |
| `headless` | offscreen描画とPPMへの書き出し | `vulkan` |
| `windowed` | Windowsの表示ループ、標準／独自Graph、カメラ操作 | Windows、`presentation` と `vulkan` または `d3d12` |
| `renderer`の`bistro` | 公開Graph APIを使ったRustのパス定義と合成 | `vulkan` またはWindowsの`d3d12` |
| `renderer-pipeline`の`bistro` | [シーン描画とシャドウ再利用](tooling.md#bistro-scene-pipeline)、Bloom、トーンマッピング、SDR出力、[材質Programの切り替え](tooling.md#material-shader-build) | `vulkan`またはWindowsの`d3d12`。表示はWindowsと`presentation`。全シーンはcook済み素材とIBLが必要 |
| `frame_loop` | [フレーム進行、履歴、アセット更新](development.md#frame-loop) | `vulkan`。計測モードはrelease |
| `engine_workloads` | [大メッシュ、マルチView、アセット差し替え](development.md#engine-workloads) | `vulkan`。計測モードはrelease |
| `profile_workload` | [CPUプロファイル用のヘッドレス転送負荷](development.md#cpu-profile-workload) | Windows、`vulkan` または `d3d12` |
| `frame_memory` | [フレーム全体のallocationとnative資源イベントの計測](development.md#frame-memory) | release、`memory-profiling`。GPUモードは対象backendも指定 |
| `raster_draw_scaling` | [indexed draw数に対するCPU/GPUスケーリング](development.md#raster-draw-scaling) | Windows、release、`vulkan,d3d12` |
| `raster_batches` | 順序付きRaster batchと複数color attachment | Windows、release、`vulkan,d3d12` |
| `indexed_indirect` | GPUで生成したindexed draw引数と可視性判定 | Windows、release、`vulkan,d3d12` |
| `indexed_instancing` | 共有index bufferの部分範囲とinstance入力 | Windows、release、`vulkan,d3d12` |
| `msaa` | 同じGraphの1×と4× MSAAの比較 | Windows、release、`vulkan,d3d12` |

Bistroの全シーン以外の実行には、リポジトリに含まれる小さな入力を使います。
二APIの受入条件と測定コマンドは[開発手順](development.md)に、
実行済みの結果は[検証状況](../verification/current.md)に置いています。

通常のCIでは、小さなCPU fixtureで準備APIとGraph構築を検証します。
両方のBistroサンプルはLinuxのVulkan構成とWindowsの二API・表示構成でビルドを確認し、
CLI、シェーダーとの接続、レシピなどのサンプル固有のCPUテストはWindowsで各版を一度ずつ明示実行します。
実GPUテストは`cargo test -p <package> --example bistro`に対象のfeatureと`-- --ignored`を指定して実行します。

`rust_pipeline`と標準レシピを使う`bistro`の入口は`crates/renderer-pipeline/examples/`にあります。
ほかの入口は`crates/renderer/examples/`にあり、試験や計測と共用するfixtureのRustコードは`crates/renderer/fixtures/`にあります。
