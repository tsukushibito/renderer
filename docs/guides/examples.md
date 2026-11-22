# Examplesの実行と選び方

リポジトリのルートでコマンドを実行します。RustとC/C++のセットアップは
[必要なツール](development.md#必要なツール)、GPUの検証設定は
[製品・開発用の検証設定](development.md#validation-settings)を参照してください。

<a id="bistro-baked-lighting"></a>

## ライトマップと照度プローブを使う

ゲーム側が用意する入力は、ワールド座標の静的な三角形、線形色の材質とテクスチャ、二つの照明条件です。アセットビルドで一度だけ `renderer-bake` を使います。プローブの位置はジオメトリから自動で決まります。

```rust
let scene = BakeScene::new(occluders, materials, textures)?;
let probes = scene.bake_probes([day, dusk], ProbeSettings::default())?;
let maps = scene.bake_lightmaps(&static_receivers, [day, dusk], LightmapSettings::default())?;
probes.with_lightmaps(&maps)?.write(output)?;
```

プローブだけなら `bake_probes` の結果を直接保存できます。ライトマップだけなら `BakedLighting::from_lightmaps(&maps, fallback)` に、範囲外で使う各シナリオの RGB 照度を渡します。配置と LOD ごとに、描画順の三角形を `ReceiverTriangle` として渡してください。各チャートの UV はベイク側が生成します。描画メッシュの各三角形は 3 個の連続する固有頂点を使い、元の UV0 は材質用に保持します。

ロード時に `renderer::pbr::BakedLighting::from_bytes` でアセットを検証します。`image_desc()` で画像を準備し、`upload` の完了後、その画像のスナップショットをシーン入力に渡します。鏡面 IBL が必要な場合は `input.environment` も指定します。静的配置の `MaterialFactors::lightmap` にはゼロ始まりの `first_chart` と描画バッファ内の `first_vertex` を指定します。移動物体や未ベイクの LOD は `None` にすると各ピクセルのワールド位置と法線でプローブを引きます。表面が両面描画の場合、裏側もプローブを使います。

```rust
input.baked_lighting = Some(BakedLightingInput::new(atlas.snapshot(), time_of_day));
frame.draw_scene(input, exposure)?;
```

昼から夕方への係数は 0～1 です。太陽の方位・色、背景、時計の進行はアプリ側で更新します。ベイクは線形の拡散照度を供給し、直接光の影と鏡面 IBL は通常の描画経路を使います。

### Bistro で試す

[Bistro の cook と IBL](development.md#bistro-b3) の後、以下をリポジトリのルートで実行します。

```powershell
cargo run --release -p renderer-bistro --features bake-tool,vulkan --bin bistro-bake -- --manifest .tools/bistro-cook/full-a3/manifest.json --output .tools/bistro.gi --samples 96
cargo run --release -p renderer-bistro --features vulkan --bin bistro -- --api vulkan --manifest .tools/bistro-cook/full-a3/manifest.json --ibl-manifest .tools/b3/ibl-final/manifest.json --look lumberyard-day --view bistro-corner --baked-lighting .tools/bistro.gi --pipeline forward-plus --white-sphere -14 2.7 0.65 --time-sweep --frames 5 --width 800 --height 450 --output .tools/bistro-gi
```

`--fixture` は配布アセットなしでベイクと描画を試せます。`--backend cpu|vulkan|d3d12|auto` はベイクの実行先を選びます。Auto は利用可能な GPU を準備できなければ CPU を使います。`--spacing`、`--max-depth`、`--lightmap-texels` で密度と面の予算を変更でき、予算 0 ならシーン全体をプローブで描きます。出力には cook と照明プリセットの識別子を保存します。入力を変更したら再ベイクし、新しい出力名を指定してください。

`--scenario-blend 0.5` は中間時刻を固定します。`--time-sweep` のオフスクリーン実行は先頭、中間、末尾を保存します。`--gi-debug sources` はライトマップを橙、プローブを青系で表示し、`irradiance` は拡散照度、`weights` は有効なプローブ重みの合計を表示します。[実機の比較画像](../verification/current.md#bistro-baked-lighting)

### Bistro で局所的な鏡面反射を使う

`tools/bistro/reflection-probes.json` の各項目に、キャプチャ位置、箱の最小・最大座標、境界から内側へのフェード幅をメートルで指定します。ひさし下と奥の客席で 1 個ずつ使っています。アセットビルドでは `BakeScene::bake_reflection_probes(&placements, [day, dusk])?` を呼びます。

```powershell
cargo run --release -p renderer-bistro --features bake-tool --bin bistro-bake -- --manifest .tools/bistro-cook/full-a3/manifest.json --reflection-probes tools/bistro/reflection-probes.json --output .tools/bistro.reflections
cargo run --release -p renderer-bistro --features vulkan --bin bistro -- --api vulkan --manifest .tools/bistro-cook/full-a3/manifest.json --ibl-manifest .tools/b3/ibl-final/manifest.json --look lumberyard-day --view bistro-corner --reflection-probes .tools/bistro.reflections --baked-lighting .tools/bistro.gi --frames 5 --width 960 --height 530 --output .tools/bistro-local
```

`--baked-lighting` は任意です。アプリではロード時に `pbr::ReflectionProbes::from_bytes`、`image_desc()`、`upload` を使い、転送完了後の画像を保持します。各フレームで既存の BRDF LUT と範囲外の反射を持つ環境に組み合わせます。

```rust
input.environment = Some(&environment_maps);
input.reflection_probes = Some(ReflectionProbeInput::new(reflections.snapshot(), time_of_day));
frame.draw_scene(input, exposure)?;
```

`--reflection-debug weights` は第 1 箱を赤、第 2 箱を緑、グローバル寄与を青で表示します。`--reflection-debug radiance --time-sweep` は材質 BRDF を掛ける前の HDR を昼・中間・夕方で保存します。`--reflection-spheres -10 0.85 0 --reflection-spheres-to 7 0.85 7` を標準の direct 描画に追加すると、粗さ 0.08 と 0.55 の金属球が路上からひさし下を通って客席へ移動します。通常表示、プローブ無効、重み表示、radiance の各出力を使う[数値比較と実機画像](../verification/current.md#bistro-local-reflections)も参照してください。

## メッシュと材質からビューを描く

```powershell
cargo run --locked --release -p renderer --features vulkan --example textured_view -- vulkan
```

| 例 | エンジンに取り込める処理 |
| --- | --- |
| `textured_view` | チェッカーテクスチャを使う材質の作成、64×64から32×32へのリサイズ、メッシュと材質の更新 |
| `two_views` | 一度バインドしたシーンを複数のカメラから描画し、別々の画像に出力 |
| `toon_views` | 外部クレートでも実装できる `ViewPipeline` でメッシュと材質を利用 |
| `mixed_post` | PBR、Unlit、独自材質、半透明を含むシーンのHDRにBloomとSDR出力を追加 |

コマンドの `--example` の値を表の名前に替えて実行できる。WindowsのD3D12ではfeatureと末尾の引数をともに `d3d12` に替える。
ヘッドレスの各例は描画結果を読み戻して確認する。エンジン側のイベントループへ組み込む際の起動・更新・解放は[ビューとアセットの手順](tooling.md#scene-materials)、
実際のフレーム処理は[共通のサンプルコード](../../crates/renderer/examples/support/views.rs)を参照。

## 最初に動かす

**Graphの合成をCPUだけで確認する**

`graph_composition`は公開Graph APIで利用側のRasterパスをつなぎます。
`rust_pipeline`は`renderer`の標準レシピと独自パスを合成します。どちらもCPUでGraphをcompileします。

```sh
cargo run --locked -p renderer-core --no-default-features --example graph_composition
cargo run --locked -p renderer --no-default-features --example rust_pipeline
```

<a id="pbr-forward-example"></a>

**標準 PBR を小さなシーンで確認する**

`pbr_forward`はサンプル内で用意した小さなシーンを、公開レシピでオフスクリーン描画します。
型付きPBR頂点を使い、テクスチャを省略した材質を太陽光で照らします。
同じシーンを `--technique forward`（既定）、`forward-plus`、`deferred` で描き分けられます。
このサンプルのForward+では点光源を1灯追加します。標準レシピは3方式ともビューごとの点光源入力を扱います。Deferredは1×のみ選べます。
色と1×の深度を読み戻し、`--output`を指定するとSDRをPPMに保存します。
`--msaa 4`はカラーをリゾルブしてから出力し、`--msaa 1`は既定の1×を選びます。
`--bloom`は明るい三角形に標準のBloomを適用します。`--reuse-shadows`は連続する2フレームを投入して、
初回の描画と次フレームの全カスケード再利用を表示します。
`--copy-hdr`はシーン描画と出力の間にアプリ独自のコピーパスを追加します。

```sh
cargo run --locked -p renderer --no-default-features --features vulkan --example pbr_forward -- --api vulkan
cargo run --locked -p renderer --no-default-features --features vulkan --example pbr_forward -- --api vulkan --technique deferred
cargo run --locked -p renderer --no-default-features --features d3d12 --example pbr_forward -- --api d3d12 --technique forward-plus --msaa 4 --bloom --reuse-shadows --output pbr-forward-plus.ppm
```

D3D12はWindowsで実行できます。準備とパスの接続は[PBRレシピの使い方](tooling.md#pbr-forward-recipe)を参照してください。

**Vulkanで画像に描画する**

`headless`はウィンドウを作らず、indexed mesh、深度、Compute、readbackを通してPPMを保存します。
次の構成はValidation Layerを使い、Graphの診断を表示します。

```powershell
New-Item -ItemType Directory -Force .tools | Out-Null
cargo run --locked -p renderer-core --no-default-features --features vulkan,development-validation,graph-diagnostics --example headless -- .tools/headless.ppm
```

**Windowsで描画する**

`windowed`は標準Graphと独自Graphを切り替えられます。WASDとQ/Eで移動し、右ボタンを押して視点を操作します。
Windowsでは `Runtime::for_window` に `HasWindowHandle` を実装するウィンドウ所有者を渡し、`WindowPresenter` を受け取ります。
この例はwinitを使います。エンジン独自のウィンドウとイベントループからも呼べます。
同じデバイスに追加のウィンドウを接続するときは `Runtime::create_presenter` を使います。
画面操作と検証手順は[Windows表示](development.md#windows-presentation)を参照してください。

```powershell
cargo run --locked -p renderer-core --no-default-features --features vulkan,presentation --example windowed -- --api vulkan
cargo run --locked -p renderer-core --no-default-features --features d3d12,presentation --example windowed -- --api d3d12
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

<a id="bistro-daylight"></a>

### Bistro 屋外の昼景

Bistro はフレームごとにカメラ行列、クリップ距離、太陽の方向・照度をまとめます。標準 PBR はその値からビューを組み立て、手動 Graph は GPU へ転送する境界で uniform に変換します。

両方の実行例で `--look lumberyard-day` を指定できます。日向の石畳と日陰の店舗を一緒に比較できる視点が `bistro-corner` です。太陽、環境光の強さ、背景色はサンプルがロード時に設定し、各視点で共有します。露出は EV +1 です。省略時は検証用の `reference` を使います。

```powershell
target/release/bistro.exe --api vulkan --manifest .tools/bistro-cook/full-a3/manifest.json --ibl-manifest .tools/b3/ibl-final/manifest.json --look lumberyard-day --view bistro-corner --width 960 --height 530 --output .tools/daylight
```

同じコマンドを `--look reference --output .tools/daylight-before` に変更すると同一構図の比較元を取得できます。次の手順で、公式プレートと両方の SDR キャプチャを並べ、外壁、ひさし、植生、日向と日陰の石畳を計測します。[ORCA の屋外画像 1](https://d29g4g2dyqv443.cloudfront.net/sites/default/files/akamai/Bistro_Exterior_1.png)

```powershell
python tools/bistro/compare_daylight.py .tools/Bistro_Exterior_1.png .tools/daylight-before .tools/daylight .tools/daylight-comparison.png
```

移動確認は `--capture-route-clip` で既存ルートの先頭 2 秒を 30 fps で取得します。色・深度と撮影条件は出力先の `route/<frame>/` に入ります。フレーム番号は 60 Hz のルート時間です。[比較結果](../verification/current.md#bistro-daylight)

## Windows版に含めるバックエンドを選ぶ

配布ビルドでは、アプリのCargo featureで利用できるバックエンドを指定します。
`windowed`をVulkan専用、D3D12専用、両対応でビルドする例です。
`--target-dir`で各構成の出力を個別に保持します。

```powershell
cargo build --locked --release -p renderer-core --no-default-features --features vulkan,presentation --example windowed --target-dir target/dist-vulkan
cargo build --locked --release -p renderer-core --no-default-features --features d3d12,presentation --example windowed --target-dir target/dist-d3d12
cargo build --locked --release -p renderer-core --no-default-features --features vulkan,d3d12,presentation --example windowed --target-dir target/dist-dual

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

`rust_pipeline`と`pbr_forward`の入口は`crates/renderer/examples/`、Bistroの2つの入口は`crates/renderer-bistro/src/bin/`にあります。
ほかの入口は`crates/renderer-core/examples/`にあり、試験や計測と共用するfixtureのRustコードは`crates/renderer-core/fixtures/`にあります。
