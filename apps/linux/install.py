#!/usr/bin/env python3
"""Install only runtime files; staging supports Debian and Flatpak without pip."""
import argparse
import pathlib
import shutil

parser = argparse.ArgumentParser()
parser.add_argument("--prefix", default="/usr")
parser.add_argument("--destdir", default="")
args = parser.parse_args()
source = pathlib.Path(__file__).resolve().parent
prefix = pathlib.Path(args.destdir + args.prefix)
runtime = prefix / "share/pigeonpost-desktop"
runtime.mkdir(parents=True, exist_ok=True)
shutil.copytree(source / "pigeonpost", runtime / "pigeonpost", dirs_exist_ok=True,
                ignore=shutil.ignore_patterns("__pycache__", "*.pyc"))
launcher = prefix / "bin/pigeonpost-desktop"
launcher.parent.mkdir(parents=True, exist_ok=True)
launcher.write_text('#!/usr/bin/env python3\nimport sys\nsys.path.insert(0, ' + repr(args.prefix + '/share/pigeonpost-desktop') + ')\nfrom pigeonpost.ui import main\nraise SystemExit(main())\n')
launcher.chmod(0o755)
for name, directory in [("dev.pigeonpost.Desktop.desktop", "applications"),
                        ("dev.pigeonpost.Desktop.metainfo.xml", "metainfo"),
                        ("dev.pigeonpost.Desktop.png", "icons/hicolor/512x512/apps")]:
    destination = prefix / "share" / directory / name
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source / "data" / name, destination)
