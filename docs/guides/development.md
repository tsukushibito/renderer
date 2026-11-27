# 開発・検証手順

以下のコマンドはリポジトリのルートで実行します。
実装範囲と設計上の契約は[Vulkan基盤の設計](../design/vulkan.md)、
今後の方針は[実装方針](../design/implementation-policy.md)を参照してください。
現在の実行結果と未確認範囲は[最新の検証結果](../verification/current.md)にまとめます。

## Survivor の不具合を記録・再現・比較する

Windows の開発ビルドで次を実行する。例の `reports` は保存先の親ディレクトリで、
各ジョブはその直下に新しい報告を作る。

```powershell
cargo build --locked --release -p survivor --features diagnostics,d3d12
target/release/survivor.exe --backend vulkan --debug-dir reports --clip-history
```

**F8** で HUD を含む PNG、入力、状態、イベントをまとめて保存する。**F9** は無音録画の
開始・停止、**F10** は直前5秒と直後2秒の採取。動画を使う環境では Python に PyAV と
NumPy を用意する。`--clip-history` を省くと過去の映像を保持しない。**右クリック**で
個体を選び、`--inspect-stdio` を追加したプロセスには NDJSON で同じ操作を送れる。
利用可能なビュー・深度・表示切替は `hello` が返す。

次は報告の閲覧、再現、画像比較の例。PowerShell の変数には調査対象の報告を指定する。
別マシンで再生する際は、記録したアセットに対応するゲームを使う。

```powershell
$report = 'reports/対象のジョブ'
python tools/diagnostics/timeline.py show $report --frame 42
target/release/survivor.exe --headless --verify-report $report
target/release/survivor.exe --headless --backend vulkan --width 320 --height 180 --replay "$report/replay.json" --record-report reports/replayed --all-frames
python tools/diagnostics/visual_diff.py compare reports/baseline reports/replayed reports/comparison
```

`--all-frames` は各固定更新後の PNG の読み戻しと保存を待つ。入力がゼロなら初期画面
1枚を出力する。同一バックエンド・解像度のオフライン反復は既定の全チャンネル
完全一致で比較する。単一時点は `--tick N` で指定できる。比較結果には各画像の
tick・view・採取段階・差分画素数が入り、失敗時には基準・実画像、8倍に強調した
差分、入力を含む両方の報告が残る。既知の差を許容する場合は
`--channel-tolerance`（8 bit 値）と `--changed-fraction`（全画素に対する割合）を指定する。
デスクトップの表示直前を経由する PNG は線形 RGBA8 の量子化を含むため、
オフラインの基準とは採取経路も記録して比較する。

Bistro の固定ビュー・巡回の `report.json` は
`python tools/diagnostics/visual_diff.py bistro OUTPUT STREET_REPORT ROUTE_REPORT` で
同じ閲覧・比較形式に取り込める。元画像のハッシュと寸法を照合し、カメラ・素材の
識別情報を各フレームに保存する。描画が使えない環境では `--headless --record-report DIR`
で入力・イベントを記録し、`--verify-report` で再現できる。
[診断の形式・性能計測](../design/live-inspector.md)

