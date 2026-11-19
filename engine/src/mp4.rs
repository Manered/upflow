use std::{
    fs::OpenOptions,
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
};

const REPLACED_PACKETS: u32 = 4;
// The Orion sample's AAC-LC program config signals two LFE elements at 48 kHz.
const LFE_SILENCE: [u8; 15] = [
    0xa0, 0x98, 0x00, 0x40, 0x01, 0x28, 0x00, 0x64, 0x00, 0x00, 0x03, 0x50, 0x00, 0x00, 0x38,
];

type Kind = [u8; 4];

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

fn u32_at(bytes: &[u8], offset: usize) -> io::Result<u32> {
    let field = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| invalid("Truncated MP4 table"))?;
    Ok(u32::from_be_bytes(field.try_into().unwrap()))
}

struct Atom<'a> {
    kind: Kind,
    bytes: &'a [u8],
    body: &'a [u8],
}

fn next_atom<'a>(remaining: &mut &'a [u8]) -> io::Result<Atom<'a>> {
    let size = u32_at(remaining, 0)?;
    let kind = remaining
        .get(4..8)
        .ok_or_else(|| invalid("Truncated MP4 box"))?
        .try_into()
        .unwrap();
    let (size, header) = match size {
        0 => (remaining.len(), 8),
        1 => {
            let extended = remaining
                .get(8..16)
                .ok_or_else(|| invalid("Truncated MP4 box"))?;
            let size = u64::from_be_bytes(extended.try_into().unwrap());
            (
                usize::try_from(size).map_err(|_| invalid("MP4 box too large"))?,
                16,
            )
        }
        size => (size as usize, 8),
    };
    if size < header || size > remaining.len() {
        return Err(invalid("Invalid MP4 box size"));
    }
    let (bytes, rest) = remaining.split_at(size);
    *remaining = rest;
    Ok(Atom {
        kind,
        bytes,
        body: &bytes[header..],
    })
}

fn child(mut body: &[u8], kind: Kind) -> io::Result<&[u8]> {
    while !body.is_empty() {
        let atom = next_atom(&mut body)?;
        if atom.kind == kind {
            return Ok(atom.body);
        }
    }
    Err(invalid("Missing MP4 box"))
}

fn write_atom(out: &mut Vec<u8>, kind: Kind, body: &[u8]) -> io::Result<()> {
    let size = u32::try_from(body.len() + 8).map_err(|_| invalid("MP4 index too large"))?;
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(&kind);
    out.extend_from_slice(body);
    Ok(())
}

fn rewrite(
    mut body: &[u8],
    mut update: impl FnMut(Atom<'_>, &mut Vec<u8>) -> io::Result<()>,
) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(body.len());
    while !body.is_empty() {
        update(next_atom(&mut body)?, &mut out)?;
    }
    Ok(out)
}

fn change_child(
    body: &[u8],
    kind: Kind,
    mut change: impl FnMut(&[u8]) -> io::Result<Vec<u8>>,
) -> io::Result<Vec<u8>> {
    rewrite(body, |atom, out| {
        if atom.kind == kind {
            write_atom(out, kind, &change(atom.body)?)
        } else {
            out.extend_from_slice(atom.bytes);
            Ok(())
        }
    })
}

fn table(body: &[u8], row_size: usize) -> io::Result<std::slice::ChunksExact<'_, u8>> {
    let count = u32_at(body, 4)? as usize;
    let rows = body
        .get(8..)
        .ok_or_else(|| invalid("Truncated MP4 table"))?
        .chunks_exact(row_size);
    if rows.len() != count || !rows.remainder().is_empty() {
        return Err(invalid("Invalid MP4 table count"));
    }
    Ok(rows)
}

