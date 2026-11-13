# 製品版と開発用の検証方針

調査・制定日: **2026-11-13**。対象はゲームへ組み込むRenderer全体であり、
Rustの公開API、asset入力、RenderGraph、VulkanとD3D12を含む各バックエンドへ適用する。
本書は[AGENTS.md](../../AGENTS.md#production-and-development-validation)から参照するリポジトリ規約である。
API仕様の要件、一次資料の推奨・公開事例、そこから採用するRendererの方針を区別する。

**製品版の描画・submit経路では、保証済みの内容を繰り返し検証しない。**
高価な不具合検出は開発用構成へ分離し、入力の保証は適切な境界で確立して引き継ぐ。
必要な同期・所有・容量管理・失敗処理は製品版でも維持する。
Vulkan Valid UsageやRustの安全性を免除する方針ではなく、条件を満たす方法と場所を整理する方針である。

[現行実装への適用](#current)で、実装済みの切替と残る保証処理を確認する。
現行の具体的な契約は[RenderGraph設計](../design/rendergraph.md)と[Vulkan設計](../design/vulkan.md)、
実行済みの確認は[現在の検証状況](../verification/current.md)を参照する。

<a id="principles"></a>

## 1. 保証と、その場での検査を分ける

VulkanのValid Usageは定義された動作を得る条件であり、同じ条件を全呼び出しで再検査する義務ではない。
仕様はcoreの検査を最小限とし、開発時はLayerを有効、検証済み製品は既定で無効とする方針を示す。
正常に定義されたエラーを返す条件はValid Usageとも区別する。[Vulkan仕様][vk-fundamentals]

本Rendererでは、処理を次の四つに分類する。関数名が`validate`や`assert`であることだけでは決めない。

| 分類 | 目的 | 製品版での扱い |
|---|---|---|
| 入力の保証を確立する処理 | 内容・範囲・形式・ABI等を保証する | cook、取込、作成・更新、Graph構築など最も早く十分な情報が揃う境界へ置く |
| 実行を成立させる処理 | 計画・同期・所有・容量・正常完了・エラー処理 | 維持する。既に保証された条件の再照合は省略できる |
| 開発用の不具合検出 | 誤用の早期診断、実装の整合性の独立再計算 | 製品構成から除外する。保証の成立をこの処理だけへ依存させない |
| 観測・調査 | 詳細dump、名前・label、profiling、障害解析 | 検証と独立して選択する。製品では必要な低コスト情報に限定する |

**規約:** 各検査について、対象条件、保証する境界、保証の有効期間、失効する操作を説明できるようにする。
既存の型とprivateな状態で説明できる場合は、新しい型・cache・traitを追加しない。
実行時に変化する状態を「assetは検証済み」という理由だけで省略しない。
逆に、必要性を証明せず「安全のため」として同じ内容の再走査やCPUコピーを常設しない。

## 2. 一次資料から確認できる境界

| 根拠の種類 | 確認した内容 | Rendererへの適用 |
|---|---|---|
| API仕様・公式推奨 | Vulkan Layersは開発向け。製品は既定で有効にせず、Guideは製品への同梱も避けるよう説明する | 製品実行の前提にValidation Layers／SDKを置かない。API契約は別途満たす [仕様][vk-fundamentals]・[Guide][vk-validation] |
| 言語の安全性契約 | safeな利用側がunsafe実装の前提を破って未定義動作を起こせる抽象化は健全でない | releaseだけ安全性の前提を削除しない。private状態・不変性・所有権で保証を移す [Rust Reference][rust-ub]・[Rustonomicon][rust-privacy] |
| ゲームエンジンの公開事例 | Unreal RDGの追加validationはDebug／Developmentで有効、Test／Shippingではコンパイル除外。resource transition生成は別の機能 | Graphの診断と計画・同期生成を分離する。Unreal固有の構成をRustへそのまま移植する意味ではない [Unreal RDG][ue-rdg] |
| asset処理の公開事例 | Unrealのcookにはasset／map validationと、検証エラーを失敗にする指定がある | 必要な検査を定義して事前処理へ移す。cookという名称だけで全条件の保証済みとは扱わない [Content Cooking][ue-cook] |
| 他APIの検証・実行契約 | D3D12 GBVはshader・commandを計装する。Fenceの完了値はDevice removalも表す | 計装の時間を製品性能にしない。正常完了とDevice異常の判別は製品にも残す [GBV][d3d-gbv]・[Fence][d3d-fence] |
| 他APIの診断事例 | Metal API ValidationはCPU費用があり、追加設定でattachment内容も変更できる | 検証はログ表示だけではない。実効設定・負荷・出力への影響を分ける [Metal API Validation][metal-validation] |

上表は比較・根拠であり、本Rendererに未実装のcook、D3D12画像実行、Metal対応を追加したとは扱わない。

<a id="classification"></a>

## 3. 検査を配置する基準

| 対象 | 保証・処理を置く場所 | 製品版のdraw／submitで残す条件 |
|---|---|---|
| 頂点形式、index内容、有限値、asset内部の参照関係 | cook／取込／作成・更新時 | 内容と利用範囲が不変なら全量再走査をしない。可変入力は更新境界で保証し直す |
| shader binary、reflection、binding ABI | 生成・取込とprogram／pipeline作成時 | 使用するprogram・layoutとの対応が保証されれば同じ照合をdrawごとに繰り返さない |
| API version、extension、feature、format、Device上限 | 実Device初期化と該当資源・pipeline作成時 | 実機に依存する条件はcookだけで済ませない。不変の照会結果は再利用する [feature有効化][vk-features] |
| byte数・offset・alignment・整数overflow | 外部入力を内部表現へ変換する境界 | 外部・可変値に必要な防止策は維持。不変範囲から証明できる演算を再検査しない |
| ハンドルのowner・世代、import内容のrevision | 取得・更新・Graph構築／投入境界 | 検証後に削除・更新できるAPIでは保証し直す。強い所有・不変計画で排除できれば再照合を省ける |
| Graphの範囲・用途・未初期化read・循環 | 構築／compile境界 | 安全な計画の生成に必要な拒否・分岐は維持。保証済みの計画を独立に再点検する処理は開発用へ |
| dead branchの詳細な誤用診断、冗長な内部整合性scan | 開発用の検証経路 | 正常な計画やsafe APIの保証に不要と確認した範囲は除外できる。現行の拒否契約を変更する場合は明記する |
| producer、hazard、culling、実行順・barrier生成 | Graphの実行計画作成 | 入力Graphが変わるなら必要な計算を行う。検証offと一緒に削除しない |
| staging／descriptor／実行枠の空き、割当成功 | 予約・作成・投入時 | 現在の容量と戻り値を扱う。検証済み入力でも資源不足は起こり得る |
| GPU完了、flush／invalidate、reset、退役・回収 | 実際のアクセス・完了境界 | 実際の同期と寿命を維持。frame番号・CPUハンドルのDrop・timeoutを正常完了とみなさない |
| acquire／presentの結果、Swapchain世代・extent | 表示の実行境界 | リサイズ、最小化、out-of-date等を処理する。事前asset検証では代替できない |
| native OOM、Device loss／removal、停止状態 | 戻り値を取得する場所と状態遷移 | 製品でも失敗として扱う。assertを外した結果、成功扱いにしてはならない |
| 詳細dump、全履歴、debug名・label、検証専用のCPU provenance | 開発・調査用の独立機能 | 製品に必要なものだけ選択する。無効時は専用の構築・保持・文字列化も除去する |

配置は検査の内容と保証に基づいて決める。「O(1)なら全部残す」「O(n)なら全部外す」のような規則にしない。
高価な走査・allocation・payload保持を優先して取り除き、残る分岐や照合も役割と費用を説明する。

<a id="guarantees"></a>

## 4. 検証済みデータの保証を引き継ぐ

### asset・meshと信頼境界

**規約:** 検証済みとして扱う入力は、どの内容・版・範囲・形式・対象能力について何を保証するかを定める。
cook済みデータを信頼する運用でも、形式version、byte長、必要な範囲等を安全な取込境界で確定するか、
それを外部契約として引き受ける入口を明示する。データ自身が持つ検証済みフラグやchecksumだけでは
意味的な正しさの保証にならない。追加の複雑な証明書形式を必須にするものではない。

不変meshなら、indexの必要な範囲の最大値等を生成時に求め、頂点数・実Device上限との適合を
取込・利用範囲の確定時に保証する。毎drawの全index走査や、検査のためだけのCPU byte列保持は避ける。
全体の最大値だけでは必要な部分範囲の契約を表せない場合は、範囲を限定するか必要な要約を選ぶ。
巨大な多条件cacheを先に作ることは要求しない。

部分write、copy、再upload、範囲・頂点bufferの変更、Device／programの変更が保証に関係するなら、
更新境界で失効・再確立する。内容revision、世代、範囲等は関係するものだけを保証へ結び付ける。
「同じハンドルだから同じ内容」「前回の試験で通ったから今回も有効」という根拠は使わない。

実行時生成やGPU生成データは、その生成処理・上限・同期で契約を成立させる。
CPUで事前検証できないことを、毎frameのreadback検証を強制する理由にも、無条件に信用する理由にもしない。
一般shaderや外部データを受け入れる機能の追加時には、その範囲の信頼境界を定める。

### RustのAPIと内部不変条件

**規約:** safe APIの保証をreleaseでも維持する。任意の利用側入力を受けるsafe関数から、
unsafe処理に必要な検査だけを消す変更は禁止する。[Rustの健全性][rust-ub]
既存のprivateな型、immutableな値、検証済み範囲、資源を保持する計画で保証できれば、反復検査は不要である。
private状態を書き換えるsafe関数も保証の一部として確認する。[private状態とunsafe][rust-privacy]

外部の呼び出し側に安全性の条件を引き受けさせる必要があるときだけ、対象条件を明示した`unsafe`境界を使う。
型・所有・範囲・更新・同期・Device条件のうち関係する責任を記述し、「正しいデータを渡すこと」だけで済ませない。
名前だけのvalidated型、複雑なtypestate、互換性のためだけのsafe／unsafe並行APIを必須にしない。

`assert!`はreleaseでも評価される。必要な内部不変条件を止める最後の防御があること自体は禁止しない。
通常発生する入力失敗や容量不足には型付きエラーを使う。[assert][rust-assert]
`debug_assert!`は開発用診断に使えるが、unsafe処理の安全性をそれだけに依存させない。[debug_assert][rust-debug-assert]
予約、submit、状態更新、資源解放等の必要な副作用をassertや検証用cfgの中へ入れない。
外部値のsize／offset計算は、debug時だけのoverflow検出へ依存させず、必要なchecked演算か成立する範囲を使う。
[checked演算][rust-checked]・[profile設定][cargo-profiles]

<a id="graph"></a>

## 5. RenderGraphの実行機能を検証から独立させる

Graphの宣言からproducer、必要なpass、hazard、native状態遷移を求める処理は、通常の実行機能である。
その結果を別のモデルで再計算したり、利用ミスを詳細な名前で早期報告したりする処理は追加の診断として分離する。
Unreal RDGも追加validationとresource transitionの生成を別に説明する。[Unreal RDG][ue-rdg]

**規約:** 検証なし構成でも、有効な入力について同じ出力、必要な依存、資源の所有、失敗時の状態遷移を保つ。
不正入力の詳細メッセージまで一致させる必要はないが、safe APIに必要な拒否を失ってはならない。
検証用の初期化・補助copy・waitが、本来不足している同期や未初期化内容を偶然補う構造は禁止する。
検証用のmetadataと、barrier生成・retirementに必要な状態は、必要性に沿って分離する。

不変の`CompiledGraph`へ論理的な保証を持たせても、外部資源を後から変更・削除できるなら、
その変化に対する保証は別に必要である。資源の強い保持、変更できる期間の制限、版の照合等から
現在のAPIへ適切な方法を選ぶ。反復検査の回避だけを目的に汎用的な計画cacheを導入しない。

<a id="runtime"></a>

## 6. 製品版に残す実行状態と失敗処理

submit受理、GPU正常完了、CPU利用終了、登録解除、物理資源の回収を区別する。
Vulkanのsubmit OOMには状態不変の保証があるが、それを全native失敗へ一般化しない。
Fence timeoutを正常完了にせず、Device loss後の内容を有効な結果として返さない。
投入済み命令が使うmemoryを早く解放しない。[submit][vk-submit]・[Fence待機][vk-wait]・[memory解放][vk-free]
D3D12の`GetCompletedValue`も、Device removal時の`UINT64_MAX`を正常完了の大きい値と解釈しない。[D3D12 Fence][d3d-fence]

VMAの内部lockは、同じAllocationやVirtualBlock、CPU／GPUアクセスのすべてを同期するものではない。
これらの同期と所有の責任も検証offで消えない。[VMAの契約][vma]
失敗時に安全に停止するか回復するかは現在のbackend契約に従い、検証設定で成功／失敗の意味を変えない。

robustnessは、対応する範囲外アクセス等への動作保証を与える本番機能であり、開発用validationとは別である。
利用するなら保証の範囲・feature有効化・費用を確認する。任意アクセス、同期、寿命全体の保証へ拡張しない。
現在のDeviceでrobustBufferAccess等が有効だとは仮定しない。[Robustness][vk-robustness]・[feature][vk-features]

<a id="builds"></a>

## 7. ビルド・実行設定

次の表は満たすべき構成の区別であり、すべての切替が現在実装済みという意味ではない。

| 構成 | 最適化 | Rendererの追加検証 | native検証 | 観測 |
|---|---|---|---|---|
| 開発・通常受入 | 用途に応じる | 有効 | Vulkan Core＋SyncVal等を明示有効 | 問題を診断できる情報 |
| 最適化済みの正しさ確認 | release相当 | 有効 | 通常受入と同じ | 最適化された通常経路を確認 |
| 製品 | release相当 | コンパイル除外 | 既定無効、検証Layer非依存 | 必要な運用情報だけ |
| 製品相当の性能測定 | 製品と同じ | 無効 | 無効を明示し観測結果・未確認事項を記録 | 必要な計測だけ追加し費用を区別 |
| 障害調査 | 再現条件に合わせる | 必要なものを明示有効 | 対象API・機能に応じる | 詳細dump／capture等を選択 |

**規約:** 最適化と検証を独立に選べるようにする。`release`や`debug_assertions`だけを
すべての検証の唯一の切替にしない。最適化ビルドでも検証を使える必要がある。
Rustの`debug-assertions`と`overflow-checks`も独立した設定である。[Cargo Profiles][cargo-profiles]

製品から除外する追加検証は、実行時boolで呼出しを避けるだけでなく、専用のmodule・field・収集・
payload保持・文字列化・依存関係まで、該当するものを条件付きコンパイルで外せる構造を基本とする。
`#[cfg]`は項目を除外し、`cfg!`はboolへ展開される。最適化による除去を使う場合は結果を確認する。
ログの非表示やsubscriber未設定を、検証処理の除去の証明にしない。[条件付きコンパイル][rust-cfg]

Cargo featureを使う場合は追加型の検証featureを基本とし、負のfeatureで安全性を消す設計を避ける。
featureは依存経路で統合されるため、製品の最終的なfeature集合・profile・環境設定を確認する。
`--no-default-features`や`--all-features`成功だけで製品構成を確認したとはしない。
現行の`development-validation`、独立した`vulkan-diagnostics`／`d3d12-diagnostics`／`graph-diagnostics`と実行例は
[開発手順](development.md#製品開発用の検証設定)を参照する。
[Cargo Features][cargo-features]

debug_utils、名前・label、profiling、障害ログは検証と独立した選択である。
製品に不要な診断拡張・依存・確保・名前処理を要求しない設計を目指す。
運用上必要なエラー種別・原因等は、適切な出力量と費用で残す。正しさをログ出力の有無に依存させない。

<a id="verification"></a>

## 8. 変更時の確認と受入

検証の追加・移動・除去では、変更に関係する次の内容を既存設計文書とBeadsへ記録する。
固定件数のテストや毎回すべてのGPU機能の再実行を要求するものではない。

1. 何を保証し、どの境界・所有・内容の不変性により反復検査を省くか。更新・削除・copy等の失効条件。
2. 製品に残る処理と、開発用へ移るscan・metadata・CPU payload保持・文字列・native検証。
3. 検証あり／なし双方のbuildと有効入力の挙動。必要な入力拒否、容量不足、未投入失敗、pending、退役の回帰。
4. GPU実行に影響する変更では、最適化＋Renderer追加検証なし＋native検証なしの通常経路でも内容照合する。
   別runでnative検証有効の適用範囲の実GPU受入を行い、両方を確認する。
5. 除去を主張する処理について、cfg／依存関係・生成物の確認、または走査回数・allocation・保持byte等の根拠。
6. 性能を主張する場合のCPU build／compile／record／submit／retire、GPU、メモリ、負荷・環境・実効設定。

検証専用経路が存在しない変更に、新しい切替やテスト基盤を形式的に増設しない。
ただし切替を実装した場合は両経路を確認し、`--all-features`だけで無効側の確認を代替しない。
出荷・性能用のfeature確認には`cargo tree -e features`等を使える。[Cargo Features][cargo-features]

実GPUの正しさ受入では既存のwarning／errorゼロ方針を維持する。
SyncValにはhostアクセス・aliasing・間接アクセス等の検出限界があり、診断ゼロだけで正しさを証明しない。
GPU-AV／GBVは計装や追加処理を行うため、通常受入・追加診断・製品相当測定を分ける。
[固定SDKのSyncVal][syncval]・[GPU-AV][gpuav]・[D3D12 GBV][d3d-gbv]
Metalの検証設定やUnrealのImmediate Modeのように負荷・最適化を変える調査経路だけの成功を、
通常の製品経路の確認で代替しない。[Metal][metal-validation]・[Unreal RDG][ue-rdg]

実行方法は[開発ガイド](development.md)、結果と未実行範囲は[現在の検証状況](../verification/current.md)へ記録する。

native検証の要求設定と、Layer等から観測した実効状態は別に記録する。
起動報告の不在だけで外部Layer注入がないと証明したことにせず、未確認ならそのまま記録して測定の主張を限定する。

<a id="current"></a>

## 9. 現行実装への適用と採用理由

制定時の実装確認点は`698c936`。レビュー修正で次の境界を実装した。

| 現行の状態 | 本方針との関係 |
|---|---|
| `development-validation`は既定off、profileと独立 | Graphの保証済み入力の独立再検査とdead部分だけの循環診断をコンパイル除外できる。live計画の拒否・同期は維持 |
| native検証の既定は上記featureありで`Required`、なしで`Disabled` | 対応するnative診断featureなしの明示`Required`は失敗。検証を省いて成功扱いにはしない |
| `vulkan-diagnostics`を独立選択 | offならdebug_utils・callback・名前／labelの構築・診断履歴を除外。運用エラーの処理・ログは維持 |
| `d3d12-diagnostics`を独立選択 | offならDebug Layer／GBVの設定・InfoQueue・callbackと専用履歴をコンパイル除外。native失敗、Fence完了・Device removal、資源保持は維持 |
| `graph-diagnostics`を独立選択 | offなら論理Graph／Vulkan loweringのdumpと専用解析情報の保持を除外。実行順・内容・必要なアクセスは維持 |
| D3D12のbuffer loweringは不変`CompiledGraph`から準備する | Graphが保証するbyte範囲を反復再走査しない。未対応操作の拒否、実Deviceへの適合、配置・命令記録・完了・CPU copyを成立させる処理は製品にも残す |
| `MeshData::new`で入力を検査しimmutableな内容要約を生成 | privateな管理下uploadへ保証を引き継ぎ、mesh描画時のindex本文再走査・検査用CPU保持を省く。実Device上限は別に適合確認 |
| 任意Graph upload／copyのCPU provenance | 部分byte更新や未知の穴からindex安全性を確立するために必要な経路は残す。開発用診断として一括削除しない |
| Graph構築のowner・範囲・用途、producer／hazard、live循環、投入時のimport照合 | safe入力と変動状態を保証するために維持。公開されたimportの更新・削除は不変mesh保証では代替しない |
| profilingは実行時opt-in | 検証・詳細dumpとは独立し、計測用queryや費用を記録する |

採用理由は、製品の反復scan・CPU保持・診断依存を抑え、保証を成立させる場所を明確にするためである。
代案の「全検証を常設」は単純だが、保証済みの大量データをdrawごとに読む費用が残る。
「releaseで全チェックを一括削除」は、変動状態やsafe APIの前提まで失うため採用しない。
保証の移動には更新時の失効漏れや検証なし側だけの不具合というリスクがあり、前節の両経路確認で扱う。
本書によって同期・寿命・native API要件、既存の実GPU受入水準を緩和しない。
削減効果は未測定で、機能を移した実装ごとに評価する。

<a id="sources"></a>

## 一次資料と適用範囲

調査では下記の本文を確認した。Webの`latest`、Epic、Microsoft、Appleの資料は動的であり、
将来の変更時は該当節を再確認する。Vulkanの現行baselineは1.3、ash 0.38、vk-mem 0.5／VMA 3.3.0、
検証SDKは1.4.341.1。最新Guideにある任意featureを有効化済みとは扱わない。
RustのWeb標準ライブラリ資料は調査時1.100表示、リポジトリtoolchainは1.99。
本方針は既存のmacro・privacy・cfg・feature・checked演算の契約を用い、新APIの導入を要求しない。
EpicのRDG・Asserts・Cookingは調査時5.9表示の比較事例であり、本Rendererの実装契約ではない。

- Vulkan: [仕様のValid Usage／Usage Validation][vk-fundamentals]、[Validation Overview][vk-validation]、
  [feature有効化][vk-features]、[Robustness][vk-robustness]、[submit][vk-submit]、[Fence待機][vk-wait]、[memory解放][vk-free]。
- allocator・検出限界: [VMAの一般契約][vma]、[SDK 1.4.341.1 SyncVal][syncval]、[同GPU-AV][gpuav]。
- Rust: [安全性][rust-ub]、[private状態とunsafe][rust-privacy]、[assert][rust-assert]、[debug_assert][rust-debug-assert]、
  [checked演算][rust-checked]、[条件付きコンパイル][rust-cfg]、[Cargo Profiles][cargo-profiles]、[Cargo Features][cargo-features]。
- ゲーム・他API: [Unreal RDG][ue-rdg]、[Asserts][ue-asserts]、[Content Cooking][ue-cook]、
  [D3D12 GBV][d3d-gbv]、[D3D12 Fence][d3d-fence]、[Metal API Validation][metal-validation]。

Unrealのassert類は種類・設定により式の評価が残る。この事例も、診断を無効にしたという名前だけで
費用や副作用を判断しない根拠とする。[Unreal Asserts][ue-asserts]

[vk-fundamentals]: https://docs.vulkan.org/spec/latest/chapters/fundamentals.html#fundamentals-validusage
[vk-validation]: https://docs.vulkan.org/guide/latest/validation_overview.html
[vk-features]: https://docs.vulkan.org/guide/latest/enabling_features.html
[vk-robustness]: https://docs.vulkan.org/guide/latest/robustness.html
[vk-submit]: https://docs.vulkan.org/refpages/latest/refpages/source/vkQueueSubmit2.html
[vk-wait]: https://docs.vulkan.org/refpages/latest/refpages/source/vkWaitForFences.html
[vk-free]: https://docs.vulkan.org/refpages/latest/refpages/source/vkFreeMemory.html
[vma]: https://gpuopen-librariesandsdks.github.io/VulkanMemoryAllocator/html/general_considerations.html
[syncval]: https://vulkan.lunarg.com/doc/view/1.4.341.1/windows/synchronization_usage.html
[gpuav]: https://vulkan.lunarg.com/doc/view/1.4.341.1/windows/gpu_validation.html
[rust-ub]: https://doc.rust-lang.org/reference/behavior-considered-undefined.html
[rust-privacy]: https://doc.rust-lang.org/nomicon/working-with-unsafe.html
[rust-assert]: https://doc.rust-lang.org/std/macro.assert.html
[rust-debug-assert]: https://doc.rust-lang.org/std/macro.debug_assert.html
[rust-checked]: https://doc.rust-lang.org/std/primitive.u64.html#method.checked_mul
[rust-cfg]: https://doc.rust-lang.org/reference/conditional-compilation.html
[cargo-profiles]: https://doc.rust-lang.org/cargo/reference/profiles.html
[cargo-features]: https://doc.rust-lang.org/cargo/reference/features.html
[ue-rdg]: https://dev.epicgames.com/documentation/en-us/unreal-engine/render-dependency-graph-in-unreal-engine
[ue-asserts]: https://dev.epicgames.com/documentation/en-us/unreal-engine/asserts-in-unreal-engine
[ue-cook]: https://dev.epicgames.com/documentation/en-us/unreal-engine/cooking-content-in-unreal-engine
[d3d-gbv]: https://learn.microsoft.com/en-us/windows/win32/direct3d12/using-d3d12-debug-layer-gpu-based-validation
[d3d-fence]: https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12fence-getcompletedvalue
[metal-validation]: https://developer.apple.com/documentation/xcode/validating-your-apps-metal-api-usage
