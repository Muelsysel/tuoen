"""造一批**恶意归档**，用来问 bsdtar：它默认会怎么做？

这不是 tuoen 的代码，是一次性取证脚本。目的是在设计安全解压之前，
先拿到"系统 tar.exe 对每种攻击的实际行为"这条事实 —— 而不是照文档推断。

用法：
    python make_evil.py <输出目录>
"""

import os
import sys
import tarfile
import zipfile

# 每一种都是票据 #5 点名要拒绝的形态。
EVIL_NAMES = [
    ("dotdot", "../escaped.txt"),
    ("dotdot-deep", "a/b/../../../escaped-deep.txt"),
    ("absolute-unix", "/escaped-abs.txt"),
    ("absolute-drive", "C:/escaped-drive.txt"),
    ("unc", "//server/share/escaped-unc.txt"),
    ("backslash-dotdot", "..\\escaped-backslash.txt"),
    ("device-name", "CON"),
    ("device-name-nested", "sub/NUL.txt"),
    ("trailing-dot", "trailing."),
    ("trailing-space", "trailing "),
    ("ads-colon", "stream.txt:evil"),
    ("case-collision", "Readme.txt"),
    ("plain", "ok/hello.txt"),
]


def make_zip(path: str) -> None:
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as zf:
        for label, name in EVIL_NAMES:
            zf.writestr(name, f"{label}\n")
        # 大小写碰撞的另一半
        zf.writestr("README.TXT", "case-collision-other\n")
        # 一个指向外部的 symlink 条目（zip 用外部属性表达）
        info = zipfile.ZipInfo("link-out")
        info.external_attr = (0o120777 << 16)  # S_IFLNK | 0777
        zf.writestr(info, "../../outside-target")


def make_tar(path: str, mode: str) -> None:
    with tarfile.open(path, mode) as tf:
        for label, name in EVIL_NAMES:
            data = f"{label}\n".encode()
            info = tarfile.TarInfo(name=name)
            info.size = len(data)
            tf.addfile(info, __import__("io").BytesIO(data))
        tf.add("README.TXT", arcname="README.TXT") if False else None
        data = b"case-collision-other\n"
        info = tarfile.TarInfo(name="README.TXT")
        info.size = len(data)
        tf.addfile(info, __import__("io").BytesIO(data))
        # symlink 条目：目标是根目录之外
        link = tarfile.TarInfo(name="link-out")
        link.type = tarfile.SYMTYPE
        link.linkname = "../../outside-target"
        tf.addfile(link)


def main() -> None:
    out = sys.argv[1]
    os.makedirs(out, exist_ok=True)
    make_zip(os.path.join(out, "evil.zip"))
    make_tar(os.path.join(out, "evil.tar"), "w")
    make_tar(os.path.join(out, "evil.tar.gz"), "w:gz")
    print(f"wrote archives into {out}")


if __name__ == "__main__":
    main()