fn patch_samples(stbl: &[u8], tail_offset: u64) -> io::Result<Option<Vec<u8>>> {
    let stsz = child(stbl, *b"stsz")?;
    let size = u32_at(stsz, 4)?;
    let samples = u32_at(stsz, 8)?;
    if samples <= REPLACED_PACKETS {
        return Ok(None);
    }
    let keep = samples - REPLACED_PACKETS;
    let mut sizes = stsz[..4].to_vec();
    sizes.extend_from_slice(&[0; 4]);
    sizes.extend_from_slice(&samples.to_be_bytes());
    if size == 0 {
        let original = stsz[12..].chunks_exact(4);
        if original.len() != samples as usize || !original.remainder().is_empty() {
            return Err(invalid("Invalid MP4 sample sizes"));
        }
        sizes.extend_from_slice(&stsz[12..12 + keep as usize * 4]);
    } else {
        for _ in 0..keep {
            sizes.extend_from_slice(&size.to_be_bytes());
        }
    }
    for _ in 0..REPLACED_PACKETS {
        sizes.extend_from_slice(&(LFE_SILENCE.len() as u32).to_be_bytes());
    }

    let original_runs = child(stbl, *b"stsc")?;
    let mut runs = table(original_runs, 12)?.peekable();
    let mut per_chunk = 0;
    let mut sample_index = 0u32;
    let (offset_kind, offsets) = match child(stbl, *b"co64") {
        Ok(body) => (*b"co64", body),
        Err(_) => (*b"stco", child(stbl, *b"stco")?),
    };
    let wide = offset_kind == *b"co64";
    let mut new_offsets = Vec::new();
    let mut new_runs = Vec::new();
    for (index, row) in table(offsets, if wide { 8 } else { 4 })?.enumerate() {
        let chunk = index as u32 + 1;
        if let Some(next) = runs.peek() {
            let start = u32_at(next, 0)?;
            if start < chunk {
                return Err(invalid("Invalid MP4 chunk order"));
            }
            if start == chunk {
                per_chunk = u32_at(next, 4)?;
                if u32_at(next, 8)? != 1 {
                    return Err(invalid("Unexpected AAC sample description"));
                }
                runs.next();
            }
        }
        if per_chunk == 0 {
            return Err(invalid("MP4 chunk has no samples"));
        }
        if sample_index < keep {
            let count = per_chunk.min(keep - sample_index);
            if new_runs.last().is_none_or(|&(_, prior)| prior != count) {
                new_runs.push((chunk, count));
            }
            let offset = if wide {
                u64::from_be_bytes(row.try_into().unwrap())
            } else {
                u64::from(u32_at(row, 0)?)
            };
            new_offsets.push(offset);
        }
        sample_index = sample_index
            .checked_add(per_chunk)
            .ok_or_else(|| invalid("MP4 sample count overflow"))?;
    }
    if sample_index != samples || runs.next().is_some() {
        return Err(invalid("MP4 chunks do not cover all audio samples"));
    }
    let tail_chunk = new_offsets.len() as u32 + 1;
    if new_runs
        .last()
        .is_none_or(|&(_, prior)| prior != REPLACED_PACKETS)
    {
        new_runs.push((tail_chunk, REPLACED_PACKETS));
    }
    new_offsets.push(tail_offset);

    let mut chunk_runs = original_runs[..4].to_vec();
    chunk_runs.extend_from_slice(&(new_runs.len() as u32).to_be_bytes());
    for (chunk, count) in new_runs {
        chunk_runs.extend_from_slice(&chunk.to_be_bytes());
        chunk_runs.extend_from_slice(&count.to_be_bytes());
        chunk_runs.extend_from_slice(&1u32.to_be_bytes());
    }
    let wide = wide || tail_offset > u64::from(u32::MAX);
    let mut chunk_offsets = offsets[..4].to_vec();
    chunk_offsets.extend_from_slice(&(new_offsets.len() as u32).to_be_bytes());
    for offset in new_offsets {
        if wide {
            chunk_offsets.extend_from_slice(&offset.to_be_bytes());
        } else {
            chunk_offsets.extend_from_slice(&(offset as u32).to_be_bytes());
        }
    }
    rewrite(stbl, |atom, out| match &atom.kind {
        b"stsz" => write_atom(out, atom.kind, &sizes),
        b"stsc" => write_atom(out, atom.kind, &chunk_runs),
        b"co64" | b"stco" => {
            write_atom(out, if wide { *b"co64" } else { *b"stco" }, &chunk_offsets)
        }
        _ => {
            out.extend_from_slice(atom.bytes);
            Ok(())
        }
    })
    .map(Some)
}

fn video_encoder_tag() -> io::Result<Vec<u8>> {
    let mut data = vec![0, 0, 0, 1, 0, 0, 0, 0];
    data.extend_from_slice(concat!("upflow ", env!("CARGO_PKG_VERSION"), "\0").as_bytes());
    let mut name = Vec::new();
    write_atom(&mut name, *b"data", &data)?;
    let mut tags = Vec::new();
    write_atom(&mut tags, *b"\xa9too", &name)?;
    let mut meta = vec![0; 4];
    let mut handler = [0; 25];
    handler[8..12].copy_from_slice(b"mdir");
    write_atom(&mut meta, *b"hdlr", &handler)?;
    write_atom(&mut meta, *b"ilst", &tags)?;
    let mut udta = Vec::new();
    write_atom(&mut udta, *b"meta", &meta)?;
    Ok(udta)
}

