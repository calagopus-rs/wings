#!/usr/bin/env bash

usage() {
    echo "Usage: $0 <tarball url>"
    exit $1
}

fatal() {
    echo >&2 "fatal: $1, aborting..."
    exit 1
}

cd "$(dirname "$(realpath "$0")")"

git status --porcelain | grep -v '^??' >/dev/null && fatal "git repository must not be dirty"

[ -z "$1" ] && usage 1
[ "$1" = "--help" ] && usage 0

curl "$1" -o src.tar.gz &&

tar xvf src.tar.gz &&

rm src.tar.gz &&

git add unrar &&

while read patch; do
    git cherry-pick -n $patch
done < patches.txt