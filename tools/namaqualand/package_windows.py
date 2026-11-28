"""Build a version-pinned Windows launcher and its cooked Namaqualand archive."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import zipfile

from restore_near_csm import references


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def notices(archive, repo):
    features = "scene-viewer,d3d12"
    common = ["--locked", "--offline", "--features", features, "--manifest-path",
              str(repo / "examples/renderer-scenes/Cargo.toml")]
    target = "x86_64-pc-windows-msvc"
    names = subprocess.check_output(
        ["cargo", "tree", *common, "-p", "renderer-scenes", "--target", target,
         "--edges", "normal", "--prefix", "none", "--no-dedupe", "--format", "{p}"],
        text=True, encoding="utf-8").splitlines()
    packages = json.loads(subprocess.check_output(
        ["cargo", "metadata", *common, "--filter-platform", target, "--format-version", "1"]))
    included = {tuple(line.split()[:2]) for line in names}
    index = []
    for package in packages["packages"]:
        name, version = package["name"], package["version"]
        if not package["source"] or (name, "v" + version) not in included:
            continue
        index.append({"name": name, "version": version, "license": package["license"],
                      "authors": package["authors"], "repository": package["repository"]})
        directory = Path(package["manifest_path"]).parent
        for path in directory.iterdir():
            if path.is_file() and path.name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "NOTICE")):
                archive.writestr(f"notices/{name}-{version}/{path.name}", path.read_bytes())
        if name == "vk-mem":
            header = directory / "vendor/VulkanMemoryAllocator/include/vk_mem_alloc.h"
            license_lines = []
            for line in header.read_text(encoding="utf-8").splitlines():
                if not line.startswith("//"):
                    break
                license_lines.append(line.removeprefix("//").lstrip())
            notice = "\n".join(license_lines)
            if "Copyright" not in notice or "THE SOFTWARE IS PROVIDED" not in notice:
                raise ValueError("Vulkan Memory Allocator notice has changed")
            archive.writestr("notices/VulkanMemoryAllocator/LICENSE.txt", notice + "\n")
    archive.writestr("notices/dependencies.json", json.dumps(index, indent=2))


def build(args):
    if not re.fullmatch(r"[A-Za-z0-9._-]+", args.version):
        raise ValueError("Expected a version without path separators")
    repo = Path(__file__).resolve().parents[2]
    scene = json.loads((args.scene / "scene.json").read_bytes())
    args.output.mkdir(parents=True, exist_ok=False)
    bundle = args.output / f"{args.version}-win64.zip"
    with zipfile.ZipFile(bundle, "x", zipfile.ZIP_DEFLATED, compresslevel=5) as archive:
        archive.write(args.viewer, "scene-viewer.exe")
        archive.write(repo / "assets/namaqualand/walk.json", "walk.json")
        archive.write(args.scene / "scene.json", "scene/scene.json")
        archive.writestr("attribution.json", json.dumps(scene["attribution"], indent=2))
        for key, size in sorted(set(references(scene))):
            if not re.fullmatch("[0-9a-f]{64}", key):
                raise ValueError("Invalid cooked object digest")
            blob = args.scene / "objects" / key
            if blob.stat().st_size != size or digest(blob) != key:
                raise ValueError(f"Cooked object mismatch {key}")
            archive.write(blob, "scene/objects/" + key)
        notices(archive, repo)
    constants = {"FileName": bundle.name, "Sha256": digest(bundle)}
    with tempfile.TemporaryDirectory(dir=args.output) as temp:
        settings = Path(temp) / "Package.cs"
        settings.write_text("internal static class Package {\n" + "".join(
            f"    public const string {k} = {json.dumps(v)};\n" for k, v in constants.items()
        ) + "}\n", encoding="utf-8")
        compiler = Path(os.environ["WINDIR"]) / "Microsoft.NET/Framework64/v4.0.30319/csc.exe"
        subprocess.run([str(compiler), "/nologo", "/optimize+", "/platform:x64",
                        "/reference:System.IO.Compression.FileSystem.dll",
                        "/out:" + str(args.output / "Start-Namaqualand.exe"),
                        str(Path(__file__).with_name("Launcher.cs")), str(settings)], check=True)
    print(f"{bundle.name}  {constants['Sha256']}  {bundle.stat().st_size:,} bytes")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("scene", type=Path)
    parser.add_argument("viewer", type=Path)
    parser.add_argument("version", help="local distribution version")
    parser.add_argument("output", type=Path, help="new output directory")
    build(parser.parse_args())
