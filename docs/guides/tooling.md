# 依存・SPIR-V・GPU調査ツール

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

管理下5種類のGLSL／SPIR-Vから、SPIRV-CrossでHLSL、DXCでDXILを生成する。
入力のSPIR-Vは既存のVulkan基準を維持し、この手順では書き換えない。
通常のRustビルドは管理下のbinaryを利用し、shader compilerやSDKを要求しない。

```powershell
& ./crates/renderer/shaders/regenerate-dxil.ps1 -SdkRoot C:/VulkanSDK/1.4.341.1
& ./crates/renderer/shaders/regenerate-dxil.ps1 -SdkRoot C:/VulkanSDK/1.4.341.1 -Verify
```

`-Verify`は生成済みファイルを変更せず、不足・差分・tool不一致・変換失敗をエラーにする。
先に既存の`regenerate.ps1 -Verify`で5種類のSPIR-V再生成一致と`spirv-val`を確認する。
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

変換optionの意味は[採用版SPIRV-Cross CLI](https://github.com/KhronosGroup/SPIRV-Cross/blob/vulkan-sdk-1.4.341.0/main.cpp)を参照する。
この確認は生成・静的ABI・再現性の証拠であり、D3D12での実行や二APIの画像等価性は未確認である。
非identity MVP、非対称なgeometry、深度、Compute／合成の全画素比較はM4-08〜10で確認する。
4／80 byteの論理定数ABIと、後続runtimeのroot constants／CBVの配置・range要件も区別する。

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

描画・Compute処理ができたら、qrenderdocから対象実行ファイルを起動して捕捉し、
コマンド列、pipelineやbinding、buffer／imageの内容を確認する。
現在のM1試験はPresentを行わないため、通常のPresent基準の捕捉トリガーでは範囲を決められない。
offscreen試験の捕捉は、M2で必要な範囲へ`StartFrameCapture`／`EndFrameCapture`を接続する。

[公式APIの推奨](https://raw.githubusercontent.com/baldurk/renderdoc/v1.47/docs/in_application_api.rst)に従い、
RenderDocから起動されたときに読み込まれたAPIを利用する。
通常実行や公開Renderer APIへRenderDocを必須依存として持ち込まない。
導入確認ではCLIとDLLのAPI取得に加え、子プロセスでinstance／deviceのcapture layer挿入を確認した。
実際のGPUキャプチャはまだ行っていない。
