# Bistro素材

Amazon Lumberyard Bistro（Amazon Lumberyard、2017年）、NVIDIA Open Research Content Archive (ORCA)。
[公式配布ページ](https://developer.nvidia.com/orca/amazon-lumberyard-bistro)と
[CC-BY 4.0](https://creativecommons.org/licenses/by/4.0/)に従い、出典と変更内容を保持する。
原本の[ライセンス全文](source/LICENSE.txt)、[READMEと引用情報](source/README.txt)、
[CHANGELOG](source/CHANGELOG.txt)はbyteを変更せず通常Gitで管理する。

| 場所 | 内容 | byte数 |
|---|---|---:|
| `source/Bistro_v5_2.zip` | 公式配布原本。屋外・屋内FBX、元DDSの全mip、付属文書、環境HDRを含む | 894,377,473 |
| `converted/BistroExterior.gltf`、`BistroExterior.bin`、`textures/` | 検証済み屋外glTF・buffer・405 PNG、計407ファイル | 851,291,697 |
| `environment/san_giuseppe_bridge_4k.hdr` | 原本からbyteを変えず取り出した、直接利用する環境画像 | 23,695,717 |
| [比較PNG](../../docs/verification/bistro-assets-preview.png) | 元FBXとglTF再取込のCPU比較画像 | 3,422,828 |

上記410ファイル、合計1,772,787,715 byteをGit LFSで管理する。
[manifest.json](manifest.json)が各パス・byte数・SHA-256を固定する。
素材と比較PNGのLFSポインタをGitで管理し、実体はoriginのLFSストレージで共有する。
検証JSON・ライセンス・本書は通常Gitで管理する。屋内を原本ZIPに保持することは屋内描画を受入範囲へ追加する意味ではない。

環境画像はGreg Zaalによる[San Giuseppe Bridge / Poly Haven](https://polyhaven.com/a/san_giuseppe_bridge)、
[CC0](https://polyhaven.com/license)として出典を保持する。同梱版は現在の配布版とのbyte一致を検証していない。
環境内の太陽と解析光の扱い、最終露出などは未確定である。

変換でDDSのbase mipをPNG化し、BC5 normalの正Z復元・DirectXからの緑成分反転、
明示的PBR/alpha recipe、重複面の頂点分離、raw normal保持を行った。
元DDSの全mipをPNGへ収録したとは扱わない。使用中2材質の全ゼロalpha解釈とruntimeの接線/mip生成は未解決で、
このglTFはRendererの完成済みcook形式でもVulkan/D3D12の受入済み画像でもない。
詳細と完全性の証拠は[現在の検証結果](../../docs/verification/current.md#bistro-assets)を参照する。

取得はルートから次を実行する。Git LFSを先にインストールし、Beadsと共用する追跡済みhooksを使う。

```powershell
git config --local core.hooksPath .beads/hooks
git lfs install --local --skip-repo
git lfs pull --include="assets/bistro/**,docs/verification/bistro-assets-preview.png" --exclude=""
python -B tools/bistro/verify_lfs_assets.py
```

`git lfs install --force`で共用hookを置換しない。
`bd hooks install --beads`はBeadsマーカー外のLFS処理を保持する。
素材不要のclone/pullでは`GIT_LFS_SKIP_SMUDGE=1`を設定し、必要になってから`git lfs pull`する。
通常CIは素材を取得せず、manifestと全LFSポインタの対応を検査する。
原本の展開、変換の再実行、tool取得は[開発手順](../../docs/guides/development.md#bistro-preparation)に従う。
作業用`.tools/bistro/`、Blender本体、host用blend/NPZや診断出力はGit管理対象外である。

今回の移行では公開済みGit履歴を変更しない。以前のコミットにある通常Gitの比較PNGはその履歴に残る。