fn without_global_encoder(udta: &[u8]) -> io::Result<Vec<u8>> {
    rewrite(udta, |atom, out| {
        if atom.kind != *b"meta" {
            out.extend_from_slice(atom.bytes);
            return Ok(());
        }
        let mut meta = atom
            .body
            .get(..4)
            .ok_or_else(|| invalid("Invalid MP4 metadata"))?
            .to_vec();
        let mut has_metadata = false;
        let entries = rewrite(&atom.body[4..], |entry, out| {
            if entry.kind == *b"ilst" {
                let list = rewrite(entry.body, |tag, out| {
                    if tag.kind != *b"\xa9too" {
                        out.extend_from_slice(tag.bytes);
                    }
                    Ok(())
                })?;
                if !list.is_empty() {
                    has_metadata = true;
                    write_atom(out, entry.kind, &list)?;
                }
            } else {
                has_metadata |= entry.kind != *b"hdlr";
                out.extend_from_slice(entry.bytes);
            }
            Ok(())
        })?;
        if has_metadata {
            meta.extend_from_slice(&entries);
            write_atom(out, atom.kind, &meta)?;
        }
        Ok(())
    })
}

fn patch_index(moov: &[u8], tail_offset: u64) -> io::Result<Option<Vec<u8>>> {
    let mut patched_audio = false;
    let index = rewrite(moov, |atom, out| {
        match &atom.kind {
            b"trak" => {
                let mdia = child(atom.body, *b"mdia")?;
                match child(mdia, *b"hdlr")?.get(8..12) {
                    Some(b"soun") => {
                        let stbl = child(child(mdia, *b"minf")?, *b"stbl")?;
                        if let Some(index) = patch_samples(stbl, tail_offset)? {
                            let track = change_child(atom.body, *b"mdia", |mdia| {
                                let mdia = change_child(mdia, *b"hdlr", |body| {
                                    let mut handler = body
                                        .get(..24)
                                        .ok_or_else(|| invalid("Invalid audio handler"))?
                                        .to_vec();
                                    handler.extend_from_slice(b"SoundHandler\0");
                                    Ok(handler)
                                })?;
                                change_child(&mdia, *b"minf", |minf| {
                                    change_child(minf, *b"stbl", |_| Ok(index.clone()))
                                })
                            })?;
                            patched_audio = true;
                            return write_atom(out, atom.kind, &track);
                        }
                    }
                    Some(b"vide") => {
                        let mut track = atom.body.to_vec();
                        write_atom(&mut track, *b"udta", &video_encoder_tag()?)?;
                        return write_atom(out, atom.kind, &track);
                    }
                    _ => {}
                }
            }
            b"udta" => {
                let metadata = without_global_encoder(atom.body)?;
                if !metadata.is_empty() {
                    write_atom(out, atom.kind, &metadata)?;
                }
                return Ok(());
            }
            _ => {}
        }
        out.extend_from_slice(atom.bytes);
        Ok(())
    })?;
    Ok(patched_audio.then_some(index))
}

// Patch the unpublished faststart export in place. The first mdat and all video
// sample tables stay intact, so existing video and audio chunk offsets still work.
pub(crate) fn patch_audio_tail(path: &Path) -> io::Result<bool> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let file_size = file.metadata()?.len();
    let mut start = 0;
    while start < file_size {
        file.seek(SeekFrom::Start(start))?;
        let mut header = [0; 8];
        file.read_exact(&mut header)?;
        let (size, header_size) = match u32_at(&header, 0)? {
            0 => return Err(invalid("Open-ended MP4 box in faststart export")),
            1 => {
                let mut extended = [0; 8];
                file.read_exact(&mut extended)?;
                (u64::from_be_bytes(extended), 16)
            }
            size => (u64::from(size), 8),
        };
        if size < header_size || size > file_size - start {
            return Err(invalid("Invalid MP4 box length"));
        }
        if &header[4..] == b"moov" {
            let len =
                usize::try_from(size - header_size).map_err(|_| invalid("MP4 index too large"))?;
            let mut original = Vec::new();
            original.try_reserve_exact(len).map_err(io::Error::other)?;
            original.resize(len, 0);
            file.read_exact(&mut original)?;
            let Some(index) = patch_index(&original, file_size + 8)? else {
                return Ok(false);
            };
            let mut tail = Vec::new();
            write_atom(
                &mut tail,
                *b"mdat",
                &LFE_SILENCE.repeat(REPLACED_PACKETS as usize),
            )?;
            write_atom(&mut tail, *b"moov", &index)?;
            file.seek(SeekFrom::End(0))?;
            file.write_all(&tail)?;
            file.seek(SeekFrom::Start(start + 4))?;
            file.write_all(b"free")?;
            return Ok(true);
        }
        if &header[4..] == b"mdat" {
            break;
        }
        start += size;
    }
    Err(invalid("MP4 faststart index is missing"))
}
