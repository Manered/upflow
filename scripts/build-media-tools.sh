#!/usr/bin/env bash
# Run from an MSYS2 UCRT64 shell. The extracted source archives are the build inputs.
set -euo pipefail

if [[ ${MSYSTEM:-} != UCRT64 ]]; then
  echo 'Run this from the MSYS2 UCRT64 shell.' >&2
  exit 1
fi

repository_root="$(cd "$(dirname "$0")/.." && pwd)"
output_directory="$repository_root/media-tools"
work_directory="$(mktemp -d)"
trap 'rm -rf "$work_directory"' EXIT

stage_directory="$work_directory/stage"
install_prefix="$work_directory/prefix"
jobs="${UPFLOW_BUILD_JOBS:-4}"
ffmpeg_tag=n7.1.5
ffmpeg_tag_object=eefb3654c445a9004712036be4ffb01a68b2c3e1
x264_commit=b35605ace3ddf7c1a5d67a2eb553f034aef41d55

for tool in git gcc make nasm pkg-config tar gzip objdump; do
  command -v "$tool" >/dev/null
done

mkdir -p \
  "$stage_directory/bin" \
  "$stage_directory/licenses" \
  "$stage_directory/source" \
  "$work_directory/ffmpeg" \
  "$work_directory/x264"

git init -q "$work_directory/ffmpeg-git"
git -C "$work_directory/ffmpeg-git" fetch \
  --depth=1 \
  https://github.com/FFmpeg/FFmpeg.git \
  "refs/tags/$ffmpeg_tag"
[[ "$(git -C "$work_directory/ffmpeg-git" rev-parse FETCH_HEAD)" == "$ffmpeg_tag_object" ]]
git -C "$work_directory/ffmpeg-git" archive --format=tar "FETCH_HEAD^{commit}" \
  | gzip -n > "$stage_directory/source/ffmpeg-$ffmpeg_tag.tar.gz"
tar -xzf "$stage_directory/source/ffmpeg-$ffmpeg_tag.tar.gz" -C "$work_directory/ffmpeg"

git init -q "$work_directory/x264-git"
x264_url=https://code.videolan.org/videolan/x264.git
if ! git -C "$work_directory/x264-git" fetch --depth=1 "$x264_url" "$x264_commit"; then
  git -C "$work_directory/x264-git" fetch --no-tags "$x264_url" refs/heads/stable
fi
git -C "$work_directory/x264-git" archive --format=tar "$x264_commit" \
  | gzip -n > "$stage_directory/source/x264-$x264_commit.tar.gz"
tar -xzf "$stage_directory/source/x264-$x264_commit.tar.gz" -C "$work_directory/x264"

(
  cd "$work_directory/x264"
  ./configure \
    --host=x86_64-w64-mingw32 \
    --prefix="$install_prefix" \
    --enable-static \
    --disable-cli
  make -j"$jobs"
  make install
)

(
  cd "$work_directory/ffmpeg"
  export PKG_CONFIG_PATH="$install_prefix/lib/pkgconfig"
  ./configure \
    --prefix="$install_prefix" \
    --target-os=mingw32 \
    --arch=x86_64 \
    --enable-gpl \
    --enable-libx264 \
    --disable-autodetect \
    --disable-network \
    --disable-shared \
    --enable-static \
    --disable-doc \
    --disable-debug \
    --disable-ffplay \
    --pkg-config-flags=--static \
    --extra-ldflags=-static
  make -j"$jobs" ffmpeg.exe ffprobe.exe
  cp ffmpeg.exe ffprobe.exe "$stage_directory/bin/"
)

for tool in "$stage_directory/bin/"*.exe; do
  objdump -f "$tool" | awk '/file format pei-x86-64/{found=1} END {exit !found}'
  # The portable distribution relies only on DLLs supplied with Windows.
  if objdump -p "$tool" | awk '/DLL Name:/{print tolower($NF)}' | grep -Eq '^lib(winpthread|gcc|stdc\+\+)'; then
    echo "Unexpected MinGW runtime dependency in $(basename "$tool")." >&2
    exit 1
  fi
done

cp "$work_directory/ffmpeg"/COPYING.* "$stage_directory/licenses/"
cp "$work_directory/x264/COPYING" "$stage_directory/licenses/x264-GPL-2.0.txt"
cp "$repository_root/scripts/build-media-tools.sh" "$stage_directory/source/"
{
  printf 'FFmpeg tag: %s (%s)\nx264 commit: %s\n\n' "$ffmpeg_tag" "$ffmpeg_tag_object" "$x264_commit"
  gcc --version
  nasm -v
  "$stage_directory/bin/ffmpeg.exe" -version
  "$stage_directory/bin/ffprobe.exe" -version
} > "$stage_directory/licenses/media-build.txt"

# Check that the compiled encoder is present and can actually produce frames.
"$stage_directory/bin/ffmpeg.exe" -v error -f lavfi -i 'testsrc2=s=64x64:r=25:d=1' \
  -c:v libx264 -f mp4 "$work_directory/check.mp4"
[[ "$("$stage_directory/bin/ffprobe.exe" -v error -select_streams v:0 -show_entries stream=codec_name -of csv=p=0 "$work_directory/check.mp4" | tr -d '\r')" == h264 ]]

mkdir -p "$output_directory"
cp -R \
  "$stage_directory/bin" \
  "$stage_directory/licenses" \
  "$stage_directory/source" \
  "$output_directory/"
printf 'Windows media tools created at %s\n' "$output_directory"
