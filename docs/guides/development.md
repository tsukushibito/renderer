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
| M4のD3D12準備試験 | Windows x64、D3D12対応実GPU、Windows Graphics ToolsのDebug Layer／GBV、Vulkan比較環境 |
| 実行環境・シェーダーの調査 | Vulkan SDK 1.4.341.1のvulkaninfo、glslc、spirv-val |
| 依存・GPU内容の調査 | cargo-deny 0.20.2、RenderDoc 1.47。[ツールガイド](tooling.md)を参照 |
| 課題管理 | Beads v1.3.4。[導入・同期手順](../../.beads/README.md)を参照 |
| Bistro素材・比較画像の共有 | Git LFS。[素材一覧とライセンス](../../assets/bistro/README.md)を参照 |

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
Windowsには`presentation` featureの全target check・Clippy・CPU試験も追加しています。
各backendは製品／開発検証＋dumpの両構成を確認し、Vulkanはrelease両構成と独立診断featureも確認します。
これは表示の実GPU受入をCIで実行するという意味ではありません。
依存更新ではlockfile差分と両feature構成を確認します。

<a id="public-api-comparison"></a>

## 公開APIのCPU比較試作

[設計比較と第一案](../design/implementation-policy.md#public-api-comparison)の利用コードは
[graph_api_comparison](../../crates/renderer/examples/graph_api_comparison.rs)で再現する。
GPU・Vulkan SDK・Bistro素材の取得は不要で、workspaceの通常のRust開発環境を使う。

```powershell
cargo run --locked -p renderer --example graph_api_comparison --no-default-features
cargo test --locked -p renderer --example graph_api_comparison --no-default-features
cargo test --locked -p renderer --example graph_api_comparison --release --no-default-features
```

命令所有案とframe内遅延callback案を同じ代表入力で組み立て、正規化した資源用途・命令の一致と
構造的な件数を確認する。試作の型はRendererの公開APIではなく、GPUのrecord／submitも行わない。
exportは次frameの内容・同期・寿命の実機検証ではない。件数を処理時間・確保量の測定へ読み替えない。
通常の`cargo test --workspace`はexample内の試験を実行しないため、上記の明示指定を使う。
CIもCPU構成でこの試験と実行を行い、その他の構成では全target check／Clippyを適用する。

<a id="foundation-comparison"></a>

## 採用基盤を比較する独立試作

[基盤比較と採用理由](../design/implementation-policy.md#foundation-selection)の外部案は
[tools/foundation-probe](../../tools/foundation-probe/Cargo.toml)で再現する。
wgpu 30.0.2の独立workspaceであり、Rendererの依存・featureへ追加しない。
Windowsの物理GPUとVulkan/D3D12の両runtimeが必要。backend欠落やsoftware adapterは失敗し、skip/fallbackしない。
GPU作業は他のprobeと並行実行しない。

```powershell
cargo fmt --manifest-path tools/foundation-probe/Cargo.toml -- --check
cargo check --locked --manifest-path tools/foundation-probe/Cargo.toml --all-targets
cargo clippy --locked --manifest-path tools/foundation-probe/Cargo.toml --all-targets -- -D warnings
cargo test --locked --manifest-path tools/foundation-probe/Cargo.toml
cargo run --locked --release --manifest-path tools/foundation-probe/Cargo.toml
cargo run --locked --release --manifest-path tools/foundation-probe/Cargo.toml -- --native-validation
cargo deny --manifest-path tools/foundation-probe/Cargo.toml --locked --config tools/foundation-probe/deny.toml check
```

各runはVulkan→D3D12を逐次実行し、8×4のdepth-only/color-only Raster→利用側Compute→readbackと
期待128byte／二APIの一致を確認する。通常testは実GPU試験をignoreし、明示実行は
`cargo test --locked --release --manifest-path tools/foundation-probe/Cargo.toml -- --ignored`を使う。
CIは両OSのbuild/Clippy、format、依存監査を担当し、実GPUを実行しない。
独立監査設定はfoldhash 0.2.0のZlibを対象packageだけ許可し、ライセンス本文の条件を確認した。
Rendererのruntime allowlistとadvisory除外は変更していない。

flags空と`--native-validation`（DEBUG＋VALIDATION要求）を区別し、
両方でpipelineなしdrawをcoreが拒否する負の対照も実行する。無効命令はsubmitしない。
この要求はnative検証の実効や破棄後の診断ゼロを証明しない。結果の該当値は未観測のnullであり、
既存のCore/SyncVal・D3D12 Debug Layer/GBVの厳格受入を代替しない。
CPUの全`Study::scenario`は従来の比較試作で確認し、この実機fixtureでは多段Bloom/MRT/mip/履歴等を実行しない。
Windows表示・自由camera・resize/最小化・pending終了とnative失敗注入もB1以降の受入へ残す。
source/lock/shader/exe hashと4runの原byteは[共有結果](../verification/foundation-comparison.json)を参照する。
試作の計時・性能比較・全Bistro常駐の合否は取得しない。

<a id="管理下uniform-programの作成と受入"></a>

## 管理下資源Programの作成と受入

`renderer-7ft.2.1.2`の公開入口は`program::ManagedProgram::uniform_readback()`と
両runtimeの`create_program`、`create_managed_buffer`、`create_bindings`である。
`.2.1.4`は`ManagedProgram::sampled_readback()`と、`create_managed_image`、
`create_image_view`、`create_sampler`を加える。RGBA8_UNORM 2Dのmip/layer/sampleは各1、
Viewはfull color範囲、Samplerは`NearestClamp`に限定する。
`.2.1.5`は`ImageFormat::Rgba16Float`と`ManagedProgram::sampled_float_readback()`を加える。
同じ2D/full View/nearest samplerでRGBA16FをFP32としてsampleし、全RGBAをbit列で返す。
`.2.1.6`は`ImageFormat::D32Float`と`ImageViewDescriptor.usage`を加える。
Viewの用途は既定の`ImageViewUsage::Sampled`と、D32専用の`DepthAttachment`を区別する。
D32はattachment/sample両用途を持ち、aspectはformatから導出する。
`ImageDescriptor::preferred_depth_clear`はD32の予定clear値を指定できる。色は`None`とし、
D32の`Some`は[0,1]の有限・非subnormal値を使う。この指定で画像内容は初期化されない。
[`program` moduleの例](../../crates/renderer/src/program.rs)は両APIの公開型でRustdocコンパイルする。
Uniformのbyte更新は新しいbuffer/bindingを作り、旧参照の内容を変更しない。
Storageの版/範囲は保持するが本文はGPU更新可能で、作成直後は未初期化である。
binding集合は全slotを満たす必要があり、入力順は自由で、成功後はProgram/資源/範囲を所有する。
`Binding`はslotと`BindingResource::{Buffer, ImageView, Sampler}`を持つ。
画像の作成/参照保持は内容初期化を意味せず、作成時の暗黙upload/submit/waitはない。
一般Graph dispatch/画像uploadと他format/mip/layer/filter、comparison sampler/PCFは後続へ残す。

SDKなしのABI契約回帰は次を使う。生成条件を更新した場合は[shader再生成](tooling.md)も実行する。

```powershell
./crates/renderer/shaders/test-managed-abi.ps1
./crates/renderer/shaders/regenerate-dxil.ps1 -SdkRoot C:/VulkanSDK/1.4.341.1 -Verify
./scripts/regenerate-d3d12-bindings.ps1 -Verify
```

実GPU受入は以下を逐次実行する。strictにはCore/SyncValまたはDebug Layer/GBVが必要で、
前提不足をskip扱いにしない。共通入力からStorageのoffset 256へ書いた旧版/新版の内容、
参照保持、未投入cancel/busy拒否/完了後reuseと最終native解放を確認する。
Storage出力は公開APIで作成し、private fixtureは各shaderが初期化する16または64 byteだけをreadbackする。
sampled fixtureは同じ2x2非対称RGBA8をnearest/clampの4座標で読み、入力16 byteへpackして照合する。
同じ受入でRGBA16Fの32-byte入力も読み、4 texelの全RGBAを64-byteのFP32 bit列へ返す。
halfで正確に表せる負値・小数・1超の値を使い、既存RGBA8と新RGBA16Fの旧版/新版をそれぞれ照合する。
D32はdepth attachment全域を0.25/0.75へclearし、同じfloat shaderでsampleした4 texelのR成分を
FP32 bit列で照合する。旧版reuseは再clearしない。attachment Viewをsampled bindingへ渡す入力は拒否する。
ImageとStorageの両方を記録/pendingから排他保護し、View/Image/sampler/heapの最終解放も確認する。

```powershell
cargo test --locked --release -p renderer --lib --no-default-features --features vulkan,development-validation vulkan_managed_storage_creation_and_versions -- --ignored --nocapture --test-threads=1
cargo test --locked --release -p renderer --lib --no-default-features --features d3d12,development-validation d3d12_managed_storage_strict -- --ignored --nocapture --test-threads=1
cargo test --locked --release -p renderer --lib --no-default-features --features vulkan vulkan_managed_storage_creation_and_versions -- --ignored --nocapture --test-threads=1
cargo test --locked --release -p renderer --lib --no-default-features --features d3d12 d3d12_managed_storage_shipping -- --ignored --nocapture --test-threads=1

cargo test --locked --release -p renderer --lib --no-default-features --features vulkan,development-validation vulkan_managed_sampled_ -- --ignored --nocapture --test-threads=1
cargo test --locked --release -p renderer --lib --no-default-features --features d3d12,development-validation d3d12_managed_sampled_strict -- --ignored --nocapture --test-threads=1
cargo test --locked --release -p renderer --lib --no-default-features --features vulkan vulkan_managed_sampled_ -- --ignored --nocapture --test-threads=1
cargo test --locked --release -p renderer --lib --no-default-features --features d3d12 d3d12_managed_sampled_shipping -- --ignored --nocapture --test-threads=1
```

製品構成の成功をnative診断の実効確認へ流用しない。結果・限界は
[現在の検証状況](../verification/current.md#managed-uniform)へ記録する。

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
cargo tree --locked -p renderer -e features --no-default-features --features vulkan
```

製品経路の実GPU内容・CPU保持の確認は、native受入と別runで実行します。

```sh
cargo test --locked -p renderer --release --lib --no-default-features --features vulkan ::vulkan_shipping_ -- --ignored --nocapture --test-threads=1
```

これはnative検証なしの内容照合です。offのmonitorは診断を収集しないため、
件数0や起動報告の不在を外部Layer注入がなかった証明には使いません。

feature統合後の実構成を確認し、
最適化・Renderer内の追加検証・native検証・profilingをそれぞれ記録します。
`--all-features`だけでは検証無効構成の確認になりません。
コード・依存関係・生成物や走査／allocation／保持byte等から、除去を主張する費用を確認します。
実GPUの正しさ受入はCore＋SyncVal等を維持し、GPU-AV／GBVによる追加診断と製品相当の性能測定を分けます。
変更に応じた確認範囲は[検証方針の受入基準](validation-policy.md#verification)を参照してください。

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
cargo check --locked --workspace --all-targets --no-default-features --features d3d12
cargo clippy --locked --workspace --all-targets --no-default-features --features d3d12 -- -D warnings
cargo test --locked --workspace --no-default-features --features d3d12
cargo check --locked --workspace --all-targets --no-default-features --features vulkan,d3d12
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
cargo test --locked -p renderer --lib --no-default-features --features vulkan,d3d12,development-validation ::d3d12_preparation_ -- --ignored --nocapture --test-threads=1
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

[公開APIの二API試験](../../crates/renderer/tests/d3d12_graph.rs)は、一度compileした同じGraphを
実際のVulkan／D3D12 Deviceで逐次実行します。vendor／device IDと有効なLUIDを照合し、
部分範囲・複数copy・同一bufferの非重複copyを含むreadbackの全byteを期待値と両API間で比較します。
この段階はbufferの受入であり、画像・shaderのGBV実行や性能の優位を確認したとは扱いません。

厳格検証と製品構成の内容照合は、設定を切り替える同一processに混ぜず、別processで実行します。
D3D12のDebug Layer／GBV設定は最初のDevice作成前に固定する必要があります。
厳格検証ではD3D12のDebug Layer／GBVとVulkan Core＋SyncValの実効を確認し、
試験が所有する全native参照の解放後までwarning／error、corruption、callback失敗等がゼロであることを要求します。
前提不足、LUID照合不能、診断の実効未確認、試験・レポート不在は失敗です。

```powershell
cargo test --locked -p renderer --release --test d3d12_graph --no-default-features --features vulkan,d3d12,development-validation m4_buffer_shared_graph_strict -- --exact --ignored --nocapture --test-threads=1
cargo test --locked -p renderer --release --test d3d12_graph --no-default-features --features vulkan,d3d12 m4_buffer_shared_graph_shipping -- --exact --ignored --nocapture --test-threads=1
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

## 実Vulkan試験

実Vulkan試験は`#[ignore]`で分離し、明示実行します。
`--features vulkan`はビルド対象を増やす指定で、GPU試験の実行指定とは別です。
正しさ試験をまとめて実行する場合は、関数名の`::vulkan_`で絞ります。
後述の性能試験はこのfilterに含まれません。

```sh
cargo test --locked -p renderer --no-default-features --features vulkan,development-validation,graph-diagnostics ::vulkan_ -- --ignored --nocapture --test-threads=1
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
cargo run --locked -p renderer --features vulkan,development-validation,graph-diagnostics --example headless -- .tools/m2.ppm
```

出力先の親ディレクトリは事前に用意します。
例は近い赤い三角形と遠い緑の三角形を描き、invertと赤青入れ替えの2つのComputeを経て、
別の最終画像へ合成します。中央が黄色、背景が白であることを確認してPPMへ保存します。
PPMは表示用のsRGB変換をしていないlinear RGBのデータであり、画面出力の色管理は含みません。
資源の作成・投入は明示的で、CPUの完了待ちは最後のreadback境界にあります。

### フレーム進行・エンジン入力

持続的なフレームループには`VulkanRuntime::submit_frame`と`poll_frame`を使います。
容量回復やreadbackを明示的に待つ場合は`wait_frame(timeout)`で一件の完了結果を受け取ります。
返された`FrameSubmission`はメタデータで、dropしても待機しません。
Runtimeが未完了処理を所有し、容量不足なら利用側がpollや明示待機を選びます。
readback結果は`FrameCompletion::result`から消費でき、未読結果の保持中にも完了済みnative資源を再利用できます。
終了時の`drain_frames(timeout)`は結果を破棄するため、必要な結果を先に受け取ります。

`engine::Engine`はGPUなしでmesh/materialとowned View/draw snapshotを扱い、
`VulkanEngine`は外部で作ったsnapshotを上限付きGPU mesh cacheと通常のGraphへ接続します。
`MeshCacheConfig`で常駐件数・論理byte数・未回収upload数を指定します。
`prepare_snapshot`／`poll_uploads`で分割転送を進め、必要なら`wait_uploads(timeout)`で明示的に待ちます。
`Ready`は全uploadの受理済みを表し、GPU完了を待たずにbuild、compile、submitへ進めます。
`unload_mesh`はCPU assetを削除せずGPU常駐を解除します。準備済みframeと未完了GPU参照は保護します。
custom passは描画後の同寸linear RGBA8画像にGraph操作を追加する構築時の拡張です。
返した画像を直接exportし、無変換の全画面copyを追加しません。
出力はconsumer投入後に`release_output`で解放します。callerがimportした出力の登録は維持します。
任意shaderや生CommandBufferを受け取るAPIではありません。
詳細は[Graphの契約](../design/rendergraph.md#エンジン入力と標準描画)を参照してください。

次の例は標準描画、外部custom処理、1×1の履歴chainを同じGraphで動かします。
`--churn`はCPU入力の更新・削除も行い、各frameの画像と履歴を照合します。
完了済みreadbackを一件遅らせて消費し、3件まで未回収frameを保持します。

```powershell
cargo run --locked -p renderer --no-default-features --features vulkan,development-validation --example frame_loop -- --verify .tools/frame-verify.json --churn --ppm .tools/frame.ppm
```

JSONには終了後のValidation件数とCore／SyncValの実効設定を含めます。
情報が認識できない場合も成功扱いにしません。PPMはlinear RGBです。

### Windows表示

Windowsでは`presentation` featureを使います。winitのイベントループは例側が所有し、
ライブラリは`Arc<Window>`をSurfaceとともに保持します。
Vulkan 1.3基盤に加え、同一Graphics＋Compute＋present queue、swapchainと
EXT swapchain maintenance1等の依存拡張・feature、SRGB_NONLINEARのRGBA8/BGRA8_SRGB、
OPAQUE合成への対応が必要です。不足は診断して停止し、別queueや拡張なしのfallbackは提供しません。

```powershell
cargo run --locked -p renderer --no-default-features --features presentation --example windowed
cargo run --locked -p renderer --no-default-features --features presentation --example windowed -- --vsync off
cargo run --locked -p renderer --no-default-features --features presentation,development-validation --example windowed -- --smoke
```

表示例もCargo featureに従います。製品構成はLayerを要求せず、`development-validation`構成は
Core＋SyncValの実効を起動時に確認し、native所有者の破棄後まで診断を確認します。

既定はVSync ON（FIFO）、`--vsync on|off`で起動時に選択し、Vキーで実行中に切り替えます。
OFFはIMMEDIATE対応が必要で、未対応の起動要求は失敗します。Vキーの未対応要求は診断して設定を維持します。
タイトルは要求設定を表示し、実際の切り替えは次のSwapchain再作成で反映します。
`--smoke`は12回のPresent要求、VSync往復切り替え、resize、最小化・復帰、処理未完了時の終了を有限回で実行します。
両モードの対応が必要です。描画Fenceとpresent fenceを回収し、終了後までCore/SyncValの警告・エラーゼロを要求します。
headless例と共通のScene、mesh/material/Viewとcustom処理を使います。
自動受入には、次のignored実ウィンドウ試験も明示実行します。

```powershell
cargo test --locked -p renderer --no-default-features --features presentation,development-validation,graph-diagnostics ::vulkan_ -- --ignored --nocapture --test-threads=1
```

表示前に`Presenter::prepare`を呼び、現在の`extent`で出力Graphを構築します。
`NotReady`はpoll後に再試行、`Suspended`はゼロextentの復帰を待ちます。
`RecreateRequired`は再作成の予約であり、`prepare`を進めるまでextentは更新されません。
`Recreated`は再作成完了を表し、寸法を取得し直します。
出力は表示用submitへ渡した後に登録解除できます。
表示用submit受理後はpresent失敗でもsourceの予定同期状態が進み得ます。
読み取りだけでは内容revisionを変えず、後続consumerの同期は投入時の状態から計画します。
正常終了はframeの回収とPresenterの明示closeを行い、最後に保持したValidationMonitorで
native所有者の破棄後までwarning／error／callback失敗ゼロを確認します。

### 表示FPSの計測方針

`windowed`には現在FPS集計を実装していません。追加する場合は、単調増加時計に対する
`PresentOutcome::Presented`の成功件数を約1秒の窓で割り、**Present要求FPS**として表示します。
`NotReady`／`Suspended`／`RecreateRequired`／`Recreated`を成功件数に数えず、
VSync変更・resize・最小化からの復帰で窓をリセットします。
窓内の待ち時間は経過時間へ含め、成功Present間隔の中央値・p95・p99も併記します。
瞬間値`1/dt`の単純平均や、GPU timestamp区間の逆数で表示FPSを算出しません。

WindowsでOSが観測する表示まで調べる場合は[PresentMon](https://github.com/GameTechDev/PresentMon)を併用します。
[公式CSV定義](https://github.com/GameTechDev/PresentMon/blob/main/README-ConsoleApplication.md)の
`MsBetweenPresents`と`MsBetweenDisplayChange`／`DisplayedTime`を区別し、未表示frameも記録します。
present maintenance fenceのsignalを実画面の表示完了と数えることはできません。
GPU Graph区間とCPU build/compile/record/submitは既存の計測で別に調べます。

最大throughputの比較ではrelease・明示Validation無効・固定解像度・warm-up後の一定時間を複数run測り、
VSync・実行枠・画像数・driver・モニターの条件を揃えます。正しさは別runでCore/SyncValを有効にして確認します。
現exampleの`WaitUntil(now + 4ms)`は意図的なイベントループ待機なので、上限を調べる計測モードでは外す必要があります。
現在のexampleをそのまま最大FPSのbenchmarkとは扱いません。表示FPSやPresentMonの実測値は未取得です。
詳細な同期・寿命の根拠は[Vulkan設計](../design/vulkan.md#windows-presentation)を参照してください。

### 再現可能なフレーム基準測定

```powershell
cargo run --release --locked -p renderer --no-default-features --features vulkan --example frame_loop -- --benchmark .tools/frame-baseline.json
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

### 大メッシュ・アセット入れ替え・複数View

`engine_workloads`は400×400 cellのgrid（160,801頂点・960,000 index、約8.34 MB）と
256 KiBのstagingを使い、大きな単体meshと合計入力を分割転送します。
32個のCPU assetを保持した入れ替え、3 pending frame中の更新・削除、共有meshからの2 View先行build、
異なる行列・寸法・材質値とCompute、同一snapshotの独立rendererへの入力も確認します。
各画像は決定的な全RGBA期待値と照合し、検証modeはnative所有者の破棄後までCore／SyncValを要求します。

```powershell
cargo run --release --locked -p renderer --no-default-features --features vulkan,development-validation --example engine_workloads -- --verify .tools/engine-workloads-verify.json
cargo run --release --locked -p renderer --no-default-features --features vulkan --example engine_workloads -- --benchmark .tools/engine-workloads-benchmark.json
cargo test --locked -p renderer --no-default-features --features vulkan,development-validation --test engine_workloads vulkan_ -- --ignored --nocapture --test-threads=1
```

性能modeはValidation無効を明示し、32 warm-up＋128測定frameを3 run実行します。
build／compile／prepare／record／submit／retire、GPU Graph区間、CPU中央値／p95と観測latency、
資源作成・staging・VMA統計を記録します。GPU時間はreadbackを含み、表示時間やFPSではありません。
性能測定とGPU試験は同時に実行しません。全resident vertex/index byte照合、upload受理直後の描画、
import出力の解放権限、少ない物理枠での常駐入れ替えは内部のignored GPU試験にも含めます。
メモリ量はprocess全体のVRAMやRAM使用量とは区別します。
通常submitの計測は既定無効で、`set_profiling(true)`を明示した場合だけ採取します。
正しさのverify runと性能runを分け、条件・commit・clock／負荷等の制約も比較時に揃えます。

### 描画数を変えた比較計測

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
cargo run --release --locked -p renderer --no-default-features --features vulkan --example frame_loop -- --benchmark .tools/draw-scaling.json --draws 512 --layout grid --extent 512x512 --no-history --no-custom --rgba .tools/draw-scaling.rgba
cargo run --release --locked -p renderer --no-default-features --features vulkan --example frame_loop -- --benchmark .tools/distinct-scaling.json --draws 512 --layout grid --extent 512x512 --mesh-layout distinct --no-history --no-custom --rgba .tools/distinct-scaling.rgba
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

Shaderのソースと生成済みbinaryは`crates/renderer/shaders/`にあります。
通常のRustビルドはチェックイン済みSPIR-Vを使います。
Windowsでは固定SDKの同じcompiler・validatorを使って再生成できます。

```powershell
& crates/renderer/shaders/regenerate.ps1 -SdkRoot C:/VulkanSDK/1.4.341.1 -Verify
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
    cargo test --locked -p renderer --no-default-features --features vulkan,development-validation,graph-diagnostics ::vulkan_ -- --ignored --nocapture --test-threads=1
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
