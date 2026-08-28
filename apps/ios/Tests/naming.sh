#!/bin/sh
# The name a photo from the library leaves with, compiled for the mac it is run on.
#
# Its own runner rather than a line in Tests/run.sh: this needs UniformTypeIdentifiers, and the
# thread-model suite deliberately compiles nothing that a plain `swiftc` on any mac cannot.
set -e
cd "$(dirname "$0")/.."
out=$(mktemp -d)
swiftc -O -parse-as-library -o "$out/naming" Tests/naming.swift Shared/Model/PickedImage.swift
"$out/naming"
