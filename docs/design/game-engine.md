# ゲーム制作 API と編集データの責務

Sashiko は、少人数のチームと AI が同じシーンを編集し、同じ条件で再現して配布するための 3D ゲームエンジンである。敵の既存行動を使う個体の追加、異なる行動の実装、ステージの配置変更を制作上の受け入れ例とする。ゲーム実行と編集の共通機能を Rust の `sashiko` クレートから提供する。独自の描画は引き続き `sashiko-renderer` と `sashiko-renderer-core` に直接記録できる。

## 衝突問い合わせと剛体

ゲームは `sashiko` の `physics` feature を有効にし、位置・変位・力積・命中点・法線に `sashiko::math::Vec3`、剛体の回転に `sashiko::math::Rotation3` を使う。これらは `sashiko-math` が定義する型で、ゲームが持っている値をそのまま物理 API に渡せる。`BodyId` と `ColliderId` の内部表現、および Rapier 型への変換は `sashiko-physics` に閉じる。

`Pose3` は平行移動と回転を持つ。点には回転と平行移動、方向には回転を適用する。`add_box` はワールド座標の `Pose3` と剛体のローカル軸に沿った半サイズを取り、`pose` は現在のワールド姿勢を返す。箱の掃引・候補判定・重なり問い合わせは、ワールド軸に沿った `Aabb3` を受け取る。`Aabb3::from_center_half_extents` は中心と各軸の半サイズを受け取り、各成分は有限、半サイズはゼロ以上とする。数学層の `contains` と `overlaps` は境界を含む。

ゲームから Rapier、Parry、glam、nalgebra への直接依存を禁止する。CI は workspace の `examples/` 配下のクレートと、`sashiko` に直接依存するクレートの Cargo マニフェストを検査する。別名・任意 feature・ターゲット別・開発用・ビルド用の依存も対象とする。外部のゲーム workspace でも、そのルートから [依存チェッカー](../../tools/check_game_dependencies.py) を実行できる。

`sashiko` の `physics` feature は `sashiko-physics` の固定時間更新ワールドを公開する。初版のバックエンドは Rapier 3D。箱形状の静的・動的剛体を登録でき、位置・回転の取得、インパルス、線分・箱の掃引・重なり判定に対応する。配置を更新した後、時間を進めずに問い合わせるには `synchronize` を呼ぶ。掃引・レイの命中距離は指定した移動量に対する割合で、点と法線はワールド座標で返す。

剛体・コライダーの ID には生成元のワールド識別子を含める。同じプロセスでワールドを作り直すと、その最初の剛体にも以前とは異なる ID が付く。`pose`、`set_translation`、`impulse` は別ワールドの剛体 ID に `WrongWorld` を返す。リプレイにはシーンと入力を記録する。

Survivor は 60 Hz のプレイ更新でプレイヤーの移動と押す力を適用し、剛体を1回進め、その配置で敵と弾を更新する。群衆の経路グリッドは静的配置から作り、実移動で運搬箱も検査する。壁と箱は弾を遮り、敵の被弾・報酬はゲームが決める。再挑戦では配置から物理ワールドを作り直す。録画のゲーム規則版は3。

### 外部ゲームで物理を有効にする

