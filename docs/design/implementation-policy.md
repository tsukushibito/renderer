# ゲームエンジン向け3D Renderer 実装方針

**対象:** Codexおよび実装担当者  
**作成日:** 2026-11-12  
**文書バージョン:** 2.2

**実装言語・対象API:** Rust / Vulkan / D3D12（native基盤を改修、上位の固定操作を置換）
**設計の中心:** RenderGraphによる描画処理の宣言・解析・実行

> 到達目標は、Bistroをテクスチャ付きPBR・影・線形HDRとトーンマッピングで描画し、利用側の独自shaderによる多段Bloomを追加・交換できる、ゲームエンジン向けRendererである。
> 同じ利用側のパス定義と入力をVulkanとD3D12で実行する。実シーンと利用コードから公開APIを設計し、採用基盤を選び、機能を縦切りで実証する。

本書は今後の方針であり、実装済みの範囲は各設計文書と検証結果で区別する。既存基盤の再利用は必須にしない。Bistro到達までの進めやすさ、総工数、公開APIのUX、品質と保守負担から、維持・改修・置換・外部基盤採用を比較して選ぶ。既存の検証実績は判断材料であり、構造を維持する理由そのものにはしない。

本文の「必須」は守るべき契約、「初期推奨」は実装対象と検証環境に応じて変更できる出発点、「後続」は初期実装の完了条件には含めない事項を示す。重要な変更は、理由・代替案を現在の設計文書へ、検証結果を現在の検証状況へ反映する。置き換え済みの判断はGit履歴から参照する。

## 1. 最重要の実装判断

