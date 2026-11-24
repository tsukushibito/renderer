# 依存・SPIR-V・CPU/GPU調査ツール

通常のビルドと試験は[開発手順](development.md)を参照する。
以下のコマンドはリポジトリのルートで実行する。
ローカル配布物と確認用ファイルはGit対象外の`.tools/`へ配置する。
現在の導入状態と確認範囲は[最新の検証結果](../verification/current.md)を参照する。
検証機能の使い分け、GPU timestamp、vendor別profilerと測定条件は
[Vulkanベストプラクティス](vulkan-best-practices.md#measurement)を参照する。

## cargo-deny

採用版は**0.20.2**。この環境では公式Windows x64配布物を
`.tools/cargo-deny/bin/cargo-deny.exe`へ配置している。

```powershell
& ./.tools/cargo-deny/bin/cargo-deny.exe --version
& ./.tools/cargo-deny/bin/cargo-deny.exe --locked --color never check
```

別環境では[公式リリース](https://github.com/EmbarkStudios/cargo-deny/releases/tag/0.20.2)の
対応する配布物を取得するか、次のように同じ版をソースから導入する。

```sh
cargo install --locked --version 0.20.2 --root .tools/cargo-deny cargo-deny
```

検査条件は[deny.toml](../../deny.toml)で管理する。
Windows MSVCとLinux GNU、全feature、開発用・ビルド用依存を対象に、
advisory、ライセンス、重複・wildcard、取得元を確認する。
CIの`dependencies` jobも同じ設定を使い、公式Action v2.1.1を完全SHA、
実行用Rustを1.99.0へ固定する。

依存ライセンスの許可一覧はApache-2.0、ISC、MIT、Unicode-3.0である。
`licenses.private.ignore`は`publish = false`のworkspace crateを扱うための設定で、
第三者依存の検査除外や、このプロジェクトの配布ライセンス指定ではない。
advisoryの個別除外はない。設定の意味は[公式ライセンス設定](https://embarkstudios.github.io/cargo-deny/checks/licenses/cfg.html)を参照する。
任意のTracy計装に使う`tracy-client-sys 0.30.1`だけは、同梱Tracy/libbacktraceのBSD-3-Clauseを
`deny.toml`のpackage/version限定例外で許可する。宣言は`(MIT OR Apache-2.0) AND BSD-3-Clause`であり、
一般のBSD-3-Clause許可へ広げない。通常構成の依存へTracyを追加する理由にも使わない。

重複版はwarningとして確認する。現在の`syn` 2と3は、Criterionの異なる間接依存が
互換性の異なるmajor版を要求しているため併存する。新しい重複が出たら依存経路と必要性を確認する。
この扱いは、実Vulkan受入試験のValidation warning／errorゼロという条件とは別である。

## SPIR-Vの検証

Vulkan SDK **1.4.341.1**の`glslc`と`spirv-val`を使う。
シェーダーの生成・変換後に、[検証スクリプト](../../scripts/validate-spirv.ps1)へ
検証対象のファイルを明示して渡す。

```powershell
& C:/VulkanSDK/1.4.341.1/Bin/glslc.exe --target-env=vulkan1.3 --target-spv=spv1.3 -fshader-stage=compute example.comp -o target/example.spv
& ./scripts/validate-spirv.ps1 -Path target/example.spv -Validator C:/VulkanSDK/1.4.341.1/Bin/spirv-val.exe
```

複数ファイルには`-Path @('target/a.spv', 'target/b.spv')`を指定する。
スクリプトは`--target-env vulkan1.3`で検証し、緩和オプションを使わない。
ファイル・toolの不足、ディレクトリの指定、validatorの失敗は非ゼロで終了する。
ビルド処理から呼ぶ場合は、その終了コードでビルドも失敗させる。

`-Validator`を省略すると、`VULKAN_SDK`、既存のWindows固定SDK、PATHの順に選ぶ。
明示したtoolや`VULKAN_SDK`が無効な場合は、その原因を報告して失敗する。
複数SDKが入っている環境では、上の例のように版の分かるパスを渡す。

管理下のShaderと生成済みSPIR-Vは`crates/renderer-core/shaders/`にある。
固定compilerでの再生成・validator・binary一致確認は[開発手順](development.md#rendergraphとヘッドレス描画)を参照する。
通常のCPU試験やCPUだけのCriterion測定にVulkan SDKを要求しない。
SDKの構成は[固定版のSPIR-V toolchain](https://vulkan.lunarg.com/doc/view/1.4.341.1/windows/spirv_toolchain.html)を参照する。

<a id="runtime-shader-import"></a>

## 実行時に外部シェーダーを読み込む

アプリのアセット層で、選んだバックエンドのバイナリと、同じビルドで確認したABIを読み込む。
Vulkan向けはlittle-endian SPIR-V 1.3以下、D3D12向けはDXIL SM6.0で、entryは`main`を使う。
次の例は頂点IDで全画面三角形を作り、RGBA8のcolor attachmentへ書くVulkan用のRaster Programを
読み込む。アプリ側でこの契約を満たすと確認したアセットを渡す。

```rust
use renderer_core::{
    ImageFormat,
    program::{
        BlendState, DepthState, ManagedRasterProgram, RasterParameterLayout,
        RasterProgramDescriptor, RasterShaderStages, ShaderBinaries, ShaderStages,
    },
};
use std::{error::Error, fs, path::Path};

/// # Safety
/// The files must contain reviewed fullscreen shaders that match this
/// descriptor and all of ManagedRasterProgram's trusted shader requirements.
unsafe fn load_fullscreen(
    vertex_path: &Path,
    fragment_path: &Path,
) -> Result<ManagedRasterProgram, Box<dyn Error>> {
    let shaders = ShaderBinaries::vulkan(RasterShaderStages {
        vertex: fs::read(vertex_path)?,
        fragment: Some(fs::read(fragment_path)?),
    });
    // SAFETY: The caller establishes the complete shader and ABI contract.
    Ok(unsafe {
        ManagedRasterProgram::from_trusted_owned_descriptor(RasterProgramDescriptor {
            entry: "main",
            vertex_stride: 0,
            vertex_attributes: &[],
            parameters: RasterParameterLayout { size: 0, stages: ShaderStages::EMPTY },
            color_outputs: &[ImageFormat::Rgba8Unorm],
            bindings: &[],
            binding_bytes: &[],
            sampling_pairs: &[],
            blend: BlendState::Replace,
            depth: DepthState::Disabled,
            shaders,
        })?
    })
}
```

D3D12用Rasterでは`ShaderBinaries::d3d12(D3D12RasterShaders { stages, inline_constants })`を使う。
Computeでは`ManagedProgram::from_trusted_owned_descriptor`へ、該当バイナリを含む
`ComputeProgramDescriptor`と、そのシェーダーに対応する`DispatchContract`を渡す。
両APIのバイナリを一つのProgramで使う場合は、共通ABIを持つ両形式を`ShaderBinaries::both`へ渡す。
取込後に元のメタデータを手放せる。

埋め込みのプラグインには `renderer_core::program::embedded_shader_binaries!(spirv, dxil)` を使える。
ホストアプリの `renderer-core` で有効なバックエンドに応じて対応する式だけを展開する。GPU機能が無い構成ではABIだけを使ってGraphを構築できる。
パイプライン側はバックエンド別のCargo featureや条件コンパイルを用意せずに、同じ `ViewPipeline` を実装できる。

描画・dispatchの予約には以下の `Preparation` を使う。直接 `WorkingSet` を組む場合はRasterを `WorkingSet::raster`、
Computeを `WorkingSet::compute` に渡す。標準の表示プリセットは `renderer::passes::output` が提供する。
単独で使う場合は `tone_map_neutral_pipeline()` と `SdrOutput::display(fxaa).pipeline()` または `SdrOutput::capture(fxaa).pipeline()` を予約する。
起動時の `Runtime::new` または変更時の `Runtime::prepare_working_set` がGPUパイプラインを準備する。
Graphでは同じProgramで `draw_fullscreen`、`draw`、`dispatch` する。差し替えた版を使うGraphは再構築する。

バイナリの検査は取込時に行う。コードの妥当性、ABIとの一致、範囲や出力初期化などの挙動は
アセットを渡す側が保証する。[取込と寿命の設計](../design/implementation-policy.md#external-shader-assets)を参照する。

<a id="rust-pipeline"></a>

## Rustで描画パイプラインを組み立てる

`renderer`の標準レシピは、フレームの`Graph`にパスを追加し、後段へ渡す画像ハンドルを返す。
利用側は標準パスと独自のRaster、Compute、copyを同じGraphに登録する。
独自のShader Programは[外部取込](#runtime-shader-import)の契約に従って用意し、標準部品と同じように準備する。

1. 起動時に同時フレーム数で `Preparation` を作る。標準部品の `prepare` へ描画寸法・方式・最大入力数を渡すと、Programと一時資源が登録される。
2. 独自のRasterパスは `reserve_raster_pass` でアタッチメント、Programと最大描画数を宣言する。メッシュを取り込むなら `Meshes::reserve_imports` も呼ぶ。バインディング、定数、アクセス、ラベルの必要量はこれらの宣言から計算する。Computeには `reserve_dispatch`、コピーには `reserve_copy`、読み戻しには `reserve_readback` がある。
3. `preparation.runtime_limits(base)?` で得た値を `Runtime::new` に、同じ値の `.graph` を `GraphPool::new` に渡す。CPUでGraphだけを組み立てるときは `graph_capacity(base)?` を使う。追加アップロード、部分範囲の分割、同時に保持する読み戻し結果、バックエンドの転送領域にはアプリの上限を指定する。
4. 各フレームで `pool.begin()?` からGraphを受け取る。レシピから得たHDRを独自パスへ渡し、保存には `SdrOutput::capture(fxaa)` と `CaptureSdr::readback`、Windows表示には `SdrOutput::display(fxaa)` を使う。HDRだけを使う場合はSDR出力を省ける。

たとえば独自のメッシュ描画パスに必要な容量は次のように申告できる。各ラベルには記録時と同じ名前を使う。

```rust
Meshes::reserve_imports(&mut preparation, max_meshes)?;
preparation.reserve_raster_pass("toon", samples, |pass| {
    pass.transient_attachment("toon color", color_desc)?;
    pass.draws(&program, max_draws)
})?;
```

分割画面やUIのクリップには `graph.raster` のコールバック内で `set_viewport(Some(rect))`、`set_scissor(Some(rect))` を呼ぶ。
矩形はアタッチメントの選択mip内の画素単位で、以後の描画に効く。`None` で全域に戻す。load・clear・store・resolveは引き続きアタッチメント全域が対象となる。

[`rust_pipeline`のソース](../../crates/renderer/examples/rust_pipeline.rs)では、unlit Forwardと標準出力の間に
HDR画像のコピーパスを挟んだGraphをCPUで構築・compileする。

```sh
cargo run --locked -p renderer --no-default-features --example rust_pipeline
```

### Cook 済みテクスチャを段階的に読み込む

`Renderer`、`WindowRenderer`、`SceneRenderer` は同じ手順で画像を準備できる。
最初に `check_texture_support(desc, linear_filtering)` で現在のデバイスが形式と用途を扱えるか確認し、
`prepare_texture(name, desc, simultaneous)` で同時に保持する画像の数を予約する。旧版を表示しながら
新版を作る場合は両方を数える。共有する一枚を材質ごとに予約する必要はない。

全レベルを一括で渡すには `upload_texture(name, desc, bytes)`、最初に粗いレベルだけを渡すには
`upload_texture_mips(name, desc, levels, bytes)` を使う。`levels` は終端を含まない範囲で、
データはその範囲の mip 番号順にパディングなしで並べる。どちらも `(FrameId, ManagedImage)` を返す。
`poll_texture_upload(id)` が `true` を返した時点で、その転送完了を回収できたことになる。
細かいレベルが届いたら `promote_texture(name, &previous, levels, bytes)` に渡す。
新しい画像は指定範囲以外の有効なレベルを旧版から GPU 上で引き継ぐため、以前のバイト列の再送は不要である。
現用の画像ハンドルは、新版の準備が終わり材質の参照を更新するまで保持できる。

<a id="scene-materials"></a>

### エンジンのシーンとカメラから描画する

`renderer::material::Materials` をシーンと一緒に保持する。単色の材質は作成時に色を渡せばよい。
同じハンドルを複数の描画に渡すと、そのハンドルへの更新が次に構築する Graph から全描画に反映される。

```rust
use renderer::material::{Materials, PbrParams, UnlitParams};

let mut materials = Materials::default();
let red_paint = materials.create_pbr(PbrParams::color([0.8, 0.04, 0.02, 1.0]))?;
let marker = materials.create_unlit(UnlitParams { color: [1.0, 0.8, 0.1, 1.0] })?;

// このメッシュだけ青い塗装に変える。
let blue_paint = materials.duplicate(red_paint)?;
materials.update_pbr(blue_paint, PbrParams::color([0.03, 0.1, 0.8, 1.0]))?;
```

アプリケーションがシーン、階層、アニメーションを所有する。描画時には、メッシュと材質のハンドルと
ワールド変換を持つ `RenderObject` のスライスを渡す。同じスライスを複数のカメラから描画できる。

標準カメラを画面外で使う場合は `renderer::Renderer::headless` で準備する。シーンと HUD は同じ Frame に記録する。
画面上の矩形とクリップは出力画素単位、画像と塗りの色は表示リニアの乗算済みアルファで渡す。

```rust,ignore
let mut renderer = Renderer::headless(backend, camera, assets)?;
let mut frame = renderer.begin_frame()?;
frame.draw_scene(scene_input, 1.0)?;
frame.draw_overlay(&hud_layers)?;
let capture = frame.capture()?;
let ready = renderer.read_capture(&capture, &mut rgba, timeout)?;
```

読み戻すバッファは幅 × 高さ × 4 バイトとし、`ready` が false の場合は同じ取得票で再試行できる。
Windows では `WindowRenderer::draw` のクロージャに同じ Frame を受け取り、シーンと HUD を記録する。
表示待ちの再試行では、すでに受理した画像を提示する。UI だけを描くフレームでは `draw_overlay` から開始できる。

空からモデルを追加するときは、ロードの境界で `renderer.prepare_scene(SceneAssets::new(&mut meshes, &mut materials, scene.objects()))?` を呼ぶ。
標準 `Renderer` はこの一覧から独立に動かせる個体数と必要なシェーダーを求め、同じデバイス上で必要な分を準備する。読取中の画像が変更を妨げる場合は、`error.capacity().map(|c| c.recovery())` が `Some(renderer::CapacityRecovery::ReleaseOwners)` なら、その結果を回収して再試行する。ホストが固定した GPU 予算を使う場合は `SceneRenderer::replace_working_set` で上限超過を受け取れる。
転送が完了したら同じビューで描画できる。準備した個体を動かすフレームは描画入力の変換だけを更新する。
画面内の端末のように自作のパスを組み込む場合は、`headless_with` または `WindowRenderer::new_with` でパスの資源を予約し、`frame.graph()` に記録する。
そこで描いた画像ビューはそのフレーム内の Canvas に渡せる。[端末の奥行きと HUD 合成の実行例](../../crates/training-yard/tests/world_panel.rs)

同じモデルを複数の位置に置くときは、`model::Model` に部位ごとのメッシュ範囲、共有材質スロット、LOD の最小画素サイズをまとめる。
`instantiate` で作った個体には変換と材質の上書きを保持する。ロード時に `assets.add_model(&actor)` で全 LOD の資産を準備し、描画時に `draws(&camera)` でそのビューの部位を抽出する。
個体の `set_material(slot, Some(handle))` はその個体に効き、`None` は共有材質に戻す。共有材質自体の変更は `Materials::update_pbr` で行う。[隊員のモデルと更新操作](../../crates/training-yard/src/yard.rs)

訓練場の装備は隊員ごとに要求を持ち、同じ装備への要求は一つの読み込みと候補にまとめる。
複数の装備は塗装画像の所有権を共有する。描画の前に一つの候補を進め、装備が描画可能になった時点で旧装備を使う最後の隊員を確認し、採用と退役を同時に確定する。
標準フレームの `headless_with` と `WindowRenderer::new_with` では、転送と描画を同時に提出するため `frames_in_flight` に 2 を渡す。
マウス選択や画像読み戻しの結果をホストに残すため、装備の転送結果は提出時の `FrameId` を指定して個別に回収する。[装備の読み込みと採用](../../crates/training-yard/src/yard/equipment.rs)

スキニングを使うメッシュは、ロード時に `MeshData::skinned_pbr` に頂点と各頂点の最大 4 関節分の重みを渡す。
個体ごとに `meshes.create_pose(mesh)` でポーズを作り、`actor.set_pose(mesh, Some(pose))` で結ぶ。
ゲーム側でアニメーションを評価したら、逆バインド変換を含むモデル空間の最終行列を `meshes.set_joint_transforms(pose, &palette)` に一度渡す。
現在は CPU で頂点を変形し、そのフレームで使うポーズごとに一つの頂点バッファをアップロードする。各材質スロット・カメラ・影が同じ頂点を読む。LOD 用モデル境界にはアニメーション全体を含める。

既存エンジンのビューポートでは、ホストが用意した Graph と描画先をそのまま使える。次の例で `drawing` は初期化済みの `SceneRenderer`、`host_target` は同じ Graph に作った表示リニア RGBA8 ターゲットである。

```rust,ignore
let (mut frame, runtime) = drawing.begin_host_frame(host_graph)?;
let scene = frame.draw_scene(&camera, input)?;
frame.write_output(camera.id, scene.color, 1.0, Some(host_target))?;
let (mut graph, mut histories) = frame.finish_recording()?;
host_ui.record(&mut graph, host_target)?;
let receipt = runtime.submit(&graph.compile()?)?;
histories.publish(&receipt)?;
```

ホストは自分の受理票で表示・読み戻しを続ける。提出を取り消す場合は Graph と `histories` を破棄する。投入が受理された後でビュー履歴の確定が中断した場合は、その受理票で `histories.publish` を再試行できる。トークンを手放した後は `drawing.finish_host_submission(&receipt)` で残りを確定する。[ホスト提出と履歴の回復試験](../../crates/renderer/tests/view_pipelines/publication.rs)

<a id="equipment-cook"></a>

### glTF 装備の取込と配布

制作側は Rust の `renderer-asset-cook` を使う。次の例では、制作ディレクトリ `source` に静的装備の `armor.gltf` と、それが参照するバッファ・PNG を置いている。

```powershell
cargo run --locked -p renderer-asset-cook -- import source/armor.gltf
cargo run --locked -p renderer-asset-cook -- cook source dist --cache .tools/armor-cache
```

`import` が作る `armor.gltf.meta.json` は原本と一緒に版管理する。改名時は両方を移動し、glTF 内の相対参照も維持する。新しい装備として複製するときは新しい原本に対して `import` を実行する。セーブはメタデータの装備 ID を保持するので、同じ装備の画像を編集して再 cook しても、セーブを書き換えずに新版を利用できる。

初版の入力は静的な三角形の glTF と PNG で、材質は最初の UV セットを使う。複数の装備で画像を共有するときは、共通の制作ディレクトリを `cook` に渡し、各 glTF からの相対 URI で参照する。法線と UV を用意し、接線を省略した場合は、Rust クレート `bevy_mikktspace` の既定設定で三角形の各コーナーに接線を生成する。原本にある接線は既存の取込処理を使う。装備 cook は Rust ツールチェーンでビルドできる。初回の ID 発行にはハードリンクを扱える制作側ファイルシステムが必要になる。スキン、モーフ、sparse accessor は取込エラーとなる。遮蔽を使う場合は metallic-roughness と同じテクスチャに格納する。

ゲームには `dist` の内容を配布する。`objects/` の成果物と `catalogs/` の索引を先に転送し、最後に `current` を切り替える。ゲームが使用中の世代と復帰先の世代が参照するファイルは保持する。変換キャッシュは制作側に置き、空の状態からも同じ原本と設定で再生成できる。cook の JSON 出力で、各段階の再実行理由、入力・レシピ・成果物のハッシュ、生成量と配布先への新規書込量を確認できる。

訓練場を開発するときは、clone ごとに一度、次のコマンドでプロジェクト指定の cooker をインストールする。セットアップは大型の Git LFS 原本をポインタのまま checkout する設定も行う。同じ clone の linked worktree はこの設定と実行ファイルを共有する。

```powershell
cargo run --locked --release -p renderer-asset-cook -- setup
```

以降は任意の worktree で通常どおり訓練場を起動する。ゲームは起動した作業ディレクトリからプロジェクトを見つけ、cooker が選んだ世代を共有ストアから開く。Windows の `--mode interactive` は左の隊員に Narrow を装着し、その worktree の原本と Git インデックスの変更を監視する。`source/paint.png` を保存すると、背景で cook と装備の準備を行い、準備が終わったフレームで新しい色に切り替わる。競合や cook エラーの理由は端末に表示する。

LFS 管理の原本を制作ツールで編集するときは、その worktree で対象のパスを限定して `git lfs pull --include="編集する原本のパス" --exclude=""` を実行する。複数の原本が必要ならパスをカンマで区切る。展開したファイルはその worktree の編集用コピーになる。

変換器を更新する場合は [プロジェクトの指定版](../../crates/renderer-asset-cook/tool-version) を更新して同じセットアップを実行する。一度インストールした版は、その版を指定する別の worktree でも使用できる。変換器を開発中の worktree だけでローカルビルドを試す場合は、`TRAINING_YARD_COOKER` に実行ファイルの絶対パスを指定する。

休眠させる worktree のルートで、次の順に容量を確認・回収できる。最初のコマンドは、現在の選択世代と復旧用の一世代の保持を解いたとき、追加で回収できるカタログと成果物のファイル長をバイトで返す。実行中の読み手と別の worktree が必要とする分を差し引いている。`--apply` は保持を解き、`gc` はその時点で全体から回収した量を返す。変換が実行中なら終了後に再試行する。未コミットの原本は作業場所に残り、次回起動は残っている成果物、または Git・LFS の原本とその編集から復元する。

```powershell
$source = 'crates/training-yard/assets/equipment/source'
cargo run --locked --release -p renderer-asset-cook -- release $source
cargo run --locked --release -p renderer-asset-cook -- release $source --apply
cargo run --locked --release -p renderer-asset-cook -- gc $source
```

共有ストアの読み込み時に内容ハッシュの不一致を検出したら、その世代を選択した worktree で同じ入力と同じバージョンの cooker を使い、`renderer-asset-cook repair <source>` を実行する。修復は各レシピの成果物をハッシュで検査し、破損・欠落分だけを再変換して原子的に公開する。選択と読み手のリースを維持する。出力 JSON の `generation` と `published_bytes` で修復世代と書き込み量を確認できる。入力が変わっていた場合は元の入力を復元して再実行する。

訓練場の配布版では、実行ファイルの隣に `assets/equipment/dist` を生成する。配布ビルドでは必要な LFS 原本を取得して上記の `cook` を実行する。別の配布物を試すときは環境変数 `TRAINING_YARD_ASSETS` に `current` のあるディレクトリを指定する。ホストは `Generation::open` で取得した世代を `Yard::set_asset_generation` に渡し、装着先と保存済み ID を指定して `request_equipment` を呼ぶ。更新先を確認するまで、旧世代の装備を描画できる。

訓練場では、装着中・準備中を合わせて異なる装備版を 4 件、装備ごとに最大 4 部位まで扱う。同じ版を装着する隊員はこの枠を共有する。新しく必要なバッファ寸法、画像形式、サンプラーは装備の変更時に `AssetPreparation::replace_asset_reservations` で準備する。画像は mip 一式で最大 4 MiB とし、1 回の更新で 1 枚を投入する。メッシュと材質の転送は更新ごとに合計 128 バイト・1 投入を上限とする。これらは小さい装備で切替を再現する訓練場の設定である。

訓練場は同じ隊員と操作を使って次のように起動できる。Windows では `--api d3d12` でも実行できる。

```powershell
cargo run --release -p training-yard --all-features -- --api vulkan --mode headless --output .tools/yard
cargo run --release -p training-yard --all-features -- --api vulkan --mode host --output .tools/yard-host
cargo run --release -p training-yard --all-features -- --api vulkan --mode window
cargo run --release -p training-yard --all-features -- --api vulkan --mode interactive
```

画面外の再生は各フレームの表示色を PPM に保存する。ホスト再生は主画面、低解像度ビュー、影、6 フレームごとに更新する監視モニターを一度に提出して読み戻す。
更新時は同じ Graph のカメラ出力をパネルに合成して `submit_with_image` で保持する。次回は保持した `ManagedImage` のスナップショットを Graph に import して使う。[監視モニターの利用コード](../../crates/training-yard/src/replay/monitor.rs)
ウィンドウ再生は自動で終了する。手動操作では 1 で左の鎧を発光、2 で共有色を変更、3 で発光を解除し、4・5・6 で左のポーズ・右のポーズ・静止姿勢を切り替える。Space で装備要求・完了・削除・再生成を 1 操作ずつ進め、Esc で終了する。[ウィンドウのイベント処理](../../crates/training-yard/src/window.rs)・[ホストの描画コード](../../crates/training-yard/src/replay.rs)

V で監視モニターと選択輪郭を含む 24 フレームを同じウィンドウに再生して終了する。更新フレームでは監視画像と提示画像を一度の `submit_with_images` で取得する。

Windows の GPU、デスクトップ、Vulkan の検証レイヤー、D3D12 の Graphics Tools が揃った環境では、次のコマンドで二つのバックエンドと厳格・製品の両構成を検証できる。引数に新しい保存先を指定する。

```powershell
python -B tools/training_yard/accept.py .tools/yard-acceptance
```

保存先にはコマンドと終了状態、Git リビジョン、実行ファイルの SHA-256、アダプター、フレーム別の入力・画像・差分が残る。ヘッドレス画像は sRGB の PPM。ホスト画像は表示リニア RGBA8、部位 ID は little-endian u32、深度と影は little-endian f32。基準は実機で独立の画素・姿勢・選択判定を通した 29 枚の PPM とホスト 24 フレームから作成した。全画素を対象に色は各成分 1、深度・影は絶対差 0.00001 を上限とし、ID と入力列は完全一致を要求する。保存なしのホスト実行でメモリを別途計測する。基準を変更する際は画像差分と期待値の変更理由をレビューする。[受入スクリプト](../../tools/training_yard/accept.py)・[基準データ](../../crates/training-yard/tests/reference.zip)

ウィンドウの寸法イベントは `WindowRenderer::resize(extent, assets)` に渡す。ゼロ寸法では記録を休止する。
3D の解像度を変えるには、ロード境界で `set_scene_extent(Some([width, height]), assets)` を呼ぶ。`Frame::scene_view()` の寸法を LOD 選択に使える。Canvas とキャプチャの寸法は出力に合わせる。`None` でウィンドウに追従する。訓練場の最後の 5 フレームでは、ガラスと障害物を弾道が横切る間に 3D 解像度を半分にし、1 画素の照準を描く。手動操作の F でこの 5 場面を順に再生して隊員画面へ戻る。[射線の材質と期待画素](../../crates/training-yard/src/effects.rs)
連続するカメラ移動には `set_camera`、カットには `cut_camera` を使う。ホストが複数ビューを管理するときは `drawing.camera_cut(view_id)` でそのビューの時間履歴を無効化できる。標準パイプラインは準備済みの影画像に次回描き直す。
資産のロードやリサイズは以前の描画と提示の完了を待てる変更境界で行う。ホストが待機時間を制御する場合は `wait_idle(timeout)` を使う。提示待ちの画像がある間は通常の `draw` で提示を進める。
ビュー構成を置き換える際は、旧ビューと新ビューの資源クラスを一時的に同時に準備する。新ビューの初期化が失敗した場合も以前のビューで描画を続けられる。

選択に使う形状は `picking::PickPass` で描ける。準備時に寸法・最大部位数・1 フレームの照会数を渡す。記録時にビューの描画対象と同じ長さの u32 ID 列を渡すと、深度判定した ID 画像が得られる。0 は遮蔽物や背景に使える。標準 PBR・頂点色の頂点ストリームを扱い、スキニングは本描画の頂点を共有する。選択対象や透明材質を含める規則、物体・部位と ID の対応はホストが決める。

`image.read_pixel(graph, [x, y])` は左上を原点として 1 画素を記録し、結果はその提出の完了時に 4 バイトで受け取る。クリック時点の対応表を保持して世代を照合すれば、古いクリックが新しい隊員に適用されるのを防げる。輪郭パスは同じ Graph で ID 画像を直接読める。[非同期クリック](../../crates/training-yard/src/replay/selection.rs)・[ホスト所有の輪郭パス](../../crates/training-yard/src/replay/selection/outline.rs)

同じ `--mode host` の後半では、1,024 体の標的をホスト側の配列から描く。実行枠が占有されていれば完了を回収し、結果枠が占有されていれば読み戻しを終えて返却する。ビューの描画数を超えた場合はフレームを省略し、ロード時に実際の標的一覧から再準備する。[容量不足からの復帰と描画結果の比較](../../crates/training-yard/src/targets.rs)

個別のパスを準備して標準 Forward を使う場合はカメラと初期シーンから構成する。カメラには安定した `ViewId`、寸法、`ViewInput` を渡す。
`build` は参照されたメッシュと材質を転送し、初期化に使った投入の完了を回収して返る。

```rust
use renderer::pipeline::{RendererSetup, SceneAssets, SceneInput};

let assets = SceneAssets::new(&mut meshes, &mut materials, &objects).with_lights(&lights);
let mut renderer = RendererSetup::standard(camera).build(backend, assets)?;

// 毎フレーム。HDR の後に独自パスを追加できる。
let mut frame = renderer.begin_frame()?;
let scene = SceneInput::new(&meshes, &materials, &objects).with_sun(sun).with_lights(&lights);
let hdr = frame.draw_scene(&camera, scene)?.color;
let sdr = frame.write_output(camera.id, hdr, exposure, None)?;
sdr.display().expect("display output configured").export_for_display(frame.graph())?;
let receipt = frame.submit()?;
```

`frames_in_flight` の既定値は 1。`additional_objects` は追加の描画、材質、モデル変換、カスケードごとの
影描画に使える件数、`additional_lights` は追加の点光源数。初期シーンでは共有材質を一度だけ数える。
登録された PBR 個体はそれぞれ独立して移動できる。初期姿勢が同じ場合も初回移動のための余裕指定は不要である。
新しいサイズのメッシュ、独自シェーダーや追加の描画方式を使う際は、変更時にそれらの資源も予約する。

同じ方式・寸法のカメラは `CameraGroup::add_view` で登録し、フレーム内で
`frame.bind_for_view(camera.id, scene)?` の戻り値を複数回の `draw_view` に使える。
寸法や方式の異なるカメラは `RendererSetup::add_group` で追加する。
`CameraGroup::standard` で描画方式、MSAA、影を、`group.output` で出力を設定する。
`CameraGroup::custom` のファクトリには寸法、シーンから求めた上限、使用中の独自材質定義が渡る。

**ホスト所有の Runtime と後処理。** ウィンドウや既存デバイスを使う場合、まず CPU 上でシーンの予約を求める。
他のサブシステムの資源と同時に使うパスもここで合算する。次の例ではホストがウィンドウと表示を管理する。

```rust
let mut plan = setup.prepare(SceneAssets::new(&mut meshes, &mut materials, &objects))?;
let bloom = Bloom::Tent.prepare(plan.preparation_mut(), ImageDesc::rgba16_float(width, height))?;
plan.reserve_texture("albedo", texture_desc)?;
plan.fit_backend(&mut backend)?;
let (mut runtime, mut presenter) = Runtime::for_window(
    backend, plan.runtime_limits(base_limits)?, plan.preparation().working_set(), window, presentation_config,
)?;
let mut renderer = plan.begin_with_runtime(&mut runtime)?.wait()?;
```

描画時は `bloom.add_passes(frame.graph(), hdr)?` の出力を `write_output` に渡す。
表示する場合は受理票を `presenter.queue_frame(&mut receipt)?` に渡し、
`presenter.present_frame(renderer.runtime())?` を呼ぶ。`NotReady` のときは同じ Presenter で再試行する。
通常の GPU 完了と読み戻しはエンジンで回収する。`reserve_readback` は同時に読む画像ごとに一回呼ぶ。

**分割ロード。** `setup.begin_build(backend, assets)?` または `plan.begin_with_runtime(&mut runtime)?`
でローダーを作り、ロードループで `poll()?` を呼ぶ。`Poll::Ready(())` を受けたら `finish()?` で描画へ移る。
各 poll は GPU を待たず、メッシュ転送は `mesh_upload_bytes`（既定 1 MiB）ずつ進む。デバイス作成と
シェーダー準備は同期処理。容量が他の投入で埋まっている場合はホスト所有の完了や結果を返却して同じビルドを再試行する。
`cancel()?` は元のアセット版を復元する。返されたフレーム ID があればホストが完了を回収する。
未完のビルドを drop すると、復元に加えて自分の未回収の投入を待つ。

独自方式は `ViewPipeline` を実装する。`reserve` で共有資源とビュー別の作業領域を予約し、
`check_object` で対応材質・頂点レイアウトを確認する。`record` 内では `object.import_mesh(graph, scene.meshes)` で
そのオブジェクトの頂点・インデックス範囲を取り込む。一つのメッシュを材質ごとに分けて描くには
`RenderObject::with_index_range` を使う。戻り値は同寸の単一サンプルのシーンリニアRec.709 RGBA16Fと、任意の単一サンプルD32。
標準DeferredのG-bufferを使う独自パスは `draw_view_with_gbuffer` のコールバックで照明前に追加する。
[外部の Toon 実装](../../crates/renderer/examples/support/toon_pipeline.rs)と
[共通の描画ループ](../../crates/renderer/examples/support/views.rs)を参照。

半透明物はすべての材質をまとめてビュー奥行きで整列する。基準点はインデックスが参照する頂点の
境界箱の中心で、モデル変換も反映する。必要なら `RenderObject::sort_center` で変更できる。
Unlit は頂点色、材質色、オブジェクトの tint を乗算する。
標準 PBR の頂点は 48 バイト、頂点色 Unlit は 28 バイトで、独自メッシュには頂点属性と境界を指定する。

ビュー履歴は方式が所有する。GPU 投入前にフレームを破棄すれば更新候補も破棄される。
`ViewSubmitError::Rejected` は再構築できる。`Accepted` は投入済みで、
`renderer.finish_submission()` を再試行して残りの履歴を確定し、表示へ進む。
独自方式の `accept` は再試行に必要な状態を保持する。不要なカメラは `remove_view` で解放する。

**リサイズ・方式変更。** 同じレジストリと新しいカメラで `setup.prepare(assets)?` を呼び、
継続するエンジンのパスも予約に含める。未完の実行と結果・受理票を返却してから
`renderer.replace_working_set(plan)?` を呼ぶ。これは Runtime 全体の作業用資源とカメラの履歴を更新する。
同じ Runtime の常駐メッシュ、材質、テクスチャを再利用できる。デバイス作成時の上限を超える計画は
`CapacityRecovery::Reconfigure` を返す。ホストが独自に作業用資源を管理する場合は、
合算した予約をホストから準備した後に `begin_with_runtime` で接続する。
一部のカメラだけ移動して他の履歴を保つ場合は `parts_mut` から `ViewRenderer::reconfigure_view` を使う。

**アセットの更新。** 予約内の変更を公開するには、旧版を使う Graph を投入または破棄し、
`renderer.prepare_scene(SceneAssets::new(&mut meshes, &mut materials, &changed_objects))?` を呼ぶ。
大きなメッシュは `meshes.stream_gpu(&mut runtime, &mut pool, &selection, byte_budget)` で優先順と1回分の転送量を指定できる。
完全な新版が受理されるまで既存メッシュを描画できる。旧版を使うGraphと同サイズの更新が重なる場合は `plan.reserve_mesh_replacement(mesh)?` でバッファを追加予約する。
`Runtime::upload_texture(&mut pool, name, desc, bytes)` は画像を転送し、`(FrameId, ManagedImage)` を返す。
あらかじめ画像の資源クラスと転送容量を予約する。入力は `ImageDesc::texture_2d` と `with_mip_levels` で記述し、
最大mipから順に詰めた `desc.byte_len()` バイトを渡す。BC1・BC3・BC5・BC7は4×4ブロック単位で並べる。
同じRuntimeへの次の投入ではCPUで転送完了を待たず `ManagedImage::snapshot()` を使える。材質を使うGraphの投入まで画像所有者を保持する。
複数画像を同じGraphで転送するときは `graph.upload_texture_for_export` で画像ハンドルを受け取り、後続パスへそのまま渡せる。
旧版を参照するGraphを投入または破棄した後に、`meshes.collect_gpu` と `materials.collect_gpu` で旧版の登録を回収する。
終了時は `renderer.clear`、`meshes.release_gpu`、`materials.clear` で各所有者の登録を解放する。

標準の材質処理に独自シェーダーを加える場合、独自Programの描画条件は1×のindexed Forwardとなる。Deferredのビューでは照明計算後に共通の深度へ描く。
影を描く定義には同じ頂点・材質入力を受け取る深度Programを指定する。テクスチャとサンプラーは `Parameter::Binding` で宣言する。
独自の定数ブロックには `Value::Uniform`、読み取り専用の構造化データには `Value::StorageRead` を使う。
GPU 常駐の PBR 係数とテクスチャを使う場合は `PbrMaterial::resident` で接続できる。
[独自 Toon の登録、描画、GPU 読み戻しの実例](../../crates/renderer/tests/pbr_gpu/mixed.rs)

<a id="pbr-forward-recipe"></a>

### PBRの標準レシピを使う

モデル空間のメッシュ1個は `Draw::model(geometry, transform_index)` で指定し、変換はフレームの表に登録する。
ワールド空間に変換済みの頂点には `WorldSpaceIndexed` または `WorldSpaceIndirect` を使う。
材質係数をフレーム間で共有するときは `ResidentMaterialUniform::import` で係数を接続し、
当該Graphのテクスチャ・サンプラーと合わせて `pbr::GraphMaterial` を組み立てる。

`renderer::pipeline::GeometryPipeline::new(technique, config, scene_limits)`でForward、Forward+、Deferredから選ぶ。
描画寸法・出力条件と、同時フレーム・材質・描画・各カスケードのキャスターの上限を渡す。
`Technique` で使用できるサンプル数、深度、G-bufferの有無を確認できる。
標準3方式で局所光源を使うには `SceneLimits::max_local_lights` にビュー当たりの上限を指定する。
フレームの光源は `pbr::PbrDrawInput::with_lights(&lights)` で渡す。上限0では局所光源の資源を予約せず、空の入力では転送やタイル判定を省く。

<a id="photometric-units"></a>

### ライトの測光単位

ワールド座標の 1 単位を 1 m とする。色は線形 Rec.709 RGB。色と強度を別々に渡すコンストラクターは、着色後の Y（0.2126 R + 0.7152 G + 0.0722 B）が指定した測光量になるよう正規化する。単色の青を選んでも指定光束は保たれる。

| 入力 | 指定する量 |
| --- | --- |
| `Sun::from_lux` | 光線に垂直な面の照度（lx）。方向は受光点から太陽へ向かう。校正済み RGB には `Sun::from_rgb_lux` を使う |
| `PointLight::from_candela` / `from_lumens` | 等方点光源の光度（cd）または全光束（lm）。1 cd は全方向で 4π lm |
| `RectLight::from_lumens` | 片面から出る全光束（lm）。面積 A の一様なランバート面の輝度は lm/(π A) |
| `EnvironmentMaps::nits_per_unit` | 元の環境画像の白 (1, 1, 1) に対応する輝度（cd/m²）。絶対輝度の HDRI は 1、相対 HDRI は撮影時の校正値を使う |
| `MaterialFactors::with_emission_nits` | 発光テクスチャが白の部分の輝度（cd/m²）。`emissive_nits` には校正済み RGB を直接指定できる |

```rust
use renderer::pbr::{LocalLight, PointLight, Sun};

let sun = Sun::from_lux([0., 1., 0.], [1.; 3], 20_000.)?;
let bulb = PointLight::from_lumens([0., 2., 0.], 12., [1., 0.8, 0.6], 800.)?;
let lights = [LocalLight::Point(bulb)];
let input = input.with_sun(sun).with_lights(&lights);

// オフラインで静的な電球からの反射光を焼く場合
bake_scene.set_point_lights(vec![bulb]);
let lighting = renderer_bake::Lighting::environment_map(sky_map, nits_per_unit).with_sun(sun);
```

点光源は正対面に I/r² lx を与える。ゲーム描画では 0.1 m 未満の発散を抑え、指定した到達距離まで滑らかに減衰させる。ベイカーにも同じ距離則が適用され、静的な遮蔽物は反射光に影を落とす。絶対値の入ったライトマップや局所プローブには HDRI の校正倍率を重ねない。露出とトーンマッピングはこれらの光を加算した後に適用する。Sponza と Bistro の比較用プリセットは絵合わせの値を使っており、現場で測定した照度とは独立している。

ゲーム側の標準経路では、点光源と矩形光源を同じスライスに入れ、`SceneInput::with_lights` でビューに渡す。
初期化時に `SceneAssets::with_lights` へ同じスライスを渡すと、その件数が自動で予約される。
次は Sponza の中庭で使った下向きの面光源。長さの単位はメートルで、半辺の外積が発光面の表を向く。

```rust
use renderer::pbr::{LocalLight, RectLight};

let panel = |x| RectLight::from_lumens(
    [x, 10.431109, -0.559192], [2.5, 0.0, 0.0], [0.0, 0.0, 0.5],
    [0.7557183, 0.8001526, 0.9096513], 300.0, 48.947266,
);
let panels = [panel(-4.739024)?, panel(2.899622)?];
let lights = panels.map(LocalLight::shadowed_rect);
let input = input.with_lights(&lights);

// 静的な光源を間接光に含める場合、オフラインの準備時に同じ形状と光束を渡す。
bake_scene.set_rect_lights(panels.to_vec());
```

各灯の面輝度は着色後の光束を面積と π で割って求める。到達距離は矩形上の最近点から測る。
直接光では面が占める立体角から拡散を積分し、GGX の鏡面反射には LTC 近似を使う。
`LocalLight::shadowed_rect` は中心から見た遮蔽を固定幅 3×3 PCF で求め、矩形の直接拡散・直接鏡面に適用する。
`LocalLight::Rect` で影を省略できる。太陽のカスケード設定とは独立して使える。
`RendererSetup::additional_shadowed_rect_lights` は初期登録後に増やしたい影付きの灯数。灯数とアトラスの両方を予約する。
初期化時は使用する光源を `SceneAssets::with_lights` に渡す。起動後に灯数や影付き灯数が増えるときも、同じ指定で `Renderer::prepare_scene` を呼べる。準備済みの範囲内での移動・調光・影の切り替えは、フレームの `SceneInput::with_lights` だけでよい。準備を超えた場合は `GraphError::capacity()` で `SceneLimits::LOCAL_LIGHTS` または `SHADOWED_RECT_LIGHTS` と必要数を判別できる。
標準パイプラインは画面外の `RenderObject::casts_shadow` も含めて、変換・スキン姿勢・メッシュ・材質・アルファマスクを追跡し、変更が重なる投影面を更新する。
低水準の `PbrDrawInput::new` は可視リストと別に共通のキャスター列を受け取る。太陽だけを個別にカリングする場合は `SunShadows::Cascades` を指定できる。矩形影をフレーム間で保持する場合は `GeometryPipeline::rect_shadow_history` で用意した履歴の `begin` に面ごとのキャスター版を渡し、`use_rect_shadow_history` で結び、`export` した更新を投入成功後に `accept` する。
ベイクでは静的な遮蔽物から矩形の発光面への可視性を評価する。
ベイクした拡散プローブ・ライトマップ・反射プローブには、矩形光が他の面で反射した寄与が入る。
矩形を変えた場合はそのデータを再ベイクする。ベイク中のシナリオ間で矩形光源のリストは共通となる。

準備済みレシピと必要な既定画像の要求を`Preparation`へ集約し、`Runtime`と`GraphPool`を作る。
追加パスの宣言、アップロード、表示やGPUの完了待ちまで保持する資源の容量もアプリの構成に沿って指定する。

```rust
use renderer_core::capacity::Preparation;
use renderer::{pbr::{Defaults, Technique}, pipeline::GeometryPipeline};

let mut preparation = Preparation::new(scene_limits.frames_in_flight);
let recipe = GeometryPipeline::new(Technique::Forward, config, scene_limits)?;
let requests = recipe.preparation();
preparation.add_requirements(requests.graph_requirements(), requests.working_set())?;
let defaults = Defaults::for_recipe(config).prepare(&mut preparation)?;
```

Runtime作成後に`defaults.initialize`を一度呼び、アプリのフレーム枠を使い切る構成では返された初期化IDを最初の描画前に回収する。
各Graphには`DefaultResources::import`で共有画像を取り込む。`Defaults::Unshadowed`は独自の材質と環境画像を用意する場合に、影を無効にした深度だけを登録する。
既定画像を使い終えたら`DefaultResources::clear`で登録を解除する。

`PbrVertex`をエンコードし、`MaterialUniform::upload`で`MaterialFactors`をアップロードする。
`GraphMaterial::new`へ係数と、`MaterialTextures`の名前付きベースカラー・法線・ORM・発光ビューを渡す。
省略したマップは`DefaultViews::material_textures`で補え、`environment`でIBLの寄与を無効にできる。
標準材質では、ベースカラー・法線・ORM・発光の各マップに独立したサンプラーを指定できる。各マップの既定は `LinearRepeat`。登録材質には `PbrParams::samplers`、Graph へ直接渡す材質には `GraphMaterial::with_samplers` を使う。全マップを同じ設定にする場合は `MaterialSamplers::uniform` でまとめて指定する。
マスク材質の影もベースカラーと同じサンプラーと mip 範囲でアルファを読む。部分常駐する登録画像は `MaterialTexture::new(snapshot, mips)` で初期化済みの連続範囲を指定し、昇格時に材質を更新する。
高水準のセットアップは選択した材質のサンプラーを準備する。ホストが Runtime を管理する場合は、登録材質なら `Materials::prepare_samplers`、Graph へ直接渡す設定なら `Preparation::reserve_sampler` をロード時や変更時に使う。
同じ係数から得たアルファ分類に従い、Opaque／Maskは不透明・影リストへ、Blendは奥から手前に並べた半透明リストへ入れる。
毎フレーム、アプリ側でカメラを更新し、最終的なビュー投影行列と視点・投影情報を`pbr::ViewInput`で渡す。
`prepare_view`でその入力と太陽光・シーンの範囲・環境照明を組み合わせ、`pbr::PbrDrawInput::new`に
Graph内の材質、不透明・半透明・影を落とす描画の各スライス、既定画像のビューを渡す。
フレームは準備済みビューの設定に従ってカスケードまたは共有の影なし深度を選ぶ。
カメラバッファは準備時に`CameraStorage`で選ぶ。
`External`では呼び出し側が渡したバッファへレシピが更新を書き込み、`PreUploaded`では可視判定などの前に
呼び出し側がビューのバイト列をアップロードする。
`add_scene_and_sdr_output(graph, frame, exposure)` は指定した露出でSDR出力まで追加し、
HDR・SDRと利用できるシーン深度、G-bufferを返す。
深度は1×で単一サンプルD32、4×では`None`となる。G-bufferはDeferredで得られる。4カスケードのシャドウマップは双方で単一サンプルD32を使う。
HDRを加工したい場合は`draw_hdr`で照明済みのHDRを受け取り、同じGraphに独自パスを追加して、
その出力を`write_output`へ渡す。`output: None`で準備すればHDR専用になり、標準の出力画像とProgramを予約しない。
出力未準備や不正な露出は`add_scene_and_sdr_output`がGraphへの追記前に拒否する。
表示用の線形SDRはexportして受理済みフレームから表示し、保存用のsRGBはreadbackする。
Graphへのレシピ追加が途中で失敗した場合は、そのフレームのGraphを破棄して作り直す。

シャドウの既定経路では共通のキャスターリストから4枚のD32画像を描く。
アプリはカスケードごとにキャスターを変えたり、取り込んだ画像を`ShadowSource::Reuse`で渡したりできる。
`shadows: None`では共有のクリア済み1×1深度を取り込み、カスケードの投影・描画・履歴を省く。

#### 標準 Bloom を追加する

`passes::bloom::Bloom::Tent`または`Bloom::Box`の`prepare`に、容量を集約する`Preparation`とシーンHDRの`ImageDesc`を渡す。
シェーダー、一時画像、Graph宣言の容量がこの準備で登録される。フレーム内では`draw_hdr`で得たHDRを
`PreparedBloom::add_passes`に渡し、同寸の結果を`write_output`へつなぐ。入出力は露出・トーンマッピング前の
単一サンプル・単一サブリソースのシーンリニアRec.709 RGBA16Fとする。`Bloom::Off`では入力画像をそのまま返す。
同じ部品を独自のシーン描画にも接続できる。

#### カスケードのシャドウを再利用する

ビューごとに`pbr::ShadowHistory`を準備する。標準レシピには`begin_view`でビューとキャスター入力の世代を渡す。
独自の影パスを組み立てる場合は`begin_frame`にカスケード行列と世代から作った`ShadowKey`を渡す。
キャスターの集合・形状・変換、アルファマスク、影の描画設定など、深度画像に効く入力が変わるときに世代を変える。
履歴は前の画像を取り込み、カスケードごとに再利用できるかを返す。`ShadowFrame::source`から
`PbrDrawInput::shadows`を作り、再描画するカスケードのキャスターを渡す。

描画を宣言した後、`ShadowFrame::export`で更新画像をexportする。Graphをcompileして`ShadowHistory::submit`に渡すと、
Runtimeが投入を受理した時点で履歴が反映される。アプリが複数ビューのGraphを直接投入する場合は、受理済みの
`FrameSubmission`と各ビューの更新をそれぞれ`ShadowHistory::accept`へ渡す。
先行フレームのGPU完了はRuntimeが管理する。影の解像度変更や終了時には`clear`で画像の登録を解除する。
キャッシュで保持する4枚の登録をRuntimeの容量に含める。

準備、入力アップロード、Bloom、シャドウ再利用は[`pbr_forward`のソース](../../crates/renderer/examples/pbr_forward.rs)で確認できる。
実行方法は[小さなPBRシーン](examples.md#pbr-forward-example)を参照する。

#### 照明方式と描画リストを選ぶ

| レシピ | MSAA | 不透明・マスク | 影 | 半透明 | Program |
| --- | --- | --- | --- | --- | --- |
| Forward | 1×／4× | Indexed／Indirect／Instanced | Indexed／Instanced | Indexed／Instanced | 組み込み |
| Forward（カスタム Program） | 1× | Indexed | Indexed | Indexed | Forward ABI |
| Forward+ | 1×／4× | Indexed／Indirect／Instanced | Indexed／Instanced | Indexed／Instanced | 組み込み |
| Deferred | 1× | Indexed／Indirect／Instanced | Indexed／Instanced | Indexed／Instanced | 組み込み |

- Forwardは各画素で局所光源を順に評価する。Forward+は16×16画素のタイルごとに候補を絞る。
  タイル数の上限は65,536で、タイルごとのマスク容量は準備した局所光源数に応じて増える。
  DeferredはG-buffer照明と後段の半透明合成で局所光源を評価する。独自Programを渡す `prepare_with_programs` のForward ABIでは `max_local_lights` を0にする。
- `pbr_deferred`は1×のG-bufferにベースカラー／メタリック、法線／粗さ、発光／AOとD32深度を保持する。
  照明後に、アプリが並べた半透明描画をHDRに合成する。
- 標準の3方式では、`SceneLimits::max_transforms`を正の値にするとモデル空間の`Draw::Instanced`と
  `Draw::TransformedIndirect`を利用できる。通常のIndexedとIndirectはワールド空間のまま使える。
  変換数は構成時に予約する。64件以下は単一の小テーブル、それ以上は1,024件単位のページを使う。
  複数ページをまたぐインスタンス範囲は順序を保って分割する。直接レシピを使う場合は
  `pbr::Draw::command_count()` で `max_draws` と各カスケードの描画上限を見積もれる。
  標準ビューでは複数プリミティブで同一のモデル変換を共有できる。
  変換は有限かつ可逆で、負の行列式も受け付ける。鏡映は表裏判定と接線フレームへ反映する。
  半透明のインスタンスと描画は利用側の指定順に並べる。影を描く不透明・マスクの形状にはシーンと同じ変換を渡す。
  間接描画の一つのグループは一つの変換を使い、可視判定には変換済みのワールド空間の境界を渡す。

ライブラリに渡すレシピの組み合わせと実シーンでの使い方は[Bistroの描画パイプライン](#bistro-scene-pipeline)を参照する。

<a id="material-shader-build"></a>

## パステンプレートと材質関数をビルドする

パスの頂点・fragmentテンプレートと材質型のHLSL includeをアセット側で用意する。
テンプレートはパス固有の`main`と入出力を定義し、必要なステージで`#include RENDERER_MATERIAL_INCLUDE`を使って材質関数を呼ぶ。
ビルド入力はJSONで、`schema_version: 1`、名前付きの`templates`と`materials`、コンパイルする組を並べた`programs`を指定する。
ソースと任意の`include_dirs`は入力ファイルからの相対パスで指定できる。

Python 3.11以降と固定版Vulkan SDKを使い、たとえば[Bistroのビルド入力](../../crates/renderer/shaders/material-programs.json)を次のようにビルドする。
出力先には新しいディレクトリを指定する。SDKの場所は`VULKAN_SDK`または`--sdk-root`で指定できる。

```powershell
python -B tools/shaders/build_material_programs.py --manifest crates/renderer/shaders/material-programs.json --output .tools/bistro-material-programs
```

`manifest.json`はテンプレート・材質型の組と、各ステージのSPIR-V／DXIL、反射情報とhashを結ぶ。
テンプレートや材質の変更時は該当する組をビルドし直す。実行時のresolverはその組のバイナリと、レビュー済みの
頂点・定数・リソースのABIおよびアクセス契約を[既存の取込境界](#runtime-shader-import)で照合してProgramを返す。

`renderer-bistro`の`bistro`では、`--shader-manifest`でForward不透明描画とシャドウのレビュー済みProgramを選ぶ。
半透明には組み込みのProgramを使う。外部バンドルを使う構成はForward 1×、直接描画、インスタンシングなしで、
取込時にビルドしたバイト列をレビュー済みのBistroバイナリと照合する。
準備済みのProgram三種を`pbr_forward::prepare_with_programs`に渡すと、レシピが頂点・定数・binding・出力と
ブレンド・深度の互換性を照合する。シェーダーのアクセス範囲と安全性は、取込時のレビュー済み契約に従う。

```powershell
cargo run --locked --release -p renderer-bistro --no-default-features --features vulkan,d3d12,development-validation --bin bistro -- --api vulkan --fixture --capture-shadow-maps --shader-manifest .tools/bistro-material-programs/manifest.json --output .tools/bistro-fixture-material-vulkan
```

D3D12では`--api d3d12`を指定する。全シーンの素材準備と追加の表示手順は[開発ガイド](development.md#bistro-b5)を参照する。

<a id="bistro-scene-pipeline"></a>

### Bistroの描画パイプラインを組み立てる

`renderer-bistro`の`bistro`は、Forward、Forward+、Deferredに公開PBRレシピを使う。
`--pipeline`でForward、Forward+、Forward-reference、Deferredを選び、対応するMSAAや描画形式を指定できる。
Forward-referenceは全点光源を評価する比較用の経路で、サンプルが共通パスから構成する。
各経路はシャドウからシーンのHDR、[BloomとSDR](#bistro-post-pipeline)までを同じGraphへ追加する。
Bistroはカメラ、IBL、材質、描画候補とその順序を供給し、
最終画像をキャプチャまたは表示する。描画寸法が変わるとレシピと一時画像を新しい寸法で準備する。

```powershell
cargo run --locked --release -p renderer-bistro --no-default-features --features vulkan,d3d12,development-validation --bin bistro -- --api vulkan --fixture --frames 8 --warmup 2 --capture-shadow-maps --output .tools/bistro-scene-vulkan
```

D3D12では`--api d3d12`を指定する。
点光源を含む構成は`--pipeline forward-plus --msaa 4`、G-bufferを使う構成は`--pipeline deferred`を指定する。
`--submission indirect`でGPU可視判定と不透明の間接描画、標準3方式の`--instances instanced`で直接インスタンシングを確認できる。

Bistroは公開の`ShadowHistory`に遮蔽物の描画条件と光源行列を渡す。再利用できるカスケードの画像は
前のフレームから取り込み、更新が必要なカスケードだけ深度パスを追加する。後続の照明パスは
選択されたカスケードの画像を読む。更新した画像と判定キーはsubmit受理後に次フレームで使えるようにする。
リサイズ時はキャッシュを破棄する。`--redraw-shadows`は各カスケードを毎フレーム更新する。

<a id="bistro-post-pipeline"></a>

#### BistroのHDRからSDRをつなぐ

`--bloom tent`または`--bloom box`では、公開のBloomレシピが露出前のシーンHDRをComputeで抽出、縮小、ぼかし、合成する。
`--bloom off`が既定である。標準の出力レシピはPBR Neutralと露出倍率2.0を適用し、
`--aa-off`を指定するとFXAAを省く。最終画像はオフスクリーンではsRGB、ウィンドウではPresenterがsRGBへ変換するための
線形出力になる。`--capture-bloom-bypass`はBloomを通さない参照画像も保存する。

<a id="bistro-tools"></a>

## Bistroのcook・材質shader・比較

[runtime cookと描画手順](development.md#bistro-pbr)は通常のRust buildから分離する。
`tools/bistro/mikk.py`がoffline用C helperをClangで作り、`cook.py`が固定glTFと元DDSの全mipを
実行用streamへ変換する。MikkTSpaceの固定commit、無変更upstream fileのhash、保持した
zlib-style licenseの所在は[provenance](../../tools/bistro/mikktspace/provenance.json)へ記録する。
ClangやMikk shared libraryをRendererのruntime依存へ追加しない。

標準PBRと影、BistroのBloom、Deferred、Forward+、可視性判定の正本は
`crates/renderer/shaders/`の`.hlsl`と共通`.hlsli`に置く。両方のBistroサンプルもこの資産を参照する。
[生成スクリプト](../../crates/renderer/shaders/regenerate.ps1)から
固定版DXCを呼び、同じ正本をSPIR-VとDXILに直接コンパイルする。
標準のNeutral・FXAA・SDR出力shaderとライセンスは`crates/renderer/shaders/output/`に置く。
表示プリセットのGLSLを変更した場合は次の順で標準fragmentのSPIR-V、DXIL、両者を含むPipelineのmanifestを更新する。
通常buildは同梱のバイナリを読み込む。

```powershell
& ./crates/renderer-core/shaders/regenerate.ps1
& ./crates/renderer-core/shaders/regenerate-dxil.ps1
& ./crates/renderer/shaders/regenerate.ps1
```

PBRなどPipeline側のHLSLのみを変更した場合は、Pipelineの生成と照合を実行する。

```powershell
& ./crates/renderer/shaders/regenerate.ps1 -SdkRoot C:/VulkanSDK/1.4.341.1
& ./crates/renderer/shaders/regenerate.ps1 -SdkRoot C:/VulkanSDK/1.4.341.1 -Verify
```

`-Verify`は一時directoryで再生成し、checkoutを書き換えずbinary・reflection・manifestを照合する。
CIでは`python -B tools/shaders/verify_pipeline_manifest.py`を実行し、SDKなしでHLSL、出力GLSL・ライセンス、include、バイナリ、
反射情報、ツールチェーン定義とRenderer側manifestのhashを照合する。Pipeline資産の欠落と未登録ファイルも検出する。
UTF-8/BOMなし・LFへ揃えたHLSLとincludeを両ターゲットで使い、同じbyte列をhashへ使う。
両形式のentry pointは`main`、shader modelは6.0、HLSLは2021、最適化は`-O3`。
`BISTRO_LOCAL_LIGHTS`と`BISTRO_INSTANCING`でバリアントを選ぶ。SPIR-V 1.3の出力に
`-spirv -fspv-target-env=vulkan1.1`を指定し、`spirv-val --target-env vulkan1.3`で実行環境の契約を確認する。
SPIRV-CrossはSPIR-Vのreflectionに使い、DXILはDXCの内部validatorと`-dumpbin`を使う。
PBRとGBufferの頂点clip変換は明示したFP32演算順序と`NoContraction`を保ち、DXILのfast演算やFMA等への
縮約・再結合を生成時に拒否する。camera/ABIやfragment最適化を変えず、対象をclip演算へ限定した
[採用理由とリスク](../design/implementation-policy.md#bistro-runtime-cook)に従う。
更新時はshaderとRust descriptorのABI、footprintと安全性契約をレビューする。
hash/reflectionの一致だけで未信頼shaderを安全とみなさない。

`compare_runtime.py`はrunnerの`report.json`とHDR/SDR/depthを読み、固定profileと全画素・ROIの誤差を集計する。
captureは実体解決後もreport directory内にあることを要求する。Windowsの通常/拡張長pathは
比較用表現を正規化し、実際の読込pathを保持したまま同じ包含条件で検査する。device/volume namespaceは受け付けない。
`measure_runtime.py`は通常releaseの保存済みEXEを逐次実行し、native詳細CPU timerとGPU timestampを
一緒に切り替えたoff/onを分ける。非収集の`native_phase_totals_ns`は`null`であり、query単独の費用を分離した計測とは扱わない。
これらの計測にWPRや管理者実行は不要であり、ETWの収集手順とは独立する。
正式B3比較ではGodotのoptional CPU clock GDExtensionも使い、両engineで全thread合計のcharged CPU/Nを測る。
時計はROI境界だけでQPC→`GetProcessTimes`→`GetThreadTimes`の順に取得し、frequency、生counter、caller thread IDと失敗を保存する。
Godotの固定editorから生成したABI header、source、compiler、DLL、descriptorをhashで結び付ける。
実装とbuild/smoke手順は[CPU clock](../../tools/godot_bistro/cpu_clock/README.md)を参照する。
計装はbenchmark用extensionとexample supportだけに置き、Rendererライブラリのshippingには呼出し・metadata・依存を追加しない。
`GetProcessTimes`は全threadのuser/kernel合計、`GetThreadTimes`はcallerだけで、後者を前者へ加算しない。
100 nsは戻り値の単位であり実効精度の保証ではなく、並列実行でprocess CPUがwallを超え得る。
[process時間](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-getprocesstimes)、
[thread時間](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-getthreadtimes)

QPCは待機・deschedulingを含むelapsedである。wallからprocess CPUを引いてwait時間とせず、明示yieldは別のelapsedとして残す。
CPU/frame平均、wall/N、個別frame percentile、GPU timestamp、観測完了latencyを混同しない。
時計二回、Godotのpre/post callback・sample作成、詳細timestampの文字列/map/query読出しにも費用がある。
timer/query無効と有効を独立反復し、差にはrun間変動が含まれることを明記する。
並行build/cook/重い試験/GPU/性能収集には`tools/bistro/exclusive_work.ps1`の共有lockを使い、
`FileMode.CreateNew`で取得した自分のtokenだけをfinallyで解放する。他者lockを消さない。
実行したprofile/hash・レビュー修正・二APIの内容照合と計測値は[現行検証](../verification/current.md#bistro-pbr)へまとめる。

## D3D12 bindingsの生成

`d3d12` featureはWindows限定のprivate bindingsを使う。公開Graph／Engineへnative型を出さず、
通常ビルドは[生成済みRust](../../crates/renderer-core/src/backends/d3d12/bindings.rs)を使う。
Device・単一DIRECT queue・buffer Graph実行もこのprivate生成型を使う。
COM参照やnative handleを公開runtime／Graph APIへ露出させない。

[生成tool](../../tools/d3d12-bindings/Cargo.toml)は独立したCargo workspaceとlockfileを持ち、
`windows-bindgen`、`windows-default`、間接依存の`windows-metadata`を0.100.0へ固定する。
runtimeの`windows-core`も0.100.0。各crateのMSRVは1.95で、リポジトリのRust 1.99.0を使う。
入力は固定`windows-default`の`Windows.Win32.winmd`、
API一覧は[filters.txt](../../tools/d3d12-bindings/filters.txt)を正とする。
固定crateのchecksumは二つのlockfileに保存し、生成コードのMIT／Apache-2.0ライセンスをtoolディレクトリへ同梱する。

```powershell
& ./scripts/regenerate-d3d12-bindings.ps1
& ./scripts/regenerate-d3d12-bindings.ps1 -Verify
```

`-Verify`は一時ファイルへ再生成し、固定Rustのrustfmt適用後に全byteのSHA-256一致を確認する。
生成toolの取得・実行にはRustと初回のネットワーク接続が必要だが、Windows SDKは不要である。
DXCのnative loaderは使わず、固定shaderの生成は次節のCLIを使う。
Device・診断に加えてcommand allocator／list、Fence、buffer copy・barrier・待機に必要なAPIを
filtersへ明示し、再生成・compile・検証を繰り返す。

通常workspaceとは別に、生成toolの依存も監査する。

```powershell
& ./.tools/cargo-deny/bin/cargo-deny.exe --locked --manifest-path tools/d3d12-bindings/Cargo.toml --config deny.toml check
cargo fmt --manifest-path tools/d3d12-bindings/Cargo.toml -- --check
```

`windows-link` 0.100.0と既存Vulkanの依存が使う0.2は、各crateの要求版が異なるため併存する。
COM ABIが同じでも、他の`windows` crateの型とprivate生成型のRust型互換性は保証しない。
初期のcommitted-resource基準と所有条件は[設計方針](../design/implementation-policy.md#161-第二バックエンドの選定と準備条件)を参照する。

## D3D12の固定shader生成

標準shaderとfixtureのうちtoolchain定義に列挙した管理下GLSL／SPIR-Vから、SPIRV-CrossでHLSL、DXCでDXILを生成する。
標準PBRとBistroのshaderは[HLSLからの直接生成](#bistro-tools)を使う。
入力のSPIR-Vは既存のVulkan基準を維持し、この手順では書き換えない。
通常のRustビルドは管理下のbinaryを利用し、shader compilerやSDKを要求しない。

```powershell
& ./crates/renderer-core/shaders/regenerate-dxil.ps1 -SdkRoot C:/VulkanSDK/1.4.341.1
& ./crates/renderer-core/shaders/regenerate-dxil.ps1 -SdkRoot C:/VulkanSDK/1.4.341.1 -Verify
```

`-Verify`は生成済みファイルを変更せず、不足・差分・tool不一致・変換失敗をエラーにする。
先に`regenerate.ps1 -Verify`で対象SPIR-Vの再生成一致と`spirv-val`を確認する。
[toolchain定義](../../crates/renderer-core/shaders/dxil-toolchain.json)でWindows x64 SDKの
glslc・spirv-val・SPIRV-Cross・DXC・dxcompiler.dllのSHA-256と、変換引数を固定する。
SPIRV-Crossは`vulkan-sdk-1.4.341.0`、DXCは`1.9.0.5180 (e35541826)`である。
DXCの`-WX`と通常の内部validatorを使い、`-Vd`は使わない。
採用版は外部`dxil.dll`を探索せず、常に内部validatorを使う変更後の版である。
[DXCのvalidator変更](https://github.com/microsoft/DirectXShaderCompiler/releases/tag/v1.8.2505)と
[CLI・reflectionの公式説明](https://github.com/microsoft/DirectXShaderCompiler/wiki/Using-dxc.exe-and-dxcompiler.dll)を参照する。

[生成物](../../crates/renderer-core/shaders/dxil/)にはHLSL、検証付きコンパイルで生成したDXIL、
DXCのsignature／binding／constant offsetのreflection、入力と出力のSHA-256 manifestを保存する。
shader modelは6.0、entry pointは`main`、HLSLは2021、最適化は`-O3`、debug情報・include・macroはない。
GLSLの入力hashだけはGitの改行設定に依存しないUTF-8／LFへ正規化し、binaryは全byteを比較する。
HLSL・reflection・manifestの改行は管理下の`.gitattributes`でLFへ固定し、reflectionの行末空白は除く。

[固定ABIの契約](../../crates/renderer-core/shaders/dxil-contracts.json)には次を記録する。

- Computeは8×8×1で、RGBA8 sourceは`u0`、destinationは`u1`、modeのu32は`b0`のoffset 0、4 byte。
  読み取り専用のstorage imageも既定変換ではUAVになるため、D3D12側はRGBA8 typed UAV load/store能力を照会する。
  型の扱いに注意が必要な`--hlsl-nonwritable-uav-texture-as-srv`は使わない。
- Meshは28-byte頂点、MVPはoffset 0の64 byte、tintはoffset 64の16 byte。
  HLSLの`row_major`と`mul(position, matrix)`が組になるため、CPUのcolumn-major行列bytesは転置しない。
  cbufferの`b0`はDXCが割り当てた実reflectionで確認し、明示register指定と混同しない。
- 両vertex shaderへ`--flip-vert-y`を指定し、Vulkanの正のviewport高さとの向きを揃える。
  深度は両APIで0〜1のためclip-space Z補正は行わず、culling無効・通常Z・linear RGBA8を前提にする。
- Compositeのcombined samplerはHLSLで`t0`と`s0`へ分かれるが、`texelFetch`は`Texture2D.Load`になる。
  DXCは未使用の`s0`を除去し、実reflectionのactive bindingは`t0`だけである。
- `uniform.comp`はlocal size 1×1×1、`std140`の`uvec4`をset 0/binding 0から読み、
  Storageのset 1/binding 0へ書く。両blockはmember offset 0、16 byteである。
  HLSL/DXILでは`b0/space0`と`RWByteAddressBuffer u0/space1`へ対応する。
  `managed-abi.ps1`がSPIRV-CrossのJSON reflectionとDXC reflectionを契約へ照合し、
  slot/type/count/offset/size/local sizeや余分な資源の不一致を生成前の検証で拒否する。
  SDK不要の`test-managed-abi.ps1`は改変したreflection/契約が拒否されることも確認し、CIで実行する。
  Rustのmanifest binding表も通常CPU試験で生成reflectionへ照合し、runtimeでの再reflectionを避ける。
- `storage_read.comp`は同じlocal sizeと16-byte出力を使い、入力をreadonly `std430 uvec4`の
  set0/binding0へ置き換える。HLSL/DXILはraw `ByteAddressBuffer t0/space0`の先頭16 byteを読み、
  `RWByteAddressBuffer u0/space1`へ書く。共通buffer-copy ABI checkerを使い、
  既存Uniformの照合を維持しながらreadonly/SRV register/raw read-only typeの差分を確認する。

- `sampled.comp`はlocal size 1×1×1、separate texture2Dのset0/binding0、samplerのset0/binding1、
  writeonly Storageのset1/binding0を使う。DXILでも`t0/space0`と`s1/space0`をactiveに保ち、
  `SampleLevel`でnearest/clampを実行する。出力は`u0/space1`の16 byteで、
  4 sampleをRGBA8へpackする。
  生成manifest schema3は共通verifierのhashと新assetのsource/SPIR-V/HLSL/DXIL/両reflectionを保持する。
- `sampled_float.comp`は同じimage/sampler slotとlocal sizeを使い、各sampleの全RGBAを
  `floatBitsToUint`/`asuint`で4個のuvec4へ返す。writeonly出力はoffset 0、array stride 16、
  block size 64で、DXILは`u0/space1`へoffset 0/16/32/48の`Store4`を行う。
  half格納のRGBA16FをFP32で読み、16-bit shader演算型を要求しない。
  `managed-abi.ps1`とSDK不要の改変回帰は4本のmanaged assetを確認する。

利用側のCompute受入shaderは`crates/renderer-core/tests/support/shaders/`へ置く。
`compute_hdr_sampled.comp`はsampled画像・samplerと16-byte UniformからRGBA16F storage画像へ書き、
`compute_hdr_storage.comp`はreadonly storage画像と16-byte StorageRead bufferから別のRGBA16F storage画像へ書く。
両方local sizeは4×4×1で、端数invocationをguardする。既存の再生成scriptはこれらの外部配置も列挙し、
同じ固定tool・ABI照合・hash・全件成功後の公開を使う。通常のRendererビルドへfixtureやcompiler依存を加えない。
SPIRV-Crossの既定変換ではreadonly storage画像もUAVであり、StorageRead bufferのSRVとは区別する。
typed画像format、mutability、group/register、local sizeとuniform/storageのbyte範囲をreflectionへ照合する。
利用側の不変descriptorからProgramを作るunsafe取込の保証は、生成物一致だけで代替できない。

変換optionの意味は[採用版SPIRV-Cross CLI](https://github.com/KhronosGroup/SPIRV-Cross/blob/vulkan-sdk-1.4.341.0/main.cpp)を参照する。
この生成確認は静的ABI・再現性の証拠であり、それ自体では実GPUの内容一致を証明しない。
uniformの二API buffer内容照合は[現在の検証状況](../verification/current.md#managed-uniform)へ分けて記録する。
sampled fixtureの二API内容照合は同じ[検証状況](../verification/current.md#managed-uniform)へ記録する。
一般Graph画像、非identity camera、geometry／深度、Compute／合成の二API照合は、
生成単独の確認と分けてB1の各受入と[B2の実GPU結果](../verification/current.md#bistro-pbr)へ記録する。
4／80 byteの従来論理定数ABIや、B2のcamera／材質ABIと、runtimeのroot constants／CBVの配置・range要件を区別する。

<a id="tracy-profiling"></a>

## Tracyの非管理者CPU zone調査

`tracy` featureは、Graph構築・解析と両backendのprepare/record等へ明示的なCPU zoneを追加する。
既定offで、通常構成にはTracyの呼出し・専用zone名/source location・依存を含めない。
手動で置いた区間の経過時間を調べる機能であり、WPRのCPU samplingやscheduler収集とは別である。
[Bistroの計測境界と費用比較](development.md#bistro-tracy)も参照する。

### 固定toolとbuild artifact

Rust側は`tracy-client = 0.19.0`、lockfileの`tracy-client-sys = 0.30.1`に同梱されたTracy **0.14.1**を使う。
default featuresを外し、`enable`・`ondemand`・`only-localhost`だけを選ぶ。
system tracing、CSwitch、sampling/callstack、source code転送、broadcastを有効にしない。
対応する[公式0.14.1 release](https://github.com/wolfpld/tracy/releases/tag/v0.14.1)のWindows ZIPを使い、
実行ファイルのhashと実際のhelp/versionも記録する。
feature有効時は同梱C++ clientをビルドするため、通常のRust toolchainに加えてC++コンパイラーが必要になる。
`windows-0.14.1.zip`のSHA-256は`f7499d74914aa3ba94a2c1ce72f36477d7b61d9d0f7c9790e05274c258c97fb5`。
portable版を`.tools/tracy-0.14.1/bin/`へ展開し、通常権限のPowerShellから実行する。
OSのUAC・ETW・kernel設定やsampling intervalは変更しない。

```powershell
python -B tools/cpu_profile/build.py --package renderer-bistro --bin bistro --features vulkan,d3d12 --profile release --output .tools/tracy/builds/normal
python -B tools/cpu_profile/build.py --package renderer-bistro --bin bistro --features vulkan,d3d12 --profile release --instrumentation tracy --output .tools/tracy/builds/instrumented
```

`--instrumentation tracy`がfeatureを追加し、manifestの`artifact_class`を`tracy-instrumented`として、
実際にビルドしたclient/sysの版・featureも記録する。通常artifactと同じsource・profileを使っても
別のEXE/PDB/hashとして保存する。通常build、WPR capture、通常release計測driverはTracy混入を拒否する。
defaultの`--instrumentation none`へ単に`--features tracy`を足す指定も拒否する。

### 計画・収集・停止

[専用CLI](../../tools/tracy/profile.py)は`plan`、`capture`、`export`、`analyze`を分ける。
以下は全量cookの例であり、`--manifest`を`--fixture`へ置き換えると小fixtureを使える。
各出力先は新規directoryを指定する。`plan`は対象を起動せず、予定コマンドとidentityを表示する。

```powershell
python -B tools/tracy/profile.py plan --build-manifest .tools/tracy/builds/instrumented/manifest.json --capture-exe .tools/tracy-0.14.1/bin/tracy-capture.exe --csvexport-exe .tools/tracy-0.14.1/bin/tracy-csvexport.exe --api vulkan --manifest .tools/bistro-pbr/cooked/manifest.json --view route --frames 1800 --warmup 3600 --width 1920 --height 1080 --inflight 2 --output .tools/tracy/runs/vulkan-connected
python -B tools/tracy/profile.py capture --build-manifest .tools/tracy/builds/instrumented/manifest.json --capture-exe .tools/tracy-0.14.1/bin/tracy-capture.exe --csvexport-exe .tools/tracy-0.14.1/bin/tracy-csvexport.exe --api vulkan --manifest .tools/bistro-pbr/cooked/manifest.json --view route --frames 1800 --warmup 3600 --width 1920 --height 1080 --inflight 2 --output .tools/tracy/runs/vulkan-connected
```

D3D12では`--api d3d12`と新しい出力先を指定する。両APIを同時に負荷実行しない。
build manifest、EXE/PDB、source commit/hash、cook manifest、埋込conditions、capture/export toolを再確認し、
`--tool-provenance <JSON>`を指定した場合は、記録の版・非管理者token・実際のtool path/hashと取得ZIPのhashを照合し、
provenanceとZIPもplanのinput identityへ含める。
controller/broker scriptのidentityと完了protocolも記録し、収集手順の更新をRust artifactの変更と分ける。
この環境の取得記録は`.tools/tracy-evidence/tool-provenance.json`で、公式ZIP・各EXEと同梱native headerの版を結び付ける。
controller・対象・capture broker・collector/exporterがWindows `TokenElevation=0`であることを確認する。
localhostのport 8086が既に使われている場合は開始せず、開始後もlistenerのPIDが自分の対象であることを確認する。

connected収集は次の順序で進む。

1. 対象がload・prepare・全warm-upと回収を終え、`BISTRO_READY`を出す。
2. helperが専用collectorをlocalhostへ接続し、`GO`を渡す。対象も接続を確認し、run固有のidentity zoneを出してから測定を始める。
3. 対象が測定と最後の回収を終え、`BISTRO_DONE`を出す。
4. helperが`END`を渡す。対象はHDR/SDR/depth captureとnative teardown・report保存を終え、通常returnする。
5. native Tracy clientの終了処理で両送信queueを排出し、collectorが自然終了してtraceを保存する。helperは対象・collectorの正常終了とDONE/reportの一致を確認する。

このhandshakeは`tracy` feature付きBistroだけで使う。GO/接続待ちは各30秒、END待ちは180秒で失敗する。
CLIの既定は全体300秒、END後のcollector自然終了・保存待ち120秒、出力2,048 MiBの監視上限である。
自然終了待ちの指定名は`--flush-timeout-seconds`のままで、上限120秒とする。
helperは通常の完了手順でcollectorのSIGINT/Disconnect停止を要求しない。対象の環境には`TRACY_NO_EXIT=0`を明示する。
Tracy 0.14.1の[Disconnect実装](https://github.com/wolfpld/tracy/blob/v0.14.1/public/client/TracyProfiler.cpp)は
未送信tailを捨て得るため、frame markやcollectorのquery backlog 0をqueue排出の根拠にしない。
frame markはzoneと異なるqueueを使い、全zoneが届いたことを証明するbarrierではない。
timeout・上限超過・切断・protocol不一致は成功にせず、失敗した出力とログを保持する。
既存の別collectorや他processへ停止を送らず、OSの管理者実行を要求しない。
内部のSTOP/SIGINT経路は中止・専用試験用であり、成功したcaptureの通常終了には使わない。
helperのrunごとのnonceはREADY/DONE/reportと、測定窓の外に置く`bistro.capture_identity`へ対応付ける。
解析では同じnonceのzoneを一つ確認し、別対象・別runのtraceを取り違えない。
保存traceはROI後の画像readback・teardownも含む。解析は完全なmeasurement root内のzoneだけを選び、これらを定常費用へ混ぜない。

切断状態の費用は同じinstrumented EXEをcollectorなしで測る。

```powershell
python -B tools/tracy/profile.py capture --build-manifest .tools/tracy/builds/instrumented/manifest.json --mode disconnected --api vulkan --manifest .tools/bistro-pbr/cooked/manifest.json --view route --frames 1800 --warmup 3600 --width 1920 --height 1080 --inflight 2 --output .tools/tracy/runs/vulkan-disconnected
```

disconnected runも測定開始/終了の未接続状態をreportで確認するが、traceは採取せずexport対象にはしない。
この二境界の観測だけから、途中にも一度も接続がなかったと証明したことにはしない。
on-demandは切断中のzone記録を避ける設定であり、clientの起動・background thread・初回location準備・
呼出し等の費用がゼロになる保証ではない。Tracy有効時の専用確保は通常Rendererのゼロ確保受入と分ける。

### exportと数値の意味

```powershell
python -B tools/tracy/profile.py export --capture .tools/tracy/runs/vulkan-connected/capture.json --output .tools/tracy/exports/vulkan-connected --max-output-mib 2048
python -B tools/tracy/profile.py analyze --export .tools/tracy/exports/vulkan-connected/export.json --output .tools/tracy/analysis-vulkan.json
```

`capture_completed: true`は所有processのprotocol、通常終了とtrace保存が終わったことを表す。
collectorの`natural_client_termination: true`／`collector_stop_requested: false`と対象exit 0を要求する。
traceの内容受入は別であり、nonce・ROI・frame数・nesting等をexportで確認して初めて`trace_content_verified: true`となる。
captureと各export stageのbyte上限はpolling監視で、書込みやflushにより一時的に超え得る。未検査のtraceを受入済みと扱わない。
exportの`--max-output-mib`は16〜65,536 MiB、既定2,048 MiBで、各native stageのstdout＋stderrに適用する。
全export合計や、続く解析の一時disk容量に対する上限ではない。

Tracy 0.14.1の[csvexport実装](https://github.com/wolfpld/tracy/blob/v0.14.1/csvexport/src/csvexport.cpp)に合わせ、
raw CPU zoneを`-u`、selfを`-u -e`、区切りを`-s`のtabで出す。この版にframe exportの`-F`はない。
zone名・source pathのcommaを区切りと誤認せず、header、列数、整数、inclusive/selfのinstance対応、
親子区間とself算術を検査する。欠落・交差したzoneや不完全なROIをゼロで埋めない。
csvexportに現れるのは完了した正の長さのzoneであり、出力されなかった区間の網羅性は確認できない。
raw eventの照合・sort・percentile算出は、Python標準SQLiteのdisk-backed workspaceで行う。
一時DBはexport先と同じdirectoryに作り、正常終了・例外時の後片付けで削除するため、解析用の空きdiskも必要になる。
SQLite cacheの設定8 MiBとevent batch等の上限は、process全体のRSS上限ではない。
SQLiteの一時bookkeeping、返すsource/edge/thread集計とOS file cacheは分けて扱う。

| 指標 | 解釈 |
|---|---|
| inclusive | nested child、wait、deschedulingを含むzoneのwall elapsed。親と子の合計を再加算しない |
| self | inclusiveから直接のchild区間を除いたwall elapsed。OS charged CPUやsampling weightではない |
| call数・calls/frame | 明示zoneの出現数と測定frame数あたりの比。未計装関数の呼出数は分からない |
| per-call p50/p95 | 個々のzone instanceの分布。frameのpercentileとは別 |
| per-frame集計 | main threadの各`bistro.frame`内に完全に入る呼出しの合計。該当呼出しがないframeも0で数える |
| frame zone p50/p95 | `bistro.frame`のCPU iteration時間。最終drain、frame間gap、表示latencyを含まない |

解析は一つの完全な`bistro.measurement`と指定数の`bistro.frame`を要求する。
viewerのbootstrap/continuous frame mark数や未対応のframe CSVを根拠にframe数を補わない。
同じ始終時刻で親子を一意にできない関係は曖昧なまま記録する。
複数threadのzone時間の総和は、同時進行により測定窓のwall時間を超え得る。
手動zoneが囲むhost処理のsource位置は得られるが、未計装のdriver/OS内部やschedulerのRunning/Ready/Waitingを帰属できない。
GPU timestampは既存の別計測であり、CPU/QPC/Tracy/GPUの絶対時刻対応をこの導入では確立しない。
通常・切断中・接続収集中の費用と、実際に観測したhotspotは[現在の検証](../verification/current.md)へ記録する。

<a id="cpu-profiling"></a>

## WindowsのCPUプロファイル

[CPU調査CLI](../../tools/cpu_profile/capture.py)は、保存したEXE/PDBの確認、
WPRによる所有セッションの収集、WPA/xperfのCLI出力、PIDと計測窓による集計を分けて行う。
GUI操作は必要ない。CPUサンプリング、schedulerのRunning／Ready／Waiting、
Rendererのphase経過時間とGPU timestampは異なる指標である。
手順とツールの準備だけでは実サンプルの帰属を確認したことにならず、
実行済み範囲・権限による未実行項目は[最新の検証結果](../verification/current.md)へ記録する。

### 環境とEXE/PDBの保存

Windows x64/MSVCの通常ビルド環境、Python 3.11以降、Windows Performance Toolkitの
WPR・WPA exporter・xperf、行解決にはLLVMの`llvm-symbolizer.exe`を用意する。
収集には管理者としてのCPU/ETW記録権限が必要である。CLIはUAC昇格、権限の付与、
sampling intervalやregistry設定の変更を行わない。
通常のbuild・CPU試験にこの収集権限やWPTを要求しない。

既定のWPRは`C:/Windows/System32/wpr.exe`、WPTは
`C:/Program Files (x86)/Windows Kits/10/Windows Performance Toolkit/`、
LLVMは`C:/Program Files/LLVM/bin/llvm-symbolizer.exe`を使う。
異なる配置には各CLIの`--wpr`、`--wpt`、`--symbolizer`を明示する。
WPRの`-help advanced`／`-help tracing`、WPA exporterとLLVMのhelpで、
使用版のinstance指定、marker、CSVとJSON形式を確認する。

```powershell
python -B tools/cpu_profile/build.py --profile profiling --example profile_workload --features vulkan,d3d12 --output .tools/cpu-profile/builds/workload-profiling
python -B tools/cpu_profile/build.py --profile release --example profile_workload --features vulkan,d3d12 --output .tools/cpu-profile/builds/workload-release
```

出力先は新規または空のディレクトリとする。`profiling`はreleaseの最適化を引き継ぎ、
full debug情報とstripなしだけを変更する。`release`は通常構成との費用比較用で、
PDBが存在しても行情報を得られるとは限らない。両構成を同条件で測り、debug情報を加えた差も記録する。
helperは専用のCargo出力領域を作り、実行ファイル、対応するPDB、`manifest.json`を保存する。
manifestにはCargoパッケージとexample、source/lock/toolchain、要求・実効feature、観測したCargo profile、EXE/PDBのhashとGUID/ageを含める。
`--package renderer`を選ぶと両crateのソースを記録する。
Bistroの両バイナリは、`renderer-bistro`と依存パッケージ（Renderer、Pipeline、サンプル補助）のソースをビルド記録に含める。
BistroのRustレシピと固定条件`tools/bistro/conditions.json`もsource hashの対象になる。
capture/export/行解決は保存したbytesを再検証し、後のCargo buildで置換される`target/`のEXEを直接使わない。

既定のパッケージは`renderer-core`、exampleは`windowed`、featuresは`vulkan,d3d12,presentation`であるため、
上のヘッドレス用指定を省略しない。変更中のsourceは既定で拒否し、
調査上必要なときだけ`--allow-dirty`で実際のhashを記録する。
profile/rustflags/compilerの環境・Cargo設定による上書き、
`development-validation`、backend/Graph diagnostics、`memory-profiling`の混入は拒否する。
正しさ・メモリの受入は[別構成の実GPU実行](development.md#cpu-profile-workload)を使う。

### 計画と収集

`plan`は対象を起動せず、再検証したartifact、引数、出力先、上限、一意なWPR instanceと実行予定コマンドをJSONで表示する。
次は4資源のupload/copy/readback負荷である。256資源との比較では`--resources 256`とし、
API、seed、poll方針、warmup、durationを揃える。各runに新しい`--run-id`を使う。

```powershell
python -B tools/cpu_profile/capture.py plan --manifest .tools/cpu-profile/builds/workload-profiling/manifest.json --run-id vulkan-4-yield --handshake --timeout-seconds 90 --max-output-mib 2048 -- --api vulkan --resources 4 --seed 1 --warmup-ms 500 --duration-ms 3000 --poll yield
python -B tools/cpu_profile/capture.py capture --manifest .tools/cpu-profile/builds/workload-profiling/manifest.json --run-id vulkan-4-yield --handshake --timeout-seconds 90 --max-output-mib 2048 -- --api vulkan --resources 4 --seed 1 --warmup-ms 500 --duration-ms 3000 --poll yield
```

実収集のコマンドだけを、CPU記録権限のあるPowerShellで実行する。
管理者PowerShellを一度開けば、そのprocessで複数のcaptureを続けて実行でき、各runで再昇格しない。
新しい管理者processを作るときのUAC操作と、既存process内でのcaptureを区別する。
`--handshake`は対象へ`--capture-handshake`を追加し、次の順序で進める。

1. 対象が準備・warmupを終え、PID/QPC情報付き`PROFILE_WORKLOAD_READY`を出す。
2. helperが自分のWPR instanceを開始し、外部begin markerを送り、stdinへ`GO`を渡す。
3. 対象が計測を終えて`PROFILE_WORKLOAD_DONE`を出したら、helperがend markerを送り、WPRを保存停止する。
4. helperが`END`を渡し、対象がnative teardown後の`PROFILE_WORKLOAD_REPORT`を出して終了する。

READY後のGO待ちは30秒、DONE後のEND待ちは180秒で失敗する。
markerはこの窓の外側を囲むため、IPCやmarkerコマンド自体の時間を含み、
DONEのQPC計測窓と同一ではない。markerへ`-flush`を付けない。
markerコマンド失敗は収集成功にせず、実ETL内のmarker存在とQPC/ETL時刻の対応は別途確認する。

表示例や既存の実行ファイルを起動から収集する場合は`--handshake`を省く。
例えば`--example windowed --features vulkan,d3d12,presentation`で別archiveを作り、
対象引数を`--api vulkan --measure --frames 4096 --warmup 24 --vsync off`へ置き換える。
この場合はstartup/warmup/終了も含む。`--measure`のnative計測を有効にした費用と、
headless負荷のnative metrics無効を区別する。

既定は`CPU.Verbose`のfile modeで、SampledProfile・CSwitch・ReadyThreadのstackを収集する。
追加providerが必要な調査だけ`--wpr-profile 'path/to/custom.wprp!Profile.Verbose'`を指定し、
必要なkeyword/stackが維持されていることを確認する。独自marker providerや毎frameのspanは必須にしない。
`--cwd`で対象の作業場所、`--output-root`で出力の親を変更できる。

既定の出力は`.tools/cpu-profile/<run-id>/`にある`capture.etl`、`capture.json`、
対象stdout/stderrと各WPRコマンドのログである。
PID、起動/終了UTC、実行引数、上限、所有instance、成功したstartとcleanup結果を保持する。
全session操作で`-instancename`を最後に置き、stop/cancelは自分のstartが成功した場合だけ実行する。
start失敗後に別セッションの停止・一括cancelで復旧しない。
cleanup失敗時は`capture.json`のinstanceとコマンドログを確認し、同じ所有instanceだけを調査する。

`--timeout-seconds`は0超〜3600秒、`--max-output-mib`は16〜65536 MiB。
対象/通常コマンドを残時間で監視し、通常WPRコマンドは各30秒、stop/cancelは各120秒までとする。
容量上限は実行中のpolling guardで、OSのhard quotaではない。file modeのstop/merge中は一時的に上限を超え得るため、
開始時に指定上限の2倍の空き容量を要求し、最終出力が上限を超えたrunも失敗にする。
target・marker・status・quotaが正常でも、`capture_valid`はstack/symbolやイベント欠落の受入を意味しない。

### CLIでの出力と集計

```powershell
python -B tools/cpu_profile/capture.py export --capture .tools/cpu-profile/vulkan-4-yield/capture.json
Get-ChildItem .tools/cpu-profile/vulkan-4-yield/export -Filter *.csv
```

exportは保存したmanifest/EXE/PDB/ETLのhashを照合し、installed AppLaunch profileのCPU定義から
重複metricを除き、`KeyColumnCount=0`でleaf-event出力を要求するprofileを生成する。
対象の正確な`name (PID)`でSampled/Preciseをfilterし、preciseにはOld/Newの両process/thread identityを残す。
installed WpaRuleMatchExporter profileのImages/Processesも同じ対象へ絞り、実load baseとimage/processの生存期間を出力する。
Imagesの`Start Address`は実load addressであり、`Default Start Address`やmanifestのpreferred baseと区別する。
filter後もanalyzerで対象PIDと窓を確認する。

既定ではxperfのtrace統計、sampling頻度、sampled関数、CSwitch、nanosecond単位のmarksと
raw-QPC/ETL時刻付きのheader限定dumpを保存する。marks/raw-headerの各コマンドはstdoutとstderrの合算を
8 MiBまでに制限し、上限超過では保持したprefixとstage失敗を残す。CSVや他のraw分析全体のhard quotaではない。
`export.json`に各stageの開始/終了、timeout/失敗、exit code、profile/CSV hash、観測できたloss fieldを残す。
生成profile、CSVヘッダー・単位・stack表現は使用WPT版の最初のETLで確認する。
未検証のprofileやfixture試験だけを実収集の受入にしない。
`--wpa-profile`で入力となるvendor profile、`--output`で新規出力先、
`--timeout-seconds`で各分析コマンドの上限（既定300秒、最大3600秒）を変更できる。

Ready Thread StackはWPA preciseのCSVを使う。追加のxperf `readythread -stacks`は
`--ready-stacks-fallback`を指定した場合だけ実行する。
一部のWPT版で実際に`0xc0000005`終了と大量のstdoutが発生したため、通常exportには含めない。
この追加分析の失敗を成功に置き換えず、rawログとstage結果を確認する。

symbol検索はmatching archiveとrun内のsymcacheを使う。`--microsoft-symbols`を明示したときだけ
Microsoft symbol serverを追加する。raw ETLには対象以外のsystem/process情報、path、stack等が含まれ得る。
ETL、PDB、cache、ログは`.tools/`にローカル保存し、自動uploadやGit追加は行わない。
共有時は含まれる情報と共有範囲を確認する。

CSVの実ファイル名を確認して`$sampledCsv`と`$preciseCsv`へ設定する。
handshakeを使った`profile_workload`では、[clock変換CLI](../../tools/cpu_profile/trace_clock.py)で
測定QPC窓をETL相対秒へ変換する。次はexportが保存したtext証拠を使い、ETLのhashを再検証する。
native分析コマンドは再実行しない。

```powershell
python -B tools/cpu_profile/trace_clock.py --capture .tools/cpu-profile/vulkan-4-yield/capture.json --header .tools/cpu-profile/vulkan-4-yield/export/raw-header.stdout.txt --statistics .tools/cpu-profile/vulkan-4-yield/export/trace-statistics.stdout.txt --marks .tools/cpu-profile/vulkan-4-yield/export/markers.stdout.txt --output .tools/cpu-profile/vulkan-4-yield/clock.json
$traceClock = Get-Content .tools/cpu-profile/vulkan-4-yield/clock.json -Raw | ConvertFrom-Json
$roiStartSeconds = $traceClock.roi.start_seconds
$roiEndSeconds = $traceClock.roi.end_seconds
```

同じimmutable ETLから生成したtext証拠を渡す。helperはcaptureの所有/成功状態、PID/QPC frequency、ETL hash、
header/statisticsのTrace Name、単一traceの明示的な`PerfCounter` clockを要求する。
raw QPCとETL nanosecondのpairを2種類以上の時刻で照合し、全pairが同じ起点を正確に示すことと、
QPC窓が観測したheader時刻の最小〜最大内にあることを確認する。
全TraceLengthへの包含を代用せず、clockや丸めでpairが一致しない場合も許容誤差で吸収しない。
起点と窓のexact rational fieldを正本にし、上の数値秒はanalyzerへ渡す近似値として扱う。

`--marks`を渡すと、この所有instanceのBEGIN/ENDがETL内に各1件だけあり、順序とQPC窓の外囲を満たすことも確認する。
省略時は実marker存在が未確認のままで、markerコマンドの終了codeから存在を推定しない。
出力は親の存在する新規JSONファイルとし、入力textのhashとloss情報を保持する。
欠けたloss fieldはunknownのままで、このclock変換だけではlossゼロやstack/symbolの網羅を受け入れない。
textのhash/Trace Nameは渡した証拠のidentityを残すもので、同じETLから生成した来歴はrun記録でも保つ。

このhelperはhandshake/QPC情報のある負荷だけを扱う。
`windowed`等の通常captureはstartup/warmup/teardownを含むため、Processes/Imagesの生存期間を確認し、
対象PIDの範囲内で選んだETL相対秒を`$roiStartSeconds`と`$roiEndSeconds`へ明示する。
起動/終了UTCや、QPC絶対tickを単に秒へ割った値からROIを推定しない。

```powershell
python -B tools/cpu_profile/capture.py analyze --capture .tools/cpu-profile/vulkan-4-yield/capture.json --sampled $sampledCsv --precise $preciseCsv --start-s $roiStartSeconds --end-s $roiEndSeconds --output .tools/cpu-profile/vulkan-4-yield/summary.json
```

analyzerはtarget PIDを抽出し、Count=1のleafイベント、明示的な単位、0x付きの実IPを要求する。
集約/subtotal、重複header/event、非有限数、逆転時刻、同じTIDの重複状態区間を拒否する。
使用版のbare timestamp列の単位を確かめた場合だけ`--timestamp-unit s|ms|us|ns`で指定する。
bare durationの単位を推測せず、sample weightは`Weight (ms)`または正規化した`weight_ms`を使う。
stackは明示したroot-to-leafの`stack_json`、または確認済みの`--stack-separator`でのみinclusive frame集計を行う。
vendor出力の表現がこの契約に合わなければ拒否原因とraw CSVを保持し、実出力を確認してadapterを更新する。

正規化CSVで使う必須列は次のとおり。`*_s`は秒、`weight_ms`はms、`count`は1とする。

| 表 | 正規化列 |
|---|---|
| sampled | `pid,tid,cpu,time_s,ip,module,function,count,weight_ms`。任意の`stack_json`はroot-to-leafの文字列配列 |
| precise | `pid,tid,cpu,count,last_switch_out_s,ready_time_s,switch_in_s,next_switch_out_s,previous_state,wait_reason,wait_stack,ready_stack`。任意の`old_pid,old_tid`はself-switchの識別用 |

sampleのCountは関数呼び出し回数ではなく標本数、Weightは実行中CPU時間の推定である。
selfはmoduleとfunctionを組にし、未知symbol/IPの費用も残す。
inclusiveは入力frame文字列への帰属で、recursive frameは一標本につき一度だけ加算する。
inclusive値の合計をCPU総時間にしない。標本timestampによる窓選択には境界の近似と短い処理の見落としがある。
preciseはRunning、実行可能だが未割当のReady、イベント待ちのWaitingを別々に窓へclipする。
Waitingはlast switch-out→readyをblocked、ready→switch-inをreadyとする。
Ready/Standby/DeferredReadyはlast switch-out→switch-inをreadyとし、過去のwakeに由来するReady Timeでblockedを作らない。
Old/Newのprocessとthreadが同一と明示されたself-switchだけは、そのeventのoff-CPUを0とする。
近接したtimestampだけから同一identityやself-switchを推定しない。
Waitingの待機理由/New Thread Stack、起床させた側のReady Thread Stack、runnable側のNew Thread Stackを分ける。
stackは状況の手掛かりであり、遅延原因を単独で証明しない。
不足した端点/状態は`unclassified_off_cpu_ms`やmissing-startとして残し、ゼロで埋めない。
coverage完了fieldは観測できた区間の分類であり、trace境界外や欠けた開始点まで計測済みとはしない。
TID再利用はそのIDの合計として残す。
複数threadの合計はwall timeを超え得る。通常のsleep/pacingもWaitingになり、長い待機だけでは欠陥と断定しない。

### 実IPからソース行を確認する

対象PIDと時刻に対応するImage_Loadの実ASLR baseを`$capturedImageBase`へ設定する。
manifestのPE preferred baseや、decoded関数の先頭アドレスで置き換えない。

```powershell
python -B tools/cpu_profile/symbolize.py --manifest .tools/cpu-profile/builds/workload-profiling/manifest.json --image-base $capturedImageBase --summary .tools/cpu-profile/vulkan-4-yield/summary.json --top 50 --output .tools/cpu-profile/vulkan-4-yield/source.json
```

matching EXE/PDBを再検証し、実sample IPから実baseを引いたRVAをLLVMへ渡す。
IP/RVA、sample weight、inline chain、filename/line、対象EXE外と未解決を分けて残す。
このhelperは指定したEXEだけを解決し、driver/DLL等の行情報を同じbaseへ結び付けない。
最適化やinlineにより行の意味は曖昧になり得る。既知RVAのoffline行解決はtoolの確認であり、
実サンプルで同じsource帰属が成立した証拠は実ETLから別途得る。

採用する測定では、status/trace統計のlost events/buffers、sample/stack数、symbol・行の解決率、
対象PID/生存期間/ROI、WPRmarkerとQPC窓の差を確認する。欠落fieldは未確認のまま残す。
`-tle`や`-tti`で欠落・時刻逆転を許可して正確な集計を主張しない。
ETW付き/なし、profiling/release、二API、4/256資源、poll方針を変えた比較は逐次実行し、
raw結果と測定条件、揺れ、未実行項目を[検証結果](../verification/current.md)へ記す。

CLI契約のCPU試験は実ETW/GPUを起動しない。

```powershell
python -B -m unittest discover -s tools/cpu_profile -p "test_*.py"
```

一次資料は[WPR CLI](https://learn.microsoft.com/en-us/windows-hardware/test/wpt/wpr-command-line-options)、
[CPU分析](https://learn.microsoft.com/en-us/windows-hardware/test/wpt/cpu-analysis)、
[WPA exporter](https://learn.microsoft.com/en-us/windows-hardware/test/wpt/exporter)、
[LLVM symbolizer](https://llvm.org/docs/CommandGuide/llvm-symbolizer.html)を参照する。

## RenderDoc

採用版は**1.47 x64**。この環境では[公式portable ZIP](https://renderdoc.org/stable/1.47/RenderDoc_1.47_64.zip)を
`.tools/renderdoc/RenderDoc_1.47_64/`へ展開している。
版を更新するときは[公式リリース](https://github.com/baldurk/renderdoc/releases/tag/v1.47)と
[公式配布ページ](https://renderdoc.org/builds)から配布物と署名を取得する。

```powershell
& ./.tools/renderdoc/RenderDoc_1.47_64/renderdoccmd.exe version
```

このportable配置のcapture layerはregistryへ登録していない。
確認済みのVulkan Loader 1.4.341では、通常権限で起動するRenderDocへ
`VK_ADD_IMPLICIT_LAYER_PATH`を渡してmanifestを発見させる。
次の例は既存値を保ち、RenderDoc終了後に元へ戻す。

```powershell
if ($env:VK_IMPLICIT_LAYER_PATH) {
    throw 'VK_IMPLICIT_LAYER_PATH overrides the additional layer path; review the current loader settings.'
}
$rendererCapturePath = (Resolve-Path -LiteralPath './.tools/renderdoc/RenderDoc_1.47_64').Path
$rendererPreviousLayerPath = $env:VK_ADD_IMPLICIT_LAYER_PATH
try {
    $rendererLayerPaths = @($rendererCapturePath, $rendererPreviousLayerPath) |
        Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
    $env:VK_ADD_IMPLICIT_LAYER_PATH = $rendererLayerPaths -join [IO.Path]::PathSeparator
    & (Join-Path $rendererCapturePath 'qrenderdoc.exe')
} finally {
    $env:VK_ADD_IMPLICIT_LAYER_PATH = $rendererPreviousLayerPath
}
```

RenderDocが対象プロセスへcapture有効化の環境変数を渡すため、
共通の開発環境へ`ENABLE_VULKAN_RENDERDOC_CAPTURE`を設定しない。
管理者権限のアプリでは環境変数によるmanifest指定が無視される。
この動作は[使用中のloaderの仕様](https://raw.githubusercontent.com/KhronosGroup/Vulkan-Loader/v1.4.341/docs/LoaderLayerInterface.md)を参照する。

qrenderdocから対象の描画・Compute実行ファイルを起動して捕捉し、
コマンド列、pipelineやbinding、buffer／imageの内容を確認する。
Presentを行わないoffscreenプログラムは、通常のPresent基準の捕捉トリガーでは範囲を決められない。
明示APIで捕捉する場合は`StartFrameCapture`／`EndFrameCapture`で対象区間を挟む。

[公式APIの推奨](https://raw.githubusercontent.com/baldurk/renderdoc/v1.47/docs/in_application_api.rst)に従い、
RenderDocから起動されたときに読み込まれたAPIを利用する。
通常実行や公開Renderer APIへRenderDocを必須依存として持ち込まない。
導入確認ではCLIとDLLのAPI取得に加え、子プロセスでinstance／deviceのcapture layer挿入を確認した。
実際のGPUキャプチャはまだ行っていない。
