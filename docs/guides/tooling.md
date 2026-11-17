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
& C:/VulkanSDK/1.4.341.1/Bin/glslc.exe --target-env=vulkan1.3 -fshader-stage=compute example.comp -o target/example.spv
& ./scripts/validate-spirv.ps1 -Path target/example.spv -Validator C:/VulkanSDK/1.4.341.1/Bin/spirv-val.exe
```

複数ファイルには`-Path @('target/a.spv', 'target/b.spv')`を指定する。
スクリプトは`--target-env vulkan1.3`で検証し、緩和オプションを使わない。
ファイル・toolの不足、ディレクトリの指定、validatorの失敗は非ゼロで終了する。
ビルド処理から呼ぶ場合は、その終了コードでビルドも失敗させる。

`-Validator`を省略すると、`VULKAN_SDK`、既存のWindows固定SDK、PATHの順に選ぶ。
明示したtoolや`VULKAN_SDK`が無効な場合は、その原因を報告して失敗する。
複数SDKが入っている環境では、上の例のように版の分かるパスを渡す。

管理下のShaderと生成済みSPIR-Vは`crates/renderer/shaders/`にある。
固定compilerでの再生成・validator・binary一致確認は[開発手順](development.md#rendergraphとヘッドレス描画)を参照する。
通常のCPU試験やCPUだけのCriterion測定にVulkan SDKを要求しない。
SDKの構成は[固定版のSPIR-V toolchain](https://vulkan.lunarg.com/doc/view/1.4.341.1/windows/spirv_toolchain.html)を参照する。

<a id="bistro-tools"></a>

## Bistroのcook・材質shader・比較

[runtime cookと描画手順](development.md#bistro-pbr)は通常のRust buildから分離する。
`tools/bistro/mikk.py`がoffline用C helperをClangで作り、`cook.py`が固定glTFと元DDSの全mipを
実行用streamへ変換する。MikkTSpaceの固定commit、無変更upstream fileのhash、保持した
zlib-style licenseの所在は[provenance](../../tools/bistro/mikktspace/provenance.json)へ記録する。
ClangやMikk shared libraryをRendererのruntime依存へ追加しない。

PBRと最終SDR resolveの利用側shaderは`examples/support/bistro/shaders/`に置く。
通常buildは生成済みSPIR-V／DXILを使い、変更時は固定SDKで再生成する。

```powershell
& ./crates/renderer/examples/support/bistro/shaders/regenerate.ps1 -SdkRoot C:/VulkanSDK/1.4.341.1 -Verify
```

`-Verify`は一時directoryで再生成し、checkoutを書き換えずbinary・reflection・manifestを照合する。
Windowsでも生成HLSLをUTF-8/BOMなし・LFへ揃えてからDXCへ渡し、同じcanonical textをhashへ使う。
PBR vertexのclip変換は明示したFP32演算順序と`NoContraction`を保ち、DXILのfast演算やFMA等への
縮約・再結合を生成時に拒否する。camera/ABIやfragment最適化を変えず、対象をclip演算へ限定した
[採用理由とリスク](../design/implementation-policy.md#bistro-runtime-cook)に従う。
更新を意図する場合は`-Verify`を外し、shaderとRust descriptorのABI、footprintと安全性契約をレビューする。
hash/reflectionの一致だけで未信頼shaderを安全とみなさない。

`compare_runtime.py`はrunnerの`report.json`とHDR/SDR/depthを読み、固定profileと全画素・ROIの誤差を集計する。
captureは実体解決後もreport directory内にあることを要求する。Windowsの通常/拡張長pathは
比較用表現を正規化し、実際の読込pathを保持したまま同じ包含条件で検査する。device/volume namespaceは受け付けない。
`measure_runtime.py`は通常releaseの保存済みEXEを逐次実行し、native詳細CPU timerとGPU timestampを
一緒に切り替えたoff/onを分ける。非収集の`native_phase_totals_ns`は`null`であり、query単独の費用を分離した計測とは扱わない。
これらの計測にWPRや管理者実行は不要であり、ETWの収集手順とは独立する。
実行したprofile/hash・レビュー修正・二APIの内容照合と計測値は[現行検証](../verification/current.md#bistro-pbr)へまとめる。

## D3D12 bindingsの生成

`d3d12` featureはWindows限定のprivate bindingsを使う。公開Graph／Engineへnative型を出さず、
通常ビルドは[生成済みRust](../../crates/renderer/src/backends/d3d12/bindings.rs)を使う。
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

toolchain定義に列挙した管理下GLSL／SPIR-Vから、SPIRV-CrossでHLSL、DXCでDXILを生成する。
入力のSPIR-Vは既存のVulkan基準を維持し、この手順では書き換えない。
通常のRustビルドは管理下のbinaryを利用し、shader compilerやSDKを要求しない。

```powershell
& ./crates/renderer/shaders/regenerate-dxil.ps1 -SdkRoot C:/VulkanSDK/1.4.341.1
& ./crates/renderer/shaders/regenerate-dxil.ps1 -SdkRoot C:/VulkanSDK/1.4.341.1 -Verify
```

`-Verify`は生成済みファイルを変更せず、不足・差分・tool不一致・変換失敗をエラーにする。
先に`regenerate.ps1 -Verify`で対象SPIR-Vの再生成一致と`spirv-val`を確認する。
[toolchain定義](../../crates/renderer/shaders/dxil-toolchain.json)でWindows x64 SDKの
glslc・spirv-val・SPIRV-Cross・DXC・dxcompiler.dllのSHA-256と、変換引数を固定する。
SPIRV-Crossは`vulkan-sdk-1.4.341.0`、DXCは`1.9.0.5180 (e35541826)`である。
DXCの`-WX`と通常の内部validatorを使い、`-Vd`は使わない。
採用版は外部`dxil.dll`を探索せず、常に内部validatorを使う変更後の版である。
[DXCのvalidator変更](https://github.com/microsoft/DirectXShaderCompiler/releases/tag/v1.8.2505)と
[CLI・reflectionの公式説明](https://github.com/microsoft/DirectXShaderCompiler/wiki/Using-dxc.exe-and-dxcompiler.dll)を参照する。

[生成物](../../crates/renderer/shaders/dxil/)にはHLSL、検証付きコンパイルで生成したDXIL、
DXCのsignature／binding／constant offsetのreflection、入力と出力のSHA-256 manifestを保存する。
shader modelは6.0、entry pointは`main`、HLSLは2021、最適化は`-O3`、debug情報・include・macroはない。
GLSLの入力hashだけはGitの改行設定に依存しないUTF-8／LFへ正規化し、binaryは全byteを比較する。
HLSL・reflection・manifestの改行は管理下の`.gitattributes`でLFへ固定し、reflectionの行末空白は除く。

[固定ABIの契約](../../crates/renderer/shaders/dxil-contracts.json)には次を記録する。

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

利用側のCompute受入shaderは`crates/renderer/tests/support/shaders/`へ置く。
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
python -B tools/cpu_profile/build.py --example bistro --features vulkan,d3d12 --profile release --output .tools/tracy/builds/normal
python -B tools/cpu_profile/build.py --example bistro --features vulkan,d3d12 --profile release --instrumentation tracy --output .tools/tracy/builds/instrumented
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
manifestにはsource/lock/toolchain、要求・実効feature、観測したCargo profile、EXE/PDBのhashとGUID/ageを含める。
Bistroの埋込固定条件`tools/bistro/conditions.json`もsource hashに含め、camera/描画条件をEXEのidentityへ結び付ける。
capture/export/行解決は保存したbytesを再検証し、後のCargo buildで置換される`target/`のEXEを直接使わない。

既定のexampleは`windowed`、featuresは`vulkan,d3d12,presentation`であるため、
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