| 項目 | 方針 |
|---|---|
| GPU処理の標準的な投入経路 | RenderGraphとする。標準描画機能もカスタムパスも、原則として同じ経路を使う。 |
| 採用基盤 | [B0の比較](#foundation-selection)により、nativeの低レベル所有・同期・退役を再利用し、上位の固定操作をowned命令・Program/bindingへ置換する。自前実装を目的にしない。 |
| 資源割当・同期・寿命 | Rendererとして保証する責任を明確にし、採用基盤へ委譲する範囲と自前で実装する範囲を決める。既存allocator等を利用し、用途のない自作を増やさない。 |
| Graphとパスの境界 | パス作者が処理と資源アクセスを定義し、基盤がフレーム全体の依存・資源・実行を管理する。新しい効果の追加でGraph本体を編集しない。 |
| RHIの扱い | 代表的な利用コードから必要な契約を設計する。型名や共通コマンド体系の形を先に固定せず、二APIの実行で確認する。 |
| 初期の実行経路 | 単一の描画・Compute用キュー、エイリアシングなし、保守的で正しい同期を基準とする。 |
| 二APIの実証 | VulkanとWindows上のD3D12を対象とする。同じ論理Graph・パス定義・入力で実証し、描画機能をbackendごとに二重実装する公開設計にしない。 |
| 公開API | エンジン向けAPIとGraph APIへ、`ash::vk`や`vk_mem`の型を露出させない。 |
| 標準機能の提供 | [汎用Graph・標準パス・標準グラフ](#public-api-layers)を公開する。標準グラフは公開標準パスから構成し、利用側が全体利用・部分交換・独自構成を選べる。 |
| 最適化の判断 | 出力の正しさとCPU時間・GPU時間・メモリ使用量の計測で判断する。薄い抽象化だけで高速化を保証しない。 |

`wgpu`もB0で再評価した。二APIをまとめて委譲する利点と、製品経路の保証・検査の配置を比較した採用理由を下記へ記す。物理aliasingや複数queueの直接制御を、Bistro初期描画の一律要件にしない。

### ローカルな拡散間接光の責務

| 所有者 | 責務 |
| --- | --- |
| アセットビルド、ゲーム | ベイク対象の静的形状と材質を抽出する。静的配置・LOD ごとの描画順を記録する。太陽と空の時刻ごとの条件、時刻の進め方、読み込みと再ベイクのタイミングを決める。 |
| `renderer-bake` | 共通 BVH を構築する。形状に近いセルの自動細分化、共有コーナーの位置補正と無効化、各軸の可視距離、拡散プローブの SH、クック済み UV から静的配置ごとのライトマップを生成する。線形色で最大 2 回の拡散反射を解き、1～16 条件の拡散照明を同じ配置で保存する。GPU 機能はオプション。 |
| `renderer` | バージョン付きベイク資産をインポート時に検証する。標準 PBR が配置に対応する静的チャートをサンプルし、その他の受光点はセルの 8 コーナーを三線形重みと可視距離で補間する。シナリオの照度を線形に混合する。 |
| `renderer-core` | 既存の画像・バッファ、転送、Compute、完了管理を提供する。 |

太陽の直射はランタイムのシャドウ付き PBR が受け持つ。空の放射輝度、発光面、他の面で拡散反射した直射光をベイクする。アトラスは現在、全体を常駐させる。範囲外はシナリオごとの空の照度、内部で全コーナーが遮蔽された場合はゼロを返す。6 軸の可視距離と SH は近似であり、薄い斜めの壁、細い戸口、異なるセル解像度の境界は光漏れ・不連続の評価対象である。

拡散プローブの次数はボリュームごとに選ぶ。既定の L1 は従来形式のまま、定数項とワールド法線の x・y・z 成分に対応する4組の RGB 係数を保存する。L2 では xy、yz、xz、x²−y²、3z²−1 の5組を追加する。どちらもベイク時にランバートの余弦重みを畳み込み、描画時には同じセル・可視距離で8コーナーを補間する。L2 は光の方向による明るさの変化をより細かく表現できる。プローブ位置と受光点の違いによる空間補間誤差は残る。

クックはメッシュごとに正規化ライトマップ UV を持ち、必要なシームで頂点を分割する。材質用の 48 バイトの主ストリーム、インデックス、ライトマップ用の副ストリームを区別する。配置ごとの寸法と表裏を先にページへ割り付ける。Bake はページごとに有効な受光点を積分し、その結果をパディングに複写する。GPU は表面の全ページでデバイスと BVH テクスチャを再利用する。標準 PBR は配置 ID からページと矩形を解決し、材質とメッシュを共有する。移動・スキニングする物体は照度プローブを参照する。固定レイアウトのページ番号を指定して部分ベイク・部分ロードできる。照明や遮蔽物の変更時は、それらの影響が及ぶ全ページとプローブを再評価する。[利用例](../guides/examples.md#bistro-baked-lighting)

N 条件の拡散アトラスでは、各プローブが L1 で 2＋4N、L2 で 2＋9N テクセルを使う。各ライトマップ画素はどちらも N テクセル。L2 の識別子は `RBKLSH02` で、GPU 側でも参照するヘッダーのタグに −N を格納する。インポート時に識別子、タグ、次数に応じたデータ長を照合する。正規化した時刻で隣接する 2 条件を選び、キー上ではその条件の照度だけを読み出す。セル、可視距離、静的チャート表は全条件で共有する。静的ライトマップの索引 RSURF003 は、各 GPU ページの寸法、配置 ID のスロット順、SHA-256 と、独立したプローブ成果物の SHA-256 を固定する。ベイク時に表裏の配置表と線形 RGBA32F の画素を一つの GPU 画像に整形し、ロードは選択したページだけを検査・転送する。索引の世代が変わるときはその索引に属する成果物を使用する。RSURF002 を使用している配布物は再ベイクする。CPU と GPU ベイクは共通の HDR 経緯度マップを参照できる。ベイク範囲外の拡散照度は空の立体角加重平均に π を掛けた近似値を使う。

### 鏡面反射プローブ

配置方式は `ReflectionPlacementMethod` で選ぶ。`Remedy` は Quantum Break の「近くから見える面積を増やす」という基準を使う。本実装ではボクセル内の面を法線の主軸と符号で 6 組にまとめ、面積重心への可視性をベイク BVH で求める。候補ごとの近傍は各軸 ±3 セル。受光面の重みは面積 × ボックス内のフェード / (1 + ボクセル幅で正規化した距離の二乗)。各面の既存の最大重みからの増分の総和で候補を選び、同点ではセル座標順を使う。ボックスは 6 軸方向の最初の遮蔽物から最大でボクセル幅の 1/8 だけ先へ延ばし、面上のフェードを確保する。探索上限は 262,144 セルと 4,194,304 本の候補・面の対応。超過はベイク前にエラーとし、生成した配置は従来の形式で編集できる。[Remedy の講演資料](https://arisilvennoinen.github.io/Publications/SIGGRAPH_2015_Remedy_Notes.pdf)

広域候補の面積利得が最初の半分を下回るか候補が尽き、個数予算に余裕があるとき、遮蔽を考慮して全枠を選び直す。占有セルの中心から6軸にレイを飛ばし、2軸以上で正負両側に面があり、裏面ヒットやボクセル幅の1割未満の近接面がない中心も候補にする。追加候補のフェード幅は箱の余白に合わせる。面側の外向き・接線方向5本と候補の6本で遮蔽を比較し、面積利得に (1 + 面側の遮蔽方向数) / (1 + 遮蔽の不一致数)² を掛ける。この段階では距離を探索半径（3セル）で正規化する。柱の近くに集中するより、そこから見える遠方の壁も覆う配置の利得が上がる。

詳細選択後、2軸以上を両側から囲まれたプローブでは、元の箱から外へ1セル以内の面も調べる。中心から面重心への最初のヒットが表向きで、距離と法線の主軸方向の符号が一致した面を、フェードの余白（最大1セル）込みで箱に収める。これにより、軸レイが柱に当たった場合も、その脇から見える壁や床を覆える。拡張候補の探索は常に元の箱を基準にする。開口部や薄壁をまたぐ場合には軸平行箱の近似誤差が残る。[配置・実描画の検証](../verification/current.md#remedy-enclosure)

配置後に遮蔽の一致度で各プローブのフェード幅を選び直す試作では、Sponza の上階側壁は改善したが、シーン全体から面積比例で抽出した点で誤差の中央値が許容幅を超えた。ベイクでは引き続き配置時に決めた幅を使う。[フェード幅の検証](../verification/current.md#remedy-blend-distance)

任意のオフライン品質チェックでは、各キャプチャから5セル以内の表向きの最初の交点を2組、各24方向まで決定的に抽出する。粗さ0.5、F0=0.04の共通材質で、各点から64本のGGXレイによる鏡面参照を計算する。配置の推定には32×16の局所キャプチャを32×8にGGXフィルタした値、箱投影・重み正規化、同じ受光点の空の積分を使う。候補間で参照とフィルタ結果を再利用する。測定範囲は既存プローブから見える面と指定した材質・視線に限られる。

一方の受光点集合で、各プローブの6面を個別に±0.5セル動かす。通る候補がなければ、誤差の大きい可視面に向けて見通せる線分の半分、最大0.5セルだけキャプチャを移す案を2個まで調べる。変更のあった4点以上で平均絶対RGB誤差が1%を超えて改善し、元のプローブごとの変更点の誤差増加が5%以下の候補から、誤差総和の減少が最大の案を選ぶ。別に確保した受光点集合で同じ基準を適用し、全体の90パーセンタイルも悪化しなければ1件採用する。評価数で計算量を制限する。プローブ数は固定で、追加の計算と低解像度キャプチャはベイクツール内で完結する。[CLI](../guides/examples.md)・[実測](../verification/current.md#reflection-placement-repair)

固定面で箱が重なる場合の次の実装候補は、中心への可視性をベイクして混合に使う方式とする。受光面側の既存チャートの表裏とUVを利用し、各プローブの可視性を最大32ビットのマスクとして格納する案を検証する。描画時は可視な候補へ元の局所重みの総和を再配分する。全候補が隠れた点と静的チャートを持たない物体には現行の箱の混合を使う。遮蔽境界での急変、マスク解像度・容量・読取費用、プローブ配置や遮蔽物変更時の再生成を実機で評価してから形式を決める。現時点で検証済みなのは、各受光点で直接判定した場合の精度。[中心1本と最大7本の比較](../verification/current.md#reflection-receiver-visibility)

既定の `Floor` は床に対するキャプチャ高さを使う。

`BakeScene::place_reflection_probes` は静的形状から編集用の初期配置を生成する。XZ 格子ごとに上から全層をたどり、傾斜 45 度以内の表向きの床で、指定したキャプチャ高さまで空いている位置を採る。中央で配置できなければセルを一度だけ四分割して試す。材質のアルファマスクを考慮し、水平４方向の最寄りの面で箱を切る。支持面にはフェード幅だけ下へ影響を延ばし、遮蔽物のない隣接セルはフェード域を重ねる。上階の床下面、斜めの境界、細い通路や箱の角の遮蔽には手調整が必要になる場合がある。探索する元の格子は最大 65,536 列、結果は既存アトラスの最大 32 個。配置・検査はオフラインで完結し、描画時は手動配置と同じアトラスを使う。

配置はアプリまたはエディターがワールド座標のキャプチャ点、影響 AABB、内側のフェード距離で指定する。`renderer-bake` は静的シーンの BVH と材質を使い、1～2 条件の入射放射輝度を CPU で画素ごとに並列キャプチャする。空への可視性、アルファマスク、拡散面で反射した太陽光と発光を含む。鏡面反射の再帰と移動物体の映り込みはベイクの対象外。線形 HDR の経緯度マップを GGX で事前フィルタする。配布形式はアセットの cook 設定が選ぶ。Bistro は BC6H unsigned を既定とし、RGBA32F を画質比較用に選べる。

単一・二条件とも `RSPROB02` に形式、シナリオ数、プローブ数、各プローブの 48 byte の配置レコード、放射輝度の順に保存する。ロード時に形式、個数、長さ、配置を検証し、GPU へは放射輝度と 1,824 byte の読み取り専用定数バッファを一度転送する。位置・境界・フェードは FP32、候補マスクと個数は uint32 として保持する。BC6H は符号なし RGB 用で、圧縮前に有限値と表現範囲を確認する。ロード時にブロックを展開する必要はない。旧 `RSPRST01`・`RSPROB01` はロード時に配置を分離して同じレイアウトへ詰め直す。

各シナリオは幅 128、高さ 132。粗さ別の有効な高さは 64、32、16、8、4、2、1 で、最後の 2 層は端の行を複製してそれぞれ 4 行を占める。独立した層同士が 4×4 ブロックを共有しない。プローブ・シナリオ当たりの画像データは BC6H で 16.5 KiB、RGBA32F で 264 KiB。全層で幅を共通にして経度方向のフィルタを連続させ、緯度は各層内のテクセル中心にクランプする。初期化時に一度アップロードし、完了まで通常の Graph 資源寿命に従う。シナリオ係数を変えたフレームの追加転送は既存ビュー定数の範囲に収まる。

Forward、Forward+、Deferred は共通の PBR 関数を使う。4 箱までは直接判定する。5～32 箱はロード時に全箱の境界を 4×4×4 セルに分け、各セルと交差する箱のビット集合を定数バッファに格納する。画素のワールド位置でセルを引き、候補ビットを元の配置順に評価する。カメラごとの再構築・転送を省き、同じビュー内でも離れた箱を除ける。巨大な箱や同一セルへの集中では全箱が候補になる。数値範囲外でグリッドを作れない場合も全箱を評価する。各ピクセルから箱の最寄りの面までの内側距離をフェード距離で割り、0～1 に制限した値を重みとする。総和 S が 1 以上なら各重みを S で正規化し、1 未満なら残余 1−S にグローバル IBL を適用する。箱との反射線の交点をキャプチャ点から見た方向で引き、粗さと時刻を線形補間してから材質の split-sum BRDF を適用する。ローカルマップはワールド方向、グローバルマップは指定した環境回転・強度に従う。各箱の奥行きは近似で、箱内部の遮蔽や鏡の映像を再構成するものではない。

## 2. 最終目標と対象範囲

### 2.1 ゲームエンジンへ提供するもの

Rendererは特定のECS、アセット管理システム、ウィンドウイベントループに依存しないライブラリとして設計する。エンジンはシーンの所有と更新を担当し、Rendererへ描画に必要なデータを渡す。

制作原本、永続的なモデル参照、装備やシーンの状態はエンジンが所有する。
`renderer` はメッシュ・材質・LOD をまとめる描画用の `Model` と `ModelInstance` を任意で提供し、利用側が抽出した描画入力も受け付ける。
`renderer-core` は個別資源とその実行・完了を扱う。制作・配布・実行時の境界は[アセットパイプラインの到達設計](asset-pipeline.md)に従う。

| 領域 | 最終的に満たすべき内容 |
|---|---|
| 3D描画 | メッシュ、マテリアル、テクスチャ、カメラ、ライトを扱い、深度を含む実用的な描画を行う。 |
| 基本的な描画品質 | PBR、シャドウ、HDRの内部描画、トーンマッピング、透明描画などを段階的に整備する。方式は実装時に検証して決める。 |
| エンジン統合 | リソースの作成・更新・破棄、複数View、オフスクリーン出力、フレーム間履歴、アップロード・readbackを明確な契約で扱う。 |
| 拡張性 | エンジン側からカスタム描画・Compute・コピー処理をグラフへ追加できる。 |
| 実シーンでの成立 | Bistroをテクスチャ付きPBR・影・線形HDR・トーンマッピングで描画する。独自Bloomの追加・無効化・交換をRenderer外の利用コードで行う。 |
| クロスプラットフォーム | Vulkanに加えて異なるネイティブAPIを実装し、公開契約の移植性を実証する。 |
| 品質保証 | 画像回帰、GPU検証、性能回帰、リソース寿命テスト、再現可能なサンプルを提供する。 |
| 調査可能性 | ユーザーのパスから、実際のリソース・バリア・キュー投入まで追跡できる。 |

初期対象はPCネイティブを想定する。最終的なOS・GPU世代・必須機能・画質目標・性能予算は、検証機材と用途を踏まえて記録する。未計測の「商用エンジンと同等性能」や、未検証のプラットフォーム対応を成果として扱わない。

### 2.2 初期の対象外

ゲームエンジン全体、ECS、物理、アニメーションシステム全体、エディター、アセット制作環境は作らない。サンプル用の最小ローダーやシーンは許可するが、Rendererの必須依存へ拡大しない。

汎用GPUライブラリとしての独立RHI、全バックエンドの同時完成、レイトレーシング、Mesh Shader、GPU-driven描画、全面的なバインドレス化、複雑な仮想ジオメトリなどは初期要件にしない。Web・モバイル・コンソール対応も別途要件化するまで実装対象に含めない。

これらを将来可能にすることと、未使用の抽象化や機能を先回りして実装することは区別する。

## 3. 現行の依存ライブラリと採用基盤の再評価

### 3.1 現行のashとvk-memの役割

以下は現行実装の構成である。B0では低レベルの再利用を選んだが、固定操作と内部構成の維持義務はない。置換時は移行先の契約と検証方法を更新する。

`ash`はVulkanに近いバインディングであり、独自のRenderGraphやGPU寿命管理を提供する層ではない。`vk.xml`から生成された型と関数を使い、Vulkanバックエンドの処理を記述する。[^ash]

`vk-mem`はVMAのRustバインディングであり、メモリタイプ選択、メモリブロックの確保・サブアロケーション、マッピングなどに利用する。グラフに基づく同期・寿命・キュー割り当てはRendererが担当する。[^vkmem]

作成時に確認した組み合わせは、`ash 0.38.0+1.3.281`と`vk-mem 0.5.0`である。`vk-mem 0.5.0`のmanifestは`ash 0.38`系を指定している。[^ashdocs][^vkmemmanifest]

```toml
[dependencies]
ash = "0.38"
vk-mem = "0.5"
```

これは開始用の互換バージョン指定であり、将来にわたって「最新」であると仮定しない。既存manifestがある場合は、その依存関係を確認してから変更する。実際の解決バージョンは`Cargo.lock`で管理し、更新は独立した検証対象にする。

### 3.2 ビルド環境

`vk-mem`は同梱のC++コードをビルドするため、対象環境のC++ツールチェーンを用意する。Rustだけで完結する依存と考えない。コンパイラー、ターゲット、ネイティブ依存の準備をCIと開発手順に記載する。[^vmbuild]

Vulkanローダー、対応ドライバー、検証レイヤーの有無は、ビルド可否と分けて確認する。SDK・バインディングに定義があるだけでGPUが対応するとは判断せず、使用するfeatureは問い合わせと有効化を行う。[^features]

Rustのedition、MSRV、シェーダーコンパイラー、ウィンドウ接続用クレートは既存環境との整合を見て決定する。外部ツールに依存する場合はバージョンと再現手順を固定し、開発者個人の環境にだけ存在する生成物へ依存しない。

<a id="foundation-selection"></a>

### 3.3 Bistro到達から選んだ基盤と移行範囲

`renderer-7ft.1.3`、調査日2026-11-14。**native基盤の改修と上位操作の置換を採用する。**
Vulkanはash 0.38／vk-mem 0.5、D3D12は現行の生成bindingsとDevice/Fence所有を出発点にする。
第7章のA（構築時owned命令＋binding/attachment由来アクセス）を両APIへ接続し、
利用側の画像変換やeffectは不変のProgramとGraph命令で記述する。
この選定比較単独は実装の証拠にせず、現行の一般パスとB2の接続・受入は[最新の検証結果](../verification/current.md#bistro-pbr)で確認する。

判断の順序は、確定要件への適合、保証・製品費用を管理できる境界、Bistroまでの総作業、保守負担とする。
既存コード量や過去の工数を採用理由にしない。再利用するのは、将来も必要な契約を実装し、
失敗・pending・最終解放までの回帰試験を持つ部品である。汎用化で前提が変わる箇所は再検証する。
aliasing、複数queue、vendor固有最適化の直接制御は今回の選定条件にしていない。

#### 同じ利用コードで比較した適合

入力は第7章の`Study::add`／`scenario`／`bloom`で固定する。
下表の「対応可能」は一次資料とAPI写像からの設計判断であり、全入力のnative実行済みを意味しない。

| 同一入力 | 固定操作を維持 | native上位置換 | safe wgpu 30.0.2 | wgpu-hal 30.0.2系を直接利用 |
|---|---|---|---|---|
| 一passの複数draw、短命sliceの借用 | 内部batchのみ。公開APIの一般化が必要 | owned列へ変換しnative記録へ渡す | owned列からRenderPassへ変換可能。RenderBundleも選べる | owned列からunsafe encoderへ変換可能 |
| depth-only／color-only、MRT／mip View | color+depth固定、mip0/layer0限定 | attachment配列とsubresource計画を追加 | optional depthとcolor配列、TextureViewへ写像 | 同様。ただし状態・範囲・寿命を自前保証 |
| 独自Computeと多段Bloom | 固定変換の追加では要件未達 | Program/binding/dispatch。依存dispatchを別passにする | ComputePipeline/BindGroup/ComputePassへ写像 | pipeline/bindingと明示barrierへ写像 |
| 履歴初回／継続／resize | Vulkanに既存契約、D3D12未対応 | canonical identity/revisionを拡張 | 強いhandleとqueue追跡を委譲。Graphの内容version/resetは自前 | identity、状態、所有、完了を自前で実装 |
| Windows二API表示と操作 | Vulkan表示のみ、自由cameraなし | Vulkan WSI再利用＋D3D12 DXGI新規 | 両backendのSurfaceを委譲。camera/event loopは利用側 | Surface機構を利用し、世代・完了・失敗を統合 |
| CPU/Linuxと既存受入 | 維持できるが最終要件未達 | GPU非依存GraphとLinux Vulkan build/CPU試験を維持 | GPU非依存Graphを残す設計が必要 | 同左。unsafe契約の二APIレビューも必要 |

全候補でModel、素材cook、PBR/IBL/CSM、Bloom、自由camera、画像差分の校正と性能測定は利用側／Rendererで作る。
外部基盤がBistroの描画機能やGraphのproducer/cullingを完成させるわけではない。
現行範囲は[Graphの形式・固定操作](../../crates/renderer-core/src/graph.rs)、
[Vulkanの固定Program](../../crates/renderer-core/src/backends/vulkan/program/creation.rs)、
[D3D12のprepare対応範囲](../../crates/renderer-core/src/backends/d3d12/graph.rs)から確認した。
高位のrenderer/engineをさらに採用する案は、そのscene/material/Graph境界を第7章へ再適合する費用が増えるため、
直接二APIを提供するwgpuを外部案の代表とした。候補網羅を採用の前提にしない。

#### 外部案の利点と製品方針との境界

[wgpu 30.0.2の固定版](https://github.com/gfx-rs/wgpu/releases/tag/v30.0.2)を比較対象とする。
native Vulkan/D3D12、資源の強い所有、同期・退役、shader変換、Surfaceをまとめて委譲できるため、
**二APIの最初の画像へ到達する実装負担が最も小さいと見込む外部案**である。速度の実測順位ではない。
即時のpass encoderにAのowned列を走査して渡せるため、callback案Bへの変更やnative型の公開は必須ではない。
WGSLから両backendへ生成する選択も可能だが、既存SPIR-V/DXIL生成との移行・ABI再検証が要る。

ただし、[`InstanceFlags::VALIDATION`](https://github.com/gfx-rs/wgpu/blob/v30.0.2/wgpu-types/src/instance.rs)はVulkan layer／D3D12 Debug Layer等の制御であり、
[wgpu-coreのpublic API検査](https://github.com/gfx-rs/wgpu/blob/v30.0.2/wgpu-core/Cargo.toml)を
製品からコンパイル除外するスイッチではない。
`strict_asserts`の追加検査とも区別する。
[draw記録](https://github.com/gfx-rs/wgpu/blob/v30.0.2/wgpu-core/src/command/render.rs)は
pipeline/bindingとdraw範囲等を扱い、Rendererが不変な命令として保証した後にもcoreへ渡すたびに検査する。
これらは一般的なsafe wgpu APIの保証に必要であり、wgpuの不具合や不要な診断という意味ではない。
一方、privateな保証済み入力についてdraw/submitで同じ内容を再検査しない本Rendererの規約とは境界が異なる。

対案として[RenderBundle](https://github.com/gfx-rs/wgpu/blob/v30.0.2/wgpu-core/src/command/bundle.rs)は
finishで検査したdrawを再実行でき、固定sceneの再検査を減らせる。
execute時の同一Device、attachment適合、resource liveness、使用scope、必要な初期化は別に残る。
それらを一括して開発診断とは分類しない。動的なdraw列の再構築とCompute記録には同じ回避策を適用できず、
publicなunchecked draw/dispatchや全core検査offの設定としては使えない。
raw HAL encoderへの[escape hatch](https://github.com/gfx-rs/wgpu/blob/v30.0.2/wgpu-core/src/as_hal.rs)も
通常のwgpu記録済みencoderへそのまま混在できる経路ではなく、状態・barrier・寿命の責務を戻す。

safe wgpuへ保証境界を委譲し、Renderer側の重複検査だけを避ける案は合理的な代案である。
採るなら「独立した追加診断の除去」と「coreの反復契約検査の許容」を区別した明示的な方針改訂が要る。
今回は[検証方針](../guides/validation-policy.md)とAGENTSの製品規約を維持し、
core forkや非公開unchecked経路を採用の前提にしない。safe wgpuを遅いと測定したための不採用ではない。

[wgpu-hal](https://docs.rs/wgpu-hal/30.0.2/wgpu_hal/)を直接使えば、二APIのnative変換とallocator等を利用しつつ、
呼出側が検査・状態追跡・barrier・生存期間を保証できる。
ただしunsafe APIであり、公式文書自身も安全性要件の記述が完全でないと説明する。
coreを外しても安全なGraph/runtimeは得られず、二APIの状態と実装固有契約の調査、所有・失敗処理・WSI受入が残る。
既存native所有者とHAL所有者を併用して曖昧な回収にせず、一貫した所有連鎖へ移す費用も要る。
今回の小試作はsafe wgpuの実現可能性を調べるもので、HAL単独の健全性を実証していない。
HALへの置換は候補として残すが、Bistroまでに減る作業の確実な根拠が不足するため選ばない。

#### 工数・試験・保守の比較

以下は**熟練担当1名の計画用人日レンジ**で、実測工数・納期・性能の予測保証ではない。
外部案にも同じ画質・対象全量・二API表示・最終解放後までの検証を含める。
共通のB2/B3/B4/B5は各10–17／10–18／4–7／7–12人日、合計31–54人日を仮置きする。
仕様探索、失敗経路、画像校正、ガイドを含み、別OS実GPUや高度な最適化は含まない。B1後に再見積もりする。

| 候補 | B1とWindows二API表示・camera統合 | 共通分込みBistro総工数 | 移行・保守と判断 |
|---|---|---|---|
| 固定Operation維持／D3D12へ完全移植後に一般化 | 要件未達の移植に手戻りが加わる | 固定操作のままでは完了できない | effect列挙と二系統APIを温存するため不採用 |
| **native上位置換＋低レベル再利用** | B1 18–32＋表示5–10 | **54–96** | unsafe native実装は自前保守。既存の所有・WSI回帰を拡張し、保証境界と製品費用を管理できるため採用 |
| native全面置換 | 上記に再初期化・転送・退役・WSI等15–30を追加 | 69–126 | 同じnative契約の再確立が必要。上位置換で解ける要求に全面置換を課さない |
| safe wgpu（保証の委譲に合わせた方針改訂を許す比較） | 17–31 | 48–85 | native実装負担は小さい。ただし現規約のままcore除去済みとはできず、今回の適合案ではない |
| wgpu-hal直接 | 29–50 | 60–104 | backend実装を共有できるが、core相当の保証と移行受入は自前。HAL単独の縦切りは未実証で、費用削減を確定しない |
| wgpu/core forkで保証済み命令の検査を除去 | safe案に10–25以上を仮加算 | 58–110以上、上限未確定 | unsafe契約再確立とupstream更新への追従が増える。未設計部分を確実な短縮として採用しない |

外部版更新によるAPI変更・shader出力差・依存監査と、nativeのSDK/bindings/VMA更新・VUID追従は、
いずれも将来の保守費用である。たとえばwgpu 30の表示はacquire→submit→`Queue::present`であり、
古いSurfaceTexture::presentの例をそのまま使わない。
[Surface::configure](https://docs.rs/wgpu/30.0.2/wgpu/struct.Surface.html#method.configure)のidle待機、
ゼロextentや旧SurfaceTexture保持時の制約も、最小化・resize・pending終了の統合費用へ含めた。
submit完了callbackを画面への表示完了と同一視しない。
現行一crateを分割することや互換wrapperは移行の先行条件にしない。
採用案の最大のリスクは、D3D12画像・PSO/descriptor・DXGI表示の新規量と一般化の安全性である。
まずB1の二API縦切りで見積もりと所有設計を更新し、scopeを超える費用が判明すれば本比較へ戻す。

#### 採用案の責務と移行順

| 対象 | 再利用／置換と保証 |
|---|---|
| Vulkan低レベル | Instance/Device、VMA資源所有、flush/invalidate、staging lease、Fence/timeout、pending/退役、WSI世代とpresent fence、最終破棄診断を再利用する。一般binding参照を含む保持へ拡張 |
| D3D12低レベル | COM Device/queue/Fence、投入前の所有確立とSignal失敗時の保持、診断lifetimeを再利用。画像、PSO/root signature/descriptor、registry/import、再利用、DXGIは新規 |
| Graph | producer/hazard/cullingと回帰fixtureを再利用候補にする。owned draw/dispatch/copy、attachment配列、stageとsubresourceを導出する上位表現へ置換し、範囲解析を再検証 |
| Program/binding | 固定Program群を不変なshader/pipeline/layout/binding版へ置換。生成・作成・更新境界でABI/owner/範囲/Device能力を保証し、更新は新しい版を作る |
| shader/tool | 固定SDKのGLSL→SPIR-V、SPIRV-Cross→HLSL→DXILを出発点にし、任意の管理下programにmanifest/reflectionを拡張。新effectは利用側shader。未信頼shaderのsandbox化はしない |
| engine/sample | Modelとcookは利用側。旧Vertex/Materialと固定合成を標準APIとして温存せず、in-repo consumerを更新。表示event loop、自由camera、固定巡回は利用側に置く |
| allocation | VulkanはVMAを継続。D3D12の最初の画像fixtureはcommitted allocationで成立させ、B2で全素材の常駐量・budget・粒度を測ってsuballocationの必要性と外部allocator採否を決める。収容可能とは未証明 |

初回対象は既存Windows x64／RTX 3060のVulkan 1.3とD3D12。
GPU非依存のCPU経路、LinuxのCPU/Vulkan build・通常試験を維持し、Linux表示・実GPUを新たな受入済み範囲へ広げない。
Vulkanの必要featureとWSI拡張は現行契約を維持し、新しいformat/binding能力は作成時に照会・有効化する。
Forward/GGX、静的IBL、CSM/PCF、RGBA16F、Neutral、FXAAを第15章の出発案として採用し、
具体的なshadow設定・mip・tangent・budgetは各縦切りで校正する。

移行はB1のProgram/binding保証→独自Raster二API→Compute/copy→永続資源・失敗/退役→二API表示の順を基本とする。
共通契約を先に巨大なRHI traitへ固定せず、同じfixtureの二API結果から抽出する。
各段階で適用範囲のrelease＋追加検証＋厳格native検証と、release＋追加検証/専用診断除外＋native Disabledを別実行する。
保証済み本文の再走査を移す場合は、保証期間・失効条件・残る同期/容量/失敗/完了処理を記録する。
既存のnative受入を落とさず、旧経路は新経路の該当回帰が揃ってから置換する。互換APIの長期併存は目的にしない。
比較試作の再現・実行範囲は[開発手順](../guides/development.md#foundation-comparison)と
[現在の検証状況](../verification/current.md#foundation-comparison)を参照する。

<a id="source-boundaries"></a>

### 3.4 全体レビューから定めたソース整理の境界

`renderer-420`で第一者のRust、shader、素材tool、生成・検証script、CIと文書構成を確認した。
確定した不要な処理を修正し、残った4つの責務境界は`renderer-hyf`でB1の機能追加に先行して整理した。
実行結果と確認限界は[現在の検証状況](../verification/current.md#repository-review)を参照する。

| 整理前の結合 | 現在の責務境界 |
|---|---|
| `graph.rs`の資源型・固定操作・builderと`engine::MeshData`依存 | `graph.rs`は公開facadeとGraph所有状態。`graph/{resource,pass,error,builder}.rs`と既存解析を分離。meshの不変stream/provenance選択は`geometry.rs`、検査と追加はGraph内の狭いbuilderが所有し、Graphからengineへの依存を除去 |
| Vulkan固定Programの作成、descriptor arena/cache、native記録 | `program.rs`が不変publicationを所有し、`program/{creation,bindings,record}.rs`へ分離。BindingPoolはinlineで容量・cache・leaseを管理し、追加allocation/Arc/lockなし。Graph型への依存はrecordの固定描画引数だけ |
| Graph配下の物理資源所有とVulkan実行エラー | 両backendの`resources.rs`へ物理buffer/image/CPU Mapを分離し、Vulkanエラーは`error.rs`へ移した。工場はGraph型に依存しない。D3D12のCPUアクセスは作成・完了・排他的所有のprivate unsafe契約を明記し、runtime work/resultがDevice所有を保持 |
| Vulkan表示のSwapchain所有、slot/cancel、記録と公開入口 | native `presentation/{surface,swapchain,slots,program,commands}.rs`が所有・退役・記録を担当。Graph側`presentation/{state,presenter,runtime,work}.rs`だけがcanonical状態とleaseを保持し、native側にGraph型を持ち込まない |

backendのnative型は既にprivateで、D3D12本番実装からVulkanへの依存はない。
生成D3D12 bindingsはfilterと固定generatorから再生成し、手書きで分割しない。
`foo.rs`と`foo/`の配置、実装末尾のunit testと公開APIのintegration testを維持する。
行数だけを理由にmoduleを増やさず、crate分割、巨大なRHI trait、互換wrapperを先行条件にしない。
公開入口はfacadeからの直接reexportで一つに保ち、所有者の内部状態は必要な操作へ閉じる。
構成整理と一般Program/manifest/reflection、一般資源・owned passの機能追加は別commit/課題とする。
後者は`renderer-7ft.2.1`以降で実装し、今回の整理をその機能の完了と扱わない。

<a id="math-library-policy"></a>

### 3.5 数学ライブラリと座標変換の責務

ゲームエンジンを設計する際は、ベクトル・行列・クォータニオンとその基本演算に `glam` などの既存ライブラリを使い、必要な層から直接利用することを推奨する。以下の型名とテストは設計例である。

| 層 | 所有する意味と演算 |
|---|---|
| 共通の空間・幾何 | エンジン内で共有する軸の向きと長さの単位を定める。レイ、軸平行境界ボックス（AABB）、点と平面の符号付き距離など、利用側が同じ定義を使えるものを共有する。 |
| シーン・アニメーション | ローカル姿勢、親子関係、ワールド変換の更新を所有する。編集と階層合成では、それぞれに適した表現を選ぶ。 |
| 描画・物理 | 描画側はクリップ空間の深度規約や視錐台を、物理側は剛体の姿勢や接触とみなす距離を所有する。剛体姿勢の型が許す操作は平行移動と回転に限る。 |
| GPU・ファイルとの境界 | シェーダーが読む行列の配置・アラインメントに合わせて値を書き出す。素材の軸と単位はインポート時にエンジンの規約へ変換する。 |

たとえば子のローカル姿勢は TRS（平行移動・回転・スケール）で編集し、合成済みのワールド変換はアフィン変換で保持する。
列ベクトル規約なら `world_from_parent * parent_from_child` は子の点をワールドへ移す。
親に非一様スケールがあり、子がその主軸から傾くと、合成後の基底は直交しないことがある。ここで再び TRS に分解するとせん断を失い得る。描画へは合成したアフィン変換を渡す。

**差し替えの範囲は、誰が実装を選ぶかで決める。**

- エンジン内部では外部型を直接使える。たとえばレイと三角形の交差判定だけに追加の精度が必要なら、その演算を自前化し、ベクトルの保存と他の演算には同じライブラリを使い続けられる。
- エンジン作者が利用側のソースを変えずに実装を交換したい公開境界には、内部表現を隠した具体型を置く。たとえば `WorldPose` の点変換では、引数と戻り値の `Point3` までエンジンが所有する。`pub type Point3 = glam::Vec3` という別名では利用側が `glam` 固有のメソッドも呼べるため、交換時にその呼び出しが影響を受ける。
- 利用側が実装を選ぶ場合や、同じアルゴリズムを複数の実装で使う場合は、必要な操作だけを trait にする。たとえば点変換だけを使う処理には、入力・出力の点型と変換操作があればよい。全演算を一つの trait にまとめると、数学実装を選ぶ型引数がシーンやカメラにまで広がる。

**共通の挙動テスト。** 交換対象の公開 API には、入出力の座標空間と単位、変換方向、合成順、失敗条件を記す。trait のシグネチャは呼び出せる操作を定める。演算の意味は、各実装に同じ入力と期待値を与えて確認する。

- 右手系・列ベクトル・メートル単位の例では、子を親の X 方向へ 2 m 移し、親を原点のまわりで +Z 軸正方向に 90 度回す。子の原点はワールドの `(0, 2, 0)` m に来る。既知の期待座標で軸と合成順を検査できる。
- 可逆な変換で点を往復させ、元の座標との差を入力の尺度と用途に応じて定めた許容誤差と比較する。逆変換が失敗を返せる API なら、一軸のスケールがゼロの入力で仕様どおりの失敗を返すことも確かめる。

この互換性を保つ範囲は、差し替えを約束した公開境界ごとに選ぶ。

## 4. 設計の中心は「グラフを実行する基盤」

### 4.1 論理的な責務分担

以下は責務の区分であり、この数だけクレートや抽象レイヤーを作る指示ではない。

| 責務 | 内容 |
|---|---|
| Engine-facing API | メッシュ・マテリアル・View・描画入力・出力など、エンジンが利用する契約。 |
| Graph API | パス、リソースアクセス、依存、出力、副作用、カスタム処理の宣言。 |
| 標準パス・標準グラフ | 公開Graph APIを使う再利用部品と、その組合せによる任意の描画構成。Engineの必須処理にせず、[三層方針](#public-api-layers)に従う。 |
| 共通解析 | 依存解析、不要パス除去、使用範囲・論理寿命の解析、宣言の検証。 |
| バックエンドを考慮する計画 | キュー、同期、一時メモリ、記録単位を対象APIの制約に合わせて決める。 |
| パス内の命令記録 | パイプライン・バインディング・draw・dispatch・copyなどを扱う。 |
| 長寿命ランタイム | デバイス、永続リソース、キャッシュ、GPU完了、遅延破棄、複数グラフ間の状態を管理する。通常の準備とフレームの寿命はバックエンド間で共通の公開契約を持つ。 |
| Presentation | Surface、Swapchain、acquire、present、再作成、表示側の同期を扱う。 |

共通解析だけで全実行計画を固定せず、メモリ要件、キュー能力、フォーマット制約などをバックエンドから計画部へ返せるようにする。参考例としてAtomは、共通のFrameGraphコンパイル処理と、API固有の同期・実行への具体化を分けている。本書はその開発順序やクラス構成を再現する指示ではない。[^atom]

ビュー描画の初期化は `renderer` の `RendererSetup` が担当する。シーンの選択、ウィンドウ、
外部画像の所有権と同期はホストが担当する。シーンの走査、容量の集約、資源作成は初期化・構成変更の境界で行う。
通常フレームは既存の ViewRenderer と同じプール、履歴トランザクション、GPU の完了契約を使う。
共有 Runtime への接続ではホストが準備した作業用資源を使用し、初期化に使った投入のみを ID で回収する。
中断した初期化は選択したアセット版を復元する。明示的な構成変更では旧登録と新しい資源の重複を予約し、
固定容量と未回収の所有者を検査する。[初期化と分割ロードの手順](../guides/tooling.md#scene-materials)

### 4.2 初期の配置例

```text
renderer/
  api/                 # エンジン向けの意味論・ハンドル・エラー
  graph/               # 宣言、中間データ、共通解析、グラフのテスト
  runtime/             # フレーム、永続リソース、完了、遅延破棄
  backends/
    vulkan/            # ash・vk-memを使用する実装
      device/
      resources/
      memory/
      graph_plan/
      commands/
      sync/
  present/             # 表示との境界。Vulkan固有部は明確に分離
  shaders/             # シェーダーの契約・生成・キャッシュとの接続
  diagnostics/         # グラフダンプ、識別子、計測
examples/
tests/
docs/
  guides/              # 現行の開発・利用手順
  design/              # 現在有効な設計と判断理由
  verification/current.md
```

既存の`renderer-core`、`renderer-present`等があれば、責務を対応づけて利用する。グラフをcoreへ置くか別クレートへ置くかは、依存方向とテストの必要性で決める。名称を合わせるためだけの大規模移動は行わない。

必須条件は、グラフの純粋な解析テストがGPU・ウィンドウなしで実行できること、通常の公開APIがVulkanやVMAの型へ依存しないこと、Presentationのために解析部分へSwapchain管理を持ち込まないことである。

<a id="steady-state-memory"></a>

### 4.3 通常フレームのメモリ規約（全バックエンド共通）

**通常フレームでは、Renderer本体の汎用CPUヒープ確保・再確保・解放と、Rendererが発行する
native資源・メモリの新規作成・破棄をゼロにすることを設計目標とする。**
ユーザー指定により、従来の「無制限な確保を避ける」方針を強める。
目的はフレームごとの確保・解放費用とそのばらつきを抑え、容量と実行費用を予測できる構造にすることである。

「通常フレーム」は、準備済みの資源・構成・容量上限内で反復するGraph構築/compile、prepare、
record、submit、poll/retire、present、結果のCPU消費/返却までを含む。
上限内の入力値・draw数・パス構成が変わる場合も対象とする。
同じ毎フレーム処理を別thread/APIや「準備」「回収」へ移すだけで対象外にしてはならない。

初期化、明示したwarm-up、ロード、資源追加/削除、resize/設定変更、容量変更、保守的trim、
異常処理/終了は、それぞれの準備・変更・保守境界として費用を別に記録する。
必要なworking setを予算内で予約し、通常フレームでは配列・作業領域・資源を再利用する。
全資源を起動時に最大量確保することや、特定のarena/allocator/cacheの採用は要求しない。
追加資源も既存poolの空きを使える。領域内のslot予約/返却、suballocation、resetは許可するが、
そのmetadataや要素の破棄も汎用ヒープの増減を起こさない設計にする。
通常の容量不足では暗黙のヒープ拡張・native増設・GPU待機へfallbackせず、
既存の型付き容量不足/backpressureを返し、明示した準備境界での容量変更や再試行を呼出元が選ぶ。

再利用/破棄は記録中の参照、実GPU完了、必要なpresent完了、readback等のCPU消費終了を満たしてから行う。
資源削除/trimの実破棄が遅延する場合も、安全な退役を変更・保守処理として追跡する。
通常frameの一時データ回収全体を例外扱いにせず、ゼロ化のために早期解放、未完了領域のreset、
必要な検証・同期・容量管理・失敗処理の削除を行わない。

達成を主張するときは、最適化された製品構成の通常フレーム全体について、対象入力/上限、warm-up、
feature、計測範囲を記録し、CPUのalloc/realloc/dealloc回数とnative作成/破棄回数を別に確認する。
VMA/poolの利用や少ないalloc回数だけではゼロ達成の証明にならない。
Rendererが制御しないdriver内部の確保は保証と区別し、未観測なら未確認とする。
開発用検証・ログ・profilingの追加費用は別構成/範囲で記録し、Renderer本体の費用をそこへ隠さない。
未達箇所・対応範囲と未実行の確認はBeadsと[現在の検証状況](../verification/current.md)へ記録する。
この規約の制定は、現行実装のゼロ達成や性能改善を示すものではない。

<a id="prepared-frame-storage"></a>

### 4.4 共通Graph・Engineの準備済み領域

`renderer-7ft.2.2.1`は、前節の通常フレーム規約に従って共通の入力・命令・解析領域を予約する。
実装した所有契約は[RenderGraphのslot設計](rendergraph.md#prepared-graph-storage)を正とする。
これはMRT/mipや任意shaderを実装したという意味でも、native実行・表示・CPU結果まで含む
全体のゼロ達成の宣言でもない。実行した構成と残る確認は[検証状況](../verification/current.md)へ記録する。

| 準備境界 | 通常フレームで使うものと容量 |
|---|---|
| `MeshRegistry::new(MeshCapacity)` | meshの登録枠、更新・削除された旧mesh版のretired枠 |
| `SnapshotPool::new(draw_capacity, slots)` | 可変draw入力のcapture、mesh解決scratch、同時snapshotの所有 |
| `GraphPool::new(GraphCapacity, slots)` | resource/pass/access/draw、明示依存、初期化range、名前/upload byte、解析edge/区間/readerの予約領域 |
| `ImportRangesPool::new(range_capacity, slots)` | runtimeが公開する不変な初期化範囲snapshot、Clone・返却用slot |

構築は`GraphPool::begin(&mut self)`から始め、`Graph::compile(self)`で同じ領域を凍結する。
GraphはCloneせず、`CompiledGraph`のCloneは予約済みslotへのArc pinだけを追加する。
全accessを解析用Vecへ射影せず、flatな入力sliceと、重ならないworkspace/outputを分けて借用する。
dataflow・culling・live hazard・順序・最終内容は、変わる入力とpass構成について毎回計算する。
固定planのcache、一定draw数だけの特別経路、構築・返却を測定外へ移す方法で対象を狭めない。

各slotの再取得は全consumer終了後だけに許可する。`GraphPool::begin`と範囲snapshotのcaptureは
`Arc::get_mut`で排他性・退役時の同期を確立し、Graph identityも再取得時に更新する。
poolの所有者は通常処理の間保持し、nativeのprovenanceや未読CPU結果が持つpinも同時容量へ含める。
`GraphPool`／`ImportRangesPool`の容量変更では`is_idle`を確認する。
これらが使用中の場合や`SnapshotPool`を変更する場合は、必要なconsumerが終わるまで旧pool ownerを保持し、
変更に紐づく明示保守で破棄する。通常frameの返却による最後の解放を保守扱いへ移さない。
通常のslot不足・metadata不足は型付き容量エラーで返し、暗黙の拡張、native増設、GPU待機は行わない。
capacity/backpressureのmessageはstaticとし、不正入力・native異常の詳細診断とは別に計測する。

公開uploadは`&[u8]`から予約済みblobへcaptureする。所有Vecと公開Shared uploadは撤去し、
任意の最後のArcが通常のplan返却でheapを解放する経路を残さない。
private mesh uploadだけは、常駐またはretired assetがanchorを持つ不変streamを共有する。
culled命令は実行順から除外するが、bounded backingには元の入力が残り得る。
plan返却後も容量は保持し、次のcheckoutでresetするため、従来の「culled本文を直ちに非保持」とは保持時期が異なる。
この差を、culled writeによる内容初期化や、通常frameの回収全体を変更境界へ移す理由にはしない。

assetの更新・削除時は旧版をbounded retired領域へ移す。明示保守でidle Graph slotの
private asset pinを`release_idle`により返し、`collect_retired_meshes`が外側のmesh版と
両byte streamのconsumerを確認して最後のanchorを退役させる。
上限不足なら変更を部分適用せずに返す。asset変更後の最終解放を通常frameへ漏らさない所有契約と、
通常のsnapshot/Graph/plan入力領域を再利用する契約を区別する。

標準pipelineの `Meshes::new(MeshCapacity)` は登録表、同時に選べるmesh数の転送計画、
公開済み・転送途中の旧版を保持する退役キューを予約する。`stream_gpu` は選択順にbyte予算を配り、
同じhandleを何度指定しても計画は1件とする。予算ゼロの照会、部分転送、完成版への切替では同じ領域を再利用する。
submitに失敗した場合は既に取得したprivate登録を保持し、次の呼び出しで直前に受理されたoffsetから再試行する。
meshの作成・更新でgeometryとlayoutを所有する費用はasset変更境界に属する。旧版の最終layout解放と
登録解除は明示した `collect_gpu` ／ `release_gpu` で行う。

CPUのallocation、reallocation、deallocationは、それぞれcaptureから結果返却までの連続区間で確認する。
optional dump/ログの費用やnative作成・破棄は別に記録する。
論理Graphの区間は予約済みAVL node arena、nativeのCPU provenance等は予約済み配列を使う。
木のmetadataと更新、配列の挿入・削除の費用は残るため、確保ゼロだけでCPU時間の改善を主張しない。

## 5. RHIを抽出する進め方

### 5.1 最初に固定しないもの

Vulkan・D3D12・Metalの全操作を列挙した巨大な`Backend`トレイト、共通コマンドバッファの全仕様、全拡張を表す列挙型、独立利用可能なRHIクレートは先に作らない。

B0で選んだnative基盤では、Vulkanバックエンド内でashを直接使ってよい。少数の検証用パスにネイティブな命令記録が必要な場合も、実験用・バックエンド内部の経路として局所化する。

### 5.2 最初から守る境界

論理リソースと物理リソース、アクセスの意味とVulkan状態、グラフの寿命とGPU完了、グラフ構築と命令実行は分ける。ネイティブハンドルを通常のGraph APIの識別子として使わない。

境界を保つための最小限の型・関数・内部インターフェースは作ってよい。「RHIを後から抽出する」ことを理由に、無制限な直接依存を許さない。

### 5.3 第二バックエンドで確認すること

第二バックエンドはVulkanと異なるAPIから選ぶ。M3でWindows上のD3D12を選定した。
実機・CI・対象OS・保守担当を判断材料とした理由と見直し条件は[16.1節](#161-第二バックエンドの選定と準備条件)に記す。

同じ論理グラフ、同等のシェーダー処理、同じ出力条件を動かしながら、次を分類する。

| 分類 | 方針 |
|---|---|
| グラフの意味として共通 | 宣言・依存解析・不要パス除去などを共有する。 |
| GPU操作の契約として共通 | リソース記述、パス内操作、完了確認などを必要な範囲で抽出する。 |
| プラットフォーム固有 | バリアの表現、メモリ配置制約、キュー実装などをバックエンドへ残す。 |

二つ目のRenderGraph全体をコピーして別系統にしない。一方、少数の実験パスで命令記録が重複することは許容する。共通化するのは似たコードではなく、同じ前提と保証を提供する契約である。

Vulkanバックエンドを別OSで動かすことは移植テストであり、異なるGPU APIへの抽象化を検証したことにはならない。MoltenVK経由の検証だけで、ネイティブMetalバックエンドの契約を検証済みとしない。

### Vulkan／D3D12の共通実行契約

アプリはGraphの上限、同時に処理するフレーム数、保持するCPU結果の数、資源とProgramの作業セットを指定する。
資源クラスの予約数には同時に保持する物理インスタンスを含める。初期化と作業セットの変更時に準備し、通常フレームでは予約枠を再利用する。
転送時の行配置、アラインメント、GPUとCPUの可視性は各バックエンドで処理する。
Vulkanの共有stagingとD3D12の実行枠に対応する転送領域は、初期化時の固有設定で容量を調整できる。
どちらの方式を採用するかは、対象の転送量、同時保持数、実測したメモリ使用量と処理時間から判断する。

Runtimeは受理済みのGPU作業を保持する。アプリが受理情報を破棄するとexportメタデータが返り、GPU完了は後続のpollまたは明示waitで観測する。
後続Graphは同じRuntimeで受理されたexportを依存入力に使える。GPU完了後のreadbackはアプリが消費するまで独立した結果枠に置く。
容量不足では必要な回復操作を型付きで返す。動的な増設とGPU完了待ちは明示した呼び出しに置く。
表示の完了とGraphの完了はそれぞれの条件で追跡し、再準備では登録済み資源の識別と内容を維持する。

### 5.4 能力ベースの設計

共通の基本契約と任意機能を分ける。全APIの共通部分だけに制限しない一方、全APIの全機能を初期から実装することも目標にしない。

「利用可能」「実装済み」「有効化済み」「今回の計画で採用」は区別する。未対応機能は明示的にエラーまたはフォールバックへ進め、成功したように見せて何もしない実装を置かない。

## 6. RenderGraphの必須契約

### 6.1 宣言する情報

各パスは、デバッグ名、処理種別、使用リソース、アクセス範囲、読み書きの用途、入力・出力を宣言する。画像はmip・layer・aspect、バッファはbyte範囲を扱える意味論にする。

初期実装で範囲を粗く扱う場合は、安全側に全体として追跡してよい。ただし、未実装の細粒度追跡を提供しているように見せない。サポート外の指定は拒否するか、保守的な扱いを診断情報へ示す。

Attachmentのload/store、clear、discardも依存と内容の有効性に関わる契約として扱う。常にwrite-onlyと仮定せず、既存内容の保持が必要な操作を区別する。

### 6.2 依存解析と内容の有効性

RAW（書き込み後の読み込み）、WAR（読み込み後の書き込み）、WAW（書き込み後の書き込み）を識別する。単にパスをトポロジカルソートするだけで同期が成立したことにしない。

リソースのバージョン化、明示的なproducer-consumer関係、またはそれと同等の規則で「どの書き込みの結果を読むか」を決める。複数書き込みが曖昧な場合はエラーにする。構築順を意味のある規則として採用するなら、その制約を文書化する。

未初期化の読み込み、範囲外アクセス、未定義リソース、循環依存を検出する。`read_write`と宣言しただけで初期値が存在するとは扱わない。初回使用時にclearまたは完全な初期化を必要とするか、外部から有効な内容を受け取る契約が必要である。

### 6.3 不要パス除去と副作用

最終出力、export、readback、外部に観測される書き込み、副作用を起点として必要なパスを残す。外部リソースへの書き込みを、グラフ内に後続の読み込みがないという理由だけで削除しない。

実行コールバックは、不要パス除去で呼ばれない場合がある。コールバック内の任意のCPU処理を、毎フレーム必ず実行されるゲーム更新処理として使わせない。副作用を持つ処理には明示的な契約を設ける。

### 6.4 一つのパス内の同期

パス単位のread/write宣言だけで、複数dispatch間の依存まで自動的に解決できると約束しない。初期実装では、同期が必要な処理を別ノードへ分割する。後に内部同期APIを追加する場合は、グラフの保証範囲との境界を定義する。Vulkanでも、先行dispatchの書き込みを後続処理で読む場合には必要な依存を設定する。[^sync]

一つのグラフノードを、一つのネイティブRenderPassや一つのCommandBufferと恒久的に一対一対応させない。物理的な記録単位は計画側の判断として残す。

### 6.5 import / exportとグラフ外の状態

永続リソースの通常のimportでは、ランタイムが把握した識別情報・既存内容・前回の使用状態・未完了処理を引き継ぐ。同じ物理リソースやそのViewを複数回importしても、別物と誤認してハザードを見落とさない。

ネイティブ外部リソースの取り込みでは、所有権、初期状態、有効な内容、完了条件、返却時の条件を別契約として明示する。すべてを無条件に「初期状態は不明だからdiscard」で処理しない。

exportは、永続化または外部へ渡す意味と、利用開始条件を定める。CPUがexportハンドルを受け取った時点でGPU処理が完了したとは扱わない。複数グラフやフレーム間の依存もランタイムで引き継ぐ。

### 6.6 バインドレスとネイティブ拡張

将来バインドレスを導入する場合、アクセスし得るリソース集合、テーブル更新、descriptorの寿命を追跡する。GPU側で決まる参照先を、グラフが自動的に知っているとは仮定しない。

ネイティブ拡張は、使用リソース・入出力状態・同期・所有権を明示する限定経路とする。通常の安全なAPIから無制限な生ハンドル操作へ抜けられる設計にしない。保証できない操作には明確な`unsafe`契約または対応外という境界を設ける。

## 7. パス内操作と公開API

通常のカスタムパスには、宣言したリソースだけを利用してpipeline・binding・draw・dispatch等を定義できるAPIを提供する。固定shaderやeffect列挙の選択だけを、カスタムパス対応の完成とは扱わない。構築時に所有された命令列を作る方式、実行callback方式、採用基盤のbuilder等をB0の利用例で比較し、型名や記録時期を先に固定しない。

API設計の検証範囲は広く取り、実装は段階化する。複数draw、depth-only、color-only、独自Compute、多段処理の部品化、履歴import/export、将来の複数描画先・mip範囲を利用例として確認する。すべての機能を初回に実装する指示ではない。

UXの受入条件は、入出力と処理を近くで読めること、同じ資源情報を不必要に重複記述しないこと、通常作者へnative barrier・submitや不要なclone・複雑な寿命指定を要求しないこと、誤りを型・パス名・資源名で判断できることとする。Rustの所有・安全性とGPU寿命の保証を満たしたうえで評価する。

| 通常のパス作者が扱うもの | 実行基盤が扱うもの |
|---|---|
| 使用するパイプライン、バインディング、viewport、draw / dispatch / copy | パス間バリア、物理メモリ配置、キューへのsubmit、GPU完了と破棄 |
| 入力と出力、必要なリソース範囲 | 実行順序、一時リソースの実体化、不要パス除去 |

グラフの規則と、別の汎用RHIのsubmit・状態遷移規則を、通常の利用者へ二重に要求しない。内部にRHIができても、その全機能を公開しない。

パスの宣言と実行時のバインディングを対応づけ、宣言漏れやアクセス用途の不一致を可能な範囲で検証する。制約を満たす通常APIは安全に提供し、検証できないネイティブ操作やシェーダーの取り込みを同じ保証に混ぜない。

ユーザー向けに生のグラフ全体を自由に書き換えさせることだけを、拡張性の目標にしない。例えば「トーンマッピング前のHDRカラーへ処理を追加する」といった、安定した挿入点と入出力の意味を提供する。

その際、色空間、解像度、サンプル数、深度・法線の表現、有効なタイミング、必要な描画設定を明記する。利用できない入力を要求された場合は、診断可能なエラーまたは明示的な代替経路にする。

<a id="public-api-comparison"></a>

### 7.1 代表利用コードによる比較と第一案

`renderer-7ft.1.2`の設計対象は利用側から見た契約である。以下は**B1で実証する第一案**であり、
現行`Graph`の実装済み機能や基盤採用の決定ではない。
[当時のCPU試作](https://github.com/tsukushibito/renderer/blob/3fee62930ed59a01d2fdee902a9a11542fdd6de1/crates/renderer-core/examples/graph_api_comparison.rs)は二案を同じ入力で比較する。
[当時の利用コードと試作本体](https://github.com/tsukushibito/renderer/blob/3fee62930ed59a01d2fdee902a9a11542fdd6de1/crates/renderer-core/examples/support/graph_api_comparison.rs)の`Study::add`が
両案の呼出し、`scenario`が代表入力、`bloom`が利用側の多段処理である。
固定effectの選択肢を増やす方式は、独自shaderをGraph本体の変更なしで追加する条件を満たさない。

比較では二つの軸を分ける。**いつ命令を記録するか**と、**何から資源アクセスを導くか**は独立である。
callbackでも型付きpass data／bindingから宣言を作れるため、callback案だけへ資源の二重記述を課さない。

| 方式 | 所有・利用コード | 保証と費用の違い |
|---|---|---|
| A: 構築時に命令を所有 | 即時closureでframeのdraw slice等を借用し、終了後は命令・定数・資源参照をplanが所有する | 実際のbinding／attachmentから用途・範囲を導ける。除去される処理も命令生成する。命令列の格納とnative記録時の走査が必要 |
| B: frame内の遅延callback | 宣言時に型付きpass dataを渡し、同じdataと制限されたencoderで後から記録する。`FnOnce + 'frame`で借用できる | 不要passのcallbackを呼ばずに済む。先に宣言したアクセス集合を実命令が越えない保証、captureの寿命、record時の失敗処理が必要 |
| B0比較元の固定操作 | 当時の単一draw `Graph::raster`、固定`ComputeTransform`、build時custom closure | B0比較当時の制限を示す。現行の一般Program／独自Raster・HDRとB2の受入は[現在の検証結果](../verification/current.md#bistro-pbr)を参照する |

**第一案はAと、binding／attachmentからアクセスを導く公開builderの組合せ**とする。
通常作者が入出力とdraw／dispatchを同じ場所で書け、実命令が確定した境界で検査でき、
任意captureをnative記録まで保持する契約を標準経路へ増やさずに済むためである。
Bも借用可能で、`'static`や全資源の`Arc::clone`を必須にはしない。
大量の除去対象drawやlate bindingではBの利益があり得るが、CPU速度の優劣は今回の試作から結論しない。
基盤によってBが自然でAへの変換費用が大きい場合も比較し、[基盤選定](#foundation-selection)ではAを維持した。
型名・crate構成・内部enum・backendを凍結する判断ではない。

### 7.2 同じ入力で確認する利用例

試作の型は設計比較専用で、実Rendererへ接続しない。新しいshaderはpipelineの入力であり、
Bloom等のeffect名をGraphの実行命令へ追加しない。共通のdraw／dispatchと型付きbindingを使う。

| 利用例 | 比較・受入で確認する点 |
|---|---|
| 複数draw | 一Raster passでdraw順を保つ。mesh／materialを借用して記述でき、完成planは短命な借用に依存しない |
| depth-onlyとcolor-only | color配列0個＋depth、color1個＋depthなしを自然に書く。未使用attachmentやダミー資源を要求しない |
| 利用側の独自Compute | programと入力sampled／出力storage binding、dispatchを近くに書く。別途同じread／write一覧を要求しない |
| 多段Bloomの部品化 | 明部抽出→縮小→ぼかし→合成をmodule内で配線し、HDR handleを返す。無効化は入力を返す。依存するdispatchは別pass |
| 履歴import／export | 初回・resetは明示初期化し、通常frameは永続画像をimportして結果をexportする。extent変更時に旧内容を読まない |
| 将来のMRT／mip範囲 | attachment配列と同じ画像のView範囲で表す。別Viewを別資源とみなさない。非重複mipと重複範囲を区別する |

範囲はbufferの半開byte区間、画像のaspect／mip／layerとする。shader stageとsampled／storage／
vertex／index／attachment等の用途をbindingや操作から得る。画像内の任意矩形を追跡する約束はしない。
readはpassを追加した時点で直前のwriteに結び付け、後の順序制約で読む内容を変更しない。
write後の内容と、同じ物理資源に必要なRAW／WAR／WAWの順序は別に計画する。
利用側に明示version管理を必須とする案は採用せず、現行の宣言順による意味を出発点にする。
旧内容の保持や同時利用が必要な場合は別画像へ書く。部分writeを全域初期化として扱わない。
`storage_write`というアクセス指定だけでも全面初期化を認めない。clearで有効内容を確立するか、
管理下shaderの全域write保証を対象extent・dispatch条件とともに定める。後者の保証はshader変更時に失効する。

### 7.3 pipeline・binding・shaderの保証境界

| 境界 | 保証と失効条件 |
|---|---|
| shader生成・取込 | source/include、compiler/options、entry、stage、対象API／feature、binding ABIを特定する。binary形式検査とreflectionは任意shaderの挙動の証明ではない |
| program／pipeline作成 | Device能力、layout、定数のoffset/size/stride、vertex形式、attachment形式／数／sample、固定状態を確定する。不変の版として保持し、reloadは新しい版を作る |
| binding作成・更新 | program layoutと種別／配列数／stage、資源owner・世代、View範囲、offset/alignmentを照合する。参照するprogram・資源の版を保持し、更新は旧記録を変更しない |
| pass追加・compile | attachment操作、binding用途、範囲、同一資源の重複、初期化、出力とproducerを確定する。実命令と宣言を別管理しない。Bでは宣言集合を越える実行をrecord時にも拒否する |
| prepare／submit | 実Deviceでの対応、importの最新予定状態／revision、容量とnative戻り値を扱う。保証済みmesh本文やbinding ABIの全量再走査をdrawごとに行わない |

通常APIは管理下のshader生成・検査経路と、制限されたencoderを使うsafeな入口とする。
ここでの管理下shaderは、範囲・競合・同期・write保証をレビューする信頼されたassetを指す。
任意のsourceを同じcompilerへ通すだけでは信頼済みに昇格しない。任意sourceの取込にも、
binary取込と同じ保証を要求するか、その機能を提供しない。
資源を隠して参照するGPU address、bindlessの動的参照、外部native命令を通常経路へ混ぜない。
それらを後から導入する場合は到達可能な資源集合・更新・同期・所有を別途定義する。
外部binaryを無条件にsafeで受け入れず、保証できない取込には明示した`unsafe`境界か対応外を選ぶ。
`unsafe`の責務には有効なbinary／feature／ABIだけでなく、宣言外アクセス、範囲外アクセス、race、
必要なshader内同期を防ぐことを含める。checksumや`spirv-val`成功だけへその保証を委ねない。
Graphがshaderの値依存挙動まで証明する契約でも、未信頼shaderのsandboxでもない。

複数drawのRasterでは通常のattachment順序を使い、storage副作用やfeedbackを暗黙に許可しない。
同じ画像の重複subresourceをsampleしながら書く操作、依存dispatch、パス内部の同期が必要な操作は分割するか、
必要な機能・同期を提供できる場合に限り別契約で受け入れる。通常作者へnative barrierを要求しない。
Vulkan 1.3のDynamic Rendering区間へ任意barrierを入れる前提を置かない。
[Vulkan指針](../guides/vulkan-best-practices.md#synchronization)と
[検証方針](../guides/validation-policy.md#guarantees)を適用し、API契約からの例外は設けない。

### 7.4 所有・履歴・失敗と拡張の契約

構築closureの借用は終了時に解消する。planは必要な命令・定数と資源の不変な版を所有し、
compileでmesh／texture本文を複製しない。planからrecord済み・pendingの所有へ必要な参照を引き継ぐ。
CPUの最後のhandle、frame番号、callback終了をGPU完了とみなさない。
descriptor／View／pipelineと参照先は、記録から未投入cancelまたは実完了まで適切に保持する。
不要なCPU命令本文はnative記録後に解放できるが、native参照の寿命は別に維持する。
Bの借用callbackは同期prepare内で消費し、borrowを含まない仕事を返す。別threadへ送る場合の`Send`、
永続callbackの`'static`／共有所有は追加契約であり、frame内借用の標準経路へ強制しない。
callbackはcullingで呼ばれないことがある。ゲーム更新・乱数列の進行・外部通知等のCPU副作用を置かず、
frame入力は記録開始まで不変とする。再試行時もCPU処理の意味が変わらないよう分離する。
`FnOnce`のBはrecord失敗時に一部callbackが消費済みになり得るため、失敗したplanを破棄して再構築する。
Aの不変命令は未投入なら再記録できるが、再試行時もimportの状態・容量・Deviceの適合を確認する。

履歴は登録keyの番号だけを渡すのでなく、同じ物理資源のcanonicalな予定状態へ結び付ける。
成功したsubmit受理後に次frame用の状態／revisionを公開し、未投入失敗では進めない。
前のGPU完了をCPUで毎frame待たず、queue依存とpending所有をruntimeが維持する。
初回、reset、extent／format変更、Device再作成では内容保証を失効させる。
同じ履歴を競合する古いsnapshotから投入した場合は診断可能な失敗にする。
swapchainはacquire世代・extent・present失敗も含む別の外部資源契約であり、履歴番号だけで代用しない。

| 失敗段階 | 利用側へ返す意味と保持 |
|---|---|
| program／binding／pass構築 | unsupported、layout／範囲／owner不一致等を分類。pass追加は失敗したpassの半端な命令を公開しない。module途中の失敗ではframeの構築を打ち切れる |
| compile | 未初期化read、衝突用途、循環、必要な出力の欠落を文脈付きで返す。GPUへは未投入 |
| prepare／record | 容量不足、native作成・記録失敗、Bのcallback失敗。全記録が成功するまで一部だけsubmitしない。未投入資源を解放する |
| submit未受理 | APIが未投入・状態不変を保証する失敗だけrollbackする。一般の失敗へ拡張しない |
| 受理済み／timeout／Device loss | 受理は正常完了ではない。timeoutは保持を継続。lossは停止状態へ移し、内容を正常結果として返さない。present失敗でも受理済み仕事を取り消した扱いにしない |

エラーは安定した種別とpass／resource／binding／rangeの適用可能な文脈を持ち、文言解析を要求しない。
開発用の詳細名・履歴・dumpを製品でも常時保持する要件にはせず、運用上必要な原因とIDは残す。
構築保証に必要な拒否、計画・同期、import照合、容量・native失敗・退役は製品にも残す。
追加の独立再検査や診断scanは`development-validation`等でコンパイル除外し、最適化と独立に選ぶ。
これは目標契約であり、現行実装の未対応経路の検証を削除する変更ではない。

標準の挿入点は**tone mapping前、露出倍率適用前のscene-linear Rec.709／D65 HDR**とする。
Bistroでは1920×1080、sample 1、RGBA16Fを出発形式とし、Bloom後に固定EV、Neutral、sRGB変換を各一回適用する。
`conditions.json`の光・背景・色の規約を引き継ぐ。返却画像は同じextent／sample／HDR形式で内容が有効なものを要求する。
深度は任意入力として通常Zのdevice depth 0..1、near／farと逆投影情報を伴う。
法線を提供する場合はworld-spaceの正規化xyzと表現形式を明示し、未提供なら要求時にエラーか明示fallbackにする。
Bloomだけのために深度・法線を常設しない。挿入の時点と色の意味を安定契約とし、Graph内部のpass番号へ依存させない。
effect moduleはGraphと入力・設定を受け取り出力handleを返す。新effectは利用側moduleとshaderを追加し、
標準グラフの該当する挿入点へ渡すだけで接続できる。Graph／backendのeffect分岐の追加を通常手順にしない。

### 7.5 根拠・CPU費用と未実証の範囲

公開例の事実とRendererの判断を区別する。
[Filamentの説明](https://google.github.io/filament/notes/framegraph.html)と
[固定版header](https://github.com/google/filament/blob/65eaf091d7236b64094c59354c8632a86b911f92/filament/src/fg/FrameGraph.h#L248-L295)は
setupの即時実行とexecuteの遅延実行を分ける。
[Unreal RDG](https://dev.epicgames.com/documentation/unreal-engine/render-dependency-graph-in-unreal-engine)は
pass parameterに基づく依存と、遅延lambdaの寿命・CPU副作用への注意を示す。
[Unity 6000.0](https://docs.unity.com/en-us/engine/6000.0/manual/render-pipelines/universal-render-pipeline/customizing-urp/render-graph/write-render-pass)は
資源宣言とrender functionを分ける。これらはcallbackの実用例であり、Rustの第一案Aが高速・唯一という根拠ではない。
[Rustのclosure](https://doc.rust-lang.org/reference/types/closure.html)は借用とmove captureを許す。
`move`は参照先の所有まで移さず、trait objectの寿命も[明示できる](https://doc.rust-lang.org/reference/lifetime-elision.html#default-trait-object-lifetimes)。
調査は2026-11-14。動的資料の版と固定ソースの区別は[Graph指針](../guides/rendergraph-best-practices.md#sources)に従う。

| CPU段階 | Aで必要な仕事 | Bで必要な仕事 |
|---|---|---|
| build | 全宣言の命令生成・格納、入力から用途導出 | pass data／宣言集合とcallbackの保持。capture所有は選択方式による |
| compile | live解析・初期化／hazard／順序等。除去対象を実行順へ入れず、入力領域は所有終了後に再利用 | 同じ解析。保守的な宣言集合が広ければ余分な依存・生存を作り得る |
| record | live命令の走査とnative変換 | live callback呼出し、命令生成／native変換、宣言集合の逸脱を防ぐ処理 |
| retain／retire | 命令payloadとnative leaseを必要期間だけ保持 | callback／captureはrecordまで、native leaseは実完了まで保持 |

CPU試作のpass・命令・用途件数はこの違いを観察する構造的証拠であり、allocator回数・byte全量・
Rustのcode size・cache効率・native record時間の計測ではない。
実測時はbuild／compile／prepare-record／submit／retire、確保回数／保持byteを同じdraw数とculling率で分ける。
同じ資源を繰り返す場合と異なる資源、動的binding、解像度変更、検証on/offを比較し、
pipeline初回作成と定常状態を分離する。現行共通領域の予約・再利用は
[4.4節](#prepared-frame-storage)で定め、plan cacheや並列recordは別の測定後の選択とする。
このCPU比較単独からGPU同期・寿命・shader ABI・画像結果・Windows表示・性能の成功を推定せず、それぞれの実機受入で確認する。
基盤の維持／改修／置換／外部利用と総工数の比較は[3.3節](#foundation-selection)に記す。

<a id="public-api-layers"></a>

### 7.6 汎用Graph・標準パス・標準グラフの公開方針

**採用方針:** 次の三層を公開し、標準機能の利用と利用側によるパス定義を同じ基盤で成立させる。
以下は採用した契約であり、実装済みの範囲は[公開標準部品](rendergraph.md#公開標準パスと標準グラフ)と作業順序を参照する。
snapshotを使う標準描画の入口は[現行契約](rendergraph.md#エンジン入力と標準描画)、移行は[作業順序](#public-api-migration)を参照する。
二つの製品クレートにこの三層を配置した根拠と受入条件は[公開 API の設計と実装](#renderer-core-plan)に記す。

| 公開する層 | 責務と利用方法 |
|---|---|
| 汎用Graph API | `renderer-core`で提供する。利用側がProgram/pipeline、binding、draw/dispatch/copy、資源と接続を定義する。独自のGraphも構築・実行できる。 |
| 標準パス | `renderer`で提供する、公開Graph APIを使った再利用部品。利用側が不透明描画やポスト処理など必要な部品を直接組み合わせられる。 |
| 標準グラフ | `renderer`で提供する、公開標準パスを組み合わせる構築レシピ。意味を定めた箇所で利用側の処理を交換・追加できる。 |

依存は`アプリ → renderer → renderer-core`の向きとする。Graph、Program、snapshotの入力接続、容量の集約、GPU実行は`renderer-core`が担う。
描画方式に応じたパスの選択・合成を`renderer`に置くことで、独自構成は`renderer-core`だけに依存できる。
`renderer-core`は共通の組み込みProgramとABI、`renderer`は標準レシピ固有の監査済みシェーダーとABIを所有する。
Rustの標準パス、レシピと準備APIは既定の構成で利用できる。GPUで使うProgramは選んだバックエンドの形式だけを埋め込み、
パイプラインの作成と資源の予約は使用するレシピの`WorkingSet`に従う。
標準グラフは標準パスを組み合わせ、各パスは通常の構築関数と型付き入出力で公開Graph APIを呼ぶ。
Graphの共通解析・backendへ、opaqueやBloom等の効果選択を追加することを通常の拡張手順にしない。

ここで描画パイプラインとは、ある出力を得るための描画構成を指す。レシピは、その一部または全体を
レンダーグラフへ組み立てる定義である。レンダーグラフはフレームごとのパスと資源の依存関係を表し、
各パスはRaster、Compute、copyなどの処理単位となる。

`geometry::MeshRegistry` はメッシュの版を管理し、描画定数とともに不変な入力をcaptureする。Unlitの入力形式・tintは上位のレシピが扱う。runtimeは資源の常駐、同期、投入、GPU完了と退役を担う。
入力のGraph接続自体が標準opaqueを追加する設計にはしない。
`renderer::unlit_forward::snapshot`は、Engineの入力接続とunlit Forwardを組み合わせた便利APIを提供する。
利用側は同じ資源接続・実行経路で独自のGraphを構築できる。
snapshotを使わないCompute-only、複数View、必要な数の画像・buffer出力も基本APIの設計対象とし、
単一RGBA8画像やView寸法をすべてのGraphへ強制しない。nativeの対応範囲とPresentation固有の形式条件は別に維持する。

#### 共通の機能経路と部品の契約

標準パスと利用側パスは、同じ公開Program/pipeline/bindingとowned命令構築APIを使う。
標準だけが描画機能をprivateに利用できる状態を、カスタム対応の完成と扱わない。
現行の固定Raster Program群の公開・移動だけでは、独自shaderやblend/depth制御の提供は完成しない。
一方、検証済み入力の保証を保持するprivateな実装最適化までraw入力へ戻す必要はない。
安全な公開入力型等を通じて保証を引き継ぎ、[検証方針](../guides/validation-policy.md)に従って再走査を避ける。
shaderのtrusted取込境界と通常のsafeなパス定義は[7.3節](#73-pipelinebindingshaderの保証境界)に従って分ける。

標準パスはGraphと必要な入力・描画先・設定を受け取り、後続へ渡せる出力を返す。
描画先color/depthやClear/Load/Storeを合成側で制御でき、depth prepass後のopaqueが必ず再clearする等の制約を持ち込まない。
0 drawでも、Loadによる既存内容の保持や必要なclear等により出力の有効性を維持する。
多段処理の中間資源は部品内部へ隠してよく、公開部品とGraphの1ノードを一対一に対応させる必要はない。
透明物の並び順・blend・depth write、色空間、format、extent、sample数、深度表現、入力が有効な時点を契約に記す。
必要なdepth等を提供できない構成は診断可能なエラーか明示的な代替経路にし、不要な入力を常設しない。

標準グラフもGraphへ処理を追加し、意味の明確な出力を返す構築部品とする。
Graph内の出力handleを返すことと、Graph外へ保持するexportは区別する。
中間画像すべてのexportや、readback/submit/presentを各部品へ強制せず、必要な出力と消費方法を合成側で選ぶ。
標準利用向けの便利入口が最終出力を確定する場合も、部品の合成経路を同じ公開APIで利用可能にする。

部分交換は「露出・tone mapping前のscene-linear HDR」等の意味を持つ境界で行う。
一つの処理を複数ノードのmoduleへ置き換えられる一方、ForwardからDeferredへの変更等では、
公開標準パスと独自パスを使って利用側がGraphを構築する。
完成Graphのnode名・番号を検索して差し替える方式や、あらゆる構造変更を受け持つhook群を標準契約の前提にしない。
`unlit_forward::snapshot`の拡張点はopaque後の同寸RGBA8を扱う。7.4節の標準HDR挿入点はHDR構成側の契約である。

#### 定義の再利用と毎フレームの入力

標準グラフの定義・設定を再利用しても、描画するオブジェクト、mesh/material、行列等のframe入力は変更できる。
基本経路は入力の不変版をcaptureし、そのframeのGraphを構築・compileする方式とする。
構築closureはその場で消費し、完成planは必要な命令・定数と資源版を所有する既決の方式を維持する。
現行CompiledGraphのdraw列を後から差し替える機能や、永続templateへのlate bindingを今回の必須条件にしない。
計画cacheは計測と依存・失効契約を伴う別の最適化として判断する。

定義・設定、Deviceに対応するpipeline/容量の準備、frame入力、Viewごとの履歴は責務を分ける。
この区分だけを理由に新しい型を増やさない。frame固有のGraph handleを次frameの定義へ保存しない。
資源pinは入力接続・構築からcompile、記録・投入・cancel・実GPU完了の必要な所有へ引き継ぎ、
呼出側へ手動の寿命token管理を通常手順として要求しない。
履歴は7.4節のとおり初回/reset/resize等で失効し、submit受理前に次frame用の状態を進めない。
可変draw数・構成も[通常frameのメモリ規約](#steady-state-memory)の対象とし、
準備済み容量を再利用する。容量不足で暗黙grow、native作成、GPU waitへfallbackしない。

#### 公開方針の受入例

次の三経路をRenderer本体の変更なしで実証する。

1. 標準グラフをそのまま使い、毎frameの描画対象・材質・行列を変える。
2. 標準グラフの意味付き挿入点へ、利用側の多段処理を追加・交換・無効化する。
3. 標準グラフを使わず、標準opaque・独自半透明・標準出力処理を利用側Graphで組み合わせる。

初回の部品抽出では既存Rasterと独自Computeで構成経路を確認した。
`.2.8`は同じ公開APIへtrusted Raster資産とblend／depthを接続し、独自半透明を含む三経路を受け入れる。
標準トーンマッピングの追加はB3とし、それ以前の標準出力処理をtone mapping実装済みと扱わない。
0/1/多数draw、depthのLoad、複数View、不要部品のculling、出力所有、未投入cancel、
容量満杯、資源更新・pending保持を適切な段階で確認する。
CPU契約回帰と二APIの実GPU内容照合、製品/追加検証の別構成、最終解放後までのnative診断、
変動frameのalloc/realloc/deallocとnative作成/破棄の計測を維持する。

<a id="public-api-migration"></a>

### 7.7 公開API変更の着手時期と作業順序

Compute/copyの二API受入`.2.4`は完了した。公開APIの構成変更は、次の一般import・寿命管理`.2.5`に先行する。
単一出力・固定opaqueを前提に資源接続を固める手戻りを避け、B2/B3の描画機能を公開部品として追加できる境界を作る。
`.2.7`では入力接続・標準部品を公開し、代表consumerの移行と独自Computeを含む三経路の二API受入を実行した。
具体的な構成・結果と未確認範囲は[検証記録](../verification/current.md#公開三経路のconsumer移行と混合compute受入)を参照する。
課題の状態・進捗・依存の正本はBeadsとする。

| 順序・Beads | 作業と完了の境界 |
|---|---|
| `.2.7.1` 方針・計画 | 本節と現行契約・導入文書を整合させ、実装課題と依存、受入条件を登録する。 |
| `.2.7.2` 入力接続・出力所有 | `connect_graph`／`begin_graph`／`connect_snapshot`で固定描画を追加せず入力接続し、任意構成でも所有と生成時保証を引き継ぐ。出力は合成後に選び、詳細と実証範囲は[現行契約](rendergraph.md#利用側からの入力接続と出力選択)と[検証](../verification/current.md#利用側graphへの入力接続と出力選択)を参照する。 |
| `.2.7.3` 標準部品 | opaque／depth prepass／color出力copyとforward／post境界の初版を公開した。callerの描画先・Load/Storeと任意depthを扱い、初期版はunlit linear colorで、同geometry prepassの限定Programを含む。現在の配置は[三層の公開方針](#public-api-layers)に従う。 |
| `.2.7.4` consumer・基準受入 | 標準forward・利用側多段Computeのpost交換／追加／無効化・標準パス直接合成を二APIで照合し、frame_loop／engine_workloadsと文書を移行した。旧描画の同等性、任意HDR出力、容量・所有・混合frameメモリを確認する。 |
| `.2.5` 一般import・寿命 | `.2.7`の境界へcanonical identity/revision、更新・pending退役、履歴・失敗を接続する。既存受入を保持し、新課題へ重複実装しない。 |
| `.2.8` 利用側Raster | `.2.7`と`.2.5`の後、独自graphics shader/ABIと必要なblend/depth設定を公開する。最小alpha fixtureで三経路を二API実証し、固定Program群の公開だけで自由なRaster対応を主張しない。 |
| `.2.6` 表示・時間計測 | `.2.5`/`.2.8`の公開Graph・importを使う同じconsumerで、二API表示、自由camera／固定巡回、resize、pending終了を接続する。共通表示契約と任意の実windowed CPU／GPU／present計測は[表示設計](#d3d12-presentation)に従う。 |

`.2.7.1`〜`.2.7.4`の親は`renderer-7ft.2.7`、`.2.8`はB1直下とする。上表の省略IDには`renderer-7ft`を前置する。
新APIと旧APIの互換wrapperや並行経路を互換性だけのために残さず、実装範囲のconsumerと文書を一緒に更新する。
B2のPBR、B3の影・HDR・tone mappingは標準パスと標準グラフへ段階追加し、既存B2/B3の到達要件は変えない。

外部小HDR Bloom`.5.1`は`.2.7`の後に前倒しし、`.2.5`以降と並行して公開APIのUXを検証する。
親`.5`と子`.5.1`のB1全体依存を`.2.7`へ変更する。早期fixtureは一時画像と完了済み`.2.4`のCompute/copyを使い、
一般import・履歴・表示の完成を主張しない。Bistro統合`.5.2`は引き続き`.5.1`とB3共通出力`.4.3`を待つ。

<a id="renderer-core-plan"></a>

### 7.8 renderer と renderer-core の公開 API を検証して実装する計画

単体ゲームが 3D と HUD を一画面に描けること、既存エンジンが自分のシーン・UI・提出権を保てること、GPU 基盤だけでも使えることを同時に満たす。
製品クレートは `renderer` と `renderer-core` の二つ。以下に利用コードから選んだ境界と、その判断を再検証する条件を示す。
現在使える API は[組み込みガイド](../guides/tooling.md#scene-materials)、実行済みの検証は[現行の検証記録](../verification/current.md#engine-integration)を参照する。

ゲーム世界の所有と描画入力の境界は、Filament の平坦な描画シーンと Bevy の描画用データ抽出が参考になる。どちらの入力でも同じ描画方式へ到達できるかを試す。
2D と 3D の連携には Godot の Viewport と同様に、描画先を別の描画の入力にする経路を用いる。[Filament Scene](https://github.com/google/filament/blob/main/filament/include/filament/Scene.h)・[Bevy ExtractSchedule](https://docs.rs/bevy/latest/bevy/render/struct.ExtractSchedule.html)・[Godot Viewport](https://docs.godotengine.org/en/stable/tutorials/rendering/viewports.html)

#### 二つのクレートが公開するもの

| 利用者・入口 | 公開する操作と所有権 | 試作で確かめること |
|---|---|---|
| 単体アプリ → `renderer` | 初期化、描画先、共通の `Frame`。ウィンドウでは acquire から提示まで、画面外では画像の出力・取得までを扱う。 | アプリの直接依存は `renderer` 一つ。空のシーンまたは UI だけで開始できる。通常の一画面に GPU の枠数指定、二重提出、個別の完了待ちが出ない。 |
| ゲーム・ECS → `renderer` のシーンと資産 | メッシュ・材質・テクスチャの共有、非同期準備と版の差し替え、描画個体の登録・削除、変換・ポーズ・部位の上書き、変更列の一括反映、ライトと環境。 | 個体の上書きと共有資産の編集を呼び出し先から識別できる。ECS はゲーム状態の正本を持ち、最終姿勢を一度渡す。ファイル探索、装備選択、アニメーション評価はアプリが担う。 |
| カメラ・UI → `renderer` のビューと Canvas | シーン・カメラ・出力の組み合わせ、レイヤー、品質、カメラカット、2D の形状・画像・クリップを受け取る。出力テクスチャを同じフレームの別の入力にできる。 | 対象ビューごとに LOD・履歴を扱う。3D の解像度を下げても HUD は出力解像度を保つ。UI の入力、文字組み、レイアウトはホスト側で選べる。 |
| 独自方式・既存ホスト → `renderer` の描画部品 | ホスト入力または標準シーンから同じ方式で記録する。ホストの Graph に標準パス・独自パスを組み、色・深度など利用できる出力とその意味を返す。 | PBR・Toon の切替で個体を再登録しない。主カメラ外の影の投射物も方式が選べる。輪郭を足すだけならカメラ方式全体の実装は不要。 |
| GPU 処理・高度な統合 → `renderer-core`（Rust 名 `renderer_core`） | GPU 非依存の Graph、画像・バッファ、Program と binding、Raster・Compute・copy、資源入出力、compile。Runtime が準備・投入・完了・読み戻しを扱う。表示とネイティブデバイス・外部画像との同期は各機能の入口で提供する。 | `renderer-core` 単独で CPU 上の Graph 構築とヘッドレス Compute が成立する。GPU 基盤からシーン・材質・Canvas への逆依存がなく、同一デバイスのホスト資源を契約どおりに返却できる。 |

製品の依存方向は `renderer → renderer-core`。通常利用で core の型を組み立てる手順は作らず、独自パス・ホスト記録には同一の Graph・画像・Program 型を渡す。
旧 `renderer-pipeline` が持っていた PBR、Unlit、標準パス、レシピは上位の公開部品として継承する。投影・描画技法・Canvas の区分はまずモジュールとする。
独自パイプラインだけを使うホストと外部プラグインがこの形でビルド・実行できるかが、独立パイプラインクレートを廃する判断の検証になる。
別配布が必要になる具体的な依存や機能が判明したら、利用コードと依存グラフを記録して配置を再評価する。

#### 利用コードで比較する入口

次は製品 API の呼び出し側の抜粋。ウィンドウのイベント処理、ゲーム内の個体 ID、HUD のレイアウトを呼び出し側が持つ。
単体アプリは空で開始でき、登録後の最初の移動にも通常の更新を使う。HUD だけのフレームでは `draw_scene` を省く。

```rust,ignore
let mut renderer = WindowRenderer::new(window, backend, camera, assets)?;
// 描画イベント。提示待ちの再試行では draw が前回の画像を使う。
let outcome = renderer.draw(|frame| {
    let input = SceneInput::new(&meshes, &materials, scene.objects());
    frame.draw_scene(input, exposure)?;
    frame.draw_overlay(&hud_layers)?;
    Ok(())
})?;
```

ホストが描画世界と提出を所有する場合は、抽出した形状・姿勢・ライトをそのまま借りて記録する。標準シーンを使う経路と同じ PBR 部品を通す。
下例ではホストが Graph と最終提出を所有する。記録中の入力借用が終わっても、提出に必要な資源の版は保持する。訓練場はこの境界で輪郭と監視モニターを追加する。[利用コード](../../crates/training-yard/src/replay.rs)

```rust,ignore
let mut graph = host_pool.begin()?;
let target = graph.image("viewport", output_desc)?;
let (mut frame, runtime) = drawing.begin_host_frame(graph)?;
let hdr = frame.draw_scene(&camera, extracted)?.color;
frame.write_output(camera.id, hdr, exposure, Some(target))?;
let (mut graph, mut histories) = frame.finish_recording()?;
canvas::record(&mut graph, target, LoadOp::Load, &hud_layers)?;
let receipt = runtime.submit(&graph.compile()?)?;
histories.publish(&receipt)?;
```

core のみの利用者は、同じ Graph に Compute と読み戻しを宣言して `graph.compile()`、`runtime.submit(&compiled)`、完了通知、結果の消費と返却までを行う。
投入の受理票は予約した出力とフレームを識別する。ホストが `poll_frame` 相当の入口で完了を受け取るまで CPU 読み戻しは公開しない。
ホスト記録で生じたビュー履歴の仮更新は該当 Graph の提出受理時に確定し、取消時には破棄する。共有 Runtime の別の提出物も並行させ、他の完了・出力を奪わないことを試す。
Graph 内の画像はその Graph だけで有効とし、別フレームや別描画基盤への受け渡しには明示した永続資源または export を使う。
ホスト所有の外部画像については、同じ Graph に記録する例と別に、同一デバイス上で先行アクセス・所有者・返却後の同期を実機で確かめる。[Vulkan のホスト画像契約](vulkan.md#host-textures)

#### 採用を判断する試作

- **フレームの終了。** 共通の Frame に描画して最後に提示する例、画面外結果を取り出す例、ホストの Graph へ記録だけする例を比較する。標準経路で提出者の選択が毎回必要な案、記録だけの部品が勝手に提出する案は見直す。準備中、容量不足、一時的な混雑、表示先の再作成、デバイス喪失は型付きの結果で判別する。
- **シーンと描画入力。** 直接編集・ECS の変更列・ホスト所有入力で同じ画像を得る。共通化により全件コピー、二重登録、パス別のポーズ再送が必要になったら入力境界を変更する。単体と一括で同じ描画 ID を使い、削除・再利用後に古い ID や選択結果が新しい個体を指さないことを確かめる。
- **UI と 3D の合成。** HUD、UI 単独、端末に映る UI、UI 内の 3D プレビューを一つのフレーム API で試す。画面固定 HUD はシーンのブルームを受けず、端末は奥行きに従って隠れる。クリップ、アルファの扱い、合成時の色空間、出力変換の所有者を明示し、同じ変換の二重適用を画像で検出する。
- **資産・ビューの変更。** 旧版を表示しながらメッシュ・テクスチャ・材質の新版を準備する。新ビューの作成失敗や提出前の取消で、既存ビューの登録・履歴・資源が有効なことを確認する。投入受理、GPU 完了、表示完了、CPU 結果返却は別の時点として試験する。

比較では Cargo の直接・推移依存、呼び出し側が所有する ID と作業領域、再登録・待機・コピー・提出の箇所、失敗と再試行のコードを残す。
別名のラッパーへ移した手順も数える。試作で条件を満たせない案は、その反例と代案を本節と Beads に記録し、入口を修正して同じ利用コードで再評価する。

ホスト記録の試作は `crates/training-yard/tests/host_output.rs` にある。ホストが出力先を作り、描画部品は借りた Graph に色変換を記録し、ホストが読み戻しと提出を一度行う。サイズの異なる出力先を渡した場合はその Graph を破棄し、同じプールから再開できる。Vulkan・D3D12 で出力バイトを照合した。この結果から、ホスト記録の引数に Runtime を要求せず、提出権は呼び出し元が保持する形を採る。

`tests/core_compute.rs` は graphics 機能を外しても実行できる。二つの Compute を提出し、後の ID から完了を受け取っても、各入力に対応する結果を取得できた。Graph プールを一枠にした試作では Vulkan の二回目の記録が枠不足になった。両 API で二件を並行させるには、通常フレームの準備で同時実行数に対応する Graph 枠も確保する。アプリ側でバックエンドごとに枠数を調整する手順を標準入口に露出させない。

Canvas の試作は表示リニア空間で乗算済みアルファを合成する。アプリは描画領域、クリップ、借用した画像ビューを渡す。透明なパネルを別の描画先に重ねる場合も同じ操作を使う。半透明の赤をクリップして青い背景に再合成する実機試験では、両 API とも該当画素が RGBA (128, 0, 127, 255)、残りは元の青となった。これにより、呼び出し元でアルファの再乗算や中間画像の export を行わずに済む。world_panel 試験では、Canvas を貼った端末の手前に緑色の板を描き、そのシーンを UI の装備欄で 2 倍に拡大して 1 画素の HUD を重ねた。ホストが一度提出し、両 API で出力の全 40 画素が一致した。

ホスト画像は既存の D3D12 の `d3d12_host` と Vulkan の `host_device_survives_adopted_graphs_and_failed_adoption` で再実行した。そこで必要なデバイス採用と画像返却は core の外部画像境界に置く。同じ Graph の中間画像を使う Canvas には持ち込まない。独自 Toon の既存例では両 API とも二ビューで同じ中心色と被覆数を得た。独自 Toon ビューは呼び出し側が選んだ個体列を受け取る。PBR と Toon 材質の個体を同じビューで描く場合は、混在した個体列を標準の描画入力に渡せる。

標準 PBR の初回移動は初期姿勢を数える予約では両 API で枠不足になった。個体ごとに変換を準備すると、追加予約なしで赤い PBR 個体が移動し、緑の独自 Toon 材質と同じ画像に描けた。保持シーンは ID と密な個体配列を対応づける。ホスト抽出配列と同じスライスを描画部品に渡せるため、記録時の全件コピーが不要になる。`standard_motion` では両入口が 32 × 16 の全画素で一致した。画面外では提出と読み戻しを分け、出力バッファの寸法を間違えても正しいバッファで再試行できた。

ウィンドウ用の試作は空のシーンから両 API で連続 3 フレームを提示した。提示待ちなら受理済み画像を再試行する。アプリ側の通常操作はシーン入力と描画先を渡す形とし、実行枠の回収と提示待ちはウィンドウ用の実装が扱う。試作の `WindowView` と `CaptureView` は準備と記録が重複するため、製品では同じ Frame の描画操作を共有し、最後の提示・取得を描画先に応じて選ぶ。ホスト記録は提出権を保つ独立した操作にする。Bistro の直接パスと Toon のような方式の追加には共通の Graph 型が必要なので、上位層は core の該当型を再公開する。独立したパイプライン配布はこの試作群に必要なかった。

#### 訓練場の受入シナリオ

シーン入力の試作は、保持した個体を列挙する入口と、ホストが抽出済みの個体列を借りる入口を比較する。CPU 試験で 1,024 個体に全件・16 件・0 件の更新を適用し、両入口から同じ材質・姿勢の 504 個体を抽出した。古い ID が混じる変更列は適用前に失敗し、削除後の個体を指すこともない。GPU の標的試験では PBR と Unlit を交互に配置し、同じ 1,024 個体の保持シーンと ECS 配列から描いた各画素を照合する。PBR と独自 Toon を含む動的ロードは別の実機試験で扱う。[多数の個体](../../crates/training-yard/src/targets.rs)・[混在材質のロード](../../crates/training-yard/tests/standard_motion.rs)

単体ゲーム、ホストのビューポート、固定入力のヘッドレス実行で同じ資産と操作を共用する。手動操作と再生は同じ更新経路を通す。
姿勢・鎧スロット・LOD を画像で識別できる隊員モデル、影を落とすライト、ガラス、箱、監視モニターを使う。
ロードの成功・失敗・完了順は制御可能にし、偶然の遅延に依存しない。下記の件数・周期は再現用の試験値とする。

| 場面 | 合格の証拠 |
|---|---|
| 空で起動、箱の生成・初移動・削除 | 全体の再準備や動的個体数の申告なしで位置・影が変わり、削除後は映らない。 |
| 同一モデルの隊員二人 | 異なる骨格姿勢が本画面と影に反映される。一人の鎧の発光は局所、共有材質の変更は両方に効く。 |
| 監視映像と装備プレビュー | 監視カメラを 6 描画フレームごとに更新し、待機画像・最新画像・前回画像を照合する。異なる LOD、ビュー限定の非表示、プレビューの照明と作成失敗が他ビューに及ぼす影響を確かめる。 |
| 装備の非同期交換 | A、B を要求し B、A の順で完了させても、ゲームが最後に選んだ B が残る。失敗時は旧版を維持し、削除済みの隊員が完了通知で復活しない。 |
| HUD・端末・装備画面・独自輪郭 | ガラス、短命な弾道、選択輪郭を合成し、前後関係を確認する。3D だけ解像度を下げたときの HUD と、UI だけのフレームも照合する。 |
| ECS と混雑 | 標的 1,024 体で全件・16 体・0 件の変更を再生し、単体更新と照合する。容量超過と実行枠の占有を別々に起こし、ホストによる再準備・再試行・描画省略を確認する。 |
| ホスト統合・結果取得・復帰 | ホストの UI を後段で記録し、最終提出の所有者を追跡する。非同期ピッキング中の個体再利用、リサイズ、最小化・復帰、カメラカットを起こし、物体・部位 ID、対象ビューの履歴、影響を受けない出力を照合する。 |

#### 実装順と完了条件

| 順序・Beads | 成果物と次へ進む条件 |
|---|---|
| 1 · `renderer-p47.2` 利用コードの試作 | 現行 API を使う examples・外部 Toon・Bistro の直接パス・core 単独利用を比較元にする。上記の入口について最小の外部利用クレートを作り、依存と通常・失敗時の手順を比較する。代表となる画像・所有権は現行基盤で実行し、型だけの試作と区別する。採用する公開操作と棄却理由を記録してから改名する。 |
| 2 · `renderer-p47.3` 二クレートへ再配置 | 旧 `renderer` を `renderer-core`、旧 `renderer-pipeline` の標準部品を新 `renderer` に移す。manifest、feature 転送、lockfile、公開パス・マクロ、テストの相対参照、Bistro、外部プラグイン、シェーダー生成・CI・利用ガイドを同時に移す。CPU 単独・単一 backend・両 backend をビルドし、既存画像・実機経路を再現する。 |
| 3 · `renderer-p47.4` 最小の画面を実装 | 共通 Frame、空から始まるシーン、通常の個体更新、Canvas 合成を既存 Graph で動かす。ウィンドウ・画面外・ホスト記録で共通の描画部品を使い、四つの UI 合成例を両 backend で実行する。 |
| 4 · `renderer-p47.5` 動的な訓練場と拡張 | スキニング・部位材質・LOD、世代付き個体と差し替え、ビュー別の履歴・影・ピッキング、ECS 一括入力とホスト画像を順に実装し、上表の成功・失敗を再生する。独自 Toon・輪郭・Bistro の直接パスが上位シーンの所有を強制されないことを外部利用者から確認する。 |
| 5 · `renderer-p47.6` 実機受入と移行完了 | 訓練場と既存 Bistro の回帰、公開 examples、rustdoc、ガイド、Cargo 機能表、ツール手順を新構成で通す。旧クレート名と互換専用の二重経路を現役の利用手順から除き、再現方法・計測値・未実行範囲を[現行の検証記録](../verification/current.md)に反映する。 |

実機受入は Windows の Vulkan・D3D12 で、実ウィンドウとヘッドレス、開発検証ありと最適化した製品構成を分ける。
ID は完全一致、色・深度・影は事前に定めた領域・許容差と基準画像で判定し、実行環境、入力列、差分、失敗フレームを保存する。
厳格構成では破棄までの native 警告・エラーを検査する。製品構成では準備済み容量内で入力とパスを変え、Graph 構築から提出・表示・結果返却までの CPU 確保・再確保・解放、Renderer 発行の native 資源／メモリ作成・破棄を個別に測り、各 0 件を受入条件とする。
起動、ロード、リサイズ、明示的な容量拡張は別に記録する。容量不足時に暗黙の拡張・GPU 待機は入れない。GPU 実行の前提を欠く環境は未実行として残す。
詳細は[通常フレームのメモリ規約](#steady-state-memory)、[検証方針](../guides/validation-policy.md)、[開発・実機試験手順](../guides/development.md)に従う。

## 8. リソース所有権とGPU寿命

### 8.1 寿命を分ける

少なくとも次の寿命を混同しない。

| 寿命 | 意味 |
|---|---|
| CPUハンドルの寿命 | エンジンやグラフ構築コードが参照を保持する期間。 |
| 論理リソースの寿命 | グラフ上で必要になる使用区間。 |
| GPUオブジェクトの寿命 | 記録済み・実行中の命令などから参照され得る期間。 |
| 物理メモリの寿命 | バインドされたリソースやGPUアクセスに必要な期間。 |
| 履歴リソースの寿命 | 複数フレームにまたがって内容を維持する期間。 |

永続ハンドルには、型・世代・所有するRendererまたはDeviceの一致を検証できる仕組みを持たせる。古いハンドルや別Deviceのハンドルを、偶然同じ整数値だから有効と扱わない。

グラフローカルの一時ハンドルは、別のグラフや次のフレームで無条件に使用できない契約にする。GPUに必要な参照は、submit時点だけでなく、命令記録からsubmitまでの期間にも保持する。記録失敗・未submit・キャンセル時の解放も設計する。

### 8.2 GPU完了と遅延破棄

リソースの削除要求と、ネイティブオブジェクトの実際の破棄を分ける。最後のCPU参照が消えただけでGPU使用中のリソースを破棄しない。

初期の単一キューでは、FenceまたはTimeline Semaphore等による完了管理を、実装対象に合わせて選ぶ。将来の複数キューでは、そのリソースを使ったすべての未完了処理を覆う完了条件を持たせる。異なるキューの数値を単純に最大値で比較する設計にしない。

command pool、descriptor pool、upload領域、query pool、フレーム一時領域も同じ原則で再利用する。通常フレームごとの`device_wait_idle`やリソースごとの待機で問題を隠さない。初期化・終了・例外的な再作成で待機する場合は、その範囲と理由を明示する。

基本的な再利用は、それを使う実行経路の導入時に整える。B1では採用基盤のCommandPool・CommandBuffer・Fence等を完了後に再利用し、複数の未完了処理を保持できる形にする。上限と空き不足時の動作を明示し、無制限な確保や暗黙のGPU待機を通常経路にしない。descriptorやフレーム一時資源も、導入時からその寿命に合う保持・回収を設計する。

### 8.3 破棄順序と失敗経路

所有する子オブジェクト・Allocationを適切に解放してからAllocator、Device、Instanceを破棄する。`vk-mem::Allocator`の存続中は、その作成に使用したVulkanオブジェクトが有効である必要がある。[^allocator]

部分初期化の失敗、シェーダー作成失敗、メモリ不足、Device lossでも、二重解放や通常経路への誤復帰を起こさない。Device loss後にGPU完了を永久に待ち続けることを避け、終了または再初期化が必要な状態として伝える。自動復旧の完全実装は初期要件にしない。

## 9. vk-mem / VMAを使うメモリ管理

この章のVMA固有の実装選択と第10章のnative実行指針は、自前のVulkan/VMA基盤を直接管理する場合に適用する。
B0で外部基盤を選ぶ場合は、採用基盤が保証する範囲とRenderer側に残る責任を確認して更新する。
割当・転送・同期・寿命の必要な保証は維持するが、VMA Virtual allocator等の採用そのものは必須にしない。

### 9.1 初期は個別Allocationでよい

初期はbuffer/imageごとにAllocationを持たせる実装で開始する。この段階では物理エイリアシングを行わない。個別Allocationは、リソースごとに独立した`vkAllocateMemory`を行うことと同じではなく、VMAのサブアロケーションを利用できる。[^vkmem]

ただし、公開契約や永続的な型設計に「画像とメモリは必ず一対一所有」という前提を埋め込まない。将来、複数の画像が一つのAllocationを共有できるよう、画像の識別とメモリ所有の概念は分ける。

### 9.2 エイリアシングはグラフとバックエンドの責任

VMAは重なるメモリ領域へリソースをバインドする構成を支援するが、グラフの使用期間を自動解析しない。`CAN_ALIAS`等のフラグを設定するだけで最適化が成立したとは扱わない。[^vmaalias]

導入時には、メモリ要件・アラインメント・メモリタイプ・dedicated allocationの条件・画像作成条件を確認する。使用の重なり、切り替え時の依存、初期化の要否を検証する。詳細は採用バージョンのVMAとVulkan仕様に従う。[^vmaalias]

複数キューでは、単一のトポロジカル順序上の区間が離れていてもGPU実行が重なる可能性を考慮する。必要な完了順序を実行計画で証明できない組み合わせは共有しない。追加同期によるメモリ削減と並列性低下を、両方計測する。

共有Allocationの所有者を一つに定め、各画像の破棄時に共有メモリまで繰り返し解放しない。`destroy_image`は画像破棄とAllocation解放を組み合わせる補助関数であるため、共有構成では同じAllocationへ安易に複数回使わない。[^allocator]

### 9.3 CPUからの転送

アップロードとreadbackは、staging領域の再利用条件、GPU完了、アクセス範囲を追跡する。非coherentメモリでは適切なflush / invalidateを行う。mapできることとGPUとの同期が不要であることを混同しない。[^allocator]

高頻度の転送へ接続する前に、capacityを持つbufferのoffset・sizeで複数copyを扱い、完了後に領域を再利用できるようにする。
現在のVulkan staging配置は、ゼロ確保目標に向け準備済みmetadataのfree-range配列を使う。
VMA Virtual allocatorから変更した理由、代案、整列/結合と探索費用の扱いは[Vulkan設計](vulkan.md#実行資源と共有stagingの再利用)に記す。
GPU完了やCPUによるreadback利用終了の判定はRendererが担う。VMAの既定メモリpoolと、作成済みbuffer内の領域管理は別の層である。

VMAが所有するメモリを直接bind / mapする必要がある場合は、VMA側の同期契約と競合しないかを確認する。通常は対応するVMAのAPIを利用する。[^allocator]

内部断片化、確保済みブロック、利用中のAllocation、一時リソースのピークを可能な範囲で区別して記録する。「論理リソースの合計サイズ」だけをVRAM使用量として報告しない。

## 10. Vulkanバックエンドの初期方針

### 10.1 APIと機能の選定

新規のPC向け実装では、Vulkan 1.3、Dynamic Rendering、Synchronization2を初期推奨とする。ただし、これは最終製品の必須環境を確定するものではない。使用する機材と既存コードを確認して現在の設計文書へ反映する。

必要なバージョン・拡張・featureが不足する環境では、明確な診断を返す。古いAPIへの複数の互換経路を最初から大量に作らない。対応範囲を広げる場合は、要求と検証環境を用意して別タスクにする。featureは対応確認とDevice作成時の有効化を分けて扱う。[^features]

### 10.2 初期実行経路

描画とComputeを実行できるキューを選び、初期はそこへ処理を集約する。present対応キューが別の場合は、それに必要な同期・所有権移譲を実装するか、初期対応外として明示する。「描画キュー一つ」と「presentも必ず同じキュー」は同一条件ではない。

パス間のアクセスから、Vulkanのステージ、アクセスマスク、画像レイアウト、必要な所有権移譲を導く。read-after-readでメモリ依存が不要な場合でも、レイアウトや所有権の条件は別途確認する。同期の削減は基準経路の正しさを確立してから行う。[^sync]

複数キューは後続の最適化として導入する。キューが複数あっても実機で並行実行・高速化が保証されるわけではないため、単一キューの経路を残し、対象GPUで比較する。[^queues]

### 10.3 ホットパスの方針

通常フレームのメモリ管理は[全バックエンド共通のゼロ目標](#steady-state-memory)に従う。
所有権と実行順序を明確に保ち、毎drawの汎用ロックや不要な同期待ちも計測して整理する。

動的ディスパッチ、enumによる分岐、ジェネリクスの選択は実装の明瞭さと測定結果で決める。「薄いRHI」を理由に、未計測のコストを削るためだけの複雑な型システムを導入しない。

## 11. Shader・Pipeline・Bindingの契約

VulkanはSPIR-V、D3D12はDXILを使う。ソース言語と変換ツールはそれぞれの出力と共通のABIを満たすよう選ぶ。

アプリは配布ビルドで`vulkan`、`d3d12`または両方のCargo featureを選ぶ。起動時に指定した
`RuntimeConfig`に対応するバイナリからpipelineを準備する。両対応版は起動時にAPIを選べる。
単独版は実装と埋め込みシェーダーを一形式に絞る。標準シェーダーの埋め込みもfeatureに合わせる。
外部シェーダーの選択的な読み込みは[外部シェーダーアセットの取り込み](#external-shader-assets)を参照する。
両形式を埋め込んだ配布物では、ネイティブの準備に使う形式を選び、物理メモリ常駐はOSのページ管理に従う。

公開Programの`ShaderBinaries`は単一形式または両形式を表す。CPUのGraph構築ではメタデータだけを
使える。必要な形式を持たないProgramは、バックエンドの準備境界で拒否する。Rasterのfragment省略は
形式全体の有無と独立に示す。D3D12のインライン定数のregisterとspaceはDXIL側の記述に含める。
入力の信頼境界と実機適合は[検証方針](../guides/validation-policy.md)に従う。

| 決めるもの | 最低限の内容 |
|---|---|
| バインディングの対応 | 論理的なbindingの識別、リソース種別、配列数、stageからの可視性。 |
| CPU / GPUのデータ配置 | アラインメント、padding、構造体サイズ、行列の規約。Rustの既定の構造体配置を暗黙のGPU ABIにしない。 |
| 描画の座標規約 | handedness、clip/depth範囲、Y方向、front face、行列の扱い、必要な変換。 |
| 色と出力 | linear / sRGB / HDRの扱い、トーンマッピングと表示変換の責任範囲。 |
| Pipelineの互換性 | attachment形式、サンプル数、binding layout、シェーダーバリアント等の識別。 |
| キャッシュ | シェーダー内容、コンパイラー設定、binding規約、対象バックエンドを含む無効化条件。 |

最初は通常のdescriptor set等でよく、全面的なバインドレス化を前提にしない。リソース更新中のdescriptor・pipelineのGPU寿命も追跡する。hot reloadは後続でよいが、古いpipelineをGPU使用中に破棄しない契約を維持する。

pipeline、binding layout、sampler、ImageViewなどは、それぞれを導入する段階から同じ条件のものを保持・再利用する。毎draw・毎フレームでの再作成を基本動作にしない。キャッシュの容量や破棄方針、永続化の程度は実際の利用と測定結果に合わせる。

リフレクションや明示メタデータを用いて、パスの宣言、binding、shaderが要求する型・用途を照合する。任意の未信頼シェーダーを安全に実行するサンドボックスは初期目標ではない。受け入れるシェーダーの信頼境界と、ホスト側で保証できる範囲を文書化する。

<a id="external-shader-assets"></a>

### 11.1 外部シェーダーアセットの取り込み

実行ファイルを再ビルドせず、起動や明示的な構成変更でRasterとComputeのシェーダーを選べる。
アプリがロードしたバイナリとABIは`ManagedRasterProgram::from_trusted_owned_descriptor`と
`ManagedProgram::from_trusted_owned_descriptor`で取り込む。埋め込みアセットと同じ
GraphとWorkingSetを使う。`renderer`の利用側も、取り込んだProgramを公開Graph APIで使う。
[外部取込の利用例](../guides/tooling.md#runtime-shader-import)と
[Program契約](rendergraph.md#program由来bindingとshader-stageのowned計画)を参照する。

| 層 | 責務 |
|---|---|
| シェーダーのアセットビルド | HLSLなどのソースを配布対象に応じてSPIR-V、DXIL、または両方へコンパイルする。entry point、stage、bindingとアクセス契約などのメタデータを対応するバイナリと組にして配布する。 |
| アプリのアセット層 | 起動時にアプリが選んだバックエンドに対し、シェーダーの論理名を対応するアセットへ解決する。両形式を配布した場合も、選択したバックエンド用のバイナリと対応するメタデータを読む。 |
| アプリのレシピ | 取り込んだProgramを準備時に`WorkingSet`へ登録する。準備済みのProgramとフレーム入力を使い、公開Graph APIで資源とパスの依存を宣言する。 |
| `renderer-core` | 読み込み済みのバイト列とメタデータを受け取り、不変のProgramとして扱う。実Deviceとの適合を確認し、`WorkingSet`の準備でネイティブのレイアウトとパイプラインを作る。 |

取込時に、バイナリ形式・版・サイズ・ステージと、記述子の構造・宣言したABIの整合を確認する。
SPIR-Vではステージに対応する`main`を確認する。DXILではコンテナとSM6.0のステージを確認する。
DXILのentry、コードとABIの対応、実アクセス範囲、初期化、競合、シェーダー内同期は
[7.3節の信頼契約](#73-pipelinebindingshaderの保証境界)に従い、利用側がビルド・レビューで保証する。
実Device固有の上限と必要なバイナリ形式はWorkingSetの準備時に確認する。

各取込に固有の版を割り当てる。Programのクローンは同じバイト列・ABI・版を共有する。
Graphは構築時にABIから必要なアクセス・引数を導出し、命令には版IDと導出結果を記録する。
WorkingSetは準備済みの版だけを実行する。旧Graphの版が新WorkingSetに含まれなければ再構築が必要になる。
GPUが使うパイプラインと対応するProgramは既存の実行所有者が保持し、取消または実完了に従って回収する。
通常フレームに取込・所有データのクローン・最終解放の処理を追加せず、
ロードやWorkingSetの置換は[準備・構成変更の境界](#steady-state-memory)で行う。
描画を継続したままのホットリロードとアセットパックの格納形式は後続の検討とする。

#### Rustのレシピと構成の準備

レシピは公開Graphを借用し、パスを追加して後段で使うハンドルを返す。構成やフレーム入力に応じた分岐は
Rustで行う。producer、hazard、初期化、GPU同期と寿命には[RenderGraph](rendergraph.md)の契約を適用する。

`renderer_core::capacity::Preparation`は、起動・構成変更時に標準レシピと独自パスの要求をまとめる。
同一フレームで同時に宣言する数を合算し、同時フレーム数を考慮して一時画像とバッファの個数を予約する。
Graphの宣言上限と`WorkingSet`を一括で加える場合は、すべての上限・資源の検査が通った時点で要求を更新する。
同時に使う構成の容量は合算する。GPU処理や結果保持が重ならない構成は`Preparation::add_alternatives`で、
同じ同時フレーム数を前提に資源クラスとGraph上限の最大値、および使用されるProgramを集約する。
実行用`WorkingSet`には使うProgramの版とRasterのsample数を含める。各レシピが自分で宣言するアップロード量を計上し、
利用側から解析と追加のアップロードに必要な上限を渡して`GraphCapacity`を確定する。
通常フレームはこの容量を再利用し、上限を超えた場合は容量不足を返す。
手順とコード例は[Rustのパイプライン構築](../guides/tooling.md#rust-pipeline)を参照する。

材質のProgramはパスのHLSLテンプレートと材質関数を組み合わせてビルドできる。アプリが対応するABIを確認して
管理下Programへ取り込み、その版を使う。フレーム内は公開Graphでbindingを指定し、描画順に合わせてRaster batchへ渡す。
[テンプレートのビルド手順](../guides/tooling.md#material-shader-build)を参照する。

`renderer`のPBRレシピは、Forward／Forward+の1×／4×とDeferredの1×を提供する。
Forward系の4×はカラーを単一サンプルHDRへリゾルブする。シーン深度は1×でD32を返し、4×では`None`になる。
ゲームエンジンはシーン階層、カメラ、アセットとワールド変換を所有する。`ViewRenderer` は描画方式と寸法ごとのグループに `PipelineId` を割り当て、
各グループが登録されたカメラの状態を管理する。標準・独自の `ViewPipeline` は一つのGraphに共存でき、単一サンプルのシーンリニアHDRを共通出力とする。
メッシュ・材質の組合せを変更時に登録し、`bind_scene` が版を確認した入力を、そのフレームの同じグループのビュー間で共有する。
カメラを別の寸法・方式へ移すときは新グループを準備して旧履歴を解放する。登録済みアセットの寿命はカメラやグループから独立している。
ビューの履歴候補はGraphの投入受理後に公開し、受理前の中断では破棄する。受理後の公開失敗では受理票と未公開の候補を保持して再試行する。
通常フレームは準備したビュー数・作業容量の範囲で記録・投入・結果返却に追加確保を要しない設計とする。標準Deferredは照明に先立ってG-bufferを参照できる。
標準の局所光源上限は `SceneLimits::max_local_lights` でビューごとに準備する。ForwardとDeferredでは全光源を評価し、
Forward+では16×16画素のタイルごとに候補を絞る。空の光源リストではデータ転送とタイル判定を省く。
標準の3方式は `pipeline::GeometryPipeline` から選択する。`SceneLimits::max_transforms` を指定すると、
不透明・マスク・半透明と影の直接描画、または不透明の間接グループにモデル変換を共有できる。
変換の登録時に逆転置と向きの符号を求め、鏡映時の表裏と接線フレームへ反映する。
変換数は構成時に予約する。64件以内の小テーブルまたは1,024件単位のページを使い、GPUが参照するページまでを初期化する。
標準ビューはモデル行列のビット列が同一のプリミティブで変換を共有する。使用しないフレームは転送しない。
共通の`pbr`で材質・ビュー・描画入力・4カスケードの影を扱う。
アプリはカメラの姿勢・投影を決め、通常Zのビュー投影行列と視点情報を`ViewInput`で渡す。
レシピはその行列を描画、可視性、カスケード計算、Deferredの位置再構成で使う。
型付き材質のアルファ分類は係数のアップロード・更新と同じ境界で確定し、フレームでは描画パスとの適合だけを確認する。
同じ公開パスを個別に呼べるため、アプリは標準の`passes::bloom`と独自のHDR加工を合成できる。
HDRだけを使う構成は標準SDR出力の画像・Programを予約せず、必要な出力をアプリが組み立てる。
Bloomは同寸のシーンリニアRGBA16Fを返し、Compute Programと中間画像・Graph宣言の容量を準備時に登録する。
`renderer-bistro`の[Bistro](../guides/tooling.md#bistro-scene-pipeline)は標準レシピと比較用の描画構成を使う。
`pbr::ShadowHistory`はカスケードの行列とアプリが供給するキャスター入力の世代で再利用を判定する。
更新した画像の版とキーはsubmit受理後にまとめて反映する。次フレームは既存のGraphのimportとGPU寿命の契約に従って画像を参照する。
履歴が持つ画像の登録は、解像度の変更や使用終了時にアプリが明示的に解除する。
Vulkanのdescriptor予約はComputeのdispatch分を含める。`renderer-bistro`の`bistro-graph`は公開Graph APIを直接使う例である。

## 12. エンジン統合とPresentation

### 12.1 エンジンとのデータ境界

アプリはシーンとゲームアセットを所有し、`renderer` はサーフェスの定義・インスタンスと描画パスへの割り当てを所有する。
`renderer-core` は GPU 資源、プログラム、Graph の実行・完了を管理する。マテリアルには pipeline の `MaterialHandle` を使う。

材質更新では新しい版を発行する。取得済み CPU スナップショットは旧版を参照し、構築済み Graph はその時点の定数・プログラム・資源参照を保持する。
外部テクスチャや常駐係数の登録は所有者が管理する。提出前に登録を取り消した資源は runtime の鮮度検査に従い、受理済み処理の資源は GPU 完了まで保持する。
パラメータ名・型・バインディングは定義・編集境界で照合する。ビューごとの解決と整列には明示準備した領域を使う。
混在描画の入力と対応範囲は[マテリアルの使い方](../guides/tooling.md#scene-materials)を参照。

CPU assetの同一性とimmutableな版はバックエンドから独立させる。Vulkanと第二APIへ同じsnapshotを渡すために、
assetを二重登録する設計にしない。GPU常駐はCPU asset寿命と分け、件数・byte予算、退避、準備済みframeのpin、
pending参照の保持を契約にする。大きなuploadは固定容量のstaging内で進め、通常frameに暗黙の待機を加えない。

フレームの入力は、レンダリング処理に必要な期間だけ有効なsnapshot、draw list、更新コマンド等として渡す。GPU処理や遅延実行コールバックが、エンジンの一時的なECS借用や可変シーンへ依存し続けない設計にする。

通常のアプリ開発者へは高水準API、描画拡張の作者へはGraph API、特殊な外部連携へは明示的なinterop契約を提供する。すべての利用者へバックエンド内部の知識を要求しない。

### 12.2 Presentationの独立性

グラフの基本動作はウィンドウなしでも検証できるようにする。最終出力はオフスクリーン画像またはimportされた表示用画像とし、coreがイベントループを所有しない。

Surface・Swapchainの作成、acquire、present、リサイズ、最小化、終了処理はPresentationの責務にする。ただし、必要なDevice・キュー選択や同期情報はランタイムと共有し、互いに同じGPUオブジェクトを独立所有しない。

Vulkanのpresent待機Semaphoreは、単なる描画submitの完了と同じ条件で再利用できると仮定しない。現行はEXT swapchain maintenance1を要求し、各slotのpresent fenceと通常GPU参照の両方を回収する。拡張なしの画像ごとのSemaphore方式へ暗黙にfallbackしない。[^wsi]

Swapchain再作成・破棄時は、旧画像とpresent操作の完了条件も確認する。通常のGPU作業の完了トークンだけで表示側の寿命まで証明したと扱わない。対応拡張の利用や明示的な完了追跡を含め、採用する手順を検証する。[^wsirecreate]

frame slot indexとSwapchain image indexは別概念とする。ゼロサイズ、`OUT_OF_DATE`、`SUBOPTIMAL`、surface loss等を、再試行・再作成・停止のどれに分類するか明示する。

`presentation` featureはbackendを選ばない。共通の`PresentationConfig`／`PresentOutcome`と
`for_window → prepare → extent → present → poll → close`の意味を揃え、nativeの寿命差は各Presenter内部で扱う。
Vulkanは旧世代を上限内で保持し、DXGIは同じHWNDの一つのflip swapchainをready確認後にresizeする。
どちらも普通の枠不足で暗黙wait／増設をせず、利用側は同じcameraと公開Graphを使える。
共通の詳細は[表示のGraph接続](rendergraph.md#windows-presentation)に従う。

時間計測は明示opt-inとし、Graph全体と表示用fullscreenのGPU区間を分ける。
CPUのbuild／compile、native prepare／record／submit、poll／退役、結果返却、acquire／present APIと
frame間隔を分け、VSync OFFの処理時間とONのpacingを別runで記録する。
query／readback領域は有効化時の準備境界で確保し、通常frameにquery作成や結果取得待機を追加しない。
追加診断・profilingなしの通常経路でも容量・同期・native失敗と未完了所有は維持する。

## 13. Rustとunsafeの実装規則

ashを使っていてもGPUの同期や寿命は自動保証されない。Vulkanを呼ぶための`unsafe`を隠すだけで、安全な抽象化が成立したとは判断しない。[^ash]

`unsafe`はバックエンドや明確なinterop境界へ局所化し、各箇所に、その呼び出しが安全である理由と責任の所在をコメントする。単に「呼び出し元が正しく使う」とだけ書かず、寿命、範囲、同期、ハンドルの由来、スレッド条件を具体化する。

`Send` / `Sync`を手動実装する場合は、Vulkan・VMAのhost側同期要件まで確認する。特にCommandPoolやQueueの共有操作について、Rustの型が共有可能でも同時呼び出しが許可されるとは仮定しない。[^queues]

通常の入力エラーや未対応機能は型付きエラーとして返し、公開APIの通常経路で`panic!`・`unwrap()`へ依存しない。FFIコールバックからpanicを外部へ伝播させない。バックエンドのエラー原因、パス、リソース、操作を失わないエラー連鎖にする。

安全なAPIの保証が実行時検証に依存する場合、その検証をreleaseで外しても安全性が維持される根拠が必要である。診断用チェックと安全性を成立させるチェックを分ける。

製品版と開発用の分離には[検証方針](../guides/validation-policy.md)を適用する。
製品のdraw／submitでは保証済みの内容を再走査せず、保証をcook／取込・作成／更新・Graph構築等の境界へ移す。
privateな不変データ・所有権・失効管理で保証できる照合も省略できる。開発用の追加検証は専用情報の保持まで
製品構成から除外する一方、実行計画、同期、GPU完了・寿命、容量とnative失敗の処理は維持する。
これは今後の実装・変更を拘束する方針であり、現在の内部検証を除去済みとは扱わない。

## 14. 診断・テスト・性能計測

### 14.1 最初から出す診断情報

```text
パス名 / 定義位置
  → リソースID / View / アクセス範囲 / バージョン
  → 依存関係 / 除去理由 / 実行順序
  → 物理リソース / Allocation / キュー / submit
  → バリア / wait・signal / 完了条件
```

決定的に比較できるグラフダンプと計画ダンプを用意する。Vulkanオブジェクト名とパスのdebug labelを対応づける。`VK_EXT_debug_utils`はオブジェクト名、ラベル、検証メッセージの受信に利用できる。[^debug]

詳細dump・命名・label・profilingは検証と独立に選択し、製品に不要な構築・保持・文字列化を常設しない。
現行は`vulkan-diagnostics`有効時にdebug_utilsを要求し、同featureなし・native検証Disabledでは要求しない。
明示Requiredには対応する診断featureが必要であり、診断を省いて成功扱いにはしない。詳細は
[Vulkan設計](vulkan.md#検証と診断)と[検証方針の差分](../guides/validation-policy.md#current)を参照する。

将来の最適化には、エイリアシング無効、複数キュー無効、並列記録無効などの切り分けスイッチを併設する。ただし、意味を誤ったアクセス宣言まで自動修正する仕組みにしない。

### 14.2 テストを三つに分ける

| 種類 | 内容 | 実行環境 |
|---|---|---|
| CPU単体・性質テスト | グラフの依存、循環検出、不要パス、内容の初期化、範囲の重なり、importの同一性、計画の再現性。 | GPUなしで実行。 |
| GPU統合・画像テスト | 描画、Compute連携、copy/readback、複数フレーム、遅延破棄、リサイズ、異なるバックエンドでの同等出力。 | 実機の対応バックエンド。 |
| 性能テスト | グラフ構築・解析、記録・submit、パス別GPU時間、メモリ使用量、フレーム時間分布。 | 条件を固定した実機。 |

GPUテストではValidation Layerを基本とし、対応環境ではSynchronization Validationも利用する。検証を通ることだけを正しさの証明にしない。Validationのwarning・errorはともにゼロを要求する。例外は確認済みの誤検出だけとし、メッセージID・対象バージョン・発生条件・根拠を個別に記録し、ログにも残す。意図的な利用や性能上の助言は許容理由にせず、広範囲な抑制で隠さない。[^validation]

画像比較は単純シーンの厳密比較と、浮動小数点差を考慮した許容誤差を使い分ける。seed・カメラ・解像度・描画条件を固定し、変更した基準画像には変更理由を添える。

### 14.3 性能の扱い

CPUのgraph build / compile / record / submitと、GPU各パスを分けて測る。GPUのquery結果を即時取得するために毎フレーム待機して、通常実行の性質を変えない。

GPU名、ドライバー、OS、ビルド設定、解像度、機能設定、検証レイヤーの状態、測定回数、warm-upを保存する。検証有効の正しさテストと、最適化ビルドの性能テストを区別する。

性能比較には明示的な検証なし設定を用意し、releaseビルドと固定負荷で測定する。既定の正しさ試験は検証有効を維持する。機能の要求設定だけでなく外部設定の影響も確認し、ログを非表示にしただけで検証なしと扱わない。

最適化と検証は独立の設定軸とする。製品構成では開発用検証を外し、最適化済みの正しさ確認では検証を有効にできるようにする。
切替を実装・変更した場合は両経路を確認し、製品相当測定と計装された測定を区別する。
具体的な分類・feature確認・受入は[検証方針](../guides/validation-policy.md#builds)に従う。

FPSの平均だけで判断せず、フレーム時間の中央値・上位percentile、VRAM、一時メモリピーク、CPU割り当ても比較する。性能予算は代表シーンと対象機材を決めてから設定する。根拠のない固定数値や改善率を作らない。

他エンジンとの比較は補助資料とし、解像度、描画機能、アセット、シェーダー条件等の差を記録する。異なる条件のFPSだけで優劣を断定しない。

任意の`tracy` featureは、最適化済みBistroのhost処理を通常権限で区分するための明示zoneを加える。
既定offのoptional `tracy-client 0.19.0`と、lockされたsys 0.30.1の同梱Tracy 0.14.1を用い、
default featuresを外して`enable`・`ondemand`・`only-localhost`だけを選ぶ。
native headerと対応するportable collector/exporterの版を照合し、版やfeatureの自動更新へ依存しない。
sysの同梱BSD-3-Clauseはpackage/version限定で監査・許可し、通常の依存ライセンス許可を広げない。

Renderer内はfeatureで囲んだprivateなzoneと`Client::running()`のguardだけを使い、Tracyの型を公開APIへ出さない。
非計装構成から呼出し・専用metadata/文字列・依存をコンパイル除外する。
計装構成ではclientの起動・background thread、初回location準備やzone送信等の費用と確保があり、
on-demand切断中を無費用やゼロ確保とみなさない。通常・同じ計装EXEの切断中・接続収集中を同条件で別々に測る。
追加検証、native query profiling、メモリ計測とは独立に選び、同期・所有・native呼出し順序や失敗処理を変えない。

inclusive/selfは明示zoneのwall elapsedで、wait/deschedulingも含む。未計装の関数・driver内部のCPU実行箇所や
scheduler状態を得るには別のsampling/tracingが必要であり、Tracyの長いzoneだけから原因を断定しない。
今回の計装はhotspotの特定を目的とし、zoneを追加したことをコード最適化や速度改善と扱わない。
正常captureはDONE後にENDを渡して対象を通常終了させ、native clientの両queue排出とcollectorの自然終了を通す。
Tracy 0.14.1のcollector側Disconnectで未送信tailを捨てる経路を、完了の証明には使わない。
frame markやquery backlog表示だけで送信完了を推測せず、完全なnonce付きROI/frame zoneを独立に受け入れる。
ROI後の画像capture/teardownはtraceへ含まれても、定常費用の集計から外す。
手順と解釈は[Tracyガイド](../guides/tooling.md#tracy-profiling)、実際の対象・費用・所見は[現行検証](../verification/current.md)へ記録する。

<a id="bistro-roadmap"></a>

## 15. Bistroを到達目標にした実装計画と完了条件

親課題は`renderer-7ft`。旧M0〜M4の固定操作による実証順序をB0〜B5へ置き換える。
旧M4は準備・Device・buffer実行の6/22課題まで実証済みで、残りを実装完了とは扱わない。
実績は[現在の検証状況](../verification/current.md)に残し、旧課題は後継を示して置換する。
既存コード、Graphの内部表現、ash・vk-mem、crate構成の維持は必須にしない。

RenderGraphの目的は、描画機能を独立して追加・合成でき、フレーム全体の情報で資源・同期・実行を管理できることである。
DAGや固定操作が動くことと、拡張性の実証を区別する。
[元のFrameGraph講演](https://gdcvault.com/play/1024612/FrameGraph-Extensible-Rendering-Architecture-in)と
[RenderGraphガイド](../guides/rendergraph-best-practices.md#passes)を設計根拠にする。

### 到達対象と共通の受入

素材は[Amazon Lumberyard Bistroの公式配布](https://developer.nvidia.com/orca/amazon-lumberyard-bistro)を基準とする。
公式配布はFBX/Falcor形式で、取得・変換・Model/scene管理は利用側のsample/asset層へ置く。
版・hash・屋内/屋外の最終対象・変換toolと設定・camera/lights・解像度・材質機能はB0で固定する。
部分シーンは初期実証に使えるが、最終対象を黙って縮小したりmesh/materialを省略したりしない。
出典・licenseと再現可能な取得/変換手順を記録し、大容量素材を既定でGitへ追加しない。

| 観点 | 到達条件 |
|---|---|
| 描画 | 対象Bistroをテクスチャ付きPBR・影・線形HDR・トーンマッピングで描く。normal/tangent/UV、色空間、sampling/mip、alphaの解釈と対応範囲を記す。 |
| 拡張性 | 利用側の別module/crateが独自shader/pipelineの多段Bloomを追加・無効化・交換できる。Renderer本体やeffect列挙型の編集を必要としない。 |
| 二API | 同じ論理Graph・利用側パス定義・asset入力・描画条件をVulkan/D3D12実GPUへ渡し、Windowsでは両APIで表示・自由カメラ操作を行える。 |
| 正しさ | 画像・差分を保存し、浮動小数点やsamplingの差に対する比較条件を根拠付きで定める。bufferのbit一致を画像へ機械的に適用しない。 |
| 所有と検証 | 作成・更新・import/export・pending・終了・失敗を各段階で確認する。厳格native検証と診断なし最適化経路を分け、検証方針を満たす。 |
| 性能 | 初回読込と定常描画を分け、CPU build/compile/record/submit・GPU時間・メモリ・uploadを記録する。対象機材・画質と実測から予算を決め、未測定のFPSを約束しない。 |

path-traced参照画像との一致、レイトレーシング/GI、native HDR表示、skinning等を自動的な必須条件にはしない。
静的Bistroだけでは確認できない履歴や動的更新は、小さな補助sceneで検証する。
表示の初期対象は現行のWindowsとする。ユーザーとの要件合意により、最終受入は両APIの画面表示・操作と、
同条件のoffscreen画像比較の両方を要求する。表示経路の実装方式はB0の基盤選定で比較する。

<a id="bistro-requirements"></a>

### Bistroの確定要件と素材検証

`renderer-7ft.1.1`で合意した要件を次に固定する。下の描画方式と準備手順は出発案であり、
素材の実体検証は[検証結果と入力hash](../verification/bistro-assets.json)へ記録した。
描画方式・API基盤の採用決定や、Renderer実装・実GPU受入の完了は意味しない。

| 項目 | 確定した要件 |
|---|---|
| 最終対象 | Bistroの屋外全体。屋内全体は含めない。部分シーンでの着手後も屋外の対象mesh/materialを黙って省略しない。 |
| 描画品質 | テクスチャ付きPBR、太陽光＋空からの光（IBL）＋影、線形HDR・トーンマッピング、利用側独自Bloom。建物・地面間の光の跳ね返りを解く追加GIは必須にしない。IBL自体は間接照明を含む。 |
| 基準解像度 | 1920×1080。画像比較・性能測定の共通条件であり、実行時の解像度変更を妨げない。 |
| カメラ | マウス・キーボードによる自由移動に加え、比較用の固定視点と再現可能な自動巡回。カメラ・入力・シーン管理は利用側に置く。 |
| 表示 | 同じサンプルをVulkan/D3D12のどちらで起動しても、Windowsの画面で表示・操作できる。offscreen画像比較も行う。 |
| 初回受入機材 | 既存Windows x64／RTX 3060環境。既存記録は12 GiBであり、実行時にadapter・driver・能力を再記録する。他GPUでの実機受入を初回の必須条件にはせず、NVIDIA専用設計も要求しない。 |
| 性能予算 | 同じ素材・画質・機材の実測から定める。未測定のFPS、VRAM内への収容、他APIへの優位を約束しない。 |

実行中のAPI切替、二APIの同時二画面表示、カメラの衝突判定・歩行物理は今回の要件に含めない。
native HDR表示、レイトレーシング、追加GI、skinningも上の完成条件には追加しない。

#### 素材と変換の確認仕様

[NVIDIAの公式配布](https://developer.nvidia.com/orca/amazon-lumberyard-bistro)はFBX/Falcor形式、
CC-BY 4.0、屋外2,832,120三角形と記載する。これは配布案内の値であり、取得ファイルの検証値ではない。
引用情報の2017年7月を配布versionとみなさず、取得日時・実ファイル・size・SHA-256で配布物を識別する。
同梱licenseと出典を確認し、作者・配布元・licenseへのリンク、変換した旨を利用側のクレジットへ残す。

| 記録対象 | 取得・変換時に確定する内容 |
|---|---|
| 幾何 | source node path、mesh/instance、transform、三角形、material slot、position/normal/tangent/UV/colorの有無と対応。座標系・単位・面の向き・接線の符号も記録する。 |
| 材質 | base color、metal/roughまたはspec/gloss、normal、emissive、AO、opacity/cutoff、両面、UV set/transform。存在と意味を同梱scene・textureと照合する。 |
| 画像・sampling | 相対path、hash、寸法、channelと色空間、wrap/filter、mipと生成条件、欠落参照。色textureとnormal等のdata textureの変換を分ける。 |
| alpha | opaque/mask/blendの分類と根拠、shadowへの適用。必要な透過表現を不透明へ黙って置き換えず、alpha blendと物理的な透過・屈折を同一視しない。 |
| 変換の再現 | toolの正確なpatch/build・exporter版、script/設定/hash、入力・出力hash、sourceから出力へのnode/instance/material/texture対応。 |

検証用の中間形式にはBlender **4.5.16 LTS / build 3f6797307da5**のheadless実行と
[同梱Khronos exporter](https://github.com/KhronosGroup/glTF-Blender-IO)によるglTF 2.0 Separateを使った。
runtime依存の採用ではない。[BlenderのglTF資料](https://docs.blender.org/manual/en/4.5/addons/import_export/scene_gltf2.html)にある認識可能なmaterial nodeや
UV/normal境界での頂点分割を考慮し、FBXからの変換を無損失と仮定しない。
選択物だけのexportや材質の単純化で欠落を隠さず、対応表・形状・画像を照合する。
[glTF Validator](https://github.com/KhronosGroup/glTF-Validator)の形式・参照検査と、元シーンの完全性確認も区別する。
取込・作成・更新境界で内容を保証し、不変なasset本文を毎drawで再走査しない。

実配布`Bistro_v5_2.zip`は894,377,473 byteで、FBX独立解析は1,296 mesh Model・2,832,120三角形、
132材質・405画像を確認した。全meshにauthored normalとTextureUV/LightMapUVがあり、tangent/colorはない。
原本ZIP・検証済みglTF一式・環境HDRと比較PNGは[素材一覧](../../assets/bistro/README.md)のとおりGit LFSで共有し、
パス・byte数・SHA-256をmanifestへ固定する。作業用展開・Blender・host用中間データは`.tools/`に分ける。
標準のBlender処理が削除する2,894重複面には、逆向き980面、異なる法線やUVも含まれる。
検証profileでは頂点indexだけを分離し、全三角形・向き・材質・属性を保持して通常のMesh検査を通す。
FBXのraw CORNER normalを別属性に保存し、Blender内部の圧縮normalを経由せずglTFへ正規化出力する。
固定exporterの4桁丸めは7桁へ変更し、変更箇所・元/変更後functionのhashを記録する。
これらは既知FBX hashと固定Blender版に限定したoffline変換であり、一般入力やRendererの検証を無効化する方針ではない。

同梱READMEのPBR規約を使い、BaseColor RGB/alpha、ORMのR=AO/G=roughness/B=metal、発光RGBを明示接続する。
BC5 normalは正Zを復元してDX規約から緑成分を反転し、色とdataの扱いを分ける。
10発光材質のFBX色係数0.5を保持し、全ModelのCullingOffを両面材質へ対応させる。
alphaはopaque 120、mask 7、blend 5とした。maskのcutoff 0.5と分類は変換方針であり、元のartist intentの証明ではない。
ORM画像がない瓶ラベル1材質はnonmetal/roughness 0.5を明示した暫定値とし、元Phong値をPBR値と断定しない。

使用中の`MASTER_Glass_Dirty_MASKED`と`Paris_LiquorBottle_01_Labels`は画像alphaが全0で、計222面に影響する。
`renderer-7ft.1.1.2`では下記の根拠と近景比較から、**元alphaを保持し、不透明overrideを加えない**方針を確定した。
PNGはbase mipだけの中間形式であり、元DDS・mip情報は保存する。
B2の[実行用cook](#bistro-runtime-cook)では元DDSの全mipとMikkTSpace tangentを接続する。
TANGENTは元にも検証glTFにもなく、[glTF仕様](https://github.com/KhronosGroup/glTF/blob/main/specification/2.0/Specification.adoc)に従いconsumerで生成する。
この構成の公式Validator警告1,591件と、保持した未使用LightMapUVに関するinfo 1,591件は原文を残す。
native API受入の警告/エラーゼロを緩和する例外ではない。

<a id="bistro-alpha"></a>

#### 全ゼロalphaの2材質に対する確定方針

対象は上記SHA-256で固定したBistro v5.2屋外である。両材質のBaseColor alphaを不透明度として保持し、
color/depthとshadowの両方へ同じ不透明度を適用し、この2材質の面が被覆や影を作らないことをB2/B3の受入条件とする。
68面/13meshの`MASTER_Glass_Dirty_MASKED`と154面/11meshの`Paris_LiquorBottle_01_Labels`を
mesh/material対応表と資源から削除する決定ではない。原本・変換済みglTF・全画像のbyteは変更しない。
現行glTFのglassは`MASK`/cutoff 0.5、labelsは`BLEND`のままとする。
この分類は既存の明示変換recipeであり、後述のFalcorの分類と同一だとは扱わない。
固定入力ではどちらもalphaが常に0なので不透明度・色結果が整合する。
`BLEND`という分類だけでdepth writeやshadowの動作まで保証したとは扱わず、後続実装でopacity 0の除外を明示して確認する。

根拠を次の順で照合した。

- 配布同梱[README](../../assets/bistro/source/README.txt)はBaseColor alphaを不透明度と定義する。
  FBXの両材質には`Opacity=1`と`TransparencyFactor=1`があるが、texture接続もある。
  scalar値だけからtexture alphaを無視することはできない。屋外`.pyscene`にはFBX読込と環境画像の設定だけがあり、材質overrideはない。
- [CHANGELOG](../../assets/bistro/source/CHANGELOG.txt)のv5はFalcor 4.0向けと記載する。v5.2は屋外alphaの修正を記載しない。
  配布物にFalcorのexact commitは指定されていないため、4.0.1の固定ソースを参照実装として調べた。
  [DDS loader](https://github.com/NVIDIAGameWorks/Falcor/blob/c53908dd7930c031eb28b7ee49025eb27a833d64/Source/Falcor/Core/API/TextureLoader.cpp#L388)のDXT5からBC3への対応、
  alpha付きformat、[material](https://github.com/NVIDIAGameWorks/Falcor/blob/c53908dd7930c031eb28b7ee49025eb27a833d64/Source/Falcor/Scene/Material/Material.cpp)のmask選択と
  [shading](https://github.com/NVIDIAGameWorks/Falcor/blob/c53908dd7930c031eb28b7ee49025eb27a833d64/Source/Falcor/Scene/ShadingData.slang)のtexture alpha評価・cutoffによる除外を追跡した。
  texture alphaを不透明へ自動修復する経路はなく、両DDSは通常のalpha testで除外される。
  [texture sample関数](https://github.com/NVIDIAGameWorks/Falcor/blob/c53908dd7930c031eb28b7ee49025eb27a833d64/Source/Falcor/Scene/TextureSampler.slang#L80)は
  texture使用時にtexture値を返すため、scalar opacity 1でalpha 0は打ち消されない。4.0-previewと後続4.4でも整合を確認した。
  Falcor実機描画の照合や作者の意図の証明ではない。[検証データ](../verification/bistro-alpha.json)へ参照path・行・hashを保存する。
- 元DDSのDXT5/BC3 alphaを全mipで独立に復号した。glassは2048²から1²まで12段、labelsは16²から1²まで5段すべて0。
  既存PNGのalphaも一致する。labelsはRGBも全0であり、不透明化はラベル画を復元せず黒い面を追加する。
- [近景比較画像](../verification/bistro-alpha-preview.png)では全sceneの幾何を保持し、各対象材質だけを
  alpha保持／強制不透明へ切り替えて同じcamera/light/seedで比較する。対象面のcoverage診断と差分を併記し、
  画面内に対象があることを確認する。強制不透明は比較候補としてのみ使用し、配布glTFへ適用しない。

代替案の不透明化は元の不透明度と参照実装に反し、窓を不透明な汚れ面、瓶を黒いラベル面へ変えるため採用しない。
物理的なガラス透過・屈折や架空のラベル画像の追加も、この入力にない材質情報を作るため今回の基準へ追加しない。
将来、別の表示要件や修正版素材を採用する場合は、利用側asset層の明示overrideとして原値・理由・比較画像を別途固定する。

保証は取込/cook時の材質・texture照合で確立する。配布物、texture、alpha mode/cutoff、変換recipeを変更した場合に再検証し、
draw/submitへ全mip走査や専用診断payloadを追加しない。[検証方針](../guides/validation-policy.md)を適用する。
今回のCPU比較は材質方針の証拠であり、最終照明・Vulkan/D3D12の画像受入・性能測定ではない。
適用したVulkan指針は色/dataと入力境界の区別で、native実装変更や一般推奨からの新規例外はない。

#### レンダラーとベイカーの照明単位

シーンの長さを m、色空間を線形 Rec.709 とする。太陽の直達照度を lx、等方点光源の光度を cd、矩形片面の全光束を lm、環境と発光材質の輝度を cd/m² で記述する。色と強度の組は入力境界で Y を指定値に正規化する。CPU と GPU のベイクは同じ光源記述を使い、遮蔽を含む反射光を積分する。最終受光面への直接光はフレーム側が加算する。既に校正したベイク結果には環境画像の倍率を再適用しない。露出は出力段で選べる。距離減衰の正則化と範囲の扱いを含む利用例は[ライトの測光単位](../guides/tooling.md#photometric-units)を参照する。

#### 描画方式の出発案

| 項目 | 第一案と採用時の確認 |
|---|---|
| Raster/PBR | 通常のForward、metallic-roughnessのGGX specular＋diffuse、normal map。太陽光＋IBLから始める構成の小ささを理由とし、Deferred等への性能優位は主張しない。材質棚卸しに必要な変換・emissive・alpha・両面を反映する。 |
| IBL | 静的環境を前処理し、diffuse irradianceとroughness別specularを使う。環境の出典/license/hash・回転・強度・前処理toolを固定し、環境内の太陽とdirectional sunの重複を調整する。 |
| 影 | 太陽のCSM＋PCF。分割数・解像度・距離・bias・filter・境界処理と安定化は、素材と巡回経路で画質・費用を確認して固定する。 |
| HDRとBloom | 線形Rec.709のRGBA16F候補へ描画し、利用側の多段Bloomを合成して固定露出・tone mappingへ渡す。Bloomの追加・無効化・交換でRenderer本体を編集しない。 |
| 出力 | SDR sRGBを初期表示とし、tone mapperはKhronos PBR Neutralを候補にする。linear入力・出力とsRGB転送を分け、encodeを一度だけ行う。formatのattachment/storage/sampled/filter等の実用途は両APIで照会・確認する。 |
| AA | 履歴・jitterを必須にしないFXAAを候補にし、AAなしの診断画像も保存する。細線・鏡面の時間方向のちらつきを解決済みとは扱わず、巡回で評価する。 |

PBR/IBLの算法は[Filamentの解説](https://google.github.io/filament/main/filament.html)、
影の算法は[MicrosoftのCSM解説](https://learn.microsoft.com/en-us/windows/win32/dxtecharts/cascaded-shadow-maps)、
tone mappingは[Khronosの参照shader](https://github.com/KhronosGroup/ToneMapping/blob/main/PBR_Neutral/pbrNeutral.glsl)を根拠にする。
CSM資料のD3D10/11向け実装説明をD3D12のAPI契約へ流用しない。
これは画質・構成の出発案であり、vendor固有の最適化や未計測の高速化ではない。
採用基盤・公開APIは[基盤選定](#foundation-selection)に従い、Vulkan/VMA等の同期・寿命・能力契約と
[製品・開発の検証方針](../guides/validation-policy.md)を維持する。一般推奨からの新しい例外は設けない。

<a id="bistro-conditions"></a>

#### 固定した描画・比較入力

`renderer-7ft.1.1`の再現入力は[conditions.json](../../tools/bistro/conditions.json)にまとめる。
素材版・全mesh/material・alphaは上記の確定方針を引き継ぎ、屋外全体を対象とする。
固定視点は全景、路地、Vespaの材質近景、glass、瓶のlabel、巡回中間・折返しの7件とした。
全景は縦FOV 48度・JSONの仮想注視点を使い、全meshの保守的な境界を各辺5%以上の余白付きで画面内へ収める。
隠れた面まで一枚から見えるという保証ではない。
全景は全体の収容と欠落、路地は建物・植生と影境界、近景はnormal/roughnessとalphaを確認する。
同じ視点・入力・出力条件をVulkan/D3D12で使う。自由カメラ操作を固定画像の再現手段にしない。

| 固定項目 | 条件 |
|---|---|
| 座標とカメラ | glTF準拠の右手・Y-up・m。cameraは−Z前方、+Y上方。position/targetと縦FOVをJSONに保存し、near 0.02 m / far 500 m、1920×1080。論理画像は上原点、深度は通常Zの0〜1。各backendでclip-space/Y/front-face変換を行い同じrayを保つ。 |
| 巡回 | street → street-mid → street-end → street-mid → street、時刻0/10/15/20/30秒。position/target/縦FOVを線形補間し、world-up +Yからlook-atを求める。60 Hzの論理時刻でframe 0〜1799を生成し、重複する30秒終端は含めない。比較frameは0/300/600/900/1200/1500/1799。衝突判定や歩行物理の要件ではない。 |
| 太陽光 | surfaceから光源への単位方向`(√(3/8),0.5,√(3/8))`（仰角30度）、RGB照射値1/1/1、角直径0.53度。scene-linear benchmark用の値で、撮影時のluxや物理的な屋外照度の校正を主張しない。 |
| 環境 | 原本同梱`san_giuseppe_bridge_4k.hdr`、4096×2048、SHA-256 `5eb82f21bced3f7e901774083cfee1a05fd2bcf22f56ff699f3171c1eec65a50`。yaw 0度、強度3。原本byteは保持する。 |
| 高輝度源の抑制 | 非負の線形RGBに対し`L=dot(rgb,(0.2126,0.7152,0.0722))`、`rgb *= min(1,4/max(L,1e-20))`を元texelへ適用し、その後でresample/prefilter・強度を適用する。太陽を完全分離する算法ではなく、他のhighlightも変える明示的なbenchmark処理である。 |
| 環境の向き | 単位方向`d`へsource yawの逆回転を適用し、`u=fract(0.5+atan2(d.z,d.x)/(2π))`、上原点`v=acos(clamp(d.y,-1,1))/π`。U repeat / V clamp。+Xはu=.5、+Zは.75、−Zは.25、+Yはv=0。 |
| 色と露出 | scene-linear Rec.709/D65、固定露出EV +1（線形値×2）。自動露出・時刻変化・camera jitter・DOF・motion blurなし。HDR→利用側Bloom→露出→Khronos PBR Neutral→sRGB OETFを一度だけ適用し、最終SDRはRGB8/alpha 1。Bloom/AAを外した診断出力も保存する。 |
| 背景 | cameraに見える背景は一定の線形RGB .18（上記と同じ露出・表示変換）。IBLは照明・反射に使い、HDR内の別の建築をBistroのgeometryとして写さない。 |

cameraの座標と縦FOVは[glTF仕様](https://registry.khronos.org/glTF/specs/2.0/glTF-2.0.html#cameras)、
環境の向きは[固定Blender buildの投影処理](https://github.com/blender/blender/blob/3f6797307da5/intern/cycles/kernel/camera/projection.h#L19-L46)、
tone mapperは[commit固定のKhronos参照](https://github.com/KhronosGroup/ToneMapping/blob/180b1a7bddec33f73fe41712a2963cc3ad8e5547/PBR_Neutral/pbrNeutral.glsl)を根拠とする。
glTFからBlenderへの座標変換は`(x,-z,y)`。縦FOVはdegreeからradianへ変換し、
glTFの参考projectionを両native APIへ無変換で流用しない。Blenderの下原点pixel配列とHDR元画像の上原点も区別する。
near 0.02/far 500 mの通常Zは遠方のdepth精度にリスクがあり、Cyclesの構図確認ではnative depthの量子化を検証できない。
B1/B3で対象depth formatの重なった面・窓・影を実GPU確認し、不十分ならreversed-Zや視点別near等を比較して
条件を明示的に改訂する。改訂時は設定hashを更新し、両APIを同じ条件で再受入する。

原本pysceneは環境強度10のみを指定しており、この光・露出案は作者の意図の復元ではない。
元HDRの最大Rec.709輝度は約82,638で、輝度100超の213 texelが全周の立体角加重輝度積分の44.902%を占めた。
cap 4は68,105 texelに作用する。平均の立体角加重輝度は0.784567から0.416680へ変わり、
太陽の一点だけを除く処理ではないことも計測へ残した。
未処理IBLへ別太陽を重ねると高輝度源が重なるため、抑制した環境で空・周囲からの光を保ち、別太陽で影を制御する。
IBLのみの候補とも同じcamera/露出で比較する。光の物理的分離・エネルギー保存や、別HDRへの一般的な最適値は主張しない。
太陽RGB 3/IBL 1からの試行では路地が暗く、太陽RGB 3/IBL 3では外壁が白くなりすぎたため、
太陽RGB 1/IBL 3とEV +1を選んだ。強い太陽や露出だけの増加では暗部と明部の両方を確認しにくいことをCPU画像で確認した。
[設定のCPUプレビューと照明候補](../verification/bistro-conditions-preview.png)は構図・露出の判断材料であり、
CyclesのBSDF/環境積分/影をRendererのGGX/IBL/CSMと等価な正解画像にはしない。
32 sampleのCPU OpenImageDenoise表示画像で構図を確認し、別に保存したnoise除去前のEXRも記録する。
瓶の近景は室内fillを追加していないため暗く、色画像だけからalphaのdepth/shadow被覆を受け入れない。
名前付きROIと、alpha不透明化等の既知の誤りへの感度確認をJSONと下記の実GPU受入手順へ記録した。
B3ではこの処理済み環境からdiffuse irradiance・roughness別specular・BRDF積分を生成し、
前処理tool・全設定・出力hashを固定して受け入れる。

上記のForward/CSM/PCF/RGBA16F/FXAAを初期実装案として採用したが、実GPU能力と画質確認を代替しない。
JSONにCSMの4分割/2048/120 m/λ=.6/3×3 PCF/texel安定化/末尾10% blendという出発profileを明記した。
depth/slope/normal bias、FXAA、Bloomの詳細値は採用実装に依存するためB3/B4で校正し、受入前に固定する。
太陽の角直径を記録したことを、CSM+PCFが物理的な面光源影を再現する保証へ読み替えない。

#### 再現・比較と完了の境界

asset/IBL/shader/設定のhash、座標・単位・projection、camera pose/FOV/near/far、巡回keyframeと固定時刻、
sun/IBL、露出、影・Bloom・AA、解像度を一組で保存する。全景・路地・材質近景・alpha・影境界を確認できる
視点を取得後に選び、自動露出や時刻変化・乱数jitterで比較条件を変えない。
最終SDRだけでなくBloom前HDRと必要な中間出力・差分を保存し、単純fixtureの厳密比較とBistroの許容誤差比較を分ける。
同API反復と二API差、既知の材質欠落・色変換ミス・影ずれを検出できることから、受入runより前に
絶対/相対誤差・誤差画素数/領域の閾値と根拠を固定する。輪郭等の一括除外で不具合を隠さない。

両APIの表示・自由操作・resize・最小化復帰・pending中の終了を実GPUで受け入れる。
最適化＋Renderer追加検証あり＋厳格native検証の経路と、Renderer追加検証・専用診断をコンパイル除外し
native検証を無効にした最適化通常経路で内容を別に照合し、
正常終了・native所有者破棄後までの警告/エラーゼロを既存方針どおり要求する。
初回読込/変換・upload・shader/pipeline準備と定常描画、CPU各phase・GPU・frame全体・メモリを分け、
warm-up・複数runのp50/p95・揺れ・VSyncと表示待ちを記録する。

準備の記録手順は[開発ガイド](../guides/development.md#bistro-preparation)に置く。
素材の版/hash・全材質/画像の棚卸しと、glTF/全参照fileのbyte一致による変換再現性は確認した。
全ゼロalphaと最終camera/path/light/IBL/露出を上記のとおり固定した。
`renderer-7ft.1.1`は描画要求と再現・比較手順の固定を完了条件とし、Renderer実装・実GPU受入とは区別する。
画像閾値の校正と性能予算は各段階の実GPU結果で具体化し、最新の実行/未実行は
[検証状況](../verification/current.md)へ記録する。

### B0: Bistro基準・公開APIのUX・採用基盤の設計

`renderer-7ft.1`。次の具体的な課題を順に実施する。

| 課題 | 成果 |
|---|---|
| `renderer-7ft.1.1` | 素材・描画要求を棚卸しし、最終対象・品質・再現条件・比較方法を固定する。 |
| `renderer-7ft.1.2` | 第7章の代表利用コードを複数案で書き、UX、所有、資源アクセス、pipeline/binding/ABI、errorと拡張時の変更箇所を比較する。 |
| `renderer-7ft.1.3` | 既存継続/改修・自前置換・外部基盤利用を一次資料と小試作で比較し、両APIのWindows表示も含む総工数・品質・保守負担から採用案と移行範囲を決める。 |

**完了条件:** 利用コードと要求を根拠にAPI・採用基盤の第一案を説明できる。
外部基盤へ委譲する範囲とRendererが保証する範囲も明示する。
設計検証は広く行い、全将来機能の即実装や公開APIの凍結は要求しない。
候補を網羅する調査で長期化させず、共通の小さな描画・Compute例と比較観点を揃えて採用可能な第一案を選ぶ。
採用後にB1以降をbackend/契約/描画成果ごとの検証可能な縦切り課題へ分割する。

要求・利用コード・[基盤と移行範囲](#foundation-selection)の設計を完了した。
後続はBeadsのB1 `.2.1`〜`.2.6`（作成保証、owned Graph、Raster、Compute/copy、寿命、表示）、
B2 `.3.1`〜`.3.3`、B3 `.4.1`〜`.4.3`、B4 `.5.1`〜`.5.2`、B5 `.6.1`〜`.6.3`へ分解した。
課題の状態・依存と詳細な受入条件はBeadsを正とする。B0完了をBistro実装・実GPU受入の完了とは扱わない。

### B1: 公開パスと資源・shader契約の二API実証

`renderer-7ft.2`。B0の採用案で、独自shader/pipeline/binding、複数drawのRaster、
depth-only/color-only、Compute/copy、import/exportとGPU寿命を段階実装する。
資源format・範囲、shader信頼境界・ABI、宣言と実命令の整合、能力・errorを実証する。

`renderer-7ft.2.1.1`のuniform作成から`.2.1.2`でUniform/Storageの共通buffer/layout/bindingへ
進める。同じ管理下compute shaderを再利用し、storage出力も公開作成APIで用意する。
native layout/root slotはmanifestから導出し、旧Uniform専用APIを置換する。
`.2.1.4`はRGBA8画像/View/nearest-clamp samplerとsampled bindingへ進める。
`.2.1.5`はRGBA16F画像とHDR値を保持するsampled readbackを同じ二APIへ加える。
`.2.1.6`はD32画像とdepth attachment/sample Viewの作成契約、clear後のdepth sampleを加える。
`.2.1.7`はreadonly StorageRead bindingと管理下shaderの16-byte copyを二APIへ加える。
一般owned Graphの計画は`.2.2`、graphicsのstage/vertex/attachment/PSOと二API Raster実行は`.2.3`で接続する。
`.2.2.2`では管理下buffer Computeのowned命令・論理bindingとshader実footprintを共通CPU計画へ接続した。
この`.2.2.2`のCPU契約受入ではuniform/storage readbackの一workgroupだけを扱い、binding全descriptorと16-byte実アクセスを分けた。
この段階のCPU計画と、`.2.4`で接続するDevice固有の保証・二APIのnative実行は区別する。
prepared arena・所有・初期化・依存・cullingとnative未対応拒否の範囲は[Graph契約](rendergraph.md#program由来bindingとshader-stageのowned計画)に従う。
`.2.2.3`は描画命令なしのattachment配列/optional depthと単一mip/layer ViewをCPU計画へ追加し、
Load/Store、各subresourceの初期化・producer/hazard/culling、全域exportを実証する。
このattachment CPU契約の受入点では旧clear入口を置換し、native loweringは1color＋depth Clear/Storeに限定していた。
owned一般Raster命令・graphics stage/vertex/PSOと実描画、複数subresource View/native実行をこの完了に含めない。
`.2.2.4`は実在mesh assetの`ManagedRasterProgram`を明示するowned複数drawへ公開Raster/Engine batchを統合する。
Program・定数・draw順・attachmentを同じ準備済み計画へ所有し、既存VulkanでLoad/Storeとindex provenanceを維持して実行する。
このmeshはdescriptorを使わない。`.2.2.5`は別の実shader `textured_mesh()`でVS UniformとFS画像/samplerを導き、各drawのbindingをowned計画へ接続する。Computeのsampled assetも同じ型付きbindingを使い、ShaderStagesと実footprintを共通解析へ保持する。利用側の二段sampled RasterとCompute readbackで短命入力・画像依存・cullingを実証する。BloomアルゴリズムはB4、一般native Compute/copyは`.2.4`で別に実装・受け入れる。

`.2.3`は同じowned Rasterから複数draw順、color-only、depth-only、RGBA16Fとdraw別Uniform/画像/samplerを
二APIへlowerする。Programのレビュー済みshader/ABIとattachment形式をpipeline keyへ含め、
選択したProgram、物理画像、descriptorの容量を明示準備境界で確保する。通常frameでは作成・増設・内部waitを行わない。
HDRは既存float shader出力の格納形式であり、露出・tone mappingやBistroの影照明を実装した意味ではない。
GPU前のindex内容保証はprivateな`graph::provenance`へ共有し、変わらないmesh本文の再走査を避ける。
この標準mesh Programの実行範囲は通常Z/LESS、u32 triangle index、stride28、inline80 byte、単一mip/layer/sample、最大一色とoptional D32である。
`.2.7.3`の標準prepass継続用には、同じshader/ABIの限定ProgramでLESS_OR_EQUAL・depth writeなしを追加する。
これは一般graphics state APIではなく、depth必須・Load必須と明示準備の契約を持つ。
任意/MRT Program、first_index/base_vertex/多instance、複数subresourceを覆うViewをこの完了の保証にしない。
他mip/layer/filter、比較samplerを含む将来機能を、`.2.1`の追加完了条件にはしない。
保証境界は[管理下bufferの契約](vulkan.md#managed-uniform)、
実行/未実行は[検証状況](../verification/current.md#managed-uniform)に記録する。

`.2.4`は利用側のtrusted静的Compute descriptorとtyped storage画像をowned dispatchへ接続する。
固定dispatchと出力extent由来の2D dispatch、bufferの実byte footprint、全域画像writeの保証を分け、
Graph構築で範囲・ABI・重複subresource・dispatch/extentを保証する。shaderとbinaryの安全性は明示したunsafe取込でレビューする。
両APIはProgramとdescriptor容量を事前準備し、通常frameでは作成・増設しない。
同じ小HDR入力の複数Compute、画像/buffer copy/readbackと変動frameメモリを別processのstrict/shippingで受け入れる。
`.2.4`の受入後は[公開API移行](#public-api-migration)の`.2.7`で、入力接続と公開標準パス・標準グラフを先行する。
常駐資源importと履歴は新しい境界を使う`.2.5`へ、利用側Raster shaderとblend/depth設定は`.2.8`へ分ける。
`.2.6`は同じ公開Graph・登録資源を使う二APIのWindows表示、自由camera／固定巡回と通常frame時間計測を接続する。
共通outcomeとnonblocking backpressureを保ち、resize／最小化復帰／取得後cancel／pending終了を扱う。
接続と計測の契約は[共通Graph](rendergraph.md#windows-presentation)と[D3D12表示](#d3d12-presentation)へまとめる。

**完了条件:** 効果専用操作をRenderer本体へ追加せず、同じ利用側パス定義と入力を二APIで実行できる。
標準描画も独自処理も同じGraph経路を使い、未投入・未完了・更新・破棄・失敗・回収を区別する。
以降の実シーンで分かった不備は設計へ戻して改訂する。固定操作の完全移植を先行条件にしない。

### B2: 利用側asset経路とテクスチャ付きBistro PBR

`renderer-7ft.3`。利用側で素材を変換・読込し、Rendererの資源・material・描画入力へ接続する。
必要な頂点属性、texture/sampler/mip、材質とalpha、GPU常駐と更新を縦切りにし、部分実証から最終対象へ広げる。

**完了条件:** geometry/material一覧と変換結果が対応し、対象Bistroをテクスチャ付きPBRで描ける。
ModelをRendererへ持ち込まず、取込/作成/更新境界の保証を保持し、毎drawでasset本文を再検査しない。
同じ入力と公開Graphを二APIへ接続する。

<a id="bistro-runtime-cook"></a>

#### 実行用cookとForward PBRの境界

実行用cookと`renderer-bistro`の`bistro`はRenderer外の利用側処理とする。
全1,296 mesh・132材質・405画像をmanifestへ対応付け、glTFのnode transformをgeometryへ反映する。
頂点はposition、normal、tangentとhandedness、UVを持つ48-byte stream、indexはlittle-endian u32とする。
normalを正規化してMikkTSpaceを実行し、退化UV/geometry、またはfloat32の角度重みの
collapseで有効な接線が得られないcornerはnormalと直交する決定的なfallbackへ置き換える。
適用件数を原因別に記録し、Mikkが返した有効なhandednessは保持する。
world transformの反射・非一様scaleでもnormal、接線の直交性・単位長とhandednessを保持する。
元のnode/mesh/primitive/materialとtransformの対応を残し、頂点分割や順序の変化で欠落を隠さない。

材質の圧縮設定、DDS ブロックの保持、glTF 画像の mip 生成とエンコードは consumer の
オフライン cook が所有する。Renderer は選択済みの形式・寸法・全 mip を受け取り、
形式に応じたブロック行で転送する。Bistro の既定値は元 DDS の BC1/BC3/BC5 と全 3,529 mip を保持する。
Sponza は base color・emissive に BC7 sRGB、ORM に BC7 UNORM、法線に BC5 UNORM を使う。
標準材質の flags.z に XYZ、XY、DirectX XY の区別を記録し、BC5 はサンプル後に正 Z を復元する。
元 DDS の BC5 だけは Y を反転する。alpha は線形値で、圧縮後の全 mip の範囲から完全透明材質を識別する。
CPU ベイクは最長辺 128 以下の復号済み RGBA8 mip のみを利用する。未指定の旧形式は RGBA8 として読める。
GPU ペイロードは画像の論理バイト数で計上し、native 確保量と CPU データ量は別に記録する。
全量を一度にstagingへ複製せず、ロード境界で画像ごとにuploadし、定常描画用の資源を保持する。

cookのhashは同じ入力・生成物を識別するために使い、意味的な正しさの証明とはしない。
Rustの読込境界で形式version、byte長、有限値、index範囲、材質・画像参照、mip範囲を確定する。
読込後はprivateな不変データとnative/import ownerで保証を引き継ぎ、asset本文をdrawごとに再検査しない。
素材・cook recipe・stream・材質や画像を更新した場合は、該当する保証とprepared working setを再確立する。
Device能力、同期、容量不足、native失敗、GPU完了の処理はこの保証とは別に維持する。
GPUへのgeometry取込は`geometry::GeometryData`のtyped full-stream uploadを使い、
stride/長さ/index範囲の生成時保証をprivateな最大index要約へ引き継ぐ。
頂点fieldの意味・有限値はcook/loaderの責任であり、汎用GeometryDataだけでPBRの属性を保証しない。
通常の任意byte uploadへこの保証を与えず、内容のwrite/copyで該当provenanceを失効させる。
取込用GraphPoolとCPU streamはGPU完了後の準備・保守境界で返し、検査用CPU index本文を定常描画へ保持しない。

材質shaderは利用側のtrusted `ManagedRasterProgram`で、GGX metallic-roughness、normal map、
AO、emissive、両面と元のOPAQUE/MASK/BLENDを扱う。元alpha0の2材質を保持し、
alpha0をcolor/depth write前に除外する。不透明overrideは受入の感度確認だけに使う。
不透明・maskの後にblendをcameraからの距離で安定に並べ、線形RGBA16Fへ合成する。
B2の照明はsunと明示した一定ambientであり、B3のIBL・影・固定EV/PBR Neutralを完了した意味ではない。
SDR確認用のclampとsRGB OETFは利用側の別passで一度だけ行う。
標準forwardのunlit recipeへPBR固有のeffect列挙やasset読込を追加しない。

Bistroと標準PBRで使うshaderのHLSLは`crates/renderer/shaders/`を正本とし、
固定版DXCの同じ入力からSPIR-V 1.3とDXILを生成する。
同じソースで二APIの材質処理・Compute・頂点変換を保守し、手書きのregisterとspaceをRust descriptorの
bindingとgroupに対応させる。API固有の宣言とclip出力Yの符号はHLSLの`__spirv__`分岐に置く。
影cascade番号とinstance配置はそれぞれ4／68 byteで、Vulkanのpush constantとD3D12の`b7, space0`のroot constantsに対応する。
既存の生成スクリプトでソース・include・バリアント・出力を固定し、SPIRV-Crossはreflectionに使う。
Vulkanのbuffer配置はDXCの既定規則を使い、追加Device featureを前提にしない。
標準PBRと影の取り込みに限り、`renderer`の非公開モジュールが固定のバイナリとdescriptorの契約を保証する。
そのモジュールで既存のtrusted Program入口を使い、公開レシピはGraphの安全な型付き資源とbindingを受け取る。
生成compilerの変更は描画の丸めにも影響するため、ABI、両APIの画像、同じAPIの移行前後を
[既存の比較閾値](../guides/development.md#bistro-pbr)で再確認する。

clip座標の計算は、world-space positionと同じFP32のview-projectionから、各列の積を
`((x + y) + z) + translation`の順に加える。固定1080pのlabel近景では、compilerごとの
matrix積の再結合・縮約によりdepth差が最大約`2.98e-6`となり、既定の`2e-6`閾値を超えた。
native/ABIの切り分け後、この演算順序をvertexの`precise`とSPIR-V `NoContraction`、
DXILの非fastな積・加算へ明示し、生成物でも維持を確認する方針とした。
camera/cook・固定条件・頂点/UBO ABIは変えず、fragmentの材質計算は最適化を維持する。
比較閾値の緩和、全shaderの最適化停止、camera-relativeやFP64への入力変更は採用せず、
再現性の問題をclip演算の範囲で扱う。vertexのFMA機会を減らす可能性があり、速度改善の主張ではない。
他GPU/driver/compilerのbit一致を保証するものでもなく、toolchain変更時は生成物と実機を再受入する。

alpha0の無被覆は、同じAPI/GPUで別に描くPBR drawなしのSceneのclear/resolveを対照にし、
HDR・SDR・depthの全byte一致で確認する。RGBA16F clearの基準をCPUの最近接丸めへ固定しない。
[Vulkanのformat conversion規則](https://docs.vulkan.org/spec/latest/chapters/fundamentals.html#fundamentals-fpconvert)は、
表現可能な二値の間の有限値について、どちらへ丸めるかを一意に定めないためである。
対照自体もclearの隣接有限値とresolve結果・depthを検査し、任意の出力を正解にしない。
これはalpha無被覆の独立対照を正す変更であり、通常のfixture/scene比較profileと閾値は変更しない。

<a id="bistro-residency"></a>

### B2の資源差し替えとメモリ観測

`renderer-7ft.3.3`では、全Bistroを保持した利用側の`bistro --resource-checks`を使う。
通常frameの計測窓とは別に、異なるcamera Viewの同時投入、旧描画の所有が残る間の画像・材質の差し替え、
古いsnapshotの拒否、準備済み容量の不足と回復、元の内容への復元を検査する。
画像の全mipをuploadし、選択したmipごとのsamplingも照合する。
差し替え用の予備枠と診断画像は明示した準備予算へ含め、通常実行へ暗黙の増設や待機を加えない。
登録解除、GPU使用終了、CPUの結果・snapshot返却、物理資源の再利用は別の境界として扱う。

論理byte、native確保要件、プロセス使用量・予算を区別する。
VulkanはVMAのAllocation/block統計と、対応時だけ直接照会する`VK_EXT_memory_budget`のheap別推定値を記録する。
VMAの既定poolとsuballocationを維持し、観測のためのVMA budget flag・毎frame更新を導入しない。
D3D12は`memory-profiling`が有効なときだけ、成功した`CreateCommittedResource`の実descriptionについて
`GetResourceAllocationInfo`の件数・必要byte・alignmentを保持し、最後のnative ownerが返ると差し引く。
DEFAULT/UPLOAD/READBACKを分け、Graph外のmanaged資源や退役中の所有もDevice単位の集計へ含める。
この専用metadata・native要件照会はfeature無効時に除外する。
DXGIの同じadapterに対するlocal/non-local segmentの使用量・予算・reservationは、別のon-demand照会とする。
D3D12のtransfer arenaは準備済み容量を示し、現在使用byte・high-waterの未観測値をゼロで埋めない。

`GetResourceAllocationInfo`はdriverの内部管理費まで含む実使用量ではなく、資源のnative確保要件である。
OSのusage/budgetも動的な会計値・推定値であり、全pageの物理VRAM常駐や次の確保成功を証明しない。
診断中の確保量が足りるか、更新を繰り返しても所有・native必要量が増え続けないかを実測で判断する。
native OOMやDevice lossの注入試験と、準備容量を使い切る通常のbackpressure試験は区別する。
追加allocator、圧縮形式への変更、suballocationの導入は、対象GPUで必要性が示されたときに別途判断する。
[再現手順](../guides/development.md#bistro-residency)と[最新の実測](../verification/current.md#bistro-residency)を参照する。

根拠: [Vulkan memory budget](https://docs.vulkan.org/refpages/latest/refpages/source/VkPhysicalDeviceMemoryBudgetPropertiesEXT.html)、
[D3D12の確保要件](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12device-getresourceallocationinfo(uint_uint_constd3d12_resource_desc))、
[DXGIの使用量と予算](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_4/nf-dxgi1_4-idxgiadapter3-queryvideomemoryinfo)。

手順は[開発ガイド](../guides/development.md#bistro-pbr)、実行した内容照合・レビュー修正・
CPU/GPU計測と未確認範囲は[現行検証](../verification/current.md#bistro-pbr)へ集約する。
`.3.3`の常駐・更新とallocation予算、B3/B4/B5の品質・表示の最終受入は各課題で扱う。

<a id="godot-performance-target"></a>

#### Godotを基準にしたB2の暫定性能予算

`renderer-yxz`でGodot 4.7.2 Forward+/Vulkanの全量Bistroを基準にする。
同じ機材・1080p・固定route・影/IBL/AA/Bloomなしの範囲で、旧scopeのCPU参考p95 3.5ms、GPU p95 5.0msを
Godot相当の予算の目安とする。独立3 runのうち最も遅いp95を0.5ms単位で切り上げた値であり、
将来の機能込み、他機材、異なるshader品質、Graph単体のoverheadに共通の固定上限ではない。
旧CPU値も現在setupと遅延viewportの観測であり、Renderer全経路へ3.5msを直接課す根拠にはしない。
全process charged CPU/frameを用いる現行B3の正式指標とは、機能条件も集計範囲も分ける。
CPUでは利用側のbuild/compileとruntimeのprepare/record/submit/retireを含む描画経路全体を評価し、
GPU待機は別計上する。並列phaseを重複加算せず、CPU/GPUの和からFPSを計算しない。

frame間隔の基本目標は60 FPSのp99予算16.67ms、追加目標は120 FPSのp99予算8.33msとする。
Godotの通常全量runの最悪p99は10.630msであり、平均約239 frame/sを安定動作の目標へ転用しない。
CPU/GPU描画費用と、OS scheduling/pacingを含むframe間隔の両方を調べる。
Rendererの現在の達成を宣言したものではなく、実表示・ゲーム処理込みの目標は別に検証する。

このB2校正のGodotはrelease実行時にPNG baseのみを非圧縮ImageTextureへ読み込む。
以前の「生成mip」という説明は誤りだった。Godotの生成tangent/材質/depth prepass/表示とRendererのcook/shaderには差があるため、
予算超過時はCPU phase・GPU workload・画像品質の差を説明し、同じ条件で改善前後を測る。
基本目標を満たすだけでGodot相当の効率を達成したとは扱わず、CPU/GPUの比較も残す。
B3/B4の機能や画質を追加したときは比較側にも同じ機能を与えて再校正する。
既存のvalidation、同期、GPU/CPU consumerの寿命、容量・失敗処理、steady-state memoryの規約は維持する。
native API方針の変更・vendor最適化の採用や、一般推奨からの逸脱は行わない。
[手順](../guides/development.md#godot-bistro)と[実測・未確認範囲](../verification/current.md#godot-bistro)を参照する。

<a id="bistro-b3"></a>

### B3: 影・線形HDR・トーンマッピング

`renderer-7ft.4`。B0で選ぶ照明・描画方式に従い、depth-only shadow、PBR照明、浮動小数点HDR中間画像、出力変換を組み合わせる。

**完了条件:** 固定camera/lightsの対象Bistroで影・材質・色空間とHDRから出力まで確認できる。
同じパス構成を二APIで実行し、機能ごとの内容照合と同期・寿命検証を実装時から行う。

利用側Bistroは通常の公開Raster/画像/transferで、静的IBL、4枚のD32によるCSM、Neutral、FXAAを構成する。
Rendererへ効果列挙や専用loweringを加えない。共通追加契約はサンプラー2種、画像とsamplerの対応宣言、
色出力なしのdiscard fragment shaderである。IBLと影を含めPBRは464-byte UBO・16 binding以内とする。
影は4×2048、距離120m、practical split λ=.6、nearestの手動3×3 PCF、末尾10%の分割blendと最終fadeを使う。
camera依存の安定したXY範囲をtexelへ量子化し、全sceneのcaster深度範囲とnormal offset/PCFの余白を確保する。
Bistroのcaster選別は、resident準備時に実際のupload頂点から求めたworld AABBを使う。
各cascadeに渡すFP32行列で射影し、丸めと非正規数の余裕を含めても同じXY clip面の外に全体があるdrawだけを省く。
画面外でもlightのXY範囲と重なるcasterを保持する。全sceneの深度範囲、cascadeの余白とblend、
材質のalpha判定、残るdrawの順序を維持する。境界の構築には従来のresident準備時の頂点走査を使い、
毎frameは準備済み境界を読む。
[VulkanのSPIR-V演算精度](https://docs.vulkan.org/spec/latest/appendices/spirvenv.html)と
[Direct3DのFP32演算規則](https://learn.microsoft.com/en-us/windows/uwp/graphics-concepts/floating-point-rules)を参照する。
Bistroは、固定のresident geometry・材質・texture・影設定の範囲で、各cascadeの
行列16要素のFP32 bit列とcasterのalpha絞り込みが一致する間、前回のD32を読む。
初回と不一致時は従来と同じclear・描画を行い、投入成功後のsnapshotを保持する。
同一queueの予定状態を引き継ぎ、read-onlyのhitではrevisionを変更しない。
既存の準備済みD32を使い、ファイルcapture後の資源差し替え試験に入る前に全登録を解除する。
測定区間で受理したhitとmissをcascadeごとに集計する。`--redraw-shadows`は同じ画質設定で毎frameの描画を再現する。
通常Zと既定near/farを維持し、画質・費用と未確認範囲は実GPU受入の記録へ分ける。

Bistroの主cameraは同じworld AABBと、PBRの頂点shaderに渡すFP32行列を再利用する。
同次clip座標の左・右・上・下・near・farのいずれか一面で、丸めの余裕を含めてAABB全体が外側と分かるdrawだけを省く。
境界と交差するdrawや計算の有限性を保証できない入力を保持する。選別はPBRのみに適用し、
opaque/maskの入力順、blendの距離順と同距離のtie順、cascadeのcaster選別、素材の常駐を維持する。
選別結果はresident準備に対応する予約済み配列を再利用する。
現行BistroのPBRは主camera cullingを適用し、Godotの主参照`all`はこれを無効にする。
測定時のdraw数と構成の差を比較に記録する。

IBLはRGBA16Fの2D diffuse irradiance128×64、specular1024×512の11 mip、BRDF LUT256×256を事前生成する。
GGX/height-correlated Smith、固定Hammersley1024 sampleから始め、diffuseはEを保存してshaderでπ除算する。
specularのroughness0〜1はLOD0〜4へ対応させる。1×1まで使う案は方向依存を失い、LOD5もpole誤差が
独立積分との事前閾値を超えたため採用しなかった。LOD4の64×32で方向依存を保持し、尾の全mipも初期化する。
半精度保存とbilinear/trilinearを含む独立数値pilot、別processでのbyte再現性を受け入れる。
環境diffuseはLambert、specularはsplit-sumで、GodotのBRDF/前処理や多重散乱補償と一致するとは主張しない。

**BistroのAO適用方針:** 元ORM131画像はRが全0で、変換glTFのocclusion参照を無条件に使うと環境diffuseが消える。
原本Exterior.pysceneはAOを有効にせず、対応する[固定FalcorのMaterial既定](https://github.com/NVIDIAGameWorks/Falcor/blob/c53908dd7930c031eb28b7ee49025eb27a833d64/Source/Falcor/Scene/Material/Material.h#L272)も無効である。
従って`conditions.json`のB3照明profileではAO適用を無効にし、Rendererは素材作成境界のAO強度、
GodotはStandardMaterial3DのAO有効値へ反映する。原本・変換glTF・全ORM byte・材質参照は保持する。
R=0を汎用shaderで特別扱いせず、独立fixtureでは本物のAO0が環境diffuseだけを抑えることを検証する。
旧CPU CyclesプレビューはglTF AOの受入証拠ではない。

**Godotの材質mip:** 固定release版のruntime glTF読込はPNGの縮小段を自動生成しない。
B3比較では読込・準備境界で全405画像に`Image.generate_mipmaps`を適用し、normal画像だけ縮小段を正規化する。
通常画像はGodotのチャンネル平均であり、RendererのDDS由来mipとの画素一致は要求しない。
`ImageTexture.set_image`で既存のResource/RIDと共有参照を保って置換し、base画素のhash、
native readbackの形式・非圧縮・完全なmip数、全667材質参照とfilter設定をROI前に検査する。
毎frameの画像走査は追加しない。B2の既存計測物はbase-onlyとして保持し、B3の新しい比較と混ぜない。
根拠は固定版の[PNG読込](https://github.com/godotengine/godot/blob/ed1daf0bf001b61586d9930840f2f1394092c079/drivers/png/png_driver_common.cpp#L123)、
[mip生成](https://github.com/godotengine/godot/blob/ed1daf0bf001b61586d9930840f2f1394092c079/core/io/image.cpp#L2048)、
[画像置換](https://github.com/godotengine/godot/blob/ed1daf0bf001b61586d9930840f2f1394092c079/scene/resources/image_texture.cpp#L63)である。

出力は露出前HDR→EV+1→Khronos PBR Neutral→FXAA→sRGB一回で、AAなしも保存する。
Godotはnative AgX/FXAAの位置/IBL/PCF、影のcaster判定、depth prepass/batchingと表示処理の差を残す。
[B3の描画・メモリ受入](../verification/current.md#bistro-lighting)は成功した。
初回比較はCPU変動とGodotのmip不足があったため目標へ採用しなかった。
全mip、Pコア群（論理CPU 0〜11、mask 4095）・HighQoSを両processで揃えた24 runの正式比較を完了した。
これは比較対象のCPU条件を制御する選択であり、shippingコードの高速化や既定schedulerでの全機材性能を意味しない。

<a id="bistro-b3-performance-target"></a>

#### B3の参照予算と改善対象

同じCore i5-14600KF／RTX 3060、1080p、全Bistro、静的IBL／4分割影／FXAA、固定routeと
上記CPU条件で、正式CPU指標は**全process・全threadの平均charged CPU時間/frame**とする。
Windowsの`GetProcessTimes`のuser＋kernel差分を、同じ定常区間のroute反復数Nで割る。
caller threadだけではGodotのculling workerを捉えられず、elapsedでは実行とGPU待機・deschedulingを区別できないため、
この共通のOS会計を採用する。利用側・worker・計装を含み、Godotにはscript/mainloopと表示も入る。
計測範囲を揃えた全体費用の比較であり、native仕事量や描画方式の等価性を証明するものではない。
[GetProcessTimesの契約](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-getprocesstimes)

CPU参照予算はGodot all・詳細timer/query無効の独立3 processの平均のうち最大を0.5 ms単位で切り上げる。
式は`0.5 ms × ceil(max(run平均1, run平均2, run平均3) / 0.5 ms)`とする。
GPUはtiming有効の最悪run p95を同様に切り上げる。値と独立反復の変動は[現行検証](../verification/current.md#bistro-b3-performance)へ記録する。
これは固定機材・全mip・B3画質・route・CPU policyでの改善目標であり、frame上限や他機材の保証ではない。
run平均をper-frame p95へ読み替えず、通常CIへ壁時計の固定合否閾値を置かない。
旧Godot setup＋viewport p95から丸めた3.0 msは異なるscopeの参考値であり、Renderer全経路の必達CPU予算にしない。
主camera culling無効を主参照とするが、Godot nativeのshadow caster判定は残す。

Rendererの定常区間は開始終了で実際のhost所有・未回収submission数を揃え、前後frameをまたぐpoll/retire・所有返却も含める。
これはGPUで実行中の占有数ではなく、GodotのRD ring容量2やpost-draw時のslot状態とも同一ではない。
同じproducer frame IDの生成から完了を囲む境界ではなく、Rendererの全loopと最終drainのCPU/wallも別に残す。
active hostは明示yieldを除きpoll呼出し・deschedulingを含む非重複phaseの和であり、loop全費用やcharged CPUではない。
Godotのpre/post draw elapsedはnative lowering・record・submit・present・ring待機/回収を含むが、mainloopの前後は除く。
旧setupは現在draw、viewport CPU/GPUはproducer IDが公開されない遅延観測なので、同一frameのCPUとして足さない。
画像の数値受入を画素等価性とせず、差をRenderGraph単体overheadや同一仕事量の効率差へ帰属しない。
wall/Nによるthroughput、iteration percentile、観測完了latency、実表示・ゲーム処理込みのFPSを分ける。
CPUとGPUの時間を加算してFPSを求めない。旧B2予算は別の機能・材質mip条件として保持する。

共通Graph buildは`renderer-7ft.9`で厳密な小writer照合とread再利用を実装し、
[効果と一般入力の追加費用](../verification/current.md#rendergraph-hotspot-optimization)を確認した。
残るpoll/retire・Vulkan prepareの内訳とD3D12 record（`renderer-c4y`）を次の改善調査とする。
GPUもpass別に影・PBR・転送の費用を分け、4 cascadeのcaster負荷を含めて確認する（`renderer-7ft.8`）。
個別timerは含まれる呼出側区間へ重複加算せず、必須の計画・同期・GPU/CPU寿命・容量・native失敗処理を維持する。
今回の結果だけからaliasing、複数queue、vendor固有方式を採用せず、正しいbaselineと同条件で利益・代償を測る。
実測、通常／計測有効の分離、反復変動と未確認範囲は[検証記録](../verification/current.md#bistro-b3-performance)を参照する。
CPU範囲の再校正とGraph最適化の証拠は別cohortとして保持する。AgX追加と再比較は別課題`renderer-5io`で扱う。

### B4: 利用側の独自Bloomとパスの合成・交換

`renderer-7ft.5`。Renderer外のmodule/crateで独自shaderの多段Bloomを定義し、標準HDR出力へ接続する。
中間資源はモジュール内部へ隠し、出力handleで他の処理と組み合わせる。
完了済み`.2.4`のCompute/copyと公開構成`.2.7`が揃った時点で、`.5.1`の小HDR入力試作を始める。
B1全体や二API表示の完成を待たず、`.2.5`以降と並行してUXを確認する。
Bistroへの最終接続はB3のHDR出力を使い、拡張性の問題発見を実シーン全体の完成待ちにしない。

利用側Bloomは、同寸法のシーンリニアRGBA16FからRGB最大成分が1を超える寄与を抽出し、
2×2平均で縦横を2回縮小する。奇数寸法は切り上げ、端は最寄りtexelで延長する。
1/4寸法で3×3テントまたはボックスを適用し、線形補間した結果を元のRGBへ1/8加える。
RGB上限は有限なbinary16の最大値とし、元のalphaを引き継ぐ。無効時は入力handleを返す。
各dispatchが異なる書込Viewを持ち、宣言から依存、初期化、culling、native同期と寿命を導く。
Program作成と一時画像の容量は準備境界で確保する。入力は同じGraphの単一subresource RGBA16Fとし、
初期化はGraph compileに委ねる。標準recipeのscene-linear callbackと利用側構成で同じ実装を使う。

**完了条件:** Renderer本体を編集せずBloomを追加・無効化・別実装へ交換できる。
同じ利用側コードを二APIで実行し、標準描画と同じ資源・同期・寿命・culling・診断を通る。
重複記述・所有負担・拡張時の変更箇所をレビューし、必要なAPI改訂を行う。

### B5: Bistro二APIの統合受入と組込性

`renderer-7ft.6`。各段階の検証を継続したうえで、1920×1080の固定view/camera pathとoffscreen比較、
両APIのWindows表示・自由移動、複数frame、更新・pending終了と補助sceneを統合確認する。
初回受入は既存RTX 3060環境とし、初回読込と定常描画を分けて測定する。

利用側のBistroはウィンドウとオフスクリーンでHDR、Bloom、露出、Neutral、FXAAを共有する。
ウィンドウ向けresolveは同寸法のlinear RGBA8をexportし、PresenterのsRGB targetで変換する。
D3D12は厳密な資源クラスで準備した既存RuntimeにPresenterを接続する。Vulkanはwindow surfaceから
Runtimeを作り、同じ論理Graphの資源を準備する。extent変更では旧Graphと表示の完了を確認し、buffer
snapshotを返却してから一時画像を準備し直す。登録済みassetの内容と所有は保持し、公開keyから
buffer snapshotを取り直す。定常処理では準備済みのGraph、結果、一時画像、表示slotを使う。

**完了条件:** 上表の描画・拡張性・二API・正しさ・所有・性能の証拠が揃う。
外部の利用コードから再現できるガイドを整え、実行/未実行と既知の制限を説明できる。
画像が出たことだけで拡張性や組込品質の達成を主張しない。

### B5後の描画パイプライン拡張

`renderer-7ft.13`。公開Graph・標準部品・入力接続の境界を使い、Forward系の4× MSAA、
Deferred用のMRTと描画区間内のProgram切り替え、Forward+用のライト一覧、直接インスタンシングとGPU生成の間接描画を順に実証する。
Rendererはパス・資源・描画命令の契約、利用側は照明・G-buffer・Program選択・可視性を担当する。
各機能を専用例とBistroで比較し、Vulkan／D3D12の製品構成と開発用検証、GPUの描画結果、
準備済み通常フレームのメモリとCPU/GPU費用を確認する。Deferredの初回受入は1×とする。

### 実測から選ぶ後続の最適化

`renderer-7ft.7`で物理aliasing、複数queue、並列記録、cacheやbarrier削減の必要性と採否を判断する。
旧M4のaliasing/複数queue実験を実装完了とは扱わず、Bistroの基準値を判断材料にする。
B0〜B5の一律blockerにはせず、採用時は能力・正しさ・利益と代償を確認して別課題へ分割する。
正しい比較経路を保持し、検証・profiling・最適化は独立に選択する。

## 16. 未確定事項と決定する時期

| 事項 | 決定時期・判断材料 |
|---|---|
| Bistroの版・対象・変換・camera/lights・品質 | B0の最初に棚卸しして固定する。部分実証と最終対象を区別する。 |
| APIの形、Graph意味論、資源範囲、所有・error | [三層の公開方針](#public-api-layers)を採用。構成境界は`.2.4`後・`.2.5`前の`.2.7`で移行し、独自Rasterは`.2.8`で実証する。具体的な型・契約はB1の二API実行とB2以降の実シーンから改訂する。 |
| 採用基盤、依存、内部RHI、crate配置 | B0でnative上位置換＋低レベル再利用を採用。B1の二API縦切りで見積もりと内部契約を改訂。独立RHI/crate分割は先行しない。 |
| 対象OS/GPU、最低能力、shader言語・compiler | 初回Windows/RTX3060、Vulkan1.3/D3D12、固定GLSL/SPIR-V→HLSL/DXILを採用。新format/binding能力はB1で照会・実証。Linux CPU/Vulkan buildを維持。 |
| 座標・色、texture/material、pipeline/binding/ABI | B0で要求を定め、B1で実証しB2/B3の素材・画質で確認する。 |
| Forward/Deferred、影、HDR format、alpha方式 | Forward/GGX、静的IBL、CSM/PCF、RGBA16F、Neutral/FXAAを出発案に採用。alphaは原本保持。B2/B3で具体値を校正・検証。 |
| 物理aliasing・複数queue等 | Bistro実測で必要性を判断する後続候補。先行実験や自前制御を一律必須にしない。 |
| 画質・性能予算とAPI採用範囲 | B0で比較方法を決め、各段階の実測とB5の統合受入で具体化する。API変更ゼロは保証しない。 |

未確定事項は局所的な暫定判断を妨げるものではない。可逆な判断は理由を記録して進める。一方、所有権・安全性・外部API互換性を暗黙の仮定のまま固定しない。

### 16.1 第二バックエンドの選定と準備条件

第二APIは**Windows上のD3D12**とする。M3の表示対象をWindowsから始める方針と、
利用可能なWindows実機上でVulkanと同じGPU・入力を比較できることを優先した本Rendererの判断である。
これはD3D12の性能優位や、全対象OSで二つのAPIを提供する方針を意味しない。
LinuxのVulkan経路とCPUのみのビルドを維持する。共通Graphの型はnative APIに依存せず、ホストDevice・テクスチャの受け渡しだけでbackend専用の公開型を使う。[エンジン所有テクスチャの契約](rendergraph.md#エンジン所有テクスチャをgraphへ渡す)

Metalも比較候補とした。Apple向けのAPI・shader検証手段がある一方、
現在利用できるMac実機・runnerは確認できていないため、最初の第二APIには選ばない。[^metal]
Apple展開が優先要件になった場合やWindows実機を維持できなくなった場合は、
利用可能な機材・shader変換・検証手段を揃えて選定を見直す。

以下は旧M4で確認した現行実装の範囲である。新計画の採用基盤は[3.3節](#foundation-selection)、追加実装と確認はB1以降に従う。

| 項目 | 確認した範囲と制限 |
|---|---|
| 実機 | RTX 3060、driver 610.88、12 GiB。準備probeでDXGI hardware adapterと実Vulkan選択のvendor/device ID・LUID一致を確認した。 |
| native初期化 | 準備probeでhardware Device、SM6.0、RGBA8 typed UAV load/store、committed bufferの作成・map・破棄を確認した。製品runtimeとbuffer投入の範囲は後述する。 |
| ランタイム・SDK | `d3d12.dll`と`d3d12sdklayers.dll`は10.0.26100.9818。Windows SDK 10.0.26100を含む複数版と、Vulkan SDK 1.4.341.1のDXC・SPIRV-Cross・glslangValidator・spirv-valが存在する。 |
| 検証 | Device作成前のDebug Layer／GBV設定、InfoQueueとGBV有効の起動報告、最終COM Release後までの診断を3回反復確認。shaderのGBV実行と画像等価性は未実施。GBVは性能測定と分ける。[^d3dvalidation] |
| CI | Windows／Linuxのbuild・check・CPU試験を維持し、WindowsのD3D12単独／Vulkan併用を追加。実GPU runnerはなく、管理者のWindows実機で逐次受入する。[実行手順](../guides/development.md#m4のd3d12準備と実gpu受入経路)を使う。 |

旧M4は固定操作をD3D12へ移植してから公開契約を抽出する計画だった。未着手分はB0〜B5へ置換した。
現在は一般Program/bindingとownedパスへB2のcook・二API PBRを接続済みで、[実GPU受入と測定](../verification/current.md#bistro-pbr)を記録した。
全素材を保持した差し替えとnative必要量・OS予算の観測は
[`.3.3`の受入](../verification/current.md#bistro-residency)で確認した。物理page常駐の保証とは区別する。
IBL・影・最終tone mapping以降はB3〜B5で扱う。
D3D12での表示経路を、共通契約の検証に先立つ必須条件にはしない。
ただし[Bistroの確定要件](#bistro-requirements)により、両APIでのWindows表示は最終受入の必須条件である。

shaderは既存GLSL／SPIR-VからSPIRV-CrossでHLSLを生成し、DXCでDXILへ変換する経路を採用した。[^spirvcross][^dxc]
固定5種類の再生成一致とreflectionを確認し、tool版・hash・引数・ABIを[ツールガイド](../guides/tooling.md#d3d12の固定shader生成)と管理下metadataに記す。
binding、combined sampler、行列・座標・データ配置の等価性は生成成功だけで判断せず、実出力と照合する。
この初期経路は標準shaderとfixtureで使う。標準PBRとBistroで共用するHLSLの生成は[shader設計](#bistro-runtime-cook)を参照する。
Rust bindingsはMicrosoftの`windows-bindgen`によるbackend-private生成を採用する。
D3D12・DXGIと必要なWin32 APIだけを生成し、生成型はD3D12バックエンド内部に閉じる。
準備範囲ではDXCのnative APIを使わず、固定CLIでshaderを事前生成する。
再利用ライブラリの依存量と版の衝突を抑える公式推奨に従う選択である。[^windowsrust]
M4準備でgenerator／metadata／runtimeを0.100.0へ固定し、MSRV 1.95が本リポジトリのRust 1.99内であることを確認した。
生成toolは独立workspaceとlockfileに分離し、限定filterとLF固定の生成済み単一RustファイルをGit管理する。
通常のRustビルドへgenerator・SDKを要求せず、再生成一致と生成toolを含む依存監査を確認した。
固定入力と手順は[ツールガイド](../guides/tooling.md#d3d12-bindingsの生成)を参照する。

代案の`windows 0.62.2`は既存allocatorとの接続が容易だが、0.62系への長期保守・修正backportの保証は確認できていない。
この短期互換性だけを理由に旧系列へ固定せず、allocatorは採用bindingsとの型・ABI・所有権・保守状況を別途検証して選ぶ。[^windowsallocator]
COM参照の保持とGPUの利用期間を区別し、resource・heap allocation・descriptor領域・command allocator等の解放や再利用をFence完了で保護する。[^d3dlifetime]

現行D3D12のbuffer基準には`CreateCommittedResource`を使う。
resourceと暗黙のheapを一つの所有者で保持でき、private生成型と既存allocatorの公開型を
未検証のcastで接続する必要がない。DEFAULT bufferは`COMMON`、UPLOADは`GENERIC_READ`、
READBACKは`COPY_DEST`で作成する。[^committed]
採用D3D12ランタイムはDEFAULT bufferの初期`COPY_DEST`を無視して`COMMON`とし、警告1328を報告するため、
準備probeと製品runtimeは実状態と一致する`COMMON`を使う。Graph実行はこの状態から依存を計画する。
代案の外部suballocatorは型・COM所有権・配置条件の接続確認を要するため、この基準には導入しない。
resourceごとのheapによるメモリ・作成費用を許容する単純な基準であり、性能改善とは扱わない。
上限付きの資源再利用は下記FramePoolで実装し、物理aliasingは現行D3D12では未実装である。
旧M4-16の順序を新計画へ持ち越さず、物理aliasingはBistro実測から採否を判断する。
準備probeは作成と破棄だけの確認であり、buffer投入後の寿命とreadback内容は製品runtimeの受入で扱う。

準備は`renderer-we8.1`・`.2`、`renderer-6qc`（adapter・検証・shader経路）、
`renderer-k51`（ローカル実GPU反復実行基盤）で実施した。
現在の`d3d12` featureはDevice・buffer／画像資源・一般Graph／Programを提供し、
`presentation` feature併用時はWindows表示も提供する。
同じ一般経路へ接続したB2 PBRも[実GPU受入](../verification/current.md#bistro-pbr)で確認した。
以下はそのうち管理下Program／bufferの保証境界を示す。

<a id="d3d12-runtime"></a>

<a id="d3d12の管理下programとuniform"></a>

### D3D12の管理下Programと資源binding

`ManagedProgram`は明示準備時にSM6.0のcompute PSOとroot signatureへ具体化する。必要能力はProgramを準備するときに確認する。
Graphはbindingの所有者、用途、offset、論理範囲とshaderのfootprintを構築時に確定する。
native適合ではUniformの256-byte offsetと64 KiB上限、raw storageの16-byte offsetを確認する。
確保時のpaddingを論理範囲の拡大に使わない。

root CBV／SRV／UAVのregister、space、root indexはProgramの不変ABIから導く。
[root descriptorの範囲契約](https://microsoft.github.io/DirectX-Specs/d3d/ResourceBinding.html#using-descriptors-directly-in-the-root-arguments)に従い、
レビュー済みshaderが論理descriptorの範囲に収まることを取込とGraph構築で保証する。
sampled imageとsamplerは各descriptor tableへ対応づけ、実行枠がshader-visible heapと参照先を実完了まで保持する。
PSOとroot signatureの部分作成はCOM所有者で回収する。

D32は`R32_TYPELESS`のDEFAULT資源とし、DSVは`D32_FLOAT`、SRVは`R32_FLOAT`で作る。
実DeviceのTEXTURE2D／DEPTH_STENCIL／SHADER_SAMPLE対応を用途ごとに確認する。
画像uploadのrow配置は[GetCopyableFootprints](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12device-getcopyablefootprints)の結果を使う。
Graphのproducer→consumer間でnative依存を設定し、実行の途中にCPU待機を挟まない。
二APIの内容と寿命の確認は[検証状況](../verification/current.md#managed-uniform)を参照する。

### D3D12のDeviceとGraph実行

Windows限定の`D3D12Runtime`が、hardware adapter、Device、単一DIRECT queueと明示終了を所有する。
adapterのvendor／device IDとLUID、照会した能力をnative型なしで公開する。
CPUだけのGraph構築とLinuxのVulkan経路には、D3D12のnative依存を追加しない。

D3D12 Deviceはadapter LUIDごとのsingletonであり、作成済みDeviceへのDebug Layer／GBV再設定は
Device removalを起こし得る。[^d3ddevice][^d3ddebug]
このruntimeはprocess内の最初の診断設定を固定し、同じ設定の生存runtimeではDevice所有者を共有する。
設定を切り替える要求はnative設定前に拒否する。複数runtimeのqueue操作も共有所有者で直列化する。
既存エンジンは `Runtime::from_d3d12_queue` にホストの DIRECT queue の `IUnknown` を渡す。
Renderer は node 0 の queue とその親 Device を COM 参照で保持し、実 adapter の LUID と能力を照会する。
queue の COM identity が一致する Renderer インスタンスは同じ投入 lock を使う。ホストは共有資源の
所有権を引き渡してから Graph へ接続し、自分の queue 操作を Renderer の実行と順序づける。
異なる queue 間の依存はホスト側の fence で保証する。
[GetDevice](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12devicechild-getdevice)・
[キューの投入順](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12commandqueue-executecommandlists)

既存 Device の診断設定はホストが Device 作成前に決める。引き受け時の `Required` は
Debug Layer と GBV の既存の証跡を検査する。グローバル設定とホストの InfoQueue filter を保持し、
この Runtime が登録した callback は終了時に解除する。確認できない設定は初期化エラーになる。
Device の診断はフィルターを経由しない callback で収集し、DXGI の診断は DXGI 専用の
producer から取得する。DXGI producer は同一プロセスで共有される。
[DXGI の診断 producer](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/dxgi-debug-id)

ホスト Device を受け取った process では、以後の Device 新規作成もグローバル診断 setter を呼ばない。

`development-validation`は`d3d12-diagnostics`も有効にし、native検証の既定を
`D3D12ValidationMode::Required`にする。追加検証なしの既定は`Disabled`である。
独立した`d3d12-diagnostics`なしの明示`Required`は対応外として拒否する。
診断なし構成ではDebug Layer／GBV設定、InfoQueue、callback、診断専用の履歴をコンパイル除外する。
nativeエラーと元の原因、実完了、Device removal、資源保持は製品構成にも残す。
monitorは診断状態だけを保持し、Deviceやqueueの寿命を延ばさない。
履歴は1,024件を上限とする。同じ履歴lockで重大度の集計と保持の判断を行い、保持が決まった場合に本文をコピーする。
上限に達した時点でも警告・エラー・破損の本文を情報メッセージより優先し、保持できなかった問題は件数を記録する。
runtimeを終了しても、別runtimeや未完了処理がnative所有者を保持する間は終了後診断の確認済みとは扱わない。
最終native解放まで観測し、診断の有効要求と取得できた実効状態を区別する。
終了確認の対象はRendererが所有するnative参照の解放までであり、外部ライブラリが保持する
同じsingleton Deviceの最終破棄まで証明するものではない。

`D3D12Runtime::prepare`は共通の不変`CompiledGraph`から、liveなbuffer upload・copy・readbackと画像upload/copy/readback・Raster・Compute dispatchを
準備済みnative資源への命令へ具体化する。Graphのproducer・範囲・culling解析を複製しない。
Graph構築で保証した内容を再走査せず、未対応操作、D3D12固有の上限と資源作成条件を準備境界で扱う。
transientと登録済みbuffer／画像のimport/exportを受け入れ、内容revisionを投入境界で照合する。
不要パス除去後の未使用操作を、そのbackendが未対応であることだけを理由に失敗させない。
bufferの要求sizeはライブラリの上限2 GiB以内とし、実際のnative割当失敗やVRAM budgetとは区別する。

製品Graphの具体化は`graph/bounded.rs`が行い、画像・graphicsの所有と記録は専用moduleへ分ける。
管理下shaderの実GPU fixtureも公開Graphを使う。buffer準備の故障注入は実bufferを一つ作成した直後、命令記録前に失敗させる。
実bufferの部分作成後の解放・エラー原因保持を確認する限定fixtureであり、実ドライバーのOOMや
製品pool初期化の全ての部分失敗を注入した証拠とは区別する。

DEFAULT、UPLOAD、READBACKのcommitted resourceを使い、legacy barrierで全bufferの状態を追跡する。
`D3D12RuntimeConfig::capacity`の`D3D12FrameCapacity`でexecution/result/resource/operation枠、
buffer operand数と最大logical size、upload・readback・scratchのbyte上限を明示し、runtime初期化で全枠を作成する。
`D3D12Runtime::new_with_working_set`は`D3D12BufferClass { size, count }`、画像classとProgramを受け、
必要な実サイズの物理bufferをglobalな個数で直接準備する。初期の均一な最大size poolを先に作らない。
`capacity.buffers`はimportも含むexecutionごとのlogical operand枠であり、物理bufferの個数とは別である。
通常prepareはその上限内で変わる入力を記録し直し、native資源やCPU配列を増設しない。
枠不足・上限超過は待たずに型付きエラーを返す。容量変更は旧consumerの寿命を終えた明示準備境界で行う。
全live passでRasterのvertex・index・uniform・storage read、またはsampled readにのみ使うimport済み
DEFAULT bufferと非depth画像は、COMMONからのimplicit promotionと実行後のdecayを使う。[^d3dbarriers]
D3D12はこれらのread stateへの昇格を許可し、bufferと暗黙にread-onlyへ昇格した画像subresourceを
`ExecuteCommandLists`のGPU実行終了時にCOMMONへ戻す。投入は単一DIRECT queueで、一回につき一つのlistを渡す。
同一keyは一つのGraph資源を指し、別のlive資源に同じ物理領域をaliasしない。
事前のaccess走査で各listの適用可否を決定し、executionごとに予約済みのlogical資源配列を再利用する。
使用したmipはdecayし、使用しないmipはCOMMONに留まるため、一部mipだけを読む場合も次のlistで再利用できる。
Raster read以外のlive accessがある資源、depth画像と一時資源には従来の明示遷移を適用する。
特にupload／copy／dispatch／attachmentとreadを同じlistで行う場合は必要な依存を記録し、
未投入のcancelや失敗は実状態を変えない。registryの失効確認・queue順序・資源寿命・Fence回収は維持する。

明示遷移した資源の記録末尾のCOMMON復帰は、active buffer、copy scratch、used imageの順を保ったtransition配列を
一回の`ResourceBarrier`へ渡す。全資源を毎回明示遷移する方針は状態の追跡が容易だが、Bistroでは同じ
mesh・材質・画像へのreadだけで多数の開始・終了transitionを発行するため、上記の条件で省く。
Microsoftは配列呼出しを要素順の個別呼出しと同じ意味とし、可能な場合のbatch化を推奨している。[^d3dbarriercall]
各executionの配列容量はbuffer operand上限＋scratch一個＋準備済みimage枠数からcheckedに求め、
nativeのUINT件数内に制限して初期化／working-set変更時だけ予約する。通常記録で増設や待機へfallbackしない。
全体の容量確認をtemporary COM cloneや命令記録より先に行い、batchの記録成功後だけslotのplanned stateをCOMMONへ更新する。
配列のoccupied要素が保持するCOM参照はflush、fill失敗、unwind、cancel、Dropで一度ずつ解放し、
実GPUの資源寿命は既存のslot leaseで保持する。`ResourceBarrier`はvoidであり、後続の`Close`失敗は従来どおり未投入の記録として扱う。
canonical registryの内容は成功したExecute／Signal後だけcommitするため、記録の容量不足やcancelから実資源の状態変更を推定しない。
任意のTracy子区間`renderer.d3d12.graph.common_barrier_call`を追加したため、変更前後の時間比較は
親`renderer.d3d12.graph.common_barriers`のinclusiveで揃える。旧leafのselfと新しい親のselfとの差を削減時間として扱わない。
CPU fixtureは順序、COMMON済み／空batch、容量不足とfill失敗／unwind、予約領域の再利用を、
shipping／development-validationの通常・最適化構成で確認する。
実COM参照の回収、native呼出し件数、buffer／画像の内容とcancel後再利用は明示実行の実GPU fixtureで扱う。
実行済み／未実行のnative確認と同条件性能比較は[現在の検証状況](../verification/current.md)へ記録する。
同じbufferの非重複copyは、別のscratch bufferを介した二段copyで共通Graphの意味を保つ。
一つのbufferへ`COPY_SOURCE`と`COPY_DEST`を同時に要求しない。
Enhanced Barriers、外部suballocatorは未対応である。import／exportは後述の共有registryを使う。

`prepare_graph_working_set(Option<&[D3D12BufferClass]>, &[D3D12ImageClass], &[ManagedRasterProgram], &[ManagedProgram])`は全execution/result、
export付きsubmissionが返った
明示準備境界でbuffer/画像classごとの個数、Programごとのgraphics/compute PSOとroot signature、descriptor heapを準備する。
buffer指定の`None`は既存のfree class budgetを保持し、`Some`は置き換える。buffer/画像のclass countは
globalな準備数で、execution数を自動で掛けない。登録済み資源の保持はこの追加budgetと分ける。
`Some(&[])`はその準備境界で未登録free bufferをゼロにし、余剰allocationを退役させる。
登録資源は保持するため、後の明示deleteでその物理storageがpoolへ返ることは別の変更境界として扱う。
空のworking setを、登録資源の将来のdeleteまで含む恒久的なpool禁止としては扱わない。
登録資源のkey・内容revision・初期化範囲・provenanceとnative資源を保持する。
再準備後のGraphには、既存の`ImportBuffer`と`ImportImage`をそのまま渡せる。
各execution leaseは物理資源を共有registryから借り、専用のRTV/DSVとdraw別SRVの枠を所有する。
samplerはDeviceのshader-visible heapに明示準備する。Graphのsampler operandが指定する不変のslotを使い、
新classにない登録画像のdescriptor枠も補う。
Rasterとattachment操作は[共通の対応範囲](rendergraph.md#対応範囲と処理の境界)に従い、
MRT、1×／4×、color resolveを扱う。複数layerとmulti-mip画像のattachment/storage利用は拒否する。
実formatのrender target/depth/sample能力、SM6.0、root signature、native作成失敗はこの境界で扱う。
GraphのRGBA8_UNORM/RGBA16F画像は初回からRT／UAVの固定用途で作成し、後から追加したProgramも同じ登録画像を使える。
typed UAV view能力を画像準備で、load/store能力をProgramの用途に応じて確認する。
`Rgba8Srgb`はsRGB SRVによるsampleとtransferだけに用い、RT/UAV用途を要求しない。
実formatのlinear filtering能力を準備境界で確認し、色を線形化してからfilterする。
depth画像へUAVを付けない。固定effect名の分岐ではなく、Programの不変binding ABIから
root CBV/SRV/UAVとdescriptor tableを導く。依存dispatchは別passとし、同じUAV状態のままでも
必要なread/write依存へUAV barrierを置く。descriptorはslotの命令記録・実GPU完了まで排他で保持する。
single-layer 2Dの複数mipは、mip0から順のpacked chainをuploadし、全levelの内容を確立する。
levelごとの`GetCopyableFootprints`とrow pitch／配置offsetを使い、prepared stagingの範囲内へ配置する。
sampled Viewは任意のcount1 mip、またはbase0からの全chainを選べる。
部分的な複数mip範囲とarrayは未対応として拒否し、native resource stateは画像全体を保守的に管理する。
画像copy/readbackはmip数1に限定し、同extent/formatのRGBA8/RGBA16F copyをCOPY_SOURCE/DEST状態で行う。
一度準備した各drawのdescriptorは未投入cancelまたは実Fence完了まで再利用しない。
D3D12は選択color Programごとのdepth有／無を準備する。Vulkanはprepared画像にD32がある場合だけ
depth有のcolor Programも準備する。buffer-onlyの初期化ではどちらもgraphicsの能力を要求しない。

標準mesh/depthのinlineはvertex b0、texturedのinlineはvertex b1であり、texturedのvertex b0を
16-byte Uniform用root CBVへ割り当てる。CBV addressの256-byte alignmentと論理descriptor範囲を
native適合境界で保証し、root descriptor自体のboundsに依存しない。
独自Rasterも同じ`RasterProgramDescriptor`からvertex属性、inlineのregister／space／stage、
Uniform／StorageReadとimage／samplerのrootを導出する。inlineは最大32 DWORDで、
buffer rootとtableを含む64-DWORD上限を準備時に確認する。定数の実byte数はGraph構築時に保証する。
VS／FS双方で読むbindingは両shaderのread stateを合成し、同じbufferのVertex／Index／Uniform用途も保つ。
colorのblend能力と頂点format対応を実Deviceで照会し、Programのblend／depthをPSOへ適用する。
depth-only PSOはpixel shaderとRTVを持たず、color-only PSOは深度test/writeを無効にする。
生成DXILのclip Y反転とpositive viewportを一組として維持する。

画像upload/readbackは`GetCopyableFootprints`のoffset512-byte・row pitch256-byte条件を扱い、
packedな利用側byte列とnativeのpadded rowを行ごとに写す。最後の行のpaddingまで利用可能とは仮定しない。
D32 copyは全subresource、source boxなし、destination XYZ0とし、RGBA16Fは4成分binary16の格納を保つ。
全資源のlegacy state遷移と必要な依存はshippingにも残す。共有provenanceはindex、そのcopy元と
将来indexとして使えるimport/export bufferを追跡し、
未知内容・頂点範囲外を記録前に拒否する。index本文のprefix要約は内容の更新で失効する。

`D3D12FrameCapacity::provenance_segments_per_resource`はexecution側buffer、copy scratchと
canonical registry entryの既知slice数を
操作数・draw数から独立して予約する。既定は64で、操作数を増やしてもこの予約量は増えない。
内容provenanceを保持しないtransient buffer-only、または完全な不変meshのindex要約だけを使う場合は0を指定できる。
一般upload/copyの内容追跡に足りない場合は`CapacityExhausted`を返し、暗黙の拡張・待機はしない。
必要な上限は明示した準備・変更境界で選ぶ。registry側の保持分も容量へ含め、
旧slot内資源だけの予約枠数や比率を現在の常駐量・時間性能へ読み替えない。

`index_prefixes_per_resource`は同じ内容について保持する異なるindex-countの最大値要約数で、
byte slice数とは独立して予約する。count順に保持して二分探索し、一度検査したprefixを
draw数や頂点数／Device上限の変更だけで再走査しない。write／copyの内容変更は影響する保証を失効し、
既存のsnapshotは元の要約を保持する。満杯なら型付きの容量不足を返し、追い出し・暗黙の再確保はしない。
管理下meshの不変要約だけを使う経路ではこの予約も0にできる。

各Rasterのbuffer/image用途は、そのpassの不変access列を一回列挙して合成する。
`RasterStorage`は準備済みbuffer数とimage数に対応するstate配列・初回使用index列を別々に持ち、
同じbufferのVertex/Index/Uniform/StorageReadのstateを合わせる。適用は従来どおりbufferの初出順、
次にimageの初出順で行い、使用したscratchだけCOMMONへ戻す。後段のindex拒否・cancelや次passへ
用途を残さず、clear-onlyではbufferへ触れない。記録途中のunwindでも残るindex列からresetできる。
使用index列は各資源につき一件だけ追加し、対応する準備済み配列の要素数以内へ収める。
buffer二回・image二回の全access走査を除いた代わりに、資源数に比例する有界metadataを明示準備する。

Rasterの画像SRVは一つのexecution lease内で同じnative資源・sampled format・mip開始/個数を再利用する。
2D/layer 0/plane 0/default component mapping/LOD clamp 0は現在固定であり、可変化するときはkeyも拡張する。
準備済みの半分以下の占有率を持つ有界hash表と使用bucket列を使い、missだけ新しいdescriptorへ書き、
hitでは既存descriptorを変更しない。keyはnative所有者を追加保持せず、slotの全physical leaseが
記録・pending GPUからの参照を満たす。正常Fence完了または未投入cancel後のexclusive slot resetで、
使用bucketだけ消してからregistryがphysical資源を付け替える。Graphの論理IDや前frameのpointer一致を
再利用根拠としない。OOM・未知完了・pending処理ではこのresetへ進めず、従来の保持契約を維持する。
descriptor容量の事前検査は引き続き全binding occurrenceの保守的上限であり、unique View数の予算ではない。
SRV再利用による新しい容量受理範囲や、密な入力の時間改善は未計測のまま主張しない。
任意Tracyの`renderer.d3d12.raster.create_srv` child zoneはmiss時の実CreateSRV呼出しだけを囲み、
発行数を観測する。旧descriptor区間はleafだったためselfとinclusiveが等しく、新構成との時間比較は
同じ親区間のinclusiveを使う。新selfはこのchildを除くため旧selfとの差を削減時間とみなさない。
計装費用・初回source locationの確保は通常構成と区別し、比較のwarm-upとfeatureを揃える。
[D3D12 descriptorのGPU handle・参照先寿命](https://learn.microsoft.com/en-us/windows/win32/direct3d12/descriptors-overview)
に従い、shader-visible heapと参照先を全命令の完了まで保持する。
logical imageのslot選択はdescription別の予約済み候補列を二分探索し、class内cursorで取得する。
登録画像も同じ対応づけを使い、同descriptionの重複classでは元のindex順を保つ。
attachment用途の画像だけを既存のaccess列から一度集め、その画像へRTV/DSVを設定する。
sampled専用画像等のtarget view作成を除き、native identityが変わり得るslot間でviewを使い回すcacheは追加しない。
候補列・使用attachment列は明示準備するmetadataであり、GPU完了または未投入cancel後の
exclusive resetでのみ返す。COMMON復帰、importの失効検査、pendingのlease保持は変更しない。
depth clearの有限値・範囲・subnormal拒否はGraph構築で確立し、privateな不変operandを引き継ぐ。
native適用時は、実Deviceへの適合、容量・同期・完了を管理する。

Graphのclear値はframeやpassで変えられるが、D3D12のoptimized clear値は資源作成時に固定される。
作成時に値を渡さないclearは厳格受入でwarning820/821を発生させる。固定optimized値と異なる値も
API上は許可されるが効率上の警告対象であり、診断を抑制して通常の受入を通さない。
[CreateCommittedResourceの契約](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12device-createcommittedresource)
に従い、色`[0,0,0,1]`／深度`1`の作成時既定値と一致するclearはnative命令、
それ以外は明示準備した全域triangleと専用PSOで行う。
色clearはDSVをbindせず、深度clearはRTVをbindせずALWAYS/writeを使い、他のattachmentを保持する。
通常描画の前にpipeline/root/buffer/viewport等の状態を設定し直す。
全域geometryとclear用pipelineも記録・GPU完了の所有連鎖へ含め、通常frameの資源作成は増やさない。

これはattachmentのnative clearを優先する一般推奨からの限定的な変更である。代案のclear値ごとの資源増設は
変更可能な入力を固定された資源容量へ無制限に写し、毎frame作成はメモリ規約に反するため採用しない。
追加draw/帯域・状態設定の費用が残り、時間性能の優位は未計測である。独立期待値とVulkanのnative clearとの内容照合、
厳格診断・製品構成の別実行で適用範囲を確認する。shaderのFP32 subnormal flush差を避けるため、D32 clearの
subnormal値はGraph構築で拒否し、ゼロまたは0〜1の通常値を不変operandへ引き継ぐ。

`submit`または`submit_prepared`は待機せずに投入し、`D3D12Submission`がcommand allocator／list、
全buffer/image・allocation、Program/descriptorとDevice所有者を実GPU完了まで保持する。
未投入のprepared処理の破棄はキャンセルであり、記録の終了後に既存枠へ返却する。
`poll`／`wait(timeout)`で対象Fenceの正常完了を確認するとnative execution枠を返却する。
独立result枠の常時map済みREADBACK bufferを保持し、`D3D12SubmissionResult::readback_len`でサイズを調べ、
`readback_into(pass, &mut [u8])`で利用側の領域へコピーする。完了時の追加CPUコピーはなく、
未読結果もnative実行枠を占有しない。result枠とREADBACK容量は別に準備する。ゼロ確保を時間性能の改善とは説明しない。
timeoutは結果を返さず、同じsubmissionを再度待てる。readbackは一度だけ消費し、長さ不一致では結果を保持する。
Fenceの`UINT64_MAX`やDevice removalを正常完了とみなさない。[^d3dfence]

`ExecuteCommandLists`後の`Signal`失敗は未投入rollbackを保証できない。[^d3dexecute][^d3dsignal]
その場合はDeviceを停止し、GPU利用終了を証明できない所有連鎖を保持する。
未完了submissionの通常Dropはpoolへ処理を引き渡し、`D3D12Runtime::poll`で実Fenceを観測して回収する。
最後のpool所有者を終了する境界では全対象Fenceに共通の最大5秒予算で待ち、完了を証明できなければ同じ保持を行う。
この異常経路では保持資源を回収できない。通常経路は明示的な完了確認で回収し、Drop待機へ依存しない。
通常のsubmitへQueue／Device全体の暗黙waitを追加しない。

共通Graphの全byte一致、終了後までの厳格診断、最適化された診断なし経路の内容を別runで確認する。
実行方法は[開発手順](../guides/development.md#d3d12-buffer-acceptance)、
実行済み・未実行の範囲は[現在の検証状況](../verification/current.md)に記す。
canonical import/exportは`.2.5`で一般buffer／画像の共有registryへ接続した。
予定内容とGPU完了、更新revision、pending参照と再利用、容量上限の現行契約は
[RenderGraphのD3D12資源管理](rendergraph.md#d3d12-buffer-lifetime)を参照する。
slotごとのCOMMON復帰を維持し、同じqueueでのGraph間受け渡しにCPU待機を要求しない。
旧M4-05〜07の順序を必須にせず、B0の選定を受けてB1以降へ組み直す。
一般RHIや将来の描画・binding契約をこのbuffer実行だけで確定したとは扱わない。

<a id="d3d12-presentation"></a>

### D3D12のWindows表示と任意時間計測

`d3d12,presentation`は`D3D12Runtime::for_window`／`D3D12Presenter`を公開する。
表示設定・outcome・再試行手順は[二API共通契約](rendergraph.md#windows-presentation)と同じであり、
D3D12だけの構成へVulkanのruntimeやash／VMAを追加しない。
利用側は同じ公開Graph・標準recipe・登録meshを使い、cameraとevent loopも共有する。
内部を一つのRHI traitへ先に統合する代わりに、既にあるnative所有を維持して観測可能な意味を揃える選択である。

Win32の`HWND`について一つのflip-discard swapchainを作り、作成APIにはDeviceではなくDIRECT queueを渡す。
backbufferはRGBA8_UNORM、RTVはRGBA8_SRGBとし、既存fullscreen shaderで同寸法のlinear RGBA8 sourceをsampleする。
sRGB変換を無変換copyで代用せず、現在の単一sample／不透明Window契約を保つ。
sourceはCOMMON→PIXEL_SHADER_RESOURCE→COMMON、backbufferはPRESENT→RENDER_TARGET→PRESENTと遷移する。
Graphも同queueでCOMMONへ戻すため、pendingなGraph出力から表示へのCPU waitは不要である。
[Swap Chains](https://learn.microsoft.com/en-us/windows/win32/direct3d12/swap-chains)の作成queue・状態・buffer寿命条件に従う。

VSync ONはsync interval 1、OFFは0とALLOW_TEARINGを使う。
OFFは`DXGI_FEATURE_PRESENT_ALLOW_TEARING`を照会し、対応がなければ要求を拒否して設定を保つ。
swapchain作成にも同じtearing flagを設定し、automatic Alt+Enterを無効にしてWindowの変更を利用側event loopへ揃える。
[DXGI_PRESENT](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/dxgi-present)の条件を守り、
通常の`Present`にはDO_NOT_WAITを指定する。`WAS_STILL_DRAWING`を成功へ潰さず、
受理されていない表示は`NotReady`とし、既に提出したfullscreen命令とsource所有は実Fenceまで保持する。
再試行で同じ描画を無条件に重ねて投入しない。Device removal、Signal失敗など完了不明の状態では停止して所有を保持する。

各表示slotは固定のcommand allocator／list、Fence、source descriptorと強いsource leaseを持つ。
描画後のFenceと、受理されたPresentの後へ同queue上で置くmarkerを区別する。
slotの再利用は実Fence値で確認し、`UINT64_MAX`を正常完了としない。
このmarkerはアプリが提出したGPU処理の再利用根拠であり、Vulkan maintenance1のpresent fenceや
画面への表示完了時刻と同じ機能ではない。DXGIは受理したbackbufferの表示側参照を自身で管理する。
アプリのsource・descriptor・command所有と、DXGIの表示参照を別々に扱う。

resizeは同じswapchainへ`ResizeBuffers`を行う明示変更境界である。
`prepare`はこのPresenterの未完了slotがあれば`NotReady`を返し、通常経路にwaitを追加しない。
完了後に古いcommand listの記録をresetし、backbufferの直接参照を解放してからresizeする。
新しい同HWNDのswapchainを並行作成する案は採用しない。
[ResizeBuffers](https://learn.microsoft.com/en-us/windows/win32/api/dxgi/nf-dxgi-idxgiswapchain-resizebuffers)
はbuffer／view／commandからの直接・間接参照の解放を要求するため、Vulkanの旧世代保持をそのまま移植できない。
希望extentゼロはnativeへ渡す前に`Suspended`へ変換する。DXGIの寸法0はclient areaを意味し、最小化の契約の代用にしない。
VSync変更もreadyになるまで新しい設定の適用を保留し、成功した`prepare`を変更完了として報告する。
このbackendは退役swapchain世代を保持せず、共通の`collect_retired_swapchains()`は0を返す。
その代わりresize時の新規描画を一時停止する費用があり、無停止resizeや時間性能の優位は主張しない。

`wait_idle`／`close`はこのPresenterのmarkerだけを明示timeout内で待ち、Queue／Device全体のidleを呼ばない。
正常なcloseはアプリのGPU参照を回収してswapchainを解放し、その後も診断monitorで最終native解放を確認できる。
timeoutやnative失敗からscanout完了を推定せず、Dropの保守的回収でも証明できない所有連鎖はWindowごと保持する。
通常pollでsourceのleaseを返すときもprepared registryにanchorが残り、毎frameの最後のheap／native解放を発生させない。

任意計測は`crate::profiling::{GpuTiming, SubmissionMetrics, PresentationMetrics}`を二APIで共有する。
`D3D12Runtime::set_profiling(true)`の初回は実行leaseを返した明示準備境界で、各executionに二つのtimestamp queryと
16-byteの常時map済みREADBACK領域を用意し、DIRECT queueのtimestamp frequencyを照会する。
記録する命令の前後へqueryを置き、resolve済みtickを実Fence完了後に読み、queueのHzから時間へ変換する。
64-bit modulo差とhost上の経過時間から複数wrapを排除できないsampleは未測定とする。
使用中のqueryを上書きせず、結果取得のwaitや通常frameのquery／readback作成を加えない。
[Direct3D 12 Timing](https://learn.microsoft.com/en-us/windows/win32/direct3d12/timing)に従い、
CPU時計とGPU tickを直接引き算したり、複数queueの共通時刻域を仮定したりしない。
D3D12のtimestampはDIRECT queueの先行処理が終わった位置で採るBOPの値である。
VulkanのTOP_OF_PIPE／BOTTOM_OF_PIPEとはnativeのmarker意味が異なるため、
同じ記録命令の区間を囲む値であってもshaderだけの占有時間や普遍的なAPI速度差を示すとは扱わない。

Graph結果の`metrics()`はprepare／record／submit／退役のCPU時間とGraph GPU区間を返す。
Presenterは独立した`set_profiling`と`take_profile`を持ち、表示用GPU区間と
acquire／source準備／record／submit／present／退役のCPU区間を返す。
pending＋未消費sampleは`max_frames`内に制限し、満杯ならacquire前に容量不足を返す。
GPU submit済みのsampleは表示要求が拒否されても回収し、sample数を表示成功数と同一視しない。
失敗pollの反復費用と明示waitのblock時間はsampleの退役時間へ含めず、利用側のpoll全体・待機時間として測る。
一度も有効にしなければ専用queryを作らずphase timerも呼ばない。
無効化は将来の記録に適用し、準備済み領域は明示変更／終了まで保持して通常frameで破棄しない。

受入はstrictとshippingを別processで実行し、同じcamera／Graph出力の内容照合、
resize／最小化復帰／取得後cancel／pending終了と最終native解放まで確認する。
通常frameのメモリ窓にはbuild／compile／記録／投入から、表示、実完了、readback消費・全owner返却までを含める。
VSync OFFの処理時間とONのpacingは別runにし、native診断による計装費用を製品性能へ混ぜない。
実行手順は[開発ガイド](../guides/development.md#windows-presentation)、
実行済み・未実行の環境と結果は[現在の検証状況](../verification/current.md)に記録する。

## 17. Codexの作業手順

### 17.1 着手時

最初にリポジトリの指示・状態・実装を読み、依頼対象の段階を特定する。本書の章構成を、そのまま大量のクレートやファイルへ機械的に変換しない。

既存の変更を尊重し、関係のないリファクタリングや削除を同時に行わない。現在のコードと本書が衝突する場合は、差分と移行方法を記録する。既存基盤の再利用を必須にせず、B0の比較から維持・改修・置換を選ぶ。置換する場合も、失われる対応・保証、移行方法と検証を明記し、未検証の新経路を完成扱いにしない。

一つの作業では、検証できる縦切りの機能または限定的な設計変更を選び、変更対象・受入条件・非対象を決める。必要な仕様は、採用バージョンの公式ドキュメントと実際の依存コードで確認する。存在しないAPIを推測して使用しない。

Vulkan関連の実装・レビューでは、[AGENTS.mdの実装規約](../../AGENTS.md#vulkan-implementation-and-review)に従い、
[ベストプラクティス](../guides/vulkan-best-practices.md)の該当節を適用する。
Vulkan/VMAの契約と現行Renderer方針は必須、一般推奨は原則採用とし、異なる選択には理由・代案・根拠を残す。
機材依存の最適化は正しさと計測した効果・代償で判断する。後続機能の解説を理由に今回の実装範囲を広げない。

### 17.2 実装と検証

典型的な確認コマンドは以下である。これは本書作成時に実行した結果ではなく、リポジトリに合わせて設定する基準例である。

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

実際にはtarget・backend feature・GPUテストを分けたCI matrixを用意する。OS専用機能や排他的featureがあるため、常に`--all-features`で検証できるとは仮定しない。既存のlint違反を修正する場合も、依頼と無関係な大量変更に広げない。

CPUテスト、実GPUテスト、性能測定を分けて実行・報告する。GPUへアクセスできない環境では、実GPU確認を「未実行」として理由を残す。ソフトウェア実装やモックの成功を、対象GPUでの性能・同期・ドライバー互換性の検証と同一視しない。

テストを通すために検証レイヤーを無効にしたり、全フレーム待機を隠れて追加したり、画像差分の許容値を理由なく広げたりしない。暫定策が必要なら、影響・除去条件・追跡先を残す。

### 17.3 変更完了時の報告

報告には、実装した内容、変更した契約、実行した検証、未実行の検証、既知の制約、計測結果、次の最小タスクを含める。実行したコマンドと環境を記録し、実測値と推定値を混ぜない。

重大な設計判断は現在の設計文書を更新する。背景、採用案、比較した代替案、影響、見直す条件を短く記載し、検証結果は現在の検証状況へまとめる。段階別・日付順の記録を増やさず、置き換え前の内容はGit履歴に残す。文書だけを更新して、対応する動作確認まで完了したように報告しない。

## 18. 変更ごとのDefinition of Done

以下を満たした範囲だけを完了とする。機能に無関係な項目は、その理由を明示して対象外にできる。

- [ ] 対象タスクの受入条件を満たし、非対象の機能を不用意に追加していない。
- [ ] 公開境界にVulkan・VMAの型や暗黙の所有権が漏れていない。
- [ ] Vulkan関連変更はベストプラクティスの該当要件に従い、一般推奨からの変更理由・根拠と、実行済み／未実行の検証を記録した。
- [ ] 関連する同期・寿命・失敗経路のテストがあり、既存テストを維持している。
- [ ] `unsafe`の前提と責任範囲を説明できる。
- [ ] 実GPUの検証状況を、成功・失敗・未実行に分けて記録した。
- [ ] 最適化を含む場合は基準経路との比較結果があり、画質・性能・メモリの変化を説明できる。
- [ ] 診断情報から、変更した処理とパス・リソースを対応づけられる。
- [ ] API・操作手順・設計判断・既知の制限を、変更に応じて更新した。

## 19. 最初にCodexへ渡す作業指示

次の指示を入口とする。配置先は既存リポジトリの文書構成に合わせる。

```text
この実装方針を読み、まず既存リポジトリの状態を確認してください。
目標はBistroのテクスチャ付きPBR・影・HDR/トーンマッピングと、
利用側独自Bloomを、同じパス定義でVulkan/D3D12へ接続することです。

B0で決定した素材・画質・API利用例と3.3節の採用基盤・移行範囲、現在の検証結果を入力にしてください。
一般パスとB2 cook・二API PBRの接続は完了済みです。続く作業はBeadsの現行依存関係と依頼範囲から選んでください。
既存基盤の維持や自前実装を目的にせず、進めやすさ・総工数・UX・品質で選んでください。
公開APIを少数の成功例だけで決めず、実装は検証可能な縦切りに分けてください。

Beadsの現行Bistro計画から依頼範囲の未完了タスクを選び、
そのタスクの実装と実行可能な検証まで進めてください。
GPU検証ができない場合は、実行済みの確認と未検証項目を明確に分けてください。
所有権・同期・公開APIに影響する判断は、理由を現在の設計文書へ反映してください。
```

## 20. 参考資料

以下は外部ライブラリ・APIの仕様確認に用いた一次資料である。本文の設計・実装順序は本プロジェクトの方針であり、参考実装の採用方法や開発順序をそのままコピーしたものではない。確認日は2026-11-12。変更される資料については、実装時にも採用バージョンを確認する。

[^ash]: ash公式README。Vulkanに近い薄いラッパー、生成される定義、検証・安全性の責任に関する説明。`https://github.com/ash-rs/ash`

[^ashdocs]: ash 0.38.0+1.3.281 APIドキュメント。`https://docs.rs/ash/0.38.0+1.3.281/ash/`

[^vkmem]: vk-mem 0.5.0。VMAのRustバインディング、Allocationとサブアロケーションの概要。概要内の古いサンプルをそのまま採用せず、実際のAPI定義と照合する。`https://docs.rs/crate/vk-mem/0.5.0`

[^vkmemmanifest]: vk-mem 0.5.0のCargo.toml.orig。ashの互換バージョンとfeatureの確認。`https://docs.rs/crate/vk-mem/0.5.0/source/Cargo.toml.orig`

[^vmbuild]: vk-mem 0.5.0のbuild.rs。C++のVMA実装をビルドする処理。`https://docs.rs/crate/vk-mem/0.5.0/source/build.rs`

[^allocator]: vk-mem 0.5.0 Allocator API。作成時の寿命条件、bind / map / flush / invalidate、破棄・解放の契約。`https://docs.rs/vk-mem/0.5.0/vk_mem/struct.Allocator.html`

[^vmaalias]: VMA公式資料「Resource aliasing (overlap)」。共有メモリの要件と利用上の注意。オンライン資料とvk-mem同梱VMAのバージョン差にも注意する。`https://gpuopen-librariesandsdks.github.io/VulkanMemoryAllocator/html/resource_aliasing.html`

[^features]: Khronos Vulkan Guide「Enabling Features」。問い合わせと有効化の手順。`https://docs.vulkan.org/guide/latest/enabling_features.html`

[^sync]: Khronos Vulkan Guide「Synchronization Examples」。アクセスに対応する依存、バリア、レイアウト遷移の例。`https://docs.vulkan.org/guide/latest/synchronization_examples.html`

[^queues]: Khronos Vulkan Guide「Queues」。キュー間順序、host側同期、ハードウェアとの対応。`https://docs.vulkan.org/guide/latest/queues.html`

[^wsi]: Khronos Vulkan Guide「Swapchain Semaphore Reuse」。present待機Semaphoreの再利用。`https://docs.vulkan.org/guide/latest/swapchain_semaphore_reuse.html`

[^wsirecreate]: Khronos Vulkan Samples「Swapchain Recreation」。旧Swapchainとpresent関連リソースの寿命管理。`https://docs.vulkan.org/samples/latest/samples/api/swapchain_recreation/README.html`

[^validation]: Khronos Vulkan Guide「Vulkan Validation Overview」。Valid Usageと検証レイヤー。`https://docs.vulkan.org/guide/latest/validation_overview.html`

[^debug]: Khronos Vulkan Guide「VK_EXT_debug_utils」。オブジェクト名、ラベル、debug messenger。`https://docs.vulkan.org/guide/latest/extensions/VK_EXT_debug_utils.html`

[^atom]: O3DE Atom「AZ::RHI::FrameGraphCompiler」。共通コンパイルとプラットフォーム固有処理の分担。`https://docs.o3de.org/docs/api/gems/atom/class_a_z_1_1_r_h_i_1_1_frame_graph_compiler`

[^d3dvalidation]: Microsoft「GPU-based validation and the Direct3D 12 Debug Layer」。Device作成前のGBV設定と、計装・非同期出力・性能への影響。`https://learn.microsoft.com/en-us/windows/win32/direct3d12/using-d3d12-debug-layer-gpu-based-validation`

[^dxc]: Microsoft DirectX Shader Compiler公式README。HLSLからDXIL／SPIR-Vへの生成。`https://github.com/microsoft/DirectXShaderCompiler`

[^spirvcross]: Khronos SPIRV-Cross公式README。SPIR-VからHLSL等への変換と制約。`https://github.com/KhronosGroup/SPIRV-Cross`

[^windowsrust]: Microsoft windows-rs公式README。再利用ライブラリのfocused crate・private binding生成の選択指針。実装時には採用版と照合する。`https://github.com/microsoft/windows-rs`

[^windowsallocator]: gpu-allocator 0.28.0の公開manifestは`windows >=0.53, <=0.62`を要求する。新しい生成型との接続を未検証のまま互換と扱わない。`https://docs.rs/crate/gpu-allocator/0.28.0/source/Cargo.toml`。Microsoft担当者の旧系列の不具合への回答はmasterでの修正と0.62での回避策を示し、0.62へのbackportを約束していない。`https://github.com/microsoft/windows-rs/issues/5189#issuecomment-6448298133`

[^d3dlifetime]: Microsoft「Important Changes from Direct3D 11 to Direct3D 12」。明示同期と、アプリが管理する資源・descriptorメモリの寿命。`https://learn.microsoft.com/en-us/windows/win32/direct3d12/important-changes-from-directx-11-to-directx-12`

[^committed]: Microsoft「ID3D12Device::CreateCommittedResource」。resourceと暗黙のheapの所有、UPLOAD/READBACKの初期状態、最後の参照を解放する前のGPU利用終了条件。`https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12device-createcommittedresource`

[^d3ddevice]: Microsoft「D3D12CreateDevice」。adapter LUIDごとのDevice singletonと、removed Deviceの再作成失敗。`https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-d3d12createdevice`

[^d3ddebug]: Microsoft「ID3D12Debug::EnableDebugLayer」。Device作成後の有効化はDeviceをremoveする。`https://learn.microsoft.com/en-us/windows/win32/api/d3d12sdklayers/nf-d3d12sdklayers-id3d12debug-enabledebuglayer`

[^d3dfence]: Microsoft「ID3D12Fence::GetCompletedValue」。Device removal時の戻り値は`UINT64_MAX`。`https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12fence-getcompletedvalue`

[^d3dexecute]: Microsoft「ID3D12CommandQueue::ExecuteCommandLists」。投入呼び出し自体は戻り値を持たない。`https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12commandqueue-executecommandlists`

[^d3dsignal]: Microsoft「ID3D12CommandQueue::Signal」。queue上のGPU側signalをHRESULTで報告する。`https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12commandqueue-signal`

[^d3dbarriers]: Microsoft「Using Resource Barriers to Synchronize Resource States in Direct3D 12」。read／write stateの制限、bufferのpromotion／decayと明示遷移、barrier削減の推奨。`https://learn.microsoft.com/en-us/windows/win32/direct3d12/using-resource-barriers-to-synchronize-resource-states-in-direct3d-12`

[^d3dbarriercall]: Microsoft「ID3D12GraphicsCommandList::ResourceBarrier」。配列の件数はUINT、指定順の個別呼出しと同じ意味を持つ。戻り値はなく、runtime検証の失敗は後続のCloseへ報告される。`https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12graphicscommandlist-resourcebarrier`

[^metal]: Apple「Metal」。AppleプラットフォームとAPI／shader検証・診断ツールの概要。`https://developer.apple.com/metal/`

### 動的な資産準備と描画入力

標準描画は `Renderer` / `WindowRenderer` と `Frame`、ホスト所有の Graph は `pipeline`、個別技法は `ViewPipeline`・レシピ・`passes` を使う。装備の採否はアプリが決める。

変更境界で `SceneAssets` にモデルを追加すると全 LOD と個体の姿勢・材質を収集し、描画枠は個体ごとの最大部位数を合計する。独立した資産の先読みとメッシュ・材質の組の事前登録は区別する。装備交換は候補の準備成功後に公開する。保持結果の返却や再構成は呼び出し側が `AssetPrepareError` の容量情報から判断する。

配布物を読むホストは、実行中に初めて使うバッファ寸法・画像形式・サンプラーを `AssetPreparation::replace_asset_reservations` で変更境界に準備できる。呼出元が保持中のフレーム結果を返し、使用中と準備中の資源を合わせて必要な予約を渡す。ウィンドウ版で提示結果の回収中は `Poll::Pending` を返す。ホストはイベント処理と描画を続け、次の更新で予約を再試行する。同じカメラ、時間履歴および登録済みハンドルを保ったまま装備の追加分を用意できる。訓練場では並行して必要な装備版だけから予約を算出する。[装備の予算と使用方法](../guides/tooling.md#equipment-cook)