CI の WebGPU ジョブは SwiftShader が実際に表示した 320×180 の fixture を比較する。
基準は [main の実行 45935142467](https://github.com/tsukushibito/sashiko/actions/runs/45935142467)
の `webgpu-fixture/fixture-shadowed/canvas.png`。チャンネル差 2 を超える画素の
許容割合は 0.001。失敗時は Actions の `diagnostic-visual-comparison` を開く。

<a id="cpu-jobs"></a>

## CPU ジョブを検証する

Survivor のネイティブ版は、起動時に検出した並列度でゲームジョブを実行する。人数を固定するには実行ファイルと同じディレクトリに `sashiko.json` を置くか、`survivor --config PATH` でファイルを指定する。引数を指定したときはそのファイルだけを読む。並列度はホストを含むため、次の例では補助ワーカーを 7 人起動する。

```json
{ "jobs": { "parallelism": 8 } }
```

既定のファイルや設定項目を省略するか `"auto"` を指定すると、利用可能な並列度を使う。検出できなければ 1 を使う。ネイティブ版では同じ人数を Rapier の専用プールにも指定する。例えば 1 ならゲームジョブは呼び出し元で実行し、物理ステップ中は専用ワーカー 1 人が計算して呼び出し元は完了を待つ。ヘッドレス実行でも物理側の指定は有効。変更は次回起動時に反映される。明示ファイルの欠落や設定値の誤りは起動エラーになる。Windows 埋め込み版はセッション開始時にホスト実行ファイル横の設定を読む。

ネイティブのスコープ契約と定常時ヒープ、軽い加算の起床コスト、PoseBatch の連続実行・16 ms 休止後の p50 / p95 を確認する。PoseBatch は 16 / 64 関節、8 / 256 姿勢で最大並列度 1・2・4・8 を比較する。先頭 1/4 のみブレンドする条件も測る。カウンタ計測は別に実行する。

CPU と Web のサンプルは `sashiko-heap` の `CountingAllocator` をグローバルアロケータに設定し、ジョブが合流した境界で `HeapSnapshot` の差分を取る。GPU の作成・破棄と VMA のホスト割り当ても測る描画ホストは `memory-profiling` と `sashiko_renderer::core::measurement::Snapshot` を使う。外部の描画ホストでスケジューラを計測するには、依存する `sashiko-renderer` の `scheduler-metrics` feature を有効にすると `JobSystem::scheduler_counts` が使える。

```sh
cargo test --locked --release -p sashiko-jobs -p sashiko-animation
cargo run --locked --release -p sashiko-jobs --example frame_jobs
cargo run --locked --release -p sashiko-animation --example pose_jobs
cargo run --locked --release -p sashiko-animation --example pose_jobs -- --idle
cargo run --locked --release -p sashiko-animation --features scheduler-metrics --example pose_jobs
```

実験用 Rayon と既定ワーカーの契約を同じテストで比較する。フレーム計測の `--rayon` はホスト生成時にだけ作用する。成立した仕事の件数や CPU 時間はスケジューラ固有のカウンタから推定せず、同じ入力の結果とホストの経過時間を使う。

```sh
cargo test --locked --release -p sashiko-jobs -p sashiko-animation --features sashiko-animation/experimental-rayon
cargo run --locked --release -p sashiko-jobs --features experimental-rayon --example frame_jobs -- --rayon
cargo run --locked --release -p sashiko-animation --features experimental-rayon --example pose_jobs -- --rayon --idle
```

型消去した借用クロージャの受け渡しと panic 時の合流は `cargo +nightly-2026-11-15 miri test --locked --target x86_64-unknown-linux-gnu -p sashiko-jobs` で検証できる（同 nightly の miri と rust-src が必要）。

Rapier とゲームの CPU ジョブで Rayon プールを共有する試作は、下の二つのコマンドを一つずつ実行して比較する。ブラウザ側には Google Chrome、Playwright、下記の Wasm ツールチェーンを使う。`--wasm-bindgen` で CLI のパスを指定できる。各行の先頭は実行方式、並列度、負荷名。時間の単位は µs。計測窓と結果の読み方は[共有プールの評価](../verification/current.md#shared-worker-budget)を参照する。

```sh
python -B tools/worker-budget/compare.py native
python -B tools/worker-budget/compare.py browser
```

ブラウザのサンプルは同じ Rust 処理を二通りの Wasm にビルドする。共有メモリ版だけに固定 nightly と atomics 用の標準ライブラリを使う。リポジトリのルートで次を実行する。`wasm-bindgen-cli` は Cargo.lock の版に合わせる。

```sh
rustup target add wasm32-unknown-unknown
rustup toolchain install nightly-2026-11-15 --profile minimal --component rust-src --target wasm32-unknown-unknown
cargo install --locked wasm-bindgen-cli --version 0.2.130
python -B examples/jobs-web/build.py
python -m pip install playwright==1.65.0
python -m playwright install chromium --no-shell
python -B examples/jobs-web/verify.py build/jobs-web
python -B examples/jobs-web/serve.py build/jobs-web
```

ブラウザで Rayon も検証するときは `build.py` と `verify.py` の両方に `--experimental-rayon` を追加する。逐次版・既定の共有メモリ版に加え、検証用 Wasm で両バックエンドを明示的に選択し、姿勢行列、Worker 数、部分起動失敗、終了・再生成を検査する。

ローカル配信時は表示された URL を開き、Start host、Run batch を順に押す。ページの入力欄は計算中も編集できる。Stop host で親 Worker とその子 Worker を終了する。配信先では HTTPS（開発時は localhost）を使い、文書と Worker スクリプトに `Cross-Origin-Opener-Policy: same-origin` と `Cross-Origin-Embedder-Policy: require-corp` を付け、サンプル資産を同一オリジンで配信する。分離ヘッダーを付けない配信では逐次版を選ぶ。「Require parallel execution」を指定すると要件不足を初期化エラーとして表示する。

<a id="graph-review-gates"></a>

## Graphの容量・保証再利用・出力APIを変更したときの確認

Graphの上限は[容量の数え方](../design/rendergraph.md#capacity-planning)と公開Rustdocを確認する。
`GraphError::capacity()`で不足領域を識別し、予約量の変更はconsumerが返った明示境界で行う。
runtime側の`index_prefixes_per_resource`は、同じbuffer内容について利用する異なるindex-count数の上限である。
一般のprefix drawを使うときは必要数を予約する。管理下geometryの全countだけを描く経路やindexを使わない経路では0にできる。

pass内overlap/read圧縮を変更する場合は、`graph::accesses::tests`の生成したpairwise参照との一致、
writer数4/5の境界、元の宣言順の競合pair、全access容量、read属性/rangeの昇格とpass間resetを確認する。
その後も以下のnative受入と通常frameメモリ窓を実行する。
局所probeと通常release全Bistroは測定範囲を分け、前後で同じprofile・画質・計測境界・CPU条件を使う。
[現在のGraph構築比較](../verification/current.md#rendergraph-hotspot-optimization)はsource/EXE/PDBのidentity、
全独立process、個別sampleのp95、一般入力の交換条件、未実行範囲を記録している。

`FrameGraph`は確定済み出力を持つ。標準snapshotの拡張と出力選択は`sashiko_renderer::unlit_forward::snapshot::build_graph`のcallback内で行い、
`output()`／`readback()`のhandleを保存してから`compile()`へ渡す。任意の構築を続ける場合は
`connect_graph`と通常のGraphを使い、確定済みの出力・所有情報を個別に変更しない。

CPUで追加検証の有無を確認した後、nativeに影響する修正は両構成でGPU試験を明示実行する。
以下のlib試験は省略時に成功扱いにせず、ログで各filterの試験数が1件であることも確認する。
Vulkan Core/SyncValとD3D12 Debug Layer/GBVの準備が必要で、通常ホストでGPU試験を逐次実行する。

```powershell
foreach ($features in @('vulkan,d3d12,memory-profiling', 'vulkan,d3d12,memory-profiling,development-validation,graph-diagnostics')) {
    cargo test --locked --workspace --release --no-default-features --features $features
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    foreach ($test in @('vulkan_owned_raster_descriptor_capacity_recovery', 'd3d12_raster_srv_reuse_preserves_views_cancel_and_pending_ownership', 'd3d12_index_provenance_capacity_rejects_and_recovers')) {
        cargo test --locked -p sashiko-renderer-core --release --lib --no-default-features --features $features $test -- --ignored --nocapture --test-threads=1
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    }
}
```

これらは共有descriptorの容量／取消／再利用、attachmentだけのRTV/DSV、一般prefixの容量拒否と
管理下meshへの回復を確認する。描画内容・寿命の全体には既存の[owned Raster受入](#bistro-pbr)と
[B3受入](#bistro-b3)、通常frameの確保／再確保／解放には既存の`frame_memory`を併用する。
build/cookを終えてからGPU受入を実行し、ログ・binary hash・source commit・featureと未実行範囲を保存する。
構造上の走査回数削減やCPU小負荷の比較を、通常Bistroの速度改善やGPU/FPSの結果へ読み替えない。

## WebGPU のブラウザ実機試験

WebGPU を有効にした Chromium と対応する ChromeDriver を用意し、
`chromedriver` と Cargo.lock の `wasm-bindgen` に一致するバージョンの
`wasm-bindgen-test-runner` を PATH に追加する。次の試験はブラウザのデバイスを
使い、公開 Runtime の完了通知、リソースの世代、読み戻し、深度付きのテクスチャ描画を検査する。

```powershell
rustup target add wasm32-unknown-unknown
$env:CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER = "wasm-bindgen-test-runner"
cargo test --locked --release --target wasm32-unknown-unknown -p sashiko-renderer-core --features webgpu --test webgpu_runtime -- --include-ignored
```

WebDriver を使わずにインストール済みのブラウザで実行するには、同じコマンドに
`$env:NO_HEADLESS = "1"` を設定し、端末に表示された localhost の URL を開く。
Python の Playwright があれば、`python tools/run_webgpu_tests.py --channel chrome`
でビルドから実機テストの判定まで自動化できる。Edge は `--channel msedge` を指定する。
Windows の CI は Playwright に固定した Chromium を使い、`--channel chromium --software --development-validation` で同じテストを実行する。GPU を持たないランナーでは ANGLE の画面合成に WARP、WebGPU の描画に SwiftShader を使う。

フルシーンの画像比較には [WebGPU ビューアーと配信用データ](examples.md#webgpu-scenes) を用意する。
ネイティブの `renderer-scenes` を `vulkan,development-validation` 付きでビルドし、
`tools/renderer-scenes/reference.py` に配信時と同じ入力を指定する。次は両方の基準画像を
同じディレクトリに作り、Windows のハードウェア GPU で検証する例。

```powershell
foreach ($scene in "bistro", "sponza") {
    python -B tools/renderer-scenes/reference.py $scene --binary target/release/renderer-scenes.exe --manifest "$scene/cooked/manifest.json" --ibl-manifest "$scene/ibl/manifest.json" --baked-lighting "$scene/day.gi" --reflection-probes "$scene/day.reflections" --output build/webgpu-references
}
foreach ($channel in "chrome", "msedge") {
    python -B tools/renderer-scenes/verify_web.py build/renderer-scenes-web --scenes bistro sponza --references build/webgpu-references --channel $channel --output "build/webgpu-results/$channel"
}
```

各出力先の `result.json` に Canvas と読み戻し画像、Vulkan と SDR・HDR・深度の比較、矩形光の影、ブラウザとアダプター、表示間隔、プロセスツリーのピーク常駐メモリを記録する。Python の画像比較には NumPy、Pillow、psutil、Playwright を使う。

## 必要なツール

| 用途 | 必要なもの |
|---|---|
| 通常のCPUライブラリビルド | Git、rustup、Rust 1.99.0（edition 2024）、対象OSのリンカー |
| テスト・benchmark・全target確認 | 上記に加えてCコンパイラー。Criterionの依存allocaがC helperをビルドする |
| Vulkan featureのビルド | C++コンパイラー。vk-memが同梱VMAのC++をビルドする |
| 実Vulkan試験 | Vulkan 1.3対応ドライバー／ローダー、Validation Layer、debug_utils、layer_settings |
| M4のD3D12準備試験 | Windows x64、D3D12対応実GPU、Windows Graphics ToolsのDebug Layer／GBV、Vulkan比較環境 |
| 実行環境・シェーダーの調査 | Vulkan SDK 1.4.341.1のvulkaninfo、glslc、spirv-val |
| 依存・GPU内容の調査 | cargo-deny 0.20.2、RenderDoc 1.47。[ツールガイド](tooling.md)を参照 |
| WindowsのCPUサンプリング・待機分析 | Python 3.11以降、WPR/WPA exporter/xperf、行解決用LLVM、収集時の管理者権限。[CPUプロファイル手順](tooling.md#cpu-profiling)を参照 |
| 非管理者でのCPU zone調査 | optional `tracy` featureのビルド用C++コンパイラー、対応するTracy 0.14.1 portable tools、Python 3.11以降。[Tracy手順](tooling.md#tracy-profiling)を参照 |
| 課題管理 | Beads v1.3.4。[導入・同期手順](../../.beads/README.md)を参照 |
| Bistro素材・比較画像の共有 | Git LFS。[素材一覧とライセンス](../../assets/bistro/README.md)を参照 |
| Bistroのruntime cook | Python/NumPy/Pillow、Clang。固定MikkTSpaceのoffline helperをビルドする。Rustの通常ビルド・描画にはClang不要 |

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

<a id="bistro-preparation"></a>

## Bistro素材の取得・変換・検証

対象と描画条件は[実装方針の確定要件](../design/implementation-policy.md#bistro-requirements)に従う。
このoffline検証はPython 3.11、NumPy 1.26.4、Pillow 11.1.0、Node 24、Blender 4.5.16 LTSを使う。
公式原本ZIP、検証済み屋外glTF一式と環境HDRは`assets/bistro/`、比較PNGは`docs/verification/`でGit LFS管理する。
[素材manifest](../../assets/bistro/manifest.json)の412ファイルは合計1,775,343,744 byteである。
作業用の展開・portable tool・全量reportはGit管理外の`.tools/bistro/`へ置く。通常のRustビルドはこれらへ依存しない。
CIの`asset-tools`は小fixtureとLFSポインタを検査し、Bistro実体やBlenderをダウンロードしない。

Git LFSを導入済みのcloneで、次を一度実行する。共用hooksはBeadsの処理とLFSの送信処理を保持し、
pre-pushのref入力を両方へ渡す。`git lfs install --force`で上書きしない。

```powershell
git config --local core.hooksPath .beads/hooks
git lfs install --local --skip-repo
git lfs pull --include="assets/bistro/**,docs/verification/bistro-*-preview.png" --exclude=""
python -B tools/bistro/verify_lfs_assets.py
```

通常CIと同様に実体を取得しないclone/pullには`GIT_LFS_SKIP_SMUDGE=1`を設定する。
そのcheckoutでは`python -B tools/bistro/verify_lfs_assets.py --pointers`で全ポインタのOID/sizeを照合できる。
実体照合は`git lfs pull`後に行い、ポインタを実画像やZIPと取り違えない。
`bd hooks install --beads`による更新でも、Beadsマーカー外のLFS処理を保持する。

[NVIDIA公式ページ](https://developer.nvidia.com/orca/amazon-lumberyard-bistro)のDownload入口は
`https://developer.nvidia.com/bistro`である。redirect先の期限付きtokenは再現情報へ保存しない。
`Bistro_v5_2.zip`のSHA-256は`0d50e3c724c6c5da19f8eb99ad3f53e36fec37ffa2df9621f9ccf0603f3934e1`。
[Blender公式4.5配布](https://download.blender.org/release/Blender4.5/)の
`blender-4.5.16-windows-x64.zip`は`e258e90a1ebc14798ad65b3426bc5f11f5e0e013541c61cc7c3ee9bbf4ca0ce6`。
素材は共有済みの`assets/bistro/source/Bistro_v5_2.zip`を使える。公式から取得する場合も同じSHA-256を要求する。
素材とBlenderはSHA-256を照合してから、それぞれ`.tools/bistro/source/`と`.tools/bistro/tools/`へ初回展開する。
原本ZIPは`Bistro_v5_2/`を含むため、素材の初回展開は次で行う。既存の展開物を`-Force`で上書きしない。

```powershell
New-Item -ItemType Directory -Force .tools/bistro/source | Out-Null
Expand-Archive -LiteralPath assets/bistro/source/Bistro_v5_2.zip -DestinationPath .tools/bistro/source
```
入力の変更は別配布物として再監査し、既存入力を上書きしない。同梱licenseとクレジットを保持する。

次はルートからの実行例である。Python依存は専用venvへ導入してよい。

```powershell
python -m venv .tools/bistro/venv
$bistroPython = '.tools/bistro/venv/Scripts/python.exe'
& $bistroPython -m pip install -r tools/bistro/requirements.txt
npm.cmd ci --ignore-scripts --prefix tools/bistro-validator
$bistroSource = '.tools/bistro/source/Bistro_v5_2'
$bistroBlender = '.tools/bistro/tools/blender-4.5.16-windows-x64/blender.exe'
$bistroFbxModule = '.tools/bistro/tools/blender-4.5.16-windows-x64/4.5/scripts/addons_core/io_scene_fbx'
$bistroAudit = '.tools/bistro/logs/source-inventory.json'
$bistroTextures = '.tools/bistro/logs/textures.json'
$bistroPng = '.tools/bistro/prepared-textures'
$bistroOutput = '.tools/bistro/output/verified'
$env:BLENDER_USER_RESOURCES = Join-Path $PWD '.tools/bistro/blender-user'
& $bistroPython -B tools/bistro/source_inventory.py --source "$bistroSource/BistroExterior.fbx" --blender-fbx-module $bistroFbxModule --asset-root $bistroSource --output $bistroAudit
& $bistroPython -B tools/bistro/prepare_textures.py $bistroSource $bistroAudit $bistroPng $bistroTextures
& $bistroBlender --background --factory-startup --threads 4 --python-exit-code 1 --python tools/bistro/convert_blender.py -- $bistroSource $bistroAudit $bistroTextures $bistroPng $bistroOutput
& $bistroPython -B tools/bistro/compare_source_bindings.py --source "$bistroSource/BistroExterior.fbx" --audit $bistroAudit --baseline "$bistroOutput/baseline.json" --blender-fbx-module $bistroFbxModule --output .tools/bistro/logs/source-bindings.json
& $bistroPython -B tools/bistro/compare_geometry.py "$bistroOutput/baseline.json" "$bistroOutput/BistroExterior.gltf" --output .tools/bistro/logs/geometry.json
& $bistroPython -B tools/bistro/compare_materials.py "$bistroOutput/baseline.json" $bistroTextures $bistroPng "$bistroOutput/BistroExterior.gltf" --output .tools/bistro/logs/materials.json
node tools/bistro-validator/validate.mjs "$bistroOutput/BistroExterior.gltf" .tools/bistro/logs/validator.json
```

各コマンドの非ゼロ終了を確認して停止し、失敗reportを成功へ置き換えない。
source inventoryは固定Blenderの純粋なFBX parserを使い、Blenderアプリへの取込と独立して元配列・slot・参照を調べる。
変換は既知FBXのhashとauditのsource hash、固定Blender版を要求する。重複面の頂点分離後にMesh検査を行い、
元のraw CORNER normalを直接出力するための限定したfunction変更もhashへ残す。インストール済みtoolのファイルは書き換えない。
DDSのbase mipをPNGへ変換し、normalの正Z復元/緑反転を含む画素規則を照合する。元DDSの全mipをglTFへ収録したとは扱わない。
形状比較は全instance・world transform・三角形の数/向き/重複・材質・normal・UV0/UV1を照合し、
同位置の重複三角形も全属性の一対一対応を要求する。頂点数や配列順の一致を条件にしない。
source binding比較は元FBX配列からlocal位置・corner normal・UV2組・面ごとの材質を独立に照合する。
Blenderの63 byte超の名前短縮も固定版の規則で追跡し、Model/Geometryの取り違えを検出する。

別の出力先で変換コマンドをもう一度実行し、
`tools/bistro/compare_outputs.py <first.gltf> <second.gltf> --output <report.json>`で全参照fileのbyte一致を確認する。
共有済みの変換結果は`assets/bistro/converted/BistroExterior.gltf`であり、再生成物と同じコマンドで比較できる。
`render_validation.py <source.blendまたは出力.gltf> <同じviews.json> <画像directory>`を同じBlender引数で実行すると、
固定camera/lightのCycles CPU 8 sample・960×540画像を作る。`compare_previews.py`で比較画像と数値を保存する。
これは変換QAであり、最終1920×1080の描画品質・Vulkan/D3D12の受入画像や性能値ではない。
`summarize.py --help`の各report引数を指定し、[素材検証データ](../verification/bistro-assets.json)へ現在の証拠をまとめる。

検証profileのTANGENT省略による警告、未使用UVのinfo、確定した全ゼロalphaの扱いは
[検証結果](../verification/current.md#bistro-assets)を参照する。警告を黙って抑制せず、全量reportと失敗した変換条件も保持する。
検査の保証は取込・作成・更新境界へ置き、描画ごとのasset全量検査や診断専用payload保持を前提にしない。

### 2材質のalpha根拠と比較画像の再現

[確定方針](../design/implementation-policy.md#bistro-alpha)は元alpha保持・不透明overrideなしである。
`inspect_alpha.py`は固定原本ZIPと展開物・FBX audit・現行glTFの対応、元DXT5の全17 mipのalpha、
DDS base mipと既存PNGの全RGBA、manifestの全payloadを照合する。上記のsource inventoryを先に生成する。
prepared PNGがない場合は`--prepared`だけを省ける。原本DDSと現行PNGの照合は省かない。

```powershell
python -B tools/bistro/inspect_alpha.py --source .tools/bistro/source/Bistro_v5_2 --audit .tools/bistro/logs/source-inventory.json --gltf assets/bistro/converted/BistroExterior.gltf --output .tools/bistro-alpha/alpha-evidence.json
& $bistroBlender --background --factory-startup --threads 4 --python-exit-code 1 --python tools/bistro/render_alpha_comparison.py -- assets/bistro/converted/BistroExterior.gltf tools/bistro/alpha-views.json .tools/bistro-alpha/renders --samples 32
python -B tools/bistro/compare_alpha.py .tools/bistro-alpha/renders .tools/bistro-alpha/preview.png .tools/bistro-alpha/comparison.json
```

追跡済みviewsは窓と瓶の近景で、瓶には確認しやすい固定area fill lightを追加する。
各視点のcamera/lightはalpha保持・対象だけ強制不透明・対象coverageの3条件で共通である。
全sceneのmeshを保持し、対象材質のcopyだけを変更して終了時に元slotへ戻す。
coverageは不透明copyのmaterial index passを用い、他の幾何による遮蔽も残す。
比較ツールはcoverageと交差する画素差がない視点を失敗にする。有限sampleの間接光・noiseも差分へ含まれるため、
差分値をruntimeの合否閾値や性能値へ転用しない。設定・入力/resource/script hashとfull-size画像のhashはreportへ残す。

[共有検証データ](../verification/bistro-alpha.json)は原本値、commit固定のFalcor参照path/行/hash、
CPU比較と採用理由をまとめる。Falcorコードの実行結果とは扱わない。
`summarize.py`で素材reportを再作成するときは、他のreportに加えて
`--alpha-report docs/verification/bistro-alpha.json`を渡す。元FBX hashが異なる判断は拒否する。
材質・texture・recipeの変更時はこの調査を再実行し、基準画像を更新する理由を記録する。

<a id="bistro-conditions"></a>

### Bistroの固定視点・照明・比較手順

[conditions.json](../../tools/bistro/conditions.json)が固定視点7件、30秒の巡回、sun/IBL/露出と再現入力である。
[設計上の理由と完了の境界](../design/implementation-policy.md#bistro-conditions)、
[CPUプレビュー](../verification/bistro-conditions-preview.png)も参照する。
入力検査は準備境界で一度行い、変更時に再実行する。Rendererのdraw/submitへscanや診断payloadを加えない。

```powershell
python -B tools/bistro/conditions.py tools/bistro/conditions.json --verify-inputs --frames-output .tools/bistro-conditions/route-frames.json
& $bistroBlender --background --factory-startup --threads 4 --python-exit-code 1 --python tools/bistro/render_conditions.py -- assets/bistro/converted/BistroExterior.gltf tools/bistro/conditions.json .tools/bistro-conditions/renders --asset-root .
python -B tools/bistro/compare_conditions.py .tools/bistro-conditions/renders docs/verification/bistro-conditions-preview.png .tools/bistro-conditions/comparison.json --config tools/bistro/conditions.json
```

最初のコマンドはZIP/glTF/bin/HDRのsize/SHAとcamera/光/経路の有限値・範囲・参照・補間時の視線を検証する。
全textureを含むLFS実体の照合は先の`verify_lfs_assets.py`を使う。
フレーム列はframe `i`の論理時刻を`i/60`とする0〜1799で、終端30秒は開始poseと重複するため出力しない。
実測の壁時計やマウス入力で経路を進めず、同じframe番号には同じposition/target/FOVを渡す。
自由カメラ・衝突判定とは別の仕組みで、閉路の折返しは線形補間である。

Blender 4.5.16 LTS/固定buildで全mesh/materialを保持し、Cycles CPU・960×540・32 sample・seed 0、
adaptive samplingなしで構図・露出を確認する。CPU OpenImageDenoise（accurate/HDR/normal・albedo guide）の
表示用32-bit scene-linear EXRと、別のnoise除去前EXR、固定Khronos PBR Neutral→sRGBのPNGを出力する。
入力/設定/tool/image hashと実効設定も保存する。noise除去は本番のAAを追加した意味ではない。
hash対象の設定JSONは`.gitattributes`の`-text`で実行時のbyteを保持し、新規condition toolと検証JSONはLFへ固定する。
OSの改行自動変換で実行記録とcheckoutのhashが変わらないよう、index内のblobも照合する。
本番の1920×1080、GGX/IBL前処理/CSM、AA/BloomやGPUの正しさ・性能をこの結果で受け入れない。
HDRは原本byteを維持し、Rec.709輝度4のcapを元texelへ適用して高輝度源を抑える。
太陽の物理的な完全分離や、撮影時の照度復元とは扱わない。

二APIの画像比較はB2/B3/B5で次の順に進める。

1. `conditions.json`と全入力・shader/compiler・IBL cook出力・Graph/画質設定、commit/build/driverをhash付きで保存する。
   投影のray・Y方向・depth・front faceは単純fixtureでも確認する。
2. 各APIを同じ機材で3回以上反復し、同APIの揺れと二API差を分ける。
   全7視点と巡回frame 0/300/600/900/1200/1500/1799のSDR、Bloom前HDR、depth、shadow、Bloom bypassを保存する。
3. 材質欠落、sRGB二重変換、alpha不透明化、shadowずれの既知の誤りを入れた補助出力で感度を確認する。
   RGB/HDRの絶対・相対誤差、誤差画素数、名前付きROIの閾値を決め、受入runより前に設定hashを固定する。
   輪郭を一括除外せず、解像度違い・NaN/Infは失敗とする。CPUプレビューのnoiseから数値閾値を借りない。
4. 最適化＋厳格検証と、最適化＋Renderer追加検証/専用診断のコンパイル除外＋native検証無効を別runで照合する。
   alpha 0の2材質はcolor/depth/shadow被覆0を確認する。表示・resize/最小化復帰・pending終了は別に確認する。

性能予算はRTX 3060での実測から設定する。cold読込/変換・upload・shader/pipeline準備を定常描画と分け、
2巡回（各1800 frame）をwarm-up、続く3巡回を計測する。30秒は各巡回の論理時刻であり、
実行を30秒へ引き伸ばしたり打ち切ったりしない。offscreenはVSyncなしでCPU build/compile/record/submit/retire、
GPU timestamp、frame壁時計、GPU常駐量・使用量/予算・high-waterを生sampleとrun別p50/p95で保存する。
APIは逐次実行し、温度/clock・背景負荷・電源設定と変動を記録する。windowedのpresent/VSync待ちは別測定とする。
固定FPSやVRAM予算、二APIの性能優位は現時点で主張しない。

今後の実GPU受入では、両APIを同時に負荷実行せず、同じ機材で逐次確認する。
offscreenの最終SDR・Bloom前HDR・必要な中間出力と差分を保存し、画面表示・自由操作・resize・最小化復帰・
pending終了も別に確認する。最適化＋Renderer追加検証あり＋Vulkan Core/SyncValまたはD3D12 Debug Layer/適用するGBVの厳格受入と、
Renderer追加検証・専用診断をコンパイル除外しnative検証を無効にした最適化経路の内容照合を別runで行う。
性能測定も後者の構成で行い、厳格受入の計装付き時間と分ける。
[GBVはshader・commandを計装する](https://learn.microsoft.com/en-us/windows/win32/direct3d12/using-d3d12-debug-layer-gpu-based-validation)ため、
その実行時間を通常性能として扱わない。既存の警告/エラーゼロと破棄後までの確認を維持する。
画像閾値は既知の誤りを検出できる条件から受入前に固定し、性能予算は同条件の実測から決める。
記録先は[現在の検証状況](../verification/current.md)、実GPUの未完了追跡はB2/B3/B4/B5の各実装課題を使う。

<a id="bistro-pbr"></a>

## Bistroのruntime cook・Forward PBR・計測

`sashiko-7ft.3.1`／`.3.2`は、利用側で素材をcookして読み込み、同じ公開Graphとtrusted Rasterを
Vulkan/D3D12へ接続する。Renderer本体へModelの読込やPBR固有のeffect列挙を追加しない。
[形式・保証・対応範囲](../design/implementation-policy.md#bistro-runtime-cook)、
[実行済みの検証と計測](../verification/current.md#bistro-pbr)も参照する。

### 実行用データのcook

前節のLFS素材を取得し、Python依存を導入してから実行する。
`mikk.py`はvendored MikkTSpace commit `3e895b49d05ea07e4c2133156cfa94369e19e409`と小さなC bridgeを
Clangで共有ライブラリへビルドし、source/libraryのhashとcompiler版・flagsを`build.json`へ残す。
WindowsでClangがPATHにない場合は`--compiler`で実行ファイルを明示できる。

```powershell
python -m pip install -r tools/bistro/requirements.txt
python -B tools/bistro/mikk.py .tools/bistro-pbr/mikk
python -B tools/bistro/cook.py assets/bistro/converted/BistroExterior.gltf .tools/bistro-pbr/cooked --mikk-library .tools/bistro-pbr/mikk/bistro_mikk.dll --source-zip assets/bistro/source/Bistro_v5_2.zip
```

Linuxのhelper名は`libbistro_mikk.so`である。出力先は新規または空directoryを指定する。
失敗したcookの部分出力を成功として再利用せず、原本・検証済みglTF/PNGを上書きしない。
生成物は`manifest.json`、`vertices.bin`、`indices.bin`と画像ごとのpacked mip streamで、`.tools/`に保持する。
manifestはsource、変換recipe、Mikk source、全mesh/node/primitive/material/texture対応、transform、
各streamの長さ/hashと接線fallback等の件数を記録する。

Bistro の既定設定は元 DDS の全 405 画像・3,529 mip を BC1/BC3/BC5 のまま GPU に送る。
cook 時に base mip を既存 PNG と照合する。Sponza のように元画像が PNG/JPEG の場合、
色は線形空間、法線は方向を正規化して mip を生成し、色と ORM に BC7、法線に BC5 を使う。
色は sRGB ビュー、ORM と法線は線形ビューでサンプルする。CPU ベイカー用には、最長辺が
128 texel 以下になる最初の mip を圧縮結果から復号して画像ごとに保存する。

`cook.py --texture-policy settings.json` で形式を指定できる。例えば次の設定では ORM を BC7 にし、
指定した画像を RGBA8 にできる。`images` のキーは glTF 内の画像 URI と完全に一致させる。

```json
{"default":"auto","roles":{"orm":"bc7"},"images":{"textures/detail.png":"rgba8"}}
```

優先順は画像 URI、用途、全体設定の順。用途は `base_color`、`emissive`、`normal`、`orm`、
形式は `auto`、`rgba8`、`bc1`、`bc3`、`bc5`、`bc7`。BC5 は法線専用で、BC1 の新規エンコードは
不透明画像に限る。全画像を非圧縮で比較するには `{"default":"rgba8"}` を使う。
出力 manifest の各画像に形式・ミップ範囲・SHA-256、各 base color mip に alpha の最小値と最大値が入る。
runner report の `material_texture_bytes` は全 mip の GPU 転送用ペイロードの合計で、既定テクスチャ、
CPU ベイク用画像、ネイティブアロケーションのパディングを含まない。D3D12 で最上位 mip の幅または高さが
4 の倍数でない BC 画像を使うには `UnalignedBlockTexturesSupported` が必要になる。該当機能のない対象では
その画像のインポート形式に `rgba8` を指定する。

接線は正しいnormalとUVからMikkTSpaceで生成する。退化UV/geometryとfloat32角度重みのcollapseを
別に数え、接線が得られないcornerでは有効なhandednessを保って決定的な直交fallbackを使う。
world transformを反映したposition/normal/tangent、handednessと反射時のwindingを照合する。
2つのalpha0材質も元値・画像・geometryを保持する。不透明化をcookへ埋め込まない。
同じコマンドを別directoryへ実行し、manifestと全streamのhashを比較して再生成の一致を確認する。
小fixtureの契約回帰は`python -B -m unittest discover -s tools/bistro -p 'test_*.py'`で実行する。

<a id="bistro-b3"></a>

### B3の環境照明と影・表示変換

全Bistroには、geometry/material cookに加えて静的IBLのmanifestを明示する。
固定Blender4.5.16/build `3f6797307da5`はHDR復号にだけ使い、NumPyで前処理する。
元HDRの輝度cap4を適用してから、diffuse/specular/BRDFを線形RGBA16Fへ生成する。
強度3とyaw0は実行時に一度だけ適用する。元HDRと既存素材を上書きしない。

```powershell
python -B tools/bistro/ibl.py --source assets/bistro/environment/san_giuseppe_bridge_4k.hdr --blender .tools/bistro/tools/blender-4.5.16-windows-x64/blender.exe --output .tools/bistro-pbr/ibl
python -B tools/bistro/ibl.py --source assets/bistro/environment/san_giuseppe_bridge_4k.hdr --blender .tools/bistro/tools/blender-4.5.16-windows-x64/blender.exe --output .tools/bistro-pbr/ibl-repeat --chunk 37
```

両出力のmanifest、raw3画像、Godot用HDRのhashを比較する。最大prefilter LOD4で粗さ1の方向依存を保持し、
全11 mipを初期化する。BRDF/環境の独立数値pilotは`tools/bistro/ibl_pilot.py --help`を参照する。
Godot用HDRは同じcap済み元画像をU方向へ四分の一周ずらし、両実装の投影方向を揃える。
shaderの再生成は[`crates/sashiko-renderer/shaders/regenerate.ps1`](../../crates/sashiko-renderer/shaders/regenerate.ps1)の
`-Verify`で確認し、配布時には同directoryの`licenses/`通知も保持する。

### 同じ描画例を二APIで実行する

`renderer-scenes`の`renderer-scenes`は公開の[標準 PBRレシピ](tooling.md#pbr-forward-recipe)で
Forward／Forward+の1×／4×とDeferredの1×を描く。Bloomも同じGraphに接続する。
以下のBistro実行・計測・受入はこの例を使う。公開Graph APIを直接使うRust版は[Examplesガイド](examples.md#最初に動かす)を参照する。
小fixtureはLFS素材やcook生成物を要求しない。Vulkan実行にはVulkan 1.3 runtime、D3D12にはWindowsの実GPUを使う。
native検証を要求する受入では`development-validation`を付け、対応するLayer／Graphics Toolsを用意する。
通常releaseの測定では追加検証・専用診断を含めず、別runで内容を照合する。

```powershell
cargo run --locked --release -p renderer-scenes --no-default-features --features vulkan,d3d12,development-validation --bin renderer-scenes -- --api vulkan --fixture --frames 8 --warmup 2 --output .tools/bistro-pbr/fixture-vulkan-strict
cargo run --locked --release -p renderer-scenes --no-default-features --features vulkan,d3d12,development-validation --bin renderer-scenes -- --api d3d12 --fixture --frames 8 --warmup 2 --output .tools/bistro-pbr/fixture-d3d12-strict
cargo run --locked --release -p renderer-scenes --no-default-features --features vulkan,d3d12 --bin renderer-scenes -- --api vulkan --manifest .tools/bistro-pbr/cooked/manifest.json --ibl-manifest .tools/bistro-pbr/ibl/manifest.json --view street --width 1920 --height 1080 --frames 120 --warmup 24 --inflight 2 --output .tools/bistro-pbr/street-vulkan
cargo run --locked --release -p renderer-scenes --no-default-features --features vulkan,d3d12 --bin renderer-scenes -- --api d3d12 --manifest .tools/bistro-pbr/cooked/manifest.json --ibl-manifest .tools/bistro-pbr/ibl/manifest.json --view street --width 1920 --height 1080 --frames 120 --warmup 24 --inflight 2 --output .tools/bistro-pbr/street-d3d12
```

`--manifest`と`--fixture`は入力の選択である。`--view`には`conditions.json`の固定視点名、または`route`を使う。
`route`は1,800 frame周期でcameraを補間し、測定窓はwarm-upと独立にroute frame0から始まる。
`--frames`は測定frame数、`--warmup`はその前のframe数、`--inflight`は1〜4の同時投入上限であり、
実効設定をreportへ残す。`--gpu-timing`は既定offのnative詳細CPU timerとGPU timestampを一緒に有効にする。
`--alpha-only`は元alpha0材質のgeometryだけを診断し、`--alpha-override opaque`は
そのcolor/depth被覆が非ゼロになることを確かめる感度確認に使う。
現在の描画例はB3のIBL・影・表示変換を含む。上記の短いrunはB5最終巡回受入を代替しない。

各runは`report.json`と、測定後に別途描いた診断frameのHDR／SDR／depthを保存する。
診断は指定したviewを使い、`route`ではframe0を描く。最終測定frameそのもののreadbackではない。
HDRはlittle-endian RGBA16F、SDRはRGBA8、depthはlittle-endian FP32で、
寸法・format・pathはreportの`captures`にある。capture/readbackは測定窓の外に置く。
`--capture-all-views`は同じ常駐assetを使い、追加で全7固定視点を`views/<view>/report.json`へ出力する。
これらは`capture_only: true`／`measurement: null`の診断であり、巡回全frameのcaptureではない。
この例はoffscreenの材質描画であり、Windows表示・VSync待ちや操作の受入とは区別する。
B3はGGXのsun＋静的IBL、4分割の影、線形RGBA16Fのblend、EV+1、Neutral、FXAA、sRGB一回を使う。
`--aa-off`は出力AAを無効にし、`--sun-shadows-off`は太陽の影だけを無効にする。矩形ライトの直接光と影は `--rect-lights off|unshadowed|shadowed` で指定する。
通常のAAありrunも測定後に`aa-off/`へ診断画像を保存する。
`--capture-shadow-maps`は測定後の4枚のD32画像を`shadow_captures`へ保存し、readback容量も準備時に予約する。
full-sceneのAO適用は固定照明profileに従って無効とし、元ORM・glTF・材質参照を保持する。
汎用shaderのAO契約は独立fixtureで検証する。[採用理由](../design/implementation-policy.md#b3-影線形hdrトーンマッピング)を参照する。
`--bloom off|tent|box`は露出前のBloomを選ぶ。既定は`off`。
有効時は`hdr.bin`に加えて、合成後・露出前の`bloom_hdr.bin`を保存する。
`--capture-route-samples`は固定の巡回frame 0/300/600/900/1200/1500/1799の画像を別に取得する。
表示・自由操作の最終統合受入はB5で行う。

<a id="bistro-bloom-fixture"></a>

### Bloomの小HDR入力受入

公開のBloomレシピをGraphへ組み込み、無効・テント・ボックスを比較する。
Windowsの同一物理GPUで最適化済みのstrictとshippingを別processとして実行する。

```powershell
cargo test --locked --release -p sashiko-renderer --test bloom_graph --no-default-features --features vulkan,d3d12,development-validation,graph-diagnostics native::consumer_bloom_gpu -- --exact --ignored --nocapture --test-threads=1
cargo test --locked --release -p sashiko-renderer --test bloom_graph --no-default-features --features vulkan,d3d12 native::consumer_bloom_gpu -- --exact --ignored --nocapture --test-threads=1
```

全Bistroでは、`memory-profiling`付きのstrictとshippingを別にビルドする。
各API・各Bloom設定について、同じ実行用cook／IBLを指定し、以下の条件で逐次実行する。
出力名は`<strict|disabled>-<vulkan|d3d12>-<tent|off|box>`とし、`tent`では`--resource-checks`も指定する。

```powershell
& $binary --api $api --manifest $manifest --ibl-manifest $iblManifest --bloom $effect --view route --width 1920 --height 1080 --frames 24 --warmup 8 --inflight 2 --capture-all-views --capture-route-samples --output $runDirectory
python -B tools/bistro/bloom_acceptance.py $cohortDirectory
```

<a id="bistro-b5"></a>

### 全Bistroの表示と統合受入

シーンのHDRにアプリ側のBloomをつなぎ、`sashiko_renderer::passes::output`のNeutralとSDR変換を適用する。
WindowsのPresenterには、swapchainと同寸法のlinear RGBA8画像をexportして渡す。
オフスクリーンでは`SdrOutput::capture`を選んでsRGBのバイト列を読み戻す。ウィンドウでは`SdrOutput::display`で線形出力を準備し、
投入受理済みの表示画像をPresenterへ渡す。sRGBへの変換はPresenterの描画先が行う。

```powershell
cargo run --locked --release -p renderer-scenes --bin renderer-scenes --no-default-features --features vulkan,d3d12,presentation -- --api vulkan --manifest $manifest --ibl-manifest $iblManifest --bloom tent --windowed --output .tools/bistro-window
```

`--api d3d12`でも同じ入力とGraphを使う。WASDで移動、Q/Eで上下、Shiftで加速、右ボタンを押してマウスで視点を動かす。
Tで巡回を切り替え、Escで終了する。`--view route`は固定1,800フレームの巡回から始める。
`--frames`を指定するとwarmupと測定を終えて自動終了する。`--vsync on|off`は表示要求を選ぶ。
resizeではGraphと表示の利用者が返却された時点で、登録済みassetを保持したままextent依存の画像を準備し直す。

`--msaa 4`はForwardのHDRカラーと深度を4サンプルで描き、半透明を含む最後の描画でカラーを
1サンプルにresolveしてBloomに渡す。既定の`--msaa 1`ではB5と同じ描画経路を使う。
FXAAは独立に`--aa-off`で無効化できる。4×のoffscreen captureはresolve済みHDR/SDRと指定した
1×の影マップを出力する。4×のカメラ深度はreadback対象外で、`depth_coverage_pixels`は`null`となる。

`--pipeline deferred --msaa 1`では不透明・MASKを3枚のRGBA16FとD32へ描画し、
深度と逆ビュー投影行列から位置を復元して共通のPBR照明を評価する。半透明はそのHDRと深度を読み込み、
奥から手前にForwardで合成する。Deferredにも同じBloom、露出、Neutral、FXAA、表示が続く。
オフスクリーンとWindow reportの`pipeline`で経路を確認できる。

`--pipeline forward-plus`と`--pipeline forward-reference`は同じ64灯と材質・カメラを使う。
Forward+は16×16ピクセルのタイルごとに交差候補をComputeで求め、不透明・半透明の両方が一覧を読む。
全灯参照は同じ局所光の計算を各フラグメントで全灯に対して実行する。両経路は`--msaa 1`と`--msaa 4`で比較できる。
Forward+はフレーム内の一時タイルマスクを全域書き込み、Windowのresizeでは他のフレーム用画像と同様に新しいextentで準備する。
標準レシピは点光源が0件のとき通常Forwardを選ぶ。

```powershell
cargo test --locked --release -p sashiko-renderer --test pbr_tile_masks --no-default-features --features vulkan,d3d12,development-validation -- --ignored --nocapture
python -B tools/bistro/compare_runtime.py $tiledReport $allLightReport --profile repeat --planes hdr sdr --forward-plus-reference --output $comparison
```

実GPU試験は固定の投影でタイルごとの一覧をCPU参照と照合する。製品構成には`development-validation`を外す。
画像照合で`--forward-plus-reference`を使うと、局所光プロファイル、視点、サンプル数等の一致を要求した上で両描画経路を比較できる。

`--instances individual` と `--instances instanced` は、Forwardの同じBistro小物を24か所に同じ条件で配置して、
24回の直接描画と1回のインスタンス描画を比較する。既定値は `off` で、比較時も `--msaa 1|4` を使える。
`renderer-scenes`の`renderer-scenes`では、同じモデル変換をシーンの描画と4カスケードのシャドウに使う。
`--submission packed` と `--submission indirect` は、同じ共有頂点・インデックスと同じ不透明／MASKの材質グループを使う。
`packed` ではCPU、`indirect` ではGPUが可視性を判定する。間接経路でも半透明はCPUが奥から手前に並べる。
オフスクリーンの `prepared.packed_opaque` とWindowの `packed_opaque` は、準備したグループ数とプリミティブ候補数を示す。
オフスクリーンの `draw_count` はカメラパスに発行した直接描画数で、GPUで可視数が決まる間接経路では `null` になる。
インスタンス比較はForwardの `--submission direct` で実行する。

```powershell
cargo test --locked --release -p sashiko-renderer-core --test raster_batches --test indexed_instancing --test indexed_indirect --no-default-features --features vulkan,d3d12,development-validation -- --ignored --nocapture --test-threads=1
python -B tools/bistro/b5_benchmark.py --scenario window-tearing --binary $ordinaryRelease --manifest $manifest --ibl-manifest $iblManifest --submission indirect --output $performanceDirectory
```

`--scenario window-tearing` は各APIのVSync off実ウィンドウを3回ずつ計測する。サンプル数、パイプライン、
描画の方法を変える場合は、出力先を分けて各比較で同じカメラ巡回と画質条件を使う。

```powershell
cargo run --locked --release -p sashiko-renderer-core --example msaa --no-default-features --features vulkan,d3d12
cargo test --locked --release -p sashiko-renderer-core --test msaa --no-default-features --features vulkan,d3d12,development-validation -- --ignored --nocapture
```

専用例と実GPU試験では、同じGPU上の両APIで斜めの輪郭の1×/4×画像、奥の三角形の遮蔽、
drawを含まないclear/resolveを照合する。製品構成の試験には`development-validation`を外す。

画像受入では、両APIのreleaseを`memory-profiling`付きでビルドし、別の出力先にstrictとshippingを保存する。
strictに`development-validation,graph-diagnostics`を加える。下記の`pilot`で比較基準と故障検出を確認し、
同じEXE・入力・閾値を使う`accept`で、両API・両構成・3反復を逐次実行する。
`--capture-bloom-bypass`で同じ視点の無効時SDRを別に取り、比較のROI、差分画像、閾値hashを保存する。

```powershell
python -B tools/bistro/b5_images.py pilot --strict $strict --shipping $shipping --manifest $manifest --ibl-manifest $iblManifest --profile $profile --output $pilotDirectory
python -B tools/bistro/b5_images.py accept --strict $strict --shipping $shipping --manifest $manifest --ibl-manifest $iblManifest --profile $profile --output $imageDirectory
```

中断した画像受入は同じコマンドに`--resume`を足して再開する。進捗にはEXE・cook・IBL・校正の
SHA-256と完了した試行のreportを記録する。完了済みのreportとcaptureを再検査し、途中で止まった試行は
別ディレクトリでやり直す。入力や実行物を差し替えた場合は新しい出力先を指定する。
表示受入には新しい`accept`の出力を渡す。reportに明記されたsample数・pipeline・描画方法と、
実際に読み込んだ素材および組み込まれた描画条件・shaderの識別をオフスクリーンと照合する。

同じrelease構成に`presentation`を追加し、各API・構成で`--window-smoke --bloom tent --frames 24`を実行する。
1080pの自由カメラ入力をアプリ内で再生し、実際のウィンドウをresize・最小化・復帰した後、pendingから終了する。
続いて`--windowed --view route --bloom tent --warmup 24 --frames 1800 --vsync off`で巡回と定常メモリを確認する。
メモリの固定計測からresize・設定変更・画像ファイル書出しは外し、別の受入で確認する。

```powershell
python -B tools/bistro/b5_window_acceptance.py $windowSmokeDirectory $imageDirectory --route-directory $windowRouteDirectory
python -B tools/bistro/b5_benchmark.py --binary $ordinaryRelease --manifest $manifest --ibl-manifest $iblManifest --output $performanceDirectory
```

4×の画像・性能受入では`b5_images.py`の両stageと`b5_benchmark.py`に`--msaa 4`を渡し、
profileと出力先を分ける。`b5_window_acceptance.py`はoffscreen参照とWindow reportのサンプル数を照合する。
Deferredの画像・性能スクリプトには`--pipeline deferred`を渡し、Forwardと別のprofile・出力先を使う。
`--pipeline forward-plus`と`--pipeline forward-reference`もそれぞれ別のprofile・出力先を使う。
`b5_window_acceptance.py`は同じ描画経路のoffscreen参照を要求する。

性能には`vulkan,d3d12,presentation`のみのreleaseを使う。hostのCPU policyを検証してから逐次起動し、
初回の読込／準備／upload、定常のCPU phase／GPU時間／frame間隔、VSync on/offのpresentを記録する。
オフスクリーンは3,600 warmup＋5,400測定frame、別枠のウィンドウ計測は120 warmup＋600測定frameとする。
各API・条件を独立3 processで実行する。
表示側の計時経路だけを修正した前後の記録には、`--window-binary`でウィンドウ測定用EXEを明示できる。
性能計測を`--resume`で再開する場合も、開始時と同じEXE・入力・条件を使う。
実ウィンドウの時刻はアプリとnative APIの観測であり、ディスプレイ走査時刻はこの方法で測定しない。

### 画像比較と性能値の読み方

同API反復と二API差は別に比較する。比較profileとそのhashを受入前に固定し、
NaN/Inf・寸法違い・欠損出力を拒否する。輪郭を一括除外せず全画素を数え、
全量の数値比較に加えて材質fixtureと実alpha0のcolor/depth被覆、強制不透明化への感度を確認する。
比較profile、実行コマンドと結果は[現行検証](../verification/current.md#bistro-pbr)へ記録する。

```powershell
python -B tools/bistro/compare_runtime.py .tools/bistro-pbr/fixture-vulkan-strict/report.json .tools/bistro-pbr/fixture-d3d12-strict/report.json --profile fixture --output .tools/bistro-pbr/fixture-comparison.json
python -B tools/bistro/compare_runtime.py .tools/bistro-pbr/street-vulkan/report.json .tools/bistro-pbr/street-d3d12/report.json --profile scene --output .tools/bistro-pbr/street-comparison.json
```

`repeat`は同API反復のbit一致、`fixture`は小材質、`scene`は全景用の数値許容差を使う。
名前付き領域の追加検査には`--regions`、既知の誤りの検出には`--expect-different`を使う。
診断で意図して変えるidentityは`--identity-changes`へ変更前後の値を明示する。
全体の誤差画素率だけで、小さな材質やalphaの欠落を受け入れない。
材質の実GPU回帰は17 panelのfixtureを使う。normal、回転したtangentとhandedness、metallic/roughness、
emissive、front/back face、alphaのcolor/depthを、shaderと独立したf64の期待値と照合する。
理想鏡面に近いpeak panelは丸めへの感度が高いため、通常panelの解析誤差と分けてRGBA16Fの有限性・
SDR resolveとの整合を確認する。2×2画像のUV probeも使い、異なるtexelをsampleすることで、
正しいbindingを使っていてもUVが固定・反転している誤りを検出する。
実GPU試験は`#[ignore]`なので、CPU試験の成功から実行済みとは扱わず、両APIのstrict/shippingで明示実行する。
alpha0のHDR/SDR/depthは、同じAPI/GPUで独立に実行したPBR drawなしのclear/resolve Sceneと全byteで照合する。
RGBA16F clearの許されるformat変換とalpha被覆を混同せず、CPUの最近接丸めだけを厳密な背景値とはしない。
対照の変換範囲も別途検査し、通常の画像比較profileは維持する。

通常性能はETW収集なしのreleaseで、同じEXE・入力・view・解像度・in-flight数を用い、
各APIを交互の順序で独立3 process以上測る。測定中にcook・build・他のGPU負荷を並行させない。
読込/hash照合、upload、Program/pipelineと容量の準備は初期化費用として定常描画から分ける。
`setup_phases_ns`は次の準備区間を別々に記録する。

| 区間 | 範囲 |
|---|---|
| `asset_load` | 固定条件・manifestの読込、assetの境界検証 |
| `budget` | 必要な資源classと容量の算出 |
| `native_prepare` | native資源・Program/pipelineの準備 |
| `upload` | 画像の逐次読込/hash照合、copy・完了待ち、CPU geometry入力の解放 |
| `frame_storage_and_profiling_prepare` | frame領域とoptional profilingの準備 |
| `warmup` | 測定前frameの構築・投入・完了・返却 |

`setup_ns`はこれらとreport用metadata/bookkeepingを含む準備全体の経過時間である。
個別checkpointは全処理を完全には分割しないため、内訳の合計と`setup_ns`の一致は要求しない。
定常窓にはGraph構築/compile、prepare/record/submit、poll/retireと結果返却を含める。
process/main threadのOS charged CPU、各phaseのwall elapsed、GPU timestampとframe時間は別の量である。
複数in-flightの測定窓wall/frameはthroughputの費用であり、一frameの投入から完了までのlatencyではない。
p50/p95は範囲が明示されたframe sampleから算出し、run平均の分布と混同しない。
`--gpu-timing`のoff/on差はnative詳細CPU timerとGPU timestampを合わせたprofiling費用の観測であり、
GPU query単独のoverheadとはしない。トレースや厳格native検証の計装付き時間も通常性能へ混ぜない。
off時の`native_phase_totals_ns`は未収集を表す`null`であり、処理が無費用だったという意味ではない。
headless転送負荷で得たAPI差を、PBRのCPU/GPU費用や画面表示FPSへ換算しない。

Windowsでは既存の[build archive](tooling.md#cpu-profiling)でcleanなreleaseのEXE/PDB/source identityを保存し、
計測driverへ渡せる。buildは測定開始前に完了させ、出力先は新規directoryとする。
BistroのRustレシピと`tools/bistro/conditions.json`もsource hashへ含め、cook入力のidentityはrunner reportで別に記録する。

```powershell
python -B tools/cpu_profile/build.py --package renderer-scenes --bin renderer-scenes --features vulkan,d3d12 --profile release --output .tools/bistro-pbr/release-build
python -B tools/bistro/measure_runtime.py plan --build-manifest .tools/bistro-pbr/release-build/manifest.json --manifest .tools/bistro-pbr/cooked/manifest.json --ibl-manifest .tools/bistro-pbr/ibl/manifest.json --frames 1800 --warmup 3600 --inflight 2 --timeout-seconds 600 --output .tools/bistro-pbr/measurements
python -B tools/bistro/measure_runtime.py run --build-manifest .tools/bistro-pbr/release-build/manifest.json --manifest .tools/bistro-pbr/cooked/manifest.json --ibl-manifest .tools/bistro-pbr/ibl/manifest.json --frames 1800 --warmup 3600 --inflight 2 --timeout-seconds 600 --output .tools/bistro-pbr/measurements
```

既定のrouteは1,800 frameで一周し、`--warmup 3600`の二周後に一周を測る。独立3 processの各回で二周をwarmupする。
短いwarmupは動作確認用に明示指定できるが、固定性能条件の結果とは分けて記録する。
driverはAPIとnative profiling off/onを交互に独立3回ずつ実行し、各reportと内容一致、実効設定、
計測窓、終了結果を確認する。全process時間にはロード・capture・終了も含まれるため、
測定窓のRenderer CPU/frame費用へ加算しない。raw画像はローカルに残し、共有する集計へ画像本体を埋め込まない。
内容照合はROI後に別途描くroute frame 0のHDR／SDR／depthを対象とし、全測定frameの画像照合を意味しない。

<a id="bistro-residency"></a>

### Bistroの常駐・差し替え・メモリ受入

`bistro --resource-checks`は通常frame窓の外で資源の変更を検査する。
`memory-profiling`と2以上のin-flight枠を要求し、準備・upload・warmup・通常frame後・検査後の
native必要量、論理byte、staging容量、プロセス使用量・budgetを`report.json`へ保存する。
メモリ観測のscanとDXGI照会、診断画像のreadbackを通常frame時間へ混ぜない。
差し替え用の予備classも含む検証用予算であり、最小の通常描画予算とは区別する。
[設計上の境界](../design/implementation-policy.md#bistro-residency)も参照する。

```powershell
cargo build --locked --release -p renderer-scenes --no-default-features --features vulkan,d3d12,memory-profiling,development-validation --bin renderer-scenes --target-dir .tools/bistro-residency/strict-build
cargo build --locked --release -p renderer-scenes --no-default-features --features vulkan,d3d12,memory-profiling --bin renderer-scenes --target-dir .tools/bistro-residency/shipping-build
python -B tools/bistro/resource_acceptance.py --strict-binary .tools/bistro-residency/strict-build/release/renderer-scenes.exe --shipping-binary .tools/bistro-residency/shipping-build/release/renderer-scenes.exe --source-commit (git rev-parse HEAD) --output .tools/bistro-residency/fixture
python -B tools/bistro/resource_acceptance.py --strict-binary .tools/bistro-residency/strict-build/release/renderer-scenes.exe --shipping-binary .tools/bistro-residency/shipping-build/release/renderer-scenes.exe --source-commit (git rev-parse HEAD) --manifest .tools/bistro-pbr/cooked/manifest.json --ibl-manifest .tools/bistro-pbr/ibl/manifest.json --output .tools/bistro-residency/full
```

各batchは二API×strict／Disabled×独立2 processを直列で実行する。
fullは1080p、全素材、通常窓24 frame＋warmup 8、7固定視点の既存profileによる同API反復・二API照合を行う。
同時Viewとpending差し替えは別の診断内で検査する。反復更新後のnative件数・byteと論理所有量の一致を確認し、
OSの動的usage/budgetの増減をそれ自体でリークと判定しない。
プロセスの失敗・timeout、計測欠落・不一致は失敗にし、途中のログとreportを出力先へ残す。
出力先は新しいdirectoryを指定し、過去の結果を再利用しない。実行中は別のGPU試験を止める。
生成したEXEを別buildで上書きせず、source commit・EXE hash・cook hash・全reportと比較結果を保持する。
この計装構成の時間を通常releaseの性能結果として扱わない。

native resource要件の合計はdriver内部の管理費や物理page常駐の測定ではない。
Vulkanのmemory budget extension非対応やD3D12のIDXGIAdapter3非対応はAPIでは`None`として扱うが、
このRTX 3060向けの受入batchはnative usage/budgetの欠落を失敗にする。
D3D12 stagingのlive byte/high-waterは未観測なので`null`で残し、準備容量・native heap必要量と区別する。
native OOM・Device lossの注入やOSによるVRAM evictionの受入は、この容量不足試験には含めない。
[最新結果と共有データ](../verification/current.md#bistro-residency)で実行済み範囲を確認する。

<a id="godot-bistro"></a>

### Godotを使ってBistroの性能目標を校正する

`tools/godot_bistro/`はGodotの描画系全体を比較基準にする利用側のプロジェクトである。
Renderer本体や既存のcookを変更しない。既定profileは影・IBL・AA・BloomなしのB2相当である。
`--profile b3`は静的IBL・CSM・FXAAを含み、BistroのAO適用はRendererと同様に無効とする。
Godot native AgX/FXAA位置/IBL前処理/PCF/表示処理の差は残し、B4のBloom追加後は再校正する。
実測と暫定予算は[現在の検証状況](../verification/current.md#godot-bistro)を参照する。

[公式Godot 4.7.2](https://godotengine.org/download/archive/4.7.2-stable/)のStandard Windows x86_64版、
Standard export templates、同releaseの`SHA512-SUMS.txt`を取得する。
[配布元](https://github.com/godotengine/godot-builds/releases/tag/4.7.2-stable)と固定値は次のとおり。
ZIP/TPZをSHA512一覧と照合してから展開する。全OS用TPZは約1.28 GBだが、必要な実行用templateは
`templates/windows_release_x86_64.exe`だけであり、既存Godotの設定・templateを変更しない。

| 配布物 | SHA512 |
|---|---|
| `Godot_v4.7.2-stable_win64.exe.zip` | `83decd58fdf67b9d657958a1ae6bf1929c20785315a81effe245874cdc57acb709bf868e00778a96984338c1b29dafdb453c6847747694621c6ecf5da2259993` |
| `Godot_v4.7.2-stable_export_templates.tpz` | `ca4d71c4d7b81dfc15d1a98baa07534aa95b03fdda78a0075b06672e1648d2e5f40980c9adc28d23e1b92e732ee7bf3461997aa804af74ec2fcd7a93ccb84079` |

editorは`.tools/godot-baseline/editor/`、Windows release templateは
`.tools/godot-baseline/templates/`へ展開した例を示す。
最初に[Bistro LFS素材](../../assets/bistro/README.md)を取得する。worktreeでは素材のある元checkoutを
`--asset-root`へ指定でき、全407 glTF/buffer/PNGをmanifestのsize/SHA256と照合する。

```powershell
python -B tools/godot_bistro/benchmark.py prepare --editor .tools/godot-baseline/editor/Godot_v4.7.2-stable_win64_console.exe --template .tools/godot-baseline/templates/windows_release_x86_64.exe --asset-root . --output .tools/godot-baseline/prepared
python -B tools/godot_bistro/benchmark.py run --prepared .tools/godot-baseline/prepared --output .tools/godot-baseline/smoke --smoke --warmup 120 --frames 60 --repeats 1 --validation
python -B tools/godot_bistro/benchmark.py run --prepared .tools/godot-baseline/prepared --output .tools/godot-baseline/formal
python -B tools/godot_bistro/summarize.py .tools/godot-baseline/formal .tools/godot-baseline/evidence.json
```

各outputは新しいdirectoryを要求する。`prepare`だけはheadless editorで小さなrelease packを作る。
計測は実GPUへ接続したrelease exportであり、headlessやeditor実行を使わない。
APPDATA/LOCALAPPDATAはchild processだけoutput配下へ向け、editor設定・shader cacheも隔離する。
Godot本体は[MIT license](https://godotengine.org/license/)、Bistroは既存のCC-BY帰属を維持する。
EXE/PCK、shader cache、入力原本、画像はGitへ追加しない。

正式条件は1920×1080、Forward+、明示Vulkan、VSync/FPS上限なし、固定60Hz入力routeである。
cameraはwall時間で進めずframe番号で進める。3,600 frameの二周warmup後、1,800 frameを収集する。
全geometryのcullingを無効にする`all`と、frustum cullingを使う`frustum`のそれぞれで、
viewport timingの無効/有効を独立3 processずつ、計12 run直列実行する。周回ごとに順序を回す。
Godotの全1,296 mesh/1,591 surface/2,832,120 triangleと132材質/405画像を確認し、LOD/occlusionは無効にする。
内部viewportを固定するのでWindowsのwindow枠による小さい描画領域を1080pと誤認しない。

CPUの診断内訳として`get_frame_setup_time_cpu()`とviewport CPUを別々に保存する。
setupは現在draw、viewport CPU/GPUは遅延結果であり、同一frameのCPU時間として加算しない。
`render_pre_post_ms`はSafe/main-thread描画のpre/post signal間のelapsed、frame間隔はpost-drawの単調時計差である。
前者はnative Graph lowering・command記録・submit・present・ring-slot待機/回収も含むが、draw前のmainloop同期とscript/physics、
post signal後のtimestamp dispatch・memory monitor更新は除く。process全体のCPU占有時間とは区別する。
GPU queryは遅れて観測され得るので、sampleのframe番号をGPU命令の厳密なframe対応に使わず、
同じ周期routeの分布として比較する。表示完了時刻・input latency・実scanout FPSは計測していない。
通常実行のframe間隔とtiming有効時のCPU/GPU値を区別する。

生sample、各runの平均/p50/p95/p99、実行引数、EXE/PCK/source/conditions/入力/画像のhashを保存する。
`summarize.py`は12 runの集合・hash・生sampleからの再集計を照合し、
最も遅い全量・timing有効runのGPU p95を0.5ms単位で切り上げた参照値を出力する。
native CPU時計を指定した場合は、全量・timing無効runの全process charged CPU/frame平均を別に集計し、
最も遅いrun平均を0.5ms単位で切り上げる。平均CPUの値をper-frame p95へ読み替えない。
欠落や0/非有限の有効時GPU時間、debug build、誤backend、異なる解像度・pacing、
不完全なframe窓は拒否し、未計測値を0へ補完しない。
固定7視点とroute先頭のPNGはROI後に取得する。画像の形式・寸法を検査しただけで、
Rendererとのpixel同等性を主張しない。必要な画質照合は別に行う。

runtime glTF importはGodotのMikkTSpace・StandardMaterial3Dを使う。
B2のrelease実行時読込は非圧縮ImageTextureのPNG baseだけを保持する。以前の「自動でmipを生成する」という説明は誤りだった。
B3では`material_mips.gd`が読込境界で全405画像から完全なmip列を明示生成する。
normal画像だけは縮小段を正規化し、色・ORM・emissiveはGodotの通常のチャンネル平均を使う。
元のbase画素・材質参照とResource/RIDを保持し、準備時のnative readbackで形式・非圧縮・全3,529段・
base hashを確認する。材質のlinear/mipmap filterとtrilinear設定もgetterで検査する。
1×1画像の縮小段数は0と扱う。これらの画像走査・生成・読戻しはwarmup/ROIの前であり、毎frameには行わない。
元DDSから供給済みの全mipを保持するRendererとは、縮小段の画素が異なる。
Godot固有のBRDF、depth prepass、drawのまとめ方、表示処理も残す。
これらは描画系全体の基準であって、Graph単体の追加費用や二実装の同一仕事量の証明ではない。
Godotのmemory monitor値を実VRAM常駐量やRendererのzero-allocation保証として扱わない。
計測中は他のbuild/GPU試験/cook/画像解析を並行させず、native validationは別runで実施する。

CPUだけの証拠受入試験は以下で実行できる。通常CIにも含める。

```powershell
python -B -m unittest discover -s tools/godot_bistro -p 'test_*.py'
```

### B3の画像受入とGodotとの正式比較

IBL・影・表示変換を完成させてから、画像受入と通常版の性能比較を別々に実行する。
画像受入は`memory-profiling`付きstrict/Disabledの独立EXEを保存し、通常窓24 frame＋warmup8、
各条件3 processで全7視点・影4枚・AAなし・差し替え・同時View・容量不足からの回復を確認する。

```powershell
python -B tools/bistro/b3_acceptance.py --strict-binary .tools/bistro-residency/strict-build/release/renderer-scenes.exe --shipping-binary .tools/bistro-residency/shipping-build/release/renderer-scenes.exe --source-commit (git rev-parse HEAD) --manifest .tools/bistro-pbr/cooked/manifest.json --ibl-manifest .tools/bistro-pbr/ibl/manifest.json --output .tools/bistro-pbr/b3-acceptance --repeats 3
```

正式比較のRenderer EXEは検証・memory計装・Tracyを含めない。同じclean commitからRendererとGodotを準備する。
Godotには別buildのoptional native CPU clockを指定し、元editor/templateとDLL/descriptor/ABI header/sourceのhashを保存する。
通常ホストの非管理者・非AppContainerで実行し、計測中はbuild/cook・他のGPU試験や画像解析を並行させない。
ゲーム等の別の描画負荷も待機させ、実行時のCPU条件を記録する。

以下の`--cpu-policy performance-cores`は、同じ最高`EfficiencyClass`のコア群を両対象processへ割り当て、
HighQoSを明示する。各processを停止状態で作成し、affinityとQoSのnative読戻しが成功してから初期threadを再開する。
設定失敗や予期しない初期thread数は、対象を終了・回収して測定失敗とする。
この機材ではPコアの論理CPU 0〜11が対象であり、CPU全体を既定schedulerに任せた値とは条件を分ける。
単一processor groupを対象とする。`system`が既定で、CPU設定を変更しない参考条件として残している。
CPU clockや外部負荷は別途確認し、過去の変動原因をコア選択だけで断定しない。
選択根拠は[Windows CPU Sets](https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-system_cpu_set_information)、
QoSは[Windowsの契約](https://learn.microsoft.com/en-us/windows/win32/procthread/quality-of-service)に従う。

```powershell
python -B tools/cpu_profile/build.py --package renderer-scenes --profile release --bin renderer-scenes --features vulkan,d3d12 --output .tools/bistro-pbr/b3-normal-build
python -B tools/godot_bistro/benchmark.py prepare --profile b3 --cpu-clock-manifest .tools/b3-cpu-clock/manifest.json --ibl-manifest .tools/bistro-pbr/ibl/manifest.json --editor .tools/godot-baseline/editor/Godot_v4.7.2-stable_win64_console.exe --template .tools/godot-baseline/templates/windows_release_x86_64.exe --asset-root . --output .tools/godot-baseline/prepared-b3
python -B tools/bistro/b3_benchmark.py plan --build-manifest .tools/bistro-pbr/b3-normal-build/manifest.json --manifest .tools/bistro-pbr/cooked/manifest.json --ibl-manifest .tools/bistro-pbr/ibl/manifest.json --godot-prepared .tools/godot-baseline/prepared-b3 --output .tools/bistro-pbr/b3-comparison --timeout 1200 --cpu-policy performance-cores
python -B tools/bistro/b3_benchmark.py run --build-manifest .tools/bistro-pbr/b3-normal-build/manifest.json --manifest .tools/bistro-pbr/cooked/manifest.json --ibl-manifest .tools/bistro-pbr/ibl/manifest.json --godot-prepared .tools/godot-baseline/prepared-b3 --output .tools/bistro-pbr/b3-comparison --timeout 1200 --cpu-policy performance-cores
```

`plan`は実行予定を表示し、`run`だけが新規outputへ実行する。
Renderer二APIとGodotのall/frustum、timer off/onを各3独立process、計24 processで順序を回して直列実行する。
Bistroで影map再利用の費用を個別に測るには、通常実行と`--redraw-shadows`を加えた実行を同じ条件で比較する。
両方で影とAAを有効にし、通常実行のreportに含むcascade別hit／missとフレーム間隔を記録する。
`all`は主cameraのfrustum cullingを無効にする条件であり、Godotの影のcascade／caster判定は残る。
Godotのdraw／primitive値はエンジン内のbatch等の統計で、RendererのGraph draw数やnative API呼出数と同じ指標ではない。
影の絞り込み・depth prepass・batchingを含む描画構成の差を保持した、レンダラー全体の参考比較とする。
3600 frameのwarmup後に1800 frameを測り、CPUの各phase・poll/yield・OS charged CPU・GPUを分ける。
`active_host_elapsed_ns`は重複しないcaller処理の経過時間で、yieldを除くがOS deschedulingを含み得る。
native CPU timerは内包されるので加算せず、観測完了latencyやCPU/GPUの和から表示FPSを計算しない。
正式CPU指標は両engineで共通のWindows全process・全thread charged CPU/frame平均とする。
Godotはwarmup最後とN frame後のpost-draw callback、Rendererは`--steady-state-cpu`のN route反復を囲む。
Rendererは開始終了のhost所有未回収submission数を実測して揃え、Godotは通常frame ringを維持し、強制GPU idle/readbackを加えない。
これは定常的な流入出を揃えた区間であり、同じproducer frame IDの生成から完了までを囲む境界ではない。
Godotのqueryには公開producer IDがなく、sample frameはcallback/routeの観測番号である。
caller thread CPU、QPC wall/N、throughput、Rendererの全loop/最終drainと各submissionの観測完了latencyも独立して残す。
平均CPUをper-frame p95へ読み替えず、wallから全thread CPUを引いてwait時間としない。
native時計のQPC窓は呼出し前後の単調時計で囲んで整合を検査し、境界のdeschedulingを架空の精度で消さない。
Godotはthread_model=1を明示しworker設定を保存する。実worker数はscriptへ非公開なので、別のverbose metadata probeと固定ソースで調べ、formal runごとの観測値とは区別する。
verboseは固定Godotの既存RGB8→RGBA8変換通知を追加表示するため正式測定には使わず、警告拒否も緩めない。
affinity論理CPU数をworker数とはみなさない。
Godotにはscript/mainloop/表示が含まれ、Rendererはoffscreenである。BRDF/影/AA等の違いと計装費用も残す。
正式CPU参照予算はGodot all・timer/query無効の最悪run平均を0.5ms単位で切り上げ、旧3ms p95は流用しない。
画像・strict/shipping・寿命・ゼロframe割当はstationary経路でも別artifactで確認する。表示完了・操作の最終受入はB5で行う。
Godot画像のB3反復は、予備検証で固定した全画素最大1 RGB code・変動畫素16以下・alpha完全一致を使う。
露出/AAの既知誤りを拒否する感度を確認し、正式runの失敗に合わせて閾値を変更しない。
Renderer反復とstrict/Disabled照合はbit exactを維持する。結果と未確認範囲は[現在の検証状況](../verification/current.md)へ記録する。

<a id="bistro-tracy"></a>

### TracyでBistroのhost処理を区分する

`tracy`は既定offのCPU zone計装で、通常権限でcollectorへ接続できる。
[固定tool・artifactとCLI](tooling.md#tracy-profiling)に従い、同じsource/profileから通常EXEと計装EXEを別々に保存する。
通常release、同じ計装EXEのdisconnected、connectedを、同じcook/conditions/shader・API・view・
解像度・warm-up・frame数・in-flight数で比較する。例の全量条件はroute、1920×1080、
2 in-flight、3,600 warm-up＋1,800測定frameであり、採用したmatrixと実行済み範囲は[現行検証](../verification/current.md)へ記録する。
build/cook・他APIの負荷・別collectorの処理を測定中に並行させない。

feature付きの`--capture-handshake`は、warm-upと全回収後のREADY→接続/GO→測定/DONE→
END→画像capture/native teardown・対象の通常return→client送信queueの排出→collector自然終了・保存という順序で進む。
通常完了でcollectorへSIGINT/Disconnectを要求せず、traceに含まれるROI後の画像/終了処理は解析対象から外す。
対象・runを結ぶnonceの`bistro.capture_identity`は測定前に出し、metadataコピーをROIへ入れない。
`bistro.measurement`は指定frameの処理と最後の回収を含む一つのzoneである。
`bistro.frame`とそのframe markはCPU iterationを示し、枠が埋まっていれば古いsubmissionの回収、
camera生成、Graph構築/compile、submitとexport/deleteのbookkeepingを含む。
別zoneの`bistro.final_drain`やiteration間のgapはframe zoneから外れる。
frame markの到着はzoneの全送信完了を保証せず、解析には完全なmeasurement rootと指定数のframe zoneを要求する。
既存`frame_wall_ns`の観測完了latencyや表示FPSへ読み替えず、per-callとper-frameのpercentileも分ける。

Graph解析の段階、Vulkanのprepare/record・draw batch、D3D12のprepare/record・draw/binding等へ
明示zoneがある。D3D12のdescriptor/bind区間はSRVの具体化とroot binding等を含み、
CreateSRVだけの費用やdriver内部の実行箇所を分離したものではない。
inclusive/selfはwaitやdeschedulingを含むwall elapsedで、長いzoneは次の調査対象を示す。
未計装関数のsampling、scheduler解析、GPUの絶対時刻との対応はこの収集で得たものとして扱わない。

`--gpu-timing`のnative CPU timer/GPU query、追加検証、memory counterはTracyとは独立である。
Tracy導入だけでこれらを変更せず、通常のゼロ確保受入はTracyを除外した構成で行う。
計装clientの専用確保・background処理とzoneの送信費用を別に記録し、切断中にもゼロ費用・ゼロ確保を主張しない。
通常artifactへ計装EXEの時間を混ぜず、観測したhotspotと今後の最適化候補を区別する。

## ビルド・品質確認

Vulkanの同期・寿命・資源更新・機能導入のレビューには
[ベストプラクティス](vulkan-best-practices.md)を参照します。
仕様要件、対象GPUで測る最適化候補、現行実装の契約を区別しています。

Rustのモジュール配置とテスト方針は[AGENTS.md](../../AGENTS.md)に従います。
`mod.rs`は禁止し、`foo.rs`と`foo/bar.rs`の形式を使います。
workspaceの`clippy::mod_module_files`を`deny`に設定しています。

CPU経路の確認にはVulkan SDK、ローダー、GPU、ウィンドウシステムは不要です。
テスト・全target確認には前述のCコンパイラーが必要です。

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --no-default-features -- -D warnings
cargo test --locked --workspace --no-default-features
```

Vulkan実装もビルド対象へ含めます。
ashは実行時ロードするため、ビルドにVulkan SDKやリンクライブラリは不要です。
以下の通常テストはVulkanローダーを読み込まず、Instanceも作成しません。

```sh
cargo clippy --locked --workspace --all-targets --no-default-features --features vulkan -- -D warnings
cargo test --locked --workspace --no-default-features --features vulkan
```

[CI](../../.github/workflows/ci.yml)はWindows／LinuxとCPU／Vulkan featureを確認し、
別jobで[cargo-deny](tooling.md#cargo-deny)の依存検査を実行します。
ワークスペースのdoctestはLinuxのCPU構成（Debug、既定feature無効）で1回実行します。現在の3件は、公開APIから `FrameGraph` の非公開フィールドを書き換えるコードがコンパイル時に拒否されることを確認します。
`docs/guides/` 直下のMarkdownと `docs/verification/current.md` のいずれか、またはその両方だけを変更したpush・PRでは、HTMLの生成、リンク・画像の検査、Mermaidの再ビルドとChromiumでの描画検証を実行します。ほかのファイルが差分に含まれる場合や比較元を取得できない場合は、Rustとasset-toolsの検証も実行します。
ワークスペースの単体・統合テスト一式は、Linux CPU（Debug・開発検証なし）、Linux Vulkan（Debug・開発検証あり）、Windows Vulkan＋D3D12＋presentation（Release・開発検証あり）の3構成で実行します。
各構成では全targetをClippyで検査します。Windowsの`presentation`単独では、ネイティブbackendを無効にした状態で表示とシェーダープログラムの契約もテストします。
LinuxのVulkanとWindowsの二API表示では、BistroとPBRをパッケージ単独でも確認します。Cargoが有効にする依存featureは、ワークスペース全体を対象にしたときと異なるためです。
Linux VulkanとWindowsのGPU全6構成では、開発検証の有無それぞれでReleaseをリンクし、`sashiko-renderer-core` のbackend・runtime・表示・シェーダープログラムのテストを実行します。Windows二API表示の開発構成では、これらを上記のテスト一式に含めます。
WebGPU の表示・公開 API と共有 CPU ワーカーのブラウザ試験は別ジョブで並列に実行します。両方とも `Cargo.lock` に対応する公式 Windows 版 wasm-bindgen CLI と test runner を使い、展開前に SHA-256 を照合します。wasm-bindgen を更新するときは CI のハッシュも更新してください。
Tracyを有効にした構成では全targetをClippyで検査し、ReleaseのBistroでキャプチャ接続・コマンド処理のテストを実行します。Rust検証ジョブはバックエンドと試験の組み合わせごとにCargo依存のキャッシュを再利用します。
サンプルのCPU契約は代表構成で実行します。`windowed` の引数・再描画・終了処理、読戻しの容量と画素の判定は、Windowsの二API表示でDebugを1回実行します。実際の表示と最小化からの復帰は実機の `--smoke`・`--verify` で確認します。`frame_loop` の引数・容量・集計はLinux VulkanのReleaseで検証します。CPUのGraph構成例2本はLinuxでDebugのエントリーポイントを実行し、Releaseのテストで生成したパスを照合します。
各backendは製品／開発検証＋dumpの両構成を確認し、Vulkanはrelease両構成と独立診断featureも確認します。
これは表示の実GPU受入をCIで実行するという意味ではありません。
依存更新ではlockfile差分と両feature構成を確認します。

ドキュメントはすべてのpushとPRでHTMLを生成し、ローカルリンク・アンカー・画像を検査してHTML成果物を保存します。`docs/`、`tools/docs/`、`.gitattributes`、CIワークフローの変更時と差分の比較元を取得できない場合には、Mermaidの再ビルドとChromiumでの描画検証も実行します。Playwright更新時は、CIの公式コンテナのバージョンとダイジェストをnpmのロックファイルに合わせて更新してください。

<a id="public-api-comparison"></a>
<a id="foundation-comparison"></a>

## 設計比較の再現

公開APIと実装基盤の採用時に使った試作は、[実行手順を記録したコミット](https://github.com/tsukushibito/sashiko/blob/3fee62930ed59a01d2fdee902a9a11542fdd6de1/docs/guides/development.md#公開apiのcpu比較試作)から再現できる。
[公開APIの比較結果](../verification/current.md#public-api-comparison)と[実装基盤の比較結果](../verification/current.md#foundation-comparison)に実行条件を記録している。
現行の公開APIを動かすには[Examplesガイド](examples.md)を参照する。

<a id="管理下uniform-programの作成と受入"></a>

## 管理下資源Programの作成と受入

Graphのowned構成は、SDKやGPUを使わず次の例で確認できる。

```powershell
cargo run --locked -p sashiko-renderer-core --example graph_composition
cargo test --locked -p sashiko-renderer-core --test graph_shader_bindings
cargo test --locked -p sashiko-renderer-core --example graph_composition
```

例は利用側の関数に二段のsampled Rasterと中間画像を隠し、出力handleだけを返す。
短命draw/binding配列をGraphが所有し、用途とVertex/Fragment stageを実Programから導く。
これはCPU計画の例で、一般native graphics/dispatch実行やBloom処理の受入ではない。
現在の範囲と保証境界は[Graph設計](../design/rendergraph.md#program由来bindingとshader-stageのowned計画)を参照する。

native実行は`Graph::dispatch`と、実行前のCompute Program準備を使う。
`uniform_readback`と`storage_readback`は16-byte範囲、`sampled_readback`はRGBA8のnearest/clamp、
`sampled_float_readback`はRGBA16FとD32のFP32出力を確認できる。
資源の初期化はGraph上のupload、clear、またはProgramが保証する書き込みで行う。

SDKなしのABI契約回帰は次を使う。生成条件を更新した場合は[shader再生成](tooling.md)も実行する。

```powershell
./crates/sashiko-renderer-core/shaders/test-managed-abi.ps1
./crates/sashiko-renderer-core/shaders/regenerate-dxil.ps1 -SdkRoot C:/VulkanSDK/1.4.341.1 -Verify
./scripts/regenerate-d3d12-bindings.ps1 -Verify
```

厳格構成と製品構成の実GPU受入は[owned Computeの二API手順](#owned-compute-acceptance)を使う。
結果・限界は[現在の検証状況](../verification/current.md#managed-uniform)へ記録する。

<a id="validation-settings"></a>

## 製品・開発用の検証設定

実装・レビューでは[製品版と開発用の検証方針](validation-policy.md)に従います。
保証済みの入力をdraw／submitごとに再走査せず、保証を取込・更新・Graph構築等の境界へ移します。
同期生成、GPU寿命・正常完了、容量とnative失敗の処理は製品でも維持します。
検証の切替を変更するときは、適用範囲の有効／無効両構成と最適化された通常経路を確認します。

既定featureは空です。最適化と独立した追加featureを使います。

| feature | 追加する処理 |
|---|---|
| `development-validation` | Graphの独立再検査・dead部分の循環診断。`vulkan-diagnostics`と`d3d12-diagnostics`も有効にする |
| `vulkan-diagnostics` | debug_utils、native callback・monitor、命名・label。単独でも選択可能 |
| `d3d12-diagnostics` | D3D12のDebug Layer／GBV要求、InfoQueue・callback・終了後monitor。単独でも選択可能 |
| `graph-diagnostics` | 論理Graph／Vulkan loweringのdumpと専用解析情報。検証とは独立 |

`development-validation`ありではnative検証の既定が`ValidationMode::Required`、なしでは`Disabled`です。
`ValidationMode`はnative Layer要求を選び、Renderer側の追加検証はCargo featureで選びます。
`vulkan-diagnostics`なしで`Required`を明示すると、検証できない要求として失敗します。
WindowsのD3D12も`D3D12ValidationMode`で同じ既定を選び、`d3d12-diagnostics`なしの明示`Required`は失敗します。
製品構成は`--release --no-default-features --features vulkan`で、診断拡張やLayerを要求しません。
任意Graph入力の保証、実行計画・同期・寿命・容量・native失敗の処理は両構成に残ります。

```sh
cargo test --locked --workspace --release --no-default-features --features vulkan
cargo test --locked --workspace --release --no-default-features --features vulkan,development-validation,graph-diagnostics
cargo tree --locked -p sashiko-renderer-core -e features --no-default-features --features vulkan
```

製品経路の実GPU内容・CPU保持の確認は、native受入と別runで実行します。

```sh
cargo test --locked -p sashiko-renderer-core --release --lib --no-default-features --features vulkan ::vulkan_shipping_ -- --ignored --nocapture --test-threads=1
```

これはnative検証なしの内容照合です。offのmonitorは診断を収集しないため、
件数0や起動報告の不在を外部Layer注入がなかった証明には使いません。

feature統合後の実構成を確認し、
最適化・Renderer内の追加検証・native検証・profilingをそれぞれ記録します。
`--all-features`だけでは検証無効構成の確認になりません。
コード・依存関係・生成物や走査／allocation／保持byte等から、除去を主張する費用を確認します。
実GPUの正しさ受入はCore＋SyncVal等を維持し、GPU-AV／GBVによる追加診断と製品相当の性能測定を分けます。
変更に応じた確認範囲は[検証方針の受入基準](validation-policy.md#verification)を参照してください。

<a id="runtime-capacity"></a>

## 準備容量と不足時の回復

`runtime::Runtime::new`またはWindows用の`Runtime::for_window`に、バックエンドを選ぶ
`RuntimeConfig`、共通の`RuntimeLimits`、資源とProgramの`WorkingSet`を渡します。
初期化の後も、登録済み資源のキーを保持したまま`Runtime::prepare_working_set`で作業セットを交換できます。
新しい作業セットの準備が成功した時点で、選ばれなかった未登録・使用終了済みの資源を解放します。
`FrameSubmission`の破棄はGPUを待機せず、受理済み作業は`poll_frame`または明示的な`wait_frame`で完了を確認します。
特定の投入結果は`wait_for_frame`で取得できます。他のフレームの結果は引き続き取得できます。
表示時は`WindowPresenter::prepare`の`Ready { extent, .. }`を使ってGraphを構築します。

通常frameでは準備済み容量を再利用します。`GraphPool`の`GraphCapacity`が宣言時の各上限を適用します。
`RuntimeLimits::graph`には同じ値を渡し、バックエンドの実行領域の準備に使います。
Vulkanの共有stagingとD3D12の転送領域はそれぞれ
`runtime::VulkanConfig`と`runtime::D3D12Config`で指定します。異なるサイズのbufferは、サイズごとに`ResourceClass::Buffer`として予約します。
buffer/画像classのcountはglobalな物理準備数で、execution数による自動乗算はありません。
`capacity.buffers`は各executionのlogical buffer operand数、`buffer_bytes`は最大logical sizeです。
同時保持数には記録中・GPU実行中だけでなく、未読CPU結果、export、import snapshotも含めます。
余裕を増やす操作は初期化・ロード・resize等の変更境界で行い、通常のsubmitに増設やwaitを隠しません。

Graphの`GraphError::capacity()`は不足した項目と`limit`／`required`を返します。
`runtime::ExecutionError`および両native runtimeのエラーも`capacity()`で共通の`capacity::RuntimeCapacityError`を返し、
`resource`と`recovery`を文字列照合なしで確認できます。byte容量の値はbyte、slot類は個数です。
値が判明しない場合や必要数がoverflowする場合は`None`であり、ゼロや確保成功の保証ではありません。

| `CapacityRecovery` | 次に行う操作 |
|---|---|
| `Prepare` | 不足しているworking setやProgramを、対応する明示準備APIで用意する |
| `Reconfigure` | 要求または設定上限を見直して再準備する。同じ要求のpollだけでは回復しない |
| `Poll` | 受理済みGPU／表示処理をpollし、完了した作業ownerを返してから再試行する |
| `ReleaseResults` | 未読readbackや計測結果を消費・返却する。GPU完了だけではその領域は戻らない |
| `ReleaseOwners` | 保持しているexport、snapshot、記録中資源等を返す。GPU側のownerは完了観測で退役させる |
| `Recreate` | 採番等の継続できない状態を持つ該当資源／Runtimeを明示的に再作成する |
| `RestartProcess` | process全体の採番を使い切ったため、処理を終了してprocessを再起動する |

回復方法は次のpollの成功や、必要なownerが自動的に返ることを約束しません。
特に`CapacityExhausted`を一律に再試行するloopは、未準備Programや固定byte上限超過で進行しなくなります。
必要な内容を消費してからownerを返し、同じ要求が上限を超える場合は明確なエラーとして扱います。
D3D12のbuffer／transfer byte上限はRuntime作成時の設定なので、画像working setだけの再準備では変わりません。

## M4のD3D12準備と実GPU受入経路

M4の実GPU受入は、現在利用できるWindows x64機（Core i5-14600KF、RTX 3060）で
VulkanとD3D12を逐次実行するローカル手順を採用します。
機材・実行の担当はリポジトリ管理者の`tsukushibito`で、Codexは許可されたCLI操作を実行します。
各実行では選択adapterとdriverを記録し、二APIが同じhardwareを使うことをLUIDで確認します。
現在のGitHub Actionsはbuild・Clippy・CPU試験を担当し、実GPU runnerは用意していません。
外部runnerの契約や常設サービスの導入は必要ありません。

`d3d12`はDevice・単一DIRECT queueと、共通Graphのtransient buffer upload・copy・readbackを提供します。
現在の画像、import／export、資源pool等の対応範囲と、process内で固定するnative診断設定の制約は
[D3D12 runtimeの設計](../design/implementation-policy.md#d3d12-runtime)を参照してください。

準備試験には通常のRust／MSVC環境に加えて、D3D12ランタイム、Windows Graphics Toolsの
Debug LayerとGPU-based validation（GBV）、Vulkan 1.3の実行環境とValidation Layerが必要です。
shader再生成の確認にはVulkan SDK **1.4.341.1**の固定toolを使います。
bindingsとshaderの生成手順・固定入力は[ツールガイド](tooling.md)を参照してください。
通常のRustビルドは管理下の生成物を使い、生成toolやSDKを要求しません。

Windowsの`d3d12`と`vulkan,d3d12`構成にも、実GPUを起動しない品質確認を実行します。

```powershell
cargo clippy --locked --workspace --all-targets --no-default-features --features d3d12 -- -D warnings
cargo test --locked --workspace --no-default-features --features d3d12
cargo clippy --locked --workspace --all-targets --no-default-features --features vulkan,d3d12 -- -D warnings
cargo test --locked --workspace --no-default-features --features vulkan,d3d12
```

両backend構成について、`development-validation,graph-diagnostics`を追加した構成も確認します。
CIはD3D12単独／Vulkan併用のrelease CPU試験、および独立したnative診断featureのcheckも実行します。
CPU試験・build CIの成功は実GPUの内容・同期確認と分けて扱います。

### 準備probe

実GPUの準備probeはignored試験として分離します。次のスクリプトはshader再生成一致を確認し、
対象のignored試験が存在することを検査してから、同じprobeを既定3回、同時実行せずに実行します。
64-bitのWindows PowerShell 5.1またはpwshで、リポジトリのルートから呼び出します。

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File ./scripts/verify-m4-preparation.ps1 -SdkRoot C:/VulkanSDK/1.4.341.1 -Runs 3
```

出力は共有checkout直下のGit対象外の`.tools/m4/preparation/<実行ID>/`へ保存します。
worktreeから呼んだ場合も、この既定の保存先は共有checkout側です。
`-OutputDirectory .tools/m4/preparation/my-run`で新しい保存先を明示することもできます。
既存ディレクトリへの上書きは拒否します。
`summary.json`にcommit・worktree差分の有無・OS／CPU・Rust／Cargo・実行者・tool引数・
Cargo.lockとshader生成物のSHA-256・関連するValidation環境変数を記録します。
`shader-verification.log`、`probe-inventory.log`、各回の`probe-NN.log`と`probe-NN.json`を保持し、
失敗した回もログと失敗理由を残します。実行の経過時間は準備手順の壁時計時間です。

probeの単独実行コマンドは次のとおりです。レポートを保存するときは
`RENDERER_M4_PREPARATION_REPORT`へ新しいJSON出力先を指定します。
反復スクリプトはこの環境変数を各回へ渡し、終了時に元の値へ戻します。

```powershell
cargo test --locked -p sashiko-renderer-core --lib --no-default-features --features vulkan,d3d12,development-validation ::d3d12_preparation_ -- --ignored --nocapture --test-threads=1
```

準備段階では実hardware adapterの照合、Device作成前のDebug Layer／GBV要求と診断interface、
最小のallocation作成、およびnative資源の終了までの診断を確認します。
検証設定の要求・成功したinterface照会・実際に取得できる設定情報は区別してレポートへ残し、
取得できない実効設定を推測で確認済みにしません。
GBVの実効は起動message ID1016の有効報告で確認し、原文をJSONへ保存します。
InfoQueueの既定storage filterも実際に取得し、INFO／MESSAGEだけを抑制することを検査してから解除します。
元のfilterと抑制数は保存し、追加抑制・メッセージ破棄・callback失敗をゼロとします。
filterを無視するcallbackはprocess寿命の同期済みログを使い、最終Device Releaseまで登録を維持します。
DXGIのglobal queueも全native所有者の解放後まで保持して確認します。
warning／error／corruptionはゼロを要求し、Vulkan側も終了後まで既存の厳格な診断条件を適用します。
tool・Debug Layer・実GPUの不足、LUID照合不能、probeの不在、JSON欠落を成功やskipへ置き換えません。

この準備probeはGraphの実行や二APIの画像等価性の受入には数えません。
buffer byteの受入は次節、新しい画像・寿命契約はB1〜B5の各実装段階で、
同じ入力・解像度・期待値と寿命／失敗条件を二APIで反復確認します。
各段階でraw byte／画像・差分、Graph条件、終了後の検証ログを同じ実行IDへ保存します。
CPU／GPU時間とメモリ比較は診断なしの別のrelease実行とし、GBV等を有効にした正しさ試験の時間を
性能値へ流用しません。両APIの対応する負荷と計測経路が揃うまで、性能比較は未実行として記録します。
実GPU試験・計測を並行実行せず、機材を使えない場合は受入未完了として理由を残します。

<a id="d3d12-buffer-acceptance"></a>

### D3D12のbuffer Graph実行

[公開APIの二API試験](../../crates/sashiko-renderer-core/tests/d3d12_graph.rs)は、一度compileした同じGraphを
実際のVulkan／D3D12 Deviceで逐次実行します。vendor／device IDと有効なLUIDを照合し、
部分範囲・複数copy・同一bufferの非重複copyを含むreadbackの全byteを期待値と両API間で比較します。
この段階はbufferの受入であり、画像・shaderのGBV実行や性能の優位を確認したとは扱いません。

厳格検証と製品構成の内容照合は、設定を切り替える同一processに混ぜず、別processで実行します。
D3D12のDebug Layer／GBV設定は最初のDevice作成前に固定する必要があります。
厳格検証ではD3D12のDebug Layer／GBVとVulkan Core＋SyncValの実効を確認し、
試験が所有する全native参照の解放後までwarning／error、corruption、callback失敗等がゼロであることを要求します。
前提不足、LUID照合不能、診断の実効未確認、試験・レポート不在は失敗です。

```powershell
cargo test --locked -p sashiko-renderer-core --release --test d3d12_graph --no-default-features --features vulkan,d3d12,development-validation m4_buffer_shared_graph_strict -- --exact --ignored --nocapture --test-threads=1
cargo test --locked -p sashiko-renderer-core --release --test d3d12_graph --no-default-features --features vulkan,d3d12 m4_buffer_shared_graph_shipping -- --exact --ignored --nocapture --test-threads=1
```

製品構成は追加検証・両native診断featureをコンパイル除外し、native検証の要求は`Disabled`です。
offのmonitorは診断を収集しないため、件数0を外部のLayer注入がなかった証明には使いません。
strictとshippingのどちらも最適化された通常のprepare／submit／wait／readbackを通します。

[反復スクリプト](../../scripts/verify-m4-buffer.ps1)は両構成のignored試験が存在することを確認し、
既定3回、strictとshippingを逐次実行します。buffer copyにはshader生成がないためSDKの引数は不要です。

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File ./scripts/verify-m4-buffer.ps1 -Runs 3
```

出力先は既定で共有checkoutの`.tools/m4/buffer/<実行ID>/`です。
`-OutputDirectory`で新しい保存先を指定でき、既存の証拠への上書きは拒否します。
`summary.json`、ignored試験のinventory・featureログ、各回の`strict-NN.json`／`.log`と
`shipping-NN.json`／`.log`に、source・差分状態・環境、実adapterと検証設定、全byte照合と終了後の診断を保存します。
単独実行時は`RENDERER_M4_BUFFER_REPORT`に新しいJSON出力先を指定できます。
レポートはログにも`M4_BUFFER_REPORT=`で出力します。

backend内の試験では未投入キャンセル・作成失敗の後始末、Fence完了と資源保持も確認します。
長時間耐久・VRAM飽和や性能閾値を、このbuffer機能の受入条件へ追加しません。
性能改善を主張するときの比較測定は、正しさの受入と別に実施します。

<a id="owned-raster-acceptance"></a>

### owned Rasterの二API受入

[公開API試験](../../crates/sashiko-renderer-core/tests/raster_graph.rs)は同じowned Graphの複数draw、color-only、
depth付きcolor、depth-only、draw別Uniform/画像/samplerとRGBA16Fを二APIで逐次実行します。
公開標準パスによる同geometry prepass後のLoad／等深度描画／depth非更新と、
標準forwardの0 draw clear・二段画像copyのpost・HDR出力copyも同じfixtureで確認します。
さらに[標準構成fixture](../../crates/sashiko-renderer-core/fixtures/standard_fixture.rs)で、
標準forward直接、identity post、利用側多段Computeのpost、標準パスと同じComputeの直接合成を比較します。
3×7のRGBA16Fに既存の利用側sampled乗算／storage加算shaderを接続し、
無効・乗算→加算・加算→乗算・追加加算、draw数0／1／4／2と定数を変えます。
同じ入力のrecipeと直接合成は同じ全画素byteを要求し、使わないpost枝は通常のcullingへ任せます。
vendor/device IDとLUIDを照合し、全readback byteを独立期待値とAPI間で比較します。
Runtime/公開planの破棄後もcompletion/readbackの所有連鎖を維持し、D3D12のprepare/cancel、
後半drawの不正index拒否後の再利用、保持中tokenによるbackpressureも確認します。
tiny fixtureでhostから完了を観測する前に破棄する試験であり、破棄時点でGPUが実行中だったという主張はしません。

D3D12のindex内容追跡は`D3D12FrameCapacity::provenance_segments_per_resource`（既定64）で、
操作数・draw数と独立して予約します。buffer-onlyと完全な不変mesh要約だけなら0を指定でき、
任意upload/copyのslice数が容量を超える場合は`CapacityExhausted`です。上限変更は明示準備境界で行います。
backend内の`d3d12_shared_buffer_scopes_preserve_clear_results`と
`d3d12_index_provenance_capacity_rejects_and_recovers`は、共有Vertex/Index/Uniformの用途合成と次passへの分離、
容量0/1での拒否・cancel・再試行・Graph pin返却を確認するignored実GPU回帰です。
以下の二API受入と同じstrict／shippingのfeature集合で`--lib <試験名> -- --ignored --nocapture --test-threads=1`を使い、
設定ごとに別processで実行します。

```powershell
cargo test --locked --release -p sashiko-renderer-core --test raster_graph --no-default-features --features vulkan,d3d12,development-validation,graph-diagnostics owned_raster_common_graph_strict -- --exact --ignored --nocapture --test-threads=1
cargo test --locked --release -p sashiko-renderer-core --test raster_graph --no-default-features --features vulkan,d3d12 owned_raster_common_graph_shipping -- --exact --ignored --nocapture --test-threads=1
```

`RASTER_GRAPH_REPORT`に条件・内容・実効native検証と最終解放後の診断を出力します。
strictはCore＋SyncVal／Debug Layer＋GBV、shippingはRenderer追加検証と専用診断を除外しnative `Disabled`を要求します。
設定は別processで切り替え、前提不足をskipしません。診断なしの件数0は外部Layer注入不在の証明には使いません。

全画面描画は、頂点資源なしのProgramをindexed描画と同一パスに混ぜ、7×5の全画素と描画順を両APIで照合します。
テストのvertex shaderを更新する場合は、固定SDKの生成手順に`-Fullscreen`を指定します。

```powershell
./crates/sashiko-renderer-core/tests/support/shaders/custom_raster-regenerate.ps1 -Fullscreen -SdkRoot C:/VulkanSDK/1.4.341.1 -Verify
cargo test --locked --release -p sashiko-renderer-core --no-default-features --features vulkan,d3d12,development-validation --test fullscreen -- --ignored --nocapture --test-threads=1
cargo test --locked --release -p sashiko-renderer-core --no-default-features --features vulkan,d3d12 --test fullscreen -- --ignored --nocapture --test-threads=1
```

独自Rasterの公開契約は[`custom_raster`](../../crates/sashiko-renderer-core/tests/custom_raster.rs)と
[利用側fixture](../../crates/sashiko-renderer-core/fixtures/custom_raster_fixture.rs)で確認します。
この試験は`RasterProgramDescriptor`の静的取込を使います。外部ファイルから読み込む場合は
[実行時の取込手順](tooling.md#runtime-shader-import)を参照してください。
24-byte頂点、32-byteのVS／FS inline定数、三groupの疎なUniform／StorageRead／画像／sampler bindingを使い、
標準forward直接・postの独自Raster交換・標準opaque＋独自半透明＋標準出力copyを同じ入力で比較します。
straight／premultiplied alpha・加算・置換、draw順、depth testとwriteなしを独立の全画素期待値で照合します。
標準meshの定数は`RasterParameters::new(...).unwrap().into()`、独自定数は
`RasterConstants::new(&words)`を使います。後者は有限値の意味検証を行わず、trusted shaderは全許容word値で範囲安全である必要があります。

外部ファイルのRasterとComputeを実GPUで確認するには、次のテストを両構成で逐次実行します。

```powershell
cargo test --locked --release -p sashiko-renderer-core --no-default-features --features vulkan,d3d12,development-validation,graph-diagnostics --test fullscreen --test compute_graph runtime_loaded_ -- --ignored --test-threads=1
cargo test --locked --release -p sashiko-renderer-core --no-default-features --features vulkan,d3d12 --test fullscreen --test compute_graph runtime_loaded_ -- --ignored --test-threads=1
```

通常ビルドはチェックイン済みartifactを使い、shader compilerを要求しません。
shader変更時は[固定SDKの生成手順](tooling.md)に加え、利用側fixtureの全artifactを再生成・照合します。

```powershell
./crates/sashiko-renderer-core/tests/support/shaders/custom_raster-regenerate.ps1 -SdkRoot C:/VulkanSDK/1.4.341.1 -Verify
```

```powershell
cargo test --locked --release -p sashiko-renderer-core --no-default-features --features vulkan,d3d12,development-validation --test custom_raster -- --ignored --nocapture --test-threads=1
cargo test --locked --release -p sashiko-renderer-core --no-default-features --features vulkan,d3d12 --test custom_raster -- --ignored --nocapture --test-threads=1
```

同fixtureの変動frame全体を独立processで計測するときは、次のmodeを使います。
24 warm-up＋24測定frame、3 pendingと3保持結果を使い、readback・poll／退役・全slotの返却まで測ります。
診断ありbaselineはfeatureへ`development-validation,graph-diagnostics`を追加し、`--baseline`を渡した別processで実行します。
初期化／明示Program集合の変更時のpipeline・descriptor準備は通常frameと分けます。

```powershell
cargo run --locked --release -p sashiko-renderer-core --no-default-features --features vulkan,memory-profiling --example frame_memory -- vulkan-custom-raster
cargo run --locked --release -p sashiko-renderer-core --no-default-features --features d3d12,memory-profiling --example frame_memory -- d3d12-custom-raster
```

通常フレームのヒープ/native作成・破棄はlibtest外の独立processで別に測定します。
`frame_memory`の`vulkan-raster`/`d3d12-raster`は明示準備後に入力・draw数・Program・attachmentを変え、
Graph構築からreadback消費・全token返却までを連続窓に含めます。診断ありは
`development-validation,graph-diagnostics`を追加した別buildへ`--baseline`を指定して別計測します。
`--baseline`だけで診断featureを有効化するものではありません。

```powershell
cargo run --locked --release -p sashiko-renderer-core --no-default-features --features vulkan,memory-profiling --example frame_memory -- vulkan-raster
cargo run --locked --release -p sashiko-renderer-core --no-default-features --features d3d12,memory-profiling --example frame_memory -- d3d12-raster
```

<a id="general-resource-acceptance"></a>

### 一般資源・履歴の二API受入

[`general_resources`](../../crates/sashiko-renderer-core/tests/general_resources.rs)は、同じ利用側のowned Computeと
標準opaqueパスをVulkan／D3D12で逐次実行する。初回／reset／resize、3未完了Graphの履歴chain、
一般buffer・HDR画像の更新／delete、重複import、read-only snapshotの再利用、古いrevisionの拒否、
importしたvertex/indexとattachment Loadを全画素照合する。D3D12はprepare後の更新拒否、未投入cancel、
二つ目のexport range token取得失敗後の回復も確認する。
`support/general_working_set.rs`は、返却済みの変更境界で登録buffer／画像をdeleteせず、
resizeとProgram追加を同じ利用側処理で両APIへ実行する。新classから省いた登録画像のstorage read/write、
以前compileしたread-only plan、index provenance、6-entry上限での反復変更と準備失敗後の内容を照合する。
export付きsubmissionを保持したときの交換拒否、buffer snapshotの最後のcloneを保持したままの交換と通常返却時の確保／解放ゼロも確認する。

```powershell
cargo test --locked --release -p sashiko-renderer-core --no-default-features --features vulkan,d3d12,development-validation,memory-profiling --test general_resources general_resources_strict -- --exact --ignored --nocapture --test-threads=1
cargo test --locked --release -p sashiko-renderer-core --no-default-features --features vulkan,d3d12,memory-profiling --test general_resources general_resources_shipping -- --exact --ignored --nocapture --test-threads=1
```

`GENERAL_RESOURCE_REPORT`はadapter/LUID、履歴9出力の全byteと最終native解放後の診断をJSONで出す。
shippingの`GENERAL_RESOURCE_MEMORY`は各APIの24 warm-up＋24変動frameで、build/compile/prepare/record/submit、
Fence完了、readback、delete、plan/result/metadata返却を測る。履歴値と不要枝を変更し、固定plan再生を条件にしない。
libtest内の専用逐次窓で測るため、下記の独立processによる混合描画／Computeメモリゲートも保持する。
timeout／Signal失敗と部分native作成失敗は既存CPU注入／実GPU寿命回帰を併用し、
実OOM・Device removalを意図的に発生させた証拠とは区別する。

<a id="owned-compute-acceptance"></a>

### owned Compute・画像copyの二API受入

[公開API試験](../../crates/sashiko-renderer-core/tests/compute_graph.rs)は利用側のレビュー済みshaderを
`ComputeProgramDescriptor`のunsafe境界で取り込み、同じowned Graphを二APIで逐次実行する。
3×7のRGBA16F入力にsampled画像＋Uniformの乗算、storage画像＋GPU生成StorageRead bufferの加算を
1/2/3段で接続し、画像copy、FP32 sample、buffer dispatchとcopyの計4出力・416 byteを
独立期待値とAPI間で全byte照合する。4×4 local sizeの端数guardを含み、effect列挙は増やさない。
固定sampleの座標がtexel境界に当たらない寸法を選び、画像の許容差を広げて通さない。

```powershell
cargo test --locked --release -p sashiko-renderer-core --test compute_graph --no-default-features --features vulkan,d3d12,development-validation,graph-diagnostics native::owned_compute_common_graph_strict -- --exact --ignored --nocapture --test-threads=1
cargo test --locked --release -p sashiko-renderer-core --test compute_graph --no-default-features --features vulkan,d3d12 native::owned_compute_common_graph_shipping -- --exact --ignored --nocapture --test-threads=1
```

`COMPUTE_GRAPH_REPORT`へ実adapter/LUID、入力条件、全readback byte、実効検証と最終native解放後の診断を出す。
strictとshippingは別processで実行する。未準備Programと画像class不足を独立に拒否し、
明示準備・容量変更後のcancel/再試行、公開plan/runtimeの先行破棄と結果消費までの保持も確認する。
CPU試験は未初期化storage read、重複subresource、短いABI、不正dispatch/format/extentと構築失敗後の再試行を扱う。
shader自体の安全性はunsafe取込のレビュー責務であり、reflectionだけで証明しない。

定常メモリはlibtest外の独立processで測定する。明示準備後に24 warm-up＋24変動frame、
3 in-flightと3保持結果、入力値・Uniform offset・storage dispatch数・ping-pong bindingを変える。
Graph構築から全readback消費・poll/retire・全plan/result/Graph slot返却までを連続窓に含める。

```powershell
cargo run --locked --release -p sashiko-renderer-core --no-default-features --features vulkan,memory-profiling --example frame_memory -- vulkan-compute
cargo run --locked --release -p sashiko-renderer-core --no-default-features --features d3d12,memory-profiling --example frame_memory -- d3d12-compute
```

標準構成の混合Raster／Computeを計測するときは、同じフレーム全体の計測基盤へ
上記の標準構成fixtureを接続した次のmodeを使います。各APIの24 warm-up＋24 frameで、
4出力・672 byte／frameを照合し、3 pendingと3保持結果、全slotの返却まで含めます。
Raster用draw／attachment、vertex/index bufferとprovenance、画像classと両種のProgramは準備境界で予約します。

```powershell
cargo run --locked --release -p sashiko-renderer-core --no-default-features --features vulkan,memory-profiling --example frame_memory -- vulkan-standard
cargo run --locked --release -p sashiko-renderer-core --no-default-features --features d3d12,memory-profiling --example frame_memory -- d3d12-standard
```

診断baselineは各backendのfeatureへ`development-validation,graph-diagnostics`を追加した別buildで、
同じmodeへ`--baseline`を渡す。製品／診断の各API計4 processを逐次実行し、startup/shutdownの正の対照と
`FRAME_MEMORY_REPORT`を保存する。確保回数とbyte、native作成/破棄・descriptor確保解放とresetを区別する。
時間計測の拡張、import/export、表示、Bistro/Bloomアルゴリズムをこの受入へ含めない。

## 実Vulkan試験

実Vulkan試験は`#[ignore]`で分離し、明示実行します。
`--features vulkan`はビルド対象を増やす指定で、GPU試験の実行指定とは別です。
正しさ試験をまとめて実行する場合は、関数名の`::vulkan_`で絞ります。
後述の性能試験はこのfilterに含まれません。

```sh
cargo test --locked -p sashiko-renderer-core --no-default-features --features vulkan,development-validation,graph-diagnostics ::vulkan_ -- --ignored --nocapture --test-threads=1
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

### RenderGraphとヘッドレス描画

Graphの宣言・解析と公開APIのintegration testは既定のCPU構成で実行できます。
producer・不要パス除去・Attachment・import/exportの契約は
[RenderGraphの設計](../design/rendergraph.md)を参照してください。
Vulkan環境では、ウィンドウなしで次の例を実行できます。

```powershell
cargo run --locked -p sashiko-renderer-core --features vulkan,development-validation,graph-diagnostics --example headless -- .tools/m2.ppm
```

出力先の親ディレクトリは事前に用意します。
例は近い赤い三角形と遠い緑の三角形を描き、invertと赤青入れ替えの2つのComputeを経て、
同じ形式の最終画像へコピーします。中央が黄色、背景が白であることを確認してPPMへ保存します。
PPMは表示用のsRGB変換をしていないlinear RGBのデータであり、画面出力の色管理は含みません。
資源の作成・投入は明示的で、CPUの完了待ちは最後のreadback境界にあります。

<a id="cpu-profile-workload"></a>

### CPUプロファイル用の固定負荷

[`profile_workload`](../../crates/sashiko-renderer-core/examples/profile_workload.rs)は、毎frameで変わる16 byteをuploadし、
`resources - 1`回のbuffer copyとreadback後に全16 byteを照合するWindowsのheadless例である。
1件のin-flightと準備済み容量を使い、build/compile/submit/poll/readback/返却を通す。
描画、present/VSync、GPU timestamp、native metricsは含まず、大きな転送帯域を測る負荷でもない。
`--api`は必須。`--resources`は1〜4096、`--warmup-ms`は0〜60000（0でも1 frame以上）、
`--duration-ms`は1〜60000で、時間指定では最後に開始したframeの完了まで待つ。
`--frames N`は指定件数すべてが期限内に完了しないと失敗する。

正しさ受入は物理GPUで次を逐次実行し、同じ指定の`--api d3d12`も別processで実行する。
Core/SyncValまたはDebug Layer/GBVの実効設定とwarning/errorゼロを、最終teardown後まで確認する。
4/256資源やpoll方針を比較する場合は同じ条件のshipping実行も確認する。

```powershell
cargo run --locked --profile profiling -p sashiko-renderer-core --no-default-features --features vulkan,d3d12,development-validation,graph-diagnostics --example profile_workload -- --api vulkan --resources 4 --seed 1 --warmup-ms 500 --duration-ms 3000 --poll yield
cargo run --locked --profile profiling -p sashiko-renderer-core --no-default-features --features vulkan,d3d12 --example profile_workload -- --api vulkan --resources 4 --seed 1 --warmup-ms 500 --duration-ms 3000 --poll yield
```

受入には`PROFILE_WORKLOAD_REPORT`と終了code 0、内容照合、`retained_graph_slots=0`、
最終native検証結果を使い、途中のDONEだけを成功としない。
CPU契約試験は`cargo test --locked -p sashiko-renderer-core --no-default-features --example profile_workload`でGPUなしに実行できる。
`memory-profiling`を追加する場合は別runとし、通常窓のallocation/native判定とsetup/shutdownの正の対照を確認する。
これらの正しさ・メモリ試験の時間をshipping性能へ流用しない。

`--poll busy|yield|sleep`（既定yield）はcallerの待ち方であり、busyは即時再poll、
yieldは`yield_now()`、sleepは未完了poll後に1 ms休止する。
CPU時間、Ready/Waiting、poll回数とwall timeが変わるため、同じ方針同士で比較する。
`elapsed_ns`と`phase_elapsed_ns`はwall time、process/main-threadの`*_100ns`はCPU時間である。
後者をmsへ換算するときは10000で割り、process全threadとcaller threadを区別する。
QPC経過秒は`qpc_elapsed_ticks / qpc_frequency`であり、boundaryはQPC→process CPU→thread CPUの順で読む。
これらの窓は完全に同時ではない。phase合計はpayload生成やloop/timer周辺の全費用を含まず、frame別p95も保存しない。
digest比較にはseedと測定frame数を揃える。時間指定で件数が違うrunではdigestの一致を期待しない。

保存EXE/PDB、WPRの所有session、外部marker付きhandshake、CSV/ソース行の分析は
[CPUプロファイル手順](tooling.md#cpu-profiling)に従う。
外部markerは測定QPC窓の外側を囲む。handshake負荷では`trace_clock.py`でheaderの実QPC/ETL時刻との厳密な対応を確認してから、
Sampledの実行標本とPreciseのRunning/Ready/Waiting、および未分類coverageを分ける。
Ready/Standby/DeferredReadyとWaitingを混同せず、Old/Newの同じprocess/thread identityで確認したself-switchだけはoff-CPU 0とする。
通常のwindowed captureは生存期間表を基にETL相対ROIを明示し、UTCからQPC窓を推定しない。
実ETLの欠落、stack/symbol帰属、最適化構成の費用比較は実収集後の別受入とする。

<a id="frame-loop"></a>

### フレーム進行・エンジン入力

持続的なフレームループには`VulkanRuntime::submit_frame`と`poll_frame`を使います。
容量回復やreadbackを明示的に待つ場合は`wait_frame(timeout)`で一件の完了結果を受け取ります。
返された`FrameSubmission`はメタデータで、dropしても待機しません。
Runtimeが未完了処理を所有し、容量不足なら利用側がpollや明示待機を選びます。
readback結果は`FrameCompletion::result`から消費でき、未読結果の保持中にも完了済みnative資源を再利用できます。
終了時の`drain_frames(timeout)`は結果を破棄するため、必要な結果を先に受け取ります。

`geometry::MeshRegistry`はGPUなしでメッシュの版を管理し、UnlitのView・Drawは `sashiko_renderer::unlit_forward` で捕捉します。
`VulkanEngine`は外部で作ったsnapshotを上限付きGPU mesh cacheと通常のGraphへ接続します。
`MeshCacheConfig`で常駐件数・論理byte数・未回収upload数を指定します。
`prepare_snapshot(snapshot, non_mesh_resources)`／`poll_uploads`で分割転送を進め、必要なら`wait_uploads(timeout)`で明示的に待ちます。
第二引数はmesh以外に残す登録容量で、buffer-onlyでは0、現行の標準color/depth構成では2を指定します。
`Ready`は全uploadの受理済みを表し、GPU完了を待たずにbuild、compile、submitへ進めます。
`unload_mesh`はCPU assetを削除せずGPU常駐を解除します。準備済みframeと未完了GPU参照は保護します。
利用側が構成する場合は`begin_graph()` → 必要な`connect_snapshot()` → 公開Graph操作 →
`compile()` → `submit_graph()`を使います。snapshotなし、複数View、任意の画像/buffer出力も同じ入口です。
一画像の出力と解放をまとめる場合は構成後に`select_output(image, readback)`を選び、`submit_frame`へ渡します。
`Graph::raster`で描画先とLoad/Storeを指定し、クロージャ内で接続済みのdraw列を登録します。
次の例では、接続leaseを構築後に返せます。

```rust,ignore
graph.raster("opaque", attachments, |raster| {
    raster.draw(&program, inputs.draws())
})?;
```

mesh inputは読み取り専用で直接exportできません。必要な変更・exportは独立bufferへのcopy後に行います。
`sashiko_renderer::unlit_forward::forward`の出力を標準または独自の後処理へ直接つなげます。
`sashiko_renderer::passes`のopaqueやdepth prepassを直接組み合わせることもできます。
出力handleから必要な消費方法を選び、同formatの別画像に保存する場合は`Graph::copy_image`を使います。
API例・Load/Store・任意depth・色／深度規約は[標準部品の契約](../design/rendergraph.md#公開標準パスと標準グラフ)を参照してください。
同geometry prepass後には`OpaqueDepth::Prepassed`とdepth Loadを使い、`mesh_prepassed`／`mesh_hdr_prepassed`も明示準備します。
`sashiko_renderer::unlit_forward::snapshot::build_vulkan`も公開forwardを呼び、
custom passは描画後の同寸linear RGBA8画像にGraph操作を追加する構築時の拡張です。
返した画像を直接exportし、無変換の全画面copyを追加しません。
出力はconsumer投入後に`release_output`で解放します。callerがimportした出力の登録は維持します。
任意shaderや生CommandBufferを受け取るAPIではありません。
詳細は[Graphの契約](../design/rendergraph.md#エンジン入力と標準描画)を参照してください。

入力接続・任意出力の実GPU受入は、snapshotなしCompute、同mesh旧新版の複数View、HDR全画素、
cancel、readonly、出力解放権限を通常経路とCore/SyncVal有効経路で確認します。

```powershell
cargo test --locked --release -p sashiko-renderer-core --test engine_graph --no-default-features --features vulkan,development-validation,graph-diagnostics caller_composed_engine_graph_strict -- --exact --ignored --nocapture --test-threads=1
cargo test --locked --release -p sashiko-renderer-core --test engine_graph --no-default-features --features vulkan caller_composed_engine_graph_shipping -- --exact --ignored --nocapture --test-threads=1
```

次の例は標準描画、外部custom処理、1×1の履歴chainを同じGraphで動かします。
`frame_loop`と`engine_workloads`の構築例は、`begin_graph`／`connect_snapshot`から
公開`graphs::forward`を呼び、合成後の`select_output`へ接続します。
入力leaseを構築後に返し、compile・submit・履歴・結果回収は同じ公開経路を使います。
`--churn`はCPU入力の更新・削除も行い、各frameの画像と履歴を照合します。
完了済みreadbackを一件遅らせて消費し、3件まで未回収frameを保持します。

```powershell
cargo run --locked -p sashiko-renderer-core --no-default-features --features vulkan,development-validation --example frame_loop -- --verify .tools/frame-verify.json --churn --ppm .tools/frame.ppm
```

JSONには終了後のValidation件数とCore／SyncValの実効設定を含めます。
情報が認識できない場合も成功扱いにしません。PPMはlinear RGBです。

<a id="windows-presentation"></a>

### Windows表示

`presentation`はWindow依存だけを追加します。Vulkan／D3D12も明示的に選び、両方をビルドした場合は
`--api vulkan|d3d12`で選択します。既定はコンパイル済みのVulkan、VulkanがなければD3D12です。
指定したAPIが未コンパイルなら失敗し、別APIへ黙ってfallbackしません。
winitのevent loop・camera・入力は例側が所有し、ライブラリは`Arc<Window>`をnative表示所有者とともに保持します。

```powershell
cargo run --locked -p sashiko-renderer-core --no-default-features --features vulkan,presentation --example windowed -- --api vulkan
cargo run --locked -p sashiko-renderer-core --no-default-features --features d3d12,presentation --example windowed -- --api d3d12
cargo run --locked -p sashiko-renderer-core --no-default-features --features vulkan,d3d12,presentation --example windowed -- --api d3d12 --vsync off --graph custom
```

Vulkanには同一Graphics＋Compute＋present queue、EXT swapchain maintenance1等の依存extension／feature、
SRGB_NONLINEARのRGBA8／BGRA8_SRGBとOPAQUE合成が必要です。D3D12はDIRECT queueとDXGIのflip-discardを使い、
RGBA8_UNORMのbackbufferへSRGB RTVを作成します。両方とも完全なlinear RGBA8の同寸法出力をsampleします。
不足は診断して停止し、別queue、拡張なしのWSI、無変換copyへfallbackしません。
VSyncは既定ON。`--vsync on|off`で起動時に選び、Vキーで変更します。
OFFにはVulkanのIMMEDIATE、DXGIのtearing capabilityが必要で、未対応要求は既存設定を維持して失敗します。
タイトルの要求設定と、prepare後に反映されるnative状態、実際のcompositor／driverの表示挙動は区別します。

同じ常駐cube meshと入力行列から、`--graph standard`は`sashiko_renderer::unlit_forward::forward`、
`--graph custom`は利用側のRasterと明示的な出力copyを使います。backendごとにGraphを実装し直しません。
操作はWASDで前後左右、Q／Eで上下、Shiftで加速、右mouseで視点操作、Tで固定巡回、Escで終了です。
focusを失うと押下状態とmouse操作を解除し、復帰直後の長いdeltaを移動へそのまま適用しません。
`--tour`は受理frame番号を基準に固定巡回します。CPU実行速度やVSyncが違っても同じ番号に同じcameraを渡せます。

```powershell
cargo run --locked --release -p sashiko-renderer-core --no-default-features --features vulkan,d3d12,presentation,development-validation,graph-diagnostics --example windowed -- --api vulkan --smoke
cargo run --locked --release -p sashiko-renderer-core --no-default-features --features vulkan,d3d12,presentation,development-validation,graph-diagnostics --example windowed -- --api d3d12 --smoke
cargo run --locked --release -p sashiko-renderer-core --no-default-features --features vulkan,d3d12,presentation --example windowed -- --api vulkan --tour --verify --frames 24
cargo run --locked --release -p sashiko-renderer-core --no-default-features --features vulkan,d3d12,presentation --example windowed -- --api d3d12 --tour --verify --frames 24
```

`--smoke`は最初の5 frameで共通入力controllerへ移動・相対mouse・右ボタン解放・focus lossを再生し、
そのcameraを実際のGraphへ渡します。移動／視点変更によるreadback digest変化と、
右ボタン解放後の移動継続・focus loss後の停止を`input_replay`欄で確認します。
続いて固定巡回、連続resize、VSync往復、最小化・復帰、pending所有を残した終了を有限回で実行します。
入力再生中は周囲のdesktop入力をcontrollerへ混在させません。OSへの入力注入やWindowsからwinitへの配送を検証する試験ではありません。
`--verify`はsource画像のreadback、独立したpalette／geometry条件とframeごとのdigestを記録します。
両APIへ同じextent・camera番号・Graph構成を渡して比較し、画面上のscanoutの全byte照合とは区別します。
受入時だけreadbackを追加するため、通常利用や時間測定へ無条件のreadbackを強制しません。
D3D12のこの例は1結果あたり16 MiBのreadback領域をRuntime作成時に準備します。
resize後のRGBA8画像が生byte数だけでこの上限を超える場合は、GPU作業の準備前に
要求byte／上限byte付きで失敗します。native row pitchの整列による超過も恒久的な上限不足として扱い、
同じ要求の再試行を続けません。大きな検証画像を使う場合は明示した準備容量を変更します。
終了後の`WINDOWED_ACCEPTANCE` JSONに設定、frame入力、内容digestとnative診断の実効状態を記録します。

表示前は`prepare`を進め、readyな`extent`でGraphを構築します。
`NotReady`ではpoll後に再試行します。容量不足は[回復方法](#runtime-capacity)を確認し、
この例がpoll・結果消費・owner返却で回復できるbackpressureだけを再試行します。
未準備や固定上限超過は理由を出して失敗し、通常経路にwait／増設を加えません。
`Suspended`はゼロextentからの復帰を待ちます。`RecreateRequired`は変更予約、`Recreated`は変更完了です。
表示submit受理後はsource登録を解除でき、native側がGPU使用終了まで所有します。
`NotReady`でも先のfullscreen submitがpendingの場合があり、未投入rollbackを推定しません。

Vulkanの旧世代はGPUとpresent fenceの両方で退役させ、明示した保守境界で回収します。
DXGIは同じHWNDの一つのchainを、GPU完了とbackbuffer参照解放後にresizeします。
DXGIのqueue markerやVulkanのpresent fenceを画面への表示完了とは数えません。
正常終了ではGraph結果・exportを返し、Presenterを明示closeし、最終native所有者を破棄してからmonitorを確認します。
strictはCore＋SyncVal／Debug Layer＋GBVの実効とwarning／error／corruption／callback失敗ゼロを要求します。
製品相当は追加診断をコンパイル除外し、native Disabledのreleaseを別processで実行します。

共通 API の複数ウィンドウ試験では、エンジン所有の HWND 二枚に同じ登録済み画像を表示します。
最初のウィンドウを閉じた後も、残りの Presenter と資源登録を使用できることを確認します。

```powershell
cargo test --locked --release -p sashiko-renderer-core --test host_windows --no-default-features --features vulkan,d3d12,presentation,development-validation -- --ignored --test-threads=1
cargo test --locked --release -p sashiko-renderer-core --test host_windows --no-default-features --features vulkan,d3d12,presentation -- --ignored --test-threads=1
```

既存のVulkan WSI受入も保持します。同じsourceの再present、取得後取消、旧世代の保持、
複数pendingと保持readback、全owner返却を通常frameのメモリ窓まで確認する試験です。

```powershell
cargo test --locked --release -p sashiko-renderer-core --lib --no-default-features --features vulkan,presentation,development-validation,graph-diagnostics vulkan_presentation_graph_resize_cancel_and_pending_shutdown -- --ignored --nocapture --test-threads=1
cargo test --locked --release -p sashiko-renderer-core --lib --no-default-features --features vulkan,presentation vulkan_presentation_graph_resize_cancel_and_pending_shutdown -- --ignored --nocapture --test-threads=1
```

D3D12の独立した実Window試験も、同じstrict／shippingの区別で実行します。
resize／VSync／最小化復帰／取得後cancel／pending終了と、18 source・36 presentの全lifecycleを扱います。
`memory-profiling`付きでは通常窓のRust確保・再確保・解放とnative作成・破棄を検査します。
各Window試験は独立processで起動します。winitは一processで複数event loopを作成できないため、両APIの試験を一つのfilterへまとめません。

```powershell
cargo test --locked --release -p sashiko-renderer-core --lib --no-default-features --features d3d12,presentation,development-validation,graph-diagnostics,memory-profiling d3d12_windowed_resize_cancel_pending_shutdown_and_frame_memory -- --ignored --nocapture --test-threads=1
cargo test --locked --release -p sashiko-renderer-core --lib --no-default-features --features d3d12,presentation,memory-profiling d3d12_windowed_resize_cancel_pending_shutdown_and_frame_memory -- --ignored --nocapture --test-threads=1
```

共通の契約は[Graphの表示接続](../design/rendergraph.md#windows-presentation)、nativeの根拠は
[Vulkan](../design/vulkan.md#windows-presentation)と[D3D12](../design/implementation-policy.md#d3d12-presentation)を参照します。

<a id="frame-memory"></a>

### 通常フレームのメモリ計測

`memory-profiling`は追加検証から独立した計測featureで、offでは専用counter moduleを除外する。
計測用実行ファイルは`CountingAllocator<System>`をglobal allocatorに設定し、alloc/alloc_zeroed/realloc/deallocを別計測する。
library本体はアプリのglobal allocatorを強制しない。native作成/破棄、descriptor、VMAへ渡したhost callback要求は別eventで扱う。
host callbackはVulkanへ転送されることもあるため、VMA/driverの起源は区別しない。
Vulkan memory eventはVMAの成功通知、D3D12はcommitted resourceの暗黙heap要求であり、物理割当数として加算しない。
counterがゼロでも未接続のhookを覆った証拠にはならないため、実行した経路と観測範囲を報告する。

```powershell
$env:CARGO_INCREMENTAL='0'
cargo run --locked --release -p sashiko-renderer-core --no-default-features --features memory-profiling --example frame_memory -- cpu
cargo run --locked --release -p sashiko-renderer-core --no-default-features --features d3d12,memory-profiling --example frame_memory -- d3d12
cargo run --locked --release -p sashiko-renderer-core --no-default-features --features vulkan,memory-profiling --example frame_memory -- vulkan
cargo run --locked --release -p sashiko-renderer-core --no-default-features --features vulkan,memory-profiling --example frame_memory -- vulkan-image
cargo test --locked --release -p sashiko-renderer-core --lib --no-default-features --features vulkan,presentation,memory-profiling vulkan_presentation_graph_resize_cancel_and_pending_shutdown -- --ignored --nocapture --test-threads=1
```

末尾の`--baseline`は値を記録しゼロassertを省く観測モードで、ゼロ目標の受入には使わない。
省略時は観測した確保/再確保/解放のどれかが残れば失敗する。Resetだけは既存枠の再利用として許可する。
CPU caseはsnapshot/build/compileと全owner返却までを含み、
draw数0/1/4/2、追加compute pass0/1/2、入力値を変え、3frame分のCPU ownerを保持してから全て返却する。
capacity/busy拒否と未構築snapshot/未compile Graphの取消も窓内に含む。
同じCPU実行の`owned_compute`欄は管理下buffer dispatchの論理build/compile/返却を別窓で測る。
consumer数1/2/3、binding offset 0/16/32、入力値、culled枝を変え、32-byte descriptorと16-byte実footprintを区別する。
3plan保持、未compile dispatch取消、全owner返却と次のslot resetも窓内に含める。native dispatch実行の測定ではない。
`attachment_subresources`欄は3mip×2layerの画像でcolor-only/depth-only/MRT、Load/Store、入力clear値とViewを変え、
取消・culled枝・3plan保持・全返却/resetまで別窓で測る。native MRT/subresource実行の証拠にはしない。
`owned_raster`欄は公開のProgram＋draw sliceで0/1/4/2 draw、共有する2mesh、任意depth、定数/clear値を変え、
呼出元sliceの変更・未compile取消・不要write・3plan保持・全返却/resetまで測る。CPU窓とnative画像/WSI窓を区別する。
`shader_bindings`欄は1/2/3段のsampled Rasterを各2drawで構築し、Uniform offset 0/16/32、
3mip×2layerからのView、drawごとのbindingと入力値を変え、sampled Computeの64-byte出力へ接続する。
短命配列・取消・culled write・3plan保持・全返却/resetを含める。新しいshaderのnative実行の計測ではない。
buffer caseは3pending/3held結果、可変copyと自己copy、長さ不一致後のreadback再試行、未消費結果破棄まで含み、
D3D12では未投入取消とsubmission破棄後のruntime pollも通す。
image caseは通常の公開Engine APIでRaster→0/1/2段のInvert→Compositeを毎frame構築し、
3pending/3held結果とprepared/compiled取消から全出力・結果返却まで測る。固定のplan再生専用経路ではない。

WSIは18sourceを毎回build/compileし、36present・18readbackと最終GPU/present退役を一つの通常epochに含める。
D3D12のWindow試験も18source・36present、三つのpending Graphと保持結果、sourceの返却までを連続窓に含める。
プロセス全体、同期的なwinit/Window呼出し、Rendererのsetup/normal/shutdownを別rowへ記録する。
shipping RendererにRust workerがないこの独立fixtureでは、通常epoch全体からcallerのRust要求だけを除く。
native/host callbackはcaller呼出し中も含むepoch全体を数え、rowの合計が重複する場合がある。
資源変更・resize/VSync/suspend/acquire取消とそれに紐づく明示回収は別境界とし、通常frameの結果返却をそこへ移さない。
回収可能な旧Swapchainも通常epoch中は保持したままゼロを確認し、その後`collect_retired_swapchains`で明示回収する。

追加検証ありの正しさは別runで確認する。standaloneはfeatureへ`development-validation,graph-diagnostics`を加え、
`--baseline`で追加診断の費用を観測する。WSIも同featureを加えると診断baselineとなり、終了後までCore/SyncValを要求する。
旧meshの遅延退役回帰は`vulkan_engine_retired_pending_mesh_is_collected_only_at_maintenance`を同様に明示実行する。
準備/終了は通常窓外の正の対照として記録し、hook未接続のゼロを受理しない。
callbackに現れないdriver/OS/native内部確保は未観測。D3D12 bootstrap/CPU shader blobとtest専用旧経路はnative hook対象外で、
通常の準備済みframe経路で呼ばないことを別に監査する。時間性能や物理VRAMの改善率はこのevent計測から推定しない。
現行結果と測定範囲は[通常フレームのメモリ計測](../verification/frame-memory.json)を参照する。

### 表示frameのCPU／GPU時間計測

`windowed --measure`は実際のWindowへ表示する同じworkloadを測ります。
既定は24 accepted frameのwarm-upと96 accepted frameの計測で、`--warmup N`／`--frames N`で変更できます。
`--measure`と`--smoke`は併用できません。通常の時間測定はreadbackなしで、明示的な`--verify`追加時だけ内容照合を含みます。
VSync OFFの処理時間とONのpacingを別process・同じAPI／Graph／camera条件で実行します。
strictの正しさ受入を別に行い、計装されたGPU実行を製品性能へ混ぜません。

```powershell
cargo run --locked --release -p sashiko-renderer-core --no-default-features --features vulkan,d3d12,presentation --example windowed -- --api vulkan --tour --measure --vsync off
cargo run --locked --release -p sashiko-renderer-core --no-default-features --features vulkan,d3d12,presentation --example windowed -- --api d3d12 --tour --measure --vsync off
cargo run --locked --release -p sashiko-renderer-core --no-default-features --features vulkan,d3d12,presentation --example windowed -- --api vulkan --tour --measure --vsync on
cargo run --locked --release -p sashiko-renderer-core --no-default-features --features vulkan,d3d12,presentation --example windowed -- --api d3d12 --tour --measure --vsync on
```

| 区間 | 測る対象 |
|---|---|
| CPU build／compile | 同じ公開Graphを可変camera定数で構築し、必要な依存・計画を生成する利用側の時間 |
| Graph CPU prepare／record／submit／retire | `SubmissionMetrics`。native具体化、命令記録、投入・受理処理、成功した完了回収を分ける |
| Graph GPU | Graph命令のtimestamp区間。表示用submitとpresentation engine／VSync／scanoutを含まない |
| 表示CPU acquire／prepare／record／submit／present／retire | `PresentationMetrics`。native acquire／present呼出しに含まれる費用を別値で記録する |
| 表示GPU | fullscreen sample・必要な資源遷移のtimestamp区間。画面への到達時間ではない |
| poll／結果返却／明示待機 | 利用側の反復poll、CPU消費・owner返却、capacity backpressureによる再試行や明示した待機の費用 |
| accepted Present間隔 | `Presented`を受理したCPU時刻の差。event loopとOS scheduling、VSync、待ちを含む |

runtimeとPresenterのprofilingは独立した`set_profiling(true)`で準備します。
Graph結果の`metrics()`とPresenterの`take_profile()`から値を受け取ります。
表示のpending＋未消費sample上限は`max_frames`で、未消費のまま埋めるとacquire前に容量不足になります。
表示submit済みsampleには、後続presentが拒否されたものも含まれます。sample数を成功Present数へ読み替えません。
queryは準備済み容量内で再利用し、実Fence完了後に結果を読むための毎frame waitを追加しません。
profilingなしでは専用query作成とphase timerを避けます。初期化・明示有効化・終了の費用は通常窓と分けます。
VulkanのmarkerはTOP_OF_PIPE／BOTTOM_OF_PIPE、D3D12はDIRECT queueのBOP timestampです。
いずれも記録命令の区間を測り、shaderだけの専有時間ではありません。
同じworkload／設定／markerを生sampleと中央値・個別sampleのp95に併記し、
小さいGPU区間の差だけから一般的なAPI性能の優劣を結論づけません。

出力は固定容量で収集し、終了時のJSONへ生sampleを記録します。
`samples[].graph`はGraphごとの値、`presentation_samples`は表示用GPU submitごとの独立した完了列です。
拒否されたPresentも後者へ含まれ得るため、両配列をframe番号で機械的に一対一対応させません。
`present_attempts`／`present_api_total_ns`はbackpressureを含む全ての外側present呼出しを数えます。
中央値・p95やrun間変動の集計は、この生sampleと測定窓を明示した比較記録に残します。
例のevent loopはbackpressure時のbusy loopを避ける有限の再試行間隔を持ち、そのfloorを報告します。
従ってaccepted Present間隔を、GPU単体の最大throughputや実画面のFPSとは説明しません。
瞬間値`1/dt`の単純平均や、GPU区間の逆数も表示FPSの代用にしません。
比較では解像度、warm-up／sample数、camera、構成、capacity、validation、driver、monitor条件とrun間変動を揃えます。
実行済みの値・未実行範囲は[検証状況](../verification/current.md)を参照します。

WindowsでOSが観測する表示まで調べる場合は[PresentMon](https://github.com/GameTechDev/PresentMon)を併用します。
[公式CSV定義](https://github.com/GameTechDev/PresentMon/blob/main/README-ConsoleApplication.md)の
`MsBetweenPresents`と`MsBetweenDisplayChange`／`DisplayedTime`を区別し、未表示frameも記録します。
RendererのGPU Fence／present fenceからscanout完了を推定せず、PresentMon未取得ならそのまま未確認とします。
headless測定は補助として維持し、upload／readback込みの小fixtureを実windowed通常frameの代表値へ読み替えません。
詳細は[Vulkan計測](../design/vulkan.md#graphの任意計測)と[D3D12表示・計測](../design/implementation-policy.md#d3d12-presentation)を参照します。

### 再現可能なフレーム基準測定

```powershell
cargo run --release --locked -p sashiko-renderer-core --no-default-features --features vulkan --example frame_loop -- --benchmark .tools/frame-baseline.json
```

既定は128×96、3 in-flight、warm-up 32 frame、測定128 frameを3 runです。
標準描画・custom処理・履歴と毎frameのreadbackを含むheadless workloadで、表示FPSではありません。
`--frames`、`--warmup`、`--runs`、`--no-history`、`--no-custom`で条件を変えられます。
固定負荷ではwarm-up後にnative作成数が増え続けないことも確認します。

性能modeはreleaseを要求し、Validationを明示的にDisabledへ設定して任意timestampを有効にします。
要求mode、観測したLayer起動状態、環境の上書きを記録し、起動報告の不在だけで外部Layerがないと断定しません。
CPU各phase、GPU Graph区間、明示的な完了待機、観測latency、各runの中央値／p95と生sample、
資源作成・再利用、stagingのhigh-water、VMA Allocation／blockをJSONへ保存します。
schema3の`completion_wait`は、pollと明示的なFence待機を含む壁時計時間です。
主指標の`cpu_active`はbuild開始からsubmit復帰までで、容量回復の待機を含みません。
CPU asset更新・snapshot・mesh upload準備とupload待機は別項目です。
`cpu_caller_active`はそれらのCPU処理と`cpu_active`を足し、明示待機を除いた利用側の費用を表します。
startup uploadは測定loopの前に完了させ、そのbyte数とCPU・待機時間を別に記録します。
以前のschema1にあるpoll費用の数値と、名前だけを合わせて比較しないでください。

<a id="engine-workloads"></a>

### 大メッシュ・アセット入れ替え・複数View

`engine_workloads`は400×400 cellのgrid（160,801頂点・960,000 index、約8.34 MB）と
256 KiBのstagingを使い、大きな単体meshと合計入力を分割転送します。
32個のCPU assetを保持した入れ替え、3 pending frame中の更新・削除、共有meshからの2 View先行build、
異なる行列・寸法・材質値とCompute、同一snapshotの独立rendererへの入力も確認します。
各画像は決定的な全RGBA期待値と照合し、検証modeはnative所有者の破棄後までCore／SyncValを要求します。

```powershell
cargo run --release --locked -p sashiko-renderer-core --no-default-features --features vulkan,development-validation --example engine_workloads -- --verify .tools/engine-workloads-verify.json
cargo run --release --locked -p sashiko-renderer-core --no-default-features --features vulkan --example engine_workloads -- --benchmark .tools/engine-workloads-benchmark.json
cargo test --locked -p sashiko-renderer-core --no-default-features --features vulkan,development-validation --test engine_workloads vulkan_ -- --ignored --nocapture --test-threads=1
```

性能modeはValidation無効を明示し、32 warm-up＋128測定frameを3 run実行します。
build／compile／prepare／record／submit／retire、GPU Graph区間、CPU中央値／p95と観測latency、
資源作成・staging・VMA統計を記録します。GPU時間はreadbackを含み、表示時間やFPSではありません。
性能測定とGPU試験は同時に実行しません。全resident vertex/index byte照合、upload受理直後の描画、
import出力の解放権限、少ない物理枠での常駐入れ替えは内部のignored GPU試験にも含めます。
メモリ量はprocess全体のVRAMやRAM使用量とは区別します。
通常submitの計測は既定無効で、`set_profiling(true)`を明示した場合だけ採取します。
正しさのverify runと性能runを分け、条件・commit・clock／負荷等の制約も比較時に揃えます。

<a id="raster-draw-scaling"></a>

### 描画数を変えた比較計測

`raster_draw_scaling`は同じWindows GPU上でVulkanとD3D12の直接描画を比較する。
512×512に1,024、10,000、50,000個の可視オブジェクトを配置し、1個につき1件のindexed drawを記録する。
`--words 0`は共有Uniformと常駐の頂点・インデックス範囲を使う。`--words 20`は標準Meshとオブジェクトごとの
変換・色を使い、事前生成した二つの入力を交互に描く。両条件とも2 in-flightで、既定は120 warmup＋600測定frame。

```powershell
cargo build --locked --release -p sashiko-renderer-core --no-default-features --features vulkan,d3d12 --example raster_draw_scaling
target/release/examples/raster_draw_scaling.exe --api vulkan --draws 50000 --words 0 --report .tools/raster-50000-vulkan.json
target/release/examples/raster_draw_scaling.exe --api d3d12 --draws 50000 --words 20 --report .tools/raster-50000-d3d12.json
```

`active_ns`は枠が空いた後のGraph構築開始からsubmit復帰までを直接測る。`interval_ns`は回収を含むframe開始間隔で、
先頭の0を集計から外す。`wall_ns`は最後の回収も含む。`--profile`を付けた別runでnative prepare・recordとGPU時間も収集する。
画像比較は別runで`--warmup 0 --frames 0 --capture <stem>`を指定し、同じAPI・入力・出力番号のRGBAをバイト照合する。
実機検証には`development-validation`を有効にした実行物を使う。性能比較の実行物は同じfeatureで作り、
PコアとHighQoSを読戻した独立processを変更前後交互に起動する。build・画像照合・他のGPU試験が重ならないようにする。
[計測結果](../verification/current.md#raster-constant-storage)を参照する。

同じ総描画数で CPU のコマンド生成を比較するには `--passes`、`--batches-per-pass`、`--topology independent|chain`、`--workers` を指定する。各パスが専用の同寸法画像を描く。最大32パスで、2カラー出力・2フレーム分の画像データは128 MiBになる。`chain` は同じ GPU の描画内容にパス間の明示的な順序依存を加える。前の画像をサンプリングする GPU 処理は含まない。パス数を変えると画像数とクリア回数も変わるため、ジョブの効果はパス数・バッチ数が等しい条件で比較する。

`--workers 0` は Graph 所有者が直接 `RasterPass::draw` に入力を渡す。`1` は同じ描画列を予約済み領域に逐次生成し、`2` 以上はホストを含む指定人数でバッチごとに生成する。いずれも同じバッチ順に Graph へ登録し、バックエンドが通常の経路で記録・投入する。`packet_prepare_ns` は生成開始からジョブの合流までで、`build_ns` と `active_ns` に含まれる。単一パス・単一バッチでは参加人数を増やしても生成するジョブは1個である。

以下は全5万 draw で、単一パス、独立4パス、明示依存付き4パス、1パス×4バッチ、4パス×4バッチを測る例。出力には未使用のディレクトリ名を渡す。ランナーはプロセス単位で予熱し、反復ごとに参加人数の実行順を入れ替える。`summary.json` に最終回収までの完了フレーム/秒、フレーム開始間隔の p95・p99、各 CPU 区間と別走の GPU 計時を残す。

```powershell
python tools/renderer-scenes/raster_job_benchmark.py --exe target/release/examples/raster_draw_scaling.exe --output .tools/raster-jobs --draws 50000 --words 0 --workers 0 1 4 --warmup 120 --frames 600 --repeats 3 --profile both
```

`--conditions in-pass` でパス内4バッチに絞れる。`--profile off` は完了フレーム/秒を測る通常走、`--profile on` はネイティブ記録・GPU 時間を取得する診断走に使う。`build_excluding_packet_p50_ms` は各フレームの `build_ns` から命令の別配列への生成・合流にかかった `packet_prepare_ns` を差し引いて集計した値である。直接登録の値にはインラインの命令生成も含まれる。[5万描画の比較結果](../verification/current.md#raster-jobs)

ネイティブ記録を比較する場合は `--workers 0` で入力生成を固定し、`--native-workers 0 1 4` を変える。0 は元の直接記録、1 は同じ二次バッファ／バンドルを1人で記録、4 は呼び出し元を含む4人で記録する。一つの実行ファイルを使い、画像照合、通常の FPS、`--profile on` の記録・GPU 計時は別プロセスで測る。`memory-profiling` 付き実行ファイルでは `memory` に計測ループの Rust 確保・ゼロ初期化確保・再確保・解放、VMA host callback、native event の各累積差分も出る。配列の順序は `measurement::{RustEvent, HostEvent, NativeEvent}` に対応する。[ネイティブ記録の比較結果](../verification/current.md#native-raster-jobs)

```powershell
python tools/renderer-scenes/raster_job_benchmark.py --exe target/release/examples/raster_draw_scaling.exe --output .tools/native-record --workers 0 --native-workers 0 1 4 --conditions in-pass independent mixed --draws 50000 --words 0 20 --warmup 80 --frames 360 --repeats 3 --profile both
```

<a id="standard-renderer-jobs"></a>

**標準パイプライン・視錐台カリングを含む計測**。Windows上の物理GPUで、通常のRelease実行ファイルを使う。以下の各ランナーは空いている出力先を必要とする。1実行ごとにCPU affinityを設定して、同じカメラ軌道、材質、描画先、同時進行2フレームで最終GPU回収までを測る。

```powershell
cargo build --release --locked -p sashiko-renderer --features vulkan,d3d12,presentation --example standard_scene_scaling --example packed_visibility_scaling
python tools/renderer-scenes/standard_job_benchmark.py --exe target/release/examples/standard_scene_scaling.exe --output .tools/standard-jobs --objects 50000 --visible 10 50 100 --stages serial graph scene both native scene-graph-native auto --warmup 120 --frames 600 --repeats 3
python tools/renderer-scenes/standard_job_benchmark.py --exe target/release/examples/standard_scene_scaling.exe --output .tools/standard-present --present --extent 1920x1080 --objects 50000 --visible 50 --stages serial auto
python tools/renderer-scenes/packed_visibility_benchmark.py --exe target/release/examples/packed_visibility_scaling.exe --output .tools/packed-jobs --objects 50000 --visible 10 50 100 --moving 0 500 --groups 1 --variants cpu cpu-all4 ranges4 direct-all gpu
```

`--workloads textured blend shadow-static shadow-dynamic` で、テクスチャ付き複数材質、半透明混在、4カスケードの再利用・更新を選べる。`--capture` はGPU計測後に2フレームを再生し、SDR・HDR・深度と該当する影マップのハッシュを直列経路と突き合わせる。`development-validation` を有効にした別ビルドでは `--strict --capture`、`memory-profiling` ビルドでは `--memory` を使う。各ビルドの実行ファイルは別名で保持する。`scheduler-metrics` ビルドの `--profile` では、シーン・Graph・バックエンドの区間、GPUタイムスタンプ、合流時に実際に待機した時間を調べられる。時間が包含関係にある区間は加算しない。

同一シーンを備えた旧バイナリと比較するには、標準ランナーに `--serial-exe PATH --stages serial auto` を渡す。反復ごとに両実装の実行順を入れ替え、双方の実行物のSHA-256を記録する。

低レベルの `raster_job_benchmark.py` は `--graph-workers 0 1 4` で従来の登録、最終領域への1人の一括登録、4人の一括登録を比べられる。GPUカリングの比較では同じジオメトリと材質順を使い、個別のCPU直接描画に加え、可視の連続範囲をまとめる直接描画、グループ全体を一括描画できる場合の直接描画も含める。[実機の採用結果](../verification/current.md#standard-renderer-jobs)

`frame_loop`は`--draws 0..1024`、`--layout overlap|grid`、`--extent WIDTHxHEIGHT`、
`--mesh-layout shared|distinct`を受け付けます。
指定しなければ従来の1 draw・128×96・history/custom有効の負荷です。
`overlap`は同じ三角形を重ね、最初のdrawより後は深度で除外されるため、主にdraw管理の負担を調べます。
`grid`は共有meshを格子へ配置し、占有・空きcellの内部画素を検査します。
gridの各cellは検査に必要な8×8 pixel以上を要求します。
`shared`は一つのmeshを繰り返し使い、`distinct`は同じ三角形データを持つ独立mesh資源をdrawごとに使います。
distinctはasset churnと併用できません。meshは開始時に登録し、warm-upでuploadしてから計測します。
必要なasset容量と物理entry容量は実行前に計算してJSONへ記録し、実行中に無制限に増設しません。
history/customなしの1024 distinct drawでは、物理entry容量は2057です。

```powershell
cargo run --release --locked -p sashiko-renderer-core --no-default-features --features vulkan --example frame_loop -- --benchmark .tools/draw-scaling.json --draws 512 --layout grid --extent 512x512 --no-history --no-custom --rgba .tools/draw-scaling.rgba
cargo run --release --locked -p sashiko-renderer-core --no-default-features --features vulkan --example frame_loop -- --benchmark .tools/distinct-scaling.json --draws 512 --layout grid --extent 512x512 --mesh-layout distinct --no-history --no-custom --rgba .tools/distinct-scaling.rgba
```

比較するdraw数は0、1、8、32、128、512、1024、管理負担用はoverlap・64×64、
可視描画用はgrid・512×512、meshはshared/distinctを使います。asset churnを無効にし、永続mesh cacheを使って
毎frameのmesh uploadを測定へ混ぜません。入力draw list生成はframe計測の外です。
CPU activeは容量不足時のpollを済ませてから、build開始〜submit復帰を直接測ります。
phaseごとの中央値の合計ではありません。GPU値はreadbackを含むGraph全体で、Rasterだけの時間やFPSではありません。

正しさは`--verify`でCore/SyncValを有効にし、同じ負荷と`--rgba`で比較します。
`.rgba`はheaderなしのpacked RGBA8で、JSONの幅・高さと組にして全byteを照合します。
静的負荷では各frameの全RGBA byteも照合し、ファイル出力は計測loopの外で行います。
性能はrelease・明示Validation無効、3 in-flight、warm-up 32＋測定128 frameを3 runとし、
比較対象の実行順を交互にします。GPUを使う別の試験・計測を同時に走らせません。
run間の揺れ、p95、GPU時間、作成数、cacheとVMAの保持量を併記し、通常CIに壁時計の閾値を設けません。

### Graphのlowering診断

論理的な依存・用途は`graph-diagnostics`構成の`CompiledGraph::dump()`で確認します。
Vulkanで実際に記録する同期や物理資源との対応が必要な場合は、通常の`submit`の代わりに
次のAPIを使います。診断は要求した投入だけで生成され、通常経路には文字列構築を追加しません。

```rust,ignore
let (mut submission, lowering) = runtime.submit_with_diagnostics(&compiled)?;
eprintln!("{lowering}");
// 結果の利用・資源回収には、別途submissionの実完了を確認する。
```

出力は用途、物理keyと世代、native handle、importの初期予定状態、barrierの実際のscope／layout、
生成理由、最終予定状態を含みます。投入が失敗した場合は既存の型付きエラーを返します。
診断文字列をGPU完了や外部資源の所有権の証明には使いません。

Shaderのソースと生成済みbinaryは`crates/sashiko-renderer-core/shaders/`にあります。
通常のRustビルドはチェックイン済みSPIR-Vを使います。
Windowsでは固定SDKの同じcompiler・validatorを使って再生成できます。

```powershell
& crates/sashiko-renderer-core/shaders/regenerate.ps1 -SdkRoot C:/VulkanSDK/1.4.341.1 -Verify
```

`-Verify`は一時ファイルへコンパイルし、`spirv-val --target-env vulkan1.3`の成功と
チェックイン済みbinaryとのSHA-256一致を要求します。意図的なShader変更では
`-Verify`を外して再生成し、ソース・binary・ABI・GPU出力を一緒に確認します。
生成条件はshaderc v2026.1、SPIRV-Tools v2026.1、GLSL 450 core、entry point `main`、
`--target-env=vulkan1.3 --target-spv=spv1.3 -O -Werror`で、include・macro・debug情報はありません。
スクリプトはtoolのversionも検査します。ローカルLinuxでの同一生成は未確認です。

Deviceを使う試験にはGraphics＋Computeキュー、`timelineSemaphore`、
`synchronization2`、`dynamicRendering`が必要です。
image試験にはRGBA8_UNORMのoptimal 2D imageのtransfer source／destination対応も必要です。
必要条件の不足は理由を報告して失敗し、黙ってskipしません。
CPU型のソフトウェア実装は通常の候補になれますが、実GPU受入の成功には数えません。

### Validationの判定条件

`development-validation`構成の正しさ受入は`ValidationMode::Required`で、`VK_LAYER_KHRONOS_validation`と
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

fragment sampleからCompute sampleへ渡る画像の同期は、releaseの`--test vulkan_layout_dependency`を
`-- --ignored --test-threads=1`で実行します。厳格構成では専用の`vk_layer_settings.txt`に
`khronos_validation.syncval_shader_accesses_heuristic = true`を設定し、そのディレクトリを当該processの
`VK_LAYER_SETTINGS_PATH`に指定します。製品構成ではこの設定と診断featureを外して同じ画素を確認します。

<a id="resource-reuse-measurement"></a>

## CPU資源探索の変更前後比較

Python 3.11以降と固定Rust toolchainを使い、GPUなしで候補の取得・返却を比較できます。
ビルド完了後に測定を始めるscriptで、測定中は他のビルド・性能試験を止めてください。

```powershell
python -B tools/measure_resource_reuse.py --baseline 483e304 --runs 3 --output .tools/resource-reuse/comparison.json
```

Vulkanは指定commitのprivate cacheをGitから取得し、現在と同じharnessを`rustc -O`でcompileします。
D3D12はreleaseの`d3d12_reuse` benchmark内の旧scan referenceとproduction indexを使います。
前後の順序を交互にし、各caseを最低100ms warm-upして31 sampleを保存します。
各sampleは複数cycleの平均時間で、中央値やp95を個々のframeのlatencyと読み替えません。
原本commit、実装commit、harness／helper hash、実際のwarm-up数、全sampleと
alloc／alloc_zeroed／realloc／deallocを出力します。各通常測定窓の4eventはゼロを要求します。

Vulkanのkeyは整数、D3D12のnative所有境界は`Arc<()>`であり、GPUやdriverを起動しません。
したがって資源探索と所有取得・返却のCPU費用だけを比較し、native prepare全体、GPU時間、
FPSの改善を主張しません。登録済みprefix、多数候補、多class、lease保持候補を別caseで確認します。
単独で現在のharnessを試す場合は`cargo bench --locked -p sashiko-renderer-core --no-default-features --bench vulkan_resource_cache`、
`--bench d3d12_reuse`を使えます。後者の`-- --reference`は旧scanの比較専用です。
通常CIへ固定の時間閾値は加えません。

## CPU転送計画の性能測定（Criterion）

同じbufferへの多数の部分転送について、同期判定と初期化済み範囲の管理を測定します。
専用targetは本番のRustソースを組み込み、内部APIを公開せずGPUも初期化しません。
ビルドにはCriterionのC helper用コンパイラーが必要ですが、SDKやVMA用C++コンパイラーは不要です。

```sh
cargo bench --locked -p sashiko-renderer-core --no-default-features --bench transfer_planning
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
cargo bench --locked -p sashiko-renderer-core --no-default-features --bench transfer_planning -- --save-baseline planning-main
cargo bench --locked -p sashiko-renderer-core --no-default-features --bench transfer_planning -- --baseline planning-main
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

製品でLayerを無効にする場合と性能試験では、`ValidationMode::Disabled`を明示指定します。
この転送性能試験はその設定を選びます。現行はdebug_utilsと診断経路を維持し、
releaseビルドだけで自動的に検証を外しません。内部検証除去とは別の設定です。
外部loader設定によるLayerの強制注入も確認してください。
VVLの起動状態を受信した場合は測定を失敗させ、ログ非表示だけを検証なしとは扱いません。

releaseと`RENDERER_RUN_PERFORMANCE=1`を指定します。

```powershell
$rendererPreviousPerformance = $env:RENDERER_RUN_PERFORMANCE
try {
    $env:RENDERER_RUN_PERFORMANCE = '1'
    cargo test --release --locked -p sashiko-renderer-core --no-default-features --features vulkan performance_transfer_measurements -- --ignored --nocapture --test-threads=1
} finally {
    $env:RENDERER_RUN_PERFORMANCE = $rendererPreviousPerformance
}
```

Linuxでは次のように実行します。

```sh
RENDERER_RUN_PERFORMANCE=1 cargo test --release --locked -p sashiko-renderer-core --no-default-features --features vulkan performance_transfer_measurements -- --ignored --nocapture --test-threads=1
```

固定負荷のwarm-up後、CPUのwrite・record・submit・wait（回収を含む）・readbackを分けて測定します。
中央値・p95、資源の作成・再利用回数を報告し、パターン生成と全byte照合は計測から外します。
GPU timestampはqueueの対応と精度を確認し、実Fence完了後に取得します。
非対応環境はGPU時間を未取得と報告します。

native資源の毎回作成と再利用の比較に加え、複数submitと単一バッチの比較があります。
後者も個別側の全件をsubmitしてから待ち、GPU時間は測定しません。
これらを複数フレームのthroughputや描画FPSの結果とは扱いません。
正しさ試験の検証有効結果と性能結果は別に記録します。

<a id="live-inspector"></a>

## 実行中のゲームを NDJSON で調べる

ヘッドレスの「扉と鍵」を次のコマンドで起動する。初期状態は tick 0、一時停止中である。端末からは JSON を一行ずつ入力する。LLM やテストは同じ子プロセスの標準入力と標準出力を開いたまま使う。ログと Cargo の出力は標準エラーへ出る。標準入力の EOF でセッションを終了する。

```sh
cargo run --quiet --release --locked -p inspector-door --features live-inspector -- --inspect-stdio
```

最初に以下の三行を送る。各応答には要求と同じ文字列の `id` があり、成功時は `ok:true` と `result`、失敗時は `ok:false` と `error.code` が返る。この接続の版・セッション・ワールド・対応操作は `hello` に含まれる。検索結果の ID は、その起動でのみ有効な不透明な文字列として扱う。

```jsonl
{"id":"h","op":"hello"}
{"id":"types","op":"schema","kind":"Door"}
{"id":"doors","op":"find","kind":"Door","limit":1}
```

次の二行で解錠拒否を待ち、更新を再開する。待機は `registered` を返した時点で有効になる。待機中も同じ接続で質問できる。`w` の終了通知には、イベントの `self`（扉）、`actor`（プレイヤー）、判定に使った `facts`、その tick 終了時のスナップショットが入る。小例では最初の拒否で止まる。

```jsonl
{"id":"w","op":"wait","event":"unlock_denied","pause":true,"capture":{"self":["state"],"actor":["held_key"]},"timeout_ms":30000}
{"id":"play","op":"resume"}
```

拒否時に提示した鍵は `result.event.facts.presented_key`。tick 終了時に持っていた鍵は、次の要求例の `SNAPSHOT` と `PLAYER` を通知の `result.snapshot.id` と `result.event.actor` に置き換えて読む。その後の `step` はちょうど 1 tick 進める。描画や OS のデバッガを必要としない。[外部クライアントの実装例](../../tools/inspect/verify.py)

```jsonl
{"id":"key","op":"read","snapshot":"SNAPSHOT","object":"PLAYER","fields":["held_key"]}
{"id":"next","op":"step"}
```

| 操作 | v1 の要求と寿命 |
|---|---|
| `schema` / `find` | `kind` を省略すると全型が対象。`find` は 1〜128 件（既定 32 件）。次ページには前の `revision` と `next_offset` を `offset` として渡す。版が変われば `STALE_PAGE` を返す。 |
| `read` / `snapshot` | 一括採取は `{"id":"s","op":"snapshot","objects":{"OBJECT":["field"]},"ttl_ms":60000}`。投影は合計 64 項目まで、同時保持は 16 件、寿命は採取から 1〜60,000 ms。未収録は `NOT_CAPTURED`、期限切れは `SNAPSHOT_EXPIRED`、消滅した個体への現在値要求は `OBJECT_GONE`。 |
| `wait` | `event` はスキーマのイベント ID。`object` は任意の発生元フィルタ。`capture` の役割は `self` と任意の `actor`。`pause` は既定 false。期限は 1〜86,400,000 ms（既定 60,000）。`TIMEOUT` は実時間で判定する。 |
| `watch` / `cancel` | `watch` は指定した種類のイベントを継続購読する。過去分は `since` に最後に読んだ sequence の文字列を渡す。過去 64 イベントを越えたら `gap` と欠落区間、その後に保持中の記録を返す。待機と購読は合計 16 件。`cancel` に `subscription` を渡すと取消応答と元の購読 ID の終了通知が返る。 |
| `pause` / `step` / `resume` | 停止中も要求処理は続く。`step` は停止中のみ使用できる。v1 は単一ワールドの読み取り専用で、確定版は完了した tick に一致する。 |

要求 ID はセッション中の未完了操作間で一意にする。tick・revision・sequence は十進文字列。1 行は改行を除き 16 KiB まで、入出力のキューは各 32 件とする。遅い読者がいる間はゲームを進め、カーソルの欠落を通知する。スナップショット枠の不足は `CAPACITY` を返し、期限内の既存の投影は保持する。イベント発生後に当事者が消滅しても判定時の事実を保存し、終了時に取得できなかった当事者を `missing` に示す。

### ビルドと受け入れ

`live-inspector` は最適化や Renderer の開発用検証とは独立した Cargo feature である。通常版は同じコミット・アセットでこの feature を外す。検査版でも `--inspect-stdio` を省略した実行はセッションを開かない。ゲームを組み込むときは `sashiko::inspect::World` に検索・スキーマ・読取・単一 tick の更新を実装する。個体 ID はゲームがセッション内で再割当を防ぎ、判断理由はルールが実際にそれを決めた箇所で記録する。[ゲームとの接続例](../../examples/inspector-door/src/inspection.rs)・[観測 API](../../crates/sashiko-inspect/src/lib.rs)

```sh
cargo test --locked -p sashiko-inspect
cargo build --release --locked -p inspector-door --features live-inspector
python -B tools/inspect/verify.py target/release/inspector-door
cargo run --quiet --release --locked -p inspector-door --features memory-profiling -- --measure 100000
cargo run --quiet --release --locked -p inspector-door --features live-inspector,memory-profiling -- --measure 100000
cargo run --quiet --release --locked -p inspector-door --features live-inspector,memory-profiling -- --measure-observed 100000
```

Windows の検証スクリプトには `target/release/inspector-door.exe` を渡す。独立にビルドした通常版があれば `--shipping` にそのパスを渡すと、通常版の機能除外と同一 tick の結果も照合する。[計測結果と測定範囲](../verification/current.md#live-inspector)

## 診断ログと生成物

ライブラリは`tracing`へログを発行し、global subscriberや出力先を設定しません。
実Vulkan試験の既定filterは`renderer=info,sashiko_renderer_core::vulkan::validation=warn`です。
`--nocapture`で成功時も表示できます。詳細な転送ログは`debug`で有効になります。

```powershell
$rendererPreviousRustLog = $env:RUST_LOG
try {
    $env:RUST_LOG = 'renderer=trace'
    cargo test --locked -p sashiko-renderer-core --no-default-features --features vulkan,development-validation,graph-diagnostics ::vulkan_ -- --ignored --nocapture --test-threads=1
} finally {
    $env:RUST_LOG = $rendererPreviousRustLog
}
```

表示filterにかかわらず診断の全重要度を受信・集計します。
問題メッセージはID・本文・対象操作とともに確認し、終了中の診断も含めて判定します。
callbackから呼ばれるsubscriberはVulkan APIへ再入してはいけません。
保持上限やエラー時の所有契約は[Vulkan基盤の設計](../design/vulkan.md)を参照してください。

`target/` と外部ツールのローカル配置先 `.tools/` は Git 管理対象外です。
現在の設定・結果は対応する文書を更新し、過去版の記録はGit履歴から参照します。

### 課題別の生成物の保管と削除

| 置き場所 | 用途 | 削除できる時期 |
|---|---|---|
| `.tools/<ツール名>/` | RenderDoc など、ローカルで必要な外部ツール本体と同梱依存物 | ツールが不要になり、本体や補助プロセスが使い終えたとき。再利用時は必要な版を再取得する |
| `target/` | Cargo の共有ビルドキャッシュ | ビルドや実行で使っていないとき。通常は再ビルドを速くするために残す |
| 作業中のチェックアウトの `build/tasks/<Beads ID>/` | 新しい課題の変換結果、撮影タイル、診断ログ、試作バイナリ | 課題が完了し、必要な記録を保存し、他の課題や実行中のジョブも使い終えたとき |
| Git または長期保管先 | 手書きの再現スクリプト、設計判断、後から参照する実測データ | 通常の容量整理の対象外 |

外部ツールで採取するキャプチャやログ、その場限りの検証スクリプトも
課題の `build/tasks/<Beads ID>/` に置きます。繰り返し使う自作スクリプトは
`tools/` で Git 管理します。過去の手順にある `.tools/` 配下の検証出力例は、
新しい課題ではその課題の出力先に読み替えます。

中断時は課題ディレクトリをそのまま残します。再開に時間のかかる処理があれば、
その出力先、再開位置、まだ動いているジョブだけを Beads に一行記録します。
突然の中断で記録がなくても、未完了課題の出力は保持します。

完了時は検証結果を既存の文書にまとめます。元のトレースなども必要なら、
長期保管先に移し、Beads と参照元の文書からその保管先にリンクします。
一時フォルダを実測記録の唯一の参照先にしません。
容量が必要になったときに完了済みの課題フォルダから整理すればよく、
ファイルごとの台帳、セッションごとの掃除、既存データの一括移行は不要です。
