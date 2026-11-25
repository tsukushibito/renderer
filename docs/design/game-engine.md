# ゲーム制作 API と編集データの責務

Sashiko は、少人数のチームと AI が同じシーンを編集し、同じ条件で再現して配布するための 3D ゲームエンジンである。敵の既存行動を使う個体の追加、異なる行動の実装、ステージの配置変更を制作上の受け入れ例とする。ゲーム実行と編集の共通機能を Rust の `sashiko` クレートから提供する。独自の描画は引き続き `sashiko-renderer` と `sashiko-renderer-core` に直接記録できる。

## 設計判断

| 制作上の判断 | 背景と採用する契約 |
|---|---|
| 編集用の構造と実行時の集計 | Godot のシーンは一組の部品を配置して再利用できる。外側のツリーへの固定パスは別シーンへの移植を難しくする。配置は安定した ID で参照し、実行時の衝突・経路探索・描画はそれぞれ準備済みの配列を使えるようにする。[Godot のシーン構成](https://docs.godotengine.org/en/stable/tutorials/best_practices/scene_organization.html) |
| 個別設定の説明 | Unity の Prefab は原型を共有しつつ配置ごとに値を変えられる。上書き済みの値は原型を編集しても引き継がれない。Sashiko の編集 API は、個体の値を調べるときに原型と個別設定を辿れるものとする。[Unity の Prefab Overrides](https://docs.unity3d.com/6000.0/Documentation/Manual/PrefabInstanceOverrides.html) |
| 独自機能の編集 | Unreal Engine のネイティブ機能と Blueprint の関係を参考に、ゲームの Rust 型も制作者が編集できる形に登録する。効果の寿命と取消は共通の機能にし、ダメージ・勝敗・報酬の定義はゲームが所有する。[Unreal の Gameplay Abilities](https://dev.epicgames.com/documentation/en-us/unreal-engine/using-gameplay-abilities-in-unreal-engine) |
| 更新順と再現性 | Bevy のシステム登録は Rust からの合成に適するが、相対順に意味がある処理は順序を指定する必要がある。入力を固定ステップで確定し、ゲームが結果を決めた後で表示や音に渡す。表示の再試行で同じ操作を二度適用しない。[Bevy の System ordering](https://docs.rs/bevy/latest/bevy/ecs/system/index.html#system-ordering) |

## 共通 CPU ジョブ

`sashiko-jobs` は独立したライブラリである。ホストが一つの `JobPool` を作り、各サブシステムへ参照を渡す。独立した二つの処理を `join` に渡すと、結果を引数順に受け取れる。`for_each_chunk_mut` は元の開始位置と重ならない可変スライスを渡す。借用の期限は各呼び出しから戻るまでで、結果の格納位置を利用者が決める。コールバックの完了順は任意である。空入力は何もせず、一つのチャンクはその場で処理する。

```rust,ignore
let pool = JobPool::new(4.try_into()?)?;
pool.join(
    || pool.for_each_chunk_mut(&mut poses, batch_size, evaluate_poses),
    || prepare_background(&scene),
);
```

ワーカーは同じプールの子ジョブを実行しながら合流できる。兄弟同士をチャネルで待たせる処理やブロッキング I/O はホストで区別して扱う。小規模な仕事には `JobPool::inline()` を選べる。ネイティブはホストごとに Rayon プールを持つ。Web の並列構成は専用ホスト Worker の Wasm メモリにつき一度起動し、そのインスタンスのグローバル Rayon プールをジョブ専用に使う。他の Rayon 利用処理もこのホストではジョブ内で実行する。

通常のフレームは一つのホストから入り、複数の仕事をその中で分岐させる。準備時に確保する投入スロットがホストのスタック上の処理をワーカーに一時的に貸す。書き込み完了を mutex で確認してから借用を返し、ネイティブの巻き戻しも同期後に伝える。複数の外部スレッドが同時に投入するときと別プールのワーカーから入るときは、進行を保つため Rayon の投入キューを使う。この経路はヒープを使いうる。

将来、依存グラフは実行可能になったノードを同じ実行器に渡し、ノード内では既存の範囲並列を使える。フレームをまたぐ処理は所有データと完了ハンドルの契約を別途定める。現在の利用例と検証手順は[開発ガイド](../guides/development.md#cpu-jobs)に記載する。

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
