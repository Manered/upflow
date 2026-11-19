# How upflow patches videos

upflow first makes an H.264 MP4, then changes how the file points to its last four audio packets. Both steps happen on your PC. The second step leaves the newly encoded picture untouched.

## 1. Make the MP4

upflow takes the first video track with actual footage and the first audio track, if there is one. It keeps the coded resolution and passes through the frame timestamps. If a clip is 60 fps and its timestamps already land on a 60 fps grid, the encoder uses a `1/60` time base. Otherwise it uses the source time base, including for variable frame rate.

Video is encoded with libx264 using the `faster` preset, H.264 High Profile, and `yuv420p`. The first pass gathers stats. The second uses them to distribute a **16,617 kbps average bitrate target** across the clip. The actual bitrate varies with the footage. 1080p60 fits H.264 level 4.2. upflow selects a higher level when the resolution or frame rate needs it.

When the input has audio, upflow encodes it as AAC-LC with a 160 kbps target. It keeps the input sample rate and channel layout when FFmpeg supports them. The bundled tools are FFmpeg 6.1.1 and x264 r3108 (`31e19f9`). Changing builds or the thread count can change the encoded bytes.

### FFmpeg example

This reproduces the encode for a typical 1080p60 clip with video at stream 0 and audio at stream 1. Run it in PowerShell with the bundled FFmpeg on your PATH. The last command creates the MP4 that upflow would patch in step 2.

```powershell
$encode = @(
    '-filter_threads', '1', '-noautorotate', '-i', 'input.mp4', '-map', '0:0',
    '-map_chapters', '-1', '-c:v', 'libx264', '-preset', 'faster', '-b:v', '16617k',
    '-profile:v', 'high', '-level:v', '4.2', '-pix_fmt', 'yuv420p',
    '-fps_mode:v', 'passthrough', '-enc_time_base:v', '1:60',
    '-g', '250', '-keyint_min', '25', '-sc_threshold', '40', '-refs', '2', '-bf', '3',
    '-x264-params', 'b-adapt=1:b-pyramid=2:rc-lookahead=20:mbtree=1:aq-mode=1:aq-strength=1.0:psy-rd=1.0,0.0',
    '-threads:v', '16', '-passlogfile', 'upflow-pass'
)

ffmpeg @encode -pass 1 -flags:v +global_header -an -f null NUL
ffmpeg @encode -pass 2 -map 0:1 -c:a aac -profile:a aac_low -b:a 160k -movflags +faststart encoded.mp4
```

## 2. Patch the end of the audio

The patch runs when the encoded audio is **48 kHz stereo AAC-LC** and has at least five packets. Videos with no audio, a different audio layout or sample rate, or four or fewer packets stop after the regular encode.

An MP4 stores the audio and video bytes in an `mdat` box. Its `moov` box holds the track information and tables that tell a player where each packet lives. upflow edits the tables so the file uses four replacement packets at the end.

1. Append a second `mdat` containing four copies of the 15-byte AAC packet below. The packet signals two silent low-frequency effects channels (LFE).
2. Build a new `moov`. In the audio track, update `stsz` (packet sizes), `stsc` (packets per chunk), and `stco` or `co64` (chunk offsets). Keep the earlier audio packets in their original chunks and point the last four entries to the new chunk. The sample count, timing tables, and stereo track description stay in place.
3. Set the audio handler name to `SoundHandler` and put the upflow encoder tag on the video track. Append the new `moov` at the end and relabel the old one `free`. The original `mdat` stays where it was, so the video offsets still work and the video packets stay byte for byte intact.

```text
a0 98 00 40 01 28 00 64 00 00 03 50 00 00 38
```

[The complete MP4 patch is in `engine/src/mp4.rs`](engine/src/mp4.rs). The [FFmpeg arguments are in `engine/src/encode.rs`](engine/src/encode.rs).

## What to expect

About 85 ms of audio at the end is replaced. Some decoders see that as a channel layout change and can fail when they reach it, including during an edit or another conversion. Finish editing before running upflow and keep the original for later edits.

upflow verifies the output file with FFprobe before saving it. Upload behavior is up to TikTok, so a local export check can’t tell you whether an upload will be recompressed or what quality TikTok will serve.