Rapier 0.36.1 が使う Parry 0.31.1 は rstar 0.13 を要求する。Sashiko は同梱の rstar 0.13.1 アダプターを通して rstar 0.14 と heapless 0.9 を使用する。Cargo はアプリの workspace ルートに置かれた patch を解決に使うため、外部ゲームにも下記の設定を入れる。[Cargo の patch 規則](https://doc.rust-lang.org/cargo/reference/overriding-dependencies.html#the-patch-section)

ゲームと Sashiko のチェックアウトが隣接する場合、ゲームパッケージの `Cargo.toml` に依存を追加する。

```toml
[dependencies]
sashiko = { path = "../sashiko/crates/sashiko", default-features = false, features = ["physics"] }
```

ゲームの **workspace ルート**の `Cargo.toml` に次の表を追加する。各パスはそのマニフェストから同じ Sashiko チェックアウトを指すように調整する。

```toml
[patch.crates-io]
rstar = { path = "../sashiko/crates/sashiko-physics/compat/rstar" }
```

ゲーム側で `cargo build` を実行してロックファイルを更新し、`cargo tree --locked -p sashiko-physics` で Parry からローカルの rstar 0.13.1、上流の rstar 0.14、heapless 0.9 への経路を確認する。ゲームの `Cargo.lock` を保存して以後は `cargo build --locked` で同じ依存を再現する。

## 設計判断

| 制作上の判断 | 背景と採用する契約 |
|---|---|
| 編集用の構造と実行時の集計 | Godot のシーンは一組の部品を配置して再利用できる。外側のツリーへの固定パスは別シーンへの移植を難しくする。配置は安定した ID で参照し、実行時の衝突・経路探索・描画はそれぞれ準備済みの配列を使えるようにする。[Godot のシーン構成](https://docs.godotengine.org/en/stable/tutorials/best_practices/scene_organization.html) |
| 個別設定の説明 | Unity の Prefab は原型を共有しつつ配置ごとに値を変えられる。上書き済みの値は原型を編集しても引き継がれない。Sashiko の編集 API は、個体の値を調べるときに原型と個別設定を辿れるものとする。[Unity の Prefab Overrides](https://docs.unity3d.com/6000.0/Documentation/Manual/PrefabInstanceOverrides.html) |
| 独自機能の編集 | Unreal Engine のネイティブ機能と Blueprint の関係を参考に、ゲームの Rust 型も制作者が編集できる形に登録する。効果の寿命と取消は共通の機能にし、ダメージ・勝敗・報酬の定義はゲームが所有する。[Unreal の Gameplay Abilities](https://dev.epicgames.com/documentation/en-us/unreal-engine/using-gameplay-abilities-in-unreal-engine) |
| 更新順と再現性 | Bevy のシステム登録は Rust からの合成に適するが、相対順に意味がある処理は順序を指定する必要がある。入力を固定ステップで確定し、ゲームが結果を決めた後で表示や音に渡す。表示の再試行で同じ操作を二度適用しない。[Bevy の System ordering](https://docs.rs/bevy/latest/bevy/ecs/system/index.html#system-ordering) |

## 共通 CPU ジョブ

ホストが所有する `JobSystem` に、参加スレッドの総数を指定する。1 ならホストだけで実行する。`scope` から戻ると、投入したすべての仕事とその子孫が完了し、結果の領域を再び借用できる。

```rust,ignore
system.scope(|s| {
    s.spawn(|| visibility.update(&scene, &mut visible));
    s.spawn(|| audio.prepare(&listeners, &mut mix));
    s.for_each_mut(&mut particles, |p| p.advance(dt));
});
```

投入順・実行スレッドは任意で、その場で実行する場合もある。兄弟をチャネルで待つ処理やブロッキング I/O は別に扱う。途中で結果を使うサブシステムは `s.scope` で子孫の完了を待てる。巻き戻し可能な構成では残る仕事を完了してから panic を再送出する。

ホストと各ワーカーに準備済みの有界両端キューを持たせる。所有者は直近の仕事、空いた参加者は他のキューの古い仕事を取得する。各キューに個別の mutex を用い、空のキューは atomic の状態を見てスキップする。スコープごとの未完了数は実行関数の終了後に減らす。仕事がなければ待機登録後にキューと完了条件を再確認して眠る。通常フレームのスケジューラは汎用ヒープを使わない。現在は各キューに 16 件分を準備し、128 バイト・64 バイトアラインメント以内のクロージャを所有できる。満杯または格納できないキャプチャは呼び出し元で実行する。これらの値は内部の選択である。

`for_each_mut` は各要素を排他的に借用する。`for_each_chunk_mut` は最終端を除き指定サイズの論理チャンクと開始位置を渡す。ホストを含む参加者が 3 人以上で、参加者あたり平均 4 チャンク以上ある場合は、バッチを参加者数の最大 2 倍の連続範囲に分ける。投入元も計算に参加し、各参加者は担当範囲が終わるたびに次の範囲を取得する。補助ジョブの投入は最大でワーカー数と同じ回数で済む。範囲の取得にはそのバッチ専用の mutex とスコープ上のイテレータを使い、コールバックを実行する前にロックを解放する。

それより小さいチーム・バッチでは、参加者数以下の連続範囲にまとめ、分割したスレッドが 1 範囲を直接処理する。盗まれた範囲は元のキューが空で待機中の参加者がいれば追加分割する。どちらの方式も同じワーカーのローカルキューを利用する。外部の Rayon プールとは独立して動作する。[負荷別の実測](../verification/current.md#job-range-profile)

Web は専用ホスト Worker と同じ共有メモリで動作する子 Worker を使用する。初期化失敗時は部分起動したチームを回収する。通常の破棄では子がループから戻り、Wasm のスタックと TLS を解放して終了する。実行中の Wasm trap はホストとそのメモリ単位で回復する。サンプルはホストに一意の名前を付け、ページが `BroadcastChannel("sashiko-jobs:" + hostName)` で子の trap を受け取る。応答の途絶はページ側の期限で検出する。

借用ジョブは投入先の `scope` 内で完了する。将来、所有ジョブや依存グラフを追加する際は、依存が解決した仕事を同じワーカーに渡せる。[検証手順](../guides/development.md#cpu-jobs)

## ゲーム作者との境界

キャラクターの固定 tick、ECS との受け渡し、表示用ポーズの並列評価と GPU 変形の分担は[キャラクターアニメーションの設計方針](animation.md)に従う。

ゲームは行動の条件と結果を定義する。「狙う相手が近づくと、予告の後に突進する」はゲームの規則で、予告時間・使用クリップ・速度はその規則が読むデータである。範囲を調べる、位置を更新する、経過時間からクリップを評価する、個体の消滅に合わせて再生を終了する操作はエンジンから提供する。

| 系統 | 正本と寿命 |
|---|---|
| 編集・配置 | シーンと原型が制作上の配置・参照を所有する。配置 ID で変更対象を特定し、保存前に参照と編集値の整合性を確かめる。GUI と CLI が同じファイルを書き戻せることを検証する。 |
| 固定更新 | ゲームの状態は CPU 上で進む。更新ごとの入力、初期配置、開始状態、seed を記録すると、同じ実行ファイルとコンテンツで同じ更新結果を再生できる。 |
| 描画 | 更新結果から見える対象の姿勢・材質を抽出する。静的な部分は再利用し、通常の描画数をあらかじめ予約する。GPU ハンドルの有効期間はデバイスが管理する。 |
| 素材・配布 | 素材 ID は配布物でも解決できる。配置 ID・生存中の個体・GPU ハンドルとは寿命が異なる。変換元と実行時データの契約は[アセットパイプライン](asset-pipeline.md)に従う。 |

## アプリへの組み込み

`sashiko` の固定更新、入力、シーン、保存を使うヘッドレスホストは、Cargo の依存宣言で `default-features = false` を指定できる。たとえば次の構成は、描画、音声、アニメーションのクレートをビルドせずに実行中の状態を検査できる。

```toml
[dependencies]
sashiko = { path = "../sashiko/crates/sashiko", default-features = false, features = ["live-inspector"] }
```

| 機能 | 利用できる API |
|---|---|
| `asset` | アセット ID、カタログ、配布オブジェクトの読み込み。 |
| `animation` | `asset` に加え、クリップのタイムライン、ポーズ、キャラクター制御。 |
| `rendering` | Renderer と接続するビュー、UI、キャプチャ。`animation` と併用するとスキン付きモデルのロードと描画用パレットの準備も使える。 |
| `audio` | WAV の読み込みとミキシング。対応するデスクトップ環境で実際に再生するには `audio-output` を選ぶ。 |

既定では `animation`、`audio`、`rendering` が有効になる。物理と Inspector はそれぞれ `physics`、`live-inspector` で追加する。ウィンドウの提示には `presentation`、GPU バックエンドには `vulkan`、`d3d12`、`webgpu` を選ぶ。

ホストは描画先、イベントループ、ゲーム操作を受け付けるビューを所有する。Sashiko の標準ホストを使うとゲームを単体で起動できる。組み込み時はホストが経過時間で進める更新、単一 tick の更新、現在の状態の描画を個別に呼ぶ。停止中の編集や提示の再試行でも tick は進まない。

ゲームはデジタル操作と連続値の割り当てを宣言する。Sashiko の `DeviceInput` はボタンの変化を次の固定更新まで保持し、スティックの半径方向のデッドゾーンを適用する。ホストから同じ入力を注入できる。Windows は XInput、ブラウザは標準マッピングの Gamepad API を用い、サンプルはスロット 0 を使う。フォーカスを外すと保留中の操作と軸値を消去する。リプレイにはゲームに渡した操作を保存し、再生中はデバイスを参照しない。[Survivor の割り当て](../../examples/survivor/src/controls.rs)

WinForms の `SashikoView` は UI スレッドで子 HWND に接続し、破棄前に描画セッションを閉じる。Survivor は単体版と同じ3Dシーン、HUD、Renderer の提示・退役処理を使う。Web の `attach` は Canvas に入力リスナーを登録し、ホストからのフレーム呼び出しと標準のアニメーションループに対応する。サンプルの Web 表示は WebAssembly 内のゲーム状態から生成する Canvas 2D の俯瞰図である。[サンプルの操作](../guides/examples.md#embedded-game)

## エンジンを実証するサンプルアプリ

`survivor` の基準入力で、初期配置・移動・接触・強化・再挑戦をヘッドレスと実画面で比較する。実験台を動かしたケースでは、移動先・障害物の見た目・群衆の迂回に同じ配置が反映されることを確認する。500 体のシナリオは、可視数、提示間隔、GPU 時間と一緒に評価する。実測条件と未確認の利用例は[検証記録](../verification/current.md)に記す。

`training-yard` はゲームの目的や入力が別でも共通 API が利用できるかを確認する。`renderer-scenes` は独自の RenderGraph を直接構築する利用者として維持する。

## 研究施設で敵を追加して再生する

[研究施設のシーン](../../assets/survivor/arena.json)を作業用にコピーする。原型は `prefabs`、原型を使う配置は `placements` にある。画面上での編集と CLI の操作は[配置・プロパティ・クリップの編集手順](../guides/examples.md#配置プロパティクリップを編集する)から確認できる。新しい種類を一つ試すには、`placements` 配列に次の要素を追加する。

```json
{
  "id": "scout",
  "prefab": "walker",
  "overrides": {
    "copies": 1,
    "hp": 2,
    "walk_speed": 2.5,
    "model": "alternate",
    "clip": "walk"
  }
}
```

`Subject` の配置は出現する敵の種類を表す。プレイ中は登録した種類の順に、各種類の `copies` 体ずつを繰り返して生成する。上の設定では歩行速度・耐久力・外観が異なる敵が加わる。新しい座標に置く実験台には `Bench` の原型を使い、`translation` をメートル単位の XYZ で与える。親を指定するとその座標は親から見た値になる。研究施設の実験台は床の X・Z 軸に沿った長方形を使う。

研究施設の4個の点光源は `PointLight`、見下ろしカメラは `GroundCamera` の配置である。たとえばインスペクタで `lamp-nw` の `candela` を 0 にすると、その灯具の寄与が消える。実験台は等倍の `room` の子としてメートル単位で配置する。点光源には独立した親 `lighting-frame` があり、部屋の半幅・奥行きに合わせてその X・Z のスケールを変えると、4灯の間隔をまとめて調整できる。`camera` の `height` は画面の縦に収める高さ（メートル）、`pitch_degrees` は俯角、配置位置は追従の基点である。ゲームは同じ配置から準備した照明とカメラを実画面とキャプチャで使う。

座標系を共有する配置には、標準の `Group` を親として使える。`lighting-frame` はこの型の一例で、どのゲームの `Registry::default()` にも登録される。

別の動きを試すには、[キャラクターカタログ](../../assets/survivor/characters/catalog.json)の対象モデルの `clips` に名前とサンプル範囲・再生速度を登録し、`Subject.clip` からその名前を参照する。たとえば `alternate` の既存歩行をゆっくり再生するクリップは `"slow_walk": {"first": 0, "frames": 80, "hz": 30, "looping": true}` と書ける。カタログのコピーを別ディレクトリに置く場合、`skin`・`color` の相対参照先も用意する。`--character-catalog` でそのカタログを指定して表示する。

ゲーム固有の挙動を加えるときは、ゲームの登録処理に型・編集項目・検査関数・固定更新関数をまとめて渡す。実装例の `Marker` は公開する半径と、再生中だけ保持する進入状態を持つ。プレイヤーが半径内に入った更新でイベントを一度発生させる。[登録と動作](../../examples/survivor/src/authoring.rs)

```rust
types.register_behavior("Marker", &["radius"], Marker::validate, Marker::update)?;
```

`radius` を原型の `fields` または配置の `overrides` で編集するとゲームの判定が変わる。同じ登録表をインスペクタとゲーム実行が利用する。新しい型には別の型名を登録し、その型を使う原型と配置をシーンに加える。効果の意味と更新処理はゲームの Rust に置く。

次の例は、作業用シーンで遊んだ入力を記録し、指定 tick まで GPU なしで再生する。最初のコマンドでは 120 tick 以上プレイして終了する。

```powershell
cargo run --locked --release -p survivor --bin survivor -- --arena my-lab.json --seed 506 --record my-run.json
cargo run --locked --release -p survivor --bin survivor --no-default-features -- --headless --replay my-run.json --ticks 120
```

記録には開始シーンの原型・上書き・階層、ゲームルールの版、seed、更新ごとの入力が入る。ヘッドレス再生は記録したシーンからゲームを構築し、終了時に HP、撃破数、目印への到達回数などを JSON で出力する。

### 提示フレームの計測

Windows のゲーム側は、表示したフレーム数をエンジンの WindowTimings に渡す。描画と提示が返した GPU 時間を退役時に回収し、最後に両方の完了を待って提示数と照合する。描画準備や終了時の明示的な観測には renderer_memory と working_set を使う。GPU の資源量、論理容量、プロセスの現在・ピークのワーキングセットを区別して保存する。敵や弾などの負荷の意味とリプレイ区間はゲーム側が定義する。[計測API](../../crates/sashiko/src/profiling.rs)
