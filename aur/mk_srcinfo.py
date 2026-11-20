#!/usr/bin/env python3
"""Emit .SRCINFO for a PKGBUILD on argv[1] — parses simple key=value lines."""
import re
import shlex
import sys

fields = {}
for line in open(sys.argv[1]):
    m = re.match(r"^(\w+)=\(?(.+?)\)?$", line.strip())
    if m:
        fields.setdefault(m.group(1), []).extend(shlex.split(m.group(2)))

print(f"pkgbase = {fields['pkgname'][0]}")
for key in ("pkgdesc", "pkgver", "pkgrel", "url", "arch", "license",
            "depends", "makedepends", "provides", "conflicts",
            "source", "sha256sums"):
    for value in fields.get(key, []):
        print(f"\t{key} = {value}")
for name in fields["pkgname"]:
    print(f"\npkgname = {name}")
