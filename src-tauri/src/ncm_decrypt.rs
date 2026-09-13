use aes::Aes128;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use cipher::{BlockDecrypt, KeyInit, generic_array::GenericArray};
use std::{
    fs::{self, File},
    io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

pub const CORE_KEY: [u8; 16] = [0x68,0x7a,0x48,0x52,0x41,0x6d,0x73,0x6f,0x35,0x6b,0x49,0x6e,0x62,0x61,0x78,0x57];
pub const MODIFY_KEY: [u8; 16] = [0x23,0x31,0x34,0x6c,0x6a,0x6b,0x5f,0x21,0x5c,0x5d,0x26,0x30,0x55,0x3c,0x27,0x28];

/// Streaming chunk size for audio decryption (1 MiB reduces filesystem I/O calls).
const CHUNK: usize = 1024 * 1024;

pub fn aes_ecb_decrypt(key: &[u8; 16], data: &[u8]) -> Vec<u8> {
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let mut out = data.to_vec();
    for chunk in out.chunks_exact_mut(16) {
        cipher.decrypt_block(GenericArray::from_mut_slice(chunk));
    }
    if let Some(&pad) = out.last() {
        let p = pad as usize;
        if (1..=16).contains(&p) && out.len() >= p && out[out.len() - p..].iter().all(|b| *b == pad) {
            out.truncate(out.len() - p);
        }
    }
    out
}

pub fn build_keybox(key: &[u8]) -> [u8; 256] {
    let mut b = [0u8; 256];
    for (i, x) in b.iter_mut().enumerate() {
        *x = i as u8;
    }
    let (mut last, mut off) = (0u8, 0usize);
    for i in 0..256 {
        let swap = b[i];
        let c = swap.wrapping_add(last).wrapping_add(key[off]) as usize;
        off = (off + 1) % key.len();
        b.swap(i, c);
        last = c as u8;
    }
    b
}

/// Precompute the 256-byte XOR lookup table used by the audio keystream.
pub fn build_stream_lut(keybox: &[u8; 256]) -> [u8; 256] {
    let mut lut = [0u8; 256];
    for j in 0..256 {
        let idx = keybox[j].wrapping_add(keybox[keybox[j].wrapping_add(j as u8) as usize]);
        lut[j] = keybox[idx as usize];
    }
    lut
}

/// Decrypt a buffer in place starting at the given absolute stream offset.
pub fn stream_decrypt_in_place(lut: &[u8; 256], offset: usize, data: &mut [u8]) {
    for (i, b) in data.iter_mut().enumerate() {
        *b ^= lut[(offset + i + 1) & 0xff];
    }
}

pub fn stream_decrypt(keybox: &[u8; 256], data: &[u8]) -> Vec<u8> {
    let lut = build_stream_lut(keybox);
    let mut out = data.to_vec();
    stream_decrypt_in_place(&lut, 0, &mut out);
    out
}

#[derive(Clone, Debug, Default)]
pub struct NcmMetadata {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub format: String,
    pub picture_type: u8,
}

fn parse_metadata(raw: &[u8]) -> NcmMetadata {
    let mut m = NcmMetadata { format: "mp3".into(), picture_type: 3, ..Default::default() };
    let decoded = decode_metadata(raw);
    let text = String::from_utf8_lossy(decoded.get(6..).unwrap_or(&decoded));
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
        m.title = v.get("musicName").and_then(|x| x.as_str()).unwrap_or("").into();
        m.album = v.get("album").and_then(|x| x.as_str()).unwrap_or("").into();
        if let Some(a) = v.get("artist").and_then(|x| x.as_array()) {
            m.artist = a.iter().filter_map(|x| x.as_array()?.first()?.as_str()).collect::<Vec<_>>().join("/");
        }
        m.format = v.get("format").and_then(|x| x.as_str()).unwrap_or("mp3").into();
    }
    m
}

pub fn decode_metadata(data: &[u8]) -> Vec<u8> {
    let decoded = STANDARD.decode(data).unwrap_or_else(|_| data.to_vec());
    aes_ecb_decrypt(&MODIFY_KEY, &decoded)
}

/// Header offsets parsed from an NCM file, plus the reusable key box.
struct NcmHeader {
    keybox: [u8; 256],
    metadata: NcmMetadata,
    cover: Option<(u64, usize)>,
    audio_offset: u64,
}

