# Examplesの実行と選び方

リポジトリのルートでコマンドを実行します。RustとC/C++のセットアップは
[必要なツール](development.md#必要なツール)、GPUの検証設定は
[製品・開発用の検証設定](development.md#validation-settings)を参照してください。

## メッシュと材質からビューを描く

```powershell
cargo run --locked --release -p renderer-pipeline --features vulkan --example textured_view -- vulkan
```

`textured_view` はチェッカーテクスチャを貼った三角形を描き、64×64 から 32×32 へリサイズして、メッシュの更新と赤から緑への材質変更を反映する。
`two_views` は同じオブジェクトを二つのカメラから一つの Graph に描く。`toon_views` は同じループを外部実装の Toon 方式で実行する。
`mixed_post` は PBR、Unlit、独自 Toon、半透明を描き、HDR に Bloom をかけて SDR に変換する。
各例は GPU の結果を読み戻し、表示領域と色の変化を確認する。Windows の D3D12 ではコマンドの両方の `vulkan` を `d3d12` に置き換える。

## 最初に動かす

**Graphの合成をCPUだけで確認する**

`graph_composition`は公開Graph APIで利用側のRasterパスをつなぎます。
`rust_pipeline`は`renderer-pipeline`の標準レシピと独自パスを合成します。どちらもCPUでGraphをcompileします。

```sh
cargo run --locked -p renderer --no-default-features --example graph_composition
cargo run --locked -p renderer-pipeline --no-default-features --example rust_pipeline
```

<a id="pbr-forward-example"></a>

**標準 PBR を小さなシーンで確認する**

`pbr_forward`はサンプル内で用意した小さなシーンを、公開レシピでオフスクリーン描画します。
型付きPBR頂点を使い、テクスチャを省略した材質を太陽光で照らします。
同じシーンを `--technique forward`（既定）、`forward-plus`、`deferred` で描き分けられます。
Forward+ では点光源を1灯追加します。Deferred は1×のみ選べます。
色と1×の深度を読み戻し、`--output`を指定するとSDRをPPMに保存します。
`--msaa 4`はカラーをリゾルブしてから出力し、`--msaa 1`は既定の1×を選びます。
`--bloom`は明るい三角形に標準のBloomを適用します。`--reuse-shadows`は連続する2フレームを投入して、
初回の描画と次フレームの全カスケード再利用を表示します。
`--copy-hdr`はシーン描画と出力の間にアプリ独自のコピーパスを追加します。

```sh
cargo run --locked -p renderer-pipeline --no-default-features --features vulkan --example pbr_forward -- --api vulkan
cargo run --locked -p renderer-pipeline --no-default-features --features vulkan --example pbr_forward -- --api vulkan --technique deferred
cargo run --locked -p renderer-pipeline --no-default-features --features d3d12 --example pbr_forward -- --api d3d12 --technique forward-plus --msaa 4 --bloom --reuse-shadows --output pbr-forward-plus.ppm
```

D3D12はWindowsで実行できます。準備とパスの接続は[PBRレシピの使い方](tooling.md#pbr-forward-recipe)を参照してください。

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

`renderer-bistro`の`bistro-graph`は公開Graph APIを使い、Rustでパスを定義して描画パイプラインを組み立てます。
同じパッケージの`bistro`は公開のPBR Forward、Forward+、Deferredと比較用の描画構成を組み立てます。どちらも`--fixture`で小さなシーンを描画し、
レポートとキャプチャを出力します。実行ごとに新しい出力先を指定します。

```powershell
cargo run --locked --release -p renderer-bistro --no-default-features --features vulkan --bin bistro-graph -- --api vulkan --fixture --output .tools/bistro-rust-fixture
cargo run --locked --release -p renderer-bistro --no-default-features --features vulkan --bin bistro -- --api vulkan --fixture --output .tools/bistro-pipeline-fixture
```

Bistroで全シーンを描画するには[Bistroのcook・描画手順](development.md#bistro-pbr)を参照してください。

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
| `rust_pipeline` | [準備APIとパス合成](tooling.md#rust-pipeline)、標準レシピへの独自パスの接続 | CPU。追加feature不要 |
| `pbr_forward` | [標準 PBR Forward](tooling.md#pbr-forward-recipe)で型付き頂点・既定の材質テクスチャ・1×／4×・Bloom・シャドウ再利用・独自HDRパスの接続を確認 | `vulkan`またはWindowsの`d3d12` |
| `headless` | offscreen描画とPPMへの書き出し | `vulkan` |
| `windowed` | Windowsの表示ループ、標準／独自Graph、カメラ操作 | Windows、`presentation` と `vulkan` または `d3d12` |
| `bistro-graph` | 公開Graph APIを使ったRustのパス定義と合成 | `vulkan` またはWindowsの`d3d12` |
| `bistro` | [シーン描画とシャドウ再利用](tooling.md#bistro-scene-pipeline)、Bloom、トーンマッピング、SDR出力、[材質Programの切り替え](tooling.md#material-shader-build) | `vulkan`またはWindowsの`d3d12`。表示はWindowsと`presentation`。全シーンはcook済み素材とIBLが必要 |
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
`pbr_forward`のCPUテストと両方のBistroサンプルのビルドは、LinuxのVulkan構成とWindowsの二API・表示構成で確認します。
BistroのCLI、シェーダーとの接続、レシピのCPUテストはWindowsで各版を一度ずつ実行します。
Bistroの実GPUテストは`cargo test -p renderer-bistro --lib`に対象featureと`-- --ignored`を指定して実行します。

`rust_pipeline`と`pbr_forward`の入口は`crates/renderer-pipeline/examples/`、Bistroの2つの入口は`crates/renderer-bistro/src/bin/`にあります。
ほかの入口は`crates/renderer/examples/`にあり、試験や計測と共用するfixtureのRustコードは`crates/renderer/fixtures/`にあります。
