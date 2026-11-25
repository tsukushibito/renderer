# Examplesの実行と選び方

リポジトリのルートでコマンドを実行します。RustとC/C++のセットアップは
[必要なツール](development.md#必要なツール)、GPUの検証設定は
[製品・開発用の検証設定](development.md#validation-settings)を参照してください。

## ゲーム更新を Rust で書く

小さな入力・更新ループは `cargo run --locked -p sashiko --example fixed_game` で実行できます。[サンプル](../../crates/sashiko/examples/fixed_game.rs)は右移動を 3 更新続け、その間のジャンプ押下を 1 回消費します。ゲームはキーとアクションの対応を `ActionMap` に渡し、固定更新ごとに `sample` で押下・解放・軸値を読み取ります。Windows ホストでは `WindowInput` が同じマップに物理キーの変化を渡します。

<a id="survivor"></a>

## 研究施設のサバイバーを遊ぶ

ゲームパッドの左スティックで移動し、十字キーの左右で強化を選び、下側のフェイスボタンで決定・再挑戦します。マウスの左ボタンでも選択中の強化を決定できます。

Windows の既定構成では Vulkan のウィンドウが開きます。WASD で実験台を回り込み、敵を自動攻撃して経験値を拾います。レベルアップ時は 1・2・3 で強化を選ぶか、左右の矢印キーでフォーカスを移動して Enter で決定します。

```powershell
cargo run --locked --release -p survivor
cargo run --locked --release -p survivor -- --scenario endgame --seed 506
```

配置は [arena.json](../../assets/survivor/arena.json) を編集し、`--arena assets/survivor/arena.json` で読み直せます。座標は水平面の X・Z をメートルで指定します。次の例はウィンドウの各更新で消費した入力を保存し、同じ開始条件で GPU なしで再生します。終了時の JSON にはゲーム状態と接触・到達の累計が含まれます。

```powershell
cargo run --locked --release -p survivor -- --seed 506 --record run-input.json
cargo run --locked --release -p survivor --no-default-features -- --headless --replay run-input.json
```

画面から記録したリプレイは選択した外観、キャラクターカタログの定義、使用したメッシュ・ポーズ・色画像と床・鋼板の画像のハッシュを保持します。描画再生では選択を復元し、素材が変わった場合は開始時に該当する記録との不一致を報告します。同じ素材を別の場所に置く場合は `--character-catalog` でその場所を指定します。CPU 再生は記録内のシーンを使い、外観のファイルを必要としません。

通常プレイで初めて敵を撃破すると別の実験スーツが解放され、次に起動した通常プレイで反映されます。Windows の保存先は `%LOCALAPPDATA%/sashiko/survivor` です。`--profile-status` で解放状態と設定を JSON 表示できます。`--width`、`--height`、`--volume`（0〜1）を指定した通常プレイでは次回の設定も更新します。再生・記録・計測・ヘッドレスでは毎回既定設定から開始します。`--user-dir <directory>` を明示すると、その専用プロファイルで保存の試験ができます。

配布用には release 実行ファイルと cook 済み素材をまとめます。出力には新しいディレクトリを指定します。パッケージ内で相対配置を保ったままディレクトリごと移動できます。検証コマンドはチェックサムを照合し、一時作業ディレクトリからプレイ・保存・再起動と設定復元を実行します。`--backend vulkan` または `--backend d3d12` を付けると、別プロセスで配布版のウィンドウを起動し画像の読み戻しまで確認します。

```powershell
cargo build --locked --release -p survivor
python tools/package_game.py --project assets/survivor/package.json --exe target/release/survivor.exe --output build/survivor-package
python tools/survivor/verify_package.py build/survivor-package --backend vulkan
```
記録には入力列、開始時の配置、seed、シナリオ、ゲームルールの版、更新周波数を保存します。再生時は記録した設定を使い、明示した設定と矛盾した場合はエラーを返します。入力だけの JSON 配列は CLI の開始条件で再生できます。

終盤直行では実験体 500 体を活動させます。初回の計測条件と受入の範囲は[ゲームの制作計画](../design/survivor-game.md)にまとめています。

<a id="embedded-game"></a>

### ゲーム画面をアプリに組み込む

Windows x64、.NET 10 SDK と上記の Rust 環境で、次のプロジェクトを Visual Studio から開くかコマンドで起動します。ビルド時にゲーム DLL とキャラクター素材を出力先へ配置します。

```powershell
dotnet run --project examples/survivor-winforms -c Release
```

起動直後は一時停止しています。X・Z を変更して **Place player** を押すと、通行できる位置にプレイヤーが移動します。**Step** は1回の固定更新、**Play** は連続再生です。ゲーム画面にフォーカスがある間だけ操作を受け付けます。フォームにビューを配置する例と、ゲームが実装する接続・入力・描画の契約を参照してください。[フォーム](../../examples/survivor-winforms/Program.cs)・[SashikoView](../../hosts/winforms/SashikoView.cs)

Web 版は同じ研究施設を俯瞰図で表示します。次のコマンドで静的サイトを作成し、ブラウザで `http://localhost:8000/` を開きます。ブラウザがゲームパッドを公開するには、ページを開いた後に実機のボタン操作が必要になることがあります。

```sh
rustup target add wasm32-unknown-unknown
python examples/survivor-web/build.py
python -m http.server 8000 --bind 127.0.0.1 --directory build/survivor-web
```

既存ページに組み込むときは、生成した `game.js` の `attachSurvivor(canvas, wasmUrl)` に表示先を渡します。返された `view.frame(timeInMilliseconds)` をホストの描画周期で呼ぶか、`view.start()` で標準ループを使います。ビューを外すときは `view.dispose()` を呼びます。[Canvas ホスト](../../hosts/web/view.js)

### 配置・プロパティ・クリップを編集する

Windows で次をリポジトリのルートから実行します。画面を使う場合は Tk を含む Python 3 が必要です。

```powershell
cargo build --release --locked -p survivor --bins
python tools/editor/scene.py assets/survivor/arena.json --inspector target/release/survivor-inspector.exe gui --game target/release/survivor.exe --catalog assets/survivor/characters/catalog.json
```

左で配置 ID を選ぶと、右に登録型と原型、各項目の最終値・原型値・出典が表示されます。上面図のドラッグで移動し、項目をダブルクリックして数値・配列などを JSON で編集します。適用時にはゲームの読込規則でも検査し、成功した変更を開いているファイルへ保存します。「Use prefab value」は個体の上書きを取り除きます。「Play」で保存済みシーンをゲーム本体で起動し、「Inspect state」で指定 tick の状態を確認します。GUI 起動時に `--replay 記録.json` を追加すると同じ入力が両方に渡ります。「Preview clip」ではモデルとクリップを選び、ゲームと同じサンプルを形状プレビューで再生できます。

CLI でも同じ操作と検査を利用できます。例は一時シーンを編集します。

```powershell
Copy-Item assets/survivor/arena.json build/arena-edit.json
python tools/editor/scene.py build/arena-edit.json --inspector target/release/survivor-inspector.exe move bench-west -4 0 0
python tools/editor/scene.py build/arena-edit.json --inspector target/release/survivor-inspector.exe set bench-west half_extent "[1.5,1.0]"
python tools/editor/scene.py build/arena-edit.json --inspector target/release/survivor-inspector.exe reset bench-west half_extent
```

<a id="bistro-baked-lighting"></a>

### 終盤の負荷を再生する

通常の初期状態から WASD 相当の入力で施設を周回し、経験値を獲得して連射・貫通・衝撃波を選びます。
次のコマンドは敵500体を維持した最後の720 tickを含む全入力を書き出します。計測器は開始地点までの入力を
CPUで再生してから120フレームを表示してウォームアップし、続く600フレームを記録します。
序盤・既存の終盤・この入力列を各3回、通常とGPUタイムスタンプ付きで別々に測ります。

~~~powershell
cargo run --locked --release -p survivor --no-default-features --example overlap_replay -- build/earned.json
cargo build --locked --release -p survivor
python -B tools/survivor/benchmark.py --exe target/release/survivor.exe --overlap-replay build/earned.json --output build/survivor-measured
~~~

CSVの各行に敵、弾、ドロップ、強化段階と攻撃中の衝撃波を保存します。独立した起動で得る
overlap-memory.json には描画準備後、最後の提示直後、GPUの完了待ち後の資源量を記録します。
ゲーム配列の値は [使用要素, 確保要素]、VMAのバイト数はアロケーションと確保ブロックをそれぞれ示します。

GPUパスの内訳は同じリビジョンのVulkan診断ビルドで計測します。Windowsのx64 Visual Studio Developer Shellで、
RenderDoc 1.47の公式バイナリと同じ版の renderdoc/api/replay ヘッダを指定してください。

~~~powershell
cargo build --locked --release -p survivor --features renderer/vulkan-diagnostics --target-dir target/pass-capture
python -B tools/survivor/renderdoc_passes.py --exe target/pass-capture/release/survivor.exe --benchmark build/survivor-measured --replay build/earned.json --output build/survivor-passes --renderdoc .tools/renderdoc/RenderDoc_1.47_64 --headers build/renderdoc-sdk/renderdoc/api/replay
~~~

取得した各シナリオの代表フレームをRenderDocで再生し、GPUアクション時間をGraphパスのマーカーごとに合算します。
同名のアップロードなどはまとめ、提示は独立して出力します。再生時のアクション時間と通常実行のフレーム間隔は
それぞれ個別に記録します。

## ライトマップと照度プローブを使う

静的配置には面に沿った拡散間接照度を、移動・スキニングする物体にはワールド空間の照度プローブを使います。メッシュには、材質の UV0 と独立したライトマップ用 UV を一度だけ用意します。同じメッシュと材質を複数の場所に置けます。照度のアドレスは静的配置の ID が決めます。

実行時とベイクには同じ光源記述を使います。太陽は lx、点光源は cd または lm、矩形光源は lm、環境と発光材質は cd/m² で指定できます。[ライトの測光単位](tooling.md#photometric-units)

アセットビルドで、遮蔽・反射に参加する三角形、材質、テクスチャを `BakeScene` に登録します。`StaticReceiver::from_mesh` にインデックス付きメッシュ、元のテクセル寸法、配置の変換を渡すと、ワールド座標と配置の縮尺に対応した寸法を準備します。ワールド座標でクック済みのデータは `StaticReceiver` に直接渡せます。

```rust
let scene = BakeScene::new(occluders, materials, textures)?;
let layout = SurfaceLayout::pack(&receivers, 3)?;
let lights = [day, afternoon, dusk];
let settings = ProbeSettings::default();
let transport = scene.prepare_surface_bake(lights, settings.backend, 128)?;
let selected: Vec<_> = (0..layout.gpu_page_count()).collect();
let mut records = Vec::new();
layout.bake_gpu_pages(transport, &selected, |_, record, bytes| {
    write_blob(record.sha256(), &bytes)?;
    records.push(record);
    Ok(())
})?;
let mut probes = Vec::new();
scene.bake_probes(lights, settings)?.write(&mut probes)?;
let probe_hash = <[u8; 32]>::from(sha2::Sha256::digest(&probes));
write_blob(&probe_hash, &probes)?;
let index = SurfaceLightmapIndex::new(lights.len(), records, probe_hash)?;
publish_index(&index.to_bytes())?;
```

この例の `write_blob` と `publish_index` は配布先に応じてアプリ側が用意します。前者は SHA-256 に対応する不変のバイト列を書き出し、後者は必要なページとプローブの確定後に索引を公開します。ハッシュ計算には `sha2::Digest` を使います。レイアウトを固定してページを再ベイクする場合は `selected` に対象ページの番号だけを渡し、対応する索引レコードを差し替えます。プローブは独立して生成できます。光源や遮蔽物の変更が影響する範囲はビルド側で判定します。

固定照明には `day` を直接渡します。表裏を描く受光面には `double_sided: true` を指定すると、それぞれの向きで焼きます。透過マスクの判定には材質 UV とアルファカットオフを使います。ページの予算を検査する場合は `SurfaceLayout::new(&receivers, 2048, 3, Some(texel_budget))?` を使います。全配置の収録に足りなければ必要なサイズとページ数を返します。単一の配置の UV 矩形は境界を含めて１ページに収めます。巨大なメッシュはクック時にプリミティブを分割してください。

```rust
// メッシュのロード時。uvs は頂点と同じ順序の正規化 UV。
let mesh = meshes.create(MeshData::pbr(&vertices, &indices)?.with_lightmap_uvs(&uvs)?)?;
let index = SurfaceLightmapIndex::from_bytes(&level_index_bytes)?;
let baked = index.load(visible_pages, Some(&probe_bytes), read_blob)?;
let lighting = renderer.load_surface_lighting(&baked, Some(&reflections))?;

let mut curtain = RenderObject::new(mesh, cloth);
curtain.transform = gallery_transform;
let objects = [curtain.with_lightmap(curtain_placement_id), actor];
let input = SceneInput::new(&meshes, &materials, &objects)
    .with_baked_lighting(&lighting, time_of_day);
```

`visible_pages` はロードする GPU ページ番号の列です。`read_blob` は指定ハッシュのバイト列を返します。索引は読み込んだページとプローブを照合し、GPU 用のバイト列を保持します。静的な表面だけが必要なら `probe_bytes` の引数に `None` を渡せます。

`with_lightmap` は呼び出し時のメッシュと変換を記録します。配置を動かしたとき、別メッシュの LOD に切り替えたとき、またはスキンポーズを指定したときは照度プローブを参照します。変形や遮蔽物、照明、材質の変更を静的 GI に取り込むには再ベイクが必要です。プローブのみなら `scene.bake_probes(day, settings)?` と `renderer.load_probe_lighting(...)?` を使えます。鏡面プローブは独立して `scene.bake_reflection_probes(&boxes, day)?` で生成します。

拡散照明には 1～16 条件を渡せます。係数は全区間で 0～1 です。この 3 条件の例では 0.5 が独立してベイクした午後、0.75 が午後と夕方の中間です。キーは等間隔です。鏡面プローブには固定条件または両端の 2 条件を渡します。太陽の方位・色、背景、時計の進行はアプリ側で更新します。ベイクは線形の拡散照度を供給し、直接光の影と鏡面 IBL は通常の描画経路を使います。

### 局所反射プローブの初期配置を生成する

`BakeScene::place_reflection_probes` は Y-up、メートル単位の静的シーンから編集可能な配置候補を返します。探索範囲 `bounds` を指定すると、部屋や階ごとに間隔を変えて試せます。

```rust
let boxes = scene.place_reflection_probes(ReflectionPlacementSettings {
    spacing: 8.0,
    capture_height: 1.5,
    blend_distance: 0.5,
    bounds: None,
    ..Default::default()
})?;
// 必要に応じて位置・箱を編集し、空なら局所反射のベイクを省略する。
if !boxes.is_empty() {
    let reflections = scene.bake_reflection_probes(&boxes, day)?;
    reflections.write(writer)?;
}
```

Bistro と Sponza のクック済みシーンでは、同じ CLI で配置の書き出しとベイクができます。次の例の間隔は 8 m です。生成された JSON を編集してから 2 行目を実行できます。

```text
renderer-scenes-bake --manifest SCENE/manifest.json --place-reflection-probes 8 --output layout.json
renderer-scenes-bake --manifest SCENE/manifest.json --reflection-probes layout.json --day-only --output reflections.bin
```

既定の `floor` は水平な床や屋根から一定の高さに配置します。屋内に絞るには Rust API の `bounds` で高さを指定します。候補が予算を超えた場合は間隔を広げるか、探索範囲を絞ります。

面の向きや階の高さを事前に指定せず、個数を制限する場合は `remedy` を使えます。例えば次のコマンドは、一辺 4 m のボクセルから最大 18 個を選びます。

```text
renderer-scenes-bake --manifest SCENE/manifest.json --place-reflection-probes 4 --placement-method remedy --max-probes 18 --output layout.json
```

Rust API では `method: ReflectionPlacementMethod::Remedy` と `max_probes` を設定します。三角形をボクセル境界で切り、向き別に面積と重心を集計します。面に隣接する空ボクセルの中心がキャプチャ候補です。候補から見える近傍の面について、面積、距離、影響ボックス内のフェードから重みを求め、すでに選んだプローブからの改善が最大の候補を順に採用します。個数予算の既定値と上限は 32 です。

通常の候補を追加する効果が最初の半分を下回るか、通常の候補を使い切り、個数予算に余裕があると、遮蔽を評価して全枠を選び直します。狭い通路では、三角形が通るセルにも、中心に空きがあり対向する面に囲まれた候補を追加します。囲まれたプローブは柱の脇や奥に直接見える面まで影響ボックスを広げます。既存のシーンに反映するには配置JSONを再生成して、そのJSONで反射プローブを再ベイクします。ライトマップは再利用できます。[配置時間と画質の比較](../verification/current.md#remedy-enclosure)

品質チェックと自動修正を使う場合は、配置コマンドに `--repair-placement` を追加し、反射ベイクと同じ照明プリセットと環境を指定してください。次は Sponza の例です。

```powershell
renderer-scenes-bake --manifest SCENE/manifest.json --place-reflection-probes 2 --placement-method remedy --max-probes 22 --look o3de-atrium --ibl-manifest SKY/manifest.json --repair-placement --output layout.json > quality.json
```

`layout.json` はそのまま `--reflection-probes` に渡せます。標準出力の JSON には、採否と理由（`decision`）、判定用の受光点における当初・候補・採用後の誤差（`before`、`proposed`、`after`）、最大誤差の位置、品質チェックの秒数を記録します。`changed_probe_index` は0始まりです。`NoReceivers` は評価点が得られなかったことを表します。照明は昼の1条件です。Rust では生成した配置を `BakeScene::repair_reflection_placement` に渡します。[評価方法と実測値](../verification/current.md#reflection-placement-repair)

配置は静的ジオメトリで決まり、カメラ、材質の反射率、部位名を入力に使いません。アルファマスクの可視性はレイで判定しますが、テクスチャに穴がある面もボクセルを占有します。細い通路や小さな張り出しを解像したい場合はボクセルを小さくします。軸方向の 6 本のレイで合わせた箱は、斜めの壁や出入口では実際の空間をまたぐことがあります。ベイク前に JSON で位置と箱を調整できます。

### Bistro で試す

拡散照度プローブは既定で一次 SH（L1、RGB 4 係数）を使います。二次 SH（L2、RGB 9 係数）でベイクするには、下の `renderer-scenes-bake` コマンドに `--probe-sh l2` を追加してください。Rust API ではベイク時に `ProbeSettings { sh_order: ProbeShOrder::L2, ..Default::default() }` を渡します。描画側はアセットから次数を読み取ります。ライトマップのテクセル数と鏡面プローブの設定は独立です。

[Bistro の cook と IBL](development.md#bistro-b3) の後、以下をリポジトリのルートで実行します。

```powershell
cargo run --release -p renderer-scenes --features bake-tool,vulkan --bin renderer-scenes-bake -- --manifest .tools/bistro-cook/full-a3/manifest.json --ibl-manifest .tools/b3/ibl-final/manifest.json --output .tools/bistro.gi --samples 96
cargo run --release -p renderer-scenes --features vulkan --bin renderer-scenes -- --api vulkan --manifest .tools/bistro-cook/full-a3/manifest.json --ibl-manifest .tools/b3/ibl-final/manifest.json --look lumberyard-day --view bistro-corner --baked-lighting .tools/bistro.gi --pipeline forward-plus --white-sphere -14 2.7 0.65 --time-sweep --frames 5 --width 800 --height 450 --output .tools/bistro-gi
```

`--output .tools/bistro.gi` は世代を示す JSON を書きます。同じディレクトリの `lighting-objects/` に配置索引、GPU 用ライトマップ各ページ、照度プローブが SHA-256 名で入ります。配布・コピー時は `bistro.gi` と `lighting-objects/` を一緒に渡してください。同じ出力先への再ベイクでは成果物の書き込み後に JSON を置き換えます。旧世代の JSON を保存しておけば、その参照先が残っている間は旧版を再度ロードできます。従来の単一ファイルを使っていた場合は別名で再ベイクしてください。

昼の静的ライトマップを短時間で比較するには、通常の `renderer-scenes-bake` の manifest・IBL・look に `--day-only --backend vulkan --samples 128 --benchmark-seed 17 --output run-a.json` を加える。各ページを約 32,768 受光点の区画に分け、区画ごとに連続する最大 512 点を抽出する。通常のバッチ幅で積分し、全ページと照度プローブの書き出しは一時ファイルで計測する。出力 JSON に cook・照明の識別子、レイ数、抽出位置を決める seed、ページ別の件数、線形 RGB 照度、経過時間を記録する。比較する両実装で同じ seed を使い、採用候補を seed 43 でも確認する。`estimated_seconds` は、各ページの積分経過時間だけを受光点数で換算した値。GPU タイムスタンプとホストの完了待ちは重なる。どちらも積分経過時間に含まれる。

`--backend vulkan` は対応 GPU で静的ライトマップに Ray Query を使う。`vulkan-rt` はその経路を要求し、`vulkan-software` は従来の Compute BVH を選ぶ。照度プローブは両方とも Vulkan Compute で焼く。`--diffuse-reference` にこれらのバックエンドを指定すると、少数の既知の受光点を同じ実装で評価できる。CPU 高サンプル参照には `--backend cpu` を指定する。

`--fixture` は配布アセットなしでベイクと描画を試せます。`--backend cpu|vulkan|d3d12|auto` はベイクの実行先を選びます。Auto は利用可能な GPU を準備できなければ CPU を使います。`--spacing` と `--max-depth` は照度プローブを調整します。クックは有効な glTF TEXCOORD_1 を再利用し、欠損・重複する場合は xatlas で生成します。クック時の `--texels-per-metre` の既定値は 4、ベイクは 128 rays/受光テクセル、昼・中間・夕方の３条件です。`--estimate` は配置数、面数、ページ数、テクセル数、照度のデータ量を計算して終了します。`--lightmap-texels N` は条件ごとの全ページに対する上限です。超過時は書き出し前に停止します。旧形式の静的ライトマップを使っていたシーンはジオメトリから再クックしてください。`--endpoints-only` で同じ配置に昼と夕方だけを焼いて比較できます。出力には cook と照明プリセットの識別子を保存します。入力を変更したら再ベイクし、新しい出力名を指定してください。

`--scenario-blend 0.5` は中間時刻を固定します。`--time-sweep` のオフスクリーン実行は先頭、中間、末尾を保存します。`--gi-debug sources` はライトマップを橙、プローブを青系で表示し、`irradiance` は拡散照度、`weights` は有効なプローブ重みの合計を表示します。[実機の比較画像](../verification/current.md#bistro-baked-lighting)

### Bistro で局所的な鏡面反射を使う

`tools/bistro/reflection-probes.json` の各項目に、キャプチャ位置、箱の最小・最大座標、境界から内側へのフェード幅をメートルで指定します。ひさし下と奥の客席で 1 個ずつ使っています。アセットビルドでは `BakeScene::bake_reflection_probes(&placements, [day, dusk])?` を呼びます。

```powershell
cargo run --release -p renderer-scenes --features bake-tool --bin renderer-scenes-bake -- --manifest .tools/bistro-cook/full-a3/manifest.json --reflection-probes tools/bistro/reflection-probes.json --output .tools/bistro.reflections
cargo run --release -p renderer-scenes --features vulkan --bin renderer-scenes -- --api vulkan --manifest .tools/bistro-cook/full-a3/manifest.json --ibl-manifest .tools/b3/ibl-final/manifest.json --look lumberyard-day --view bistro-corner --reflection-probes .tools/bistro.reflections --baked-lighting .tools/bistro.gi --frames 5 --width 960 --height 530 --output .tools/bistro-local
```

拡散と鏡面は個別に有効化できます。局所反射だけなら `ProbeLighting { diffuse: None, reflections: Some(&reflections) }` をロードします。標準 PBR が BRDF LUT を用意します。大域反射も使う場合は `input.environment` を指定します。

ベイク時の `--day-only` は昼だけを保存します。`--ibl-manifest FILE` は `lumberyard-day` の大域反射と同じ経緯度画像の先頭 mip と強度を使います。昼夕をベイクする場合、夕方の空は昼からの RGB 比で着色します。実行時は同じ IBL と look を指定してください。画像・向き・強度はロード時に照合されます。

反射の配布形式は `cook.py --texture-policy` の JSON で `{"reflection_probes":"bc6h"}`（既定）または `"rgba32f"` を指定します。`renderer-scenes-bake` は cook 済み manifest から設定を読み、比較時は `--reflection-format rgba32f` で上書きできます。BC6H でベイクする端末では `python -m pip install -r tools/bistro/requirements.txt` を実行した環境を使ってください。実行時は両形式とも同じ `--reflection-probes FILE` で読み込みます。

`--reflection-debug weights` は第 1 箱を赤、第 2 箱を緑、グローバル寄与を青で表示します。`--reflection-debug radiance --time-sweep` は材質 BRDF を掛ける前の HDR を昼・中間・夕方で保存します。`--reflection-spheres -10 0.85 0 --reflection-spheres-to 7 0.85 7` を標準の direct 描画に追加すると、粗さ 0.08 と 0.55 の金属球が路上からひさし下を通って客席へ移動します。通常表示、プローブ無効、重み表示、radiance の各出力を使う[数値比較と実機画像](../verification/current.md#bistro-local-reflections)も参照してください。

## メッシュと材質からビューを描く

```powershell
cargo run --locked --release -p sashiko-renderer --features vulkan --example textured_view -- vulkan
```

| 例 | エンジンに取り込める処理 |
| --- | --- |
| `textured_view` | チェッカーテクスチャを使う材質の作成、64×64から32×32へのリサイズ、メッシュと材質の更新 |
| `two_views` | 一度バインドしたシーンを複数のカメラから描画し、別々の画像に出力 |
| `toon_views` | 外部クレートでも実装できる `ViewPipeline` でメッシュと材質を利用 |
| `mixed_post` | PBR、Unlit、独自材質、半透明を含むシーンのHDRにBloomとSDR出力を追加 |

コマンドの `--example` の値を表の名前に替えて実行できる。WindowsのD3D12ではfeatureと末尾の引数をともに `d3d12` に替える。
ヘッドレスの各例は描画結果を読み戻して確認する。エンジン側のイベントループへ組み込む際の起動・更新・解放は[ビューとアセットの手順](tooling.md#scene-materials)、
実際のフレーム処理は[共通のサンプルコード](../../crates/sashiko-renderer/examples/support/views.rs)を参照。

## 最初に動かす

**Graphの合成をCPUだけで確認する**

`graph_composition`は公開Graph APIで利用側のRasterパスをつなぎます。
`rust_pipeline`は`sashiko-renderer`の標準レシピと独自パスを合成します。どちらもCPUでGraphをcompileします。

```sh
cargo run --locked -p sashiko-renderer-core --no-default-features --example graph_composition
cargo run --locked -p sashiko-renderer --no-default-features --example rust_pipeline
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
cargo run --locked -p sashiko-renderer --no-default-features --features vulkan --example pbr_forward -- --api vulkan
cargo run --locked -p sashiko-renderer --no-default-features --features vulkan --example pbr_forward -- --api vulkan --technique deferred
cargo run --locked -p sashiko-renderer --no-default-features --features d3d12 --example pbr_forward -- --api d3d12 --technique forward-plus --msaa 4 --bloom --reuse-shadows --output pbr-forward-plus.ppm
```

D3D12はWindowsで実行できます。準備とパスの接続は[PBRレシピの使い方](tooling.md#pbr-forward-recipe)を参照してください。

**Vulkanで画像に描画する**

`headless`はウィンドウを作らず、indexed mesh、深度、Compute、readbackを通してPPMを保存します。
次の構成はValidation Layerを使い、Graphの診断を表示します。

```powershell
New-Item -ItemType Directory -Force .tools | Out-Null
cargo run --locked -p sashiko-renderer-core --no-default-features --features vulkan,development-validation,graph-diagnostics --example headless -- .tools/headless.ppm
```

**Windowsで描画する**

`windowed`は標準Graphと独自Graphを切り替えられます。WASDとQ/Eで移動し、右ボタンを押して視点を操作します。
Windowsでは `Runtime::for_window` に `HasWindowHandle` を実装するウィンドウ所有者を渡し、`WindowPresenter` を受け取ります。
この例はwinitを使います。エンジン独自のウィンドウとイベントループからも呼べます。
同じデバイスに追加のウィンドウを接続するときは `Runtime::create_presenter` を使います。
画面操作と検証手順は[Windows表示](development.md#windows-presentation)を参照してください。

```powershell
cargo run --locked -p sashiko-renderer-core --no-default-features --features vulkan,presentation --example windowed -- --api vulkan
cargo run --locked -p sashiko-renderer-core --no-default-features --features d3d12,presentation --example windowed -- --api d3d12
```

### Bistro と Sponza を Web ブラウザで開く

Python 3.11 以降を使い、WASM と静的ページをビルドします。ビルド時の `wasm-bindgen` CLI は `Cargo.lock` と同じバージョンを使います。CLI のインストールコマンドはビルドスクリプトにも表示されます。

```powershell
rustup target add wasm32-unknown-unknown
cargo install --locked wasm-bindgen-cli --version 0.2.130
python tools/renderer-scenes/build_web.py --out build/renderer-scenes-web
```

各シーンの UV 付き cook、IBL、対応する昼の GI と反射プローブを用意します。以下は各入力ディレクトリを `BISTRO` と `SPONZA` とした例です。シーン切り替えに必要な両方のパッケージを同じサイトに置きます。

```powershell
python tools/renderer-scenes/package_web_scene.py --scene bistro --manifest BISTRO/cooked/manifest.json --ibl-manifest BISTRO/ibl/manifest.json --baked-lighting BISTRO/day.gi --reflection-probes BISTRO/day.reflections --site build/renderer-scenes-web
python tools/renderer-scenes/package_web_scene.py --scene sponza --manifest SPONZA/cooked/manifest.json --ibl-manifest SPONZA/ibl/manifest.json --baked-lighting SPONZA/day.gi --reflection-probes SPONZA/day.reflections --site build/renderer-scenes-web
python -m http.server 8000 --bind 127.0.0.1 --directory build/renderer-scenes-web
```

Windows の Chrome または Edge で `http://localhost:8000/` を開きます。シーンと代表視点を上部で選べます。Canvas をドラッグして向きを変え、WASD で移動、Q / E で下降・上昇します。「Reset camera」で選択中の視点に戻ります。表示中の視点を共有する例は `?scene=sponza&view=gallery` です。小規模な組み込みシーンは素材の用意前でも `?scene=fixture` で開けます。別の端末に配信する場合は HTTPS を使います。

既存ページには、生成した `viewer.js` と `pkg/` とシーンを配置し、表示サイズを CSS で指定した Canvas を渡します。ホストが描画周期を管理する場合は、前の呼び出しが完了した後で `await view.frame(timeInMilliseconds)` を呼びます。

```js
import { attachScene } from "./viewer.js";

const canvas = document.querySelector("canvas");
const statusElement = document.querySelector('[role="status"]');
const view = await attachScene(canvas, "./scenes/sponza/scene.json", {
    onProgress: message => { statusElement.textContent = message; },
    onError: error => { statusElement.textContent = String(error); },
});
view.start();
document.querySelector("#close").addEventListener("click", () => view.dispose(), { once: true });
```

`stop()` で自動描画を止め、`start()` で再開します。ホストが Canvas の描画解像度を直接決める場合は初期化時に `autoResize: false` を指定します。GPU を失った場合はエラー通知後に `dispose()` して同じ Canvas に接続し直します。[ブラウザホスト](../../examples/renderer-scenes/web/viewer.js)

**Bistroの小さなシーンを描画する**

`renderer-scenes`の`renderer-scenes-graph`は公開Graph APIを使い、Rustでパスを定義して描画パイプラインを組み立てます。
同じパッケージの`renderer-scenes`は公開のPBR Forward、Forward+、Deferredと比較用の描画構成を組み立てます。どちらも`--fixture`で小さなシーンを描画し、
レポートとキャプチャを出力します。実行ごとに新しい出力先を指定します。

```powershell
cargo run --locked --release -p renderer-scenes --no-default-features --features vulkan --bin renderer-scenes-graph -- --api vulkan --fixture --output .tools/bistro-rust-fixture
cargo run --locked --release -p renderer-scenes --no-default-features --features vulkan --bin renderer-scenes -- --api vulkan --fixture --output .tools/bistro-pipeline-fixture
```

Bistroで全シーンを描画するには[Bistroのcook・描画手順](development.md#bistro-pbr)を参照してください。

<a id="bistro-daylight"></a>

### Bistro 屋外の昼景

Bistro はフレームごとにカメラ行列、クリップ距離、太陽の方向・照度をまとめます。標準 PBR はその値からビューを組み立て、手動 Graph は GPU へ転送する境界で uniform に変換します。

両方の実行例で `--look lumberyard-day` を指定できます。日向の石畳と日陰の店舗を一緒に比較できる視点が `bistro-corner` です。太陽、環境光の強さ、背景色はサンプルがロード時に設定し、各視点で共有します。露出は EV +1 です。省略時は検証用の `reference` を使います。

[現行 Bistro と ORCA 公式画像の見開き・拡大比較](../verification/current.md#bistro-daylight)を再生成するには、上記のベイク手順で昼の HDR 環境から作成した 3 時刻の拡散アセットと局所反射アセットを使います。公式画像を `.tools/Bistro_Exterior_1.png` に保存して次を実行します。[ORCA 屋外画像 1](https://d29g4g2dyqv443.cloudfront.net/sites/default/files/akamai/Bistro_Exterior_1.png)

```powershell
cargo run --release -p renderer-scenes --features bake-tool --bin renderer-scenes-bake -- --manifest .tools/bistro-cook/full-a3/manifest.json --ibl-manifest .tools/b3/ibl-final/manifest.json --day-only --reflection-probes tools/bistro/reflection-probes.json --output .tools/bistro-hdr-day.reflections
cargo run --release -p renderer-scenes --features vulkan --bin renderer-scenes -- --api vulkan --pipeline forward --manifest .tools/bistro-cook/full-a3/manifest.json --ibl-manifest .tools/b3/ibl-final/manifest.json --baked-lighting .tools/bistro.gi --reflection-probes .tools/bistro-hdr-day.reflections --look lumberyard-day --view bistro-corner --scenario-blend 0 --width 1920 --height 1061 --output .tools/current-orca-day
python tools/bistro/compare_daylight.py .tools/Bistro_Exterior_1.png .tools/current-orca-day .tools/orca-day.png --details .tools/orca-details.png
```

<a id="bistro-lighting-debug"></a>

照明を分離するには、標準 Forward の `renderer-scenes` に `--lighting-pass base-color|direct|diffuse|diffuse-no-ao|specular|local-specular|emissive` のいずれかを指定します。省略時は通常の合成です。ロード時に入力を切り替え、通常と同じ標準シェーダーで材質、アルファ、影を評価します。間接照度を白い拡散面で見るには既存の `--gi-debug irradiance` を使います。各成分の比較にはトーンマッピング前の `hdr.bin` を使います。

次のコマンドは同じカメラ・時刻で 10 回撮影し、照明の内訳、同じ HDR を使った露出とトーンカーブの比較、グローバル鏡面の寄与を変えた部分比較を保存します。標準出力に線形 RGB・輝度と表示 RGB・輝度の領域統計、成分再合成誤差を出します。既存キャプチャから再集計するときは最初の 3 引数だけを渡します。

```powershell
cargo build --release -p renderer-scenes --features vulkan --bin renderer-scenes
python tools/bistro/diagnose_daylight.py .tools/Bistro_Exterior_1.png .tools/bistro-passes .tools/bistro-debug --bistro target/release/renderer-scenes.exe --manifest .tools/bistro-cook/full-a3/manifest.json --ibl-manifest .tools/b3/ibl-final/manifest.json --baked-lighting .tools/bistro.gi --reflection-probes .tools/bistro-hdr-day.reflections
```

<a id="bistro-awning-reference"></a>

中央のひさしの鏡面反射を実形状に照らして検証するには、上で撮った `beauty`、`base-color`、`specular` のキャプチャを使います。次のツールは同じ画素の受光位置と材質を復元し、CPU ベイカーで昼の HDR・太陽を使って鏡面を積分します。出力は 3 測定域の表示色見本と、空・形状別の線形 RGB、現行 GPU、形状なし対照、半分のサンプル数との比較を収めた JSON です。[実測結果](../verification/current.md#bistro-awning-rays)

```powershell
cargo build --locked --release -p renderer-scenes --features bake-tool --bin renderer-scenes-bake
python tools/bistro/awning_reference.py .tools/bistro-cook/full-a3/manifest.json .tools/b3/ibl-final/manifest.json .tools/bistro-passes target/release/renderer-scenes-bake.exe .tools/Bistro_Exterior_1.png .tools/awning-reference
```

<a id="bistro-upper-probe"></a>

ひさし上面の局所反射を試すには、元の 2 個に上面用を加えた配置から昼のアトラスを焼きます。次の例は上で作った `.tools/bistro-passes` を比較元に使い、通常色・鏡面・重みを 1920 × 1061 で取得します。比較ツールは GPU の深度・アセット識別子を照合し、3 地点の鏡面輝度、周辺の表示色差、比較図を出力します。[比較図と実測値](../verification/current.md#bistro-awning-probe-trial)

```powershell
$trial = '.tools/bistro-upper-awning'
New-Item -ItemType Directory -Force -Path $trial | Out-Null
target/release/renderer-scenes-bake.exe --manifest .tools/bistro-cook/full-a3/manifest.json --ibl-manifest .tools/b3/ibl-final/manifest.json --day-only --reflection-probes tools/bistro/reflection-probes-awning.json --output "$trial/day.reflections"
$common = @('--api','vulkan','--pipeline','forward','--manifest','.tools/bistro-cook/full-a3/manifest.json','--ibl-manifest','.tools/b3/ibl-final/manifest.json','--baked-lighting','.tools/bistro.gi','--reflection-probes',"$trial/day.reflections",'--view','bistro-corner','--look','lumberyard-day','--width','1920','--height','1061','--warmup','1','--frames','1','--inflight','1','--aa-off')
target/release/renderer-scenes.exe @common --lighting-pass beauty --output "$trial/beauty"
target/release/renderer-scenes.exe @common --lighting-pass specular --output "$trial/specular"
target/release/renderer-scenes.exe @common --reflection-debug weights --output "$trial/weights"
python tools/bistro/compare_awning_probe.py .tools/Bistro_Exterior_1.png .tools/bistro-passes $trial docs/verification/bistro-awning-rays.json "$trial/comparison.jpg"
```

既定の描画設定と昼景プリセットを比較する場合は、次の 960 × 530 の手順を使います。

```powershell
target/release/renderer-scenes.exe --api vulkan --manifest .tools/bistro-cook/full-a3/manifest.json --ibl-manifest .tools/b3/ibl-final/manifest.json --look lumberyard-day --view bistro-corner --width 960 --height 530 --output .tools/daylight
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
cargo build --locked --release -p sashiko-renderer-core --no-default-features --features vulkan,presentation --example windowed --target-dir target/dist-vulkan
cargo build --locked --release -p sashiko-renderer-core --no-default-features --features d3d12,presentation --example windowed --target-dir target/dist-d3d12
cargo build --locked --release -p sashiko-renderer-core --no-default-features --features vulkan,d3d12,presentation --example windowed --target-dir target/dist-dual

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
| `renderer-scenes-graph` | 公開Graph APIを使ったRustのパス定義と合成 | `vulkan` またはWindowsの`d3d12` |
| `renderer-scenes` | [シーン描画とシャドウ再利用](tooling.md#bistro-scene-pipeline)、Bloom、トーンマッピング、SDR出力、[材質Programの切り替え](tooling.md#material-shader-build) | `vulkan`またはWindowsの`d3d12`。表示はWindowsと`presentation`。全シーンはcook済み素材とIBLが必要 |
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
Bistroの実GPUテストは`cargo test -p renderer-scenes --lib`に対象featureと`-- --ignored`を指定して実行します。

`rust_pipeline`と`pbr_forward`の入口は`crates/sashiko-renderer/examples/`、Bistroの2つの入口は`examples/renderer-scenes/src/bin/`にあります。
ほかの入口は`crates/sashiko-renderer-core/examples/`にあり、試験や計測と共用するfixtureのRustコードは`crates/sashiko-renderer-core/fixtures/`にあります。