/// Parse only the NCM header, leaving the reader positioned at the audio data.
fn read_header<R: Read + Seek>(reader: &mut R, total: u64) -> io::Result<NcmHeader> {
    let mut head = vec![0u8; 10];
    reader.read_exact(&mut head)?;
    if &head[0..8] != b"CTENFDAM" && &head[0..8] != b"NETCMADF" {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid NCM header"));
    }

    let key_len = {
        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf)?;
        u32::from_le_bytes(buf) as usize
    };
    if key_len == 0 || key_len as u64 > total {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid key block"));
    }
    let mut key = vec![0u8; key_len];
    reader.read_exact(&mut key)?;
    for b in &mut key {
        *b ^= 0x64;
    }
    let dec = aes_ecb_decrypt(&CORE_KEY, &key);
    let keybox = build_keybox(dec.get(17..).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid key"))?);

    let meta_len = {
        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf)?;
        u32::from_le_bytes(buf) as usize
    };
    let metadata = if meta_len > 0 {
        let mut raw = vec![0u8; meta_len];
        reader.read_exact(&mut raw)?;
        for b in &mut raw {
            *b ^= 0x63;
        }
        parse_metadata(&raw[22.min(raw.len())..])
    } else {
        NcmMetadata { format: "mp3".into(), picture_type: 3, ..Default::default() }
    };

    // Layout after the metadata block, matching upstream ncmdump exactly:
    //   [9 bytes reserved][4 bytes cover_len]
    // Then `image_start` (the position right after the length field) is the
    // first byte of the cover, and the audio begins at image_start + cover_len.
    // Verified byte-for-byte against ncmdump 0.6 on the reference files.
    let mut reserved = [0u8; 9];
    reader.read_exact(&mut reserved)?;
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf)?;
    let cover_len = u32::from_le_bytes(buf) as u64;
    let image_start = reader.stream_position()?;

    let avail = total.saturating_sub(image_start);
    let cover_len = cover_len.min(avail);
    let cover = if cover_len > 0 { Some((image_start, cover_len as usize)) } else { None };
    let audio_offset = image_start.saturating_add(cover_len);

    Ok(NcmHeader { keybox, metadata, cover, audio_offset })
}

pub fn process_ncm_file(input: &Path, output_dir: &Path) -> io::Result<PathBuf> {
    process_ncm_file_with_progress(input, output_dir, |_, _| {})
}

/// Decrypt one NCM file, reporting coarse progress through `on_progress`.
///
/// `stage` is a short human readable label for the queue UI and `pct` is the
/// percentage of the whole job (0-100).
pub fn process_ncm_file_with_progress<F>(input: &Path, output_dir: &Path, mut on_progress: F) -> io::Result<PathBuf>
where F: FnMut(&str, u8) {
    let total = fs::metadata(input)?.len();
    let mut reader = BufReader::with_capacity(CHUNK, File::open(input)?);
    on_progress("读取文件", 2);
    let header = read_header(&mut reader, total)?;

    fs::create_dir_all(output_dir)?;
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("output");

    // Peek at the decrypted payload to detect the container format. The probe
    // must use the same key box and offset as the real write, and it must not
    // rely on BufReader state (a seek does not clear a partially read buffer).
    let audio_start = header.audio_offset;
    let ext = {
        let lut = build_stream_lut(&header.keybox);
        let mut probe = [0u8; 4];
        reader.seek(SeekFrom::Start(audio_start))?;
        let n = reader.read(&mut probe)?;
        if n > 0 {
            stream_decrypt_in_place(&lut, 0, &mut probe[..n]);
        }
        if n >= 4 && &probe[..4] == b"fLaC" {
            "flac"
        } else if n >= 3 && &probe[..3] == b"ID3" {
            "mp3"
        } else if n >= 2 && probe[0] == 0xff && (probe[1] & 0xe0) == 0xe0 {
            "mp3"
        } else {
            "flac"
        }
    };

    let out = output_dir.join(format!("{stem}.{ext}"));
    // Audio is the whole real workload, so it owns the full 0-90 % range. The
    // remaining 10 % is the (fast) tagging steps, which keeps the bar moving
    // realistically instead of stalling in the middle.
    {
        let lut = build_stream_lut(&header.keybox);
        reader.seek(SeekFrom::Start(audio_start))?;
        let mut writer = BufWriter::with_capacity(CHUNK, File::create(&out)?);
        let mut buf = vec![0u8; CHUNK];
        let mut written = 0usize;
        let span = total.saturating_sub(audio_start).max(1);
        let mut last_pct = 0u8;
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 { break; }
            stream_decrypt_in_place(&lut, written, &mut buf[..n]);
            writer.write_all(&buf[..n])?;
            written += n;
            let pct = ((written as u64 * 90 / span) as u8).min(90);
            if pct != last_pct {
                last_pct = pct;
                on_progress("解密音频", pct);
            }
        }
        writer.flush()?;
    }
    on_progress("写入 Tag", 92);
    let metadata = &header.metadata;
    if ext == "mp3" {
        let _ = crate::audio_tags::append_id3_metadata(&out, &metadata.title, &metadata.artist, &metadata.album, metadata.picture_type);
    } else {
        let _ = crate::audio_tags::append_flac_metadata(&out, &metadata.title, &metadata.artist, &metadata.album);
    }

    if let Some((start, len)) = header.cover {
        on_progress("写入封面", 96);
        let mut cover = vec![0u8; len];
        reader.seek(SeekFrom::Start(start))?;
        if reader.read_exact(&mut cover).is_ok() && !cover.is_empty() {
            let cover_ext = if cover.starts_with(&[0x89, 0x50, 0x4e, 0x47]) { "png" } else { "jpg" };
            let cover_path = output_dir.join(format!("{stem}.cover.{cover_ext}"));
            fs::write(cover_path, &cover)?;
        }
    }

    on_progress("完成", 100);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decrypt_reference_ncm_files() {
        let out = std::env::temp_dir().join("nadt-rust-test");
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        for name in ["Swedish House Mafia,The Weeknd - Moth To A Flame.ncm","The Weeknd - Blinding Lights.ncm"] {
            let input = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../TEST").join(name);
            if input.exists() {
                let result = process_ncm_file(&input, &out);
                assert!(result.is_ok(), "failed to decrypt {name}: {:?}", result.err());
                assert!(result.unwrap().exists());
            }
        }
    }
}
