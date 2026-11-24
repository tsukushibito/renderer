"""Fetch the CC0 Standard edition source inputs used by cook_character.py."""

import argparse
import hashlib
from pathlib import Path
from urllib.request import urlopen


BASE = (
    "https://raw.githubusercontent.com/zongjunlv/FPS/"
    "43d750b81ba3b86033656fa281e8a6d59f37609d/Assets/ThirdParty/Quaternius/"
)
FILES = {
    "UniversalBaseCharacters/Models/Superhero_Male_FullBody.fbx":
        "79344418d754a59730b79d1874752e9592143db34abe8adf138fa9a92a4768e9",
    "UniversalBaseCharacters/Models/Superhero_Female_FullBody.fbx":
        "0727e7b236eeea4115531e07aeb2bb7690c1a58155f743bbf54282944fb97ea9",
    "UniversalAnimationLibrary/Animations/UAL1_Standard.fbx":
        "21b32d912da3cb93426d974fb945e86f5b2e86970acd2ce89905e0fbf9f1dcc2",
    "UniversalBaseCharacters/Textures/T_Superhero_Male_Ligh.png":
        "419f96dbb8811511e2d515bcdabc085ff2f219ec7b8b4e91050a84cba6888fe9",
    "UniversalBaseCharacters/Textures/T_Superhero_Female_Dark_BaseColor.png":
        "d366843ee7d9e9f37ec87a0e1c98c7f2e45573c08eeb80a51a6d9344e68cf288",
    "UniversalBaseCharacters/Textures/T_Eye_Brown.png":
        "d08e3356a83211bc6ca21fe3a8e39f4b5c1a3b8f85457fc2c0fb57be09935025",
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("dest", type=Path)
    dest = parser.parse_args().dest
    dest.mkdir(parents=True, exist_ok=True)
    for source, digest in FILES.items():
        output = dest / Path(source).name
        if output.exists() and hashlib.sha256(output.read_bytes()).hexdigest() == digest:
            continue
        with urlopen(BASE + source, timeout=120) as request:
            data = request.read()
        if hashlib.sha256(data).hexdigest() != digest:
            raise ValueError(f"source hash mismatch: {source}")
        output.write_bytes(data)
        print(f"{digest}  {output}")


if __name__ == "__main__":
    main()
