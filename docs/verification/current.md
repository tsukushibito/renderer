# 現行実装の検証状況

最終確認日: 2026-11-14。実行手順は[開発ガイド](../guides/development.md)、
現在の契約は[RenderGraph](../design/rendergraph.md)と[Vulkan設計](../design/vulkan.md)、
全体範囲は[実装方針](../design/implementation-policy.md)を参照する。
過去の検証表・比較はGit履歴に残し、この文書は現在の確認範囲を示す。

<a id="managed-uniform"></a>

## 管理下Programと資源bindingの二API実装

`renderer-7ft.2.1.5`。Uniform/Storage共通契約と`.2.1.3`の重複検査整理、`.2.1.4`の
RGBA8画像/View/samplerを維持し、RGBA16F 2D画像とHDR値を保持するsampled readbackを両APIへ追加した。
`Binding`/`BindingResource`がbuffer/View/samplerを一つのcomplete collectionで扱い、
旧buffer専用binding APIを置換した。既存の固定ProgramKindは増やしていない。
親`renderer-7ft.2.1`のdepth、他mip/layer/filterや管理下assetの拡張と、
`.2.2`以降のowned Graph実行は未完了である。
[作成保証とVulkan所有](../design/vulkan.md#managed-uniform)、
[D3D12のroot/tableと保証](../design/implementation-policy.md#d3d12の管理下programとuniform)、
[再実行](../guides/development.md#管理下uniform-programの作成と受入)を参照する。

safeなshader入口はレビュー済みのuniform/sample/sample-float readback 3本である。
両sampled assetはseparate texture2Dとsamplerを`textureLod`で実際に使い、
DXILでも`t0/space0`と`s1/space0`をactiveに保持する。既存assetはRGBA8へpackして16 byte書き、
新`sampled_float.comp`は4 sampleの全RGBAをFP32 bit列の64 byteとしてStorageへ書く。
SPIR-VはShader capabilityと32-bit floatのみで、shaderFloat16や16-bit演算型を要求しない。
生成時のSPIR-V/DXIL reflection照合はslot/type/count/member offset/size/local sizeに加え、
float2D image、sampler、writeonly出力と余分な資源を確認する。
新出力のarray 4、literal count、stride 16、offset 0、block size 64も照合する。
固定SDKの全8本SPIR-V/DXIL再生成Verify、SDK不要の改変拒否（pwshとWindows PowerShell 5.1）、
D3D12 bindings Verifyが成功した。旧7shaderのsource/binary/text/reflectionの37hashは比較元`e6e8fd0`と不変である。
新sampled-float SPIR-Vは`ec3213ae49b0a47ee2d96cceb28e73f54edb0cb917df77b9f76b98bd3f779438`、
DXILは`a4a959173d88b4cce681bc2286ee426f698e9170fbadca0fc04fde654808891b`。
生成bindingsは`0057a6da906d1c8b0f4c23b29fb75074638cc76fe09d3fe29b84d75d34d60a28`で一致した。

作成境界で全slotの欠落/重複/余剰、resource kind/用途、owner、論理buffer範囲とDevice alignment、
実format/usage/extent対応、View範囲を保証する。画像はmip/layer/sample各1、未初期化作成であり、
暗黙upload/submit/waitを持たない。Viewは画像を保持し、bindingはProgram、buffer/範囲、
View/Image、samplerとdescriptor heap/setを強保持する。不変なslotと資源metadataを記録時に再走査しない。
入力を直接参照するlazy slot解決を維持し、検査専用Vec、選択済みslotの再照合、別のslot再探索を増やしていない。
全入力検査に成功してからnative descriptorを一度具体化し、失敗時は途中の参照を解放する。
Vulkanのsampler生存数上限は全旧/新作成箇所でDevice単位予約/失敗返却/native破棄後返却へ揃えた。
D3D12のtexture/samplerはshader-visible SRV/SAMPLER heapのtable、bufferはroot CBV/UAVを使う。
native容量/失敗・同期・実Fence完了・GPU寿命は製品に残す。
公開Image/Storageの作成成功は内容初期化を意味せず、各shaderが書いた16または64 byteだけをreadbackする。
ImageとViewは同じ実formatを使い、実DeviceのRGBA16F対応はRGBA8と区別して作成境界で保証する。

private upload/dispatch/readbackと入力色、Image/Storageの排他lease/stateは`cfg(test)`に限定する。
通常buildにCPU検査用画素payloadやprivate executor metadataを保持しない。
初回uploadした旧画像と別に作った新画像を保持し、旧画像再利用時は再uploadせず旧内容をsampleする。
VulkanはCOPY→sampled readとStorage write→copy→host依存、D3D12は実GetCopyableFootprintsの
row配置とCOMMON→COPY_DEST→NON_PIXEL_SHADER_RESOURCE→COMMONを確認した。
取消で未初期化を公開せず、pending中の競合を拒否し、不確定失敗では既存Retainedと所有連鎖を保持する。
Vulkan初回画像遷移の既存COPY execution対策も維持する。

Windows x64/Rust 1.99.0でfmt、CPUのみ/製品/開発の全target Clippy（`-D warnings`）が成功した。
CPUのみ通常71件、製品release通常191件、開発release通常202件と、各構成のRustdoc 1件が成功した。
Rustdocは両backendの公開Image/View/Sampler/binding作成例をコンパイルする。
共通契約、shader/ABIと両backendの作成/同期/所有の独立レビューで修正必須の未解決指摘はない。
境界値の組合せは共通CPUへまとめ、native試験での同じ拒否classのmatrixを形式ごとに複製しない。
SDK不要の新negative caseは64-byte配列ABIの差分へ絞り、既存の共通image/sampler検査を再列挙しない。

RTX 3060/driver 610.88でreleaseの実GPU試験を逐次実行した。
RGBA8は既存の2x2非対称入力をnearest/clampで読み、公開272-byte Storageのoffset256からの16 byteが一致した。
RGBA16Fの旧入力はrow-major成分 `[-2,-1,-0.5,-0.25,0,0.25,0.5,1,1.5,2,3,4,8,16,32,64]`、
新入力は成分の逆順で、各32 byteの正確なhalf格納である。両APIとも公開320-byte Storageの
offset256からの64 byteが独立に指定したFP32期待bit列と全byte一致した。
NaN/subnormal/負のゼロの追加契約は今回要求しない。境界外clamp、nearest対linearの違い、
画像の行/成分、逆順binding、型/owner拒否、未投入cancel、pending/busy、実完了後reuse、
runtime facade先drop、最終View/Image/sampler/heap/Device解放を確認した。
strictはVulkan Core/SyncValまたはD3D12 Debug Layer/GBVの実効を確認し、
最終native解放後までwarning/error/corruption/callback問題ゼロだった。
製品の追加検証/専用診断除外・native Requested Disabledでも同じbyte一致を別processで確認した。
この成功だけから外部Layer不存在やnative診断有効を主張しない。

今回の統合結果では既存StorageとRGBA8を含め、strict/製品の各Vulkan 3件、D3D12 2件、計10件が成功した。
D3D12のsampled 1件はRGBA8/RGBA16Fの両caseを実行する。workerで実GPUを重複実行せず、統合後に逐次実行した。
正式な統合ゲートログは`.tools/b1-rgba16f/gates/`に保存した。
変更範囲外のGraph/WSI全体の実GPU回帰は今回再実行していない。直近の全60件成功はGit履歴の`.2.1.4`の記録を参照する。

Linux実GPU、別GPU/driver、一般shader/owned Graph画像実行、depthの二API管理下契約、
性能・長時間耐久、実native OOM/Device lossは未実行である。性能優位や計算量の改善は主張しない。
既知の未再現起動crash `renderer-0pb`の原因確定や解決も主張しない。

<a id="repository-review"></a>

## ソース全体のレビューと効率修正

`renderer-420`、比較元`f649bcf`。第一者のRenderer Rust 64ファイル、example 11、
integration test 4、Bistro Python 28、validator 2、PowerShell 6と、benchmark、
独立試作、生成・依存設定、CI、文書構成を分担レビューし、主要な修正を別担当が再レビューした。
生成bindingsとshader成果物はFFI・ABI・所有・固定入力による再生成の境界で確認した。
配布asset全体の任意の破損や全driverの動作を証明するレビューではない。

| 課題 | 確認した問題と修正 |
|---|---|
| `renderer-420.1` | compile解析用の名前・初期範囲・依存の複製を借用へ変更。drawの中間配列を除去し、不変snapshotのパラメーター保証をprivateな値で引き継ぐ |
| `.2` | mesh uploadの完了prefix・残byte・全resident退役走査をcursor、残量、retiring索引へ変更。不要なLRU sortと重複Arc cloneを除去。readbackは退役後に一度sortして検索。importはowner/generation/revisionを維持し、深い不変metadata再比較だけ開発診断へ移す |
| `.3` | resize時とsampleの重複Fence pollを除去。再描画の期限を固定し、ゼロ寸法で待機する。非ゼロ寸法への復帰はSwapchain作成済みかどうかに依存しない |
| `.4` | 同一file/archive/FBXの重複hash、画像sourceの全量保持、Node Bufferの複製、出力されないtangent計算を除去。validatorによる入力上書きと、失敗した比較結果をsummaryが成功として公開する不具合も修正 |
| `.5` | 不変Device限界値を初期化時から再利用。参照transferの二重検査・初期化範囲の複製/書込ごとの再反映・記録後のplan保持を除去。広域dead-code抑止を撤去し、参照専用経路とprivate INFO履歴をtest構成へ限定 |
| `.6` | D3D12 readbackの全量zero-fill後の上書きを除去。結果検索/残数判定の二乗走査をsort・二分探索・残数へ変更し、保証されたCOM親interfaceへのQueryInterfaceを除去 |
| `.7` | 同一queueで既に可視化済みの完全に同じread scopeだけ、重複barrierを省略。CPU回帰は8読取の依存8件から1件。新stage/access、write、WAR、layout変更への依存は保持 |
| `.8` | PPMのpixelごとの小書込をbuffer化。計測reportにcompile featureを記録し、観測したnative検証のbenchmark混入を拒否。shader hashをstream化、照合textをLF固定、全SPIR-Vのcompile/検証成功後に公開 |

[機械可読の証拠と同一CPU harness](source-review.json)を保存した。Rust 1.99.0、release、
default featureなしで、2,048資源/4,096passのcompileはallocation要求46,146→40,001回、
総要求byte 7,230,276→7,125,062。8,192drawのframe構築は19→18回、
2,689,489→1,706,449 byteだった。後者はsnapshot作成後の測定で、常駐量・peakではない。
GPU速度、wall-clock、Bistro性能の改善率は測定していない。

Blender 4.5.16 LTSの同一入力でtangent計算の有無を比較し、1,296mesh/
2,832,120triangle、11,664配列比較とmetadata、全407個のglTF関連fileが一致した。
alpha再検証の出力39,662 byteも変更前後で一致した。
shaderは最後のcompileを故障させても全5個の既存SPIR-Vを保持し、
`core.autocrlf=true`のcheckout相当で13個のtextがLFとなって再生成照合に成功した。

構成上のGraphとengineの相互依存、Program/bindingと記録、物理資源とGraph、
WSIの複数所有境界の同居は、下記`renderer-hyf`で機能追加前に整理した。
[現在の境界](../design/implementation-policy.md#source-boundaries)を既存B1課題へ反映する。backendのprivate分離、生成bindingsの隔離、
`foo.rs`と`foo/`、integration test/実GPUignoredの区分は維持する。
現在のraw Graph作成境界のextent/clear検査、動的なowner/revision、同期・寿命・容量・native失敗は保持する。
将来の一般Programではread scope省略の成立条件を再確認する。
既知の未再現native起動crash `renderer-0pb`は未解決で、このレビューで解決したとは扱わない。

統合したRustソースでfmt、製品/開発の全target Clippy（`-D warnings`）、CPU通常66件、
`presentation,d3d12`のrelease通常180件、追加検証/Graph診断ありのrelease通常191件が成功した。
RTX 3060で逐次実行したreleaseの厳格Vulkan 46件、製品経路/新規回帰4件、
Renderer追加scanなしのbatch 1件、D3D12寿命1件、二API共有bufferの厳格/製品各1件の
合計54件が成功し、表示smokeも厳格/製品の両方で成功した。
各厳格gateは正常終了とnative最終解放後まで既存の診断判定を維持し、新規native警告/エラーはない。
製品構成の成功をnative検証が実効有効だった証拠へ流用しない。
Python 60件、Node 10件とshader再生成照合が成功した。
公式offline glTF validatorの0 error/1,591 warning/1,591 infoは従来の`issues`全体と一致し、
nativeの診断ゼロと区別する。Linuxは実GPU未実行で、CIのCPU/build確認を別に記録する。

<a id="source-reorganization"></a>

### B1実装前の4つの責務境界の整理

`renderer-hyf`、整理前`9f6b3a4`。`renderer-7ft.2.1`の一般Program機能を始める前に、
4点を実装・統合した。[設計](../design/implementation-policy.md#source-boundaries)と
[機械可読の検証記録](source-review.json)の`source_reorganization`に対応する。

| 対象 | 整理後の境界 |
|---|---|
| GPU非依存Graph | 公開facade、resource/pass/error/builder、共通解析を分離。Graphからengine型を参照せず、mesh変換はengine bridge、保証と追加はGraph builderが担当 |
| Vulkan Program | publication、creation、inline BindingPool、native recordを分離。作成・bindingにGraph依存なし。追加allocation/Arc/lockなし、lease保持とpool→Programの解放順を維持 |
| 両APIの物理資源 | backend直下のresources、Vulkan errorへ抽出。物理owner/factoryにGraph型の依存なし。D3D12のCPUアクセスは完了・範囲・排他的所有のprivate unsafe契約を明記 |
| Windows表示 | native Surface/Swapchain/slot/acquire guard/Program/commandsとGraph adapterを分離。native側にGraph状態を持たず、private slot/guardの操作が取消と退役を所有 |

公開入口はfacadeからの直接reexportで維持し、並行API、一般RHI、`mod.rs`を追加していない。
独立レビューでmesh provenance/Arc共有/失敗時の未追加、部分作成cleanup、
descriptorと参照資源、GPU＋present両完了、未確定失敗時の保持・旧世代退役を確認した。
依存方向の機械検査もすべて成功した。shaderのsource/binaryと、実行可能な固定操作の範囲は変えていない。

統合後のfmt、製品/開発の全target Clippy（`-D warnings`）、CPU通常67件、
`presentation,d3d12`のrelease通常183件、追加検証/Graph診断ありのrelease通常194件が成功した。
前節と同じrelease実GPU54件および製品/厳格の表示smokeが再び成功し、
resize/cancel/pending終了と最終native解放の既存診断判定を維持した。新規native警告/エラーはない。
Programのnative call site 38件とshader include 6件は不変。
WSIは53→52件で、唯一の差は不変limitsの再照会を既存Device cacheへ置換したことだった。
固定SDKのSPIR-V/DXIL再生成照合も成功した。

同じrelease CPU harnessで、compileのallocation 40,001回/7,125,062要求byteと、
frame構築18回/1,706,449要求byteが整理前後で一致した。
これは合成入力のallocation要求の確認で、実時間・GPU速度・Bistro性能の測定ではない。
Linux実GPU、別GPU/driver、Bistro全体、既知`renderer-0pb`の再現・解決は今回未実行である。

## Bistro計画と現行実装の区別

開発計画を[実装方針のB0〜B5](../design/implementation-policy.md#bistro-roadmap)へ改訂した。
追跡は`renderer-7ft`。`renderer-7ft.1.1`では利用者要件を合意し、
[確定要件と素材検証](../design/implementation-policy.md#bistro-requirements)を記録した。
対象は屋外全体、太陽光＋IBL＋影、PBR/HDR/tone mappingと利用側Bloom、基準1920×1080、
自由カメラと固定視点・自動巡回、両APIのWindows表示・操作、初回受入は既存RTX 3060である。
要件の合意を実装・実GPU受入の成功とは扱わない。
B0で維持・改修・置換・外部基盤を比較し、[native上位置換＋低レベル再利用](../design/implementation-policy.md#foundation-selection)を採用した。
旧M4の6/22完了は準備・Device・buffer実行の実績であり、新計画の進捗率へ流用しない。
旧M4の未着手分は後継課題を示して置換し、実装完了とは扱わない。

素材の取得・棚卸し・変換とCPU検証を下記へ記録した。`renderer-7ft.1.1.1`がこの作業の追跡先である。
全ゼロalphaに加え、最終camera/path/light/IBL/露出と再現・比較手順を下記のとおり固定した。
要求・比較入力の固定に続き、公開APIの利用例と基盤比較・採用・移行範囲を設計し、B0の設計を完了した。
B0ではRenderer本体・管理下shader・native経路は変更せず、以降の既存GPU・Rust試験表は従来の固定ソースの実績として保持する。
BistroのRenderer描画、独自Bloom、公開パスAPIの二API実証とD3D12表示は未実行である。
画像閾値の校正・性能予算・RTX 3060での全体収容も未実測であり、offline変換をその成功へ流用しない。

<a id="public-api-comparison"></a>

### 公開APIのUXと拡張契約の比較

`renderer-7ft.1.2`の[比較と第一案](../design/implementation-policy.md#public-api-comparison)を記録した。
構築時owned命令案Aとframe内借用callback案Bに同じ型付きpass dataを使い、記録時期と依存宣言の軸を分けた。
第一案はA＋binding／attachment由来のアクセスである。理由は実命令の確定境界と所有契約の単純さで、
CPU／GPU速度の優位を実測した判断ではない。基盤選定の`.1.3`でもAを維持した。B1の実Renderer二API実証は未完了である。

[Rust試作](../../crates/renderer/examples/graph_api_comparison.rs)はGPU非依存のCPU意図を比較する。
複数draw、depth-only／color-only、独自Compute、module内の多段Bloom、履歴初回／継続／resize、
将来のMRT／mip Viewを同じ入力で書き、ownerだけを正規化した命令とアクセス用途を照合する。
短命なdraw slice終了後のAの命令保持、Bのframe借用と不要callback非実行、
owner／範囲／bindingの不一致と宣言外アクセスの拒否を対象にする。
再現は[開発ガイド](../guides/development.md#public-api-comparison)を参照する。

Windows x64／Rust 1.99.0でworkspaceの既存CPU試験65件と新規9件が成功し、
新規9件はdebug／release双方で確認した。fmt、CPU全target Clippy（`-D warnings`）、release比較実行も成功した。
独立レビューの指摘を反映し、Loadのread用途、Bloomの抽出／blur／無効化、重複writeとcallbackの出力欠落を補った。

| 比較入力 | probeが保持したpass | CPU命令 | 資源用途 | Aのbuild／record callback | Bのbuild／record callback |
|---|---:|---:|---:|---:|---:|
| 履歴初回 | 12 | 22 | 24 | 13／0 | 0／12 |
| 同extentの履歴継続 | 12 | 22 | 25 | 13／0 | 0／12 |
| extent変更によるreset | 12 | 22 | 24 | 13／0 | 0／12 |

各入力で13passを登録し、未使用effectを1pass除去した。両案の命令・用途は各入力で一致した。
Aは除去前のbuild時にenum本文1,840 byteを構築し、Bはその時点で命令を構築しない。
Bにもpass dataとcaptureの保持があり、この値だけで総メモリや速度を比較しない。
文書のUTF-8／fenceと相対link／anchor、`git diff --check`を確認した。

試作の選択処理は画像単位の保守的な後方到達性probeであり、現Rendererのcompilerを実装し直すものではない。
範囲の局所検査と利用コードの一致を確認するが、mip単位のproducer／culling、初期化、
完全なRAW／WAR／WAW、native barrierを検証しない。mesh／programの識別、history import／exportも記号的表現で、
native参照の強い保持、canonicalな予定状態、revision、submit失敗・GPU完了の実証ではない。
件数・enum本文byteは構造的な観測であり、nested Vec、callback capture、allocator、catalog等を含む全保持量ではない。
shader生成・ABI・GPU内容・Windows表示・実GPU性能は今回未実行で、Renderer本体とshaderの実装を変更していない。
Vulkan指針の用途由来同期・記録から完了までの寿命と検証方針を設計へ適用し、API契約からの例外はない。

<a id="foundation-comparison"></a>

### 基盤の比較・採用と小試作

`renderer-7ft.1.3`の[採用理由・代案・工数・移行範囲](../design/implementation-policy.md#foundation-selection)を記録した。
固定操作の維持は独自パス要件を満たさず、全面native置換、safe wgpu、wgpu-hal直接利用と比較した。
採用はnativeの低レベル所有・同期・退役・WSIの再利用と、上位の一般owned命令／Program/bindingへの置換である。
safe wgpuのnative実装負担の小ささとRenderBundleによる事前検査を認めたうえで、
現AGENTSの保証済み内容の反復検査を製品経路から外す規約を維持する判断とした。
nativeの速度優位や工数実績を証明したものではない。B1〜B5に受入条件・依存を持つ17子課題を作成した。

[独立試作](../../tools/foundation-probe/src/main.rs)はwgpu 30.0.2を固定し、Rendererのruntimeへ接続しない。
短命draw Vecを即時closureからowned planへ移し、depth-only／color-only各2draw、
利用側WGSL Computeでcolor/depthを読む3passを8×4で記録した。
submit直後にplanと呼出側textureをDropし、実submissionとmap callbackの完了を待って128byteを照合した。
source／lockfile／shader／実行ファイルのSHA-256、原byte、adapter情報と4runを
[共有JSON](foundation-comparison.json)へ保存した。再現は[開発手順](../guides/development.md#foundation-comparison)を参照する。

| 実行要求（release、二backend逐次） | Vulkan | D3D12 |
|---|---|---|
| native flags空（bits 0） | 全128byte期待値一致 | 同左、Vulkanとも全byte一致 |
| DEBUG＋VALIDATION要求（bits 3） | 全128byte期待値一致 | 同左、Vulkanとも全byte一致 |
| pipelineなしdrawの負の対照 | 両flagsでcore Validation errorを返した。無効命令は未submit | 同左 |

Windows x64、Rust 1.99.0、NVIDIA GeForce RTX 3060、vendor 4318/device 9351、
Vulkan driver 610.88／D3D12 driver 32.0.16.1088を観測した。
vendor/device/nameの一致を確認したが、この試作ではLUIDを照合せず、同型複数adapterの同一性を証明しない。
有効fixtureのwgpu API Validation errorはゼロ。負の対照の期待errorと区別する。
depthの読取りが結果へ反映されるよう、出力blueは描画depth 0.25のUNORM値64を期待する。
depth drawを空にした一時変異ではclear値1.0からblue255となり、内容不一致で失敗することを確認して復元した。
native検証の実効、SyncVal/GBV、最終native解放後のwarning/errorは未観測でJSONのnullとし、
**この小試作を既存の厳格native受入の合格として数えない。**

全`Study::scenario`／多段Bloom、MRT/mip、履歴revision/reset、Windows表示・自由camera・resize/最小化・
pending終了、native失敗注入、HAL単独の安全性、全Bistro実行と性能・メモリは今回未実証である。
全代表入力の比較表はAPI写像の設計判断であり、この3passだけで同期・寿命の全契約を証明しない。
wgpuは独立workspaceの採用実験に限定し、Rendererの依存とnative実装を変更していない。
Vulkan指針の能力照会・用途由来同期・所有と実完了、検証方針の保証境界を選定へ適用した。
API契約・一般推奨・既存GPU受入からの新しい例外、vendor固有最適化、性能主張はない。

統合時にworkspaceのCPU試験65件、API比較9件のdebug/releaseと比較実行、fmt、CPU全target Clippyを確認した。
独立試作のfmt/check/全target Clippy/通常test（実GPU1件はignore）、release buildと上記4実行が成功した。
独立依存のcargo-deny 0.20.2はadvisories/bans/licenses/sourcesが成功。
hashbrown 0.16/0.17とsyn 2/3の重複warningを確認し、advisory除外は追加していない。
foldhash 0.2.0のZlibは独立試作の監査設定だけで個別許可し、Rendererの依存方針を拡張していない。

<a id="bistro-assets"></a>

### Bistro屋外素材の実体検証

入力・tool/function hash、全画像の対応、材質recipe、全meshの誤差とValidator原文は
[機械可読の現在データ](bistro-assets.json)、見た目は[比較画像](bistro-assets-preview.png)、
再現は[開発ガイド](../guides/development.md#bistro-preparation)を参照する。

原本ZIP・検証済み屋外glTF一式・環境HDRと上記比較PNGはGit LFSで共有する。
[素材一覧とライセンス](../../assets/bistro/README.md)、[全412ファイルのmanifest](../../assets/bistro/manifest.json)で
パス・byte数・SHA-256を固定した。固定条件PNGを含む実体は計1,775,343,744 byteで、既存の検証済み407ファイルもbyte一致する。
重複画像を共有するLFSオブジェクトは210個、計1,601,410,069 byteである。
原本・変換済み素材・従来の比較PNGは今回もbyteを変更していない。
ライセンス・検証JSON・manifestは通常Git、toolや作業中間物は引き続きGit管理外である。
既存410ファイルのLFS共有時には、originへ208/208オブジェクトの送信が成功し、ローカルキャッシュを共有しないdetached checkoutで
全410ポインタと空キャッシュを確認した後、originから全量取得して全ファイルのsize/SHA-256一致を確認した。
元checkoutと取得先の`git lfs fsck --objects --pointers`も成功した。
共有hooksは全6本を実行可能として追跡し、pre-pushのstdin保持・LFS失敗時停止と
`bd hooks install --beads`による全hookのbyte保持を確認した。
manifest・ポインタ・素材の変更時は取得境界で再照合する。Rendererのdraw/submitへ全量走査を追加していない。

| 確認 | 結果 |
|---|---|
| 公式入力 | `Bistro_v5_2.zip` 894,377,473 byte、SHA-256 `0d50e3c724c6c5da19f8eb99ad3f53e36fec37ffa2df9621f9ccf0603f3934e1`。FBXは`471991629c7e35a88338c48f3565390ad19cc5d1d7c7e067fc2a60b4f3b5f195`。同梱README/licenseもhash保存 |
| 独立FBX解析 | 1,296 Geometry/mesh Model、2,832,120三角形、132材質・405画像、欠落参照/名前衝突なし。有限値、index/normal/UV/material slot範囲と接続を確認 |
| 元FBXから取込結果への対応 | 全1,296 Model/Geometryの元float32位置・向き・raw corner normal・両UV・面ごとの材質slotが完全一致。428件の長い名前の短縮も追跡し、衝突なし。world行列の元FBXからの再導出はこの検査に含めず、取込後からglTFへの行列保持を別に確認 |
| 元の面と属性 | normal＋TextureUV/LightMapUV、tangent/colorなし。重複面2,894（逆向き980）、厳密な面積ゼロ19を保持。重複面のうち2,609は法線、765はUVが異なる |
| 変換・材質 | Blender 4.5.16 LTS build 3f6797307da5。通常の面削除とnormal精度低下を避ける限定profileを使用。132材質のfactor/alpha/両面/UV/チャンネル、405画像の変換画素を照合 |
| 独立glTF形状比較 | 全instance・三角形・向き・重複・材質・raw source normal・UV0/UV1を照合して成功。最大world位置差2.60e-5 m、normal成分差3.90e-7、UV差0。float32の絶対/相対許容値はデータに記録 |
| 形式検査 | Khronos Validator 2.0.0-dev.3.10、外部buffer/画像も検査、エラー0。TANGENT consumer生成の警告1,591、保持した未使用LightMapUVのinfo 1,591を抑制せず保存 |
| 再生成 | 元FBXを別Blender processで再取込し、glTF・buffer・全405画像、計407 file / 851,291,697 byteがすべてbyte一致 |
| CPU画像QA | 元FBXから明示PBR recipeを構成したsceneとglTF再importを同じ3視点・固定光・Cycles CPU 8 sample・960×540で比較。形状/材質の大きな欠落は見られず、RGB PSNR 34.8〜39.2 dB。完全一致やruntimeの合否閾値とは扱わない |

raw normalの圧縮表現を使うとBlender内ではゼロ/方向差が生じたため、glTFデータは保存したraw値を直接正規化して出力した。
CPU previewにはBlenderの再符号化・sample noise等の差が残り、屋根等の局所差も画像に残す。
glTF再import後のBlender終了ログには24 byteの未解放block報告が1件あり、tool診断として保持する。
Rendererのnative所有・GPU完了の受入を行った結果ではない。

**確定した素材解釈（`renderer-7ft.1.1.2`）:** `MASTER_Glass_Dirty_MASKED`は68面/13mesh、
`Paris_LiquorBottle_01_Labels`は154面/11meshで使われる。元alphaを保持し、不透明overrideは加えない。
両材質の面はopacity 0によって不可視で、shadowにも被覆を作らないことを後続実装の受入条件とする。
いずれも非退化面であり、mesh/materialを削除する決定ではない。
原本・glTF・全画像はbyteを変更せず、glassはMASK/cutoff 0.5、labelsはBLENDを維持する。
mask/blendの分類と、ORMのないラベル材質のnonmetal/roughness 0.5も明示した変換方針である。

元DDSのDXT5 alphaを独立に全mip復号し、glassの12段・labelsの5段で全sample 0を確認した。
既存PNGへのbase mip変換もRGBA一致し、labelsはRGBも全0である。不透明化でラベル画は復元されない。
同梱READMEの不透明度規約・屋外pysceneのoverride不在と、Falcor 4.0.1のBC3読込・alpha付きtextureのmask分類・
texture alphaによる除外を照合した。4.0-previewと4.4でも整合を確認したが、配布物はexact Falcor commitを指定していない。
現行labelsのBLENDとFalcorのMASKを一般に等価とは扱わず、固定したalpha 0での被覆0が整合するという判断である。
[確定方針と代替案](../design/implementation-policy.md#bistro-alpha)、[入力・一次ソース・計測](bistro-alpha.json)、
[alpha保持／不透明化／差分／対象coverageの比較画像](bistro-alpha-preview.png)を保存する。
比較は全sceneのmesh・材質を保持し、対象材質だけを変更するCycles CPU診断である。
1,296 mesh・2,832,120面を保持し、32 sample・seed 0・960×540、固定camera/lightで6画像を生成した。
瓶の近景には全条件共通の固定area lightを追加し、暗所での見落としを避けた。
不透明copyの対象coverageは窓97,308／瓶164,497画素で、その領域内のRGB差>2は59,855／126,290画素だった。
値は低sampleのnoise・間接光変化も含む診断結果であり、runtimeの合否閾値へ使わない。
このalpha調査で作者の意図・Falcor実行・Renderer実GPU画像/性能・最終camera/lightの受入は確認していない。
構図・照明入力の選定は下記の別CPUプレビューで確認し、実GPU受入は後続に残す。
取込/cook境界の保証と変更時の再検証に限定し、draw/submitへ検査や診断payloadを追加していない。
alpha追加分を含むPython 41件と既存Node 5件が成功した。Nodeのsymlink試験はsandbox内のEPERM後、
通常権限で5件すべてを再実行して成功した。Rust/native実装変更がないためローカル実GPU試験は今回対象外である。
文書5件のUTF-8/fence・相対link/anchor 107件、全411 payloadのsize/SHA-256、
alpha判断を含む素材reportの再生成と`git diff --check`も成功した。独立レビューの指摘を反映し、
glTFで省略されたMASK cutoffの実効既定値0.5を判断データへ記録した。

元にないTANGENTはこのglTFへ生成収録していない。標準生成を試した別条件ではzero tangent 345個が形式エラーとなったため、
MikkTSpace生成と退化入力fallback・実行用mipをB2の取込/cook要件へ記録した。
PNGはbase mipだけで、元DDSとmip情報は保持する。これらの警告はnative API検証ゼロ方針の例外ではない。
素材はAmazon Lumberyard Bistro / NVIDIA ORCA / [CC-BY 4.0](https://creativecommons.org/licenses/by/4.0/)。
配布元は[公式ページ](https://developer.nvidia.com/orca/amazon-lumberyard-bistro)であり、画像は本検証で変換・描画した。

検証ツールのローカルCPU試験はPython 34件・Node 5件成功。独立レビューで材質検査のAO用UV/transformと
予期しない発光の検出、重複三角形の対応、元FBXから正規化normalへの照合を補強し、回帰試験を追加した。
文書4件のUTF-8/fence・相対link/anchor 79件・既存policyへのbacklinkと`git diff --check`を確認した。
LFS共有追加では文書7件のUTF-8/fence・相対link/anchor 106件も確認した。
通常CIへ小fixtureの`asset-tools`を追加した。ローカルのRust/native GPU再実行はRenderer実装変更がないため対象外とし、
pushした組合せのbuild/testはCIで別に確認する。

<a id="bistro-conditions"></a>

### Bistroの描画・比較条件

`renderer-7ft.1.1`の固定入力は[conditions.json](../../tools/bistro/conditions.json)、
判断と再現条件は[実装方針](../design/implementation-policy.md#bistro-conditions)、
手順は[開発ガイド](../guides/development.md#bistro-conditions)にまとめた。
[機械可読の実行記録](bistro-conditions.json)と[CPUプレビュー](bistro-conditions-preview.png)に、
入力/設定/tool/画像のhash、全scene保持、照明候補と未実行の範囲を記録する。

固定7視点（全景・路地・材質・glass・label・巡回中間・折返し）と、
30秒/60 Hz/1800 frameの往復巡回を指定した。基準は1920×1080、near 0.02/far 500 m、
右手Y-up/m・縦FOV・論理上原点の画像・通常Zの0〜1とし、native変換は同じrayを保つ。
cameraの自動露出・jitter・DOF・motion blurは使わない。設定の有限値・参照・視線の補間特異点、
閉路と半開frame列、入力4件のsize/SHA、HDR方向のcardinal/逆yawをCPUで検証した。

同梱HDRの原本を保持し、元texelのRec.709輝度を4へcapして高輝度源を抑え、別太陽を加える。
輝度100超の213 texelが全周の立体角加重輝度積分の44.902%を占め、cap 4で変更するtexelは68,105件だった。
この処理は太陽の完全な分離でも照度の物理校正でもない。未処理IBL＋太陽、IBLのみとの比較を保存する。
cameraに見える背景は一定の線形RGB .18とし、環境内の別建築をBistroのgeometryに見せない。
IBLは照明・反射へ残す。固定露出EV +1、commit固定のKhronos PBR NeutralとsRGB一回で出力する。

CPU画像はBlender 4.5.16 LTS build `3f6797307da5`、Cycles CPU・960×540・32 sample・seed 0、
adaptiveなし、限定したbounce数、CPU OpenImageDenoiseの表示用画像で構図・露出を判断した。
全1,296 mesh・132材質・2,832,120三角形を保持し、元glTF/buffer/textureのbyteは変更していない。
全景は全mesh境界を各辺5%以上の余白でfrustum内へ収めた。巡回のeye経路4 segmentと
全1800 frame×5本のnear-plane rayで侵入0を確認するが、連続したfrustum体積や歩行可能性の証明ではない。
noise除去前/後のscene-linear 32-bit EXRとPNGを生成し、CSM/PBR/IBL前処理/Bloom/AAの実GPU正解画像とは扱わない。
太陽は仰角30度/RGB 1、IBLはcap 4/強度3、露出はEV +1とした。
瓶の近景はfillを追加しておらず暗いため、alpha被覆を色画像だけで受け入れない。

画像閾値は同API反復3回以上・二API差・既知の誤りへの検出感度で受入前に校正する手順とし、
性能はcold準備、1800 frame×2 warm-up＋×3計測、生sampleとphase別p50/p95を記録する手順を固定した。
数値閾値・FPS/VRAM予算は未校正で、B2/B3/B4/B5の実装・RTX 3060実GPU受入に残る。
alpha 0のcolor/depth/shadow被覆0、二APIの表示/操作/resize/終了と破棄後の厳格native検証も未実行である。
near .02/far 500 mの通常Zにおける遠方のdepth精度も未検証であり、B1/B3の実GPUで確認し、必要なら条件を改訂する。
Python 57件・Node 5件、文書5件のUTF-8/fence・相対link/anchor 124件、全412 payloadのsize/SHA、
固定条件を含む素材reportの再生成とdiff checkを確認した。改行変換による記録hashの不一致を防ぐため、
条件JSONの元byteと新規tool/検証JSONのLFを属性へ固定し、既存alpha reportの参照SHAもGit blobに一致させた。
Rust/native変更がないため、ローカル実GPU試験は今回対象外である。
Rust/native/shader・製品版の検証経路は変更せず、入力走査をdraw/submitへ追加していない。

## 環境と対応範囲

| 項目 | 確認値 |
|---|---|
| OS / target | Windows x64 / `x86_64-pc-windows-msvc` |
| CPU | Intel Core i5-14600KF |
| Rust / Cargo | 1.99.0 / 1.99.0 |
| GPU / driver | NVIDIA GeForce RTX 3060 / NVIDIA 610.88 |
| Vulkan SDK / Loader | 1.4.341.1 / 1.4.341 |
| Instance / VMA要求API | Vulkan 1.3 |
| Queue | family 0、index 0、Graphics＋Compute、表示時は同じqueueのpresent対応も照会 |
| bindings / allocator | ash 0.38.0+1.3.281、vk-mem 0.5.0、同梱VMA 3.3.0 |
| Windows表示依存 | winit 0.30.13、ash-window 0.13.0、raw-window-handle 0.6.2 |
| D3D12 | Windows限定、private bindings 0.100.0、単一DIRECT queue、committed buffer、legacy barrier |

CPUのGraph・engine入力はGPU・windowなしで構築できる。
表示はWindows、EXT swapchain maintenance1と依存拡張・feature、
同一Graphics＋Compute＋present queue、SRGB_NONLINEARのRGBA8/BGRA8_SRGB、OPAQUE対応に限定する。
実機で必要能力を照会した。固定bindingsにあるEXT版を利用し、headlessの必要能力は増やしていない。
別queue、拡張なしfallback、透明Window、HDR、任意shaderは対応したと扱わない。

## 品質ゲート

| 確認 | 結果 |
|---|---|
| fmt | 成功 |
| CPU / Vulkan / Windows presentation / D3D12 / Vulkan＋D3D12の全target check・Clippy `-D warnings` | 各backendの製品／開発＋dump、計10構成成功 |
| CPUのみの通常試験 | 両構成とも単体53件＋公開API 9件＋負荷fixture 3件成功 |
| Vulkan featureの通常試験 | 製品145／開発157単体＋公開API 9件＋負荷fixture 4件成功、45／44 ignored |
| presentation featureの通常試験 | 製品150／開発162単体＋公開API 9件＋負荷fixture 4件成功、46／45 ignored |
| D3D12 featureの通常試験 | 製品63／開発62単体＋公開API 9件＋負荷fixture 3件成功、2／3 ignored |
| Vulkan＋D3D12 featureの通常試験 | 製品155／開発166単体＋公開API 9件＋負荷fixture 4件成功、各48 ignored（公開二API受入と負荷fixtureを含む） |
| 最適化・独立feature | releaseのVulkan／D3D12／併用、各製品・開発構成のCPU試験成功。各backendのnative診断だけ／Graph診断だけ／開発検証・dumpなしのcheck、受入workerの併用5構成Clippy成功 |
| native検証ありの明示GPU試験 | release、実Win32 Surfaceを含むVulkan 43件を再実行して成功、逐次実行 |
| 製品経路の明示GPU試験 | release・診断featureなし・native Disabledのmesh画素照合1件成功。CPU upload Arc解放・provenance保持0・index走査0も確認 |
| D3D12 runtime | releaseの厳格検証／製品構成各1件成功。共有所有、設定不一致拒否、終了と同一process内再作成を確認 |
| D3D12 buffer寿命 | release厳格検証1件成功。未投入キャンセル、作成途中の注入失敗、逆順待機、runtime先行終了、pending Drop、空Graphの最終解放順を確認 |
| 二API共通buffer Graph | clean `2b15cd0`でrelease厳格検証3回＋製品3回成功。同じ10-pass Graph・同じGPUで各308 byteが全一致 |
| D3D12準備の実機回帰 | 既存3回の準備受入に加え、clean `2b15cd0`で反復scriptを1回再実行して成功。固定shader再生成・同一GPU・厳格診断を確認 |
| Validation / SyncVal | 実効設定を確認し、native所有者の破棄後までwarning・error・callback失敗ゼロ |
| Shader再生成検証 | 固定SDKの5種類のSPIR-V検証・再生成一致に加え、HLSL・検証済みDXIL・reflection・manifestの再生成一致 |
| headless例／frame loop例 | 既存のPPM・24-frame照合結果を継承。今回CLI本体の再実行はなく、同じ操作・寿命はGPU試験と下記負荷でも確認 |
| engine負荷 | releaseの開発検証あり45画像／製品519画像が全RGBA一致。大mesh、47 chunk upload、更新・削除、複数View、3 in-flightを確認 |
| engine Windows表示例 | releaseの開発検証ありでVSync ON/OFF各起動、製品構成でも`windowed --smoke`成功。各12 Present要求、VSync往復・resize・最小化復帰・pending終了 |
| 表示例の引数検査 | VSync既定ON、ON/OFF指定、smoke併用、不正引数拒否のCPU試験1件成功 |
| exampleのCPU試験 | frame計測9件、大メッシュfixture・統計4件、windowed引数1件成功 |
| 依存監査 | 今回は依存追加・lockfile変更なし。cargo-denyの既存成功結果を継承し、CIでもworkspace・独立generatorを再確認する |
| Rustdoc | 製品／開発＋dump構成で`RUSTDOCFLAGS=-D warnings`成功 |
| 文書 | UTF-8、相対リンク・anchor、fenced block、差分空白を確認 |

実GPUはignored試験を明示実行し、前提不足を成功・skipへ置き換えていない。
CPU fallbackの成功で実GPU受入を代替しない。
今回のM4-03/04統合確認のソースは`2b15cd0bc7ca68892f433c1a67d5033278f7252f`、実行時のmainはcleanである。
品質確認の全コマンド・終了値は`.tools/m4/runtime-buffer/gates/summary.json`、
実GPUの逐次実行は同`gpu/summary.json`へ保存した。既存engine負荷・表示smokeの数値は各節の固定ソース結果を継承する。
入力拒否を確認するアプリケーションERRORログはValidationの異常とは区別する。
最終品質ゲートでは`CARGO_INCREMENTAL=0`を使い、初期の並行確認で見られた
hard-link不可というincremental cacheの環境warningをソース診断と区別した。

Windows / LinuxのCPU・Vulkan、およびWindows presentationのcheck・Clippy・CPU試験を
[GitHub Actions](https://github.com/tsukushibito/renderer/actions/workflows/ci.yml)に設定している。
このローカル確認はLinux実行や実GPU CIの合格を意味しない。

### 製品版と開発用の検証分離

ライブラリの最終確認点は`e96d270`、表示例の追補は`aa5f1eb`。
`renderer-y7e`／`fa3`／`585`／`vmj`／`kyt`／`jp2`を統合した。
profileと独立した`development-validation`、`vulkan-diagnostics`、`graph-diagnostics`を実装した。
製品の既定native modeはDisabledで、診断なしのRequired要求は明示的に拒否する。

Graph構築が保証したowner・範囲・用途の独立再検査とdead部分だけの循環検出を開発用へ移した。
producer・初期化・culling・live hazard／循環・最終内容・import revisionの保証は維持する。
管理下meshは生成時の不変count／最大値をprivate uploadへ引き継ぎ、一般writeで失効する。
位置を保つ完全copyだけが継承し、証明できない部分範囲は拒否する。
任意Graphの部分byte更新には入力保証としてprovenanceを残す。
同count・交互の頂点数の32 drawで、一般入力の走査は旧32回から1回、管理下meshは0回となるCPU回帰を確認した。

native診断offのmonitor／内部handleはZSTで、診断専用Arc・callback・履歴・debug_utils・命名／label構築を持たない。
Graph診断offはdump API・専用依存／寿命metadata・culled名・未使用transient名を除外する。
live／export／importの運用エラー用の名前は残す。通常のreleaseライブラリを使う外部allocation probeで、
1024資源の保持量を447,488→352,256 byte（全culled）、899,072→774,144 byte（2048 live pass）と確認した。
開発検証のon/offでは変わらず、Graph診断だけが保持を追加する。各caseでplan破棄後の収支は0。
これはallocatorへの要求byteで、overhead・stack・process RSS・VRAMではない。

release LLVM IRでもlowering dump固有の4文字列が製品構成に存在せず、Graph診断ありのpositive controlには存在した。
operational log用のtracingは残る。同期・完了・容量・native失敗を検証用cfgへ移していない。
最適化＋検証あり／なしのCPUと実GPUの両経路を確認した。

trim_idle_resourcesは明示的なworking set境界で、登録中・pending・escaped leaseを保護する。
実GPUでidle画像2個を解放し、VMA allocationは7→5件、9,888→8,864 byte、cache論理byteは320→0。
登録中64 byteとretained 132 byteは維持し、全owner退役後はstagingの2件／8,192 byteへ戻った。
VMA blockは100,663,296 byteのままなので、process VRAM削減やbyte budgetの保証とはしない。
世代上限・古いkey・失敗後の空きslot再利用も確認し、hidden waitを追加していない。

Rasterは名前付きdraw／typed attachmentとClear(value)へ移行し、未使用clear入力を除去した。
解析専用declare_passの競合拒否は安全な構築境界に残し、共有O(A log A)の検査へ変更した。
全read 8192宣言は線形・一時allocationなしで、元のbuilderの最初の衝突pair／error contextを保つ。
資源所有・準備・記録を既存private moduleへ分離し、独立レビューでも契約の欠落は見つからなかった。

native検証ありのrelease GPU43件はCore／SyncValの実効を確認し、native破棄後までwarning・error・callback失敗ゼロ。
別runの製品mesh試験は画素とCPU upload Arcの解放、保持segment・走査ゼロを確認した。
製品monitorは収集しないため、件数0や起動報告の不在を外部Layer非注入の証明にしない。

### VSyncと表示例

既定ONはFIFO、OFFは実Surface対応を確認したIMMEDIATEで、未対応要求はUnsupportedとなる。
同値変更、acquire／present、resize、最小化復帰、取消とpending終了を実Surface試験で再確認した。
表示例は開発構成のVSync ON/OFF各起動と製品構成の3回、releaseのsmokeが終了コード0。
開発構成はCore＋SyncValの実効と終了後の診断ゼロを要求し、製品構成はmonitor処理を持たない。
実際のVキー手動入力、別GPU／driver、driver／compositor設定、表示FPSやPresentMonは未確認である。

## 動作・寿命・異常経路

| 領域 | 確認した契約 |
|---|---|
| Graph解析 | byte範囲ごとのproducer、RAW/WAR/WAW、生存解析、循環、未初期化の穴、Attachment load/store/discard、dead宣言も含む検査、安定dump |
| native同期・内容 | upload/copy→vertex/index、color/depth→Compute→sampled composite→readback、端数dispatch、全byte/画素照合、用途別Programの保持、初回画像遷移の既存driver対策 |
| import/export | pending producerをCPU waitせずconsumerへ引継ぎ、owner/generation/内容revision、read-onlyと表示は内容revisionを維持、投入時の同期rebase、書き込み後の旧内容拒否、削除と再利用 |
| frame進行 | metadata Dropは非待機、Runtimeのbounded pending、投入前capacity拒否、単件poll／結果を返すwait、共有timeout、明示drain、native完了とreadbackのCPU寿命の分離 |
| 再利用 | 3 in-flight×4 roundで未読24 readback leaseを保持して再利用、warm-up後の作成数はresource 9 / execution 3 / descriptor arena 3で安定、結果byte不変 |
| 容量・削除 | pending中の削除と旧key拒否、別entry/世代、size切替、4096-byte readback容量の不足と結果消費後の回復、既存の未投入失敗回収 |
| history | 18 frameの連鎖＋read-only利用、初回・同extent reset・5×3へのresize、3件連続のhost未待機submit、pending consumer中reset、CPU参照との全byte照合 |
| engine入力 | owner/世代付きmesh/material CRUD、immutable snapshot、旧snapshotの独立利用、View/model/tintの画素変化、empty draw、実index上限、GPU pending中の更新・削除 |
| 準備と投入 | upload準備とbuildを分離、同一snapshotを独立rendererへ入力、2 View先行build、旧版と新版の独立常駐、foreign prepared Graphの拒否、pin中の退避保護 |
| custom pass | crate外のconsumerによる標準Graphへの処理追加、同寸linear RGBA8の契約、foreign handle/format/extent/未初期化の拒否、不要branchの除去 |
| 計測query | 通常off、pending lease保持、disable後の受理済みsample回収、完了後即再利用、readbackの遅延消費、有効bit幅・wrap・availability |
| WSI | 実Win32 Surfaceで18表示、各frameのheadless画素照合、resize/連続resize、ゼロextent/最小化復帰、取得後cancel、pending close、Windowの保持と解放 |
| 失敗状態 | CPU境界試験でsubmit OOMの状態不変、timeout/不確実失敗の保持、Device loss、presentのenqueue区分、SUBOPTIMAL、上限なし画像数等を確認 |

追加のupload境界試験では、Fence完了後の結果取得エラーだけなら退役済みentryを回収して次へ進み、
未完了Fenceエラー・timeoutでは所有を維持することを確認した。実driverへのquery OOM注入とは区別する。
presentの`OUT_OF_DATE`は`RecreateRequired`で、受理された`Presented`とは別にするCPU回帰を追加した。

未完了所有の保証は「GPUが遅いはず」という時間の推測に置かず、CPU状態試験と
実際の複数未待機submit・内容照合で分けて確認した。
WSI実試験は主に正常/resize/cancel経路であり、native OOM・Device loss・Surface lossを
実driverへ発生させた試験とは区別する。

frame/metrics、engine/custom、WSIを独立レビューした。
CPU retireにnative実行枠のreset費用が含まれない点、release失敗後の再release、
terminal pending残留による回収停止は修正し、最終差分を再確認した。
追加の確定した同期・早期解放問題は見つかっていない。これは未確認環境までの保証ではない。

push後のLinux CIで、Instance拡張名をpointerの値で整列する実装がOSごとに異なる順序を返し、
既存lifecycle試験が失敗した。C文字列の内容による整列・重複除去へ修正し、
別heapの同名文字列を使う回帰試験を追加した（`renderer-u4h.14`）。
修正後のVulkan／presentation全target check・Clippy・CPU試験と実GPU26件を再実行して成功した。

## 採用した基準と制限

既存ExecutionPool・staging・画像/View・descriptorを活用し、frameごとの新設poolや
全面idleを導入していない。通常frameはpollで回収し、Dropの待機に依存しない。
単一queue、物理aliasingなし、通常descriptor set、管理下shaderという基準を維持する。

表示はGraphと同じqueueへの別submitとし、Graph解析にSwapchain所有を持ち込まない。
sourceのcanonical状態とGPU leaseを共有する。submit追加のCPU費用・latencyを最適化済みとは主張しない。
EXT maintenance1のpresent fenceと通常GPU参照の両方を回収根拠に使い、描画Fenceだけでpresent終了を推定しない。
旧Swapchain作成失敗、enqueue済みpresent error、取得後cancelの状態を区別する。
キャンセル等の例外回収には10秒のAPI待機予算があり、通常frameの待機とは分ける。
完了やrelease結果を証明できなければ停止し、依存するnative所有連鎖を保持する。

ViewのABIはcolumn-major MVP 64 byte＋linear tint 16 byte。従来rasterはidentityで互換を維持する。
独自shaderの将来追加にはpipeline/binding metadata/ABI/信頼境界の検証が必要で、
M3のGraph構築拡張をその完了として扱わない。一般の公開パスAPIは未実装であり、新計画のB0で設計しB1で二API実行を検証する。

## 計測経路と基準値

以下の`752e785`は従来の128×96基準負荷の保存値で、現行APIの再計測値ではない。
現行schema3のCPU各phaseと待機の分離は開発ガイド、大メッシュの測定値は後述のエンジン負荷節を参照する。

`frame_loop --benchmark`にrelease・明示Validation設定・任意query・raw JSON保存を実装した。
対象は128×96、3 in-flight、標準描画・custom SwapRedBlue・1×1 history・毎frame readbackのheadless Graph。
warm-up 32 frame＋測定128 frameを3 run行う。表示/VSync/FPSの測定ではない。
CPU build/compile/prepare/record/submit/retire、GPU Graph区間、poll費用と観測latency、
論理byte、staging予約high-water、VMA Allocation/blockを分ける。
process VRAM/RAMやdriver/Swapchainの確保はこのVMA値に含めず、process budgetは未取得とする。

実装commit `752e7859cb55d50fd270dc7d74b67274d8c191a6`、worktree cleanで3 runを実行した。
後続の拡張名集約修正はcold初期化とCPU回帰試験だけを変更し、測定対象のframe経路・shaderは変更していない。
[計測JSON](current-metrics.json)に環境、要求/観測Validation、各runの統計、全384 sampleを保存する。
全sampleでGPU timestampは64 valid bits、1 ns/tick、availabilityを確認できた。
次の範囲は各runの中央値とnearest-rank p95の最小〜最大で、単位はµs。

| 区間 | runごとの中央値の範囲 | runごとのp95の範囲 |
|---|---:|---:|
| CPU build | 1.30–1.60 | 1.50–2.00 |
| CPU compile | 9.40–10.90 | 10.10–12.40 |
| CPU prepare | 2.40–2.90 | 2.60–3.80 |
| CPU record | 8.40–10.35 | 8.90–15.90 |
| CPU submit | 7.40–8.00 | 8.60–10.00 |
| CPU retire | 1.00–1.10 | 1.10–1.40 |
| 完了を返したpoll全体 | 1.50–1.70 | 1.70–2.10 |
| submit returnから完了観測 | 147.35–187.65 | 153.70–259.20 |
| GPU Graph区間 | 22.080–22.304 | 22.496–22.752 |

phase別中央値の和をframe全体の中央値として扱わない。観測latencyはpoll頻度と他frameのCPU作業にも依存する。
run 1にはCPU compile最大89.9 µs、record最大83.8 µsの外れ値があり、全sampleとrun間変動を残した。
測定loop時間はwarm-upのin-flight tail、最後の回収とCPU内容照合も含み、GPU区間とは異なる。
clock・affinity・外部負荷は固定しておらず、この小さな基準負荷を代表sceneの性能や表示FPSへ一般化しない。

3 runともwarm-up後のnative作成数はresource 18 / execution 3 / descriptor arena 3で増加なし。
各runの160 frameでresource再利用785回、execution/descriptor再利用各157回を確認した。
frame回収後・永続meshとhistoryをまだ登録している時点のメモリは次のとおり。

| 指標 | 3 runで共通の値 |
|---|---:|
| 登録済み論理byte | 100 |
| pending等で保持中の未登録論理byte | 0 |
| 再利用cacheの論理byte | 589,836 |
| VMA Allocation | 20件、9,177,200 byte |
| VMA確保block | 3件、100,663,296 byte（96 MiB） |
| upload予約high-water | 256 byte |
| readback予約high-water | 196,864 byte |
| 回収後upload / readbackのlive range | 両方0 |

正しさと同じbyte/画素照合を性能runでも実施し、native破棄後の診断件数はゼロだった。
正しさのRequired runではCore/SyncValの実効値と終了後診断を確認した。
性能用Disabled要求でLayer起動報告がない場合も、外部Layerが絶対にない証明とは扱わず、
要求・観測値・環境変数をJSONへ残した。今回の性能runは起動報告なし、実効値は未確認のnullである。

## RenderGraphのAPIとcache改善

`renderer-lup`でowner検査付きの`Graph::image_desc`を追加し、custom処理の幅・高さの別渡しを除去した。
`renderer-bcf`でGraphErrorの8分類、pass/resource・名前・範囲、実際の有向循環のcontextを追加した。
CPUのみで一時/import画像とdepth、foreign handle、未初期化の穴とexport、同名passと循環の後続tail、
構築前のpass名、customエラーを確認した。旧struct literalは`GraphError::custom`へ移行した。
互換性維持を既定の制約にしない方針は`AGENTS.md`に明記した。

`renderer-vxf`は同じdescriptorの再利用を優先し、件数上限までは異なる条件も保持する。
上限時の退避は最古の受理serial・同値ならindex順で、登録中・pending lease・世代上限を保護する。
追加のstrict GPU回帰4件で、2×2/3×3画像の交互8回が**新規2・再利用6**になること、
容量圧力、世代、未観測pending、置換先の命名失敗後の旧allocation維持と再利用を確認した。
命名失敗はnative image/view作成後の部分失敗であり、実driverのOOM注入試験ではない。

cacheの性能比較は変更前`55f94ee`と、cache修正`27fbd83`相当のライブラリを静的に組み込んだ同じprobeで行った。
release・Disabled要求・1 execution slot・容量8、64×64固定／64×64と96×96交互、
warm-up 32＋測定128を各3 run。各submit後に明示Fence待機し、全byteを照合する制御されたcache試験で、
継続frameのthroughputとは異なる。CPU activeはbuild〜submit復帰で、Fence待機を含まない。
before→after順に測り、clock/affinity/電力/OS負荷は固定していない。

| 交互サイズの指標 | 変更前 | 変更後 |
|---|---:|---:|
| CPU activeのrun中央値 | 25.90–26.80 µs | 15.00–16.10 µs |
| CPU prepareのrun中央値 | 5.90–6.25 µs | 1.70–1.80 µs |
| warm-upを含む160回の新規作成／再利用 | 160 / 0 | 2 / 158 |
| 終了時cacheの論理byte | 36,864 | 53,248 |
| VMA Allocation件数／byte | 3 / 8,437,760 | 4 / 8,454,144 |
| VMA block件数／byte | 3 / 100,663,296 | 3 / 100,663,296 |

固定サイズcontrolのCPU active中央値は変更前14.80–16.10 µs、変更後15.30–16.35 µsで範囲が重なる。
交互サイズのGPU Graph中央値も変更前6.368–6.640 µs、変更後6.320–6.448 µsで、GPU高速化は主張しない。
再利用と引き換えに保持allocationは増える。block容量が今回増えなかったことを、VRAM budgetや全負荷の保証にしない。

### Draw listの本実装と比較計測

**標準の不透明描画はbatch経路を既定としてmainへ統合した。**
非空draw listは一つのGraph Raster操作とnative描画区間にし、color/depthを一度clearする。
draw順・MVP/tint・各drawのindex/provenance検査・最終Storeを維持し、空listはclear-onlyとする。
単一drawの`Graph::raster`は参照経路として残し、公開PassEncoder・任意shader・汎用パス統合は追加していない。

同じmeshだけでなく、多数の異なるmeshに対応するため、Graph importのcanonical索引とnative用途集約を
順序付きmapにした。compile側のパス内競合検査はwriter列を使う検索へ変更し、入力全組合せの走査を除去した。
CPUで4096通りの独立pairwise参照との比較、8192入力、重複read、半開区間、失敗contextを確認した。
さらにRasterノード数、128種類のmesh producer、構築失敗とimport同一性を検査した。
strict GPU試験ではscalarとの全RGBA一致、空draw、逆順、同深度、異なるmesh/行列/tint、後続Compute/composite、
後半drawの不正index、同じindex bufferで頂点範囲が異なるdraw、共有Vertex/Indexのscope合成、
96 draw・3 meshのpending中更新/削除と所有の維持を確認した。独立static reviewで追加の修正指摘はなかった。

比較sourceはscalar `bf0d61c`（`776cbab`＋distinct計測ハーネス）と本実装`97d41cc`。
実装のRustコードを変更せず、両方cleanな状態でreleaseを作り、実行時のsource commitと実行ファイルhashを記録した。
共有1 meshとdrawごとに独立したmesh資源を使うdistinctの両方で、同じ3頂点/3 indexの小さい三角形を描く。
draw数0/1/8/32/128/512/1024、overlap 64×64とgrid 512×512で、計28負荷を使った。
history/custom/churnなし、永続mesh、3 in-flight、Disabled要求、warm-up 32＋測定128 frameを各3 run。
各負荷でscalar→batch、batch→scalar、scalar→batchの順に単独実行した。
全168 run・21504測定frameでGPU timestamp availableとwarm-up後のnative作成数安定を確認した。
別のRequired/Core/SyncVal有効56 runでは各8 frameを照合し、28負荷すべての最終RGBAが全byte一致した。
測定frameのchecksumも配置/draw数ごとに両実装・mesh条件・全runで同一だった。

以下はrunごとの中央値の範囲。単位はµs。CPUはpollを除くbuild〜submit復帰の直接計測、
GPUはcompositeとreadbackを含むGraph全体で、draw数自体の削減やinstancingは行っていない。

| 負荷 | CPU scalar | CPU batch | GPU scalar | GPU batch |
|---|---:|---:|---:|---:|
| shared grid・8 draw | 56.75–59.65 | 36.70–38.65 | 70.304–70.336 | 54.400–54.496 |
| shared grid・128 draw | 394.20–423.25 | 51.50–52.45 | 314.592–315.408 | 56.128–56.144 |
| shared grid・1024 draw | 3303.05–3387.45 | 141.90–214.80 | 1914.976–1917.072 | 68.512–68.544 |
| distinct grid・128 draw | 679.05–736.00 | 381.45–408.00 | 315.056–315.136 | 55.840–55.872 |
| distinct grid・1024 draw | 6814.90–6977.00 | 3349.95–3521.70 | 2156.848–2171.040 | 71.232–71.280 |
| distinct overlap・1024 draw | 6720.30–6874.55 | 3332.35–3389.15 | 1636.272–1848.432 | 36.048–36.112 |

gridの8 draw以上ではshared/distinctとも全runでCPU中央値・p95とGPUが改善した。
distinctではCPU側に多数の資源のsnapshot・検証・命令準備の負担が残り、sharedと同じCPU時間にはならない。
1024 distinctの物理entry容量は2057、warm-up後の作成数も2057で安定した。
最後の統計はmeshを登録中に取得しており、登録済み論理byteはshared 96、distinct 98,304。
その後に全meshを削除し、native所有者の破棄後まで診断ゼロを確認した。

小規模sceneのCPU tailの変動は引き続き残す。
通常条件のshared grid 0 drawはscalarの中央値34.15–35.85 µs／p95 46.10–69.20 µsに対し、
batchは中央値33.25–55.00 µs／p95 43.60–214.50 µsだった。overlap 0 drawでも遅いrunが双方に現れた。
0 drawはどちらもclear/composite/readbackへ進むが、変動の原因をschedulerと断定していない。
別条件の追加controlでは子processを同じ論理CPU（affinity mask 1）へ固定し、
sharedの0/1 draw・両配置をwarm-up 64＋測定512 frame、各3 runの交互順で測った。
overlap 0 drawの中央値はscalar 15.90–16.50 µs、batch 16.20–16.40 µs、
p95は19.10–23.00 µsと19.10–21.20 µsだった。
grid 0 drawの中央値は32.80–34.90 µsと32.55–32.90 µs、p95は40.80–63.60 µsと41.40–75.80 µsだった。
1 drawは両配置の全runでbatch中央値が小さかったが、p95の変動は残り、通常desktopでの非回帰は保証しない。

利益はGraph粒度、import/競合/用途検索、barrier、描画scope、状態設定を合わせた変更の結果で、
begin/end削減だけへ帰属しない。CPU readback照合とchecksumもloop時間へ影響するため、FPS倍率とは呼ばない。
distinctも同じ小さいgeometryの独立資源であり、重いmesh、別GPU/vendor、tile GPU、任意shaderの性能は未確認。
[比較JSON](rendergraph-metrics.json)のschema 2に環境・source/実行ファイルhash、全runの統計・生sample、
保持メモリ、affinity control、RGBAのSHA-256を保存した。以前の試作測定はGit履歴に残す。
cache単独変更時の比較は同じJSON内に元のsourceと条件で保持する。
性能runの実効Core/SyncValは未観測のnullであり、Disabled要求と外部Layerが絶対にない証明は区別する。

### 初回GPU実行のnative異常

前回の`ba91f3b`での最初のpresentation全GPU実行は、buffer試験後のDevice smoke開始付近で`0xc0000005`により終了した。
Windows Application Errorのmoduleはunknown、offsetは`0xcb`で、直前のValidation警告・エラーはなかった。
同時期に試作側のGPU suiteも実行していたが、並行実行を直接原因とは断定していない。
Device単独、その後のpresentation全30件を2 run、headless全29件、上記比較と例の単独実行では再現しなかった。
失敗ログを保存し、原因未確定の観測を`renderer-0pb`で追跡する。driver原因や解決済みとは扱わない。
batch本実装`97d41cc`のheadless 33件・表示込み34件、56比較runと各例でも再現しなかった。
今回の`2e543b4`のheadless 40件・表示込み41件・公開負荷1件、release検証・計測と各例も単独実行し、再現していない。

## エンジン負荷と入力・資源管理

最終ライブラリソースe96d270をreleaseで確認した。
大meshは400×400 cell、160,801頂点・960,000 index、8,342,428 byte。
別meshと合わせた11,764,664 byteを256 KiB stagingから47 chunkで受理し、
最後の受理からCPU完了待ちなしで描画へ進む。頂点／index byteの照合も含む。

engine_workloadsのverifyは開発検証・native Core＋SyncValありで45画像、
benchmarkはRenderer診断なし・native Disabledで519画像が全RGBA期待値と一致した。
32個のCPU assetを保持した4-mesh cacheの入れ替え、3 pending中の更新・削除／再登録、
2 Viewの先行build、独立renderer入力を含む。verifyはnative破棄後まで診断ゼロ。
製品runのnative実効フラグは未観測としてnullを記録した。

変更前d61ca29と製品構成を、32 warm-up＋128測定frame×3 run、3 in-flightで比較した。
大meshを64×64へ描くwarm経路で、CPU activeはbuild開始からsubmit復帰まで、
cold upload・snapshot作成・容量待機を含まない。GPUはreadback込みで表示・VSyncを含まない。

| 指標 | 変更前 | 製品構成 |
|---|---:|---:|
| CPU active p50／p95 | 56.0／93.2 µs | 21.3／49.7 µs |
| GPU Graph p50／p95 | 111.072／112.128 µs | 110.688／114.240 µs |
| cold upload受理／完了観測 | 5.623／5.673 ms | 7.291／7.369 ms |

検証・診断の設定を意図的に変えた作業環境サンプルで、個別修正の因果効果やGPU高速化を示さない。
clock・affinity・電力・OS負荷を固定せず、統計的な有意差やFPSを主張しない。
cold uploadは事前生成済みmeshの転送操作で、level load全体の時間ではない。
最終常駐4 mesh／19,600論理byte、staging high-water 262,144 byte、
VMA Allocation 12,459,488 byte、block 100,663,296 byteは前後で同じだった。
trimは明示APIであり、この負荷へ暗黙のevictionを追加していない。

条件・生sample・ソースhash・再構築可能な通常ライブラリallocation probeとLLVM文字列検査結果は
[現在のエンジン負荷データ](engine-workloads-metrics.json)へ保存した。
以前の登録／cache索引化比較と出力hashはGit履歴に残す。

## 第二APIの準備とbuffer実行

M4の第二APIはD3D12を選定した。既存Windows実機とshaderツールを使って、
同じ入力・Graph・GPUでVulkanと比較しやすいことが理由である。
準備4課題（`renderer-we8.1`・`.2`、`renderer-6qc`・`renderer-k51`）に続き、
M4-03の公開`D3D12Runtime`とM4-04の共有Graph buffer実行を追加した。
Windows限定の`d3d12` featureで、単一DIRECT queueのupload／copy／readbackを使える。
画像、import/export、上限付き再利用と一般RHIの実装は後続課題へ残す。

bindingsは`windows-bindgen`／`windows-default`／`windows-core` 0.100.0、MSRV 1.95で固定した。
入力`Windows.Win32.winmd`のSHA-256は`818706A4908FE51447EBFD88F1E54195CEDA5EC437F95FE17F1250471680811D`、
M4-03/04で必要APIのfilterを拡張したLF固定の生成Rustは
`05ADD69A1266EF2B3EF24D73CDCAC193A0D603B441ACCEDD6D1B1EB7A490D9C0`で再生成一致した。
通常ビルドは生成済みRustを使い、generatorとmetadataの依存は独立workspaceで監査する。
既存依存の`windows-link` 0.2と新規0.100.0の併存、tool監査の未使用license許可に関するwarningは
GPU Validationとは別の依存監査診断として確認した。ライセンスやadvisoryの除外追加はない。

固定SDK 1.4.341.1のSPIRV-Cross 1.4.341.0とDXC 1.9.0.5180で5種類のshaderを生成・検証し、
既存SPIR-V、HLSL、DXIL、binding／定数reflection、manifestの再生成一致を確認した。
固定toolのhash・引数と4／80-byte定数、行列・座標・samplerの対応は
[ツールガイド](../guides/tooling.md#d3d12の固定shader生成)と管理下metadataに記録した。
生成物の改変とSDK不足が失敗することも確認した。

準備段階のソース`e96d270410db3f528bfec2df9ff067d6200c489d`での3回成功に加え、
統合ソース`2b15cd0bc7ca68892f433c1a67d5033278f7252f`、clean mainで
[準備スクリプト](../../scripts/verify-m4-preparation.ps1)を1回再実行して成功した。
機材・実行担当・再実行方法は[開発手順](../guides/development.md#m4のd3d12準備と実gpu受入経路)を参照する。

| 確認 | 準備probeの結果 |
|---|---|
| GPU同一性 | NVIDIA RTX 3060、vendor/device IDとDXGI／Vulkan LUIDが一致 |
| 能力 | SM6.0、RGBA8 typed UAV load/store、resource binding tier 3、heap tier 2 |
| allocation基準 | DEFAULT／UPLOAD／READBACKの64-byte committed bufferを作成・検査・破棄。各allocation 65,536 byte、alignment 65,536 |
| Debug Layer／GBV | Device作成前に設定、InfoQueue取得と起動message ID1016のGBV有効報告を確認。設定getterだけから推測していない |
| 終了後診断 | 最終Device Releaseを含むcallbackとDXGI global queueを確認。warning／error／corruption、callback失敗、破棄メッセージはゼロ |
| Vulkan検証 | 実選択Deviceを使い、Core／SyncValの実効と破棄後の診断ゼロを確認 |

初期storage filterはINFOだけを65件抑制していた。filterのallow／deny全条件を検査し、
警告・error・corruptionを抑制しないことを確認してから解除した。元のfilterと件数はJSONへ残し、
解除後の追加抑制はゼロ。既存メッセージを消去して成功扱いにはしていない。
最初のprobeで検出したDEFAULT bufferの初期状態に関する警告1328は`COMMON`への修正で解消し、例外は追加していない。

M4-03/04の通常経路は、Graph構築／compileの不変保証と実行順を共有し、
D3D12でlive操作の対応可否と作成上限をprepare時に確定する。submit時に同じGraph内容を再走査しない。
DEFAULTは`COMMON`から明示legacy transitionを記録し、同一buffer内の非重複copyはscratch経由で実行する。
1 bufferの2 GiB制限はライブラリの対応上限であり、実native allocation成功やVRAM予算の保証ではない。
DeviceのCOM所有共有、processで固定した検証設定、最終所有者の解放順とcallback寿命は独立レビューで確認した。
診断なしではnative診断module・Session fieldを除外し、monitorはZSTとなることをCPU試験でも確認した。
同期計画、native作成失敗、Fence、Device異常、GPU利用中の所有は製品にも残る。

最適化済みの厳格runtime試験では、共有する二つのruntimeの先行shutdown、設定不一致の拒否、
最終shutdown後の同一process内再作成と再終了が成功した。
buffer寿命試験では未投入キャンセル、最初の実committed resource作成後の注入失敗、
二つの投入の逆順待機、runtimeを先に終了したpending、pending Drop、readbackのowner／二重消費拒否、
readbackなしの空Graphについて正常完了と最後のnative参照の解放順を確認した。
別途CPU試験で、voidのExecuteより前の所有確立と後続Signal失敗／unwind時の保持を確認した。
実native OOMやDevice removalを発生させた試験ではない。Device部分初期化の解放はRAIIと静的レビューで確認した。

[二API反復スクリプト](../../scripts/verify-m4-buffer.ps1)は同じcleanソースで
`-Runs 3 -OutputDirectory .tools/m4/runtime-buffer/acceptance-final`を実行した。
各構成は別processのreleaseで、各回VulkanとD3D12のGPU作業を逐次実行した。

| 比較 | 3回共通の結果 |
|---|---|
| 実adapter | RTX 3060、vendor `4318`／device `9351`、両APIの有効LUID `[116,226,0,0,0,0,0,0]`が一致 |
| 入力 | 一度compileした同じ10-pass Graph。3 upload、4 copy（同一buffer内1件）、3 readback |
| 内容 | 部分offsetと未変更領域を含む308 byte／backendが、期待値・Vulkan・D3D12で全一致 |
| 厳格構成 | `vulkan,d3d12,development-validation`。Debug Layer／GBV、Core／SyncValの実効を確認。最終解放までwarning／error／corruption／callback失敗／問題本文欠落ゼロ |
| 製品構成 | `vulkan,d3d12`。開発検証・両native診断・Graph診断・debug assertionsが無効。明示native Disabledで同じ全byte一致 |
| 証拠 | `summary.json`、各構成のfeature tree、6回分のJSON／ログに原byte・checksum・実効設定・終了診断を保持 |

製品構成の診断件数0は観測を組み込まない結果であり、外部からの検証注入がなかった証明には使わない。
GBVを有効にしてbuffer copyを確認したが、shaderの計装を実行した証拠ではない。
画像等価性と二APIの性能比較、長時間耐久、実VRAM飽和は未実行で、性能優位や閾値を受入条件にしていない。
旧M4-04までの6／22課題を確認した実績であり、旧M4全体や新Bistro計画の完了ではない。
実GPU CIは追加せず、既存CIはCPU／buildと依存監査を担当する。

別GPU/driver、Linux表示・実GPU、Mac/Metal、実native OOM/Device loss/Surface loss、
拡張なしWSI、透明Window、HDR、物理aliasing、複数queueは今回の追加実機確認に含まない。
上記の追加affinity controlを除き、clock・affinity・電力・OS負荷は固定せず、他エンジンとの性能比較も行っていない。
過去のtransfer-only Criterionや局所compiler比較の数値を、現在のframe基準値やFPSに読み替えない。

## 再現物

今回の10構成の品質・release両構成・独立feature・Rustdoc・実GPU・表示例のログは`.tools/graph-review/gates/`、
3回反復の環境・clean source commit・shader hash・全probe JSON／logは
`.tools/graph-review/preparation-final/`へ保存した。最初のPython経由起動はPowerShell module探索で失敗し、
その証拠は`.tools/graph-review/preparation/`に保持した。PowerShellからの直接起動で前提検査を省かず成功した。
再実行sourceと生成済みbindings／shaderはGit管理し、`.tools/`だけに依存しない。

M4前の入力・資源管理のゲートは`.tools/engine-readiness/gates/`へ保存した。
`{cpu,vulkan,presentation}-{check,clippy,test}.log`、`fmt.log`、`examples-test.log`、
`gpu-{vulkan,presentation}.log`、`gpu-game-public.log`、`workloads-{verify,benchmark}.log`、
`frame-verify.log`、`distinct-verify.log`、`headless.log`、`windowed-{on,off}.log`を含む。
現在の負荷とCPU保持量probeは[エンジン負荷JSON](engine-workloads-metrics.json)に収めた。以前の索引化probeはGit履歴を参照する。
大メッシュの再実行は開発ガイドの`engine_workloads`を使う。性能値だけの固定合否閾値はCIに追加していない。

今回のゲートは`.tools/opaque-batching/gates/{cpu,vulkan,presentation}-{check,clippy,test}.log`と`fmt.log`、
`examples.log`、`gpu-{vulkan,presentation}.log`、`default-verify.log`、`windowed-{on,off}.log`。
比較の各run JSON/RGBAと追加controlは`.tools/opaque-batching/{comparison,controls}/`へ保存した。
共有する条件・統計・生sampleは[RenderGraph比較JSON](rendergraph-metrics.json)を正とし、比較sourceはmainのGit履歴から取得できる。
cache比較のprobeとログは`.tools/pre-m4/`にあり、前回のnative異常は同ディレクトリの
`gates/gpu-all-initial-native-crash.log`、単独Device確認は`gates/gpu-device-isolated.log`へ残した。
ローカルの`.tools/`自体を共有sourceの代わりにはしない。
既存GLSL／SPIR-V binaryは今回も変更せず、固定SDKで再生成一致を再確認した。
今回追加したD3D12依存・生成tool依存・DXILの検証結果は上記M4準備のログを参照する。

従来のM3ゲートは`.tools/m3/gates/{cpu,vulkan,presentation}-{check,clippy,test}.log`、
`gpu-all.log`、`windowed-smoke.log`、`example-cpu.log`、`shader-verify.log`、`headless-example.log`。
Linux CI修正後の追加確認は`ci-fix-{vulkan,presentation}-{check,clippy,test}.log`と`ci-fix-gpu.log`。
統合verifyのJSON/PPMは`.tools/m3/frame-verify.json`、`frame.ppm`、
WSI能力の元情報は`.tools/m3/preflight-vulkaninfo.log`、第二API probeは`.tools/m3/second-api-probe.txt`。
`.tools/`はGit対象外で、再実行方法は開発ガイドに記載する。
性能runの生ログは`.tools/m3/gates/frame-baseline.log`、Gitで共有する基準データは
[current-metrics.json](current-metrics.json)を正とする。
